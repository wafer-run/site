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
#   ./scripts/deploy-cloudflare.sh           # build + deploy
#   ./scripts/deploy-cloudflare.sh build     # build only
#   ./scripts/deploy-cloudflare.sh secret    # set JWT worker secret only
#   ./scripts/deploy-cloudflare.sh tail      # stream worker logs

set -euo pipefail

cd "$(dirname "$0")/.."
SITE_ROOT=$(pwd)

# ── Load .env ────────────────────────────────────────────────────────
if [[ ! -f .env ]]; then
  echo "error: .env not found. Copy .env.example to .env and fill in CF values." >&2
  exit 1
fi
set -a
. ./.env
set +a

# ── Validate required env ────────────────────────────────────────────
need=(CLOUDFLARE_ACCOUNT_ID CLOUDFLARE_API_TOKEN SOLOBASE_CLOUDFLARE_D1_DATABASE_ID)
for var in "${need[@]}"; do
  val=${!var:-}
  if [[ -z "$val" || "$val" == "REPLACE_ME" ]]; then
    echo "error: $var is unset or still 'REPLACE_ME' in .env" >&2
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

cmd=${1:-deploy}
case "$cmd" in
  build)
    "$SOLOBASE_BIN" build --target cloudflare
    ;;
  deploy)
    "$SOLOBASE_BIN" deploy --target cloudflare
    echo
    echo "Next: ./scripts/deploy-cloudflare.sh secret   # if JWT not yet set"
    ;;
  secret)
    if [[ ! -f "$WRANGLER_TOML" ]]; then
      echo "error: wrangler.toml not found — run 'build' first" >&2
      exit 1
    fi
    if [[ -z "${SUPPERS_AI__AUTH__JWT_SECRET:-}" ]]; then
      echo "error: SUPPERS_AI__AUTH__JWT_SECRET not in .env" >&2
      exit 1
    fi
    printf '%s' "$SUPPERS_AI__AUTH__JWT_SECRET" \
      | wrangler secret put SUPPERS_AI__AUTH__JWT_SECRET --config "$WRANGLER_TOML"
    ;;
  tail)
    if [[ ! -f "$WRANGLER_TOML" ]]; then
      echo "error: wrangler.toml not found — run 'build' first" >&2
      exit 1
    fi
    wrangler tail wafer-site --config "$WRANGLER_TOML" --format pretty
    ;;
  *)
    echo "usage: $0 [build|deploy|secret|tail]" >&2
    exit 1
    ;;
esac
