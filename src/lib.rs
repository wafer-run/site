//! wafer-site library crate.
//!
//! The entrypoint (`src/main.rs`) is a thin shell that calls [`run`]. All
//! the composition — WAFER runtime setup, block registration, HTTP listener
//! wiring — lives here so it can be exercised from tests and future
//! integration harnesses.
//!
//! ## Targets
//!
//! - `target-native` (default): builds the binary, listens on TCP, uses
//!   on-disk SQLite + LocalStorage. This is the canonical dev/test path.
//! - `target-cloudflare`: cdylib for `wasm32-unknown-unknown` consumed by
//!   `worker-build`. The cloudflare entry (`fetch_main`) routes requests
//!   through `impresspress_cloudflare::run` with this crate's
//!   [`register_blocks_for_site`] / [`register_post_build_for_site`] hooks.
//!   The post-build hook receives a [`StorageService`] (LocalStorage on
//!   native, R2 on cloudflare) which [`blocks::content`] uses to serve
//!   the site SPA chrome, so both targets serve `/` and the docs/registry
//!   routes uniformly.

pub mod blocks;
pub mod flows;

use std::{collections::HashMap, sync::Arc};

#[cfg(feature = "target-native")]
use impresspress_core::builder;
use impresspress_core::builder::ImpresspressBuilder;
use impresspress_core::features::BlockSettings;
#[cfg(feature = "target-native")]
use wafer_block_local_storage::service::LocalStorageService;
use wafer_core::interfaces::storage::service::StorageService;

#[cfg(feature = "target-native")]
use impresspress_native::{
    init_tracing, load_dotenv, register_http_listener, register_observability_hooks,
    serve_until_shutdown, InfraConfig,
};

// ---------------------------------------------------------------------------
// Shared registration helpers — used by both the native `run()` below and
// the cloudflare `fetch_main` worker entry. They're kept free-functions
// rather than methods on a struct so they can be passed by name to
// `impresspress_cloudflare::run`'s `FnOnce` parameters.
// ---------------------------------------------------------------------------

/// Pre-build hook: applies site-specific [`ImpresspressBuilder`] configuration.
///
/// Currently just wires [`block_settings_for_site`]. Kept as its own
/// function for symmetry with [`register_post_build_for_site`] and so the
/// cloudflare worker entry can pass it as a closure argument.
pub fn register_blocks_for_site(
    builder: ImpresspressBuilder,
) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>> {
    Ok(builder.block_settings(block_settings_for_site()))
}

/// Post-build hook: registers site-owned blocks, overrides default block
/// configs, and registers the `wafer-site-main` flow.
///
/// `content_storage` is the [`StorageService`] the site content block
/// reads its assets from. Native passes a [`LocalStorageService`] rooted
/// at `<repo>/dist` (folder=""); cloudflare passes the R2-backed service
/// from `impresspress-cloudflare` (folder="dist", since `impresspress deploy
/// --target cloudflare` uploads `dist/**` under that prefix in R2). Both
/// targets serve the SPA chrome from `/` uniformly.
///
/// ## Registry env vars
///
/// The registry block reads its config from env vars. On native those come
/// from `.env` via `dotenv`; on cloudflare they come from D1's `variables`
/// table merged into the worker `env` by `impresspress_cloudflare::run`'s
/// protected-key loader. Missing values soft-default to empty strings here
/// rather than panicking, so wasm32 builds of the worker don't trip
/// `expect()` at startup. The registry block surfaces a clear error at
/// request time if its config is empty/wrong, which matches the failure
/// mode for any other missing impresspress config.
pub fn register_post_build_for_site(
    wafer: &mut wafer_run::Wafer,
    content_storage: Arc<dyn StorageService>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 4a. Site content block. Native's LocalStorage is rooted at
    //     <repo>/dist (folder=""); cloudflare's R2 service holds the
    //     whole bucket (folder="dist" since deploy uploads dist/**
    //     under that prefix).
    #[cfg(feature = "target-native")]
    let content_folder = "";
    #[cfg(feature = "target-cloudflare")]
    let content_folder = "dist";
    crate::blocks::content::register(wafer, content_storage, content_folder)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    // `ImpresspressBuilder` configures `wafer-run/inspector` with
    // `allow_anonymous: false` because impresspress runs behind auth. The site
    // exposes the inspector publicly, so override here.
    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({ "allow_anonymous": true }),
    );

    // CSP override: chrome (`<sa-header>` / `<sa-footer>`) loads from
    // `https://site-kit.suppers.ai/dist/...`; Cloudflare Web Analytics
    // injects `https://static.cloudflareinsights.com/beacon.min.js`
    // after the response leaves the worker. Default CSP only allows
    // `'self'`; extend script-src and style-src accordingly.
    wafer.add_block_config(
        "wafer-run/security-headers",
        serde_json::json!({
            "csp": "default-src 'self'; \
                    script-src 'self' 'unsafe-inline' https://site-kit.suppers.ai https://static.cloudflareinsights.com; \
                    style-src 'self' 'unsafe-inline' https://site-kit.suppers.ai; \
                    img-src 'self' data: blob: https:; \
                    font-src 'self' https:; \
                    connect-src 'self'; \
                    frame-ancestors 'none'; \
                    base-uri 'self'; \
                    form-action 'self'"
        }),
    );

    // 4b. Registry block. See doc comment above re: soft-default behaviour.
    let jwt_secret =
        std::env::var(impresspress_core::blocks::auth::JWT_SECRET_KEY).unwrap_or_default();
    let registry_cfg = crate::blocks::registry::RegistryConfig {
        jwt_secret,
        admin_email: std::env::var("WAFER_RUN__REGISTRY__ADMIN_EMAIL").unwrap_or_default(),
        storage_key_prefix: std::env::var("WAFER_RUN__REGISTRY__STORAGE_KEY_PREFIX")
            .unwrap_or_else(|_| "registry".into()),
        required_auth_method: std::env::var("WAFER_RUN__REGISTRY__REQUIRED_AUTH_METHOD")
            .unwrap_or_default(),
    };
    crate::blocks::registry::register(wafer, registry_cfg)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    // 4c. Health block. Backs `/_health` with a deploy-time config
    //     validation summary; the deploy script rolls back on non-200.
    crate::blocks::health::register(wafer)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    // 5. Site flow + router routes. Must run *after* `build()` so our
    //    `wafer-run/router` config overwrites impresspress's default.
    crate::flows::register_site_main(wafer)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    Ok(())
}

