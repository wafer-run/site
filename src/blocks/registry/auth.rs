//! Admin-gate middleware for the registry block.
//!
//! Two public entry points:
//!
//! - [`require_user`] — resolve the caller to an `AuthedUser` by checking a
//!   Bearer PAT against `registry_tokens` first, then falling back to
//!   verifying a solobase-issued JWT (Bearer header or `auth_token` cookie).
//! - [`require_admin`] — wraps `require_user` and additionally gates on the
//!   configured admin email. Non-admins get the "coming-soon" response.
//!
//! No raw SQL is used: the PAT lookup goes through
//! `wafer_core::clients::database::get_by_field`.

use wafer_run::{context::Context, types::Message, OutputStream};

use crate::blocks::registry::{db, routes::resp, templates, RegistryConfig};

/// The authenticated caller, as resolved by [`require_user`].
///
/// `email` is read from the token's `email` claim (JWT) or the
/// `registry_tokens.email` column (PAT). It may be an empty string when the
/// token was minted before we started capturing email — callers like
/// [`require_admin`] that compare on email must treat an empty string as
/// "not the admin".
#[derive(Clone, Debug)]
pub struct AuthedUser {
    /// Opaque user id — matches `UserId(String)` in `wafer-core::interfaces::auth`.
    pub id: String,
    /// User's email address. Empty string when the token didn't carry one.
    pub email: String,
    /// How the user authenticated — `"password"`, `"oauth.github"`, etc.
    /// `"pat"` for registry-issued personal-access tokens.
    /// Empty string when the JWT was minted before this claim existed
    /// (treat as "unknown / not OAuth" by gates that require a method).
    pub auth_method: String,
}

/// Resolve the caller to an [`AuthedUser`].
///
/// Resolution order:
///
/// 1. Bearer PAT in the `Authorization` header — looked up in
///    `registry_tokens` by `sha256(raw_token)` hex. Revoked tokens (those
///    with a `revoked_at` set) are skipped and we fall through to step 2.
///    This path exists because PATs are minted by
///    `POST /registry/api/cli-login/exchange` and live only in the
///    registry's own store — `suppers-ai/auth` doesn't know about them.
/// 2. Delegate to `suppers-ai/auth` via `AUTH_REQUIRE_USER`. The session
///    cookie (and Authorization header, for solobase-managed PATs) ride on
///    `http.header.*` meta keys — same convention
///    `wafer-run/http-listener` uses.
///
/// Returns an `OutputStream` error response on any failure path so callers
/// can early-return without additional shaping.
pub async fn require_user(
    ctx: &dyn Context,
    msg: &Message,
    cfg: &RegistryConfig,
) -> Result<AuthedUser, OutputStream> {
    // 1. Try bearer PAT against registry_tokens. This path handles PATs the
    //    registry itself issued via CLI-login exchange. The PAT inherits the
    //    auth method of the session that minted it — for now we tag it
    //    `"pat"` so a future tightening can require, say, "PATs only issued
    //    from OAuth sessions" without re-plumbing.
    let auth_header = msg.header("authorization");
    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if let Ok(Some((user_id, email))) = db::resolve_bearer(ctx, token).await {
            return Ok(AuthedUser {
                id: user_id,
                email,
                auth_method: "pat".to_string(),
            });
        }
        // Fall through to JWT verification — a mismatched PAT might be a
        // solobase-minted JWT.
    }

    // 2. Try solobase's JWT. Solobase's OAuth callback sets the signed JWT
    //    as `auth_token` cookie (or passes it as `Authorization: Bearer`).
    //    Solobase's runtime router does JWT verification transparently for
    //    `/b/**` routes via `extract_auth_meta`, but our `/registry/**` flow
    //    bypasses the router, so we verify here. Uses solobase's own crypto
    //    helpers to stay consistent with the signing key derivation.
    let jwt_token = find_jwt_token(msg);
    if let Some(token) = jwt_token {
        if let Some(claims) = verify_jwt(&token, &cfg.jwt_secret) {
            let sub = claims
                .get("sub")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let email = claims
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let auth_method = claims
                .get("auth_method")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !sub.is_empty() {
                return Ok(AuthedUser {
                    id: sub.to_string(),
                    email: email.to_string(),
                    auth_method: auth_method.to_string(),
                });
            }
        }
    }

    Err(unauthorized_response())
}

