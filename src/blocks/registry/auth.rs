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
pub async fn require_user(ctx: &dyn Context, msg: &Message) -> Result<AuthedUser, OutputStream> {
    // 1. Try bearer PAT against registry_tokens. The sha256+lookup lives in
    //    `db::resolve_bearer` so the exchange endpoint and this gate share
    //    one implementation — Task 12 centralized it there.
    let auth_header = msg.header("authorization");
    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if let Ok(Some((user_id, email))) = db::resolve_bearer(ctx, token).await {
            // Email was captured at CLI-login exchange time and lives on the
            // token row, so no cross-block profile fetch is needed here.
            return Ok(AuthedUser { id: user_id, email });
        }
        // Fall through to auth-block delegation on None/Err — a mismatched
        // or revoked PAT shouldn't hard-fail if the caller also has a valid
        // session cookie.
    }

    // 2. Delegate to suppers-ai/auth. Propagate cookie + authorization
    //    headers via the same `http.header.*` meta keys the real HTTP
    //    adapter uses — see `MigrationTestCtx`'s dispatch path in
    //    solobase-core/tests/auth/block_dispatch.rs for the reference shape.
    let mut auth_msg = Message::new(ServiceOp::AUTH_REQUIRE_USER);
    auth_msg.set_meta("req.action", ServiceOp::AUTH_REQUIRE_USER);
    let cookie = msg.header("cookie");
    if !cookie.is_empty() {
        auth_msg.set_meta("http.header.cookie", cookie);
    }
    if !auth_header.is_empty() {
        auth_msg.set_meta("http.header.authorization", auth_header);
    }

    let buf = ctx
        .call_block_buffered("suppers-ai/auth", auth_msg, &[])
        .await
        .map_err(|_| unauthorized_response())?;
    let uid: UserIdResponse =
        serde_json::from_slice(&buf.body).map_err(|_| unauthorized_response())?;

    let email = fetch_email(ctx, msg, &uid.user_id).await;
    Ok(AuthedUser {
        id: uid.user_id,
        email,
    })
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
    let user = require_user(ctx, msg).await?;
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
