//! Publish endpoint: POST /registry/api/publish
//!
//! Populated in Task 13 (publish endpoint implementation).

use wafer_run::{Context, Message, InputStream, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// POST /registry/api/publish — publish a new package version.
pub async fn post(
    _ctx: &dyn Context,
    _msg: &Message,
    _input: InputStream,
    _cfg: &RegistryConfig,
) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "POST /registry/api/publish — not implemented yet (Task 13)".to_string(),
        meta: vec![],
    })
}
