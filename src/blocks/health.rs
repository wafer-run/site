//! `wafer-site/health` — deploy-time config validation endpoint.
//!
//! Responds to `GET /_health` with a JSON summary of every registered
//! block's `ConfigVar` declarations. A block is considered "broken" if
//! any non-optional declared key fails to resolve through the active
//! `ConfigSource` and has no hard-coded default.
//!
//! Delegates to `Context::validate_all_block_configs` (wafer-run #106),
//! which walks the same `ConfigSource::load_for_block` path every block
//! uses for lazy init. Post-Phase-1 lazy init means D1-stored vars no
//! longer live in the shared `ctx.config_get` snapshot on Cloudflare;
//! validating against `ctx.config_get` would false-alarm on every
//! per-block D1 var.
//!
//! ## Response
//!
//! - 200 OK with `{ "ok": [..], "broken": [] }` when every block is
//!   satisfied.
//! - 503 Service Unavailable with `{ "ok": [..], "broken": [{ "block",
//!   "missing_keys" }] }` when any required key is unset.
//!
//! Deploy scripts gate post-deploy rollback on a 200 response.

use std::sync::Arc;

use wafer_run::{
    Block, BlockCategory, BlockInfo, Context, InputStream, InstanceMode, LifecycleEvent, Message,
    MetaEntry, OutputStream, RuntimeError, Wafer, WaferError, META_RESP_CONTENT_TYPE,
    META_RESP_STATUS,
};

/// Block name. Site-local; mirrors `wafer-site/content` namespacing.
pub const NAME: &str = "wafer-site/health";

/// Health block — no state, validates against `ctx` on each request.
pub struct HealthBlock;

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for HealthBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            NAME,
            "0.0.1",
            "http-handler@v1",
            "Deploy-time config validation endpoint (/_health)",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let report = ctx.validate_all_block_configs().await;

        let body = serde_json::json!({
            "ok": report.ok,
            "broken": report.broken
                .iter()
                .map(|b| serde_json::json!({
                    "block": b.block,
                    "missing_keys": b.missing_keys,
                }))
                .collect::<Vec<_>>(),
        });
        let status = if report.broken.is_empty() { "200" } else { "503" };
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        OutputStream::respond_with_meta(
            bytes,
            vec![
                MetaEntry {
                    key: META_RESP_STATUS.to_string(),
                    value: status.to_string(),
                },
                MetaEntry {
                    key: META_RESP_CONTENT_TYPE.to_string(),
                    value: "application/json".to_string(),
                },
            ],
        )
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Register the health block with the runtime.
pub fn register(w: &mut Wafer) -> anyhow::Result<()> {
    w.register_block(NAME, Arc::new(HealthBlock))
        .map_err(|e: RuntimeError| anyhow::anyhow!("register {NAME}: {e}"))
}
