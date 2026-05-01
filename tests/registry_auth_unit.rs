//! Unit tests for the Task-11 admin gate.
//!
//! The registry-bootstrap harness doesn't wire `suppers-ai/auth`, so tests
//! that exercise the auth-block delegation path are deferred to Tasks 12–13
//! (which need a full solobase runtime anyway). What we can cover here:
//!
//! 1. `require_user`: bearer-PAT resolution against `registry_tokens`.
//! 2. `require_admin`: non-admin email (empty, because `fetch_email` has
//!    nothing to call) falls through to the 403 coming-soon JSON branch.
//!
//! Both tests talk to the registry block's in-memory harness directly —
//! no HTTP.

mod common;

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use wafer_core::clients::database as db;
use wafer_run::types::Message;
use wafer_run::OutputStream;

use wafer_site::blocks::registry::{
    self,
    auth::{require_admin, require_user},
    RegistryConfig,
};

fn test_cfg() -> RegistryConfig {
    RegistryConfig {
        admin_email: "admin@example.com".into(),
        storage_key_prefix: "registry".into(),
        jwt_secret: "test-secret".into(),
        required_auth_method: String::new(),
    }
}

/// Reads the HTTP status code out of an `OutputStream` by draining it —
/// mirrors what the real HTTP adapter does with `META_RESP_STATUS`.
async fn status_of(out: OutputStream) -> u16 {
    let buf = out
        .collect_buffered()
        .await
        .expect("status-carrying response");
    buf.meta
        .iter()
        .find(|m| m.key == wafer_run::meta::META_RESP_STATUS)
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
        .find(|m| m.key == wafer_run::meta::META_RESP_STATUS)
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
    data.insert("name".into(), serde_json::json!("test"));
    data.insert("hash".into(), serde_json::json!(hash));
    db::create(ctx.as_ref(), registry::db::TOKENS, data)
        .await
        .expect("insert token");

    // Dispatch through `require_user` with the bearer token. `fetch_email`
    // will fail (no `suppers-ai/auth` registered in this ctx) and return
    // `""`, so we assert on id only.
    let mut msg = Message::new("retrieve");
    msg.set_meta("http.header.authorization", format!("Bearer {raw}"));

    // `OutputStream` isn't `Debug`, so we can't use `.expect`.
    let Ok(user) = require_user(ctx.as_ref(), &msg, &test_cfg()).await else {
        panic!("bearer PAT resolves to AuthedUser");
    };
    assert_eq!(user.id, "u1");
    assert_eq!(
        user.email, "",
        "fetch_email returns empty when suppers-ai/auth is not registered"
    );
}

#[tokio::test]
async fn require_admin_rejects_non_admin_with_coming_soon_json() {
    let ctx = common::boot_registry_against_memory().await;

    // Same PAT seeding as above — user "u1" whose email we can't fetch.
    let raw = "another-secret";
    let hash = hex::encode(Sha256::digest(raw.as_bytes()));
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("user_id".into(), serde_json::json!("u1"));
    data.insert("name".into(), serde_json::json!("test"));
    data.insert("hash".into(), serde_json::json!(hash));
    db::create(ctx.as_ref(), registry::db::TOKENS, data)
        .await
        .expect("insert token");

    let cfg = RegistryConfig {
        admin_email: "admin@example.com".into(),
        storage_key_prefix: "registry".into(),
        jwt_secret: "test-secret".into(),
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
async fn missing_credentials_delegates_to_auth_block_and_returns_401() {
    // No PAT, no cookie — the auth-block delegation path runs and fails
    // (no `suppers-ai/auth` in the in-memory harness). `require_user` must
    // surface that as the 401 `unauthorized` JSON.
    let ctx = common::boot_registry_against_memory().await;

    let msg = Message::new("retrieve");
    let Err(err_out) = require_user(ctx.as_ref(), &msg, &test_cfg()).await else {
        panic!("no creds should not resolve");
    };

    assert_eq!(status_of(err_out).await, 401);
}
