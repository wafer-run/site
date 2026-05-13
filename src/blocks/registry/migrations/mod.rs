//! Registry block migrations. Applied from the block's `Init` lifecycle.
//!
//! SQL files are embedded with `include_str!`. Backend selection reads the
//! `SOLOBASE_SHARED__DATABASE__BACKEND` config key (`sqlite` | `postgres`).
//! Falls back to `sqlite` when the config block is not registered — the same
//! default solobase-native applies.
//!
//! Statements are executed one-by-one through `wafer-run/database`'s typed
//! `db::ddl` message contract — the WRAP-aware path that lets any
//! attributable caller run `CREATE TABLE` / `CREATE INDEX` / `DROP TABLE`
//! against its own (`{org}__{block}__*`) tables without an admin grant. The
//! parser splits on bare `;` outside of `--` line comments. Embedded `;`
//! inside string literals is NOT supported — the canonical migration files
//! don't use any.

use wafer_core::clients::{config, database as db};
use wafer_run::context::Context;

const SQL_001_SQLITE: &str = include_str!("001_initial_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_initial_schema.postgres.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Sqlite,
    Postgres,
}

async fn backend(ctx: &dyn Context) -> Backend {
    let raw = config::get_default(ctx, "SOLOBASE_SHARED__DATABASE__BACKEND", "sqlite").await;
    match raw.to_ascii_lowercase().as_str() {
        "postgres" => Backend::Postgres,
        _ => Backend::Sqlite,
    }
}

/// Apply all registry migrations in order. Idempotent: every `CREATE TABLE` /
/// `CREATE INDEX` uses `IF NOT EXISTS`.
pub async fn apply(ctx: &dyn Context) -> Result<(), String> {
    let b = backend(ctx).await;
    apply_script(
        ctx,
        match b {
            Backend::Sqlite => SQL_001_SQLITE,
            Backend::Postgres => SQL_001_POSTGRES,
        },
    )
    .await
    .map_err(|e| format!("migration 001: {e}"))?;
    Ok(())
}

/// Execute each statement in `sql` via `db::ddl`.
async fn apply_script(ctx: &dyn Context, sql: &str) -> Result<(), String> {
    for stmt in split_statements(sql) {
        if !has_executable_content(&stmt) {
            continue;
        }
        let trimmed = stmt.trim();
        db::ddl(ctx, trimmed)
            .await
            .map_err(|e| format!("ddl failed on `{trimmed}`: {e}"))?;
    }
    Ok(())
}

/// Split `sql` on `;` outside `--` line comments.
fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_line_comment = false;
    for ch in sql.chars() {
        if in_line_comment {
            current.push(ch);
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if ch == '-' && current.ends_with('-') {
            in_line_comment = true;
            current.push(ch);
            continue;
        }
        if ch == ';' {
            out.push(std::mem::take(&mut current));
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Returns true if `stmt` contains at least one non-blank, non-comment line.
fn has_executable_content(stmt: &str) -> bool {
    stmt.lines().any(|line| {
        let l = line.trim();
        !l.is_empty() && !l.starts_with("--")
    })
}

#[cfg(test)]
mod tests {
    use super::{has_executable_content, split_statements};

    #[test]
    fn empty_chunk_has_no_executable_content() {
        assert!(!has_executable_content(""));
        assert!(!has_executable_content("   \n  "));
    }

    #[test]
    fn comment_only_chunk_is_skipped() {
        assert!(!has_executable_content("-- one\n-- two\n"));
    }

    #[test]
    fn ddl_with_leading_comment_is_executed() {
        assert!(has_executable_content(
            "-- header\nCREATE TABLE foo (id TEXT)"
        ));
    }

    #[test]
    fn split_ignores_semicolons_inside_line_comments() {
        let sql = "-- Placeholder; text\nSELECT 1;";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("SELECT 1"));
    }

    #[test]
    fn split_handles_multiple_statements() {
        let sql = "DROP TABLE foo;\nCREATE TABLE bar (id TEXT);\n";
        let count = split_statements(sql)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn sql_files_split_into_expected_chunks() {
        let sqlite_count = split_statements(super::SQL_001_SQLITE)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            sqlite_count, 12,
            "sqlite migration: expected 12 statements, got {sqlite_count}"
        );

        let postgres_count = split_statements(super::SQL_001_POSTGRES)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            postgres_count, 12,
            "postgres migration: expected 12 statements, got {postgres_count}"
        );
    }
}
