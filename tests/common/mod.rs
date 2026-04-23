//! Shared test harness for the registry block's integration tests.
//!
//! Two flavors:
//!
//! - [`boot_registry_against_memory`] — in-process only. Returns an
//!   [`InMemoryCtx`] and a booted registry block; callers invoke helpers in
//!   `registry::db` directly. Used by `registry_bootstrap` and
//!   `registry_queries`.
//!
//! - [`start_test_site`] / [`start_test_site_with_admin`] — the same
//!   in-memory stack, plus a real ephemeral HTTP server bound to
//!   `127.0.0.1:0`. Returns a [`TestApp`] with a `reqwest::Client` pointed
//!   at the server's base URL. Used by the HTTP-level tests.
//!
//! Task 13 extends the harness to wire a `wafer-run/storage` block
//! (LocalStorageService on a tempdir) and a minimal `suppers-ai/auth` stub
//! that answers `auth.require_user` / `auth.user_profile` from a
//! statically-seeded user row. That stub is what lets the publish admin
//! gate work without dragging the full auth block into the test graph.
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
use serde_json::json;
use sha2::{Digest, Sha256};
use wafer_block_http_listener::{http_to_message, wafer_output_to_response};
use wafer_run::{
    block::Block,
    context::Context,
    types::{LifecycleEvent, LifecycleType, Message, WaferError},
    InputStream, OutputStream,
};

use wafer_site::blocks::registry::{self, handlers::RegistryBlock, RegistryConfig};

/// In-memory context wiring the three blocks the registry's publish/read
/// paths need:
///
/// - `wafer-run/database` — SQLite-in-memory, backs the registry's own
///   collections.
/// - `wafer-run/storage` — LocalStorageService on a tempdir, so `put` /
///   `delete` / `get` actually persist something we can verify.
/// - `suppers-ai/auth` — a minimal stub that resolves seeded
///   `(user_id -> email)` mappings for `auth.user_profile`. Empty when
///   unseeded, in which case `require_admin` treats the user as non-admin.
pub struct InMemoryCtx {
    db_block: Arc<dyn Block>,
    storage_block: Arc<dyn Block>,
    /// Seeded identities: `user_id -> email`. The stubbed
    /// `auth.user_profile` matches the request's `user_id` against this
    /// map. `auth.require_user` always fails in the stub — PAT-based auth
    /// (via `db::resolve_bearer`) runs *before* the fallback, so only
    /// `auth.user_profile` is exercised in practice.
    identities: HashMap<String, String>,
    /// Holder for the tempdir backing LocalStorageService. Dropped when
    /// the context is, which tears the filesystem down too.
    _storage_tmp: tempfile::TempDir,
}

impl InMemoryCtx {
    pub fn new() -> Self {
        Self::new_with_identities(HashMap::new())
    }

    /// Construct with a `user_id -> email` identity map. The stub
    /// `suppers-ai/auth` block uses it to answer `auth.user_profile`
    /// lookups; `auth.require_user` always errors (tests exercise PAT
    /// auth, which runs earlier in `require_user`).
    pub fn new_with_identities(identities: HashMap<String, String>) -> Self {
        // Database: in-memory SQLite.
        let svc = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        let db_block: Arc<dyn Block> =
            Arc::new(wafer_core::service_blocks::database::DatabaseBlock::new(svc));

        // Storage: LocalStorageService on a tempdir. Using the real block
        // rather than a shim gives us accurate error types (folders get
        // auto-created under LocalStorageService::put's path).
        let tmp = tempfile::TempDir::new().expect("tempdir for storage");
        let storage_svc = Arc::new(
            wafer_block_local_storage::service::LocalStorageService::new(tmp.path())
                .expect("LocalStorageService"),
        );
        let storage_block: Arc<dyn Block> = Arc::new(
            wafer_core::service_blocks::storage::StorageBlock::new(storage_svc),
        );

        Self {
            db_block,
            storage_block,
            identities,
            _storage_tmp: tmp,
        }
    }
}

