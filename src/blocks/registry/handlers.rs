//! HTTP request handler and route dispatcher for the registry block.

use wafer_run::{
    types::CollectionSchema, Block, BlockCategory, BlockInfo, Context, InputStream, InstanceMode,
    LifecycleEvent, LifecycleType, Message, OutputStream, WaferError,
};

use crate::blocks::registry::{db, routes, RegistryConfig, NAME};

/// Registry block instance with configuration.
#[derive(Clone)]
pub struct RegistryBlock {
    pub cfg: RegistryConfig,
}

impl RegistryBlock {
    pub fn new(cfg: RegistryConfig) -> Self {
        RegistryBlock { cfg }
    }
}

// On wasm32 (cloudflare target) the futures returned by service-call
// helpers (e.g. `wafer_core::clients::database::create`) are `!Send`
// because the underlying CF Worker D1/R2 bindings wrap `JsFuture`. The
// upstream `Block` trait is declared `?Send` on wasm32 — match that here
// so the impl agrees with the trait.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Block for RegistryBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(NAME, "0.1.0", "http-handler@v1", "WAFER package registry")
            .instance_mode(InstanceMode::Singleton)
            .category(BlockCategory::Infrastructure)
            .requires(vec![
                "wafer-run/database".into(),
                "wafer-run/storage".into(),
                "suppers-ai/auth".into(),
            ])
            .collections(vec![
                CollectionSchema::new(db::ORGS)
                    .field_unique("name", "string")
                    .field_optional("owner_user_id", "string")
                    .field_optional("verified_via", "string")
                    .field_optional("verified_ref", "string")
                    .field_default("is_reserved", "bool", "false"),
                CollectionSchema::new(db::PACKAGES)
                    .field_ref("org_id", "string", &format!("{}.id", db::ORGS))
                    .field("name", "string")
                    .field_optional("summary", "string")
                    .field("created_by", "string")
                    .unique_index(&["org_id", "name"]),
                CollectionSchema::new(db::VERSIONS)
                    .field_ref("package_id", "string", &format!("{}.id", db::PACKAGES))
                    .field("version", "string")
                    .field("abi", "int")
                    .field("sha256", "string")
                    .field("storage_key", "string")
                    .field("size_bytes", "int")
                    .field_optional("license", "string")
                    .field_optional("readme_md", "string")
                    .field_default("dependencies", "string", "[]")
                    .field_default("capabilities", "string", "{}")
                    .field_default("yanked", "bool", "false")
                    .field_optional("yanked_reason", "string")
                    .field_optional("yanked_at", "datetime")
                    .field("published_by", "string")
                    .field("published_at", "datetime")
                    .unique_index(&["package_id", "version"])
                    .index(&["package_id", "yanked"]),
                CollectionSchema::new(db::CODES)
                    .field_unique("code", "string")
                    .field("user_id", "string")
                    .field("expires_at", "datetime")
                    .field_optional("used_at", "datetime"),
                CollectionSchema::new(db::TOKENS)
                    .field("user_id", "string")
                    .field_default("name", "string", "wafer-cli")
                    .field_unique("hash", "string")
                    .field_optional("last_used_at", "datetime")
                    .field_optional("revoked_at", "datetime"),
            ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let action = msg.action();
        let path = msg.path();

        // Route dispatcher: match on (action, path) and delegate to handlers.
        // Order matters: more specific patterns come before general ones.
        match (action, path) {
            // Browse endpoints.
            ("retrieve", "/registry") | ("retrieve", "/registry/") => {
                routes::browse::index(ctx, &msg, &self.cfg).await
            }
            ("retrieve", "/registry/search") => routes::browse::search(ctx, &msg, &self.cfg).await,
            // Package detail must come before the generic /registry/* catch.
            ("retrieve", p) if p.starts_with("/registry/api/packages/") => {
                routes::packages::get(ctx, &msg, &self.cfg).await
            }
            ("retrieve", "/registry/api/me") => routes::me::get(ctx, &msg, &self.cfg).await,
            ("retrieve", "/registry/cli-login") => {
                routes::cli_login::page(ctx, &msg, &self.cfg).await
            }
            ("retrieve", p) if p.starts_with("/registry/download/") => {
                routes::download::get(ctx, &msg, &self.cfg).await
            }
            // Catch remaining /registry/* GETs (package detail pages).
            ("retrieve", p) if p.starts_with("/registry/") => {
                routes::browse::package_detail(ctx, &msg, &self.cfg).await
            }
            // Publish endpoint. `http_to_message` maps POST to the action
            // `"create"` (see `wafer-block-http-listener`); the dispatcher
            // matches on that rather than a WAFER-abstract `"mutate"`.
            ("create", "/registry/api/publish") => {
                routes::publish::post(ctx, &msg, input, &self.cfg).await
            }
            // CLI login exchange.
            ("create", "/registry/api/cli-login/exchange") => {
                routes::cli_login::exchange(ctx, &msg, input, &self.cfg).await
            }
            // Yank and unyank endpoints: match by path suffix.
            ("create", p) if p.ends_with("/yank") => {
                routes::yank::yank(ctx, &msg, input, &self.cfg).await
            }
            ("create", p) if p.ends_with("/unyank") => {
                routes::yank::unyank(ctx, &msg, input, &self.cfg).await
            }
            // Unmatched route.
            _ => OutputStream::error(WaferError {
                code: wafer_run::ErrorCode::NotFound,
                message: format!("No route for {action} {path}"),
                meta: vec![],
            }),
        }
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if matches!(event.event_type, LifecycleType::Init) {
            db::seed_reserved_orgs(ctx).await.map_err(|e| {
                WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    format!("registry seed: {e}"),
                )
            })?;
            // Pre-create the storage folder so the first publish doesn't
            // race on folder creation. Safe on re-run — the helper
            // tolerates "already exists" from every backend we care about.
            db::ensure_storage_folder(ctx, &self.cfg.storage_key_prefix)
                .await
                .map_err(|e| {
                    WaferError::new(
                        wafer_run::ErrorCode::Internal,
                        format!("registry storage folder: {e}"),
                    )
                })?;
        }
        Ok(())
    }
}
