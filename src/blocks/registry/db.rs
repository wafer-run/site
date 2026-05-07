//! Typed helpers for the registry block's collections.
//!
//! No raw SQL — all access goes through `wafer_core::clients::database`.
//! Owns the table-name constants, reserved-orgs seed, and every query helper
//! used by the route handlers under `routes/`.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rand::RngCore;
use serde_json::json;
use sha2::{Digest, Sha256};
use wafer_core::clients::database::{self as db, Filter, FilterOp, ListOptions, Record, SortField};
use wafer_run::context::Context;

use crate::blocks::registry::models::{
    PackageDetail, PackageSummary, VersionDetail, VersionSummary,
};

// ---- Collection names ------------------------------------------------------

// Collection names follow the WRAP `{org}__{block}__{name}` namespace
// convention (`wafer-run/registry` → prefix `wafer_run__registry__`). Without
// the prefix, WRAP rejects every `call_block("wafer-run/database", ...)`
// invocation from this block with
// `PermissionDenied: unnamespaced resource … denied`.
pub const ORGS: &str = "wafer_run__registry__orgs";
pub const PACKAGES: &str = "wafer_run__registry__packages";
pub const VERSIONS: &str = "wafer_run__registry__versions";
pub const CODES: &str = "wafer_run__registry__cli_login_codes";
pub const TOKENS: &str = "wafer_run__registry__tokens";

/// Reserved org names seeded into `registry_orgs` on block init. Owned by the
/// project: external users cannot claim these names. Kept in alphabetical
/// order to match the test assertion.
pub const RESERVED_ORGS: &[&str] = &["solobase", "suppers-ai", "wafer", "wafer-run"];

// ---- Init / seed -----------------------------------------------------------

/// Seed reserved-orgs rows on block init. Idempotent — probes each org with
/// `get_by_field` before inserting, so reboot doesn't duplicate rows.
pub async fn seed_reserved_orgs(ctx: &dyn Context) -> Result<()> {
    for &name in RESERVED_ORGS {
        match db::get_by_field(ctx, ORGS, "name", json!(name)).await {
            Ok(_) => continue, // already present
            Err(e) if is_not_found(&e) => {
                // fall through to create
            }
            Err(e) => {
                return Err(anyhow::anyhow!("probing reserved org {name}: {e:?}"));
            }
        }
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("name".into(), json!(name));
        data.insert("is_reserved".into(), json!(true));
        // owner_user_id, verified_via, verified_ref left unset (optional).
        db::create(ctx, ORGS, data)
            .await
            .map_err(|e| anyhow::anyhow!("creating reserved org {name}: {e:?}"))?;
    }
    Ok(())
}

/// Whether a `WaferError` from the typed database client represents a
/// not-found condition (the row being probed doesn't exist).
fn is_not_found(err: &wafer_run::WaferError) -> bool {
    err.code == wafer_run::ErrorCode::NotFound
}

/// Ensure the storage folder `prefix` exists.
///
/// Publish writes `{prefix}/{org}/{name}/{version}.wafer` via
/// `storage::put`. The LocalStorageService backend `create_dir_all`s on
/// demand, but S3 has no implicit "folder exists" concept — a fresh bucket
/// without the top-level prefix configured would surface first-publish
/// errors in production. Creating the folder on block init sidesteps that.
///
/// The call is idempotent in practice: LocalStorageService's
/// `create_folder` uses `fs::create_dir_all` (succeeds whether or not the
/// folder exists), and S3-style backends typically surface "already
/// exists" via an `AlreadyExists` error code. We tolerate any error here
/// — the worst case is a no-op on a folder that already exists, and the
/// real failure surface is the subsequent `storage::put` path.
pub async fn ensure_storage_folder(ctx: &dyn Context, prefix: &str) -> Result<()> {
    match wafer_core::clients::storage::create_folder(ctx, prefix, false).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Idempotency: on a re-run where the folder already exists,
            // accept whatever error the backend raises. `LocalStorage`
            // won't raise (create_dir_all is a no-op); S3-style backends
            // raise `AlreadyExists` or similar. Logging rather than
            // propagating keeps block init resilient across both paths.
            tracing::debug!(
                prefix,
                code = ?e.code,
                message = %e.message,
                "ensure_storage_folder: folder probably already exists, continuing"
            );
            Ok(())
        }
    }
}

