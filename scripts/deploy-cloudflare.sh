#!/usr/bin/env bash
# Deploy wafer-site to Cloudflare Workers + R2 + D1.
#
# Prerequisites (one-time):
#   1. wrangler CLI installed and `wrangler login` (or set CLOUDFLARE_API_TOKEN)
#   2. wrangler d1 create <name>          → put the id in .env
#   3. wrangler r2 bucket create <name>   → name must match impresspress.toml
#   4. cp .env.example .env, fill in:
#        CLOUDFLARE_ACCOUNT_ID
#        CLOUDFLARE_API_TOKEN
#        IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID
#   5. impresspress binary on PATH, OR env $IMPRESSPRESS_BIN points at it,
#      OR a sibling clone of impresspress/impresspress exists at ../impresspress
#
# Usage:
#   ./scripts/deploy-cloudflare.sh                    # build + deploy
#   ./scripts/deploy-cloudflare.sh build              # build only
#   ./scripts/deploy-cloudflare.sh secret             # set worker secrets (JWT + deploy token)
#   ./scripts/deploy-cloudflare.sh tail               # stream worker logs
#
# Deploy flow: `deploy` shells out to `impresspress deploy --target cloudflare`,
# which is atomic — it uploads a new worker version without routing traffic
# to it, POSTs to that version's `/_deploy/init` (authenticated with
# IMPRESSPRESS_DEPLOY_TOKEN) to run migrations + seeds, and only promotes the
# version to 100% traffic if that call succeeds. There's no separate manual
# seeding step anymore: migrations/seeds run in-worker as part of every
# deploy, gated on that same call succeeding.
#
# First-time setup for a brand-new worker:
#   1. secret   — push WAFER_RUN__AUTH__JWT_SECRET + IMPRESSPRESS_DEPLOY_TOKEN
#                 to the worker secret store (requires a prior `build` so
#                 wrangler.toml exists)
#   2. deploy   — runs the atomic flow above
#
# A worker that has never been deployed has no secrets set yet, so if you
# run `deploy` before `secret`, `/_deploy/init` 404s (secret unset ⇒ endpoint
# disabled) and the version is uploaded but never promoted — harmless, and
# it leaves wrangler.toml in place for the `secret` step. Sequence is then:
# deploy (fails, pre-promote) → secret → deploy (succeeds). Re-deploys after
# that can just run `deploy`; secrets don't need to be re-pushed unless
# rotating them.
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
    # Fail fast, before the (multi-minute) wasm build, if the deploy token
    # isn't in the environment. `impresspress deploy` itself also refuses to
    # run without it, but checking here avoids burning a build first.
    if [[ -z "${IMPRESSPRESS_DEPLOY_TOKEN:-}" ]]; then
      echo "error: IMPRESSPRESS_DEPLOY_TOKEN not in $ENV_FILE (or exported)." >&2
      echo "       Run './scripts/deploy-cloudflare.sh secret' to provision it," >&2
      echo "       then export the same value before re-running deploy." >&2
      exit 1
    fi

    "$IMPRESSPRESS_BIN" deploy --target cloudflare

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
    ;;

  secret)
    if [[ ! -f "$WRANGLER_TOML" ]]; then
      echo "error: wrangler.toml not found — run 'build' first" >&2
      exit 1
    fi
    if [[ -z "${WAFER_RUN__AUTH__JWT_SECRET:-}" ]]; then
      echo "error: WAFER_RUN__AUTH__JWT_SECRET not in $ENV_FILE" >&2
      exit 1
    fi
    printf '%s' "$WAFER_RUN__AUTH__JWT_SECRET" \
      | wrangler secret put WAFER_RUN__AUTH__JWT_SECRET --config "$WRANGLER_TOML"

    if [[ -z "${IMPRESSPRESS_DEPLOY_TOKEN:-}" ]]; then
      echo "error: IMPRESSPRESS_DEPLOY_TOKEN not in $ENV_FILE" >&2
      exit 1
    fi
    # Deploy-init auth token: the worker's `/_deploy/init` endpoint (hit by
    # `impresspress deploy` pre-promote, to run migrations + seeds) compares the
    # `x-deploy-token` header against this same secret. The same value must
    # also be exported as IMPRESSPRESS_DEPLOY_TOKEN in the shell that runs
    # `deploy`, so the CLI can send it.
    printf '%s' "$IMPRESSPRESS_DEPLOY_TOKEN" \
      | wrangler secret put IMPRESSPRESS_DEPLOY_TOKEN --config "$WRANGLER_TOML"
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
