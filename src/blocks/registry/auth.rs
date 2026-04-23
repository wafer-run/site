//! Admin-gate middleware for the registry block.
//!
//! Two public entry points:
//!
//! - [`require_user`] — resolve the caller to an `AuthedUser` by checking a
//!   Bearer PAT against `registry_tokens` first, then falling back to the
//!   `suppers-ai/auth` block (session cookie or solobase-issued token).
//! - [`require_admin`] — wraps `require_user` and additionally gates on the
//!   configured admin email. Non-admins get the "coming-soon" response.
//!
//! Task 11 does not mount either helper on a route — Tasks 12+ will plug
//! them in. Keeping them isolated here means the gate implementation can be
//! unit-tested without touching the HTTP dispatch path.
//!
//! No raw SQL is used: the PAT lookup goes through
//! `wafer_core::clients::database::get_by_field`.

use serde::Deserialize;
use wafer_run::{common::ServiceOp, context::Context, types::Message, OutputStream};

use crate::blocks::registry::{db, routes::resp, templates, RegistryConfig};

/// The authenticated caller, as resolved by [`require_user`].
///
/// `email` is populated best-effort from `suppers-ai/auth`'s user-profile
/// service. In test harnesses that don't wire the auth block it will be
/// empty — callers like [`require_admin`] that compare on email must treat
/// an empty string as "not the admin".
#[derive(Clone, Debug)]
pub struct AuthedUser {
    /// Opaque user id — matches `UserId(String)` in `wafer-core::interfaces::auth`.
    pub id: String,
    /// User's email address. Empty string when we couldn't fetch a profile.
    pub email: String,
}

#[derive(Deserialize)]
struct UserIdResponse {
    user_id: String,
}

#[derive(Deserialize)]
struct UserProfileResponse {
    email: String,
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
fn verify_jwt(token: &str, jwt_secret: &str) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    let derived = solobase_core::crypto::derive_block_jwt_key(
        jwt_secret,
        "suppers-ai/auth",
    );
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

/// Best-effort profile lookup for the email. Returns `""` on any failure —
/// the auth block may not be registered (test harnesses) or the user may
/// have been deleted out from under a valid token. Callers must treat an
/// empty email as "not admin" to stay safe.
async fn fetch_email(ctx: &dyn Context, inbound: &Message, user_id: &str) -> String {
    // Solobase's SolobaseAuthBlock exposes auth as an http-handler block —
    // not the auth@v1 service interface. That strips the service-op names
    // (`auth.user_profile`, `auth.require_user`) from the runtime's action
    // validator, so we can't call `ServiceOp::AUTH_USER_PROFILE` directly.
    // Instead, call the block's declared HTTP endpoint `/b/auth/api/me`
    // with action="retrieve" — which IS in the endpoint list — and
    // forward the inbound cookie/authorization so the auth block treats
    // us as the same caller the browser is.
    //
    // The returned body shape is solobase's `MeResponse { user: { email }, ... }`
    // (see `crates/solobase-core/src/blocks/auth/handlers/me.rs`).
    let mut me_msg = Message::new("");
    me_msg.set_meta("req.action", "retrieve");
    me_msg.set_meta("req.resource", "/b/auth/api/me");
    let cookie = inbound.header("cookie");
    if !cookie.is_empty() {
        me_msg.set_meta("http.header.cookie", cookie);
    }
    let authz = inbound.header("authorization");
    if !authz.is_empty() {
        me_msg.set_meta("http.header.authorization", authz);
    }
    let _ = user_id; // solobase /me reads identity from the cookie/bearer, not a body param

    #[derive(Deserialize)]
    struct MeUser {
        email: String,
    }
    #[derive(Deserialize)]
    struct MeResponse {
        user: MeUser,
    }

    match ctx
        .call_block_buffered("suppers-ai/auth", me_msg, &[])
        .await
    {
        Ok(buf) => {
            // Try the nested-user shape first; fall back to flat {email} if
            // the shape ever changes.
            if let Ok(m) = serde_json::from_slice::<MeResponse>(&buf.body) {
                return m.user.email;
            }
            if let Ok(m) = serde_json::from_slice::<UserProfileResponse>(&buf.body) {
                return m.email;
            }
            String::new()
        }
        Err(_) => String::new(),
    }
}

/// Gate on the configured admin email.
///
/// On a hit, returns the authenticated user. On a miss, returns a 403
/// "coming-soon" response — HTML when the caller `Accept`s HTML, JSON
/// otherwise. Empty/missing email is treated as "not admin" (see the
/// [`fetch_email`] doc comment for why an empty email can occur).
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
