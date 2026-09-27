//! `wafer-site-main` flow definition + route table.
//!
//! Middleware chain mirrors impresspress's `site-main`; only the router target
//! table differs. The native HTTP listener dispatches to [`FLOW_ID`] (see
//! [`crate::run`]).

/// The flow's id: the flow the HTTP listener dispatches every request to.
pub const FLOW_ID: &str = "wafer-site-main";

/// Flow JSON registered via `wafer.add_flow_json`. Identical middleware
/// pipeline to impresspress's `site-main`
/// (`impresspress_core::flows::site_main::JSON`) — we just own the ID so the
/// HTTP listener targets the right set of routes.
pub const JSON: &str = r#"{
    "id": "wafer-site-main",
    "name": "WAFER Site Main",
    "version": "0.1.0",
    "description": "Top-level HTTP dispatch for wafer-site — site content + impresspress router",
    "steps": [
        { "id": "security-headers", "block": "wafer-run/security-headers" },
        { "id": "cors",             "block": "wafer-run/cors" },
        { "id": "readonly-guard",   "block": "wafer-run/readonly-guard" },
        { "id": "body-limit",       "block": "impresspress/body-limit" },
        { "id": "router",           "block": "wafer-run/router" }
    ],
    "config": { "on_error": "stop" }
}"#;

/// Route table installed on `wafer-run/router` for the site flow.
///
/// Precedence follows the order of this vec (see
/// `wafer-block-router::parse_routes` — first match wins). Block-specific
/// routes are listed before the catch-all.
pub fn routes() -> serde_json::Value {
    serde_json::json!([
        // Runtime debugger. Registered by `ImpresspressBuilder` under
        // `wafer-run/inspector`.
        { "path": "/_inspector/**", "block": "wafer-run/inspector" },
        { "path": "/_inspector",    "block": "wafer-run/inspector" },

        // Package registry — `wafer-run/registry` (publish, yank, download,
        // browse, CLI login). Registered by `crate::blocks::registry`.
        { "path": "/registry/**", "block": "wafer-run/registry" },
        { "path": "/registry",    "block": "wafer-run/registry" },

        // Deploy-time config validation endpoint. Returns 200 when every
        // block's required `ConfigVar`s have a value or a default; 503
        // otherwise with a per-block breakdown. The deploy script gates
        // rollback on this status code.
        { "path": "/_health", "block": "wafer-site/health" },

        // Impresspress-owned routes: auth (`/b/auth/*`), admin, health, etc.
        // `impresspress/router` is registered by `ImpresspressBuilder`.
        { "path": "/b/**",                   "block": "impresspress/router" },
        { "path": "/health",                 "block": "impresspress/router" },
        { "path": "/openapi.json",           "block": "impresspress/router" },
        { "path": "/.well-known/agent.json", "block": "impresspress/router" },

        // Landing page + docs + playground + everything else served from
        // `$CARGO_MANIFEST_DIR/dist`.
        { "path": "/**", "block": "wafer-site/content" }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flow_json_carries_flow_id() {
        let flow: serde_json::Value = serde_json::from_str(JSON).expect("the flow is JSON");
        assert_eq!(flow["id"], FLOW_ID);
    }

    /// The middleware chain is impresspress's `site-main` chain, step for
    /// step; only the router's route table differs.
    #[test]
    fn the_middleware_chain_is_impresspress_site_main() {
        let steps = |json: &str| -> Vec<(String, String)> {
            let flow: serde_json::Value = serde_json::from_str(json).expect("the flow is JSON");
            flow["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .map(|s| {
                    (
                        s["id"].as_str().unwrap_or_default().to_string(),
                        s["block"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        };
        assert_eq!(
            steps(JSON),
            steps(impresspress_core::flows::site_main::JSON)
        );
    }
}
