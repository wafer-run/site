//! Shared test harness for the registry block's integration tests.
//!
//! Two flavors:
//!
//! - [`boot_registry_against_memory`] — in-process only. Returns an
//!   [`InMemoryCtx`] and a booted registry block; callers invoke helpers in
//!   `registry::db` directly. Used by `registry_bootstrap` and
//!   `registry_queries`.
//!
//! - [`start_test_site`] — the same in-memory stack, plus a real ephemeral
//!   HTTP server bound to `127.0.0.1:0`. Returns a [`TestApp`] with a
//!   `reqwest::Client` pointed at the server's base URL. Used by the Task 9
//!   HTTP-level tests (`registry_read_empty`).
//!
//! The HTTP dispatch path mirrors the production `wafer-run/http-listener`:
//! axum request -> `http_to_message` -> `RegistryBlock::handle` ->
//! `wafer_output_to_response`. No stubs in the middle.
//!
//! `dead_code` is silenced at module scope because Rust compiles
//! `tests/common/mod.rs` once per test binary (`registry_bootstrap`,
//! `registry_queries`, `registry_read_empty`) and each binary only uses
//! a subset of the helpers.

#![allow(dead_code)]

use std::{net::SocketAddr, sync::Arc};

use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    routing::any,
    Router,
};
use wafer_block_http_listener::{http_to_message, wafer_output_to_response};
use wafer_run::{
    block::Block,
    context::Context,
    types::{LifecycleEvent, LifecycleType, Message, WaferError},
    InputStream, OutputStream,
};

use wafer_site::blocks::registry::{self, handlers::RegistryBlock, RegistryConfig};

/// In-memory context: only the database service block is available. All other
/// `call_block` targets return `NotFound`. Good enough for tests that exercise
/// CRUD on the registry's collections.
pub struct InMemoryCtx {
    db_block: Arc<dyn Block>,
}

impl InMemoryCtx {
    pub fn new() -> Self {
        let svc = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        let db_block: Arc<dyn Block> =
            Arc::new(wafer_core::service_blocks::database::DatabaseBlock::new(svc));
        Self { db_block }
    }
}

#[async_trait::async_trait]
impl Context for InMemoryCtx {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        match block_name {
            "wafer-run/database" => self.db_block.handle(self, msg, input).await,
            _ => OutputStream::error(WaferError::new(
                wafer_run::types::ErrorCode::NotFound,
                format!("block '{block_name}' not registered in test ctx"),
            )),
        }
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
}

/// Construct the registry block with a minimal config and dispatch
/// `LifecycleEvent::Init` against an in-memory context. Returns the context
/// so the caller can query the seeded collections via `db::*`.
pub async fn boot_registry_against_memory() -> Arc<InMemoryCtx> {
    let ctx = Arc::new(InMemoryCtx::new());

    let cfg = RegistryConfig {
        admin_email: "test@example.invalid".into(),
        storage_key_prefix: "registry".into(),
    };
    let block: Arc<dyn Block> = Arc::new(RegistryBlock::new(cfg));

    block
        .lifecycle(
            ctx.as_ref(),
            LifecycleEvent {
                event_type: LifecycleType::Init,
                data: Vec::new(),
            },
        )
        .await
        .expect("registry Init lifecycle seeds reserved orgs");

    // Silence unused-import lint when only the helper is imported: mention
    // `registry` so the module is reachable from the test.
    let _: &str = registry::NAME;

    ctx
}

// -----------------------------------------------------------------------
// HTTP harness — real axum server over the registry block.
// -----------------------------------------------------------------------

/// A live test server wired to a freshly-booted registry block + in-memory
/// SQLite. Drop the struct to tear the server down (the oneshot shutdown
/// channel fires on drop).
pub struct TestApp {
    pub base: String,
    pub client: reqwest::Client,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TestApp {
    /// GET `path` against the test server. Panics on transport errors —
    /// tests that need to assert on network failure should construct a
    /// `reqwest::Request` by hand.
    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.base, path))
            .send()
            .await
            .expect("test request")
    }
}

#[derive(Clone)]
struct AppState {
    ctx: Arc<InMemoryCtx>,
    block: Arc<dyn Block>,
}

async fn dispatch(State(state): State<AppState>, req: Request) -> Response<Body> {
    let (parts, body) = req.into_parts();
    const MAX_BODY: usize = 10 * 1024 * 1024;
    let body_bytes = axum::body::to_bytes(body, MAX_BODY)
        .await
        .unwrap_or_default()
        .to_vec();
    let uri = &parts.uri;
    let path = uri.path();
    let query = uri.query().unwrap_or("");
    let remote_addr = parts
        .extensions
        .get::<SocketAddr>()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".into());

    let msg = http_to_message(parts.method, path, query, &parts.headers, &remote_addr);
    let input = InputStream::from_bytes(body_bytes);
    let output = state.block.handle(state.ctx.as_ref(), msg, input).await;
    wafer_output_to_response(output).await
}

/// Spin up the registry block behind an ephemeral axum server. Returns
/// once the TCP listener is bound — `reqwest` calls against
/// [`TestApp::base`] will hit a live server.
pub async fn start_test_site() -> TestApp {
    let ctx = Arc::new(InMemoryCtx::new());

    let cfg = RegistryConfig {
        admin_email: "test@example.invalid".into(),
        storage_key_prefix: "registry".into(),
    };
    let block: Arc<dyn Block> = Arc::new(RegistryBlock::new(cfg));

    // Seed reserved orgs (mirrors `RegistryBlock::lifecycle(Init)` as run by
    // the WAFER runtime's startup validation).
    block
        .lifecycle(
            ctx.as_ref(),
            LifecycleEvent {
                event_type: LifecycleType::Init,
                data: Vec::new(),
            },
        )
        .await
        .expect("registry Init lifecycle seeds reserved orgs");

    let state = AppState {
        ctx: ctx.clone(),
        block,
    };
    let app = Router::new()
        .route("/{*rest}", any(dispatch))
        .route("/", any(dispatch))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral");
    let addr = listener.local_addr().expect("local addr");
    let base = format!("http://{}", addr);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    TestApp {
        base,
        client: reqwest::Client::new(),
        _shutdown: tx,
    }
}
