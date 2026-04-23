//! Typed helpers for the registry block's collections.
//!
//! No raw SQL — all access goes through `wafer_core::clients::database`.
//! Task 7 ships constants + the reserved-orgs seed. Tasks 8, 12, 13, 14
//! extend this module with query helpers as routes come online.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::json;
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
fn field_i64(data: &std::collections::HashMap<String, serde_json::Value>, key: &str) -> Option<i64> {
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
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "1" | "true" | "TRUE" | "True" => true,
            _ => false,
        },
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
pub async fn find_package(
    ctx: &dyn Context,
    org_id: &str,
    name: &str,
) -> Result<Option<Record>> {
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
pub async fn latest_version_for(
    ctx: &dyn Context,
    package_id: &str,
) -> Result<Option<Record>> {
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
    Ok(res.records.into_iter().find(|r| !field_bool(&r.data, "yanked")))
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
        versions: versions.into_iter().map(version_summary_from_record).collect(),
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
