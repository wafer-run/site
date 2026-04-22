//! Package detail API endpoint: GET /registry/api/packages/{org}/{package}
//!
//! Populated in Task 9 (public JSON read endpoints).

use wafer_run::{Context, Message, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// GET /registry/api/packages/{org}/{package} — JSON package metadata.
pub async fn get(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/api/packages/{org}/{package} — not implemented yet (Task 9)".to_string(),
        meta: vec![],
    })
}
