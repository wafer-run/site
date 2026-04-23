//! `.wafer` tarball parsing + validation for the publish endpoint.
//!
//! A `.wafer` is a gzipped tar archive containing exactly:
//! - `wafer.toml` — the package manifest.
//! - `{anything}.wasm` — the WASM module (exactly one).
//! - `README.md` — optional, rendered on the detail page.
//!
//! Size caps:
//! - 20 MiB for the whole archive (rejected before gunzipping when possible).
//! - 16 MiB for the `.wasm` entry.
//! - 1 MiB for the `README.md`.
//!
//! All error variants map to 4xx HTTP statuses via [`TarballError::status_code`].
//! Internal decode errors are surfaced as 422 — a malformed archive is the
//! client's fault, not the server's.

use std::io::Read;

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::Archive;

/// Maximum allowed size of the raw (gzipped) `.wafer` archive in bytes.
const MAX_TARBALL_BYTES: usize = 20 * 1024 * 1024;
/// Maximum allowed size of the `.wasm` entry in bytes (after decompression).
const MAX_WASM_BYTES: usize = 16 * 1024 * 1024;
/// Maximum allowed size of `README.md` in bytes.
const MAX_README_BYTES: usize = 1024 * 1024;

/// Deserialized `wafer.toml`. Captures the structured fields the registry
/// stores; any extra keys are ignored by serde.
#[derive(Deserialize, Debug)]
pub struct WaferToml {
    pub package: WaferPackage,
    /// Block dependency list — stored as a JSON blob in the `dependencies`
    /// column. Parsed opaquely so the registry doesn't have to track the
    /// wafer-toml dependency grammar verbatim. `None` when the manifest
    /// omits the section; serialized as `[]` on insert.
    #[serde(default)]
    pub dependencies: Option<toml::Value>,
    /// Capability map — same shape as dependencies. Serialized as `{}` when
    /// absent.
    #[serde(default)]
    pub capabilities: Option<toml::Value>,
}

#[derive(Deserialize, Debug)]
pub struct WaferPackage {
    pub org: String,
    pub name: String,
    pub version: String,
    pub abi: u32,
    pub summary: Option<String>,
    pub license: Option<String>,
}

/// Result of a successful parse + validate. Owned — the archive is consumed
/// in the process.
#[derive(Debug)]
pub struct ExtractedTarball {
    pub wafer_toml: WaferToml,
    pub wasm_bytes: Vec<u8>,
    pub readme_md: Option<String>,
    /// Hex-encoded SHA256 of the *gzipped* tar bytes (what the client
    /// uploaded). Callers store this verbatim.
    pub sha256: String,
    /// Length of the raw archive in bytes.
    pub size_bytes: usize,
}

/// All error conditions that can surface from [`parse_and_validate`]. Every
/// variant maps to a 4xx status via [`status_code`].
#[derive(Debug)]
pub enum TarballError {
    /// Raw archive exceeded `MAX_TARBALL_BYTES`.
    TooLarge,
    /// Gzip/tar decode failure. Carries the underlying error message for
    /// debugging — shouldn't be returned to the client verbatim.
    Decode(String),
    /// No `wafer.toml` entry found.
    MissingManifest,
    /// No `.wasm` entry found.
    MissingWasm,
    /// More than one `.wasm` entry found.
    MultipleWasm,
    /// `.wasm` entry exceeded `MAX_WASM_BYTES`.
    OversizeWasm,
    /// `README.md` exceeded `MAX_README_BYTES`.
    OversizeReadme,
    /// Manifest failed structural/semantic validation. Message is
    /// diagnostic-friendly.
    BadManifest(String),
}

impl TarballError {
    /// HTTP status for surfacing this error to the publish client.
    ///
    /// Size caps → 413; every other 4xx condition is 422 (unprocessable).
    pub fn status_code(&self) -> u16 {
        match self {
            TarballError::TooLarge => 413,
            _ => 422,
        }
    }
}