#[async_trait::async_trait]
impl Context for InMemoryCtx {
    async fn call_block(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
        match block_name {
            "wafer-run/database" => self.db_block.handle(self, msg, input).await,
            "wafer-run/storage" => self.storage_block.handle(self, msg, input).await,
            "suppers-ai/auth" => self.handle_auth_stub(msg, input).await,
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

impl InMemoryCtx {
    /// Minimal `suppers-ai/auth` stub.
    ///
    /// - `auth.require_user` — honors the session-cookie convention
    ///   `Cookie: session=<user_id>`. When the incoming `http.header.cookie`
    ///   meta carries `session=<id>` and `<id>` is present in
    ///   `identities`, return `{"user_id": "<id>"}`. Every other shape
    ///   (no cookie, unknown user, other cookie values) surfaces as
    ///   `Unauthenticated`. This keeps the PAT-based path (which runs
    ///   *before* the auth-block fallback in
    ///   `registry::auth::require_user`) the primary credential in
    ///   existing tests while letting a new cookie-branch test exercise
    ///   the session flow end-to-end.
    ///
    /// - `auth.user_profile` — decodes `{"user_id": "..."}` from the body
    ///   and returns the matching seeded email, or empty when unknown.
    ///   Matches the real auth block's contract so the registry's
    ///   `fetch_email` round-trip works verbatim.
    async fn handle_auth_stub(&self, msg: Message, input: InputStream) -> OutputStream {
        // `Message::new(op)` stores the service-op name in `msg.kind`, not
        // in the `req.action` meta. The real auth block's handler
        // discriminates on `msg.kind.as_str()` (see
        // wafer-core/src/interfaces/auth/handler.rs) — we match the same
        // field here so service-op calls route correctly.
        let action = msg.kind.clone();
        let body_bytes = input.collect_to_bytes().await;
        match action.as_str() {
            "auth.require_user" => {
                // `registry::auth::require_user` copies the HTTP cookie
                // header onto `http.header.cookie` before dispatching to
                // the auth block; `Message::header("cookie")` reads it
                // back via the same convention. Parse
                // `session=<user_id>` out of it and match against the
                // seeded identity map.
                let cookie = msg.header("cookie");
                let user_id = parse_session_cookie(cookie);
                match user_id.filter(|id| self.identities.contains_key(id.as_str())) {
                    Some(id) => {
                        let body =
                            serde_json::to_vec(&json!({ "user_id": id })).unwrap();
                        OutputStream::respond(body)
                    }
                    None => OutputStream::error(WaferError::new(
                        wafer_run::types::ErrorCode::Unauthenticated,
                        "auth stub: missing or unknown session cookie".to_string(),
                    )),
                }
            }
            "auth.user_profile" => {
                #[derive(serde::Deserialize)]
                struct Req {
                    user_id: String,
                }
                let Ok(req) = serde_json::from_slice::<Req>(&body_bytes) else {
                    return OutputStream::error(WaferError::new(
                        wafer_run::types::ErrorCode::InvalidArgument,
                        "auth stub: bad body".to_string(),
                    ));
                };
                let email = self
                    .identities
                    .get(&req.user_id)
                    .cloned()
                    .unwrap_or_default();
                let body = serde_json::to_vec(&json!({ "email": email })).unwrap();
                OutputStream::respond(body)
            }
            other => OutputStream::error(WaferError::new(
                wafer_run::types::ErrorCode::NotFound,
                format!("suppers-ai/auth stub: unhandled action {other}"),
            )),
        }
    }
}

/// Parse a `session=<value>` token out of an RFC 6265 `Cookie` header.
/// Returns `None` when the header is empty or no `session=...` segment is
/// present. Extra cookies on the line are ignored.
fn parse_session_cookie(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("session=") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
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

    let _: &str = registry::NAME;

    ctx
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
    ctx: Arc<InMemoryCtx>,
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

    let msg = http_to_message(parts.method, path, query, &parts.headers, &remote_addr);
    let input = InputStream::from_bytes(body_bytes);
    let output = state.block.handle(state.ctx.as_ref(), msg, input).await;
    wafer_output_to_response(output).await
}

/// Start the registry block behind an ephemeral axum server. Shared setup
/// for every `start_test_site_*` entry point.
async fn start_with(
    admin_email: &str,
    identities: HashMap<String, String>,
) -> (TestApp, Arc<InMemoryCtx>) {
    let ctx = Arc::new(InMemoryCtx::new_with_identities(identities));

    let cfg = RegistryConfig {
        admin_email: admin_email.into(),
        storage_key_prefix: "registry".into(),
    };
    let block: Arc<dyn Block> = Arc::new(RegistryBlock::new(cfg));

    // Mirror `RegistryBlock::lifecycle(Init)` as run by the WAFER runtime's
    // startup validation — seed the reserved orgs.
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
    let (app, _ctx) = start_with("test@example.invalid", HashMap::new()).await;
    app
}

/// Start the site with an admin identity pre-seeded, plus a freshly-minted
/// PAT stored in the registry's `TOKENS` collection and surfaced on
/// [`TestApp::admin_token`].
///
/// Flow: compute a `wafer_pat_<hex>`, insert its `(user_id, hash)` into
/// `TOKENS` via the typed DB API (same path `exchange_cli_code` takes), and
/// hand the raw token back to the caller. Requests carrying
/// `Authorization: Bearer <admin_token>` resolve through
/// `db::resolve_bearer` → `user_id` → stub's `user_profile` → `email`, so
/// `require_admin`'s email check hits.
pub async fn start_test_site_with_admin(admin_email: &str) -> TestApp {
    let admin_id = "test-admin-id".to_string();
    let mut identities = HashMap::new();
    identities.insert(admin_id.clone(), admin_email.to_string());
    let (mut app, ctx) = start_with(admin_email, identities).await;

    let raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    seed_token(ctx.as_ref(), &admin_id, &raw).await;
    app.admin_token = raw;
    app
}

/// Start the site with an admin identity seeded and the reqwest client
/// configured to send `Cookie: session=admin-user-id` on every request.
///
/// Unlike [`start_test_site_with_admin`], this variant doesn't mint a PAT
/// — the cookie is the credential, so `registry::auth::require_user`
/// routes through the `suppers-ai/auth` session branch rather than the
/// PAT-lookup shortcut. That's the branch the admin actually hits in
/// production when they open `/registry/cli-login` in a browser.
pub async fn start_test_site_with_admin_cookie(admin_email: &str) -> TestApp {
    // The stub matches the cookie value against identity keys verbatim,
    // so both sides agree on `"admin-user-id"`.
    let admin_id = "admin-user-id".to_string();
    let mut identities = HashMap::new();
    identities.insert(admin_id.clone(), admin_email.to_string());
    let (mut app, _ctx) = start_with(admin_email, identities).await;

    // Swap the default client for one that pins the session cookie.
    let mut default_headers = reqwest::header::HeaderMap::new();
    default_headers.insert(
        reqwest::header::COOKIE,
        reqwest::header::HeaderValue::from_str(&format!("session={admin_id}"))
            .expect("session cookie header"),
    );
    app.client = reqwest::Client::builder()
        .default_headers(default_headers)
        .build()
        .expect("build cookie client");
    app
}

/// Start the site with both an admin identity *and* a non-admin identity
/// seeded. The admin's PAT ends up on `admin_token`; a separate PAT for
/// the non-admin lands on `user_token`.
///
/// Non-admin tokens bypass the CLI-login flow in tests — in production
/// only admins can acquire one, but we insert it directly here to cover
/// the 403-coming-soon path.
pub async fn start_test_site_with_user(
    user_email: &str,
    admin_email: &str,
) -> TestApp {
    let admin_id = "test-admin-id".to_string();
    let user_id = "test-user-id".to_string();
    let mut identities = HashMap::new();
    identities.insert(admin_id.clone(), admin_email.to_string());
    identities.insert(user_id.clone(), user_email.to_string());
    let (mut app, ctx) = start_with(admin_email, identities).await;

    let admin_raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    seed_token(ctx.as_ref(), &admin_id, &admin_raw).await;

    let user_raw = format!("wafer_pat_{}", hex::encode(rand::random::<[u8; 32]>()));
    seed_token(ctx.as_ref(), &user_id, &user_raw).await;

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
async fn seed_token(ctx: &dyn Context, user_id: &str, raw_token: &str) {
    use wafer_core::clients::database as db;
    let hash = hex::encode(Sha256::digest(raw_token.as_bytes()));
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("user_id".into(), json!(user_id));
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
