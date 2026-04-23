//! Publish endpoint: `POST /registry/api/publish`.
//!
//! Flow (admin-gated in Step 2):
//!
//! 1. `auth::require_admin` — returns 401 if unauthenticated, 403
//!    "coming-soon" for non-admins.
//! 2. Parse the `multipart/form-data` body; extract the part named
//!    `tarball`. Enforced size cap: [`MAX_PUBLISH_BODY_BYTES`].
//! 3. Hand the bytes to [`tarball::parse_and_validate`] for sha256 +
//!    structural checks. Size / manifest failures surface as 4xx JSON.
//! 4. Dedupe against [`db::version_exists`] — duplicate `(org, name,
//!    version)` returns 409.
//! 5. [`db::upsert_org`] the declared org. Reserved orgs are writable by
//!    admin (we already passed the admin gate).
//! 6. Write the archive to the `registry` storage folder under
//!    `{prefix}/{org}/{name}/{version}.wafer`.
//! 7. Insert the version row. On failure, best-effort compensating delete
//!    against storage so we don't leak orphaned blobs.
//!
//! The multipart parser is hand-rolled to avoid dragging a ~200KLOC crate
//! in for the single "find the part named tarball" case. Spec-accurate
//! enough for `reqwest::multipart::Form` bodies, which is what both the CLI
//! and our own tests generate.

use serde_json::json;
use wafer_run::{Context, InputStream, Message, OutputStream};

use crate::blocks::registry::{auth, db, routes::resp, tarball, RegistryConfig};

/// Cap on total body size accepted by `POST /registry/api/publish`. Matches
/// the 20 MiB archive cap — the multipart envelope adds a few hundred bytes
/// of boundaries on top, so the effective check inside
/// [`tarball::parse_and_validate`] still fires for oversize archives. We
/// short-circuit here too so a malicious client can't stream gigabytes
/// before we reject.
const MAX_PUBLISH_BODY_BYTES: usize = 22 * 1024 * 1024;

/// `POST /registry/api/publish` — accept a `.wafer` tarball and register it.
pub async fn post(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    cfg: &RegistryConfig,
) -> OutputStream {
    // 1. Admin gate.
    let admin = match auth::require_admin(ctx, msg, cfg).await {
        Ok(u) => u,
        Err(out) => return out,
    };

    // 2. Multipart parse.
    let bytes = match read_multipart_tarball(msg, input).await {
        Ok(b) => b,
        Err(status) => return multipart_error(status),
    };

    // 3. Tarball parse + validate.
    //
    // The wire body uses `Display`, not `Debug`, so 422s read as
    // "version: empty pre-release segment" rather than
    // `BadManifest("version: ...")` — the Rust variant name isn't part of
    // the public contract.
    let t = match tarball::parse_and_validate(&bytes) {
        Ok(t) => t,
        Err(e) => {
            return resp::json_response(
                e.status_code(),
                &json!({
                    "error": "invalid-tarball",
                    "message": format!("{e}"),
                }),
            );
        }
    };

    // 4. Duplicate version guard.
    match db::version_exists(
        ctx,
        &t.wafer_toml.package.org,
        &t.wafer_toml.package.name,
        &t.wafer_toml.package.version,
    )
    .await
    {
        Ok(true) => {
            return resp::json_response(
                409,
                &json!({
                    "error": "version-exists",
                    "message": format!(
                        "{}/{}@{} already exists",
                        t.wafer_toml.package.org,
                        t.wafer_toml.package.name,
                        t.wafer_toml.package.version
                    ),
                }),
            );
        }
        Ok(false) => {}
        Err(e) => return resp::internal(&format!("version_exists: {e}")),
    }

    // 5. Upsert org (reserved flag preserved on existing rows).
    let reserved = db::is_reserved(ctx, &t.wafer_toml.package.org)
        .await
        .unwrap_or(false);
    let org_id = match db::upsert_org(ctx, &t.wafer_toml.package.org, &admin.id, reserved).await {
        Ok(id) => id,
        Err(e) => return resp::internal(&format!("upsert_org: {e}")),
    };

    // 6. Storage write. Folder is the configured prefix ("registry" by
    //    default). We don't proactively `create_folder` here — the storage
    //    block's `put` is responsible for auto-creating the folder in
    //    backends that need it. The local-storage backend used in dev +
    //    tests creates directories on demand.
    let storage_key = format!(
        "{}/{}/{}.wafer",
        t.wafer_toml.package.org, t.wafer_toml.package.name, t.wafer_toml.package.version
    );
    let folder = &cfg.storage_key_prefix;

    if let Err(e) = wafer_core::clients::storage::put(
        ctx,
        folder,
        &storage_key,
        &bytes,
        "application/octet-stream",
    )
    .await
    {
        return resp::internal(&format!("storage put: {e:?}"));
    }

    // 7. DB insert. On failure, best-effort compensating delete on storage.
    if let Err(e) = db::insert_version(
        ctx,
        &org_id,
        &t.wafer_toml.package.name,
        &admin.id,
        &t,
        &storage_key,
    )
    .await
    {
        let _ = wafer_core::clients::storage::delete(ctx, folder, &storage_key).await;
        return resp::internal(&format!("db insert: {e}"));
    }

    resp::ok_json(&json!({
        "package": format!("{}/{}", t.wafer_toml.package.org, t.wafer_toml.package.name),
        "version": t.wafer_toml.package.version,
        "download_url": format!(
            "/registry/download/{}/{}/{}.wafer",
            t.wafer_toml.package.org, t.wafer_toml.package.name, t.wafer_toml.package.version
        ),
        "sha256": t.sha256,
    }))
}

