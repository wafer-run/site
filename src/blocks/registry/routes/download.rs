//! Download endpoint: GET /registry/download/{org}/{package}/{version}
//!
//! Populated in Task 14 (download + yank + /api/me).

use wafer_run::{Context, Message, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// GET /registry/download/{org}/{package}/{version} — download a package tarball.
pub async fn get(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/download/{org}/{package}/{version} — not implemented yet (Task 14)".to_string(),
        meta: vec![],
    })
}
