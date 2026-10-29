//! Registry block migrations, applied from the block's `Init` by
//! impresspress's migration ledger runner
//! ([`impresspress_core::migration_helper::lifecycle_init`] over [`MIGRATIONS`]).
//!
//! The runner records each file in `impresspress__admin__applied_migrations`
//! by its basename and content hash, and applies a pending file as one
//! transaction together with that ledger row, so a file is either wholly
//! applied and recorded or not applied at all. `block_settings.current_hash`
//! then holds the list's
//! [`impresspress_core::migration_helper::migration_set_hash`]. The dialect
//! is picked by `WAFER_RUN_SHARED__DATABASE__BACKEND` (`sqlite` |
//! `postgres`, default `sqlite`).
//!
//! A shipped file is frozen: its basename is its ledger key and its
//! statements are its content hash, which `tests/registry_migrations.rs` pins
//! against `content-hashes.txt`. A schema change is a new numbered file.
//!
//! **The history is empty.** It would name the list states a registry
//! database migrated before the ledger can be in, so the runner could adopt
//! them. There is none to adopt: the one such database, wafer.run's D1, is
//! recreated, and a development database from before the ledger is refused on
//! its auth block anyway. A registry database from before the ledger is
//! therefore refused as an unknown state (nothing changed); recreate it. If a
//! second install ever needs one, generate it from this repository's git
//! history the way impresspress generates its own
//! (`examples/migration-history.rs`), never by hand.

use impresspress_core::migration_helper::{BlockMigrations, MigrationFile};

/// The SQLite (and D1) dialect of `001_initial_schema`.
pub const SQL_001_SQLITE: &str = include_str!("001_initial_schema.sqlite.sql");
/// The Postgres dialect of `001_initial_schema`.
pub const SQL_001_POSTGRES: &str = include_str!("001_initial_schema.postgres.sql");

/// The SQLite migration files, in apply order, as `(basename, content)`.
pub const SQLITE_MIGRATIONS: &[MigrationFile<'static>] = &[("001_initial_schema", SQL_001_SQLITE)];

/// The Postgres migration files, matching [`SQLITE_MIGRATIONS`] one for one.
pub const POSTGRES_MIGRATIONS: &[MigrationFile<'static>] =
    &[("001_initial_schema", SQL_001_POSTGRES)];

/// The registry's migrations as its `lifecycle(Init)` hands them to
/// [`impresspress_core::migration_helper::lifecycle_init`]. Empty history:
/// see the module docs.
pub const MIGRATIONS: BlockMigrations = BlockMigrations {
    block: super::NAME,
    sqlite: SQLITE_MIGRATIONS,
    postgres: POSTGRES_MIGRATIONS,
    sqlite_history: &[],
    postgres_history: &[],
};
