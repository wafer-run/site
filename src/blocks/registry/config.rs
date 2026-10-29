//! The registry block's config variables.
//!
//! Declared in the block's `BlockInfo::config_keys` ([`config_vars`]), stored
//! as rows of the deployment's variables table (the admin Variables page
//! edits them) and read per request through the config service, so an edit
//! applies to the next request — on every target, the same way. All three
//! are block-scoped (`WAFER_RUN__REGISTRY__*`, the prefix of
//! `wafer-run/registry`) and all three are optional: an unset value is a
//! working registry with publishing closed, never a block that fails to
//! start (`/_health` stays 200).
//!
//! The process environment does not set them. impresspress (at the pinned
//! `IMPRESSPRESS_REF`) seeds a variables row from the environment only for a
//! key one of its own blocks declares (`config_vars::is_declared_key`), and
//! the Cloudflare target seeds no row from Worker vars or secrets.

use wafer_core::clients::config;
use wafer_run::{context::Context, ConfigVar, WaferError};

/// The one account allowed to publish, yank and mint CLI tokens, by email.
/// Empty — the default — means nobody: publishing is closed.
pub const ADMIN_EMAIL_KEY: &str = "WAFER_RUN__REGISTRY__ADMIN_EMAIL";

/// Top-level storage folder registry tarballs live under.
pub const STORAGE_KEY_PREFIX_KEY: &str = "WAFER_RUN__REGISTRY__STORAGE_KEY_PREFIX";

/// When non-empty, the auth method (the access token's `auth_method` claim,
/// e.g. `oauth.github`) the admin must have signed in with for an
/// admin-gated route. Empty — the default — accepts any method.
pub const REQUIRED_AUTH_METHOD_KEY: &str = "WAFER_RUN__REGISTRY__REQUIRED_AUTH_METHOD";

/// [`STORAGE_KEY_PREFIX_KEY`]'s default.
pub const DEFAULT_STORAGE_KEY_PREFIX: &str = "registry";

/// The registry's declared config variables, for `BlockInfo::config_keys`.
pub fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            ADMIN_EMAIL_KEY,
            "Email of the one account that may publish and yank packages and \
             mint CLI tokens. Empty closes publishing to everyone.",
            "",
        )
        .name("Registry admin email")
        .optional(),
        ConfigVar::new(
            STORAGE_KEY_PREFIX_KEY,
            "Storage folder package tarballs are kept under",
            DEFAULT_STORAGE_KEY_PREFIX,
        )
        .name("Registry storage folder")
        .optional(),
        ConfigVar::new(
            REQUIRED_AUTH_METHOD_KEY,
            "Auth method the registry admin must have signed in with for \
             admin actions (e.g. oauth.github: an OAuth-issued token proves \
             the provider verified the address, a password login does not). \
             Empty accepts any method.",
            "",
        )
        .name("Registry admin auth method")
        .optional(),
    ]
}

/// [`ADMIN_EMAIL_KEY`]'s value; empty when unset.
pub async fn admin_email(ctx: &dyn Context) -> Result<String, WaferError> {
    config::get_default(ctx, ADMIN_EMAIL_KEY, "").await
}

/// [`STORAGE_KEY_PREFIX_KEY`]'s value, or [`DEFAULT_STORAGE_KEY_PREFIX`]
/// when unset.
pub async fn storage_key_prefix(ctx: &dyn Context) -> Result<String, WaferError> {
    config::get_default(ctx, STORAGE_KEY_PREFIX_KEY, DEFAULT_STORAGE_KEY_PREFIX).await
}

/// [`REQUIRED_AUTH_METHOD_KEY`]'s value; empty when unset.
pub async fn required_auth_method(ctx: &dyn Context) -> Result<String, WaferError> {
    config::get_default(ctx, REQUIRED_AUTH_METHOD_KEY, "").await
}