// ---- Query helpers --------------------------------------------------------
//
// Typed read helpers that back the HTTP route handlers. Every call goes
// through `wafer_core::clients::database`; raw SQL is forbidden (WRAP-gated).
// JOINs are done client-side via a second lookup — the typed API has no JOIN
// operator.

/// Build an `Eq` filter over a single field. Small sugar so every helper
/// below doesn't repeat the full `Filter { field, operator, value }` literal.
fn eq(field: &str, value: serde_json::Value) -> Filter {
    Filter {
        field: field.into(),
        operator: FilterOp::Equal,
        value,
    }
}

/// Tolerant i64 extraction.
///
/// SQLite stores `DATETIME` and `INTEGER` columns as their native types when
/// the schema was declared via `CollectionSchema`, but falls back to `TEXT`
/// when a table is auto-created from raw `db::create` data (see
/// `wafer-block-sqlite::service::ensure_table`). So a value round-tripped
/// from the DB can come back as either `Number(n)` or `String("n")`. Accept
/// both.
fn field_i64(
    data: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
) -> Option<i64> {
    match data.get(key)? {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse::<i64>().ok(),
        serde_json::Value::Bool(b) => Some(if *b { 1 } else { 0 }),
        _ => None,
    }
}

/// Tolerant bool extraction, same motivation as `field_i64`. `"0"`/`"1"`,
/// `"true"`/`"false"`, `Bool(_)`, and `Number(0|non-zero)` all decode.
fn field_bool(data: &std::collections::HashMap<String, serde_json::Value>, key: &str) -> bool {
    match data.get(key) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_i64().map(|i| i != 0).unwrap_or(false),
        Some(serde_json::Value::String(s)) => matches!(s.as_str(), "1" | "true" | "TRUE" | "True"),
        _ => false,
    }
}

/// Find an org by name. Returns `None` if no row matches.
pub async fn find_org_by_name(ctx: &dyn Context, name: &str) -> Result<Option<Record>> {
    match db::get_by_field(ctx, ORGS, "name", json!(name)).await {
        Ok(r) => Ok(Some(r)),
        Err(ref e) if is_not_found(e) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("find_org_by_name({name}): {e:?}")),
    }
}

/// Find a package by `(org_id, name)`. Returns `None` if no row matches.
pub async fn find_package(ctx: &dyn Context, org_id: &str, name: &str) -> Result<Option<Record>> {
    let rows = db::list_all(
        ctx,
        PACKAGES,
        vec![eq("org_id", json!(org_id)), eq("name", json!(name))],
    )
    .await
    .map_err(|e| anyhow::anyhow!("find_package({org_id}, {name}): {e:?}"))?;
    Ok(rows.into_iter().next())
}

/// Latest non-yanked version for a package, sorted by `published_at` desc.
/// Returns `None` if the package has no non-yanked versions.
///
/// Yank filtering is done client-side because column affinity differs
/// between backends (TEXT `"0"` vs INTEGER `0` vs BOOLEAN `false`) and a
/// server-side `yanked = false` filter misses rows on mismatched backends.
/// `field_bool` handles all three representations.
pub async fn latest_version_for(ctx: &dyn Context, package_id: &str) -> Result<Option<Record>> {
    let opts = ListOptions {
        filters: vec![eq("package_id", json!(package_id))],
        sort: vec![SortField {
            field: "published_at".into(),
            desc: true,
        }],
        // Small limit — a package's full version list tops out in the tens;
        // we only need to walk to the first non-yanked row.
        limit: 100,
        offset: 0,
    };
    let res = db::list(ctx, VERSIONS, &opts)
        .await
        .map_err(|e| anyhow::anyhow!("latest_version_for({package_id}): {e:?}"))?;
    Ok(res
        .records
        .into_iter()
        .find(|r| !field_bool(&r.data, "yanked")))
}

