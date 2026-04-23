//! Shared JSON response helpers for registry routes.
//!
//! Every handler in this module speaks JSON with a kebab-case `error` tag on
//! non-2xx responses. Using `OutputStream::error(...)` directly would
//! serialize the `ErrorCode` variant as PascalCase — the registry's public
//! contract is kebab-case, so we build responses by hand with explicit
//! `META_RESP_STATUS` + `META_RESP_CONTENT_TYPE` entries.

use serde::Serialize;
use wafer_run::{
    meta::{META_RESP_CONTENT_TYPE, META_RESP_STATUS},
    types::MetaEntry,
    OutputStream,
};

/// Build a JSON response with an explicit HTTP status. Falls back to a 500
/// JSON error envelope if serialization fails (should never happen for the
/// DTOs defined in `models.rs`, but surfacing it is cheaper than panicking).
pub fn json_response<T: Serialize>(status: u16, value: &T) -> OutputStream {
    let body = match serde_json::to_vec(value) {
        Ok(b) => b,
        Err(e) => {
            // Re-entrant safety: `serialize-failed` doesn't itself depend on
            // `T`, so the only way *this* branch fails is an OOM. Accept the
            // trap and emit a minimal hand-rolled body.
            return OutputStream::respond_with_meta(
                format!(
                    "{{\"error\":\"internal\",\"message\":\"serialize failed: {e}\"}}"
                )
                .into_bytes(),
                vec![
                    MetaEntry {
                        key: META_RESP_STATUS.into(),
                        value: "500".into(),
                    },
                    MetaEntry {
                        key: META_RESP_CONTENT_TYPE.into(),
                        value: "application/json".into(),
                    },
                ],
            );
        }
    };
    OutputStream::respond_with_meta(
        body,
        vec![
            MetaEntry {
                key: META_RESP_STATUS.into(),
                value: status.to_string(),
            },
            MetaEntry {
                key: META_RESP_CONTENT_TYPE.into(),
                value: "application/json".into(),
            },
        ],
    )
}

/// 200-OK with `value` as the JSON body.
pub fn ok_json<T: Serialize>(value: &T) -> OutputStream {
    json_response(200, value)
}

/// 404 Not Found — `{"error":"not-found","message":"<message>"}`.
pub fn not_found(message: &str) -> OutputStream {
    json_response(
        404,
        &serde_json::json!({ "error": "not-found", "message": message }),
    )
}

/// 400 Bad Request — `{"error":"bad-request","message":"<message>"}`.
pub fn bad_request(message: &str) -> OutputStream {
    json_response(
        400,
        &serde_json::json!({ "error": "bad-request", "message": message }),
    )
}

/// 500 Internal Server Error — `{"error":"internal","message":"<message>"}`.
pub fn internal(message: &str) -> OutputStream {
    json_response(
        500,
        &serde_json::json!({ "error": "internal", "message": message }),
    )
}
