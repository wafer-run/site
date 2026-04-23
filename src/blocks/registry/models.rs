//! Data Transfer Objects (DTOs) for JSON responses.
//!
//! Populated by Task 8's query helpers in `db.rs` and consumed by the HTTP
//! route handlers from Task 9 onward. Shapes mirror the registry's public API
//! contract.

use serde::{Deserialize, Serialize};

/// Single-row summary used by the browse page and `/api/packages` listing.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PackageSummary {
    pub org: String,
    pub name: String,
    pub summary: Option<String>,
    pub latest: Option<String>,
}

/// Package detail including every published version.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PackageDetail {
    pub org: String,
    pub name: String,
    pub summary: Option<String>,
    pub versions: Vec<VersionSummary>,
}

/// One row of a package's version history.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VersionSummary {
    pub version: String,
    pub abi: i64,
    pub sha256: String,
    pub size_bytes: i64,
    pub license: Option<String>,
    pub yanked: i64,
    pub published_at: i64,
}

/// Full version detail used by `/api/packages/{org}/{name}/{version}` and the
/// download endpoint.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VersionDetail {
    pub org_name: String,
    pub pkg_name: String,
    pub version: String,
    pub abi: i64,
    pub sha256: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub license: Option<String>,
    pub readme_md: Option<String>,
    pub dependencies: Option<String>,
    pub capabilities: Option<String>,
    pub yanked: i64,
    pub yanked_reason: Option<String>,
    pub published_at: i64,
}
