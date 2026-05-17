#!/usr/bin/env bash
# Deploy wafer-site to Cloudflare Workers + R2 + D1.
#
# Prerequisites (one-time):
#   1. wrangler CLI installed and `wrangler login` (or set CLOUDFLARE_API_TOKEN)
#   2. wrangler d1 create <name>          → put the id in .env
#   3. wrangler r2 bucket create <name>   → name must match solobase.toml
#   4. cp .env.example .env, fill in:
#        CLOUDFLARE_ACCOUNT_ID
#        CLOUDFLARE_API_TOKEN
#        SOLOBASE_CLOUDFLARE_D1_DATABASE_ID
#   5. solobase binary on PATH, OR env $SOLOBASE_BIN points at it,
#      OR a sibling clone of suppers-ai/solobase exists at ../solobase
#
# Usage:
#   ./scripts/deploy-cloudflare.sh                    # build + deploy
#   ./scripts/deploy-cloudflare.sh build              # build only
#   ./scripts/deploy-cloudflare.sh secret             # set JWT worker secret
#   ./scripts/deploy-cloudflare.sh seed-d1-vars       # push runtime config to D1
#   ./scripts/deploy-cloudflare.sh tail               # stream worker logs
#
# First-time setup order (must run in this sequence):
#   1. secret         — push JWT_SECRET to worker secret store
#   2. deploy         — first cold start runs admin migrations and creates
#                       the variables table with the proper schema
#   3. seed-d1-vars   — push env-bound runtime config into the now-existing
#                       variables table (depends on step 2 having run)
#
# Re-deploys can run in any order; `seed-d1-vars` is idempotent. The
# previous version of this script pre-created `variables` inline with a
# minimal schema so `seed-d1-vars` could run first, but that competed with
# admin migration 001 and left `variables` permanently schema-drifted (see
# the comment on the `seed-d1-vars)` branch below).
#
# Environment selection: ENV_FILE=.env.prod ./scripts/deploy-cloudflare.sh deploy
#   defaults to .env if unset.

set -euo pipefail

cd "$(dirname "$0")/.."
SITE_ROOT=$(pwd)

ENV_FILE=${ENV_FILE:-.env}

# ── Load env ─────────────────────────────────────────────────────────
if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: $ENV_FILE not found. Copy .env.example and fill in CF values." >&2
  exit 1
fi
set -a
. "$ENV_FILE"
set +a
echo "using env: $ENV_FILE"

# ── Validate required env ────────────────────────────────────────────
need=(CLOUDFLARE_ACCOUNT_ID CLOUDFLARE_API_TOKEN SOLOBASE_CLOUDFLARE_D1_DATABASE_ID)
for var in "${need[@]}"; do
  val=${!var:-}
  if [[ -z "$val" || "$val" == "REPLACE_ME" ]]; then
    echo "error: $var is unset or still 'REPLACE_ME' in $ENV_FILE" >&2
    exit 1
  fi
done

# ── Locate solobase binary ───────────────────────────────────────────
if [[ -n "${SOLOBASE_BIN:-}" && -x "$SOLOBASE_BIN" ]]; then
  :  # honored as-is
elif command -v solobase >/dev/null 2>&1; then
  SOLOBASE_BIN=$(command -v solobase)
elif [[ -x ../solobase/target/release/solobase ]]; then
  SOLOBASE_BIN=$(readlink -f ../solobase/target/release/solobase)
elif [[ -d ../solobase ]]; then
  echo "building solobase from ../solobase (one-time, ~1 min)…"
  (cd ../solobase && cargo build -p solobase --release --quiet)
  SOLOBASE_BIN=$(readlink -f ../solobase/target/release/solobase)
else
  echo "error: solobase binary not found. Set \$SOLOBASE_BIN, install via" >&2
  echo "       cargo install (from ../solobase), or place a sibling clone." >&2
  exit 1
fi
echo "using solobase: $SOLOBASE_BIN"

WRANGLER_TOML="$SITE_ROOT/target/solobase-cloudflare/wrangler.toml"

# Variable-name prefixes whose values belong in D1's runtime config table
# (`suppers_ai__admin__variables`). The worker reads from there at cold
# start. Solobase deploy creds (CLOUDFLARE_*, SOLOBASE_CLOUDFLARE_*) and
# native-only infra (SOLOBASE_DB_PATH, SOLOBASE_LISTEN, etc.) are
# excluded.
D1_VAR_PREFIXES='^(SOLOBASE_SHARED__|SUPPERS_AI__|WAFER_RUN__)'
# JWT secret lives as a worker secret (set via `secret` subcommand), not
# in D1 — the worker reads it from `env.secret(JWT_SECRET_KEY)` directly.
D1_VAR_BLOCKLIST='^SUPPERS_AI__AUTH__JWT_SECRET$'

# Generate UUIDs for the inserted rows.
gen_uuid() {
  if [[ -r /proc/sys/kernel/random/uuid ]]; then
    cat /proc/sys/kernel/random/uuid
  else
    # Fallback: 32 hex chars, dashed.
    od -An -N16 -tx1 /dev/urandom | tr -d ' \n' \
      | sed 's/\(........\)\(....\)\(....\)\(....\)\(.*\)/\1-\2-\3-\4-\5/'
  fi
}

# SQL-escape a value for single-quoted literals (' → '').
sql_escape() { printf '%s' "$1" | sed "s/'/''/g"; }

