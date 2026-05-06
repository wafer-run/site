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
//!   through `solobase_cloudflare::run` with this crate's
//!   [`register_blocks_for_site`] / [`register_post_build_for_site`] hooks.
//!   v1 ships solobase routes only (admin / auth / registry); the
//!   site SPA chrome is skipped because [`blocks::content`] is backed by
//!   `LocalStorageService`, which doesn't compile on wasm32. Refactoring
//!   the content block to dispatch through the configured `StorageService`
//!   (R2 on cloudflare) is tracked as a follow-up.

pub mod blocks;
pub mod flows;

use std::collections::HashMap;
#[cfg(feature = "target-native")]
use std::sync::Arc;

#[cfg(feature = "target-native")]
use solobase_core::builder;
use solobase_core::builder::SolobaseBuilder;
use solobase_core::features::BlockSettings;

#[cfg(feature = "target-native")]
use solobase_native::{
    init_tracing, load_dotenv, register_http_listener, register_observability_hooks,
    serve_until_shutdown, InfraConfig,
};

// ---------------------------------------------------------------------------
// Shared registration helpers — used by both the native `run()` below and
// the cloudflare `fetch_main` worker entry. They're kept free-functions
// rather than methods on a struct so they can be passed by name to
// `solobase_cloudflare::run`'s `FnOnce` parameters.
// ---------------------------------------------------------------------------

/// Pre-build hook: applies site-specific [`SolobaseBuilder`] configuration.
///
/// Currently just wires [`block_settings_for_site`]. Kept as its own
/// function for symmetry with [`register_post_build_for_site`] and so the
/// cloudflare worker entry can pass it as a closure argument.
pub fn register_blocks_for_site(
    builder: SolobaseBuilder,
) -> Result<SolobaseBuilder, Box<dyn std::error::Error>> {
    Ok(builder.block_settings(block_settings_for_site()))
}

