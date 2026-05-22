//! Registry block migrations. Delegated to `solobase_core::migration_helper`.
//!
//! Hash-gated apply — runs only when the SQL hash differs from the recorded
//! `current_hash` in `suppers_ai__admin__block_settings`. Concatenated SQL of
//! all migration scripts is hashed and tracked.
//!
//! Backend selection reads the `SOLOBASE_SHARED__DATABASE__BACKEND` config key
//! (`sqlite` | `postgres`). Falls back to `sqlite` when the config block is
//! not registered — the same default solobase-native applies.

use solobase_core::migration_helper;
use wafer_core::clients::config;
use wafer_run::context::Context;

const SQL_001_SQLITE: &str = include_str!("001_initial_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_initial_schema.postgres.sql");

pub async fn apply(ctx: &dyn Context) -> Result<(), String> {
    let backend = config::get_default(ctx, "SOLOBASE_SHARED__DATABASE__BACKEND", "sqlite")
        .await
        .to_ascii_lowercase();
    let sql = if backend == "postgres" {
        SQL_001_POSTGRES
    } else {
        SQL_001_SQLITE
    };
    migration_helper::apply_if_blessed(ctx, "wafer-run/registry", sql).await
}