/// Parse a `.wafer` (gzipped tar) blob into an [`ExtractedTarball`].
///
/// The hash is computed on the raw bytes before decompression — the stored
/// sha256 is the one the CLI can verify against the uploaded file.
pub fn parse_and_validate(bytes: &[u8]) -> Result<ExtractedTarball, TarballError> {
    if bytes.len() > MAX_TARBALL_BYTES {
        return Err(TarballError::TooLarge);
    }
    let sha256 = hex::encode(Sha256::digest(bytes));

    let mut archive = Archive::new(GzDecoder::new(bytes));
    let mut wafer_toml: Option<WaferToml> = None;
    let mut wasm_bytes: Option<Vec<u8>> = None;
    let mut readme: Option<String> = None;

    let entries = archive
        .entries()
        .map_err(|e| TarballError::Decode(e.to_string()))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| TarballError::Decode(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| TarballError::Decode(e.to_string()))?;
        let path = path.to_string_lossy().to_string();

        match path.as_str() {
            "wafer.toml" => {
                let mut s = String::new();
                entry
                    .read_to_string(&mut s)
                    .map_err(|e| TarballError::Decode(e.to_string()))?;
                wafer_toml = Some(
                    toml::from_str::<WaferToml>(&s)
                        .map_err(|e| TarballError::BadManifest(e.to_string()))?,
                );
            }
            "README.md" => {
                // Cap the read so an attacker can't stream us 10 GB of text
                // inside a small gzipped envelope.
                let mut take = entry.take(MAX_README_BYTES as u64 + 1);
                let mut s = String::new();
                take.read_to_string(&mut s)
                    .map_err(|e| TarballError::Decode(e.to_string()))?;
                if s.len() > MAX_README_BYTES {
                    return Err(TarballError::OversizeReadme);
                }
                readme = Some(s);
            }
            p if p.ends_with(".wasm") => {
                if wasm_bytes.is_some() {
                    return Err(TarballError::MultipleWasm);
                }
                // Same gzip-bomb guard as README.
                let mut take = entry.take(MAX_WASM_BYTES as u64 + 1);
                let mut buf = Vec::new();
                take.read_to_end(&mut buf)
                    .map_err(|e| TarballError::Decode(e.to_string()))?;
                if buf.len() > MAX_WASM_BYTES {
                    return Err(TarballError::OversizeWasm);
                }
                wasm_bytes = Some(buf);
            }
            _ => {
                // Silently ignore other files. Intentionally permissive so
                // CLIs can bundle extra metadata (LICENSE, CHANGELOG) without
                // a breaking change to the archive format.
            }
        }
    }

    let wafer_toml = wafer_toml.ok_or(TarballError::MissingManifest)?;
    let wasm_bytes = wasm_bytes.ok_or(TarballError::MissingWasm)?;

    validate_manifest(&wafer_toml)?;

    Ok(ExtractedTarball {
        wafer_toml,
        wasm_bytes,
        readme_md: readme,
        sha256,
        size_bytes: bytes.len(),
    })
}

