//! Integration tests for `POST /registry/api/publish` (Task 13).
//!
//! Covers the six scenarios in the plan:
//!
//! 1. Admin publish happy path.
//! 2. Admin publish duplicate version → 409.
//! 3. Non-admin publish → 403 "coming-soon".
//! 4. Unauthenticated publish → 401.
//! 5. Reserved-org publish by admin → 200.
//! 6. Invalid manifest → 422.
//!
//! Tests drive the real axum/reqwest HTTP path, same as `registry_read_empty`.
//! The in-memory harness wires a `wafer-run/storage` block (LocalStorage on
//! a tempdir) + a minimal `suppers-ai/auth` stub keyed by a seeded `user_id
//! -> email` map. PATs are seeded directly into the registry's `TOKENS`
//! collection by `start_test_site_with_admin` / `start_test_site_with_user`.

mod common;

use common::{make_tarball, start_test_site_with_admin, start_test_site_with_user, TestApp};

/// Shortcut: POST a multipart `tarball` part with the given Bearer token.
async fn publish(app: &TestApp, token: &str, bytes: Vec<u8>) -> reqwest::Response {
    let form = reqwest::multipart::Form::new().part(
        "tarball",
        reqwest::multipart::Part::bytes(bytes)
            .file_name("w.wafer")
            .mime_str("application/octet-stream")
            .unwrap(),
    );
    app.post_multipart("/registry/api/publish", form, Some(token)).await
}

#[tokio::test]
async fn admin_publish_happy_path() {
    let app = start_test_site_with_admin("admin@example.com").await;

    let resp = publish(&app, &app.admin_token, make_tarball("acme", "widget", "0.1.0")).await;
    assert_eq!(resp.status(), 200, "publish should succeed for admin");

    let json: serde_json::Value = resp.json().await.expect("publish response json");
    assert_eq!(json["package"], "acme/widget");
    assert_eq!(json["version"], "0.1.0");
    assert_eq!(
        json["download_url"],
        "/registry/download/acme/widget/0.1.0.wafer"
    );
    assert!(
        json["sha256"]
            .as_str()
            .map(|s| s.len() == 64)
            .unwrap_or(false),
        "sha256 is 64 hex chars: {:?}",
        json["sha256"]
    );

    // Round-trip via the public read endpoint — proves the DB insert landed
    // and the org got auto-created.
    let detail = app.get("/registry/api/packages/acme/widget").await;
    assert_eq!(detail.status(), 200, "detail endpoint finds new package");
    let dj: serde_json::Value = detail.json().await.unwrap();
    assert_eq!(dj["org"], "acme");
    assert_eq!(dj["name"], "widget");
    assert_eq!(dj["versions"][0]["version"], "0.1.0");
}

#[tokio::test]
async fn admin_publish_409_on_duplicate() {
    let app = start_test_site_with_admin("admin@example.com").await;

    let bytes = make_tarball("acme", "widget", "0.1.0");

    let first = publish(&app, &app.admin_token, bytes.clone()).await;
    assert_eq!(first.status(), 200, "first publish should succeed");

    let second = publish(&app, &app.admin_token, bytes).await;
    assert_eq!(second.status(), 409, "duplicate publish should 409");
    let json: serde_json::Value = second.json().await.unwrap();
    assert_eq!(json["error"], "version-exists");
}

#[tokio::test]
async fn non_admin_publish_403_coming_soon() {
    let app = start_test_site_with_user("nonadmin@example.com", "admin@example.com").await;

    let resp = publish(&app, &app.user_token, make_tarball("acme", "widget", "0.1.0")).await;
    assert_eq!(resp.status(), 403, "non-admin must get coming-soon");
    let json: serde_json::Value = resp.json().await.expect("coming-soon json");
    assert_eq!(json["error"], "coming-soon");
}

#[tokio::test]
async fn unauthenticated_publish_401() {
    let app = start_test_site_with_admin("admin@example.com").await;

    let form = reqwest::multipart::Form::new().part(
        "tarball",
        reqwest::multipart::Part::bytes(make_tarball("acme", "widget", "0.1.0"))
            .file_name("w.wafer")
            .mime_str("application/octet-stream")
            .unwrap(),
    );
    let resp = app
        .post_multipart("/registry/api/publish", form, None)
        .await;
    assert_eq!(resp.status(), 401, "no auth header → 401");
}

#[tokio::test]
async fn reserved_org_publish_by_admin() {
    let app = start_test_site_with_admin("admin@example.com").await;

    // `wafer-run` is seeded as a reserved org on block Init (see
    // `db::RESERVED_ORGS`). Admins can publish into reserved orgs — there's
    // no special path, just the normal admin gate.
    let resp = publish(
        &app,
        &app.admin_token,
        make_tarball("wafer-run", "sqlite", "0.2.0"),
    )
    .await;
    assert_eq!(resp.status(), 200, "admin publishes into reserved org");
}

#[tokio::test]
async fn publish_bad_manifest_422() {
    let app = start_test_site_with_admin("admin@example.com").await;

    let resp = publish(
        &app,
        &app.admin_token,
        make_tarball("acme", "widget", "not-semver"),
    )
    .await;
    assert_eq!(resp.status(), 422, "bad semver → 422");
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["error"], "invalid-tarball");
}
