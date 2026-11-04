//! `wafer-site/health` — the site's health endpoint, `GET /_health`.
//!
//! It answers two questions in one body:
//!
//! 1. **Does every block's config resolve?** `Context::validate_all_block_configs`
//!    (wafer-run #106) walks every registered block's `ConfigVar` declarations
//!    through the active `ConfigSource`, the same `load_for_block` path each
//!    block's `Init` uses. On Cloudflare that source reads the runtime's
//!    variables snapshot, so it sees the D1-stored keys that the shared
//!    `ctx.config_get` map does not hold.
//! 2. **Does the runtime serving this request serve all of itself?**
//!    impresspress's runtime report (`impresspress_core::runtime_health`), the
//!    one `GET /health` answers: each block's `Init` state and, on Cloudflare,
//!    the isolate's retry window.
//!
//! ## Response
//!
//! ```text
//! { "ok": [..], "broken": [..], "degraded": [..], "failed": [..],
//!   "runtime": { "status": .., "since"?, "retry_in"?, "retries"?, "unavailable"? } }
//! ```
//!
//! The four lists answer question 1, one entry per block, each sorted by
//! block name:
//!
//! - `ok`: blocks whose config resolved.
//! - `broken`: `{ "block", "missing_keys" }` for a block with a required key
//!   that is unset and has no default.
//! - `degraded`: blocks whose config the source could not answer for a reason
//!   that clears on its own (`UnavailableReason::Transient`: D1 unreachable,
//!   overloaded, over a quota that resets, or the invocation's D1 call budget
//!   spent).
//! - `failed`: blocks whose config the source answered with an error retrying
//!   cannot fix (`UnavailableReason::SourceFault`: a malformed query, a
//!   variables table too large for one read, a D1 error nobody has classified
//!   as one that clears).
//!
//! `runtime` answers question 2. It is [`HealthReport`] exactly as `/health`
//! serializes it, so one parser reads both endpoints:
//!
//! - `ok`: every block serves;
//! - `degraded`: a block does not serve (its `Init` is retrying), or the
//!   runtime's variables read failed after it was published;
//! - `stale`: the runtime serves config older than the database's, which
//!   happens only during an outage (a rebuild failed and the old runtime
//!   serves on, or a new isolate serves the deploy's packaged plan);
//! - `failed`: a block's `Init` failed for good, or the last rebuild did.
//!
//! `unavailable` lists the blocks that do not serve, by `Init` state
//! (`retrying`, `failed`, `initializing`, `not_initialized`).
//!
//! 200 only when the four lists' `broken`, `degraded` and `failed` are empty
//! and `runtime.status` is `ok`; anything else is 503. Never cached
//! (`Cache-Control: no-store`). A 503 carries the runtime's `Retry-After`
//! when it has one; on Cloudflare the Worker gives every other 503 the
//! isolate's retry window (or one second), so every 503 carries one there.
//!
//! The body names blocks and classes only. The source's error text and a
//! block's `Init` failure (either can quote D1) go to the log, never to this
//! public route.
//!
//! ### Why `degraded` and `failed` appear twice (S0 review N6)
//!
//! The top-level lists and `runtime.status` share two words but not their
//! subject, so they are nested rather than merged:
//!
//! - top-level `degraded` / `failed` are **per block**, about the **config
//!   source**: whether the variables table answered for that block, now;
//! - `runtime.status` `degraded` / `failed` are **about the runtime**: whether
//!   its blocks initialized and whether its last rebuild worked.
//!
//! They usually move together in a D1 outage, but not always. A block whose
//! own `Init` keeps resetting D1 is `runtime: degraded` with every config
//! read answering (nothing in the top-level `degraded`); a source fault on one
//! block's config is a top-level `failed` while the runtime that was built
//! before it is `ok`. The deploy gate (`scripts/lib/deploy-checks.sh`,
//! `health_gate_verdict`) tells those cases apart, which a merged list could
//! not.
//!
//! ## What it costs
//!
//! No database query of its own on a built runtime:
//!
//! - the runtime report reads the slots and the isolate's retry window only;
//! - the config walk reads the runtime's variables snapshot, which the build
//!   already filled (or, during an outage, remembers the failed read of until
//!   the runtime is rebuilt). `tests/health_d1_free.rs` pins both.
//!
//! During an outage each hit writes one wafer-run summary line
//! ("config validation: config source unreachable") naming the failing blocks
//! once, not one line per block (wafer-run #476).
//!
//! ## Who reads it
//!
//! - `impresspress deploy` requests it on the candidate version before
//!   promoting it (`deploy_smoke_paths` in `impresspress.toml`). A non-200
//!   stops the deploy with the live version untouched.
//! - `scripts/deploy-cloudflare.sh` requests it after promotion and decides
//!   from the body whether to roll back (a fault of the new version) or keep
//!   the version and report an outage (`health_gate_verdict`).

