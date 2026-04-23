//! Shared test harness for the registry block's integration tests.
//!
//! Mirrors solobase's `MigrationTestCtx` pattern: a minimal in-memory context
//! that routes `call_block("wafer-run/database", ...)` to a real `DatabaseBlock`
//! wrapping an in-memory SQLite service. Any other block call returns
//! `NotFound`, which is sufficient for Task 7 — the registry block's `Init`
//! lifecycle only talks to the database.

use std::sync::Arc;

use wafer_run::{
    block::Block,
    context::Context,
    types::{LifecycleEvent, LifecycleType, Message, WaferError},
    InputStream, OutputStream,
};

use wafer_site::blocks::registry::{self, handlers::RegistryBlock, RegistryConfig};

/// In-memory context: only the database service block is available. All other
/// `call_block` targets return `NotFound`. Good enough for tests that exercise
/// CRUD on the registry's collections.
pub struct InMemoryCtx {
    db_block: Arc<dyn Block>,
}

impl InMemoryCtx {
    pub fn new() -> Self {
        let svc = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        let db_block: Arc<dyn Block> =
            Arc::new(wafer_core::service_blocks::database::DatabaseBlock::new(svc));
        Self { db_block }
    }
}

#[async_trait::async_trait]
impl Context for InMemoryCtx {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        match block_name {
            "wafer-run/database" => self.db_block.handle(self, msg, input).await,
            _ => OutputStream::error(WaferError::new(
                wafer_run::types::ErrorCode::NotFound,
                format!("block '{block_name}' not registered in test ctx"),
            )),
        }
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
}

/// Construct the registry block with a minimal config and dispatch
/// `LifecycleEvent::Init` against an in-memory context. Returns the context
/// so the caller can query the seeded collections via `db::*`.
pub async fn boot_registry_against_memory() -> Arc<InMemoryCtx> {
    let ctx = Arc::new(InMemoryCtx::new());

    let cfg = RegistryConfig {
        admin_email: "test@example.invalid".into(),
        storage_key_prefix: "registry".into(),
    };
    let block: Arc<dyn Block> = Arc::new(RegistryBlock::new(cfg));

    block
        .lifecycle(
            ctx.as_ref(),
            LifecycleEvent {
                event_type: LifecycleType::Init,
                data: Vec::new(),
            },
        )
        .await
        .expect("registry Init lifecycle seeds reserved orgs");

    // Silence unused-import lint when only the helper is imported: mention
    // `registry` so the module is reachable from the test.
    let _: &str = registry::NAME;

    ctx
}
