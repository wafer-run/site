//! Authenticated user endpoint: GET /registry/api/me
//!
//! Populated in Task 14 (download + yank + /api/me).

use wafer_run::{Context, Message, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// GET /registry/api/me — current user's profile (requires auth).
pub async fn get(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/api/me — not implemented yet (Task 14)".to_string(),
        meta: vec![],
    })
}
