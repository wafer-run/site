//! Admin-gate middleware for the registry block.
//!
//! Two public entry points:
//!
//! - [`require_user`] — resolve the caller to an `AuthedUser` by checking a
//!   Bearer PAT against `registry_tokens` first, then falling back to
//!   impresspress's verifier for its session token (Bearer header, or the
//!   `auth_token` cookie when the request carries no `Authorization`
//!   header).
//! - [`require_admin`] — wraps `require_user` and additionally gates on the
//!   configured admin email ([`config::ADMIN_EMAIL_KEY`]). Non-admins get
//!   the "coming-soon" response.
//!
//! No raw SQL is used: the PAT lookup goes through
//! `wafer_core::clients::database::get_by_field`.

use impresspress_core::{
    blocks::auth::{
        repo::{sessions, users},
        JWT_SECRET_KEY,
    },
    crypto::{expected_issuer, verify_access_token},
};
use wafer_block::Message;
use wafer_run::{context::Context, OutputStream, ResourceGrant};

use crate::blocks::registry::{config, db, routes::resp, templates};

/// The authenticated caller, as resolved by [`require_user`].
///
/// `email` is read from the token's `email` claim (JWT) or the
/// `registry_tokens.email` column (PAT). It may be an empty string when the
/// token was minted before we started capturing email — callers like
/// [`require_admin`] that compare on email must treat an empty string as
/// "not the admin".
#[derive(Clone, Debug)]
pub struct AuthedUser {
    /// Opaque user id — matches `UserId(String)` in `wafer-core::interfaces::auth`.
    pub id: String,
    /// User's email address. Empty string when the token didn't carry one.
    pub email: String,
    /// How the user authenticated — `"password"`, `"oauth.github"`, etc.
    /// `"pat"` for registry-issued personal-access tokens.
    /// Empty string when the JWT was minted before this claim existed
    /// (treat as "unknown / not OAuth" by gates that require a method).
    pub auth_method: String,
}

/// Resolve the caller to an [`AuthedUser`].
///
/// Resolution order:
///
/// 1. Bearer PAT in the `Authorization` header — looked up in
///    `registry_tokens` by `sha256(raw_token)` hex. Revoked tokens (those
///    with a `revoked_at` set) are skipped and we fall through to step 2.
///    This path exists because PATs are minted by
///    `POST /registry/api/cli-login/exchange` and live only in the
///    registry's own store — impresspress's auth doesn't know about them.
/// 2. An impresspress session token — the `Authorization: Bearer` header or
///    the `auth_token` cookie impresspress's login sets — checked by
///    [`verify_access_token`], impresspress's one token verifier, under the
///    deployment's own policy: the secret from the config snapshot and the
///    issuer [`expected_issuer`] resolves. `impresspress/router` verifies the
///    same token before dispatching here, but stamps no `auth_method`, which
///    [`require_admin`] gates on, so the registry runs the check itself; the
///    reads it makes are covered by [`auth_table_grants`].
///
/// The CSRF origin policy that pipeline applies exempts a request carrying
/// an `Authorization` header and judges one without it by its cookie, so
/// the cookie is consulted here only when that header is absent — a
/// credential the policy exempted is never one it did not judge.
///
/// Returns an `OutputStream` error response on any failure path so callers
/// can early-return without additional shaping. A check that could not be
/// completed (a failed read of the token's session row or of the account's
/// `auth_version`) answers with its error rather than as signed out.
pub async fn require_user(ctx: &dyn Context, msg: &Message) -> Result<AuthedUser, OutputStream> {
    // 1. Try bearer PAT against registry_tokens. This path handles PATs the
    //    registry itself issued via CLI-login exchange. The PAT inherits the
    //    auth method of the session that minted it — for now we tag it
    //    `"pat"` so a future tightening can require, say, "PATs only issued
    //    from OAuth sessions" without re-plumbing.
    let auth_header = msg.header("authorization");
    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if let Ok(Some((user_id, email))) = db::resolve_bearer(ctx, token).await {
            return Ok(AuthedUser {
                id: user_id,
                email,
                auth_method: "pat".to_string(),
            });
        }
        // Fall through to session-token verification — a PAT that is not
        // ours might be an impresspress access token.
    }

    // 2. An impresspress session token.
    let Some(token) = find_jwt_token(msg) else {
        return Err(unauthorized_response());
    };
    let secret = ctx.config_get(JWT_SECRET_KEY).unwrap_or("").to_string();
    let issuer = expected_issuer(ctx).await.map_err(OutputStream::error)?;
    let claims = verify_access_token(ctx, &token, &secret, &issuer)
        .await
        .map_err(OutputStream::error)?;
    match claims {
        Some(claims) => Ok(AuthedUser {
            id: claims.sub,
            email: claims.email.unwrap_or_default(),
            auth_method: claims.auth_method,
        }),
        None => Err(unauthorized_response()),
    }
}

