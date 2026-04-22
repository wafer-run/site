# wafer-site

Website, documentation, playground, and (future) block registry for [WAFER](https://github.com/wafer-run/wafer-run).

## Layout expectation

This crate depends on `wafer-run` crates via path deps. Check out both repos under a common parent:

```
workspace/
├── wafer-run/      # github.com/wafer-run/wafer-run
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

## History

This repo was extracted from `wafer-run/crates/wafer-site/` via `git filter-repo` on 2026-04-21. SHAs differ from the pre-split monorepo, but `git blame` continues to point at meaningful historical commits.