/// Structural + semantic validation of the `wafer.toml` manifest.
///
/// Rules:
/// - `version` must be valid SemVer.
/// - `name` and `org` must match `[a-z0-9-]+`.
/// - `abi` must be >= 1.
/// - `license` (if present) must not be empty.
fn validate_manifest(m: &WaferToml) -> Result<(), TarballError> {
    semver::Version::parse(&m.package.version)
        .map_err(|e| TarballError::BadManifest(format!("version: {e}")))?;
    if m.package.name.is_empty()
        || !m
            .package
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(TarballError::BadManifest(
            "name must match [a-z0-9-]+".into(),
        ));
    }
    if m.package.org.is_empty()
        || !m
            .package
            .org
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(TarballError::BadManifest(
            "org must match [a-z0-9-]+".into(),
        ));
    }
    if m.package.abi == 0 {
        return Err(TarballError::BadManifest("abi must be >= 1".into()));
    }
    if let Some(lic) = &m.package.license {
        if lic.is_empty() {
            return Err(TarballError::BadManifest("license empty".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Cursor;

        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(content.len() as u64);
                header.set_cksum();
                tar.append(&header, Cursor::new(*content)).unwrap();
            }
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    const VALID_TOML: &str = r#"
[package]
org = "acme"
name = "widget"
version = "0.1.0"
abi = 1
license = "MIT"
"#;

    #[test]
    fn happy_path() {
        let tarball = make_tarball(&[
            ("wafer.toml", VALID_TOML.as_bytes()),
            ("widget.wasm", b"\0asm\x01\x00\x00\x00"),
        ]);
        let extracted = parse_and_validate(&tarball).expect("happy path");
        assert_eq!(extracted.wafer_toml.package.name, "widget");
        assert_eq!(extracted.wafer_toml.package.org, "acme");
        assert_eq!(extracted.wafer_toml.package.version, "0.1.0");
        assert_eq!(extracted.wasm_bytes, b"\0asm\x01\x00\x00\x00");
        assert_eq!(extracted.sha256.len(), 64);
    }

    #[test]
    fn readme_is_captured() {
        let tarball = make_tarball(&[
            ("wafer.toml", VALID_TOML.as_bytes()),
            ("w.wasm", b"\0asm"),
            ("README.md", b"# hello"),
        ]);
        let e = parse_and_validate(&tarball).unwrap();
        assert_eq!(e.readme_md.as_deref(), Some("# hello"));
    }

    #[test]
    fn missing_manifest_errors() {
        let tarball = make_tarball(&[("widget.wasm", b"\0asm")]);
        assert!(matches!(
            parse_and_validate(&tarball).unwrap_err(),
            TarballError::MissingManifest
        ));
    }

    #[test]
    fn missing_wasm_errors() {
        let tarball = make_tarball(&[("wafer.toml", VALID_TOML.as_bytes())]);
        assert!(matches!(
            parse_and_validate(&tarball).unwrap_err(),
            TarballError::MissingWasm
        ));
    }

    #[test]
    fn multiple_wasm_errors() {
        let tarball = make_tarball(&[
            ("wafer.toml", VALID_TOML.as_bytes()),
            ("a.wasm", b"\0asm"),
            ("b.wasm", b"\0asm"),
        ]);
        assert!(matches!(
            parse_and_validate(&tarball).unwrap_err(),
            TarballError::MultipleWasm
        ));
    }

    #[test]
    fn bad_version_is_422() {
        let bad = r#"
[package]
org = "acme"
name = "widget"
version = "not-a-semver"
abi = 1
"#;
        let tarball = make_tarball(&[
            ("wafer.toml", bad.as_bytes()),
            ("w.wasm", b"\0asm"),
        ]);
        let err = parse_and_validate(&tarball).unwrap_err();
        assert_eq!(err.status_code(), 422);
        assert!(matches!(err, TarballError::BadManifest(_)));
    }

    #[test]
    fn bad_name_is_422() {
        let bad = r#"
[package]
org = "acme"
name = "Widget"
version = "0.1.0"
abi = 1
"#;
        let tarball = make_tarball(&[
            ("wafer.toml", bad.as_bytes()),
            ("w.wasm", b"\0asm"),
        ]);
        let err = parse_and_validate(&tarball).unwrap_err();
        assert_eq!(err.status_code(), 422);
    }

    #[test]
    fn zero_abi_rejected() {
        let bad = r#"
[package]
org = "acme"
name = "widget"
version = "0.1.0"
abi = 0
"#;
        let tarball = make_tarball(&[
            ("wafer.toml", bad.as_bytes()),
            ("w.wasm", b"\0asm"),
        ]);
        assert!(matches!(
            parse_and_validate(&tarball).unwrap_err(),
            TarballError::BadManifest(_)
        ));
    }

    #[test]
    fn too_large_is_413() {
        let big = vec![0u8; MAX_TARBALL_BYTES + 1];
        let err = parse_and_validate(&big).unwrap_err();
        assert_eq!(err.status_code(), 413);
        assert!(matches!(err, TarballError::TooLarge));
    }
}
