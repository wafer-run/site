//! `wafer-site/health` — deploy-time config validation endpoint.
//!
//! Responds to `GET /_health` with a JSON summary of every registered
//! block's `ConfigVar` declarations. A block is considered "broken" if
//! any non-optional declared key has neither a value in the config
//! snapshot nor a hard-coded `default`.
//!
//! ## Why duplicate `Wafer::validate_all_block_configs`?
//!
//! The canonical validator runs at startup and walks
//! `ConfigSource::load_for_block` for each registered block. From inside
//! a block's `handle()` we have only the [`Context`] trait — which
//! exposes `registered_blocks()` and `config_get(key)` but not the
//! pre-built per-block snapshots the canonical validator uses.
//!
//! Replicating the required-keys check against `Context` is ~30 LoC.
//! The alternative (a `RuntimeHandle::validate_all_block_configs`
//! passthrough) would be cleaner but requires a wafer-run change; we
//! chose the contained duplication to keep this PR site-local. If
//! `ConfigSource::load_for_block` ever grows semantics beyond
//! "required key has a value or a default", this block must follow.
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
        let mut ok: Vec<String> = Vec::new();
        let mut broken: Vec<(String, Vec<String>)> = Vec::new();

        for info in ctx.registered_blocks() {
            let mut missing: Vec<String> = Vec::new();
            for cv in &info.config_keys {
                if cv.optional {
                    continue;
                }
                let present = ctx
                    .config_get(&cv.key)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if !present && cv.default.is_empty() {
                    missing.push(cv.key.clone());
                }
            }
            if missing.is_empty() {
                ok.push(info.name.clone());
            } else {
                missing.sort();
                broken.push((info.name.clone(), missing));
            }
        }
        ok.sort();
        broken.sort_by(|a, b| a.0.cmp(&b.0));

        let body = serde_json::json!({
            "ok": ok,
            "broken": broken
                .iter()
                .map(|(block, missing_keys)| serde_json::json!({
                    "block": block,
                    "missing_keys": missing_keys,
                }))
                .collect::<Vec<_>>(),
        });
        let status = if broken.is_empty() { "200" } else { "503" };
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
