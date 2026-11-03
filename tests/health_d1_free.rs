//! `/_health` sends D1 no statement of its own on a built runtime, before or
//! during an outage.
//!
//! The endpoint is public and polled, so a query per hit would make it a way
//! to spend the account's D1 reads, and during an outage a way to add failed
//! calls to it. It reads the runtime's slots (no query) and walks every
//! block's config through the config source, which on Cloudflare reads the
//! runtime's variables snapshot: filled once by the build, and, after a
//! failed read, remembering the failure until the runtime is rebuilt
//! (`RetryMode::Rebuild`, Cloudflare's).
//!
//! The runtime here is built the Cloudflare way, with the native stand-ins
//! impresspress's own D1 cost harness uses: `request_config_source` (the
//! source every Cloudflare build and warm request uses) over `D1Shaped` (the
//! SQLite service shaped like the D1 adapter, recording every round trip and
//! able to fail a table as D1 does). The native server cannot show this: it
//! resolves block config from a static map and never queries for it.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    builder::request_config_source,
    config_source::{RetryMode, VariablesSnapshot},
    migration_helper,
    platform_state::variables,
    test_support::d1_shaped::{D1Shaped, Fault, Ledger},
};
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{Context, Wafer, META_RESP_STATUS};
use wafer_site::blocks::health;

/// A block declaring a config key, so the config walk consults the source.
const DECLARING: &str = "test/declaring";

/// How many `/_health` requests each phase sends.
const HITS: usize = 20;

struct DeclaringBlock;

#[async_trait::async_trait]
impl Block for DeclaringBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(DECLARING, "0.1.0", "test/iface@v1", "test").config_keys(vec![
            ConfigVar::new("TEST__DECLARING__KEY", "defaulted", "default"),
        ])
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        panic!("/_health must not call the blocks it validates");
    }
}

/// A D1-shaped database whose variables table exists (admin's migrations).
async fn migrated_database(dir: &std::path::Path) -> (Arc<D1Shaped>, Arc<Ledger>) {
    let ledger = Arc::new(Ledger::default());
    let shaped = Arc::new(D1Shaped::open(&dir.join("d1.sqlite3"), Arc::clone(&ledger)));
    let database: Arc<dyn DatabaseService> = shaped.clone();
    let migrations = &impresspress_core::blocks::admin::migrations::MIGRATIONS;
    let backend = wafer_sql_utils::Backend::Sqlite;
    let (files, history) = migrations.for_backend(backend);
    migration_helper::apply_pending_via_service(
        &database,
        backend,
        migrations.block,
        files,
        history,
    )
    .await
    .expect("apply admin migrations");
    (shaped, ledger)
}

/// A runtime built over `database` with a fresh snapshot, as a Cloudflare
/// build (or rebuild) makes one: sealed, every block's `Init` run.
async fn build_runtime(database: Arc<dyn DatabaseService>) -> Arc<Wafer> {
    let snapshot = Arc::new(VariablesSnapshot::new(RetryMode::Rebuild));
    let mut wafer = Wafer::builder()
        .config_source(request_config_source(database, HashMap::new(), snapshot))
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("build the runtime");
    wafer
        .register_block(DECLARING, Arc::new(DeclaringBlock))
        .expect("register the declaring block");
    health::register(&mut wafer).expect("register the health block");
    wafer.seal().await.expect("seal");
    wafer.init_all_blocks().await;
    Arc::new(wafer)
}

/// Send `/_health` [`HITS`] times; every answer's status.
async fn hit(wafer: &Wafer) -> Vec<String> {
    let mut statuses = Vec::with_capacity(HITS);
    for _ in 0..HITS {
        let response = wafer
            .run_block(health::NAME, Message::new("retrieve"), InputStream::empty())
            .await
            .collect_buffered()
            .await
            .expect("the health block responds");
        statuses.push(
            response
                .meta
                .iter()
                .find(|m| m.key == META_RESP_STATUS)
                .map(|m| m.value.clone())
                .expect("a status"),
        );
    }
    statuses
}

#[tokio::test]
async fn health_sends_no_statement_before_or_during_an_outage() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (shaped, ledger) = migrated_database(dir.path()).await;
    let database: Arc<dyn DatabaseService> = shaped.clone();

    // A runtime built while D1 answers. Its build reads the variables table.
    let wafer = build_runtime(Arc::clone(&database)).await;
    assert!(
        !ledger.take().is_empty(),
        "the build reads the variables table, so the snapshot holds it"
    );

    assert_eq!(hit(&wafer).await, vec!["200"; HITS]);
    let sent = ledger.take();
    assert!(
        sent.is_empty(),
        "/_health on a built runtime sent {} statement(s): {:?}",
        sent.len(),
        sent.iter().map(|trip| trip.sql()).collect::<Vec<_>>()
    );

    // D1 fails from now on. The built runtime answers from its snapshot.
    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    assert_eq!(hit(&wafer).await, vec!["200"; HITS]);
    assert!(ledger.take().is_empty(), "no statement while D1 is failing");

    // The rebuild an outage leads to: its build's read fails, and the new
    // runtime remembers that until the next rebuild.
    let degraded = build_runtime(database).await;
    assert!(!ledger.take().is_empty(), "the rebuild tried the table");
    assert_eq!(hit(&degraded).await, vec!["503"; HITS]);
    let sent = ledger.take();
    assert!(
        sent.is_empty(),
        "/_health on a degraded runtime sent {} statement(s): {:?}",
        sent.len(),
        sent.iter().map(|trip| trip.sql()).collect::<Vec<_>>()
    );
}
