//! Shared test harness for the registry block's integration tests.
//!
//! Every test runs the registry block in the frame production gives it,
//! over impresspress's own test runtime
//! ([`impresspress_core::test_support::TestContext`]): a real SQLite
//! database behind the production database block with admin's and auth's
//! migrations applied, the production storage block over an in-memory
//! store, the production config block, a real crypto block and the real
//! `impresspress/auth-ui` login routes. WRAP grants and the registry's
//! `requires` are enforced as the runtime enforces them, so a call the
//! registry makes that production would refuse fails here too.
//!
//! Two flavors:
//!
//! - [`boot_registry_against_memory`] — in-process only. Returns the
//!   registry-framed context; callers invoke helpers in `registry::db`
//!   directly.
//!
//! - [`start_test_site`] / [`start_test_site_with_admin`] — the same stack,
//!   plus a real ephemeral HTTP server bound to `127.0.0.1:0`. Returns a
//!   [`TestApp`] with a `reqwest::Client` pointed at the server's base URL.
//!
//! The HTTP dispatch path mirrors the production `wafer-run/http-listener`:
//! axum request -> `http_to_message` -> `RegistryBlock::handle` ->
//! `wafer_output_to_response`. No stubs in the middle.
//!
//! `dead_code` is silenced at module scope because Rust compiles
//! `tests/common/mod.rs` once per test binary and each binary only uses a
//! subset of the helpers.

#![allow(dead_code)]

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    routing::any,
    Router,
};
use impresspress_core::test_support::{InMemoryStorageService, TestContext};
use serde_json::json;
use sha2::{Digest, Sha256};
use wafer_block_http_listener::{http_to_message, wafer_output_to_response};
use wafer_run::{context::Context, Block, InputStream, LifecycleEvent, LifecycleType};

use wafer_site::blocks::registry::{self, handlers::RegistryBlock, RegistryConfig};

/// The registry's test runtime: a [`TestContext`] running as
/// `wafer-run/registry`.
pub type RegistryCtx = TestContext;

/// Build the runtime, register the registry block with `cfg`, and run its
/// `Init` in the registry's own frame. Returns the registry-framed context
/// and the block.
async fn boot(cfg: RegistryConfig) -> (RegistryCtx, Arc<dyn Block>) {
    let mut ctx = TestContext::with_auth().await.with_sign_in_added();
    ctx.register_block(
        "wafer-run/storage",
        impresspress_core::blocks::storage::create(Arc::new(InMemoryStorageService::new())),
    );
    let block: Arc<dyn Block> = Arc::new(RegistryBlock::new(cfg));
    ctx.register_block(registry::NAME, block.clone());
    ctx.add_deployment_grants(registry::auth::auth_table_grants());
    let ctx = ctx.running_as(registry::NAME);

    block
        .lifecycle(
            &ctx,
            LifecycleEvent {
                event_type: LifecycleType::Init,
                data: Vec::new(),
            },
        )
        .await
        .expect("registry Init lifecycle seeds reserved orgs");
    (ctx, block)
}

/// Construct the registry block with a minimal config and run its
/// `LifecycleEvent::Init`. Returns the registry-framed context so the
/// caller can query the seeded collections via `db::*`.
pub async fn boot_registry_against_memory() -> Arc<RegistryCtx> {
    let (ctx, _block) = boot(RegistryConfig {
        admin_email: "test@example.invalid".into(),
        storage_key_prefix: "registry".into(),
        required_auth_method: String::new(),
    })
    .await;
    Arc::new(ctx)
}

// -----------------------------------------------------------------------
// HTTP harness — real axum server over the registry block.
// -----------------------------------------------------------------------

/// A live test server wired to a freshly-booted registry block + in-memory
/// SQLite + tempdir storage. Drop the struct to tear the server down (the
/// oneshot shutdown channel fires on drop).
pub struct TestApp {
    pub base: String,
    pub client: reqwest::Client,
    /// Admin PAT when the app was booted with [`start_test_site_with_admin`].
    /// Empty when booted with [`start_test_site`].
    pub admin_token: String,
    /// Non-admin PAT when the app was booted with
    /// [`start_test_site_with_user`]. Empty otherwise.
    pub user_token: String,
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

