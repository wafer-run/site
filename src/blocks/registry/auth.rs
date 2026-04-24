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
}

/// Resolve the caller to an [`AuthedUser`].
///
/// Resolution order:
///
/// 1. Bearer PAT in the `Authorization` header — looked up in
///    `registry_tokens` by `sha256(raw_token)` hex. Revoked tokens (those
///    with a `revoked_at` set) are skipped and we fall through to step 2.
///    This path exists because PATs are minted by
///    `POST /registry/api/cli-login/exchange` (Task 12) and live only in the
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
    //    registry itself issued via CLI-login exchange (Task 12).
    let auth_header = msg.header("authorization");
    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if let Ok(Some((user_id, email))) = db::resolve_bearer(ctx, token).await {
            return Ok(AuthedUser { id: user_id, email });
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
            if !sub.is_empty() {
                return Ok(AuthedUser {
                    id: sub.to_string(),
                    email: email.to_string(),
                });
            }
        }
    }

    Err(unauthorized_response())
}

/// Inline copy of `solobase_core::crypto::derive_block_jwt_key` (currently
/// `pub(crate)` upstream). TODO(solobase): remove once a solobase release
/// exposes the helper publicly; replace the call in `verify_jwt` with
/// `solobase_core::crypto::derive_block_jwt_key` and drop the `hkdf`
/// direct dep from `Cargo.toml`.
fn derive_block_jwt_key_local(master_secret: &str, block_id: &str) -> String {
    use hkdf::Hkdf;
    use sha2::Sha256;

    let hk = Hkdf::<Sha256>::new(None, master_secret.as_bytes());
    let info = format!("wafer-jwt|{block_id}");
    let mut okm = [0u8; 32];
    hk.expand(info.as_bytes(), &mut okm).expect("HKDF expand");
    okm.iter().map(|b| format!("{b:02x}")).collect()
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
    // of the master secret + block id ("wafer-jwt|suppers-ai/auth"). Mirrors
    // `solobase_core::crypto::derive_block_jwt_key` so we don't depend on
    // that fn being pub upstream (it isn't yet on solobase main).
    let derived = derive_block_jwt_key_local(jwt_secret, "suppers-ai/auth");
    if let Ok(claims) = solobase_core::crypto::jwt_verify(token, &derived) {
        if claims.get("type").and_then(|v| v.as_str()).unwrap_or("") != "refresh" {
            return Some(claims);
        }
    }
    if let Ok(claims) = solobase_core::crypto::jwt_verify(token, jwt_secret) {
        if claims.get("type").and_then(|v| v.as_str()).unwrap_or("") != "refresh" {
            return Some(claims);
        }
    }
    None
}

/// Gate on the configured admin email.
///
/// On a hit, returns the authenticated user. On a miss, returns a 403
/// "coming-soon" response — HTML when the caller `Accept`s HTML, JSON
/// otherwise. Empty/missing email is treated as "not admin".
pub async fn require_admin(
    ctx: &dyn Context,
    msg: &Message,
    cfg: &RegistryConfig,
) -> Result<AuthedUser, OutputStream> {
    let user = require_user(ctx, msg, cfg).await?;
    if !user.email.is_empty() && user.email.eq_ignore_ascii_case(&cfg.admin_email) {
        return Ok(user);
    }
    let accept = msg.header("accept");
    if accept.contains("text/html") {
        Err(resp::html_response(
            403,
            &templates::coming_soon().into_string(),
        ))
    } else {
        Err(resp::json_response(
            403,
            &serde_json::json!({
                "error": "coming-soon",
                "message": "Publishing is not yet open to other users."
            }),
        ))
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
