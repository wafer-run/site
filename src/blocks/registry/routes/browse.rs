//! Browse endpoints: registry index, search, and package detail pages.

use wafer_run::{Context, Message, OutputStream};

use crate::blocks::registry::{db, routes::resp, templates, RegistryConfig};

/// GET /registry — registry index page.
pub async fn index(ctx: &dyn Context, msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    let q = msg.query("q");
    let query_str = if q.is_empty() { "" } else { q };
    let (packages, total) =
        db::list_packages(ctx, if q.is_empty() { None } else { Some(q) }, 1, 50)
            .await
            .unwrap_or_default();
    let body = templates::browse(&packages, query_str, total).into_string();
    resp::ok_html(&body)
}

/// GET /registry/search — JSON envelope `{ packages, total, query, page, page_size }`.
///
/// Query params:
/// - `q` — optional substring filter over package `name` (case-sensitive LIKE).
/// - `page` — 1-based, defaults to 1.
///
/// Page size is currently fixed at 20 rows; can be surfaced as a param
/// once pagination controls appear in the HTML.
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
pub async fn package_detail(
    ctx: &dyn Context,
    msg: &Message,
    _cfg: &RegistryConfig,
) -> OutputStream {
    let path = msg.path().strip_prefix("/registry/").unwrap_or("");
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() != 2 {
        let body = templates::not_found("Page").into_string();
        return resp::not_found_html(&body);
    }
    match db::get_package(ctx, segs[0], segs[1]).await {
        Ok(Some(pkg)) => {
            let body = templates::package_detail(&pkg).into_string();
            resp::ok_html(&body)
        }
        _ => {
            let body = templates::not_found(&format!("{}/{}", segs[0], segs[1])).into_string();
            resp::not_found_html(&body)
        }
    }
}
