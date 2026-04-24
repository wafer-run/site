//! Yank/unyank endpoints: `POST .../yank` and `POST .../unyank`.
//!
//! Both are admin-gated. The dispatcher in `handlers.rs` matches by path
//! suffix (`/yank`, `/unyank`) on the `create` action; this module owns path
//! parsing, auth, and the DB flip.
//!
//! Semantics per spec §8: yanking hides a version from "latest" resolution
//! (and from ABI-compatible search) but keeps it downloadable by explicit
//! version — the Task 14 download handler doesn't consult the flag.
//! Unyanking restores it to `latest` resolution.
//!
//! Idempotency: writing `yanked = true` over an already-yanked row succeeds,
//! because `db::update` doesn't compare the old value. That matches the plan
//! ("returns 200 without state change"). No explicit get-first needed.

use serde_json::json;
use wafer_run::{Context, InputStream, Message, OutputStream};

use crate::blocks::registry::{auth, db, routes::resp, RegistryConfig};

/// `POST /registry/api/packages/{org}/{name}/{version}/yank` — mark a
/// version yanked. Body: `{"reason": "<optional>"}`.
pub async fn yank(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    cfg: &RegistryConfig,
) -> OutputStream {
    if let Err(out) = auth::require_admin(ctx, msg, cfg).await {
        return out;
    }
    let Some((org, name, version)) = parse_path(msg.path(), "/yank") else {
        return resp::bad_request("Expected /registry/api/packages/{org}/{name}/{version}/yank");
    };

    // Best-effort reason extraction — missing body, invalid JSON, or no
    // `reason` field all collapse to `None`. We don't require a body.
    let body = input.collect_to_bytes().await;
    let reason = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
    };

    match db::set_yanked(ctx, &org, &name, &version, true, reason.as_deref()).await {
        Ok(true) => resp::ok_json(&json!({ "yanked": true })),
        Ok(false) => resp::not_found(&format!("{org}/{name}@{version} not found")),
        Err(e) => resp::internal(&format!("set_yanked: {e}")),
    }
}

/// `POST /registry/api/packages/{org}/{name}/{version}/unyank` — clear the
/// yanked flag. No body required.
pub async fn unyank(
    ctx: &dyn Context,
    msg: &Message,
    _input: InputStream,
    cfg: &RegistryConfig,
) -> OutputStream {
    if let Err(out) = auth::require_admin(ctx, msg, cfg).await {
        return out;
    }
    let Some((org, name, version)) = parse_path(msg.path(), "/unyank") else {
        return resp::bad_request("Expected /registry/api/packages/{org}/{name}/{version}/unyank");
    };
    match db::set_yanked(ctx, &org, &name, &version, false, None).await {
        Ok(true) => resp::ok_json(&json!({ "yanked": false })),
        Ok(false) => resp::not_found(&format!("{org}/{name}@{version} not found")),
        Err(e) => resp::internal(&format!("set_yanked: {e}")),
    }
}

/// Extract `(org, name, version)` from a path like
/// `/registry/api/packages/{org}/{name}/{version}/{suffix}` where `suffix`
/// is literally `/yank` or `/unyank` (leading slash included). Returns
/// `None` on any structural mismatch — the caller surfaces that as 400.
fn parse_path(path: &str, suffix: &str) -> Option<(String, String, String)> {
    let tail = path
        .strip_prefix("/registry/api/packages/")?
        .strip_suffix(suffix)?;
    let segs: Vec<&str> = tail.split('/').collect();
    if segs.len() == 3 && !segs[0].is_empty() && !segs[1].is_empty() && !segs[2].is_empty() {
        Some((
            segs[0].to_string(),
            segs[1].to_string(),
            segs[2].to_string(),
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::parse_path;

    #[test]
    fn parses_yank_path() {
        assert_eq!(
            parse_path("/registry/api/packages/acme/widget/0.1.0/yank", "/yank"),
            Some(("acme".into(), "widget".into(), "0.1.0".into()))
        );
    }

    #[test]
    fn parses_unyank_path() {
        assert_eq!(
            parse_path("/registry/api/packages/acme/widget/0.1.0/unyank", "/unyank"),
            Some(("acme".into(), "widget".into(), "0.1.0".into()))
        );
    }

    #[test]
    fn rejects_extra_segments() {
        assert_eq!(
            parse_path(
                "/registry/api/packages/acme/widget/0.1.0/extra/yank",
                "/yank"
            ),
            None
        );
    }

    #[test]
    fn rejects_missing_segment() {
        assert_eq!(
            parse_path("/registry/api/packages/acme/widget//yank", "/yank"),
            None
        );
    }
}
