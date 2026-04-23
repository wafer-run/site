//! Typed helpers for the registry block's collections.
//!
//! No raw SQL — all access goes through `wafer_core::clients::database`.
//! Task 7 ships constants + the reserved-orgs seed. Tasks 8, 12, 13, 14
//! extend this module with query helpers as routes come online.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::json;
use wafer_core::clients::database as db;
use wafer_run::context::Context;

// ---- Collection names ------------------------------------------------------

// Collection names follow the WRAP `{org}__{block}__{name}` namespace
// convention (`wafer-run/registry` → prefix `wafer_run__registry__`). Without
// the prefix, WRAP rejects every `call_block("wafer-run/database", ...)`
// invocation from this block with
// `PermissionDenied: unnamespaced resource … denied`.
pub const ORGS: &str = "wafer_run__registry__orgs";
pub const PACKAGES: &str = "wafer_run__registry__packages";
pub const VERSIONS: &str = "wafer_run__registry__versions";
pub const CODES: &str = "wafer_run__registry__cli_login_codes";
pub const TOKENS: &str = "wafer_run__registry__tokens";

/// Reserved org names seeded into `registry_orgs` on block init. Owned by the
/// project: external users cannot claim these names. Kept in alphabetical
/// order to match the test assertion.
pub const RESERVED_ORGS: &[&str] = &["solobase", "suppers-ai", "wafer", "wafer-run"];

// ---- Init / seed -----------------------------------------------------------

/// Seed reserved-orgs rows on block init. Idempotent — probes each org with
/// `get_by_field` before inserting, so reboot doesn't duplicate rows.
pub async fn seed_reserved_orgs(ctx: &dyn Context) -> Result<()> {
    for &name in RESERVED_ORGS {
        match db::get_by_field(ctx, ORGS, "name", json!(name)).await {
            Ok(_) => continue, // already present
            Err(e) if is_not_found(&e) => {
                // fall through to create
            }
            Err(e) => {
                return Err(anyhow::anyhow!("probing reserved org {name}: {e:?}"));
            }
        }
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("name".into(), json!(name));
        data.insert("is_reserved".into(), json!(true));
        // owner_user_id, verified_via, verified_ref left unset (optional).
        db::create(ctx, ORGS, data)
            .await
            .map_err(|e| anyhow::anyhow!("creating reserved org {name}: {e:?}"))?;
    }
    Ok(())
}

/// Whether a `WaferError` from the typed database client represents a
/// not-found condition (the row being probed doesn't exist).
fn is_not_found(err: &wafer_run::WaferError) -> bool {
    err.code == wafer_run::ErrorCode::NotFound
}
