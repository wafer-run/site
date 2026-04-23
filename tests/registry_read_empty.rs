//! End-to-end HTTP tests for the registry's public JSON read endpoints,
//! exercised against a freshly-seeded (empty-of-packages) in-memory stack.
//!
//! Verifies the response envelopes documented in the Task 9 plan:
//!
//! - `GET /registry/search`                  -> 200 with the full envelope.
//! - `GET /registry/api/packages/<o>/<b>`    -> 404 with `{error:"not-found"}`.
//! - `GET /registry/api/packages/<o>/<b>/v`  -> 404.
//!
//! Relies on the real axum dispatch path wired up in `tests/common/mod.rs`
//! (see `start_test_site`) — no custom shim or stubbed block handler.

mod common;

use common::start_test_site;

#[tokio::test]
async fn search_empty_returns_well_formed_shape() {
    let app = start_test_site().await;
    let resp = app.get("/registry/search").await;
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.expect("parse search json");

    assert_eq!(json["total"], 0);
    assert_eq!(json["query"], "");
    assert!(
        json["packages"]
            .as_array()
            .expect("packages is array")
            .is_empty(),
        "expected empty packages array, got {:?}",
        json["packages"]
    );
    assert_eq!(json["page"], 1);
    assert_eq!(json["page_size"], 20);
}

#[tokio::test]
async fn get_unknown_package_404() {
    let app = start_test_site().await;
    let resp = app.get("/registry/api/packages/acme/widget").await;
    assert_eq!(resp.status(), 404);
    let json: serde_json::Value = resp.json().await.expect("parse 404 json");
    assert_eq!(json["error"], "not-found");
}

#[tokio::test]
async fn get_unknown_version_404() {
    let app = start_test_site().await;
    let resp = app.get("/registry/api/packages/acme/widget/1.0.0").await;
    assert_eq!(resp.status(), 404);
    let json: serde_json::Value = resp.json().await.expect("parse 404 json");
    assert_eq!(json["error"], "not-found");
}
