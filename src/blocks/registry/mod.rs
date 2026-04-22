//! `wafer-run/registry` block — package registry for WAFER blocks.
//!
//! Task 4 registers a minimal stub so:
//! - `/_inspector/blocks` lists `wafer-run/registry`.
//! - `/registry` routes resolve (currently 501 until Task 6).
//!
//! Task 6 replaces the body with real `BlockInfo`, dispatch handlers,
//! routes, and data models.

use std::sync::Arc;

use wafer_run::{
    Block, BlockCategory, BlockInfo, Context, ErrorCode, InputStream, InstanceMode,
    LifecycleEvent, Message, OutputStream, Wafer, WaferError,
};

/// Full block name. Owned by `wafer-run` per the `{org}/{block}` naming
/// convention — this is the canonical WAFER package registry block.
pub const NAME: &str = "wafer-run/registry";

/// Configuration for the registry block.
///
/// Sourced from env vars in [`crate::run`] and passed explicitly rather
/// than pulled from `ConfigService` so the call site stays easy to audit.
#[derive(Clone, Debug)]
pub struct RegistryConfig {
    /// Email of the user allowed to publish during Step 2. Enforced once
    /// Task 13 implements the publish endpoint.
    pub admin_email: String,

    /// Top-level storage key prefix for registry tarballs. Defaults to
    /// `"registry"`.
    pub storage_key_prefix: String,
}

/// Stub block — responds with `Unimplemented` until Task 6 lands.
pub struct RegistryBlock {
    _cfg: RegistryConfig,
}

impl RegistryBlock {
    pub fn new(cfg: RegistryConfig) -> Self {
        Self { _cfg: cfg }
    }
}

#[async_trait::async_trait]
impl Block for RegistryBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            NAME,
            "0.0.1",
            "http-handler@v1",
            "WAFER package registry (stub — Task 6 fills this in)",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::error(WaferError {
            code: ErrorCode::Unimplemented,
            message: format!(
                "wafer-run/registry stub — endpoint {} not implemented yet (Task 6+)",
                msg.path()
            ),
            meta: vec![],
        })
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Register the `wafer-run/registry` stub. Task 6 replaces this with the
/// real block wiring.
pub fn register(w: &mut Wafer, cfg: RegistryConfig) -> anyhow::Result<()> {
    tracing::debug!(
        admin_email = %cfg.admin_email,
        storage_key_prefix = %cfg.storage_key_prefix,
        "registering wafer-run/registry stub"
    );
    w.register_block(NAME, Arc::new(RegistryBlock::new(cfg)))
        .map_err(|e| anyhow::anyhow!("register {NAME}: {e}"))
}
