# shellcheck shell=bash
# Checks `scripts/deploy-cloudflare.sh` runs around a deploy. Sourced, not
# executed; `tests/deploy_checks.rs` runs each function on fixtures.

# Refuse to build unless the sibling checkouts are exactly what CI builds.
#
# The site builds against path deps on ../wafer-run and ../impresspress
# (Cargo.toml), and the Worker `impresspress deploy` uploads is built from
# whatever those checkouts hold. CI checks them out at WAFER_RUN_REF and
# IMPRESSPRESS_REF (.github/workflows/ci.yml), so a deploy from any other
# commit, or from uncommitted changes, ships code CI never ran. The pins are
# read from ci.yml so there is one source for them.
#
# Usage: check_sibling_pins SITE_ROOT
# Prints one line per problem to stderr and returns 1 if there is any.
check_sibling_pins() {
  local site_root=$1
  local ci_yml="$site_root/.github/workflows/ci.yml"
  local failed=0 pair var dir want have dirty line

  for pair in WAFER_RUN_REF:wafer-run IMPRESSPRESS_REF:impresspress; do
    var=${pair%%:*}
    dir="$site_root/../${pair#*:}"
    want=$(sed -n "s/^  $var: *\([0-9a-f]\{40\}\) *$/\1/p" "$ci_yml")
    if [[ -z "$want" ]]; then
      echo "error: no 40-hex $var pin in $ci_yml" >&2
      failed=1
      continue
    fi
    if ! have=$(git -C "$dir" rev-parse --verify --quiet HEAD); then
      echo "error: $dir is not a git checkout (needed at $var $want)" >&2
      failed=1
      continue
    fi
    if [[ "$have" != "$want" ]]; then
      echo "error: $dir is at $have, but ci.yml pins $var $want" >&2
      echo "       check it out with: git -C $dir checkout --detach $want" >&2
      failed=1
    fi
    dirty=$(git -C "$dir" status --porcelain)
    if [[ -n "$dirty" ]]; then
      echo "error: $dir has uncommitted changes; CI builds $var as committed:" >&2
      while IFS= read -r line; do
        echo "         $line" >&2
      done <<<"$dirty"
      failed=1
    fi
  done
  return "$failed"
}

# Request URL once, writing the response headers to HEADERS_FILE and the
# body to BODY_FILE, and print the HTTP status.
#
# Usage: health_fetch URL HEADERS_FILE BODY_FILE
# Both files are emptied first: on a connection failure curl writes neither
# (and prints "000"), so an earlier answer must not be left behind to be
# judged or printed as this one.
health_fetch() {
  local url=$1 headers_file=$2 body_file=$3
  : >"$headers_file"
  : >"$body_file"
  curl -s -D "$headers_file" -o "$body_file" -w "%{http_code}" \
    --max-time 30 "$url" || true
}

# The `Retry-After` of a response, as whole seconds.
#
# Usage: retry_after_secs HEADERS_FILE
# HEADERS_FILE is what `curl -D` wrote. Prints the header's value when it is
# delta-seconds (the only form the Worker sends), and nothing when the header
# is absent or in any other form.
retry_after_secs() {
  local headers_file=$1
  [[ -f "$headers_file" ]] || return 0
  tr -d '\r' <"$headers_file" \
    | sed -n 's/^[Rr][Ee][Tt][Rr][Yy]-[Aa][Ff][Tt][Ee][Rr]:[[:space:]]*\([0-9][0-9]*\)[[:space:]]*$/\1/p' \
    | tail -n 1
}

