//! Registry block migrations. Delegated to `impresspress_core::migration_helper`.
//!
//! Hash-gated apply — runs only when the SQL hash differs from the recorded
//! `current_hash` in `impresspress__admin__block_settings`. Concatenated SQL of
//! all migration scripts is hashed and tracked.
//!
//! Backend selection reads the `WAFER_RUN_SHARED__DATABASE__BACKEND` config key
//! (`sqlite` | `postgres`). Falls back to `sqlite` when the config block is
//! not registered — the same default impresspress-native applies.

use impresspress_core::migration_helper;
use wafer_run::context::Context;

const SQL_001_SQLITE: &str = include_str!("001_initial_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_initial_schema.postgres.sql");

pub async fn apply(ctx: &dyn Context) -> Result<(), String> {
    migration_helper::apply_migrations(
        ctx,
        "wafer-run/registry",
        &[SQL_001_SQLITE],
        &[SQL_001_POSTGRES],
    )
    .await
}
