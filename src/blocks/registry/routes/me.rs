//! Authenticated-user endpoint: `GET /registry/api/me`.
//!
//! Returns the current user's email and whether they're the configured
//! admin. Any authenticated caller (admin or not) gets a 200 — unlike the
//! admin-gated publish/yank endpoints this is purely informational. The CLI
//! uses it as a login-probe after exchanging a device code.
//!
//! Unauthenticated callers get the 401 envelope that `require_user`
//! produces via `suppers-ai/auth`. We don't build it by hand here.

use serde_json::json;
use wafer_run::{Context, Message, OutputStream};

use crate::blocks::registry::{auth, routes::resp, RegistryConfig};

/// `GET /registry/api/me` — identity + admin flag for the caller.
pub async fn get(ctx: &dyn Context, msg: &Message, cfg: &RegistryConfig) -> OutputStream {
    match auth::require_user(ctx, msg, cfg).await {
        Ok(u) => resp::ok_json(&json!({
            "email": u.email,
            "is_admin": !u.email.is_empty() && u.email.eq_ignore_ascii_case(&cfg.admin_email),
        })),
        Err(out) => out,
    }
}