/// Hide impresspress feature blocks the site doesn't surface.
///
/// `BlockSettings` is consumed by `ImpresspressRouterBlock`: when a block is
/// disabled here, requests to `/b/{block}/**` 404 instead of dispatching.
/// The blocks are still registered statically (every impresspress feature
/// block self-registers via `register_static_block!`) and their required
/// config is still validated at start, so this is purely a routing/UX
/// concern — not a way to suppress missing-config errors.
///
/// `BlockSettings` stores the *full* block name (e.g. `impresspress/llm`)
/// and defaults to enabled. Explicitly set the features we don't want to
/// `false`.
fn block_settings_for_site() -> BlockSettings {
    let mut enabled = HashMap::new();
    for name in [
        "impresspress/legalpages",
        "impresspress/llm",
        "impresspress/projects",
        "impresspress/products",
        "impresspress/files",
        "impresspress/messages",
        "impresspress/userportal",
        "impresspress/vector",
        "impresspress/fastembed",
    ] {
        enabled.insert(name.to_string(), false);
    }
    BlockSettings::from_map(enabled)
}

// ---------------------------------------------------------------------------
// Native target — `impresspress serve --target native` / `cargo run`.
// ---------------------------------------------------------------------------

/// Run the site (native target).
///
/// Composition order:
///
/// 1. Load `.env`, init tracing.
/// 2. Read `IMPRESSPRESS_*` infra config via [`InfraConfig::from_env`].
/// 3. Build the WAFER runtime via [`ImpresspressBuilder`] + the shared
///    [`register_blocks_for_site`] pre-build hook.
/// 4. Call the shared [`register_post_build_for_site`] hook (registers
///    site content, registry, inspector + security-headers overrides,
///    `wafer-site-main` flow).
/// 6. Native-only wiring: HTTP listener + observability + start +
///    `builder::post_start` + serve until shutdown.
#[cfg(feature = "target-native")]
pub async fn run() -> anyhow::Result<()> {
    use anyhow::Context as _;

    // 1. Load `.env` + tracing. Anchor `.env` lookup to the current dir
    //    so `cargo run` from the repo root picks it up; impresspress-cli does
    //    the same with its `repo_root`.
    load_dotenv(std::path::Path::new("."));
    let log_format = std::env::var("IMPRESSPRESS_LOG_FORMAT").unwrap_or_else(|_| "text".into());
    init_tracing(&log_format).context("initialize tracing subscriber")?;
    tracing::info!("wafer-site starting (impresspress + WAFER runtime)");

    // 2. Infrastructure config (IMPRESSPRESS_*).
    let infra = InfraConfig::from_env();
    tracing::info!(
        listen = %infra.listen,
        db_path = %infra.db_path,
        storage_root = %infra.storage_root,
        "infrastructure config loaded"
    );

    // Ensure the database parent and storage root exist before anything
    // tries to open them.
    if let Some(parent) = std::path::Path::new(&infra.db_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::create_dir_all(&infra.storage_root)?;

    // 3. Build the WAFER runtime via the shared pre-build hook.
    let db = impresspress_native::make_sqlite_database_service(&infra.db_path)
        .context("create sqlite database service")?;
    let storage = impresspress_native::make_local_storage_service(&infra.storage_root)
        .context("create local storage service")?;
    let jwt_secret = std::env::var(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .expect("WAFER_RUN__AUTH__JWT_SECRET required");
    let crypto = impresspress_native::make_jwt_crypto_service(jwt_secret)
        .context("create jwt crypto service")?;
    let builder = ImpresspressBuilder::new()
        .database(db)
        .storage(storage)
        .config(Arc::new(
            wafer_core::service_blocks::config::EnvConfigService::new(),
        ))
        .crypto(crypto)
        .network(impresspress_native::make_fetch_network_service())
        .logger(impresspress_native::make_tracing_logger())
        .config_source(Arc::new(
            impresspress_core::config_source::EnvConfigSource::new(),
        ))
        .sqlite_db_path(&infra.db_path);
    let builder = register_blocks_for_site(builder)
        .map_err(|e| anyhow::anyhow!("register_blocks_for_site: {e}"))?;
    let (mut wafer, storage_block) = builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build impresspress runtime: {e}"))?;

    // 4-5. Shared post-build hooks. The content block reads from a
    //     LocalStorage rooted at <repo>/dist; this is separate from
    //     impresspress's main storage (rooted at infra.storage_root) so the
    //     two key namespaces don't collide.
    let content_storage: Arc<dyn StorageService> = {
        let dist_root = format!("{}/dist", env!("CARGO_MANIFEST_DIR"));
        Arc::new(
            LocalStorageService::new(&dist_root)
                .map_err(|e| anyhow::anyhow!("LocalStorageService::new({dist_root}): {e:?}"))?,
        )
    };
    register_post_build_for_site(&mut wafer, content_storage)
        .map_err(|e| anyhow::anyhow!("register_post_build_for_site: {e}"))?;

    // 6. Native-only wiring: HTTP listener + observability + boot.
    register_http_listener(&mut wafer, &infra.listen, "wafer-site-main");
    register_observability_hooks(&mut wafer);

    // Boot through the shared funnel (seal → init_block(admin) →
    // init_all_blocks → post_start), then run the native-only Start
    // lifecycle + socket bind — the same sequence as impresspress's native
    // server. Admin-first init guarantees admin's migrations create
    // `impresspress__admin__block_settings` before any other block's Init
    // writes its migration state there; the previous plain `start()` left
    // init order to HashMap iteration and llm/registry could permanent-fail
    // on the missing table on a fresh database. The site seeds its config
    // from env pre-build (like native impresspress), so the seed hook is a no-op.
    struct SiteBootHooks;

    #[wafer_block::wafer_async_trait]
    impl builder::BootHooks for SiteBootHooks {
        async fn seed_after_admin_init(&self, _wafer: &wafer_run::Wafer) -> Result<(), String> {
            Ok(())
        }
    }

    builder::boot(&mut wafer, &storage_block, &SiteBootHooks)
        .await
        .map_err(|e| anyhow::anyhow!("failed to boot WAFER runtime: {e}"))?;
    wafer.run_start_lifecycle().await;
    let wafer = wafer.bind_all();
    tracing::info!(listen = %infra.listen, "wafer-site listening");

    serve_until_shutdown(&wafer)
        .await
        .context("await shutdown signal")?;
    tracing::info!("wafer-site shutdown complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Cloudflare target — `impresspress build --target cloudflare` consumes this
// crate as a `cdylib`, then `worker-build` packages it into a CF Worker.
// ---------------------------------------------------------------------------

/// Cloudflare Worker `fetch` entrypoint.
///
/// Defers all the heavy lifting to [`impresspress_cloudflare::run`], which
/// loads vars from D1, wires services, and invokes our two registration
/// hooks before dispatching the request through WAFER.
#[cfg(feature = "target-cloudflare")]
#[worker::event(fetch)]
async fn fetch_main(
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
) -> worker::Result<worker::Response> {
    impresspress_cloudflare::run(
        req,
        env,
        ctx,
        register_blocks_for_site,
        register_post_build_for_site,
    )
    .await
}

/// Cloudflare Worker `start` entrypoint: one-time isolate initialization
/// (request-log queueing mode) before the first fetch event. `run()` keeps
/// a once-guard fallback, so this is the proper home rather than a hard
/// requirement.
#[cfg(feature = "target-cloudflare")]
#[worker::event(start)]
fn start() {
    impresspress_cloudflare::init_isolate();
}
