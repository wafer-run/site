//! `wafer-site-main` flow definition + route table.
//!
//! Middleware chain mirrors solobase's `site-main`; only the router target
//! table differs. The flow is served by `register_http_listener(...,
//! "wafer-site-main")` in [`crate::run`].

/// Flow JSON registered via `wafer.add_flow_json`. Identical middleware
/// pipeline to solobase's `site-main` — we just own the ID so the HTTP
/// listener targets the right set of routes.
pub const JSON: &str = r#"{
    "id": "wafer-site-main",
    "name": "WAFER Site Main",
    "version": "0.1.0",
    "description": "Top-level HTTP dispatch for wafer-site — site content + solobase router",
    "steps": [
        { "id": "security-headers", "block": "wafer-run/security-headers" },
        { "id": "cors",             "block": "wafer-run/cors" },
        { "id": "readonly-guard",   "block": "wafer-run/readonly-guard" },
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
        // Runtime debugger. Registered by `SolobaseBuilder` under
        // `wafer-run/inspector`.
        { "path": "/_inspector/**", "block": "wafer-run/inspector" },
        { "path": "/_inspector",    "block": "wafer-run/inspector" },

        // Package registry — `wafer-run/registry` (publish, yank, download,
        // browse, CLI login). Registered by `crate::blocks::registry`.
        { "path": "/registry/**", "block": "wafer-run/registry" },
        { "path": "/registry",    "block": "wafer-run/registry" },

        // Solobase-owned routes: auth (`/b/auth/*`), admin, health, etc.
        // `suppers-ai/router` is registered by `SolobaseBuilder`.
        { "path": "/b/**",                   "block": "suppers-ai/router" },
        { "path": "/health",                 "block": "suppers-ai/router" },
        { "path": "/openapi.json",           "block": "suppers-ai/router" },
        { "path": "/.well-known/agent.json", "block": "suppers-ai/router" },

        // Landing page + docs + playground + everything else served from
        // `$CARGO_MANIFEST_DIR/dist`.
        { "path": "/**", "block": "wafer-site/content" }
    ])
}
