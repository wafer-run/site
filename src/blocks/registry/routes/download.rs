//! Download endpoint: `GET /registry/download/{org}/{name}/{version}.wafer`.
//!
//! Public (no auth). Fetches the version row, reads the tarball from the
//! `registry` storage folder, and returns the raw bytes with
//! `content-type: application/octet-stream` and an immutable long-max-age
//! cache header — content is addressed by `{org}/{name}/{version}` + sha256,
//! so it never changes in place.
//!
//! Yanked versions still serve (spec §8): yank only filters "latest"
//! resolution, not direct-version downloads. Callers that request an
//! explicit `{version}.wafer` URL are assumed to know what they're pinning.

use wafer_run::{Context, Message, OutputStream};

use crate::blocks::registry::{db, routes::resp, RegistryConfig};

/// `GET /registry/download/{org}/{name}/{version}.wafer` — stream the
/// uploaded tarball back to the caller.
pub async fn get(ctx: &dyn Context, msg: &Message, cfg: &RegistryConfig) -> OutputStream {
    let Some(tail) = msg.path().strip_prefix("/registry/download/") else {
        return resp::bad_request("Expected /registry/download/{org}/{name}/{version}.wafer");
    };
    let Some((org, name, version)) = parse_download_path(tail) else {
        return resp::bad_request("Expected /registry/download/{org}/{name}/{version}.wafer");
    };

    // Look up the version row to get the storage_key. 404 if the org,
    // package, or version is missing. Yanked rows still resolve — we don't
    // check the flag here.
    let detail = match db::get_version(ctx, &org, &name, &version).await {
        Ok(Some(v)) => v,
        Ok(None) => return resp::not_found(&format!("{org}/{name}@{version} not found")),
        Err(e) => return resp::internal(&format!("get_version: {e}")),
    };

    // Read the tarball bytes from the configured folder. Storage read
    // failure maps to 404 — if the DB row exists but the blob doesn't, the
    // version is effectively unavailable. (That shouldn't happen in a
    // healthy deploy; the publish endpoint has a compensating delete.)
    match wafer_core::clients::storage::get(ctx, &cfg.storage_key_prefix, &detail.storage_key).await
    {
        Ok((bytes, _info)) => resp::binary_response(
            200,
            bytes,
            &[
                ("content-type", "application/octet-stream"),
                ("cache-control", "public, max-age=31536000, immutable"),
            ],
        ),
        Err(_) => resp::not_found(&format!("{org}/{name}@{version} blob missing")),
    }
}

/// Split the path tail (`{org}/{name}/{version}.wafer`) into its three
/// segments. Returns `None` on any structural mismatch — empty segments,
/// missing `.wafer` suffix, or wrong arity.
fn parse_download_path(tail: &str) -> Option<(String, String, String)> {
    let segs: Vec<&str> = tail.split('/').collect();
    if segs.len() != 3 {
        return None;
    }
    let version = segs[2].strip_suffix(".wafer")?;
    if segs[0].is_empty() || segs[1].is_empty() || version.is_empty() {
        return None;
    }
    Some((
        segs[0].to_string(),
        segs[1].to_string(),
        version.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_download_path;

    #[test]
    fn happy_path_parses() {
        assert_eq!(
            parse_download_path("acme/widget/0.1.0.wafer"),
            Some(("acme".into(), "widget".into(), "0.1.0".into()))
        );
    }

    #[test]
    fn missing_extension_rejected() {
        assert!(parse_download_path("acme/widget/0.1.0").is_none());
    }

    #[test]
    fn wrong_arity_rejected() {
        assert!(parse_download_path("acme/0.1.0.wafer").is_none());
        assert!(parse_download_path("a/b/c/d.wafer").is_none());
    }

    #[test]
    fn empty_segment_rejected() {
        assert!(parse_download_path("acme//0.1.0.wafer").is_none());
        assert!(parse_download_path("acme/widget/.wafer").is_none());
    }
}
