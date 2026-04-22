//! Browse endpoints: registry index, search, and package detail pages.
//!
//! Populated in Task 10 (HTML templates).

use wafer_run::{Context, Message, OutputStream, WaferError, ErrorCode};
use crate::blocks::registry::RegistryConfig;

/// GET /registry — registry index page.
pub async fn index(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry — index not implemented yet (Task 10)".to_string(),
        meta: vec![],
    })
}

/// GET /registry/search — search index page and results.
pub async fn search(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/search — search not implemented yet (Task 10)".to_string(),
        meta: vec![],
    })
}

/// GET /registry/{org}/{package} — package detail page.
pub async fn package_detail(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/{org}/{package} — detail not implemented yet (Task 10)".to_string(),
        meta: vec![],
    })
}
