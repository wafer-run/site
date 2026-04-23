//! Browse endpoints: registry index, search, and package detail pages.
//!
//! Task 9 lands `search()` — the JSON API that powers the browse page — so
//! the harness can exercise the query path end-to-end. The HTML views
//! (`index` and `package_detail`) stay stubbed at 501 until Task 10
//! populates them via maud templates.

use wafer_run::{Context, ErrorCode, Message, OutputStream, WaferError};

use crate::blocks::registry::{db, routes::resp, RegistryConfig};

/// GET /registry — registry index page.
pub async fn index(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry — index not implemented yet (Task 10)".to_string(),
        meta: vec![],
    })
}

/// GET /registry/search — JSON envelope `{ packages, total, query, page, page_size }`.
///
/// Query params:
/// - `q` — optional substring filter over package `name` (case-sensitive LIKE).
/// - `page` — 1-based, defaults to 1.
///
/// Page size is currently fixed at 20 rows; Task 10 may surface it as a
/// param once pagination controls appear in the HTML.
pub async fn search(ctx: &dyn Context, msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    let q_raw = msg.query("q");
    let q: Option<&str> = if q_raw.is_empty() { None } else { Some(q_raw) };
    let page: i64 = {
        let raw = msg.query("page");
        if raw.is_empty() {
            1
        } else {
            raw.parse().unwrap_or(1)
        }
    };
    let per_page: i64 = 20;

    match db::list_packages(ctx, q, page, per_page).await {
        Ok((packages, total)) => resp::ok_json(&serde_json::json!({
            "packages": packages,
            "total": total,
            "query": q.unwrap_or(""),
            "page": page,
            "page_size": per_page,
        })),
        Err(e) => resp::internal(&e.to_string()),
    }
}

/// GET /registry/{org}/{package} — package detail page.
pub async fn package_detail(_ctx: &dyn Context, _msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::Unimplemented,
        message: "GET /registry/{org}/{package} — detail not implemented yet (Task 10)".to_string(),
        meta: vec![],
    })
}