/// Post-build hook: registers site-owned blocks, overrides default block
/// configs, and registers the `wafer-site-main` flow.
///
/// `target-native` additionally registers [`blocks::content`], which
/// serves the SPA's `dist/**` from disk via `LocalStorageService`.
/// `target-cloudflare` skips it because `LocalStorageService` doesn't
/// compile on wasm32 — v1 cloudflare deploy serves solobase routes only.
///
/// ## Registry env vars
///
/// The registry block reads its config from env vars. On native those come
/// from `.env` via `dotenv`; on cloudflare they come from D1's `variables`
/// table merged into the worker `env` by `solobase_cloudflare::run`'s
/// protected-key loader. Missing values soft-default to empty strings here
/// rather than panicking, so wasm32 builds of the worker don't trip
/// `expect()` at startup. The registry block surfaces a clear error at
/// request time if its config is empty/wrong, which matches the failure
/// mode for any other missing solobase config.
pub fn register_post_build_for_site(
    wafer: &mut wafer_run::Wafer,
) -> Result<(), Box<dyn std::error::Error>> {
    // 4a. Native-only: site content block (LocalStorageService isn't
    //     wasm-compatible). Cloudflare deploy v1 serves solobase routes
    //     only — the site SPA chrome will land once the content block is
    //     refactored to use the configured StorageService (R2 on CF).
    #[cfg(feature = "target-native")]
    {
        let dist_root = format!("{}/dist", env!("CARGO_MANIFEST_DIR"));
        crate::blocks::content::register(wafer, &dist_root)
            .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    }

    // `SolobaseBuilder` configures `wafer-run/inspector` with
    // `allow_anonymous: false` because solobase runs behind auth. The site
    // exposes the inspector publicly, so override here.
    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({ "allow_anonymous": true }),
    );

    // CSP override: chrome (`<sa-header>` / `<sa-footer>`) loads from
    // `https://site-kit.suppers.ai/dist/...`. Default CSP only allows
    // `'self'`; extend script-src and style-src accordingly.
    wafer.add_block_config(
        "wafer-run/security-headers",
        serde_json::json!({
            "csp": "default-src 'self'; \
                    script-src 'self' 'unsafe-inline' https://site-kit.suppers.ai; \
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
    let jwt_secret = std::env::var(solobase_core::blocks::auth::JWT_SECRET_KEY).unwrap_or_default();
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

    // 5. Site flow + router routes. Must run *after* `build()` so our
    //    `wafer-run/router` config overwrites solobase's default.
    crate::flows::register_site_main(wafer)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    Ok(())
}

/// Hide solobase feature blocks the site doesn't surface.
///
/// `BlockSettings` is consumed by `SolobaseRouterBlock`: when a block is
/// disabled here, requests to `/b/{block}/**` 404 instead of dispatching.
/// The blocks are still registered statically (every solobase feature
/// block self-registers via `register_static_block!`) and their required
/// config is still validated at start, so this is purely a routing/UX
/// concern — not a way to suppress missing-config errors.
///
/// `BlockSettings` stores the *full* block name (e.g. `suppers-ai/llm`)
/// and defaults to enabled. Explicitly set the features we don't want to
/// `false`.
fn block_settings_for_site() -> BlockSettings {
    let mut enabled = HashMap::new();
    for name in [
        "suppers-ai/legalpages",
        "suppers-ai/llm",
        "suppers-ai/projects",
        "suppers-ai/products",
        "suppers-ai/files",
        "suppers-ai/messages",
        "suppers-ai/userportal",
        "suppers-ai/vector",
        "suppers-ai/fastembed",
    ] {
        enabled.insert(name.to_string(), false);
    }
    BlockSettings::from_map(enabled)
}

// ---------------------------------------------------------------------------
// Native target — `solobase serve --target native` / `cargo run`.
// ---------------------------------------------------------------------------

/// Run the site (native target).
///
/// Composition order:
///
/// 1. Load `.env`, init tracing.
/// 2. Read `SOLOBASE_*` infra config via [`InfraConfig::from_env`].
/// 3. Build the WAFER runtime via [`SolobaseBuilder`] + the shared
///    [`register_blocks_for_site`] pre-build hook.
/// 4. Call the shared [`register_post_build_for_site`] hook (registers
///    site content, registry, inspector + security-headers overrides,
///    `wafer-site-main` flow).
/// 6. Native-only wiring: HTTP listener + observability + start +
///    `builder::post_start` + serve until shutdown.
#[cfg(feature = "target-native")]
pub async fn run() -> anyhow::Result<()> {
    // 1. Load `.env` + tracing.
    load_dotenv();
    let log_format = std::env::var("SOLOBASE_LOG_FORMAT").unwrap_or_else(|_| "text".into());
    init_tracing(&log_format);
    tracing::info!("wafer-site starting (solobase + WAFER runtime)");

    // 2. Infrastructure config (SOLOBASE_*).
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
    let builder = SolobaseBuilder::new()
        .database(solobase_native::make_sqlite_database_service(
            &infra.db_path,
        ))
        .storage(solobase_native::make_local_storage_service(
            &infra.storage_root,
        ))
        .config(Arc::new(
            wafer_block_config::service::EnvConfigService::new(),
        ))
        .crypto(solobase_native::make_jwt_crypto_service(
            std::env::var(solobase_core::blocks::auth::JWT_SECRET_KEY)
                .expect("SUPPERS_AI__AUTH__JWT_SECRET required"),
        ))
        .network(solobase_native::make_fetch_network_service())
        .logger(solobase_native::make_tracing_logger())
        .sqlite_db_path(&infra.db_path);
    let builder = register_blocks_for_site(builder)
        .map_err(|e| anyhow::anyhow!("register_blocks_for_site: {e}"))?;
    let (mut wafer, storage_block) = builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build solobase runtime: {e}"))?;

    // 4-5. Shared post-build hooks.
    register_post_build_for_site(&mut wafer)
        .map_err(|e| anyhow::anyhow!("register_post_build_for_site: {e}"))?;

    // 6. Native-only wiring: HTTP listener + observability + start.
    register_http_listener(&mut wafer, &infra.listen, "wafer-site-main");
    register_observability_hooks(&mut wafer);

    let wafer = wafer
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("failed to start WAFER runtime: {e}"))?;

    // Inject WRAP grants into the storage block (cross-block access control).
    builder::post_start(&wafer, &storage_block);
    tracing::info!(listen = %infra.listen, "wafer-site listening");

    serve_until_shutdown(&wafer).await;
    tracing::info!("wafer-site shutdown complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Cloudflare target — `solobase build --target cloudflare` consumes this
// crate as a `cdylib`, then `worker-build` packages it into a CF Worker.
// ---------------------------------------------------------------------------

/// Cloudflare Worker `fetch` entrypoint.
///
/// Defers all the heavy lifting to [`solobase_cloudflare::run`], which
/// loads vars from D1, wires services, and invokes our two registration
/// hooks before dispatching the request through WAFER.
#[cfg(feature = "target-cloudflare")]
#[worker::event(fetch)]
async fn fetch_main(
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
) -> worker::Result<worker::Response> {
    solobase_cloudflare::run(
        req,
        env,
        ctx,
        register_blocks_for_site,
        register_post_build_for_site,
    )
    .await
}
