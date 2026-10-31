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
  local failed=0 var dir want have dirty

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
      echo "$dirty" | sed 's/^/         /' >&2
      failed=1
    fi
  done
  return "$failed"
}

# Decide what the post-promotion `/_health` answer means.
#
# Usage: health_gate_verdict STATUS BODY_FILE
# Prints one of:
#   healthy  — 200;
#   outage   — 503 whose body has `degraded` entries and no `broken` or
#              `failed` ones: the config source (D1) failed transiently
#              after promotion. That says nothing about the new version, and
#              the previous one would answer the same, so it is not rolled
#              back;
#   rollback — anything else: `broken` (a required key unset) or `failed`
#              (a source fault) entries, another status, or a body that is
#              not the health block's.
health_gate_verdict() {
  local status=$1 body_file=$2
  if [[ "$status" == "200" ]]; then
    echo healthy
  elif [[ "$status" == "503" ]] && jq -e '
      (.broken | type == "array" and length == 0)
      and (.failed | type == "array" and length == 0)
      and (.degraded | type == "array" and length > 0)
    ' "$body_file" >/dev/null 2>&1; then
    echo outage
  else
    echo rollback
  fi
}
