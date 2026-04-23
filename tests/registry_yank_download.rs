//! Integration tests for Task 14 — yank/unyank, download, and `/api/me`.
//!
//! Coverage matches the plan's Step 5 scenarios:
//!
//! 1. `yank_then_latest_resolution` — yanking a version drops it from
//!    `/registry/search`'s `latest` field, but the tarball still downloads.
//! 2. `unyank_restores_version` — clearing the flag brings `latest` back.
//! 3. `me_returns_admin_flag` — admin bearer returns `is_admin: true`.
//! 4. `me_unauthenticated_401` — no auth returns 401.
//!
//! Shares the `make_tarball` helper with `registry_publish.rs` via
//! `tests/common/mod.rs` — both files go through the same in-memory SQLite +
//! LocalStorage harness.

mod common;

use common::{make_tarball, start_test_site_with_admin, TestApp};

/// Publish `{org}/{name}@{version}` via the admin PAT. Asserts 200 so the
/// test fails fast on a broken publish path instead of leaking into the
/// yank/download assertions below.
async fn publish(app: &TestApp, org: &str, name: &str, ver: &str) {
    let bytes = make_tarball(org, name, ver);
    let form = reqwest::multipart::Form::new().part(
        "tarball",
        reqwest::multipart::Part::bytes(bytes)
            .file_name("w.wafer")
            .mime_str("application/octet-stream")
            .unwrap(),
    );
    let resp = app
        .post_multipart("/registry/api/publish", form, Some(&app.admin_token))
        .await;
    assert_eq!(resp.status(), 200, "publish {org}/{name}@{ver} should succeed");
}

/// POST `/registry/api/packages/{org}/{name}/{version}/{action}` with the
/// admin bearer. `body` is an optional JSON payload — yank accepts
/// `{"reason": "..."}`; unyank is body-less.
async fn post_yank_action(
    app: &TestApp,
    org: &str,
    name: &str,
    version: &str,
    action: &str,
    body: Option<serde_json::Value>,
) -> reqwest::Response {
    let url = format!(
        "{}/registry/api/packages/{org}/{name}/{version}/{action}",
        app.base
    );
    let mut req = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&app.admin_token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    req.send().await.expect("yank/unyank request")
}

/// `published_at` is stored with 1-second precision (SQLite `datetime`
/// round-tripped through `now_unix()`), so back-to-back publishes can tie
/// and the latest-version sort becomes insertion-order-dependent. Sleep
/// just over a second between the two publishes so ordering is
/// deterministic across both the yank and the unyank test.
const PUBLISH_TICK: std::time::Duration = std::time::Duration::from_millis(1100);

#[tokio::test]
async fn yank_then_latest_resolution() {
    let app = start_test_site_with_admin("admin@example.com").await;
    publish(&app, "acme", "widget", "0.1.0").await;
    tokio::time::sleep(PUBLISH_TICK).await;
    publish(&app, "acme", "widget", "0.2.0").await;

    // Yank 0.2.0 with a reason.
    let yank = post_yank_action(
        &app,
        "acme",
        "widget",
        "0.2.0",
        "yank",
        Some(serde_json::json!({ "reason": "oops" })),
    )
    .await;
    assert_eq!(yank.status(), 200, "yank should succeed for admin");
    let yj: serde_json::Value = yank.json().await.unwrap();
    assert_eq!(yj["yanked"], true);

    // Latest resolution should now be 0.1.0 — 0.2.0 is hidden from the
    // browse summary but not deleted.
    let browse: serde_json::Value = app
        .get("/registry/search")
        .await
        .json()
        .await
        .expect("search json");
    // Find the acme/widget row (order isn't guaranteed across inserts).
    let pkg = browse["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|p| p["org"] == "acme" && p["name"] == "widget")
        .expect("acme/widget in search results");
    assert_eq!(pkg["latest"], "0.1.0", "latest should skip yanked 0.2.0");

    // Yanked version still downloads — spec §8: yank filters resolution,
    // not direct downloads.
    let dl = app.get("/registry/download/acme/widget/0.2.0.wafer").await;
    assert_eq!(dl.status(), 200, "yanked version should still download");
    let ct = dl
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(ct, "application/octet-stream");
    let cc = dl
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        cc.contains("immutable"),
        "cache-control should mark content immutable: {cc:?}"
    );
    let bytes = dl.bytes().await.unwrap();
    assert!(!bytes.is_empty(), "download body should not be empty");
}

#[tokio::test]
async fn unyank_restores_version() {
    let app = start_test_site_with_admin("admin@example.com").await;
    publish(&app, "acme", "widget", "0.1.0").await;
    tokio::time::sleep(PUBLISH_TICK).await;
    publish(&app, "acme", "widget", "0.2.0").await;

    let y = post_yank_action(&app, "acme", "widget", "0.2.0", "yank", None).await;
    assert_eq!(y.status(), 200);

    let u = post_yank_action(&app, "acme", "widget", "0.2.0", "unyank", None).await;
    assert_eq!(u.status(), 200, "unyank should succeed");
    let uj: serde_json::Value = u.json().await.unwrap();
    assert_eq!(uj["yanked"], false);

    let browse: serde_json::Value = app.get("/registry/search").await.json().await.unwrap();
    let pkg = browse["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|p| p["org"] == "acme" && p["name"] == "widget")
        .expect("acme/widget in search results");
    assert_eq!(pkg["latest"], "0.2.0", "latest should restore to 0.2.0");
}

#[tokio::test]
async fn yank_unknown_version_404() {
    let app = start_test_site_with_admin("admin@example.com").await;
    // No publish — the (org, pkg, version) triple doesn't exist.
    let resp = post_yank_action(&app, "acme", "widget", "0.1.0", "yank", None).await;
    assert_eq!(resp.status(), 404, "yank of unknown version should 404");
    let j: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(j["error"], "not-found");
}

#[tokio::test]
async fn yank_unauthenticated_401() {
    let app = start_test_site_with_admin("admin@example.com").await;
    let url = format!("{}/registry/api/packages/acme/widget/0.1.0/yank", app.base);
    let resp = reqwest::Client::new()
        .post(&url)
        .send()
        .await
        .expect("yank request");
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn download_unknown_version_404() {
    let app = start_test_site_with_admin("admin@example.com").await;
    let resp = app.get("/registry/download/acme/widget/0.1.0.wafer").await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn me_returns_admin_flag() {
    let app = start_test_site_with_admin("admin@example.com").await;
    let me = reqwest::Client::new()
        .get(format!("{}/registry/api/me", app.base))
        .bearer_auth(&app.admin_token)
        .send()
        .await
        .expect("me request");
    assert_eq!(me.status(), 200);
    let json: serde_json::Value = me.json().await.unwrap();
    assert_eq!(json["email"], "admin@example.com");
    assert_eq!(json["is_admin"], true);
}

#[tokio::test]
async fn me_unauthenticated_401() {
    let app = start_test_site_with_admin("admin@example.com").await;
    let me = reqwest::Client::new()
        .get(format!("{}/registry/api/me", app.base))
        .send()
        .await
        .expect("me request");
    assert_eq!(me.status(), 401);
}