/// Browse: list packages matching `query` (case-sensitive `LIKE %q%` on
/// `name`). Returns `(summaries, total)`. The JOIN with `orgs` and
/// `versions` is done client-side — one `db::get(ORGS, org_id)` and one
/// `latest_version_for` per row.
pub async fn list_packages(
    ctx: &dyn Context,
    query: Option<&str>,
    page: i64,
    per_page: i64,
) -> Result<(Vec<PackageSummary>, i64)> {
    let mut pkg_filters: Vec<Filter> = Vec::new();
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        pkg_filters.push(Filter {
            field: "name".into(),
            operator: FilterOp::Like,
            value: json!(format!("%{q}%")),
        });
    }
    let offset = (page - 1).max(0) * per_page;

    let total = db::count(ctx, PACKAGES, &pkg_filters)
        .await
        .map_err(|e| anyhow::anyhow!("list_packages count: {e:?}"))?;

    let opts = ListOptions {
        filters: pkg_filters,
        sort: vec![SortField {
            field: "created_at".into(),
            desc: true,
        }],
        limit: per_page,
        offset,
    };
    let pkgs = db::list(ctx, PACKAGES, &opts)
        .await
        .map_err(|e| anyhow::anyhow!("list_packages list: {e:?}"))?;

    let mut summaries = Vec::with_capacity(pkgs.records.len());
    for p in pkgs.records {
        let org_id = p
            .data
            .get("org_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let org_name = db::get(ctx, ORGS, org_id)
            .await
            .map_err(|e| anyhow::anyhow!("list_packages org lookup({org_id}): {e:?}"))?
            .data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let latest = latest_version_for(ctx, &p.id).await?.and_then(|v| {
            v.data
                .get("version")
                .and_then(|x| x.as_str())
                .map(String::from)
        });
        summaries.push(PackageSummary {
            org: org_name,
            name: p
                .data
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            summary: p
                .data
                .get("summary")
                .and_then(|v| v.as_str())
                .map(String::from),
            latest,
        });
    }

    Ok((summaries, total))
}

/// Package detail — resolves org → package → all versions. Versions sorted
/// by `published_at` desc in Rust (because `list_all` has no sort param).
pub async fn get_package(
    ctx: &dyn Context,
    org: &str,
    name: &str,
) -> Result<Option<PackageDetail>> {
    let Some(org_row) = find_org_by_name(ctx, org).await? else {
        return Ok(None);
    };
    let Some(pkg_row) = find_package(ctx, &org_row.id, name).await? else {
        return Ok(None);
    };

    let mut versions = db::list_all(ctx, VERSIONS, vec![eq("package_id", json!(pkg_row.id))])
        .await
        .map_err(|e| anyhow::anyhow!("get_package versions({}): {e:?}", pkg_row.id))?;

    // Sort desc by published_at: negate the i64 for a stable descending key.
    // `field_i64` handles both INTEGER and TEXT-stored numbers — see the
    // helper's doc comment for why both representations appear.
    versions.sort_by_key(|r| -field_i64(&r.data, "published_at").unwrap_or(0));

    Ok(Some(PackageDetail {
        org: org.to_string(),
        name: name.to_string(),
        summary: pkg_row
            .data
            .get("summary")
            .and_then(|v| v.as_str())
            .map(String::from),
        versions: versions
            .into_iter()
            .map(version_summary_from_record)
            .collect(),
    }))
}

