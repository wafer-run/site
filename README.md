# wafer-site

The website, docs, playground, and package registry behind [wafer.run](https://wafer.run).

Built on [WAFER](https://github.com/wafer-run/wafer-run) + [impresspress](https://github.com/impresspress/impresspress) — both must be checked out as siblings (path deps in `Cargo.toml`), at the commits `.github/workflows/ci.yml` pins (`WAFER_RUN_REF`, `IMPRESSPRESS_REF`):

```
workspace/
├── wafer-run/
├── impresspress/
└── site/        # this repo
```

## Run it locally

```bash
cp .env.example .env       # works as is; see below for a first admin
mkdir -p data/manual       # IMPRESSPRESS_DB_PATH's directory must exist
cargo run                  # listens on http://localhost:8090
```

`.env.example` documents every variable the binary reads. Each boot applies the database's pending migrations before it serves. Optional:

- A first admin: uncomment `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` and `..._PASSWORD` (both, before the first boot on a fresh database).
- Publishing: sign in as that admin and set `WAFER_RUN__REGISTRY__ADMIN_EMAIL` on the admin Variables page (`/b/admin/`). The registry's variables are variables-table rows, never read from `.env`; unset, the registry serves and publishing is closed.
- The `IMPRESSPRESS__AUTH_UI__OAUTH_GITHUB_*` triple, then `WAFER_RUN_SHARED__ENABLE_OAUTH=true`, for GitHub login.

## Tests

```bash
cargo test                            # Rust unit + integration tests
npx playwright install --with-deps    # one-time
npx playwright test                   # browser end-to-end tests
```

Playwright spins the binary on port 8090 and drives Chromium against it.

CI also type-checks and lints the Cloudflare build:

```bash
cargo clippy --lib --target wasm32-unknown-unknown --no-default-features \
  --features target-cloudflare -- -D warnings
```

The toolchain (and the wasm target) come from `rust-toolchain.toml`.

## Deploy to Cloudflare

The crate also builds as a `wasm32-unknown-unknown` cdylib for Cloudflare Workers via the `target-cloudflare` feature. All deploy steps live in `scripts/deploy-cloudflare.sh`, which runs the `impresspress` CLI — use one built from the impresspress commit `IMPRESSPRESS_REF` pins:

```bash
ENV_FILE=.env.prod ./scripts/deploy-cloudflare.sh          # build + deploy + /_health gate
ENV_FILE=.env.prod ./scripts/deploy-cloudflare.sh build    # build only
ENV_FILE=.env.prod ./scripts/deploy-cloudflare.sh tail     # stream worker logs
```

A deploy is one command. `impresspress deploy --target cloudflare` deploys the password-hasher Worker (`wafer-site-password-hasher`, `[cloudflare.password_hasher]` in `impresspress.toml`) first, creates the main Worker if there is none, sets its `IMPRESSPRESS_DEPLOY_TOKEN` and `WAFER_RUN__AUTH__JWT_SECRET` secrets when it lacks them, uploads a version without traffic, runs `/_deploy/prepare` on it (pending migrations and seeds), verifies it (including `deploy_smoke_paths`), and only then promotes it. Before any of that, the script refuses to build or deploy unless `../wafer-run` and `../impresspress` are clean checkouts at exactly the `ci.yml` pins. After promotion it checks `https://wafer.run/_health`, which reports every block's config check and the runtime's own health (`runtime`, the body `/health` gives). A 503 with `Retry-After` is asked once more after that delay. The script then rolls back on a fault of the new version: a required key unset, a config source fault, a `failed` runtime, a block that keeps failing its `Init` while D1 answers, or any other non-200 that is not an outage. An outage — a `degraded` or `stale` runtime with no deploy-caused entry, or the Worker's plain-text 503 — says nothing about the new version, so the script exits non-zero and leaves the new version live. `scripts/lib/deploy-checks.sh` lists each rule.

One-time setup on a Cloudflare account: a D1 database (`npx wrangler@4 d1 create wafer-site-prod`, id into the env file as `IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID`), an R2 bucket (`npx wrangler@4 r2 bucket create wafer-site-assets`) and the `CONFIG_CACHE` KV namespace (id in `wrangler.overrides.toml`), plus `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` in the env file. `.env.example`'s Cloudflare section lists them.

### Rolling back

The gate rolls back on anything `health_gate_verdict` (`scripts/lib/deploy-checks.sh`) does not read as healthy or as an outage: a fault the health body names (a required key unset, a config source fault, a `failed` runtime, a block that keeps failing its `Init` while its config answers), but also any status other than 200 or 503 (a 500, an Error 1102 page, curl's `000` for a timeout or no connection), a 503 that is not JSON and carries no `Retry-After`, a 503 health body that names no cause, and JSON that is not the health body. The rule list there gives each rule's reason. On Cloudflare the body's `runtime.backend` says whether D1 failed the isolate's last attempt: `failing` is an outage and keeps the new version, while blocks still failing with `backend` `ok` are the version's own fault.

A rollback (`rollback_to_previous` in deploy-checks.sh) returns to the version at 100% in the deployment before the new one — the version `wrangler rollback` would pick — but deploys it with `wrangler versions deploy`, which fails rather than restoring secrets that changed since that version. It skips an earlier deployment that only re-promoted the live version, refuses a version created before `ROLLBACK_FLOOR_CREATED_ON` (2026-10-30T07:27:07.651Z, when `4c6ff923`, the first version after the 2026-10-30 reset, was created), and afterwards checks that the target is the live version. When it refuses or the rollback fails, the new version stays live: deploy forward from main. Never roll back by hand to a version created before that time.

### First admin on a fresh database

On Cloudflare every setting is a row of D1's variables table, and only an admin can edit one, so a fresh database needs a first admin before anything else is configured:

1. Before the deploy that creates the database's tables, set a one-time bootstrap token as a Worker secret (the main Worker passes it to the auth block, see `bootstrap_admin_config` in `src/lib.rs`):
   ```bash
   token=$(openssl rand -hex 32); echo "$token"   # keep it to redeem
   printf '%s' "$token" | npx wrangler@4 secret put WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN --name wafer-site
   ```
2. Deploy. The first `/_deploy/prepare` installs the token's row (its hash, with a 24-hour expiry). Every later runtime build — `/_deploy/verify`, each isolate — finds that row and leaves it as it is, so the 24 hours run from that first install.
3. Redeem it within those 24 hours: open `https://wafer.run/b/auth/bootstrap`, paste the token, and choose an email you own and a password (the form posts to `/b/auth/api/bootstrap`, which creates the admin and signs you in).
4. Delete the secret: `npx wrangler@4 secret delete WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN --name wafer-site`. This stops the token being installed again on a fresh database; it does **not** revoke a token already installed. Until it is redeemed (or its 24 hours run out), the stored token is itself an admin credential — the auth block accepts it as a bearer for admin routes — so treat it as an admin password, redeem it promptly, and never delete the secret instead of redeeming. Changing the secret does not cancel it either: a different token installs a second row and the first stays redeemable — even after an admin exists, which would mint a second admin — until it expires (a gap tracked in impresspress). Redeeming is the one thing that spends a token, so set one token, once.
5. Signed in as that admin, on the Variables page:
   - `WAFER_RUN_SHARED__FRONTEND_URL` = `https://wafer.run` (it is the access-token issuer, so everyone signs in again);
   - `WAFER_RUN_SHARED__ALLOW_SIGNUP` = `false` (seeded `true`);
   - `WAFER_RUN__REGISTRY__ADMIN_EMAIL` = the publishing account's email (unset, publishing is closed);
   - the branding values (`APP_NAME`, `LOGO_URL`, `LOGO_ICON_URL`, `FAVICON_URL`, `PRIMARY_COLOR`).

`impresspress.toml` holds the per-site Cloudflare config (worker name, D1/R2 bindings, smoke paths, the hasher Worker); `wrangler.overrides.toml` adds the custom-domain routes and the `CONFIG_CACHE` KV binding.

## Layout

- `src/lib.rs` — composition: registers blocks for both targets, runs the native binary
- `src/blocks/registry/` — `wafer-run/registry` block (publish, yank, download, browse, CLI login)
- `src/blocks/content.rs` — serves `dist/` from the storage service each target passes it (local disk natively, R2 on Cloudflare)
- `src/flows/site.rs` — top-level flow + route table
- `content/` — landing page, docs, playground HTML; `build.rs` renders these into `dist/`
- `tests/` — Rust integration tests + Playwright end-to-end suites under `tests/e2e/`

## History

Extracted from `wafer-run/crates/wafer-site/` via `git filter-repo` on 2026-04-22. SHAs differ from the pre-split monorepo, but `git blame` still points at meaningful historical commits.
