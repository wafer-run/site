//! Typed query-helper tests for the registry block.
//!
//! Boots the registry against an in-memory sqlite context (Task 7's Init
//! seeds the 4 reserved orgs), inserts a couple of packages + versions, and
//! exercises every helper added in Task 8 — `list_packages`, `get_package`,
//! `get_version`, plus their missing-row branches.

mod common;

use std::collections::HashMap;

use serde_json::json;
use wafer_core::clients::database as db;
use wafer_site::blocks::registry;

/// Insert a package row under the `wafer-run` reserved org (seeded during
/// block Init) and return the new row's ID.
async fn create_pkg(
    ctx: &common::InMemoryCtx,
    org_id: &str,
    name: &str,
    summary: &str,
    created_at: i64,
) -> String {
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("org_id".into(), json!(org_id));
    data.insert("name".into(), json!(name));
    data.insert("summary".into(), json!(summary));
    data.insert("created_by".into(), json!("test-user"));
    data.insert("created_at".into(), json!(created_at));
    db::create(ctx, registry::db::PACKAGES, data)
        .await
        .expect("create package")
        .id
}

/// Insert a version row for the given package.
async fn create_version(
    ctx: &common::InMemoryCtx,
    package_id: &str,
    version: &str,
    published_at: i64,
) {
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("package_id".into(), json!(package_id));
    data.insert("version".into(), json!(version));
    data.insert("abi".into(), json!(1));
    data.insert("sha256".into(), json!("0".repeat(64)));
    data.insert(
        "storage_key".into(),
        json!(format!("blobs/{version}.tar.gz")),
    );
    data.insert("size_bytes".into(), json!(1024));
    data.insert("yanked".into(), json!(false));
    data.insert("published_by".into(), json!("test-user"));
    data.insert("published_at".into(), json!(published_at));
    db::create(ctx, registry::db::VERSIONS, data)
        .await
        .expect("create version");
}

#[tokio::test]
async fn typed_query_helpers_happy_path() {
    let ctx = common::boot_registry_against_memory().await;

    // Resolve the `wafer-run` reserved org's ID (seeded during Init).
    let wafer_run_org = registry::db::find_org_by_name(ctx.as_ref(), "wafer-run")
        .await
        .expect("find_org_by_name")
        .expect("wafer-run org seeded on Init");

    // Two packages under wafer-run, each with two versions.
    let sqlite_id = create_pkg(
        ctx.as_ref(),
        &wafer_run_org.id,
        "sqlite",
        "SQLite backend block",
        1_700_000_000,
    )
    .await;
    let postgres_id = create_pkg(
        ctx.as_ref(),
        &wafer_run_org.id,
        "postgres",
        "Postgres backend block",
        1_700_000_100,
    )
    .await;

    create_version(ctx.as_ref(), &sqlite_id, "0.1.0", 1_700_000_010).await;
    create_version(ctx.as_ref(), &sqlite_id, "0.2.0", 1_700_000_020).await;
    create_version(ctx.as_ref(), &postgres_id, "0.1.0", 1_700_000_110).await;
    create_version(ctx.as_ref(), &postgres_id, "0.2.0", 1_700_000_120).await;

    // list_packages: unfiltered returns both, each with latest = 0.2.0.
    let (summaries, total) = registry::db::list_packages(ctx.as_ref(), None, 1, 20)
        .await
        .expect("list_packages none");
    assert_eq!(total, 2, "two packages inserted, two counted");
    assert_eq!(summaries.len(), 2);
    for s in &summaries {
        assert_eq!(s.org, "wafer-run");
        assert_eq!(
            s.latest.as_deref(),
            Some("0.2.0"),
            "package {} latest should be 0.2.0, got {:?}",
            s.name,
            s.latest
        );
    }

    // list_packages with a name filter narrows to exactly one.
    let (filtered, filtered_total) =
        registry::db::list_packages(ctx.as_ref(), Some("sqlite"), 1, 20)
            .await
            .expect("list_packages sqlite");
    assert_eq!(filtered_total, 1, "only one package matches LIKE %sqlite%");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "sqlite");
    assert_eq!(filtered[0].latest.as_deref(), Some("0.2.0"));

    // get_package returns all versions, sorted desc by published_at.
    let detail = registry::db::get_package(ctx.as_ref(), "wafer-run", "sqlite")
        .await
        .expect("get_package")
        .expect("sqlite package exists");
    assert_eq!(detail.org, "wafer-run");
    assert_eq!(detail.name, "sqlite");
    assert_eq!(detail.versions.len(), 2);
    assert_eq!(
        detail.versions[0].version, "0.2.0",
        "versions must be sorted desc by published_at"
    );
    assert_eq!(detail.versions[1].version, "0.1.0");

    // get_version resolves a specific row.
    let v010 = registry::db::get_version(ctx.as_ref(), "wafer-run", "sqlite", "0.1.0")
        .await
        .expect("get_version 0.1.0");
    let v010 = v010.expect("0.1.0 exists");
    assert_eq!(v010.org_name, "wafer-run");
    assert_eq!(v010.pkg_name, "sqlite");
    assert_eq!(v010.version, "0.1.0");
    assert_eq!(v010.published_at, 1_700_000_010);
    assert_eq!(v010.storage_key, "blobs/0.1.0.tar.gz");

    // Missing-package path: org exists (reserved), package doesn't.
    let missing_pkg = registry::db::get_version(ctx.as_ref(), "wafer-run", "nonexistent", "0.1.0")
        .await
        .expect("get_version nonexistent package");
    assert!(
        missing_pkg.is_none(),
        "missing package must resolve to None, got {missing_pkg:?}"
    );

    // Missing-org path: no such org.
    let missing_org = registry::db::get_package(ctx.as_ref(), "ghost-org", "sqlite")
        .await
        .expect("get_package missing org");
    assert!(missing_org.is_none());

    // Missing-version path: package exists, version doesn't.
    let missing_ver = registry::db::get_version(ctx.as_ref(), "wafer-run", "sqlite", "9.9.9")
        .await
        .expect("get_version missing version");
    assert!(missing_ver.is_none());
}
