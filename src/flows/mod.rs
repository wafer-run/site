//! Flow definitions owned by the site binary.
//!
//! Rather than reuse solobase's stock `site-main` flow — whose default
//! route table sends `/**` to `wafer-run/web` (the namespaced-storage
//! path that 404s on our unmigrated `dist/`) — we define `wafer-site-main`
//! here. It keeps the same middleware chain (security-headers → cors →
//! readonly-guard → router) but configures `wafer-run/router` with
//! site-specific routes so `/docs`, `/playground`, `/registry`, and
//! the landing page resolve to site-owned blocks, while `/b/**` still
//! delegates to `suppers-ai/router` for auth/admin/etc.

pub mod site;

use wafer_run::{RuntimeError, Wafer};

/// Register the `wafer-site-main` flow and its router configuration.
///
/// Must be called *after* `SolobaseBuilder::build()` because that builder
/// installs `site-main` with its own `wafer-run/router` config; our later
/// `add_block_config("wafer-run/router", ...)` call overwrites the default
/// route table with ours.
pub fn register_site_main(w: &mut Wafer) -> Result<(), RuntimeError> {
    w.add_block_config(
        "wafer-run/router",
        serde_json::json!({ "routes": site::routes() }),
    );
    w.add_flow_json(site::JSON)
}
