//! Package read API endpoints:
//!
//! - `GET /registry/api/packages/{org}/{block}`          → `PackageDetail` JSON
//! - `GET /registry/api/packages/{org}/{block}/{ver}`    → `VersionDetail` JSON
//!
//! 404 responses use the `{"error":"not-found","message":"..."}` shape;
//! malformed paths return 400. `OutputStream::error(NotFound, ...)` is not
//! used here because it serializes the error code as PascalCase
//! (`"NotFound"`) — the registry's public contract is kebab-case.

use wafer_run::{Context, Message, OutputStream};

use crate::blocks::registry::{db, routes::resp, RegistryConfig};

/// Dispatch both `/registry/api/packages/{org}/{block}` and
/// `/registry/api/packages/{org}/{block}/{ver}`. The route handler in
/// `handlers.rs` only matches the `/registry/api/packages/` prefix — the
/// concrete arity is decided here by splitting the suffix.
pub async fn get(ctx: &dyn Context, msg: &Message, _cfg: &RegistryConfig) -> OutputStream {
    let segments: Vec<&str> = msg
        .path()
        .strip_prefix("/registry/api/packages/")
        .unwrap_or("")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    match segments.as_slice() {
        [org, block] => match db::get_package(ctx, org, block).await {
            Ok(Some(pkg)) => resp::ok_json(&pkg),
            Ok(None) => resp::not_found(&format!("Package {org}/{block} not found")),
            Err(e) => resp::internal(&e.to_string()),
        },
        [org, block, ver] => match db::get_version(ctx, org, block, ver).await {
            Ok(Some(v)) => resp::ok_json(&v),
            Ok(None) => resp::not_found(&format!("{org}/{block}@{ver} not found")),
            Err(e) => resp::internal(&e.to_string()),
        },
        _ => resp::bad_request("expected /registry/api/packages/{org}/{block}[/{version}]"),
    }
}
