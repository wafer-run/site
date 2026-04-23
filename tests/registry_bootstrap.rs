//! Verifies CollectionSchema-driven table creation + reserved-orgs seed.
//!
//! Boots the registry block against an in-memory sqlite context, dispatches
//! `LifecycleEvent::Init`, and asserts the 4 reserved orgs are present. A
//! second Init invocation confirms the seed is idempotent.

mod common;

use wafer_core::clients::database as db;
use wafer_site::blocks::registry;

#[tokio::test]
async fn init_creates_tables_and_seeds_reserved_orgs() {
    let ctx = common::boot_registry_against_memory().await;

    // Seed ran during Init. Verify by listing orgs.
    let orgs = db::list_all(ctx.as_ref(), registry::db::ORGS, vec![])
        .await
        .expect("list orgs");
    let mut names: Vec<String> = orgs
        .iter()
        .filter_map(|r| {
            r.data
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "solobase".to_string(),
            "suppers-ai".to_string(),
            "wafer".to_string(),
            "wafer-run".to_string(),
        ],
        "reserved orgs must match the declared constant in registry::db::RESERVED_ORGS"
    );

    // Calling the seed again is a no-op: no duplicate rows.
    registry::db::seed_reserved_orgs(ctx.as_ref())
        .await
        .expect("second seed should succeed");
    let count_after = db::count(ctx.as_ref(), registry::db::ORGS, &[])
        .await
        .expect("count orgs");
    assert_eq!(
        count_after, 4,
        "seed must be idempotent — re-running must not add rows"
    );
}
