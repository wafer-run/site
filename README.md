# wafer-site

The website, docs, playground, and package registry behind [wafer.run](https://wafer.run).

Built on [WAFER](https://github.com/wafer-run/wafer-run) + [solobase](https://github.com/suppers-ai/solobase) — both must be checked out as siblings (path deps in `Cargo.toml`):

```
workspace/
├── wafer-run/
├── solobase/
└── site/        # this repo
```

## Run it locally

```bash
cp .env.example .env       # then fill in JWT secret + admin email
cargo run                  # listens on http://localhost:8090
```

`.env.example` documents every variable the binary reads. The defaults work for a local dev run; you only have to fill in:

- `SUPPERS_AI__AUTH__JWT_SECRET` — any random string
- `SUPPERS_AI__AUTH__ADMIN_EMAIL` and `WAFER_RUN__REGISTRY__ADMIN_EMAIL` — your email
- The `*GITHUB*` triple if you want OAuth login

## Tests

```bash
cargo test                            # Rust unit + integration tests
npx playwright install --with-deps    # one-time
npx playwright test                   # browser end-to-end tests
```

Playwright spins the binary on port 8090 and drives Chromium against it.

## Deploy to Cloudflare

The crate also builds as a `wasm32-unknown-unknown` cdylib for Cloudflare Workers via the `target-cloudflare` feature. All deploy steps live in `scripts/deploy-cloudflare.sh`:

```bash
./scripts/deploy-cloudflare.sh             # build + wrangler deploy + R2 upload
./scripts/deploy-cloudflare.sh build       # build only
./scripts/deploy-cloudflare.sh secret      # set the JWT worker secret
./scripts/deploy-cloudflare.sh tail        # stream worker logs
```

First-time setup needs Workers + D1 + R2 on a Cloudflare account, plus:

1. `wrangler d1 create wafer-site-prod` — copy the printed id into `.env` as `SOLOBASE_CLOUDFLARE_D1_DATABASE_ID`
2. `wrangler r2 bucket create wafer-site-assets`
3. Fill in `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` in `.env`
4. `./scripts/deploy-cloudflare.sh`

`solobase.toml` holds per-site Cloudflare config (worker name, D1/R2 bindings); `wrangler.overrides.toml` adds the custom-domain routes. v1 of the cloudflare deploy serves solobase + registry routes only — the static SPA chrome stays native-only until the content block is refactored to read through the configured storage service.

## Layout

- `src/lib.rs` — composition: registers blocks for both targets, runs the native binary
- `src/blocks/registry/` — `wafer-run/registry` block (publish, yank, download, browse, CLI login)
- `src/blocks/content.rs` — native-only static-file block that serves `dist/`
- `src/flows/site.rs` — top-level flow + route table
- `content/` — landing page, docs, playground HTML; `build.rs` renders these into `dist/`
- `tests/` — Rust integration tests + Playwright end-to-end suites under `tests/e2e/`

## History

Extracted from `wafer-run/crates/wafer-site/` via `git filter-repo` on 2026-04-22. SHAs differ from the pre-split monorepo, but `git blame` still points at meaningful historical commits.
