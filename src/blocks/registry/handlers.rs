//! HTTP request handler and route dispatcher for the registry block.

use wafer_run::{
    Block, BlockCategory, BlockInfo, Context, InputStream, InstanceMode, LifecycleEvent, Message,
    OutputStream, WaferError,
};

use crate::blocks::registry::{routes, RegistryConfig, NAME};

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

#[async_trait::async_trait]
impl Block for RegistryBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            NAME,
            "0.1.0",
            "http-handler@v1",
            "WAFER package registry (dispatch scaffold)",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
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
            ("retrieve", "/registry/search") => {
                routes::browse::search(ctx, &msg, &self.cfg).await
            }
            // Package detail must come before the generic /registry/* catch.
            ("retrieve", p) if p.starts_with("/registry/api/packages/") => {
                routes::packages::get(ctx, &msg, &self.cfg).await
            }
            ("retrieve", "/registry/api/me") => {
                routes::me::get(ctx, &msg, &self.cfg).await
            }
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
            // Publish endpoint.
            ("mutate", "/registry/api/publish") => {
                routes::publish::post(ctx, &msg, input, &self.cfg).await
            }
            // CLI login exchange.
            ("mutate", "/registry/api/cli-login/exchange") => {
                routes::cli_login::exchange(ctx, &msg, input, &self.cfg).await
            }
            // Yank and unyank endpoints: match by path suffix.
            ("mutate", p) if p.ends_with("/yank") => {
                routes::yank::yank(ctx, &msg, input, &self.cfg).await
            }
            ("mutate", p) if p.ends_with("/unyank") => {
                routes::yank::unyank(ctx, &msg, input, &self.cfg).await
            }
            // Unmatched route.
            _ => OutputStream::error(WaferError {
                code: wafer_run::ErrorCode::NotFound,
                message: format!("No route for {} {}", action, path),
                meta: vec![],
            }),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}
