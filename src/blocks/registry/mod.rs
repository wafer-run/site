//! `wafer-run/registry` block — package registry for WAFER blocks.
//!
//! Task 6 scaffolds the block structure with route dispatch and stub handlers.
//! Real handler implementations land in Tasks 7–14.

pub mod db;
pub mod handlers;
pub mod models;
pub mod routes;
pub mod templates;

use std::sync::Arc;
use wafer_run::Wafer;

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

/// Register the `wafer-run/registry` block with route dispatch.
pub fn register(w: &mut Wafer, cfg: RegistryConfig) -> anyhow::Result<()> {
    tracing::debug!(
        admin_email = %cfg.admin_email,
        storage_key_prefix = %cfg.storage_key_prefix,
        "registering wafer-run/registry with route dispatch"
    );
    let block = Arc::new(handlers::RegistryBlock::new(cfg));
    w.register_block(NAME, block)
        .map_err(|e| anyhow::anyhow!("register {NAME}: {e}"))
}
