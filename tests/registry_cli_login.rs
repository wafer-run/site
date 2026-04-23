//! CLI login flow tests — Task 12.
//!
//! These exercise `db::issue_cli_code`, `db::exchange_cli_code`, and
//! `db::resolve_bearer` directly against the in-memory registry harness.
//!
//! Why direct-to-module rather than HTTP-integration:
//! `tests/common::start_test_site` wires only the registry block, no
//! `suppers-ai/auth`. That means `auth::require_admin` can't resolve a
//! session cookie to an admin user — the admin-gated `GET
//! /registry/cli-login` path returns the coming-soon page even for the
//! intended admin. HTTP-integration coverage for the full round-trip
//! belongs in Task 13's end-to-end harness (which must stand up
//! `suppers-ai/auth` for publish-path reasons anyway). Here we assert the
//! logic the route handlers delegate to; the routes themselves are one-to-
//! one wrappers around these helpers.

mod common;

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use wafer_core::clients::database as db;

use wafer_site::blocks::registry;

/// `issue_cli_code` returns a 64-hex-char code and persists it to the
/// `CODES` collection with a future expiry and no `used_at`.
#[tokio::test]
async fn issue_cli_code_persists_code_row() {
    let ctx = common::boot_registry_against_memory().await;

    let code = registry::db::issue_cli_code(ctx.as_ref(), "u1")
        .await
        .expect("issue code");

    assert_eq!(code.len(), 64, "hex-encoded 32 bytes = 64 chars");
    assert!(code.chars().all(|c| c.is_ascii_hexdigit()));

    // Row exists in CODES.
    let row = db::get_by_field(ctx.as_ref(), registry::db::CODES, "code", serde_json::json!(code))
        .await
        .expect("fetch code row");
    assert_eq!(row.data.get("user_id").and_then(|v| v.as_str()), Some("u1"));
    // `used_at` must be absent (optional field, not set at issuance time).
    assert!(
        row.data
            .get("used_at")
            .map(|v| matches!(v, serde_json::Value::Null))
            .unwrap_or(true),
        "used_at should be null/absent at issuance"
    );
}

/// Happy path: issue a code, exchange it, get back a `wafer_pat_` token.
/// The token's sha256 must be stored in `TOKENS` linked to the same
/// user_id.
#[tokio::test]
async fn exchange_happy_path_issues_pat_and_stores_hash() {
    let ctx = common::boot_registry_against_memory().await;

    let code = registry::db::issue_cli_code(ctx.as_ref(), "admin-user")
        .await
        .expect("issue code");

    let (user_id, token) = registry::db::exchange_cli_code(ctx.as_ref(), &code)
        .await
        .expect("exchange ok")
        .expect("exchange yields Some");

    assert_eq!(user_id, "admin-user");
    assert!(token.starts_with("wafer_pat_"), "token: {token}");
    // 10 byte prefix + 64 hex chars
    assert_eq!(token.len(), "wafer_pat_".len() + 64);

    // Token row lives in TOKENS keyed by sha256(token).
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let tok_row = db::get_by_field(ctx.as_ref(), registry::db::TOKENS, "hash", serde_json::json!(hash))
        .await
        .expect("fetch token row");
    assert_eq!(
        tok_row.data.get("user_id").and_then(|v| v.as_str()),
        Some("admin-user")
    );
}

/// Double-exchange: first consumes the code, second returns Ok(None)
/// (mapped to 404 by the route handler).
#[tokio::test]
async fn exchange_twice_returns_none() {
    let ctx = common::boot_registry_against_memory().await;

    let code = registry::db::issue_cli_code(ctx.as_ref(), "admin-user")
        .await
        .expect("issue code");

    let first = registry::db::exchange_cli_code(ctx.as_ref(), &code)
        .await
        .expect("first exchange ok");
    assert!(first.is_some(), "first exchange must yield a PAT");

    let second = registry::db::exchange_cli_code(ctx.as_ref(), &code)
        .await
        .expect("second exchange ok");
    assert!(
        second.is_none(),
        "second exchange must return None (code already used)"
    );
}

/// Unknown code exchanges to Ok(None), not an error.
#[tokio::test]
async fn exchange_unknown_code_returns_none() {
    let ctx = common::boot_registry_against_memory().await;

    let res = registry::db::exchange_cli_code(ctx.as_ref(), "not-a-real-code")
        .await
        .expect("lookup ok");
    assert!(res.is_none());
}

/// `resolve_bearer` returns the `user_id` for a valid, non-revoked token.
#[tokio::test]
async fn resolve_bearer_returns_user_id_for_valid_token() {
    let ctx = common::boot_registry_against_memory().await;

    // Mint via the exchange path so the token hash lands in TOKENS.
    let code = registry::db::issue_cli_code(ctx.as_ref(), "u1")
        .await
        .expect("issue");
    let (_uid, token) = registry::db::exchange_cli_code(ctx.as_ref(), &code)
        .await
        .expect("exchange ok")
        .expect("some");

    let resolved = registry::db::resolve_bearer(ctx.as_ref(), &token)
        .await
        .expect("resolve ok");
    assert_eq!(resolved.as_deref(), Some("u1"));
}

/// Revoked tokens (those with `revoked_at` set) resolve to `None`.
#[tokio::test]
async fn resolve_bearer_skips_revoked_tokens() {
    let ctx = common::boot_registry_against_memory().await;

    // Seed a revoked token manually — the exchange path never sets
    // `revoked_at`, so we insert directly.
    let raw = "revoked-secret";
    let hash = hex::encode(Sha256::digest(raw.as_bytes()));
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("user_id".into(), serde_json::json!("u1"));
    data.insert("name".into(), serde_json::json!("test"));
    data.insert("hash".into(), serde_json::json!(hash));
    data.insert(
        "revoked_at".into(),
        serde_json::json!(registry::db::now_unix()),
    );
    db::create(ctx.as_ref(), registry::db::TOKENS, data)
        .await
        .expect("insert revoked token");

    let resolved = registry::db::resolve_bearer(ctx.as_ref(), raw)
        .await
        .expect("resolve ok");
    assert!(resolved.is_none(), "revoked tokens must not resolve");
}

/// Unknown token hashes resolve to `None` (not Err).
#[tokio::test]
async fn resolve_bearer_unknown_token_returns_none() {
    let ctx = common::boot_registry_against_memory().await;

    let resolved = registry::db::resolve_bearer(ctx.as_ref(), "never-issued-token")
        .await
        .expect("resolve ok");
    assert!(resolved.is_none());
}
