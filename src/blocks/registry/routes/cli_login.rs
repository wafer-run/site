//! CLI login endpoints: GET /registry/cli-login and POST /registry/api/cli-login/exchange
//!
//! Populated in Task 12 (CLI login page) and Task 12 (exchange endpoint).

use wafer_run::{Context, Message, InputStream, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// GET /registry/cli-login — CLI login page with code display.
pub async fn page(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/cli-login — not implemented yet (Task 12)".to_string(),
        meta: vec![],
    })
}

/// POST /registry/api/cli-login/exchange — exchange device code for token.
pub async fn exchange(
    _ctx: &dyn Context,
    _msg: &Message,
    _input: InputStream,
    _cfg: &RegistryConfig,
) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "POST /registry/api/cli-login/exchange — not implemented yet (Task 12)".to_string(),
        meta: vec![],
    })
}