# How long to wait before asking `/_health` once more, if at all.
#
# Usage: health_gate_retry_delay STATUS HEADERS_FILE
# Prints the seconds to wait when the answer is a 503 with `Retry-After`
# (at least 1, at most 60), and nothing otherwise. The gate asks once more
# after that wait, and judges the second answer alone: a 503 that clears
# within its own `Retry-After` was one isolate's blip (a D1 error during its
# cold boot, a request that spent its D1 call budget), not a verdict on the
# version; an outage, or a fault of the version, answers the same again.
health_gate_retry_delay() {
  local status=$1 headers_file=$2 seconds
  [[ "$status" == "503" ]] || return 0
  seconds=$(retry_after_secs "$headers_file")
  [[ -n "$seconds" ]] || return 0
  seconds=$((10#$seconds))
  if ((seconds < 1)); then
    seconds=1
  elif ((seconds > 60)); then
    seconds=60
  fi
  echo "$seconds"
}

# Decide what a post-promotion `/_health` answer means.
#
# Usage: health_gate_verdict STATUS HEADERS_FILE BODY_FILE
# (the HTTP status, the headers `curl -D` wrote, and the body). Prints one of:
#
#   healthy  — 200.
#   outage   — the backend (D1) is failing, which says nothing about the new
#              version, and rolling back would not bring D1 back: the new
#              version is kept, and the script reports and exits non-zero.
#              Any of:
#              - a 503 with `Retry-After` whose body is not JSON: the Worker's
#                own plain-text 503. impresspress sends it when no runtime
#                could be built inside a transient retry window, but also to
#                a request that waited on another request's failed build
#                whatever that build's class (a permanent fault included),
#                and when the build slot stayed busy. This gate cannot tell
#                those apart, so it reads them all as an outage; the one
#                retry usually turns a fault into its 500 (the retry becomes
#                the rebuild owner, or lands in the fault's window). The root
#                fix, a waiter answering the owner's class, is impresspress's;
#              - the health body with empty `broken` and `failed`, and
#                `runtime.status` `stale`: the runtime serves config older
#                than the database's because a transient failure stopped its
#                replacement — a rebuild failed and the runtime it was
#                replacing serves on, the backend check before a hand-off
#                failed, or a cold isolate whose dynamic build failed serves
#                its version's packaged plan (impresspress U-B1). An outage,
#                or rarely a block of the new version whose own `Init` resets
#                D1 so that the rebuild's reads fail too (IP-C logs a block
#                retrying across 8 runtimes at `error`);
#              - the same with `runtime.status` `degraded`, unless blocks are
#                listed as not serving while every block's config answered
#                (see below). A `degraded` runtime that lists no block (its
#                variables read failed after it was published) is an outage
#                whatever the config lists hold;
#              - the same with `runtime.status` `ok` and `degraded` config
#                entries (the config source failed transiently).
#   rollback — the new version is at fault, or the answer is not one the
#              health block gives during an outage:
#              - `broken` entries (a required key unset) or `failed` entries
#                (the config source faulted);
#              - `runtime.status` `failed` (a block's `Init`, or a rebuild,
#                failed for good);
#              - `runtime.status` `degraded` with blocks not serving while
#                every block's config answered (`degraded` empty): read as a
#                block whose own `Init` keeps failing transiently — resetting
#                D1 itself, say — with the database otherwise answering.
#                Known misjudgement: an outage that begins after a runtime's
#                variables read succeeded and during a block's `Init` gives
#                the same body for as long as it lasts. The config lists come
#                from that runtime's variables snapshot, and impresspress's
#                retry checks D1 with one read before it rebuilds; a failed
#                check builds nothing (`GateFailed`), so the runtime stays
#                `degraded` instead of turning `stale`, and the body does not
#                say the check failed. That outage is rolled back. The fix is
#                impresspress's: the health body naming a failed backend
#                check, which this rule would then read as an outage;
#              - a 503 naming nothing, any other status (a 500, curl's 000),
#                JSON that is not the health body, or a non-JSON 503 without
#                `Retry-After`.
health_gate_verdict() {
  local status=$1 headers_file=$2 body_file=$3
  if [[ "$status" == "200" ]]; then
    echo healthy
    return 0
  fi
  if [[ "$status" != "503" ]]; then
    echo rollback
    return 0
  fi
  if ! jq -e . "$body_file" >/dev/null 2>&1; then
    if [[ -n "$(retry_after_secs "$headers_file")" ]]; then
      echo outage
    else
      echo rollback
    fi
    return 0
  fi
  jq -r '
    def list(f): f | type == "array";
    if (list(.ok) and list(.broken) and list(.degraded) and list(.failed)
        and (.runtime | type == "object")
        and (.runtime.status | type == "string")) | not then "rollback"
    elif (.broken | length) > 0 or (.failed | length) > 0 then "rollback"
    elif .runtime.status == "failed" then "rollback"
    elif .runtime.status == "stale" then "outage"
    elif .runtime.status == "degraded" then
      if ((.runtime.unavailable // []) | length) > 0 and (.degraded | length) == 0
      then "rollback" else "outage" end
    elif .runtime.status == "ok" and (.degraded | length) > 0 then "outage"
    else "rollback" end
  ' "$body_file"
}

# Worker versions `deploy-cloudflare.sh` never rolls back to, one per line:
# the version id, then why. 9fbf49a3 is the same code as its successor but
# carries secrets since deleted from the Worker (the bootstrap-admin token
# among them), which a rollback to it would bring back.
REFUSED_ROLLBACK_VERSIONS='9fbf49a3-d969-4b23-981b-eb55810cbd46 carries secrets since deleted from the Worker'

# The version a rollback returns to: the one at 100% in the most recent
# deployment before the current one — what `wrangler rollback` picks with no
# version id (its `fetchDefaultRollbackVersionId`), resolved here so the
# script can refuse it before anything changes.
#
# Usage: rollback_target_version DEPLOYMENTS_JSON
# DEPLOYMENTS_JSON is what `wrangler deployments list --json` wrote. Prints
# the version id, or prints an error to stderr and returns 1 when no earlier
# deployment has a version at 100%.
rollback_target_version() {
  local deployments_json=$1 version
  version=$(jq -r '
    sort_by(.created_on) | reverse | .[1:]
    | map(.versions // [] | map(select(.percentage == 100)) | .[0].version_id // empty)
    | .[0] // empty
  ' "$deployments_json" 2>/dev/null) || version=
  if [[ -z "$version" ]]; then
    echo "error: no earlier deployment with a version at 100% in $deployments_json" >&2
    return 1
  fi
  echo "$version"
}

# Refuse a rollback target listed in REFUSED_ROLLBACK_VERSIONS.
#
# Usage: check_rollback_target VERSION_ID
# Returns 0 when the version may be rolled back to; otherwise prints why to
# stderr and returns 1.
check_rollback_target() {
  local version=$1 id reason
  while read -r id reason; do
    [[ -n "$id" ]] || continue
    if [[ "$version" == "$id" ]]; then
      echo "error: refusing to roll back to $version: $reason." >&2
      echo "       Deploy forward instead: fix the commit and run this script again." >&2
      return 1
    fi
  done <<<"$REFUSED_ROLLBACK_VERSIONS"
  return 0
}
