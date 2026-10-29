//! The registry's migrations under impresspress's per-file ledger.
//!
//! 1. Every shipped migration file's content hash is pinned in
//!    `src/blocks/registry/migrations/content-hashes.txt`: editing a shipped
//!    statement fails here, at PR time, instead of being refused by every
//!    deployed database's ledger (`ContentChanged`) at the next deploy.
//! 2. A fresh database's `Init` applies `001_initial_schema` through the
//!    ledger: one `ran` row with the file's content hash, and the list's
//!    hash stamped as the block's `current_hash`.
//! 3. A refusal keeps its classification through the block's `Init`: a
//!    registry database from before the ledger (wafer.run's prod D1 stamped
//!    `fe1cebba…`) is refused as an unknown state with `FailedPrecondition`
//!    and the runner's own message, not flattened into a generic `Internal`.

mod common;

use impresspress_core::{
    features::BLOCK_SETTINGS_CONFIG_KEY,
    migration_helper::{content_hash, migration_set_hash, RUN_MIGRATIONS_KEY},
    platform_state::{
        applied_migrations::{self, Origin},
        block_settings,
    },
};
use wafer_run::ErrorCode;
use wafer_site::blocks::registry::{self, migrations};

/// `content-hashes.txt`: one `<basename>.<dialect> <content hash>` per line.
const PINNED: &str = include_str!("../src/blocks/registry/migrations/content-hashes.txt");

#[test]
fn shipped_migration_content_hashes_are_pinned() {
    let mut shipped: Vec<String> = Vec::new();
    for (dialect, files) in [
        ("sqlite", migrations::SQLITE_MIGRATIONS),
        ("postgres", migrations::POSTGRES_MIGRATIONS),
    ] {
        for (basename, sql) in files {
            shipped.push(format!("{basename}.{dialect} {}", content_hash(sql)));
        }
    }
    let pinned: Vec<String> = PINNED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(
        shipped, pinned,
        "a shipped migration's statements changed, or a file was added without \
         pinning it. A shipped file is frozen (every database that applied it \
         refuses the edit): put the change in a new numbered file, and add its \
         line to content-hashes.txt"
    );
}

#[tokio::test]
async fn init_on_a_fresh_database_applies_001_through_the_ledger() {
    let ctx = common::boot_registry_against_memory().await;

    let rows = applied_migrations::list_for_block(&ctx.fixture(), registry::NAME)
        .await
        .expect("read the ledger");
    let rows: Vec<(String, String, Origin)> = rows
        .into_iter()
        .map(|row| (row.file_name, row.content_sha256, row.origin))
        .collect();
    assert_eq!(
        rows,
        vec![(
            "001_initial_schema".to_string(),
            content_hash(migrations::SQL_001_SQLITE),
            Origin::Ran,
        )]
    );

    let stamped = block_settings::list_all(&ctx.fixture())
        .await
        .expect("list block settings")
        .into_iter()
        .find(|row| row.block_name == registry::NAME)
        .map(|row| row.current_hash);
    assert_eq!(
        stamped,
        Some(migration_set_hash(migrations::SQLITE_MIGRATIONS))
    );
}

#[tokio::test]
async fn init_on_a_pre_ledger_database_is_refused_as_unknown() {
    // What wafer.run's D1 stored for the registry before the ledger: a list
    // hash with no ledger rows, and no history that names it.
    let pre_ledger_hash = "fe1cebba";
    let block_settings_json = serde_json::json!({
        registry::NAME: { "migration": { "current_hash": pre_ledger_hash } }
    })
    .to_string();
    let (ctx, block) = common::build_registry(&[
        (RUN_MIGRATIONS_KEY, "1"),
        (BLOCK_SETTINGS_CONFIG_KEY, &block_settings_json),
    ])
    .await;

    let err = block
        .lifecycle(&ctx, common::init_event())
        .await
        .expect_err("a pre-ledger registry is refused");
    assert_eq!(err.code, ErrorCode::FailedPrecondition, "{err:?}");
    assert!(
        err.message.starts_with(registry::NAME)
            && err.message.contains("is no state main ever had")
            && err.message.contains("Nothing was run"),
        "the runner's own refusal text: {}",
        err.message
    );
    assert!(
        applied_migrations::list_for_block(&ctx.fixture(), registry::NAME)
            .await
            .expect("read the ledger")
            .is_empty(),
        "nothing recorded"
    );
}
