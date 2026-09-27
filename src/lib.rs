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

use impresspress_core::builder::ImpresspressBuilder;
use impresspress_core::features::BlockSettings;
use impresspress_core::RouteAccess;
#[cfg(feature = "target-native")]
use wafer_block_local_storage::service::LocalStorageService;
use wafer_core::interfaces::storage::service::StorageService;

// ---------------------------------------------------------------------------
// Shared registration helpers — used by both the native `run()` below and
// the cloudflare `fetch_main` worker entry. They're kept free-functions
// rather than methods on a struct so they can be passed by name to
// `impresspress_cloudflare::run`'s `FnOnce` parameters.
// ---------------------------------------------------------------------------

/// Pre-build hook: applies site-specific [`ImpresspressBuilder`] configuration.
///
/// Wires [`block_settings_for_site`] and registers `/registry` as a route of
/// `impresspress/router`, so every registry request runs through
/// impresspress's request pipeline before the registry block sees it — the
/// CSRF origin policy (`impresspress_core::csrf::enforce_origin_policy`)
/// that refuses a cookie-authenticated cross-site mutation, and the
/// `request_logs` audit row. The route is [`RouteAccess::Public`]: the
/// registry authenticates its own callers (`blocks::registry::auth`), since
/// its CLI tokens are its own and its admin gate reads a claim the pipeline
/// does not stamp.
pub fn register_blocks_for_site(
    builder: ImpresspressBuilder,
) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>> {
    Ok(builder.block_settings(block_settings_for_site()).add_route(
        blocks::registry::ROUTE_PREFIX,
        blocks::registry::NAME,
        RouteAccess::Public,
    ))
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
                    base-uri 'self'; \
                    form-action 'self'"
        }),
    );

    // 4b. Registry block. See doc comment above re: soft-default behaviour.
    let registry_cfg = crate::blocks::registry::RegistryConfig {
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
/// The boot is impresspress's own native server
/// ([`impresspress_server::run`]): `.env` and tracing, the `IMPRESSPRESS_*`
/// infrastructure config, the platform services, the variables and
/// block-settings tables, `build()`, the admin-first boot and the HTTP
/// listener. The site adds itself through the same two hooks the Cloudflare
/// entry passes to `impresspress_cloudflare::run` — [`register_blocks_for_site`]
/// and [`register_post_build_for_site`] — and points the listener at the
/// `wafer-site-main` flow.
///
/// The content block reads the site's assets from a LocalStorage rooted at
/// `<repo>/dist`, separate from impresspress's platform storage (rooted at
/// `IMPRESSPRESS_STORAGE_ROOT`) so the two key namespaces don't collide.
#[cfg(feature = "target-native")]
pub async fn run() -> anyhow::Result<()> {
    impresspress_server::run(
        std::path::Path::new("."),
        false,
        flows::site::FLOW_ID,
        native_app_hooks()?,
    )
    .await
}

/// The hooks [`run`] hands impresspress's native boot: the site's blocks,
/// configs and flow, with the content block reading from `<repo>/dist`.
///
/// Public so the integration tests boot the runtime the binary serves
/// (`impresspress_server::start_native` with these hooks and
/// [`flows::site::FLOW_ID`]) rather than a hand-assembled copy of it.
#[cfg(feature = "target-native")]
pub fn native_app_hooks() -> anyhow::Result<impresspress_server::AppHooks> {
    let dist_root = format!("{}/dist", env!("CARGO_MANIFEST_DIR"));
    let content_storage: Arc<dyn StorageService> = Arc::new(
        LocalStorageService::new(&dist_root)
            .map_err(|e| anyhow::anyhow!("LocalStorageService::new({dist_root}): {e:?}"))?,
    );
    Ok(impresspress_server::AppHooks {
        register_blocks: Box::new(register_blocks_for_site),
        register_post_build: Box::new(move |wafer, _platform_storage| {
            register_post_build_for_site(wafer, content_storage)
        }),
    })
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
