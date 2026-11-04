#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
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
#   7. ../wafer-run and ../impresspress checked out, clean, at WAFER_RUN_REF
#      and IMPRESSPRESS_REF (.github/workflows/ci.yml). `build` and `deploy`
#      refuse to run otherwise (check_sibling_pins, scripts/lib/
#      deploy-checks.sh).
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
# shellcheck source=lib/deploy-checks.sh
. "$SITE_ROOT/scripts/lib/deploy-checks.sh"

cmd=${1:-deploy}

# ── Sibling checkouts at the CI pins ─────────────────────────────────
# Before anything builds: the Worker is built from ../wafer-run and
# ../impresspress, so they must be the commits CI tested.
if [[ "$cmd" == "build" || "$cmd" == "deploy" ]]; then
  if ! check_sibling_pins "$SITE_ROOT"; then
    echo "error: refusing to $cmd: the sibling checkouts are not at the pins in .github/workflows/ci.yml" >&2
    exit 1
  fi
  echo "siblings at the ci.yml pins"
fi
if [[ "$cmd" == "deploy" ]] && ! command -v jq >/dev/null 2>&1; then
  echo "error: the post-deploy /_health gate and its rollback need jq" >&2
  exit 1
fi

ENV_FILE=${ENV_FILE:-.env}

# ── Load env ─────────────────────────────────────────────────────────
if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: $ENV_FILE not found. Copy .env.example and fill in CF values." >&2
  exit 1
fi
set -a
# The deployer's own env file (.env.example lists its keys); nothing to lint.
# shellcheck source=/dev/null
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

case "$cmd" in
  build)
    "$IMPRESSPRESS_BIN" build --target cloudflare
    ;;

  deploy)
    "$IMPRESSPRESS_BIN" deploy --target cloudflare

    # Post-deploy health gate: `/_health` (the wafer-site/health block)
    # reports whether every block's config resolves through the config
    # source (D1) — `broken`, `degraded` and `failed` lists — and, under
    # `runtime`, impresspress's report of the runtime serving the request
    # (the `/health` body: `ok`, `degraded`, `stale` or `failed`).
    #
    # A D1 outage already underway never reaches this gate: `impresspress
    # deploy` stops at `/_deploy/prepare` or at the candidate smoke
    # (impresspress.toml `deploy_smoke_paths`), before promotion, with the
    # live version untouched. Here, after promotion, `health_gate_verdict`
    # (scripts/lib/deploy-checks.sh, the one list of its rules and why)
    # decides `healthy`, `outage` (keep the new version, report and exit
    # non-zero) or `rollback`. `rollback_to_previous` (deploy-checks.sh)
    # returns to the version at 100% in the deployment before this one — on
    # a first deploy the CLI's 503 placeholder — unless it predates the
    # rollback baseline, and checks the result; either way the deploy exits
    # non-zero.
    # A 503 carrying `Retry-After` is asked once more after that delay (at
    # most 60 s), and only the second answer is judged: one isolate's blip
    # clears within its own window, an outage or a fault answers the same.
    #
    # HEALTH_URL overrides the default wafer.run URL for canary /
    # staging environments. HEALTH_SKIP=1 disables the gate (e.g. when
    # deploying a brand-new worker whose DNS hasn't propagated yet).
    if [[ "${HEALTH_SKIP:-0}" != "1" ]]; then
      health_url=${HEALTH_URL:-https://wafer.run/_health}
      health_headers=$(mktemp)
      health_body=$(mktemp)
      trap 'rm -f "$health_headers" "$health_body"' EXIT
      echo
      echo "Waiting for new version to propagate before /_health check…"
      sleep 5
      status=$(health_fetch "$health_url" "$health_headers" "$health_body")
      delay=$(health_gate_retry_delay "$status" "$health_headers")
      if [[ -n "$delay" ]]; then
        echo "$health_url: 503 with Retry-After; asking once more in ${delay}s"
        sleep "$delay"
        status=$(health_fetch "$health_url" "$health_headers" "$health_body")
      fi
      case "$(health_gate_verdict "$status" "$health_headers" "$health_body")" in
        healthy)
          echo "$health_url: 200 — deploy complete"
          ;;
        outage)
          echo "error: $health_url returned $status: the backend (D1) is failing, which says" >&2
          echo "       nothing about the new version. It stays live; check D1 and the Worker" >&2
          echo "       logs (\`./scripts/deploy-cloudflare.sh tail\`). The answer:" >&2
          cat "$health_body" >&2 || true
          echo >&2
          exit 1
          ;;
        *)
          echo "error: $health_url returned $status — rolling back" >&2
          cat "$health_body" >&2 || true
          echo >&2
          # A refused or failed rollback has printed what to do; a verified
          # one still leaves a deploy that failed.
          rollback_to_previous "$WRANGLER_TOML" \
            "post-deploy /_health failed (status: $status)" || exit 1
          exit 1
          ;;
      esac
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
