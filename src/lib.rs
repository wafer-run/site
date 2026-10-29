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
//!   through `impresspress_cloudflare::run_with_config` with this crate's
//!   [`register_blocks_for_site`] / [`register_post_build_for_site`] hooks.
//!   The post-build hook receives a [`StorageService`] (LocalStorage on
//!   native, R2 on cloudflare) which [`blocks::content`] uses to serve
//!   the site SPA chrome, so both targets serve `/` and the docs/registry
//!   routes uniformly.

// `clippy::arc_with_non_send_sync` is stated once here, crate-wide and
// target-scoped, as impresspress-core and impresspress-cloudflare state it.
//
// wafer-run's service and block traits are bounded on
// `wafer_block::compat::{MaybeSend, MaybeSync}`: real `Send`/`Sync` on native,
// unbounded blanket markers on wasm32, so `dyn StorageService` — which
// `blocks::content::ContentBlock` holds — is `!Send + !Sync` on that target
// by construction. `wafer_run::Wafer::register_block` takes an
// `Arc<dyn Block>` by value, so the `Arc` around such a block is forced, not
// chosen over an `Rc`, and on the single-threaded wasm32 target it makes no
// cross-thread claim. On native the lint is accurate and stays on.
#![cfg_attr(
    target_arch = "wasm32",
    expect(
        clippy::arc_with_non_send_sync,
        reason = "on this single-threaded target the `Arc` is forced by the \
                  trait-object bounds, not chosen over an `Rc`"
    )
)]

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
// `impresspress_cloudflare::run_with_config`'s `FnOnce` parameters.
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
/// The registry block declares its own config variables
/// (`blocks::registry::config`) and reads them per request, so nothing here
/// passes it config.
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

    // 4b. Registry block.
    crate::blocks::registry::register(wafer)
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
/// block-settings tables, `build()`, the admin-first boot (which applies
/// every block's pending migrations, and refuses to start if one fails) and
/// the HTTP listener. The site adds itself through the same two hooks the
/// Cloudflare entry passes to `impresspress_cloudflare::run_with_config` —
/// [`register_blocks_for_site`] and [`register_post_build_for_site`] — and
/// points the listener at the `wafer-site-main` flow.
///
/// The content block reads the site's assets from a LocalStorage rooted at
/// `<repo>/dist`, separate from impresspress's platform storage (rooted at
/// `IMPRESSPRESS_STORAGE_ROOT`) so the two key namespaces don't collide.
#[cfg(feature = "target-native")]
pub async fn run() -> anyhow::Result<()> {
    impresspress_server::run(
        std::path::Path::new("."),
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

/// The first-admin bootstrap token, from the Worker secret of the same name
/// when it is set, for [`impresspress_cloudflare::run_with_config`].
///
/// On Cloudflare a `WAFER_RUN_SHARED__*` key is read from D1's variables
/// table, which an admin edits and which has no rows before the first deploy
/// creates it. Request config is how a consumer hands its blocks a Worker
/// secret instead. With it set, the auth block's `Init` in the first
/// `/_deploy/prepare` on an empty users table stores the token's hash with a
/// 24-hour expiry; its holder then creates the first admin on the
/// `/b/auth/bootstrap` page with an email and password they type
/// themselves. Once any account exists the value is ignored, and the secret
/// can be deleted.
///
/// The token path only: the email + password bootstrap would put the
/// admin's password in a Worker secret.
#[cfg(feature = "target-cloudflare")]
fn bootstrap_admin_config(env: &worker::Env) -> HashMap<String, String> {
    use impresspress_core::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY;
    env.secret(BOOTSTRAP_ADMIN_TOKEN_KEY)
        .ok()
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty())
        .map(|value| (BOOTSTRAP_ADMIN_TOKEN_KEY.to_string(), value))
        .into_iter()
        .collect()
}

/// Cloudflare Worker `fetch` entrypoint.
///
/// Defers all the heavy lifting to
/// [`impresspress_cloudflare::run_with_config`]: D1-backed config, R2
/// storage, the `/_deploy/*` funnel, our two registration hooks, then WAFER
/// dispatch, with the bootstrap token as request config
/// ([`bootstrap_admin_config`]).
#[cfg(feature = "target-cloudflare")]
#[worker::event(fetch)]
async fn fetch_main(
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
) -> worker::Result<worker::Response> {
    let config = bootstrap_admin_config(&env);
    impresspress_cloudflare::run_with_config(
        req,
        env,
        ctx,
        config,
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