/// Full version detail including manifest fields. `None` if the org,
/// package, or version row is missing.
pub async fn get_version(
    ctx: &dyn Context,
    org: &str,
    name: &str,
    version: &str,
) -> Result<Option<VersionDetail>> {
    let Some(org_row) = find_org_by_name(ctx, org).await? else {
        return Ok(None);
    };
    let Some(pkg_row) = find_package(ctx, &org_row.id, name).await? else {
        return Ok(None);
    };

    let hits = db::list_all(
        ctx,
        VERSIONS,
        vec![
            eq("package_id", json!(pkg_row.id)),
            eq("version", json!(version)),
        ],
    )
    .await
    .map_err(|e| anyhow::anyhow!("get_version({org}/{name}@{version}): {e:?}"))?;

    Ok(hits.into_iter().next().map(|r| VersionDetail {
        org_name: org.into(),
        pkg_name: name.into(),
        version: r
            .data
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        abi: field_i64(&r.data, "abi").unwrap_or(0),
        sha256: r
            .data
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        storage_key: r
            .data
            .get("storage_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        size_bytes: field_i64(&r.data, "size_bytes").unwrap_or(0),
        license: r
            .data
            .get("license")
            .and_then(|v| v.as_str())
            .map(String::from),
        readme_md: r
            .data
            .get("readme_md")
            .and_then(|v| v.as_str())
            .map(String::from),
        dependencies: r
            .data
            .get("dependencies")
            .and_then(|v| v.as_str())
            .map(String::from),
        capabilities: r
            .data
            .get("capabilities")
            .and_then(|v| v.as_str())
            .map(String::from),
        yanked: if field_bool(&r.data, "yanked") { 1 } else { 0 },
        yanked_reason: r
            .data
            .get("yanked_reason")
            .and_then(|v| v.as_str())
            .map(String::from),
        published_at: field_i64(&r.data, "published_at").unwrap_or(0),
    }))
}

// ---- CLI login + token helpers --------------------------------------------
//
// The CLI-login flow issues a short-lived "device code" on the admin-gated
// `/registry/cli-login` page. The CLI exchanges that code for a long-lived
// personal access token (PAT). Both the code and the PAT live in
// collections managed by this block (`CODES` and `TOKENS`) — no raw SQL,
// everything goes through the typed `db::*` API.

/// Current time as seconds since epoch. Datetime fields on `CollectionSchema`
/// round-trip as integers in SQLite today (stored as TEXT but decoded by
/// `field_i64`), so storing `i64` here keeps reads simple.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Sha256-hex of `input` — shared between `exchange_cli_code` (hashing the
/// token for storage) and `resolve_bearer` (hashing the presented token for
/// lookup).
fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Issue a new CLI-login device code for `user_id`. Returns the raw 64-char
/// hex code; caller displays it to the admin who pastes it into the CLI.
///
/// Codes are single-use and expire 15 minutes after issuance. The unused /
/// unexpired check runs in [`exchange_cli_code`].
pub async fn issue_cli_code(ctx: &dyn Context, user_id: &str, email: &str) -> Result<String> {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let code = hex::encode(buf);
    let now = now_unix();
    let expires = now + 15 * 60;

    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("code".into(), json!(code));
    data.insert("user_id".into(), json!(user_id));
    data.insert("email".into(), json!(email));
    data.insert("expires_at".into(), json!(expires));
    // Declare `used_at` explicitly as null at issuance. The sqlite service's
    // auto-table-creation path (`ensure_table`) builds columns from the keys
    // of the first-inserted row; if we omit `used_at` here, the column
    // doesn't exist when `exchange_cli_code` later tries to update it and
    // sqlite's UPDATE errors with "no such column". Inserting a null value
    // ensures the column is present from the start. Unrelated to the
    // `CollectionSchema` declared on the block — the schema only applies
    // when a manifest-driven runtime materializes it, which our in-memory
    // test harness skips.
    data.insert("used_at".into(), serde_json::Value::Null);

    db::create(ctx, CODES, data)
        .await
        .map_err(|e| anyhow::anyhow!("issue_cli_code: {e:?}"))?;

    Ok(code)
}

/// Exchange a device `code` for a newly-minted PAT.
///
/// Returns:
/// - `Ok(Some((user_id, token_plain)))` on success. `token_plain` is the raw
///   PAT (`wafer_pat_<64hex>`) — the caller must surface it to the client
///   since the hash is all we persist.
/// - `Ok(None)` when the code is missing, expired, or already used — a flat
///   "invalid" signal so the route doesn't leak which condition tripped.
/// - `Err(...)` only for backend errors.
pub async fn exchange_cli_code(ctx: &dyn Context, code: &str) -> Result<Option<(String, String)>> {
    let now = now_unix();

    // Look up the code row. Absent code => Ok(None). `get_by_field` maps
    // "not found" to `NotFound`, which we squash to `None`.
    let row = match db::get_by_field(ctx, CODES, "code", json!(code)).await {
        Ok(r) => r,
        Err(ref e) if is_not_found(e) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("exchange_cli_code lookup: {e:?}")),
    };

    // Expired or already used?
    let expires_at = field_i64(&row.data, "expires_at").unwrap_or(0);
    let already_used = row
        .data
        .get("used_at")
        .map(|v| match v {
            serde_json::Value::Null => false,
            serde_json::Value::String(s) => !s.is_empty(),
            _ => true,
        })
        .unwrap_or(false);
    if expires_at < now || already_used {
        return Ok(None);
    }

    let user_id = row
        .data
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if user_id.is_empty() {
        // Shouldn't happen (user_id is required on the schema), but surface
        // it as "invalid" rather than minting a PAT for nobody.
        return Ok(None);
    }
    // Email was captured at issue_cli_code time (cookie session was present
    // then). Copy it onto the token so downstream bearer-path admin checks
    // don't need a cross-block profile lookup.
    let email = row
        .data
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Mint the PAT.
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let token_plain = format!("wafer_pat_{}", hex::encode(buf));
    let token_hash = sha256_hex(&token_plain);

    // Mark the code as used first. The typed API updates by `id`, not by the
    // `code` field — we use the row we already fetched.
    let mut code_update: HashMap<String, serde_json::Value> = HashMap::new();
    code_update.insert("used_at".into(), json!(now));
    db::update(ctx, CODES, &row.id, code_update)
        .await
        .map_err(|e| anyhow::anyhow!("exchange_cli_code mark-used: {e:?}"))?;

    // Insert the token row. Store the hash, not the raw PAT.
    let mut tok_data: HashMap<String, serde_json::Value> = HashMap::new();
    tok_data.insert("user_id".into(), json!(user_id));
    tok_data.insert("email".into(), json!(email));
    tok_data.insert("name".into(), json!("wafer-cli"));
    tok_data.insert("hash".into(), json!(token_hash));
    // `last_used_at` + `revoked_at` left unset.
    db::create(ctx, TOKENS, tok_data)
        .await
        .map_err(|e| anyhow::anyhow!("exchange_cli_code create-token: {e:?}"))?;

    Ok(Some((user_id, token_plain)))
}

