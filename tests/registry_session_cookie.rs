//! Exercises the session-cookie branch of `registry::auth::require_user`.
//!
//! The pre-existing HTTP-integration tests authenticate via Bearer PAT.
//! This test covers the other branch: no PAT, only the `auth_token` cookie
//! impresspress's real login route set, checked by impresspress's token
//! verifier so the admin-email match lands end-to-end.

mod common;

#[tokio::test]
async fn cli_login_page_succeeds_via_session_cookie() {
    let admin_email = "admin@example.com";
    let app = common::start_test_site_with_admin_cookie(admin_email).await;

    let resp = app.get("/registry/cli-login").await;
    assert_eq!(
        resp.status(),
        200,
        "cookie admin should see the CLI-login page"
    );

    let body = resp.text().await.expect("body");
    // The `cli_login_code` template renders the 64-char hex code inside
    // `<code>…</code>`. We don't care about the exact surrounding HTML —
    // just that a code made it through `db::issue_cli_code`. Scan for
    // any run of 64 hex chars anywhere in the body.
    let chars: Vec<char> = body.chars().collect();
    let has_64_hex_run = chars
        .windows(64)
        .any(|w| w.iter().all(|c| c.is_ascii_hexdigit()));
    assert!(
        has_64_hex_run,
        "expected a 64-hex-char login code in body; got: {body}",
    );
}
