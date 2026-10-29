//! `wafer-run/registry` block — package registry for WAFER blocks.

pub mod auth;
pub mod config;
pub mod db;
pub mod handlers;
pub mod migrations;
pub mod models;
pub mod routes;
pub mod tarball;
pub mod templates;

use std::sync::Arc;
use wafer_run::Wafer;

/// Full block name. Owned by `wafer-run` per the `{org}/{block}` naming
/// convention — this is the canonical WAFER package registry block.
pub const NAME: &str = "wafer-run/registry";

/// The path prefix the registry serves: `/registry` and everything under it.
/// `impresspress/router` dispatches it here (see
/// [`crate::register_blocks_for_site`]).
pub const ROUTE_PREFIX: &str = "/registry";

/// Register the `wafer-run/registry` block and grant it the reads its
/// session-token check makes ([`auth::auth_table_grants`]). Its config is
/// declared on the block ([`config::config_vars`]) and read per request.
pub fn register(w: &mut Wafer) -> anyhow::Result<()> {
    w.register_block(NAME, Arc::new(handlers::RegistryBlock))
        .map_err(|e| anyhow::anyhow!("register {NAME}: {e}"))?;
    w.add_wrap_grants(auth::auth_table_grants())
        .map_err(|e| anyhow::anyhow!("grant {NAME} the session-token reads: {e}"))
}