/// Resolve a raw bearer token to its owning `user_id`.
///
/// Sha256s the presented token, looks up the `TOKENS` collection by `hash`,
/// and returns the `user_id` if the row exists and isn't revoked. Any
/// missing / revoked / backend-error case squashes to `Ok(None)` so the
/// caller can fall through to the next auth strategy without branching on
/// error kinds.
///
/// Centralized so any future token shape change (rotation, last-used
/// bookkeeping) lands in one place rather than across `auth::require_user`.
pub async fn resolve_bearer(
    ctx: &dyn Context,
    token_plain: &str,
) -> Result<Option<(String, String)>> {
    let hash = sha256_hex(token_plain);
    match db::get_by_field(ctx, TOKENS, "hash", json!(hash)).await {
        Ok(row) => {
            let revoked = row
                .data
                .get("revoked_at")
                .map(|v| match v {
                    serde_json::Value::Null => false,
                    serde_json::Value::String(s) => !s.is_empty(),
                    _ => true,
                })
                .unwrap_or(false);
            if revoked {
                return Ok(None);
            }
            let user_id = row
                .data
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let email = row
                .data
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if user_id.is_empty() {
                Ok(None)
            } else {
                Ok(Some((user_id, email)))
            }
        }
        Err(ref e) if is_not_found(e) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("resolve_bearer: {e:?}")),
    }
}

// ---- Publish helpers ------------------------------------------------------
//
// Writes that land a new `{org}/{name}@{version}` row. Every helper goes
// through the typed `wafer_core::clients::database` API — no raw SQL.
//
// Schema-drift note: the in-memory harness auto-creates tables from the
// first-inserted row's keys. So every nullable `VERSIONS` column
// (`license`, `readme_md`, `yanked_reason`, `yanked_at`) must be present in
// the first insert, even when absent from the uploaded manifest. We write
// `Value::Null` for every one — the production CollectionSchema path
// tolerates this, and the in-memory path won't drop columns we'll later
// `update` in the yank flow.

/// Get-or-create an org row by name.
///
/// - If an `orgs` row with `name == org` exists, its id is returned verbatim.
/// - Otherwise a new row is inserted with `owner_user_id = owner_user_id`
///   (stored as a string — the schema declares it that way) and the
///   requested `is_reserved` flag. Reserved rows get `owner_user_id = null`
///   so they remain unowned even after a publish.
pub async fn upsert_org(
    ctx: &dyn Context,
    name: &str,
    owner_user_id: &str,
    is_reserved: bool,
) -> Result<String> {
    if let Some(existing) = find_org_by_name(ctx, name).await? {
        return Ok(existing.id);
    }
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("name".into(), json!(name));
    data.insert("is_reserved".into(), json!(is_reserved));
    // Reserved orgs are project-owned; surface that by leaving the owner
    // explicitly null rather than pointing at whoever happened to seed them.
    if is_reserved {
        data.insert("owner_user_id".into(), serde_json::Value::Null);
    } else {
        data.insert("owner_user_id".into(), json!(owner_user_id));
    }
    let row = db::create(ctx, ORGS, data)
        .await
        .map_err(|e| anyhow::anyhow!("upsert_org({name}): {e:?}"))?;
    Ok(row.id)
}

