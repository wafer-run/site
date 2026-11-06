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
# (the HTTP status, the headers `curl -D` wrote, and the body). Prints
# `healthy`, `outage` or `rollback`.
#
# - `outage`: the backend (D1) is failing, which says nothing about the new
#   version, and rolling back would not bring D1 back. The new version is
#   kept, and the script reports and exits non-zero.
# - `rollback`: the new version is at fault, or the answer is not one the
#   health block gives during an outage.
#
# The rules, in order (the first that applies decides):
#
#  1. 200 → healthy.
#  2. Any status but 503 (a 500, an Error 1102 page, curl's 000) → rollback.
#  3. A 503 whose body is not JSON → outage when it carries `Retry-After`,
#     rollback otherwise. With `Retry-After` it is the Worker's own
#     plain-text 503: impresspress sends it when no runtime could be built
#     inside a transient retry window, but also to a request that waited on
#     another request's failed build whatever that build's class (a permanent
#     fault included), and when the build slot stayed busy. This gate cannot
#     tell those apart, so it reads them all as an outage; the one retry
#     usually turns a fault into its 500 (the retry becomes the rebuild
#     owner, or lands in the fault's window). The root fix, a waiter
#     answering the owner's class, is impresspress's.
#  4. JSON that is not the health body (the four lists, and `runtime` with a
#     string `status`) → rollback.
#  5. `broken` entries (a required key unset) or `failed` entries (the config
#     source faulted) → rollback.
#  6. `runtime.status` `failed` (a block's `Init`, or a rebuild, failed for
#     good) → rollback, whatever `runtime.backend` says. A block that turns
#     a D1 outage into a permanent error is read as a fault here too.
#  7. With `runtime.backend` (impresspress on Cloudflare: what the isolate's
#     last settled attempt found of D1, RELEASE.md "`/health` says whether D1
#     failed the last attempt"):
#     - `backend.status` `failing` → outage. D1 itself could not answer the
#       check, a read after it, or a block's `Init` calls. A block whose own
#       `Init` query makes D1 reset reads the same in one answer; impresspress
#       logs it at `error` after 8 runtimes.
#     - `backend.status` `ok`, `runtime.status` `degraded` with blocks listed
#       in `unavailable` → rollback: D1 answered, so the listed blocks fail
#       on their own (contention, `database.contended`, counts here).
#     - `backend.status` `ok`, `runtime.status` `stale` → outage: a rebuild
#       failed on something other than D1, or a cold isolate serves its
#       version's packaged plan.
#     - `backend.status` `ok`, `runtime.status` `degraded` with nothing
#       listed → outage: a config read failed after the runtime was
#       published, and no attempt has looked since.
#     - `backend.status` `ok`, `runtime.status` `ok`, `degraded` config
#       entries → outage (the config source failed transiently).
#     - anything else (another `backend.status`, a 503 naming no cause) →
#       rollback.
#  8. Without `runtime.backend` (impresspress's native server, or a version
#     from before the field):
#     - `runtime.status` `stale` → outage: the runtime serves config older
#       than the database's because a transient failure stopped its
#       replacement (a failed rebuild, a failed check before a hand-off, a
#       cold isolate serving its packaged plan). An outage, or rarely a block
#       of the new version whose own `Init` resets D1.
#     - `runtime.status` `degraded` with blocks listed while every block's
#       config answered (`degraded` empty) → rollback, read as a block whose
#       own `Init` keeps failing with D1 answering. An outage that begins
#       during a block's `Init` gives the same body; `runtime.backend` is
#       what tells them apart.
#     - any other `degraded` → outage. One that lists no block had its
#       variables read fail after it was published.
#     - `runtime.status` `ok` with `degraded` config entries → outage.
#     - anything else (a 503 naming no cause) → rollback.
#
# The gate judges the answer after one retry past `Retry-After`
# (`health_gate_retry_delay`), so the attempt `backend` reports is one made
# after the window the first answer named.
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
    def listed: ((.runtime.unavailable // []) | length) > 0;
    if (list(.ok) and list(.broken) and list(.degraded) and list(.failed)
        and (.runtime | type == "object")
        and (.runtime.status | type == "string")) | not then "rollback"
    elif (.broken | length) > 0 or (.failed | length) > 0 then "rollback"
    elif .runtime.status == "failed" then "rollback"
    elif (.runtime.backend | type) == "object" then
      if .runtime.backend.status == "failing" then "outage"
      elif .runtime.backend.status != "ok" then "rollback"
      elif .runtime.status == "degraded" then
        if listed then "rollback" else "outage" end
      elif .runtime.status == "stale" then "outage"
      elif .runtime.status == "ok" and (.degraded | length) > 0 then "outage"
      else "rollback" end
    elif .runtime.status == "stale" then "outage"
    elif .runtime.status == "degraded" then
      if listed and (.degraded | length) == 0 then "rollback" else "outage" end
    elif .runtime.status == "ok" and (.degraded | length) > 0 then "outage"
    else "rollback" end
  ' "$body_file"
}

# The oldest Worker version a rollback may return to: the first version
# made after wafer.run's 2026-10-30 reset. A version created before it
# predates the reset, so `rollback_to_previous` refuses it whatever it is.
ROLLBACK_BASELINE_VERSION=4c6ff923-be37-453a-a240-7c784072b76a

# The version a rollback returns to: the one at 100% in the most recent
# deployment before the current one — the rule `wrangler rollback` applies
# with no version id (its `fetchDefaultRollbackVersionId`).
#
# Usage: rollback_target_version <DEPLOYMENTS_JSON
# Reads what `wrangler deployments list --json` prints. Prints the version
# id, or prints why not to stderr and returns 1: the input is not a JSON
# array (a parse failure, named as one), or no earlier deployment has a
# version at 100%.
rollback_target_version() {
  local deployments version
  deployments=$(cat)
  if ! jq -e 'type == "array"' >/dev/null <<<"$deployments"; then
    echo "error: could not parse wrangler's deployments list as a JSON array" >&2
    return 1
  fi
  version=$(jq -r '
    sort_by(.created_on) | reverse | .[1:]
    | map(.versions // [] | map(select(.percentage == 100)) | .[0].version_id // empty)
    | .[0] // empty
  ' <<<"$deployments")
  if [[ -z "$version" ]]; then
    echo "error: no earlier deployment has a version at 100% to roll back to" >&2
    return 1
  fi
  echo "$version"
}

# The version serving all traffic: the one at 100% in the most recent
# deployment.
#
# Usage: live_version <DEPLOYMENTS_JSON
# Prints the version id, or prints why not to stderr and returns 1.
live_version() {
  local deployments version
  deployments=$(cat)
  if ! jq -e 'type == "array"' >/dev/null <<<"$deployments"; then
    echo "error: could not parse wrangler's deployments list as a JSON array" >&2
    return 1
  fi
  version=$(jq -r '
    sort_by(.created_on) | last // {}
    | .versions // [] | map(select(.percentage == 100)) | .[0].version_id // empty
  ' <<<"$deployments")
  if [[ -z "$version" ]]; then
    echo "error: the latest deployment has no version at 100%" >&2
    return 1
  fi
  echo "$version"
}

# When a Worker version was created, as Unix seconds.
#
# Usage: version_created_secs <VERSION_JSON
# Reads what `wrangler versions view ID --json` prints (`metadata.created_on`,
# RFC 3339 with fractional seconds). Prints the seconds, or prints why not to
# stderr and returns 1.
version_created_secs() {
  local version secs
  version=$(cat)
  if ! jq -e 'type == "object"' >/dev/null <<<"$version"; then
    echo "error: could not parse wrangler's version as a JSON object" >&2
    return 1
  fi
  if ! secs=$(jq -er '
    .metadata.created_on | sub("\\.[0-9]+"; "") | fromdateiso8601
  ' <<<"$version"); then
    echo "error: the version has no RFC 3339 metadata.created_on" >&2
    return 1
  fi
  echo "$secs"
}

# Roll the Worker back after the gate's `rollback` verdict, or refuse to.
#
# Usage: rollback_to_previous WRANGLER_TOML MESSAGE
# 1. Resolves the target from `wrangler deployments list --json`
#    (`rollback_target_version`).
# 2. Refuses a target created before ROLLBACK_BASELINE_VERSION, reading both
#    versions' `created_on` from `wrangler versions view --json`; a target or
#    baseline it cannot read is refused too.
# 3. Deploys the target at 100% with `wrangler versions deploy`, not
#    `wrangler rollback`: when the target's secrets differ from the Worker's
#    (API error 10220), `rollback` confirms on its own outside a terminal and
#    redeploys with `force=true`, restoring them; `versions deploy` never
#    forces, so that error fails the deploy instead.
# 4. Reads the deployments again and checks the target is live: wrangler
#    exits 0 when an operator declines its prompt.
# Prints progress to stdout and every failure to stderr, with what to do.
# Returns 0 only when the target is verified live.
rollback_to_previous() {
  local config=$1 message=$2 deployments new target target_secs baseline_secs live
  local forward="Deploy forward from main: merge a fix and run ./scripts/deploy-cloudflare.sh from a clean main checkout."
  if ! deployments=$(npx wrangler@4 deployments list --config "$config" --json); then
    echo "error: could not list the Worker's deployments, so nothing was rolled back." >&2
    echo "       $forward" >&2
    return 1
  fi
  new=$(live_version <<<"$deployments") || new="(unknown)"
  if ! target=$(rollback_target_version <<<"$deployments"); then
    echo "error: nothing to roll back to; $new is still live." >&2
    echo "       $forward" >&2
    return 1
  fi
  if ! baseline_secs=$(npx wrangler@4 versions view "$ROLLBACK_BASELINE_VERSION" \
    --config "$config" --json | version_created_secs) ||
    ! target_secs=$(npx wrangler@4 versions view "$target" \
      --config "$config" --json | version_created_secs); then
    echo "error: could not read when $target or the baseline $ROLLBACK_BASELINE_VERSION" >&2
    echo "       was created, so $target is refused; $new is still live." >&2
    echo "       $forward" >&2
    return 1
  fi
  if ((target_secs < baseline_secs)); then
    echo "error: refusing to roll back to $target: it predates the 2026-10-30 reset" >&2
    echo "       (older than $ROLLBACK_BASELINE_VERSION). $new is still live." >&2
    echo "       $forward" >&2
    return 1
  fi
  echo "Rolling back to $target"
  if ! npx wrangler@4 versions deploy "$target@100%" --config "$config" \
    --message "$message" --yes; then
    echo "error: the rollback to $target failed; $new is still live." >&2
    echo "       $forward" >&2
    return 1
  fi
  if ! deployments=$(npx wrangler@4 deployments list --config "$config" --json) ||
    ! live=$(live_version <<<"$deployments"); then
    echo "error: could not confirm the rollback to $target; check" >&2
    echo "       \`npx wrangler@4 deployments status --config $config\`." >&2
    return 1
  fi
  if [[ "$live" != "$target" ]]; then
    echo "error: rollback did not happen; $live is still live." >&2
    echo "       $forward" >&2
    return 1
  fi
  echo "Rolled back: $target is live"
}