    /// POST a multipart form. `bearer` injects an `Authorization: Bearer
    /// <token>` header when `Some`.
    pub async fn post_multipart(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
        bearer: Option<&str>,
    ) -> reqwest::Response {
        let url = format!("{}{}", self.base, path);
        let mut req = self.client.post(&url).multipart(form);
        if let Some(token) = bearer {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        req.send().await.expect("test request")
    }
}

#[derive(Clone)]
struct AppState {
    ctx: Arc<RegistryCtx>,
    block: Arc<dyn Block>,
}

async fn dispatch(State(state): State<AppState>, req: Request) -> Response<Body> {
    let (parts, body) = req.into_parts();
    const MAX_BODY: usize = 32 * 1024 * 1024;
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

    let msg = http_to_message(&parts.method, path, query, &parts.headers, &remote_addr);
    let input = InputStream::from_bytes(body_bytes);
    let output = state.block.handle(state.ctx.as_ref(), msg, input).await;
    wafer_output_to_response(output).await
}

/// Start the registry block behind an ephemeral axum server. Shared setup
/// for every `start_test_site_*` entry point.
async fn start_with(admin_email: &str) -> (TestApp, Arc<RegistryCtx>) {
    let (ctx, block) = boot(RegistryConfig {
        admin_email: admin_email.into(),
        storage_key_prefix: "registry".into(),
        required_auth_method: String::new(),
    })
    .await;
    let ctx = Arc::new(ctx);

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
    let base = format!("http://{addr}");

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    let app = TestApp {
        base,
        client: reqwest::Client::new(),
        admin_token: String::new(),
        user_token: String::new(),
        _shutdown: tx,
    };
    (app, ctx)
}

/// Spin up the registry block behind an ephemeral axum server without any
/// seeded identity. Used by pre-existing Task 9/10 tests where the admin
/// gate isn't exercised.
pub async fn start_test_site() -> TestApp {
    let (app, _ctx) = start_with("test@example.invalid").await;
    app
}

/// Start the site with an admin identity pre-seeded, plus a freshly-minted
/// PAT stored in the registry's `TOKENS` collection and surfaced on
/// [`TestApp::admin_token`].
///
/// Flow: compute a `wafer_pat_<hex>`, insert its `(user_id, email, hash)`
/// into `TOKENS` via the typed DB API (same path `exchange_cli_code` takes),
/// and hand the raw token back to the caller. Requests carrying
/// `Authorization: Bearer <admin_token>` resolve through
/// `db::resolve_bearer` to the token row's email, so `require_admin`'s email
/// check hits.
pub async fn start_test_site_with_admin(admin_email: &str) -> TestApp {
    let admin_id = "test-admin-id".to_string();
    let (mut app, ctx) = start_with(admin_email).await;

    let raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    seed_token(ctx.as_ref(), &admin_id, admin_email, &raw).await;
    app.admin_token = raw;
    app
}

/// Start the site with an admin account signed in through impresspress's
/// real login route, and the reqwest client sending its `auth_token` cookie
/// on every request.
///
/// Unlike [`start_test_site_with_admin`], this variant doesn't mint a PAT —
/// the session cookie is the credential, so `registry::auth::require_user`
/// takes the session-token branch rather than the PAT-lookup shortcut.
/// That's the branch the admin actually hits in production when they open
/// `/registry/cli-login` in a browser.
pub async fn start_test_site_with_admin_cookie(admin_email: &str) -> TestApp {
    let (mut app, ctx) = start_with(admin_email).await;
    ctx.seed_account(admin_email, ADMIN_PASSWORD, "user").await;
    let session = ctx.sign_in(admin_email, ADMIN_PASSWORD).await;

    let mut default_headers = reqwest::header::HeaderMap::new();
    default_headers.insert(
        reqwest::header::COOKIE,
        reqwest::header::HeaderValue::from_str(&format!("auth_token={}", session.cookie))
            .expect("auth_token cookie header"),
    );
    app.client = reqwest::Client::builder()
        .default_headers(default_headers)
        .build()
        .expect("build cookie client");
    app
}

/// The password the cookie-session admin account signs in with.
const ADMIN_PASSWORD: &str = "correct horse battery staple";

/// Start the site with both an admin identity *and* a non-admin identity
/// seeded. The admin's PAT ends up on `admin_token`; a separate PAT for
/// the non-admin lands on `user_token`.
///
/// Non-admin tokens bypass the CLI-login flow in tests — in production
/// only admins can acquire one, but we insert it directly here to cover
/// the 403-coming-soon path.
pub async fn start_test_site_with_user(user_email: &str, admin_email: &str) -> TestApp {
    let admin_id = "test-admin-id".to_string();
    let user_id = "test-user-id".to_string();
    let (mut app, ctx) = start_with(admin_email).await;

    let admin_raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    seed_token(ctx.as_ref(), &admin_id, admin_email, &admin_raw).await;

    let user_raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    seed_token(ctx.as_ref(), &user_id, user_email, &user_raw).await;

    app.admin_token = admin_raw;
    app.user_token = user_raw;
    app
}

/// Build a `.wafer` (gzipped tar) archive containing a valid `wafer.toml`
/// and a minimal `.wasm` file. Shared by `registry_publish` and
/// `registry_yank_download` so the only variable across tests is the
/// `{org}/{name}/{version}` triple.
pub fn make_tarball(org: &str, name: &str, version: &str) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Cursor;

    let toml = format!(
        r#"[package]
org = "{org}"
name = "{name}"
version = "{version}"
abi = 1
license = "MIT"
"#
    );

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        for (path, content) in [
            ("wafer.toml", toml.as_bytes()),
            ("widget.wasm", b"\0asm\x01\x00\x00\x00" as &[u8]),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_path(path).unwrap();
            h.set_size(content.len() as u64);
            h.set_cksum();
            tar.append(&h, Cursor::new(content)).unwrap();
        }
        tar.finish().unwrap();
    }
    gz.finish().unwrap()
}

/// Insert a row into the registry's `TOKENS` collection for the given
/// user. The hash is `sha256(raw)` — same shape `exchange_cli_code`
/// produces, so `resolve_bearer` accepts the raw token verbatim.
async fn seed_token(ctx: &dyn Context, user_id: &str, email: &str, raw_token: &str) {
    use wafer_core::clients::database as db;
    let hash = hex::encode(Sha256::digest(raw_token.as_bytes()));
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("user_id".into(), json!(user_id));
    data.insert("email".into(), json!(email));
    data.insert("name".into(), json!("wafer-cli"));
    data.insert("hash".into(), json!(hash));
    // Declare optional fields explicitly so the auto-schema path doesn't
    // drop the columns. See `db.rs::insert_version` docs for the same
    // drift-guard pattern.
    data.insert("last_used_at".into(), serde_json::Value::Null);
    data.insert("revoked_at".into(), serde_json::Value::Null);
    db::create(ctx, registry::db::TOKENS, data)
        .await
        .expect("seed token");
}
