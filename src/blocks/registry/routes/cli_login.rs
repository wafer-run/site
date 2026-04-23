//! CLI login endpoints: GET /registry/cli-login and POST /registry/api/cli-login/exchange
//!
//! Task 12 wires the device-code flow used by the `wafer` CLI to acquire a
//! personal access token:
//!
//! 1. Admin opens `/registry/cli-login` in a browser. The page is gated by
//!    [`auth::require_admin`] — non-admins see the Task-11 coming-soon
//!    page. On success, [`db::issue_cli_code`] mints a 64-char hex code and
//!    [`templates::cli_login_code`] renders it for copy-paste.
//!
//! 2. The CLI prompts the user for the code, then POSTs it to
//!    `/registry/api/cli-login/exchange`. That endpoint is *unauthenticated*
//!    — the code itself is the credential. [`db::exchange_cli_code`] marks
//!    the code consumed and mints a long-lived `wafer_pat_*` token. We
//!    return the raw PAT once, hashing it before storage so a DB dump never
//!    yields usable tokens.

use serde::Deserialize;
use serde_json::json;
use wafer_run::{common::ServiceOp, Context, InputStream, Message, OutputStream};

use crate::blocks::registry::{auth, db, routes::resp, templates, RegistryConfig};

/// GET `/registry/cli-login` — render a fresh device code for the admin.
///
/// Gate: admin-only via [`auth::require_admin`]. Non-admins receive whatever
/// response `require_admin` generated (HTML coming-soon when they sent
/// `Accept: text/html`, JSON coming-soon otherwise — 403 in both cases).
pub async fn page(ctx: &dyn Context, msg: &Message, cfg: &RegistryConfig) -> OutputStream {
    let admin = match auth::require_admin(ctx, msg, cfg).await {
        Ok(u) => u,
        Err(out) => return out,
    };
    match db::issue_cli_code(ctx, &admin.id).await {
        Ok(code) => resp::ok_html(&templates::cli_login_code(&code).into_string()),
        Err(e) => resp::internal(&format!("issue code: {e}")),
    }
}

#[derive(Deserialize)]
struct ExchangeRequest {
    code: String,
}

#[derive(Deserialize, Default)]
struct UserProfileResponse {
    #[serde(default)]
    email: String,
}

/// POST `/registry/api/cli-login/exchange` — swap a device code for a PAT.
///
/// Intentionally unauthenticated: the presented code *is* the credential
/// and is single-use + time-limited (`db::exchange_cli_code` enforces
/// both).
///
/// Response shape:
/// ```json
/// { "token": "wafer_pat_<64hex>", "user": { "email": "admin@example.com" } }
/// ```
/// Email is best-effort — it comes from `suppers-ai/auth` via
/// `AUTH_USER_PROFILE`. In harnesses where the auth block isn't registered,
/// the field is `""` rather than an error, since the CLI's PAT is already
/// valid regardless.
///
/// Error shapes:
/// - 400 `bad-request` — non-JSON body or missing `code` field.
/// - 404 `invalid-code` — code missing, expired, or already consumed.
/// - 500 — backend error on code exchange.
pub async fn exchange(
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
    _cfg: &RegistryConfig,
) -> OutputStream {
    let body_bytes = input.collect_to_bytes().await;
    let parsed: ExchangeRequest = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return resp::bad_request("body must be JSON with a `code` field"),
    };
    if parsed.code.is_empty() {
        return resp::bad_request("`code` must not be empty");
    }

    match db::exchange_cli_code(ctx, &parsed.code).await {
        Ok(Some((user_id, token))) => {
            let email = fetch_email(ctx, &user_id).await;
            resp::ok_json(&json!({
                "token": token,
                "user": { "email": email },
            }))
        }
        Ok(None) => resp::json_response(
            404,
            &json!({
                "error": "invalid-code",
                "message": "Code is invalid, expired, or already used."
            }),
        ),
        Err(e) => resp::internal(&format!("exchange: {e}")),
    }
}

/// Best-effort email lookup — mirrors `auth.rs::fetch_email` but lives here
/// to avoid widening that module's public surface. Returns `""` on any
/// failure (auth block not registered, user deleted, etc.).
async fn fetch_email(ctx: &dyn Context, user_id: &str) -> String {
    let prof_msg = Message::new(ServiceOp::AUTH_USER_PROFILE);
    let Ok(body) = serde_json::to_vec(&json!({ "user_id": user_id })) else {
        return String::new();
    };
    match ctx
        .call_block_buffered("suppers-ai/auth", prof_msg, &body)
        .await
    {
        Ok(buf) => serde_json::from_slice::<UserProfileResponse>(&buf.body)
            .map(|p| p.email)
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}