use std::sync::Arc;

use impresspress_core::runtime_health::{HealthReport, HealthStatus};
use wafer_run::{
    Block, BlockCategory, BlockInfo, Context, InputStream, InstanceMode, LifecycleEvent, Message,
    MetaEntry, OutputStream, RuntimeError, UnavailableReason, ValidationReport, Wafer, WaferError,
    META_RESP_CONTENT_TYPE, META_RESP_HEADER_PREFIX, META_RESP_STATUS,
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
            "Config and runtime health endpoint (/_health)",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let configs = ctx.validate_all_block_configs().await;
        let runtime = HealthReport::for_context(ctx, impresspress_core::util::now_millis());
        let response = health_response(&configs, &runtime);
        let bytes = serde_json::to_vec(&response.body).unwrap_or_default();
        let header = |name: &str, value: String| MetaEntry {
            key: format!("{META_RESP_HEADER_PREFIX}{name}"),
            value,
        };
        let mut meta = vec![
            MetaEntry {
                key: META_RESP_STATUS.to_string(),
                value: response.status.to_string(),
            },
            MetaEntry {
                key: META_RESP_CONTENT_TYPE.to_string(),
                value: "application/json".to_string(),
            },
            header("Cache-Control", "no-store".to_string()),
        ];
        if let Some(seconds) = response.retry_after {
            meta.push(header("Retry-After", seconds.to_string()));
        }
        OutputStream::respond_with_meta(bytes, meta)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// What `/_health` answers.
#[derive(Debug, PartialEq)]
pub struct HealthResponse {
    /// 200 or 503.
    pub status: u16,
    /// `Retry-After` seconds: the runtime's, on a 503 that has one.
    pub retry_after: Option<u32>,
    /// The JSON body (see the module doc).
    pub body: serde_json::Value,
}

/// The `/_health` answer for a config-validation report and a runtime report.
pub fn health_response(configs: &ValidationReport, runtime: &HealthReport) -> HealthResponse {
    let mut degraded = Vec::new();
    let mut failed = Vec::new();
    for entry in &configs.unavailable {
        match entry.reason {
            UnavailableReason::Transient => degraded.push(&entry.block),
            UnavailableReason::SourceFault => failed.push(&entry.block),
        }
    }
    let healthy = configs.broken.is_empty()
        && degraded.is_empty()
        && failed.is_empty()
        && runtime.status == HealthStatus::Ok;
    let body = serde_json::json!({
        "ok": configs.ok,
        "broken": configs.broken
            .iter()
            .map(|b| serde_json::json!({
                "block": b.block,
                "missing_keys": b.missing_keys,
            }))
            .collect::<Vec<_>>(),
        "degraded": degraded,
        "failed": failed,
        "runtime": runtime,
    });
    HealthResponse {
        status: if healthy { 200 } else { 503 },
        retry_after: if healthy { None } else { runtime.retry_in },
        body,
    }
}

/// Register the health block with the runtime.
pub fn register(w: &mut Wafer) -> anyhow::Result<()> {
    w.register_block(NAME, Arc::new(HealthBlock))
        .map_err(|e: RuntimeError| anyhow::anyhow!("register {NAME}: {e}"))
}