/// Find a JWT in the request — either the Authorization Bearer header or
/// the `auth_token` cookie that solobase's OAuth callback sets.
fn find_jwt_token(msg: &Message) -> Option<String> {
    let auth_header = msg.header("authorization");
    if let Some(t) = auth_header.strip_prefix("Bearer ") {
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let cookie = msg.header("cookie");
    for part in cookie.split(';') {
        if let Some(v) = part.trim().strip_prefix("auth_token=") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Verify a JWT against solobase's auth-block derived key, with fallback to
/// the master secret. Mirrors `solobase_core::crypto::extract_auth_meta`
/// except we return the claims map instead of mutating message meta.
fn verify_jwt(
    token: &str,
    jwt_secret: &str,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    // Derived-key signing: solobase's auth block signs JWTs with HKDF-SHA256
    // of the master secret + block id ("wafer-jwt|suppers-ai/auth").
    if let Ok(derived) = solobase_core::crypto::derive_block_jwt_key(jwt_secret, "suppers-ai/auth") {
        if let Ok(claims) = solobase_core::crypto::jwt_verify(token, &derived) {
            if claims.get("type").and_then(|v| v.as_str()).unwrap_or("") != "refresh" {
                return Some(claims);
            }
        }
    }
    if let Ok(claims) = solobase_core::crypto::jwt_verify(token, jwt_secret) {
        if claims.get("type").and_then(|v| v.as_str()).unwrap_or("") != "refresh" {
            return Some(claims);
        }
    }
    None
}

/// Gate on the configured admin email — and, when configured, the auth
/// method the user signed in with.
///
/// On a hit, returns the authenticated user. On a miss, returns a 403:
/// - `coming-soon` template/JSON when the caller's email isn't the admin
///   email (current default for non-admins).
/// - `auth-method-required` JSON / HTML when the email matches but the
///   auth method doesn't satisfy [`RegistryConfig::required_auth_method`].
///   This separates "you're not allowed" from "log in via the right
///   method" so the CLI / admin can react correctly.
///
/// Empty/missing email is treated as "not admin".
pub async fn require_admin(
    ctx: &dyn Context,
    msg: &Message,
    cfg: &RegistryConfig,
) -> Result<AuthedUser, OutputStream> {
    let user = require_user(ctx, msg, cfg).await?;
    let is_admin_email =
        !user.email.is_empty() && user.email.eq_ignore_ascii_case(&cfg.admin_email);
    if !is_admin_email {
        return Err(coming_soon_response(msg));
    }
    if !cfg.required_auth_method.is_empty()
        && !user
            .auth_method
            .eq_ignore_ascii_case(&cfg.required_auth_method)
    {
        return Err(auth_method_required_response(
            msg,
            &cfg.required_auth_method,
        ));
    }
    Ok(user)
}

fn coming_soon_response(msg: &Message) -> OutputStream {
    let accept = msg.header("accept");
    if accept.contains("text/html") {
        resp::html_response(403, &templates::coming_soon().into_string())
    } else {
        resp::json_response(
            403,
            &serde_json::json!({
                "error": "coming-soon",
                "message": "Publishing is not yet open to other users."
            }),
        )
    }
}

fn auth_method_required_response(msg: &Message, required: &str) -> OutputStream {
    let message =
        format!("Admin actions require signing in via {required}. Re-authenticate and retry.");
    let accept = msg.header("accept");
    if accept.contains("text/html") {
        resp::html_response(
            403,
            &format!("<!doctype html><meta charset=utf-8><title>Auth method required</title><p>{message}</p>"),
        )
    } else {
        resp::json_response(
            403,
            &serde_json::json!({
                "error": "auth-method-required",
                "message": message,
                "required_auth_method": required,
            }),
        )
    }
}

fn unauthorized_response() -> OutputStream {
    resp::json_response(
        401,
        &serde_json::json!({
            "error": "unauthorized",
            "message": "Login required"
        }),
    )
}