cmd=${1:-deploy}
case "$cmd" in
  build)
    "$SOLOBASE_BIN" build --target cloudflare
    ;;

  deploy)
    "$SOLOBASE_BIN" deploy --target cloudflare

    # Post-deploy health gate. `/_health` (wafer-site/health block) walks
    # every registered block's `ConfigVar` declarations and 503s if any
    # required key is unset. Roll back on non-200 so a misconfigured
    # deploy doesn't sit live with the prior version's traffic.
    #
    # HEALTH_URL overrides the default wafer.run URL for canary /
    # staging environments. HEALTH_SKIP=1 disables the gate (e.g. when
    # deploying a brand-new worker whose DNS hasn't propagated yet).
    if [[ "${HEALTH_SKIP:-0}" != "1" ]]; then
      health_url=${HEALTH_URL:-https://wafer.run/_health}
      echo
      echo "Waiting for new version to propagate before /_health check…"
      sleep 5
      status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 30 "$health_url" || echo "000")
      if [[ "$status" != "200" ]]; then
        echo "error: $health_url returned $status — rolling back" >&2
        wrangler rollback --config "$WRANGLER_TOML" \
          --message "post-deploy /_health failed (status: $status)" || true
        exit 1
      fi
      echo "$health_url: 200 — deploy complete"
    else
      echo "HEALTH_SKIP=1 — skipping post-deploy /_health gate"
    fi

    echo
    echo "Next:"
    echo "  ./scripts/deploy-cloudflare.sh secret         # set JWT worker secret"
    echo "  ./scripts/deploy-cloudflare.sh seed-d1-vars   # push runtime config to D1"
    ;;

  secret)
    if [[ ! -f "$WRANGLER_TOML" ]]; then
      echo "error: wrangler.toml not found — run 'build' first" >&2
      exit 1
    fi
    if [[ -z "${SUPPERS_AI__AUTH__JWT_SECRET:-}" ]]; then
      echo "error: SUPPERS_AI__AUTH__JWT_SECRET not in $ENV_FILE" >&2
      exit 1
    fi
    printf '%s' "$SUPPERS_AI__AUTH__JWT_SECRET" \
      | wrangler secret put SUPPERS_AI__AUTH__JWT_SECRET --config "$WRANGLER_TOML"
    ;;

  seed-d1-vars)
    # Push every env var matching $D1_VAR_PREFIXES into the D1 admin
    # variables table. Idempotent: each key is DELETEd then INSERTed,
    # so re-running picks up changed values.
    #
    # PREREQUISITE: the `suppers_ai__admin__variables` table must already
    # exist with the schema declared by admin migration 001 (see
    # solobase-core/src/blocks/admin/migrations/001_admin_schema.sqlite.sql).
    # That happens automatically on the first cold start after `deploy`,
    # via `init_block(suppers-ai/admin)` in solobase-cloudflare's worker
    # entry. Running `seed-d1-vars` before any successful `deploy` will
    # fail with `no such table` from D1.
    #
    # An earlier version of this script pre-created the table inline with
    # a minimal `(id, key, value, created_at, updated_at)` schema. That
    # competed with migration 001's richer schema (`name`, `description`,
    # `value`, `warning`, `sensitive INTEGER`, `updated_by`, UNIQUE on
    # `key`, indexes): the inline `CREATE TABLE IF NOT EXISTS` won the
    # race, migration 001 became a no-op for that table, and the missing
    # columns were later added via D1Service's lazy
    # `ALTER TABLE ADD COLUMN ... TEXT` — leaving `sensitive` typed as
    # TEXT with values like `'1.0'`. Same anti-pattern that solobase #182
    # fixed for the native bootstrap. Letting admin migration 001 own the
    # schema is the durable fix.
    db_name=${SOLOBASE_CLOUDFLARE_D1_DATABASE_NAME:-wafer-site-prod}
    sql_file=$(mktemp --suffix=.sql)
    trap 'rm -f "$sql_file"' EXIT

    : >"$sql_file"

    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    count=0
    while IFS= read -r var; do
      [[ "$var" =~ $D1_VAR_BLOCKLIST ]] && continue
      val=${!var:-}
      [[ -z "$val" ]] && continue
      key_esc=$(sql_escape "$var")
      val_esc=$(sql_escape "$val")
      uuid=$(gen_uuid)
      cat >>"$sql_file" <<EOF
DELETE FROM suppers_ai__admin__variables WHERE key = '$key_esc';
INSERT INTO suppers_ai__admin__variables (id, key, value, created_at, updated_at)
VALUES ('$uuid', '$key_esc', '$val_esc', '$now', '$now');
EOF
      count=$((count + 1))
    done < <(compgen -e | grep -E "$D1_VAR_PREFIXES" | sort)

    if [[ $count -eq 0 ]]; then
      echo "no D1-bound vars in $ENV_FILE (looking for $D1_VAR_PREFIXES)"
      exit 0
    fi
    echo "seeding $count vars into D1 ($db_name)…"
    wrangler d1 execute "$db_name" --remote --file "$sql_file"
    ;;

  tail)
    if [[ ! -f "$WRANGLER_TOML" ]]; then
      echo "error: wrangler.toml not found — run 'build' first" >&2
      exit 1
    fi
    wrangler tail wafer-site --config "$WRANGLER_TOML" --format pretty
    ;;

  *)
    echo "usage: $0 [build|deploy|secret|seed-d1-vars|tail]" >&2
    exit 1
    ;;
esac
