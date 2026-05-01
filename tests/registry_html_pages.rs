//! End-to-end HTTP tests for the registry's public HTML pages.
//!
//! Verifies the HTML rendering for:
//! - `GET /registry` — registry index/search page
//! - `GET /registry/{org}/{package}` — package detail page
//!
//! Relies on the real axum dispatch path wired up in `tests/common/mod.rs`
//! (see `start_test_site`) — no custom shim or stubbed block handler.

mod common;

use common::start_test_site;

#[tokio::test]
async fn browse_empty_renders_empty_state() {
    let app = start_test_site().await;
    let resp = app.get("/registry").await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.expect("read response body");
    assert!(
        body.contains("No packages published yet"),
        "expected 'No packages published yet' in body, got: {body}"
    );
}

#[tokio::test]
async fn unknown_package_detail_404_template() {
    let app = start_test_site().await;
    let resp = app.get("/registry/acme/widget").await;
    assert_eq!(resp.status(), 404);
    let body = resp.text().await.expect("read response body");
    assert!(body.contains("404"), "expected '404' in body, got: {body}");
    assert!(
        body.contains("acme/widget"),
        "expected 'acme/widget' in body, got: {body}"
    );
}
