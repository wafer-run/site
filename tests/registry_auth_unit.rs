//! Unit tests for the registry's auth gate.
//!
//! 1. `require_user`: bearer-PAT resolution against `registry_tokens`.
//! 2. `require_admin`: a PAT whose email is not the admin's falls through to
//!    the 403 coming-soon JSON branch.
//! 3. `require_user`: no credential at all is a 401.
//! 4. `require_user`: an impresspress session token is checked by
//!    impresspress's verifier — a signed-in session resolves, and the same
//!    token after logout (blocklisted) does not.
//!
//! The tests talk to the registry block's harness directly — no HTTP.

mod common;

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use wafer_core::clients::database as db;
use wafer_run::{Message, OutputStream};

use wafer_site::blocks::registry::{
    self,
    auth::{require_admin, require_user},
    RegistryConfig,
};

/// Reads the HTTP status code out of an `OutputStream` by draining it —
/// mirrors what the real HTTP adapter does with `META_RESP_STATUS`.
async fn status_of(out: OutputStream) -> u16 {
    let buf = out
        .collect_buffered()
        .await
        .expect("status-carrying response");
    buf.meta
        .iter()
        .find(|m| m.key == wafer_run::META_RESP_STATUS)
        .expect("response carries status meta")
        .value
        .parse()
        .expect("status is numeric")
}

/// Same as `status_of`, but also returns the body. Used for the coming-soon
/// JSON body assertion.
async fn status_and_body(out: OutputStream) -> (u16, Vec<u8>) {
    let buf = out.collect_buffered().await.expect("response");
    let status = buf
        .meta
        .iter()
        .find(|m| m.key == wafer_run::META_RESP_STATUS)
        .expect("status meta")
        .value
        .parse()
        .unwrap();
    (status, buf.body)
}

#[tokio::test]
async fn bearer_pat_resolves_against_registry_tokens() {
    let ctx = common::boot_registry_against_memory().await;

    // Mint a raw token + insert its sha256 into the registry_tokens
    // collection under user "u1". Mirrors what Task 12's exchange endpoint
    // will do.
    let raw = "secret-token-abc";
    let hash = hex::encode(Sha256::digest(raw.as_bytes()));

    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("user_id".into(), serde_json::json!("u1"));
    data.insert("email".into(), serde_json::json!("u1@example.com"));
    data.insert("name".into(), serde_json::json!("test"));
    data.insert("hash".into(), serde_json::json!(hash));
    db::create(ctx.as_ref(), registry::db::TOKENS, data)
        .await
        .expect("insert token");

    // Dispatch through `require_user` with the bearer token. The email
    // stored on the token row at exchange time is now the authoritative
    // source — resolve_bearer returns it directly.
    let mut msg = Message::new("retrieve");
    msg.set_meta("http.header.authorization", format!("Bearer {raw}"));

    // `OutputStream` isn't `Debug`, so we can't use `.expect`.
    let Ok(user) = require_user(ctx.as_ref(), &msg).await else {
        panic!("bearer PAT resolves to AuthedUser");
    };
    assert_eq!(user.id, "u1");
    assert_eq!(user.email, "u1@example.com");
}

#[tokio::test]
async fn require_admin_rejects_non_admin_with_coming_soon_json() {
    let ctx = common::boot_registry_against_memory().await;

    // Same PAT seeding as above — user "u1" whose email we can't fetch.
    let raw = "another-secret";
    let hash = hex::encode(Sha256::digest(raw.as_bytes()));
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("user_id".into(), serde_json::json!("u1"));
    data.insert("email".into(), serde_json::json!("u1@example.com"));
    data.insert("name".into(), serde_json::json!("test"));
    data.insert("hash".into(), serde_json::json!(hash));
    db::create(ctx.as_ref(), registry::db::TOKENS, data)
        .await
        .expect("insert token");

    let cfg = RegistryConfig {
        admin_email: "admin@example.com".into(),
        storage_key_prefix: "registry".into(),
        required_auth_method: String::new(),
    };

    let mut msg = Message::new("retrieve");
    msg.set_meta("http.header.authorization", format!("Bearer {raw}"));
    // No `accept` header → JSON response branch.

    let Err(err_out) = require_admin(ctx.as_ref(), &msg, &cfg).await else {
        panic!("empty email is not the admin email");
    };

    let (status, body) = status_and_body(err_out).await;
    assert_eq!(status, 403);
    let body_str = String::from_utf8(body).expect("utf8 body");
    assert!(
        body_str.contains("coming-soon"),
        "body should tag error as coming-soon: {body_str}"
    );
}

#[tokio::test]
async fn missing_credentials_returns_401() {
    // No PAT, no cookie: `require_user` answers the 401 `unauthorized` JSON.
    let ctx = common::boot_registry_against_memory().await;

    let msg = Message::new("retrieve");
    let Err(err_out) = require_user(ctx.as_ref(), &msg).await else {
        panic!("no creds should not resolve");
    };

    assert_eq!(status_of(err_out).await, 401);
}

/// A session impresspress's real login route issued resolves to its user
/// through the `auth_token` cookie, and the same cookie after the real
/// logout route blocklisted its token does not: the registry checks session
/// tokens with impresspress's verifier (blocklist and `auth_version`
/// included), not a copy of its signature check.
#[tokio::test]
async fn session_token_resolves_until_logout() {
    let ctx = common::boot_registry_against_memory().await;
    let email = "someone@example.com";
    let password = "correct horse battery staple";
    let user_id = ctx.seed_account(email, password, "user").await;
    let session = ctx.sign_in(email, password).await;

    let mut msg = Message::new("retrieve");
    msg.set_meta(
        "http.header.cookie",
        format!("auth_token={}", session.cookie),
    );
    let Ok(user) = require_user(ctx.as_ref(), &msg).await else {
        panic!("a signed-in session resolves");
    };
    assert_eq!(user.id, user_id);
    assert_eq!(user.email, email);
    assert_eq!(user.auth_method, "password");

    let logout = session.bearer(impresspress_core::test_support::anon_msg(
        "create",
        "/b/auth/api/logout",
    ));
    let out = ctx.request(logout).await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    // Logout answers with a redirect back to the site.
    assert_eq!(parts.status, 303, "logout succeeds");

    let Err(out) = require_user(ctx.as_ref(), &msg).await else {
        panic!("a logged-out session must not resolve");
    };
    assert_eq!(status_of(out).await, 401);
}
