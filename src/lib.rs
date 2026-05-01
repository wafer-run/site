//! wafer-site library crate.
//!
//! The entrypoint (`src/main.rs`) is a thin shell that calls [`run`]. All
//! the composition — WAFER runtime setup, block registration, HTTP listener
//! wiring — lives here so it can be exercised from tests and future
//! integration harnesses.

pub mod blocks;
pub mod flows;

use std::{collections::HashMap, sync::Arc};

use solobase_core::builder::{self, SolobaseBuilder};
use solobase_core::features::BlockSettings;
use solobase_native::{
    init_tracing, load_dotenv, register_http_listener, register_observability_hooks,
    serve_until_shutdown, InfraConfig,
};

/// Run the site.
///
/// Composition order:
///
/// 1. Load `.env`, init tracing.
/// 2. Read `SOLOBASE_*` infra config via [`InfraConfig::from_env`].
/// 3. Build the WAFER runtime via [`SolobaseBuilder`]. This registers all
///    solobase feature blocks (including `suppers-ai/auth` and the
///    `suppers-ai/router`) plus the standard middleware and the stock
///    `site-main` flow.
/// 4. Register site-owned blocks:
///    - `wafer-site/content` — serves `$CARGO_MANIFEST_DIR/dist/**`
///      directly from disk, bypassing the namespaced storage block.
///    - `wafer-run/registry` — stub, fleshed out in Task 6+.
/// 5. Register the `wafer-site-main` flow with site-specific routes. The
///    flow's `add_block_config("wafer-run/router", ...)` overwrites the
///    route table `SolobaseBuilder::register_site_main` installed in
///    step 3 — ours is authoritative.
/// 6. Register the HTTP listener (pointed at `wafer-site-main`), start,
///    inject WRAP grants, wait for shutdown.
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

    // 3. Build the WAFER runtime.
    let (mut wafer, storage_block) = SolobaseBuilder::new()
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
            std::env::var("SUPPERS_AI__AUTH__JWT_SECRET")
                .expect("SUPPERS_AI__AUTH__JWT_SECRET required"),
        ))
        .network(solobase_native::make_fetch_network_service())
        .logger(solobase_native::make_tracing_logger())
        .block_settings(block_settings_for_site())
        .sqlite_db_path(&infra.db_path)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build solobase runtime: {e}"))?;

    // 4a. Site content block. Points at the crate's `dist/` directory so
    //     `cargo run` works from any cwd.
    let dist_root = format!("{}/dist", env!("CARGO_MANIFEST_DIR"));
    blocks::content::register(&mut wafer, &dist_root)?;

    // `SolobaseBuilder` configures `wafer-run/inspector` with
    // `allow_anonymous: false` because solobase runs behind auth. The site
    // exposes the inspector publicly (it lists the registered block set and
    // is linked from the docs), so override that here. Task 15 will revisit
    // whether this should stay public once the registry has real data.
    wafer.add_block_config(
        "wafer-run/inspector",
        serde_json::json!({ "allow_anonymous": true }),
    );

    // CSP override: the chrome (`<sa-header>` / `<sa-footer>`) loads from
    // `https://site-kit.suppers.ai/dist/...`. The default CSP shipped by
    // `wafer-run/security-headers` only allows `'self'` for scripts and
    // styles, which blocks the kit. Extend `script-src` and `style-src` to
    // permit the kit's GitHub-Pages origin. Everything else stays at the
    // restrictive default.
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

    // 4b. Registry block stub.
    let jwt_secret = std::env::var("SUPPERS_AI__AUTH__JWT_SECRET")
        .expect("SUPPERS_AI__AUTH__JWT_SECRET required");
    let registry_cfg = blocks::registry::RegistryConfig {
        jwt_secret: jwt_secret.clone(),
        admin_email: std::env::var("WAFER_RUN__REGISTRY__ADMIN_EMAIL")
            .expect("WAFER_RUN__REGISTRY__ADMIN_EMAIL required"),
        storage_key_prefix: std::env::var("WAFER_RUN__REGISTRY__STORAGE_KEY_PREFIX")
            .unwrap_or_else(|_| "registry".into()),
        required_auth_method: std::env::var("WAFER_RUN__REGISTRY__REQUIRED_AUTH_METHOD")
            .unwrap_or_default(),
    };
    blocks::registry::register(&mut wafer, registry_cfg)?;

    // 5. Site flow + router routes. This must come *after* `build()` so
    //    our `wafer-run/router` config overwrites solobase's default.
    flows::register_site_main(&mut wafer)
        .map_err(|e| anyhow::anyhow!("failed to register wafer-site-main flow: {e}"))?;

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
