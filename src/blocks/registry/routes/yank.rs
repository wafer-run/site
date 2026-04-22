//! Yank/unyank endpoints: POST paths ending in /yank or /unyank
//!
//! Populated in Task 14 (yank/unyank + download + /api/me).

use wafer_run::{Context, Message, InputStream, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// POST /registry/api/packages/{org}/{package}/versions/{version}/yank — yank a version.
pub async fn yank(
    _ctx: &dyn Context,
    _msg: &Message,
    _input: InputStream,
    _cfg: &RegistryConfig,
) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "POST .../yank — not implemented yet (Task 14)".to_string(),
        meta: vec![],
    })
}

/// POST /registry/api/packages/{org}/{package}/versions/{version}/unyank — restore a yanked version.
pub async fn unyank(
    _ctx: &dyn Context,
    _msg: &Message,
    _input: InputStream,
    _cfg: &RegistryConfig,
) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "POST .../unyank — not implemented yet (Task 14)".to_string(),
        meta: vec![],
    })
}