/// Whether a version row exists for `{org}/{name}@{version}`. Resolves via
/// the client-side org → package → versions walk; returns `false` if any
/// step is missing (nothing to dedupe against).
pub async fn version_exists(
    ctx: &dyn Context,
    org: &str,
    name: &str,
    version: &str,
) -> Result<bool> {
    let Some(org_row) = find_org_by_name(ctx, org).await? else {
        return Ok(false);
    };
    let Some(pkg_row) = find_package(ctx, &org_row.id, name).await? else {
        return Ok(false);
    };
    let hits = db::list_all(
        ctx,
        VERSIONS,
        vec![
            eq("package_id", json!(pkg_row.id)),
            eq("version", json!(version)),
        ],
    )
    .await
    .map_err(|e| anyhow::anyhow!("version_exists({org}/{name}@{version}): {e:?}"))?;
    Ok(!hits.is_empty())
}

/// Whether the `{org}` is reserved (project-owned). Returns `false` if the
/// org row doesn't exist — a not-yet-created org can't be reserved.
pub async fn is_reserved(ctx: &dyn Context, org_name: &str) -> Result<bool> {
    match find_org_by_name(ctx, org_name).await? {
        Some(row) => Ok(field_bool(&row.data, "is_reserved")),
        None => Ok(false),
    }
}

/// Insert (or get-or-create) the package row for `{org_id, pkg_name}`, then
/// insert the version row with all manifest + storage fields.
///
/// Dependencies and capabilities are serialized to JSON strings — the
/// schema declares them as `string` with defaults of `"[]"` / `"{}"`.
///
/// Every nullable `VERSIONS` column is written (as `Null` when the manifest
/// didn't supply a value) to avoid the in-memory harness's schema-drift
/// trap: auto-created tables pick their columns from the first insert, and
/// subsequent `update` calls (e.g. yank) need those columns to exist.
pub async fn insert_version(
    ctx: &dyn Context,
    org_id: &str,
    pkg_name: &str,
    user_id: &str,
    t: &crate::blocks::registry::tarball::ExtractedTarball,
    storage_key: &str,
) -> Result<()> {
    let now = now_unix();

    // Package row: get-or-create keyed by (org_id, name).
    let pkg_id = if let Some(existing) = find_package(ctx, org_id, pkg_name).await? {
        existing.id
    } else {
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("org_id".into(), json!(org_id));
        data.insert("name".into(), json!(pkg_name));
        data.insert(
            "summary".into(),
            match &t.wafer_toml.package.summary {
                Some(s) => json!(s),
                None => serde_json::Value::Null,
            },
        );
        data.insert("created_by".into(), json!(user_id));
        let row = db::create(ctx, PACKAGES, data)
            .await
            .map_err(|e| anyhow::anyhow!("insert_version create package: {e:?}"))?;
        row.id
    };

    // Version row. Stringify dependencies + capabilities via toml::Value
    // -> serde_json::Value so the serialization is deterministic and the
    // schema's "string" column types hold. Omitted sections default to
    // the empty collection so the stored blob always parses.
    let deps_json = t
        .wafer_toml
        .dependencies
        .as_ref()
        .map(toml_to_json)
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let caps_json = t
        .wafer_toml
        .capabilities
        .as_ref()
        .map(toml_to_json)
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("package_id".into(), json!(pkg_id));
    data.insert("version".into(), json!(&t.wafer_toml.package.version));
    data.insert("abi".into(), json!(t.wafer_toml.package.abi as i64));
    data.insert("sha256".into(), json!(&t.sha256));
    data.insert("storage_key".into(), json!(storage_key));
    data.insert("size_bytes".into(), json!(t.size_bytes as i64));
    data.insert(
        "license".into(),
        match &t.wafer_toml.package.license {
            Some(s) => json!(s),
            None => serde_json::Value::Null,
        },
    );
    data.insert(
        "readme_md".into(),
        match &t.readme_md {
            Some(s) => json!(s),
            None => serde_json::Value::Null,
        },
    );
    data.insert("dependencies".into(), json!(deps_json.to_string()));
    data.insert("capabilities".into(), json!(caps_json.to_string()));
    data.insert("yanked".into(), json!(false));
    // Nullable fields declared explicitly — see the fn-level doc comment.
    data.insert("yanked_reason".into(), serde_json::Value::Null);
    data.insert("yanked_at".into(), serde_json::Value::Null);
    data.insert("published_by".into(), json!(user_id));
    data.insert("published_at".into(), json!(now));

    db::create(ctx, VERSIONS, data)
        .await
        .map_err(|e| anyhow::anyhow!("insert_version create version: {e:?}"))?;
    Ok(())
}

