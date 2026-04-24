# wafer-site

Website, documentation, playground, and (future) block registry for [WAFER](https://github.com/wafer-run/wafer-run).

## Layout expectation

This crate depends on `wafer-run` and `solobase` crates via path deps. Check out all three repos under a common parent:

```
workspace/
├── wafer-run/      # github.com/wafer-run/wafer-run
├── solobase/       # github.com/suppers-ai/solobase
└── site/           # github.com/wafer-run/site (this repo)
```

Then:

```bash
cd site
cargo build
cargo test
npx playwright install --with-deps
npx playwright test
```

Once `wafer-run` crates publish to crates.io, the path deps in `Cargo.toml` will flip to version deps.

## Configuration

Required environment variables:

- `SOLOBASE_SHARED__AUTH__GITHUB__CLIENT_ID`
- `SOLOBASE_SHARED__AUTH__GITHUB__CLIENT_SECRET`
- `SOLOBASE_SHARED__AUTH__GITHUB__REDIRECT_URL`
- `WAFER_RUN__REGISTRY__ADMIN_EMAIL` — email of the user allowed to publish.
- `SUPPERS_AI__AUTH__JWT_SECRET` — session-cookie signing key.

Optional:

- `WAFER_RUN__REGISTRY__STORAGE_KEY_PREFIX` — defaults to `registry`.
- `SOLOBASE_DB_PATH` — SQLite file path (defaults to `data/solobase.db`, per `solobase_native::InfraConfig`).
- `SOLOBASE_STORAGE_ROOT` — local-storage root dir (defaults to `data/storage`).
- `SOLOBASE_LISTEN` — bind address (defaults to `0.0.0.0:8090`).

GitHub is the only OAuth provider enabled. Google and Microsoft are disabled simply by not setting their credential triples (`SOLOBASE_SHARED__AUTH__{GOOGLE,MICROSOFT}__CLIENT_ID` etc.) — the auth block's provider registry only instantiates providers whose credentials are all present.

## History

This repo was extracted from `wafer-run/crates/wafer-site/` via `git filter-repo` on 2026-04-21. SHAs differ from the pre-split monorepo, but `git blame` continues to point at meaningful historical commits.
