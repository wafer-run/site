//! CSRF protection on the registry's state-changing routes, driven through
//! the runtime the binary serves.
//!
//! The other registry tests call `RegistryBlock::handle` directly, which
//! skips the `wafer-site-main` flow and whatever it routes through. Here the
//! site boots exactly as `wafer_site::run` boots it —
//! `impresspress_server::start_native` with [`wafer_site::native_app_hooks`]
//! and the site flow as the listener flow — on a temp SQLite database, and
//! every request goes over a real socket.
//!
//! A cookie-authenticated unsafe request that a browser marks as coming from
//! another site (`Sec-Fetch-Site`), or that names a foreign `Origin`, must be
//! refused before the registry acts on it. Requests from this origin, and
//! requests authenticated by an `Authorization: Bearer` credential (a PAT or
//! an impresspress access token — neither of which a cross-site page can
//! attach), must still work.

mod common;

use std::{collections::HashMap, time::Duration};

use common::make_tarball;
use impresspress_native::{InfraConfig, ListenerEnv};
use reqwest::{header, RequestBuilder, Response};

const ADMIN_EMAIL: &str = "registry-admin@example.com";
const ADMIN_PASSWORD: &str = "correct horse battery staple 42";

/// A port nothing is listening on, for the listener to bind.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// The site's runtime, serving on a loopback port.
struct Site {
    base: String,
    client: reqwest::Client,
    wafer: std::sync::Arc<wafer_run::Wafer>,
    _tmp: tempfile::TempDir,
}

