-- Initial registry schema. Run at block Init via db::ddl.
-- Idempotent — every statement uses IF NOT EXISTS.

CREATE TABLE IF NOT EXISTS wafer_run__registry__orgs (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    owner_user_id   TEXT,
    verified_via    TEXT,
    verified_ref    TEXT,
    is_reserved     INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_orgs_name
    ON wafer_run__registry__orgs (name);

CREATE TABLE IF NOT EXISTS wafer_run__registry__packages (
    id          TEXT PRIMARY KEY,
    org_id      TEXT NOT NULL,
    name        TEXT NOT NULL,
    summary     TEXT,
    created_by  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_packages_org_id_name
    ON wafer_run__registry__packages (org_id, name);
CREATE INDEX IF NOT EXISTS idx_packages_org_id
    ON wafer_run__registry__packages (org_id);

CREATE TABLE IF NOT EXISTS wafer_run__registry__versions (
    id              TEXT PRIMARY KEY,
    package_id      TEXT NOT NULL,
    version         TEXT NOT NULL,
    abi             INTEGER NOT NULL,
    sha256          TEXT NOT NULL,
    storage_key     TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    license         TEXT,
    readme_md       TEXT,
    dependencies    TEXT NOT NULL DEFAULT '[]',
    capabilities    TEXT NOT NULL DEFAULT '{}',
    yanked          INTEGER NOT NULL DEFAULT 0,
    yanked_reason   TEXT,
    yanked_at       TEXT,
    published_by    TEXT NOT NULL,
    published_at    TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_versions_package_id_version
    ON wafer_run__registry__versions (package_id, version);
CREATE INDEX IF NOT EXISTS idx_versions_package_id_yanked
    ON wafer_run__registry__versions (package_id, yanked);

CREATE TABLE IF NOT EXISTS wafer_run__registry__cli_login_codes (
    id          TEXT PRIMARY KEY,
    code        TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    used_at     TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_codes_code
    ON wafer_run__registry__cli_login_codes (code);

CREATE TABLE IF NOT EXISTS wafer_run__registry__tokens (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL,
    name            TEXT NOT NULL DEFAULT 'wafer-cli',
    hash            TEXT NOT NULL,
    last_used_at    TEXT,
    revoked_at      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tokens_hash
    ON wafer_run__registry__tokens (hash);