/// Convert a `toml::Value` to a `serde_json::Value`. Used to stringify the
/// dependencies / capabilities sub-documents for storage — the schema
/// declares them as `string`, so we serialize once and write the JSON text.
fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => json!(s),
        toml::Value::Integer(i) => json!(i),
        toml::Value::Float(f) => json!(f),
        toml::Value::Boolean(b) => json!(b),
        toml::Value::Datetime(d) => json!(d.to_string()),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut m = serde_json::Map::new();
            for (k, val) in t {
                m.insert(k.clone(), toml_to_json(val));
            }
            serde_json::Value::Object(m)
        }
    }
}

// ---- Yank helpers ---------------------------------------------------------
//
// Flip the `yanked` flag on a version row. Resolves
// org → package → version client-side, then updates the version row via the
// typed `db::update` API.
//
// Nullable fields (`yanked_reason`, `yanked_at`) are always written — even as
// `Value::Null` on unyank — so the in-memory harness's auto-schema path
// doesn't drop columns out from under us (see `insert_version`'s doc comment
// for the same drift-guard pattern).

/// Flip the yanked state of `{org}/{name}@{version}`.
///
/// - Returns `Ok(true)` on success (version row found + updated).
/// - Returns `Ok(false)` when the org, package, or version row is missing —
///   callers translate that to a 404 response.
///
/// Idempotent: writing `yanked = true` over an already-yanked row succeeds
/// without error, because `db::update` doesn't care whether the new value
/// differs from the old. Same for unyank.
pub async fn set_yanked(
    ctx: &dyn Context,
    org: &str,
    name: &str,
    version: &str,
    yanked: bool,
    reason: Option<&str>,
) -> Result<bool> {
    let Some(org_row) = find_org_by_name(ctx, org).await? else {
        return Ok(false);
    };
    let Some(pkg_row) = find_package(ctx, &org_row.id, name).await? else {
        return Ok(false);
    };

    let hits = db::list_all(
        ctx,
        VERSIONS,
        vec![
            eq("package_id", json!(pkg_row.id)),
            eq("version", json!(version)),
        ],
    )
    .await
    .map_err(|e| anyhow::anyhow!("set_yanked lookup({org}/{name}@{version}): {e:?}"))?;
    let Some(ver_row) = hits.into_iter().next() else {
        return Ok(false);
    };

    let now = now_unix();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("yanked".into(), json!(yanked));
    data.insert(
        "yanked_reason".into(),
        match (yanked, reason) {
            (true, Some(r)) => json!(r),
            _ => serde_json::Value::Null,
        },
    );
    data.insert(
        "yanked_at".into(),
        if yanked {
            json!(now)
        } else {
            serde_json::Value::Null
        },
    );
    db::update(ctx, VERSIONS, &ver_row.id, data)
        .await
        .map_err(|e| anyhow::anyhow!("set_yanked update({org}/{name}@{version}): {e:?}"))?;
    Ok(true)
}

fn version_summary_from_record(r: Record) -> VersionSummary {
    VersionSummary {
        version: r
            .data
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        abi: field_i64(&r.data, "abi").unwrap_or(0),
        sha256: r
            .data
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        size_bytes: field_i64(&r.data, "size_bytes").unwrap_or(0),
        license: r
            .data
            .get("license")
            .and_then(|v| v.as_str())
            .map(String::from),
        yanked: if field_bool(&r.data, "yanked") { 1 } else { 0 },
        published_at: field_i64(&r.data, "published_at").unwrap_or(0),
    }
}
