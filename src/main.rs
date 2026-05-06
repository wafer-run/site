//! wafer-site binary entrypoint.
//!
//! All composition lives in the library crate ([`wafer_site::run`]); this
//! shell exists solely so `cargo run` works. Native-only — the cloudflare
//! target produces a cdylib and uses `wafer_site::fetch_main` directly.

#![cfg(feature = "target-native")]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wafer_site::run().await
}
