//! `wafer-site/health` — deploy-time config validation endpoint.
//!
//! Responds to `GET /_health` with a JSON summary of every registered
//! block's `ConfigVar` declarations, checked through the active
//! `ConfigSource`.
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
//! `{ "ok": [..], "broken": [..], "degraded": [..], "failed": [..] }`, each
//! list sorted by block name:
//!
//! - `broken`: `{ "block", "missing_keys" }` for a block with a required key
//!   that is unset and has no default.
//! - `degraded`: blocks whose config the source could not answer for a
//!   reason that clears on its own (`UnavailableReason::Transient`: D1
//!   unreachable, overloaded or over a quota that resets).
//! - `failed`: blocks whose config the source answered with an error
//!   retrying cannot fix (`UnavailableReason::SourceFault`). Only a source
//!   that classifies its failures reports this. impresspress's Cloudflare
//!   `D1ConfigSource` reports every D1 failure as `Transient` until it
//!   classifies them (IP-A), so today a missing variables table shows as
//!   `degraded` and `failed` stays empty.
//!
//! 200 OK only when `broken`, `degraded` and `failed` are all empty; any
//! entry in one of them is 503 Service Unavailable. A config source that
//! cannot answer is never healthy: config reads, and the runtime rebuilds
//! that need them, fail until it answers.
//! The body names classes only; the source's own error text (which may
//! quote D1) goes to the log, never to this public route.
//!
//! ## Who reads it
//!
//! - `impresspress deploy` requests it on the candidate version before
//!   promoting it (`deploy_smoke_paths` in `impresspress.toml`). A non-200
//!   stops the deploy with the live version untouched. A D1 outage that is
//!   already underway stops a deploy there, or earlier, at
//!   `/_deploy/prepare`.
//! - `scripts/deploy-cloudflare.sh` requests it once after promotion. It
//!   rolls back when the body shows a fault of the deploy (`broken` or
//!   `failed` entries, or not this body at all). A 503 with only `degraded`
//!   entries is a D1 outage that began after promotion and says nothing
//!   about the new version: the script reports it and exits non-zero
//!   without rolling back.

use std::sync::Arc;

use wafer_run::{
    Block, BlockCategory, BlockInfo, Context, InputStream, InstanceMode, LifecycleEvent, Message,
    MetaEntry, OutputStream, RuntimeError, UnavailableReason, ValidationReport, Wafer, WaferError,
    META_RESP_CONTENT_TYPE, META_RESP_STATUS,
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
        let (status, body) = health_response(&ctx.validate_all_block_configs().await);
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

/// The `/_health` status code and body for a validation report.
fn health_response(report: &ValidationReport) -> (&'static str, serde_json::Value) {
    let mut degraded = Vec::new();
    let mut failed = Vec::new();
    for entry in &report.unavailable {
        match entry.reason {
            UnavailableReason::Transient => degraded.push(&entry.block),
            UnavailableReason::SourceFault => failed.push(&entry.block),
        }
    }
    let healthy = report.broken.is_empty() && degraded.is_empty() && failed.is_empty();
    let body = serde_json::json!({
        "ok": report.ok,
        "broken": report.broken
            .iter()
            .map(|b| serde_json::json!({
                "block": b.block,
                "missing_keys": b.missing_keys,
            }))
            .collect::<Vec<_>>(),
        "degraded": degraded,
        "failed": failed,
    });
    (if healthy { "200" } else { "503" }, body)
}

/// Register the health block with the runtime.
pub fn register(w: &mut Wafer) -> anyhow::Result<()> {
    w.register_block(NAME, Arc::new(HealthBlock))
        .map_err(|e: RuntimeError| anyhow::anyhow!("register {NAME}: {e}"))
}
