//! wafer-site binary entrypoint.
//!
//! All composition lives in the library crate ([`site::run`]); this shell
//! exists solely so `cargo run` works.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wafer_site::run().await
}
