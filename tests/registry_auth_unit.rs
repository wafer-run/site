//! Unit tests for the registry's auth gate.
//!
//! 1. `require_user`: bearer-PAT resolution against `registry_tokens`.
//! 2. `require_admin`: the admin email is the registry's
//!    `WAFER_RUN__REGISTRY__ADMIN_EMAIL` config variable. A PAT carrying it
//!    is admitted; one carrying another email, or any email while the
//!    variable is unset, gets the 403 coming-soon JSON; and a required auth
//!    method the PAT does not have gets the 403 auth-method-required JSON.
//! 3. `require_user`: no credential at all is a 401.
//! 4. `require_user`: an impresspress session token is checked by
//!    impresspress's verifier — a signed-in session resolves, and the same
//!    token after logout (its session row deleted) does not.
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
    config::{ADMIN_EMAIL_KEY, REQUIRED_AUTH_METHOD_KEY},
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

/// A request carrying a registry PAT minted for `email`.
async fn pat_request(ctx: &common::RegistryCtx, email: &str) -> Message {
    let raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    common::seed_token(ctx, "u1", email, &raw).await;
    let mut msg = Message::new("retrieve");
    msg.set_meta("http.header.authorization", format!("Bearer {raw}"));
    // No `accept` header → JSON response branch.
    msg
}

/// The 403 body's `error` tag.
async fn refusal_tag(out: OutputStream) -> String {
    let (status, body) = status_and_body(out).await;
    assert_eq!(status, 403);
    let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    body["error"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn require_admin_admits_the_configured_admin_email_only() {
    let ctx = common::boot_registry_with_config(&[(ADMIN_EMAIL_KEY, "admin@example.com")]).await;

    // The configured address, in any ASCII case, is the admin.
    let msg = pat_request(&ctx, "Admin@Example.com").await;
    let Ok(admin) = require_admin(ctx.as_ref(), &msg).await else {
        panic!("the configured admin email is admitted");
    };
    assert_eq!(admin.email, "Admin@Example.com");

    let msg = pat_request(&ctx, "u1@example.com").await;
    let Err(out) = require_admin(ctx.as_ref(), &msg).await else {
        panic!("another email is not the admin");
    };
    assert_eq!(refusal_tag(out).await, "coming-soon");
}

#[tokio::test]
async fn require_admin_admits_nobody_while_the_admin_email_is_unset() {
    // A deployment that never set the variable: publishing is closed, and
    // the registry still serves (`Init` ran above).
    let ctx = common::boot_registry_with_config(&[]).await;

    let msg = pat_request(&ctx, "admin@example.com").await;
    let Err(out) = require_admin(ctx.as_ref(), &msg).await else {
        panic!("no email is the admin while the variable is unset");
    };
    assert_eq!(refusal_tag(out).await, "coming-soon");
}

#[tokio::test]
async fn require_admin_enforces_the_configured_auth_method() {
    let ctx = common::boot_registry_with_config(&[
        (ADMIN_EMAIL_KEY, "admin@example.com"),
        (REQUIRED_AUTH_METHOD_KEY, "oauth.github"),
    ])
    .await;

    // A PAT's auth method is `pat`, not the required one.
    let msg = pat_request(&ctx, "admin@example.com").await;
    let Err(out) = require_admin(ctx.as_ref(), &msg).await else {
        panic!("the admin signed in another way is refused");
    };
    assert_eq!(refusal_tag(out).await, "auth-method-required");
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
/// logout route does not: logout deletes the session row of the token's
/// login family, and the registry checks session tokens with impresspress's
/// verifier (the session-row and `auth_version` reads included), not a copy
/// of its signature check.
#[tokio::test]
async fn session_token_resolves_until_logout() {
    let ctx = common::boot_registry_against_memory().await;
    let email = "someone@example.com";
    let password = "correct horse battery staple";
    let user_id = ctx
        .seed_account(
            email,
            password,
            impresspress_core::blocks::auth::repo::users::InitialRole::User,
        )
        .await;
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