impl Site {
    async fn boot() -> Site {
        // The registry block reads its admin email from the process
        // environment (see `register_post_build_for_site`). This binary
        // holds this one test, so nothing else observes the write.
        std::env::set_var("WAFER_RUN__REGISTRY__ADMIN_EMAIL", ADMIN_EMAIL);
        std::env::remove_var("WAFER_RUN__REGISTRY__REQUIRED_AUTH_METHOD");

        let tmp = tempfile::tempdir().expect("tempdir");
        let storage_root = tmp.path().join("storage");
        std::fs::create_dir_all(&storage_root).expect("create storage root");
        let port = free_port();
        let infra = InfraConfig {
            listen: format!("127.0.0.1:{port}"),
            db_type: "sqlite".to_string(),
            db_path: tmp
                .path()
                .join("site.sqlite3")
                .to_str()
                .unwrap()
                .to_string(),
            db_url: None,
            storage_type: "local".to_string(),
            storage_root: storage_root.to_str().unwrap().to_string(),
            model_cache_dir: tmp.path().join("models").to_str().unwrap().to_string(),
            listener: ListenerEnv::default(),
        };
        let database =
            impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
                .await
                .expect("construct sqlite database service");
        let wafer = impresspress_server::start_native(
            &infra,
            database,
            &HashMap::new(),
            Default::default(),
            false,
            wafer_site::flows::site::FLOW_ID,
            wafer_site::native_app_hooks().expect("the site's hooks"),
        )
        .await
        .expect("the site starts");

        let site = Site {
            base: format!("http://127.0.0.1:{port}"),
            client: reqwest::Client::new(),
            wafer,
            _tmp: tmp,
        };
        for _ in 0..50 {
            if site
                .client
                .get(site.url("/registry/search"))
                .send()
                .await
                .is_ok()
            {
                return site;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the listener never accepted a connection");
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Sign the admin up through impresspress's real signup route and keep
    /// what it hands a browser (the `auth_token` cookie) and an API client
    /// (the access token).
    async fn sign_up_admin(&self) -> (String, String) {
        let resp = self
            .client
            .post(self.url("/b/auth/api/signup"))
            .json(&serde_json::json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
            .send()
            .await
            .expect("signup request");
        let cookie = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| {
                v.split(';')
                    .next()
                    .and_then(|pair| pair.trim().strip_prefix("auth_token="))
                    .map(str::to_string)
            });
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        assert_eq!(status, 201, "signup: {body}");
        let access_token = body["access_token"]
            .as_str()
            .expect("signup issues an access token")
            .to_string();
        (
            cookie.expect("signup sets the auth_token cookie"),
            access_token,
        )
    }
}

/// A multipart publish of `acme/widget@{version}`.
fn publish(req: RequestBuilder, version: &str) -> RequestBuilder {
    let tarball = make_tarball("acme", "widget", version);
    req.multipart(
        reqwest::multipart::Form::new().part(
            "tarball",
            reqwest::multipart::Part::bytes(tarball)
                .file_name("w.wafer")
                .mime_str("application/octet-stream")
                .unwrap(),
        ),
    )
}

fn with_cookie(req: RequestBuilder, cookie: &str) -> RequestBuilder {
    req.header(header::COOKIE, format!("auth_token={cookie}"))
}

async fn status_and_body(resp: Response) -> (u16, String) {
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

/// The published versions of `acme/widget`, and which of them are yanked.
async fn versions(site: &Site) -> Vec<(String, bool)> {
    let resp = site
        .client
        .get(site.url("/registry/api/packages/acme/widget"))
        .send()
        .await
        .expect("package detail request");
    if resp.status() == 404 {
        return Vec::new();
    }
    let json: serde_json::Value = resp.json().await.expect("package detail json");
    json["versions"]
        .as_array()
        .expect("versions array")
        .iter()
        .map(|v| {
            (
                v["version"].as_str().unwrap_or_default().to_string(),
                v["yanked"].as_i64().unwrap_or_default() != 0,
            )
        })
        .collect()
}

fn published(versions: &[(String, bool)], version: &str) -> bool {
    versions.iter().any(|(v, _)| v == version)
}

fn yanked(versions: &[(String, bool)], version: &str) -> bool {
    versions.iter().any(|(v, y)| v == version && *y)
}

/// The 64-hex-char device code the CLI-login page renders.
fn cli_code(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    chars
        .windows(64)
        .find(|w| w.iter().all(char::is_ascii_hexdigit))
        .map(|w| w.iter().collect())
        .unwrap_or_else(|| panic!("no 64-hex-char code in the CLI-login page: {html}"))
}

#[tokio::test]
async fn cookie_authenticated_registry_mutations_are_csrf_protected() {
    let site = Site::boot().await;
    let (cookie, access_token) = site.sign_up_admin().await;
    let own_origin = site.base.clone();

    // --- A same-origin cookie POST is served. ---------------------------
    let (status, body) = status_and_body(
        publish(
            with_cookie(site.client.post(site.url("/registry/api/publish")), &cookie),
            "0.1.0",
        )
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .expect("same-origin publish"),
    )
    .await;
    assert_eq!(
        status, 200,
        "a same-origin cookie publish is served: {body}"
    );

    // Without Fetch Metadata, an `Origin` naming this site is accepted too.
    let (status, body) = status_and_body(
        publish(
            with_cookie(site.client.post(site.url("/registry/api/publish")), &cookie),
            "0.2.0",
        )
        .header(header::ORIGIN, &own_origin)
        .send()
        .await
        .expect("own-origin publish"),
    )
    .await;
    assert_eq!(
        status, 200,
        "an own-Origin cookie publish is served: {body}"
    );
    assert!(published(&versions(&site).await, "0.2.0"));

    // --- Cross-site cookie POSTs are refused before the registry acts. ---
    let (status, body) = status_and_body(
        publish(
            with_cookie(site.client.post(site.url("/registry/api/publish")), &cookie),
            "0.3.0",
        )
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .expect("cross-site publish"),
    )
    .await;
    assert_eq!(
        status, 403,
        "a cross-site cookie publish is refused: {body}"
    );
    assert!(
        body.contains("cross-origin request blocked"),
        "refused by the CSRF policy, not by the registry: {body}"
    );
    assert!(!published(&versions(&site).await, "0.3.0"));

    // A same-site (sibling subdomain) request is not this origin either.
    let (status, body) = status_and_body(
        publish(
            with_cookie(site.client.post(site.url("/registry/api/publish")), &cookie),
            "0.3.1",
        )
        .header("sec-fetch-site", "same-site")
        .send()
        .await
        .expect("same-site publish"),
    )
    .await;
    assert_eq!(status, 403, "a same-site cookie publish is refused: {body}");
    assert!(!published(&versions(&site).await, "0.3.1"));

    // No Fetch Metadata, a foreign `Origin`.
    let (status, body) = status_and_body(
        publish(
            with_cookie(site.client.post(site.url("/registry/api/publish")), &cookie),
            "0.4.0",
        )
        .header(header::ORIGIN, "https://evil.example")
        .send()
        .await
        .expect("foreign-origin publish"),
    )
    .await;
    assert_eq!(
        status, 403,
        "a foreign-Origin cookie publish is refused: {body}"
    );
    assert!(!published(&versions(&site).await, "0.4.0"));

    // Yank is a state change too.
    let (status, body) = status_and_body(
        with_cookie(
            site.client
                .post(site.url("/registry/api/packages/acme/widget/0.1.0/yank")),
            &cookie,
        )
        .header("sec-fetch-site", "cross-site")
        .json(&serde_json::json!({ "reason": "forged" }))
        .send()
        .await
        .expect("cross-site yank"),
    )
    .await;
    assert_eq!(status, 403, "a cross-site cookie yank is refused: {body}");
    assert!(!yanked(&versions(&site).await, "0.1.0"));

    // A non-bearer `Authorization` header does not turn the cookie into a
    // CSRF-exempt credential: the registry resolves the caller from the same
    // source the CSRF policy judged, so the cookie is not consulted at all.
    let (status, body) = status_and_body(
        publish(
            with_cookie(site.client.post(site.url("/registry/api/publish")), &cookie),
            "0.5.0",
        )
        .header(header::AUTHORIZATION, "Basic Zm9vOmJhcg==")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .expect("cross-site publish with a non-bearer Authorization header"),
    )
    .await;
    assert_eq!(
        status, 401,
        "the cookie does not authenticate a request carrying another Authorization header: {body}"
    );
    assert!(!published(&versions(&site).await, "0.5.0"));

    // --- Bearer-authenticated requests are exempt. -----------------------
    // A registry PAT, through the real CLI-login flow.
    let page = with_cookie(site.client.get(site.url("/registry/cli-login")), &cookie)
        .send()
        .await
        .expect("cli-login page");
    assert_eq!(page.status(), 200);
    let code = cli_code(&page.text().await.expect("cli-login body"));
    let exchanged: serde_json::Value = site
        .client
        .post(site.url("/registry/api/cli-login/exchange"))
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .expect("exchange request")
        .json()
        .await
        .expect("exchange json");
    let pat = exchanged["token"]
        .as_str()
        .unwrap_or_else(|| panic!("exchange yields a PAT: {exchanged}"))
        .to_string();

    let (status, body) = status_and_body(
        publish(site.client.post(site.url("/registry/api/publish")), "0.6.0")
            .bearer_auth(&pat)
            .header("sec-fetch-site", "cross-site")
            .send()
            .await
            .expect("bearer PAT publish"),
    )
    .await;
    assert_eq!(
        status, 200,
        "a bearer-PAT publish is not CSRF-gated: {body}"
    );

    // An impresspress access token in the `Authorization` header.
    let (status, body) = status_and_body(
        publish(site.client.post(site.url("/registry/api/publish")), "0.7.0")
            .bearer_auth(&access_token)
            .header(header::ORIGIN, "https://evil.example")
            .send()
            .await
            .expect("bearer access-token publish"),
    )
    .await;
    assert_eq!(
        status, 200,
        "a bearer access-token publish is not CSRF-gated: {body}"
    );

    let (status, body) = status_and_body(
        site.client
            .post(site.url("/registry/api/packages/acme/widget/0.1.0/yank"))
            .bearer_auth(&pat)
            .header("sec-fetch-site", "cross-site")
            .json(&serde_json::json!({ "reason": "bearer" }))
            .send()
            .await
            .expect("bearer yank"),
    )
    .await;
    assert_eq!(status, 200, "a bearer-PAT yank is not CSRF-gated: {body}");

    let versions = versions(&site).await;
    assert!(published(&versions, "0.6.0") && published(&versions, "0.7.0"));
    assert!(yanked(&versions, "0.1.0"));

    site.wafer.shutdown().await;
}