/// JSON error envelope for multipart-parse failures. Status codes:
/// - 400 — malformed body / missing boundary / missing `tarball` part.
/// - 413 — body exceeded [`MAX_PUBLISH_BODY_BYTES`] (pre-check).
fn multipart_error(status: u16) -> OutputStream {
    resp::json_response(
        status,
        &json!({
            "error": match status {
                413 => "body-too-large",
                _ => "invalid-multipart",
            },
            "message": "Expected multipart/form-data with a `tarball` part.",
        }),
    )
}

/// Extract the `tarball` part's raw bytes from a `multipart/form-data`
/// body. Returns the part's bytes, or an HTTP status code to surface back.
///
/// This is a minimal RFC 7578 parser — sufficient for bodies produced by
/// `reqwest::multipart::Form` (which we ship in the CLI and use in tests).
/// Part ordering is flexible; only the part whose `Content-Disposition`
/// says `name="tarball"` is returned. Preamble and epilogue are ignored.
async fn read_multipart_tarball(msg: &Message, input: InputStream) -> Result<Vec<u8>, u16> {
    let content_type = msg.header("content-type");
    if !content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return Err(400);
    }
    let boundary = extract_boundary(content_type).ok_or(400u16)?;

    let body = input.collect_to_bytes().await;
    if body.len() > MAX_PUBLISH_BODY_BYTES {
        return Err(413);
    }

    let dash_boundary = format!("--{boundary}");
    let parts = split_on(&body, dash_boundary.as_bytes());
    for part in parts {
        // Each part starts with CRLF, then header block, CRLF CRLF, then
        // body, then CRLF before the next boundary. Skip empty preamble /
        // epilogue slices.
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let Some(hdr_end) = find_bytes(part, b"\r\n\r\n") else {
            continue;
        };
        let headers = match std::str::from_utf8(&part[..hdr_end]) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !header_has_name_tarball(headers) {
            continue;
        }
        let body = &part[hdr_end + 4..];
        // Strip the trailing CRLF that precedes the next boundary marker.
        let body = body.strip_suffix(b"\r\n").unwrap_or(body);
        return Ok(body.to_vec());
    }
    Err(400)
}

/// Pull the `boundary=...` token out of a `Content-Type` header. Strips a
/// surrounding pair of double quotes when present.
fn extract_boundary(content_type: &str) -> Option<String> {
    // RFC 2045 allows boundary anywhere after the type/subtype. Scan
    // parameters split by `;`.
    for part in content_type.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("boundary=") {
            return Some(v.trim_matches('"').to_string());
        }
    }
    None
}

/// Case-insensitive check for `name="tarball"` in a part's header block.
fn header_has_name_tarball(headers: &str) -> bool {
    let lower = headers.to_ascii_lowercase();
    lower.contains("name=\"tarball\"")
}

/// Split `hay` into slices delimited by every occurrence of `needle`. The
/// needle itself is not included in the output. Adjacent needles yield
/// empty slices — the caller filters those out.
fn split_on<'a>(hay: &'a [u8], needle: &[u8]) -> Vec<&'a [u8]> {
    if needle.is_empty() {
        return vec![hay];
    }
    let mut out = Vec::new();
    let mut last = 0usize;
    let mut i = 0usize;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == *needle {
            out.push(&hay[last..i]);
            i += needle.len();
            last = i;
        } else {
            i += 1;
        }
    }
    out.push(&hay[last..]);
    out
}

/// Find the first occurrence of `needle` in `hay`. Returns the start index.
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::{extract_boundary, header_has_name_tarball};

    #[test]
    fn boundary_extracted() {
        assert_eq!(
            extract_boundary("multipart/form-data; boundary=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            extract_boundary("multipart/form-data; boundary=\"q-xyz\"").as_deref(),
            Some("q-xyz")
        );
    }

    #[test]
    fn header_match_is_case_insensitive() {
        assert!(header_has_name_tarball(
            "Content-Disposition: form-data; name=\"tarball\"\r\nContent-Type: app/octet"
        ));
        assert!(!header_has_name_tarball(
            "Content-Disposition: form-data; name=\"other\""
        ));
    }
}
