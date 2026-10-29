//! Authenticated-user endpoint: `GET /registry/api/me`.
//!
//! Returns the current user's email and whether they're the configured
//! admin. Any authenticated caller (admin or not) gets a 200 — unlike the
//! admin-gated publish/yank endpoints this is purely informational. The CLI
//! uses it as a login-probe after exchanging a device code.
//!
//! Unauthenticated callers get the 401 envelope that `require_user`
//! produces. We don't build it by hand here.

use serde_json::json;
use wafer_run::{Context, Message, OutputStream};

use crate::blocks::registry::{auth, config, routes::resp};

/// `GET /registry/api/me` — identity + admin flag for the caller.
pub async fn get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user = match auth::require_user(ctx, msg).await {
        Ok(u) => u,
        Err(out) => return out,
    };
    let admin_email = match config::admin_email(ctx).await {
        Ok(email) => email,
        Err(e) => return OutputStream::error(e),
    };
    resp::ok_json(&json!({
        "email": user.email,
        "is_admin": auth::is_admin_email(&user.email, &admin_email),
    }))
}