/// The reads [`verify_access_token`] makes, granted to the registry block:
/// the sessions table (the token's login family must still have a live
/// session row; signing out deletes it) and the users table (a token minted
/// before the user's `auth_version` moved). The same two reads the
/// `impresspress/router` block is granted for the same check.
pub fn auth_table_grants() -> Vec<ResourceGrant> {
    vec![
        ResourceGrant::read(super::NAME, sessions::TABLE),
        ResourceGrant::read(super::NAME, users::TABLE),
    ]
}

/// Find a session token in the request — the Authorization Bearer header,
/// or, when the request has no `Authorization` header at all, the
/// `auth_token` cookie impresspress's login sets. The same source rule
/// `impresspress/router` applies when it decides whether a request is
/// cookie-authenticated for the CSRF origin policy.
fn find_jwt_token(msg: &Message) -> Option<String> {
    let auth_header = msg.header("authorization");
    if !auth_header.is_empty() {
        return auth_header
            .strip_prefix("Bearer ")
            .filter(|t| !t.is_empty())
            .map(str::to_string);
    }
    let cookie = msg.cookie("auth_token");
    (!cookie.is_empty()).then(|| cookie.to_string())
}

/// Gate on the configured admin email — and, when configured, the auth
/// method the user signed in with.
///
/// On a hit, returns the authenticated user. On a miss, returns a 403:
/// - `coming-soon` template/JSON when the caller's email isn't the admin
///   email (current default for non-admins).
/// - `auth-method-required` JSON / HTML when the email matches but the
///   auth method doesn't satisfy [`config::REQUIRED_AUTH_METHOD_KEY`].
///   This separates "you're not allowed" from "log in via the right
///   method" so the CLI / admin can react correctly.
///
/// Both keys are read per request through the config service, so an edit on
/// the admin Variables page applies to the next request. A read that fails
/// answers with its error rather than as "not admin".
pub async fn require_admin(ctx: &dyn Context, msg: &Message) -> Result<AuthedUser, OutputStream> {
    let user = require_user(ctx, msg).await?;
    let admin_email = config::admin_email(ctx)
        .await
        .map_err(OutputStream::error)?;
    if !is_admin_email(&user.email, &admin_email) {
        return Err(coming_soon_response(msg));
    }
    let required_auth_method = config::required_auth_method(ctx)
        .await
        .map_err(OutputStream::error)?;
    if !required_auth_method.is_empty()
        && !user.auth_method.eq_ignore_ascii_case(&required_auth_method)
    {
        return Err(auth_method_required_response(msg, &required_auth_method));
    }
    Ok(user)
}

/// Whether `email` is the configured registry admin's, `admin_email`. An
/// empty address on either side is never the admin: an unset
/// [`config::ADMIN_EMAIL_KEY`] closes publishing, and a token without an
/// email claim names nobody.
pub fn is_admin_email(email: &str, admin_email: &str) -> bool {
    !email.is_empty() && email.eq_ignore_ascii_case(admin_email)
}

fn coming_soon_response(msg: &Message) -> OutputStream {
    let accept = msg.header("accept");
    if accept.contains("text/html") {
        resp::html_response(403, &templates::coming_soon().into_string())
    } else {
        resp::json_response(
            403,
            &serde_json::json!({
                "error": "coming-soon",
                "message": "Publishing is not yet open to other users."
            }),
        )
    }
}

fn auth_method_required_response(msg: &Message, required: &str) -> OutputStream {
    let message =
        format!("Admin actions require signing in via {required}. Re-authenticate and retry.");
    let accept = msg.header("accept");
    if accept.contains("text/html") {
        resp::html_response(
            403,
            &format!("<!doctype html><meta charset=utf-8><title>Auth method required</title><p>{message}</p>"),
        )
    } else {
        resp::json_response(
            403,
            &serde_json::json!({
                "error": "auth-method-required",
                "message": message,
                "required_auth_method": required,
            }),
        )
    }
}

fn unauthorized_response() -> OutputStream {
    resp::json_response(
        401,
        &serde_json::json!({
            "error": "unauthorized",
            "message": "Login required"
        }),
    )
}
