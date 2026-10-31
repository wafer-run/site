#!/usr/bin/env bash
# Deploy wafer-site to Cloudflare Workers + R2 + D1.
#
# Prerequisites (one-time):
#   1. `npx wrangler@4 login` (or set CLOUDFLARE_API_TOKEN)
#   2. npx wrangler@4 d1 create <name>          → put the id in the env file
#   3. npx wrangler@4 r2 bucket create <name>   → name must match impresspress.toml
#   4. npx wrangler@4 kv namespace create CONFIG_CACHE → id into
#      wrangler.overrides.toml
#   5. An env file (see .env.example) with:
#        CLOUDFLARE_ACCOUNT_ID
#        CLOUDFLARE_API_TOKEN
#        IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID
#        IMPRESSPRESS_DEPLOY_TOKEN (see below)
#   6. impresspress binary on PATH, OR env $IMPRESSPRESS_BIN points at it,
#      OR a sibling clone of impresspress/impresspress exists at ../impresspress.
#      Use the CLI from the impresspress commit this site pins
#      (IMPRESSPRESS_REF in .github/workflows/ci.yml): a CLI and a Worker from
#      different releases refuse each other.
#
# Usage:
#   ./scripts/deploy-cloudflare.sh                    # build + deploy
#   ./scripts/deploy-cloudflare.sh build              # build only
#   ./scripts/deploy-cloudflare.sh tail               # stream worker logs
#
# Deploy flow: `deploy` runs `impresspress deploy --target cloudflare`, which
# does all of it in one command:
#   - builds and deploys the password-hasher Worker first
#     (`wafer-site-password-hasher`, a SQLite-backed Durable Object; created
#     on the first deploy, see impresspress.toml [cloudflare.password_hasher]);
#   - creates the main Worker when it does not exist yet (a 503 placeholder,
#     replaced by the promotion below), and sets IMPRESSPRESS_DEPLOY_TOKEN and
#     WAFER_RUN__AUTH__JWT_SECRET on it when it lacks them — each from the
#     same-named environment variable when set, otherwise generated; a
#     generated deploy token is printed once with the `export` line later
#     deploys need;
#   - uploads a new main Worker version without routing traffic to it, runs
#     `/_deploy/prepare` on it (every block's pending migrations, each file
#     one transaction with its ledger row, and the seeds), verifies it
#     (`/_deploy/verify`: preview lockdown, release assets, the hasher probe,
#     impresspress.toml's `deploy_smoke_paths`), and only then promotes it.
# A Worker that already holds its deploy token needs IMPRESSPRESS_DEPLOY_TOKEN
# exported with the same value; without it the CLI stops before the build and
# names the command that sets a new one.
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
need=(CLOUDFLARE_ACCOUNT_ID CLOUDFLARE_API_TOKEN IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID)
for var in "${need[@]}"; do
  val=${!var:-}
  if [[ -z "$val" || "$val" == "REPLACE_ME" ]]; then
    echo "error: $var is unset or still 'REPLACE_ME' in $ENV_FILE" >&2
    exit 1
  fi
done

# ── Locate impresspress binary ───────────────────────────────────────────
if [[ -n "${IMPRESSPRESS_BIN:-}" && -x "$IMPRESSPRESS_BIN" ]]; then
  :  # honored as-is
elif command -v impresspress >/dev/null 2>&1; then
  IMPRESSPRESS_BIN=$(command -v impresspress)
elif [[ -x ../impresspress/target/release/impresspress ]]; then
  IMPRESSPRESS_BIN=$(readlink -f ../impresspress/target/release/impresspress)
elif [[ -d ../impresspress ]]; then
  echo "building impresspress from ../impresspress (one-time, ~1 min)…"
  (cd ../impresspress && cargo build -p impresspress --release --quiet)
  IMPRESSPRESS_BIN=$(readlink -f ../impresspress/target/release/impresspress)
else
  echo "error: impresspress binary not found. Set \$IMPRESSPRESS_BIN, install via" >&2
  echo "       cargo install (from ../impresspress), or place a sibling clone." >&2
  exit 1
fi
echo "using impresspress: $IMPRESSPRESS_BIN"

WRANGLER_TOML="$SITE_ROOT/target/impresspress-cloudflare/wrangler.toml"

cmd=${1:-deploy}
case "$cmd" in
  build)
    "$IMPRESSPRESS_BIN" build --target cloudflare
    ;;

  deploy)
    "$IMPRESSPRESS_BIN" deploy --target cloudflare

    # Post-deploy health gate. `/_health` (wafer-site/health block) walks
    # every registered block's `ConfigVar` declarations and 503s if any
    # required key is unset (the registry's keys are all optional, so an
    # unset registry admin email is not one) or if the config source (D1)
    # cannot answer for a block, so a deploy during a D1 outage rolls back
    # by design. Roll back on non-200 so a misconfigured deploy doesn't sit
    # live: `wrangler rollback` returns to the previous version, which on a
    # first deploy is the CLI's 503 placeholder.
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
        npx wrangler@4 rollback --config "$WRANGLER_TOML" \
          --message "post-deploy /_health failed (status: $status)" || true
        exit 1
      fi
      echo "$health_url: 200 — deploy complete"
    else
      echo "HEALTH_SKIP=1 — skipping post-deploy /_health gate"
    fi
    ;;

  tail)
    if [[ ! -f "$WRANGLER_TOML" ]]; then
      echo "error: wrangler.toml not found — run 'build' first" >&2
      exit 1
    fi
    npx wrangler@4 tail --config "$WRANGLER_TOML" --format pretty
    ;;

  *)
    echo "usage: $0 [build|deploy|tail]" >&2
    exit 1
    ;;
esac
