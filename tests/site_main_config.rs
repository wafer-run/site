//! The blocks every static page request runs through read no table-backed
//! config, so the static pages need no D1.
//!
//! A block's `config_keys` are variables-table keys: the block loads them
//! through the config source when it initializes, and on Cloudflare that
//! source is D1. A block that declares one can therefore only become ready
//! while D1 answers. The `wafer-site-main` middleware steps run on every
//! request, and `wafer-site/content` serves `/`, `/docs/` and the rest of the
//! static site, so if any of them declared a key, a D1 outage (the account's
//! daily read quota, 2026-10-30) would take the static pages down with it.
//! The middleware's own settings are step config (`flow_config`), carried by
//! the flow JSON and the router's block config, not by the variables table.
//!
//! This is the site's twin of impresspress's pin on its `site-main` chain
//! (D1-outage plan, I2f). It checks the blocks the binary actually
//! registers, booted as `wafer_site::run` boots them.

mod common;

use wafer_site::flows::site;

/// The blocks a request for a static page passes through: each step of the
/// `wafer-site-main` flow, then the route table's catch-all target.
fn static_path_blocks() -> Vec<String> {
    let flow: serde_json::Value = serde_json::from_str(site::JSON).expect("the flow is JSON");
    let mut blocks: Vec<String> = flow["steps"]
        .as_array()
        .expect("the flow has steps")
        .iter()
        .map(|step| {
            step["block"]
                .as_str()
                .expect("a step names its block")
                .to_string()
        })
        .collect();
    let routes = site::routes();
    let catch_all = routes
        .as_array()
        .expect("the route table is a list")
        .iter()
        .find(|route| route["path"] == "/**")
        .expect("the route table has a catch-all");
    blocks.push(
        catch_all["block"]
            .as_str()
            .expect("the catch-all names its block")
            .to_string(),
    );
    blocks
}

#[tokio::test]
async fn the_static_path_declares_no_table_config() {
    let blocks = static_path_blocks();
    assert!(
        blocks.contains(&wafer_site::blocks::content::NAME.to_string()),
        "the catch-all serves the site content: {blocks:?}"
    );

    let site = common::start_native_site().await;
    let infos = site.wafer.block_infos();
    for name in &blocks {
        let info = infos
            .iter()
            .find(|info| &info.name == name)
            .unwrap_or_else(|| panic!("`{name}` is on the static path but not registered"));
        let keys: Vec<&str> = info
            .config_keys
            .iter()
            .map(|var| var.key.as_str())
            .collect();
        assert!(
            keys.is_empty(),
            "`{name}` is on the static path, so it must read no variables-table \
             config, but it declares {keys:?}"
        );
    }
    site.wafer.shutdown().await;
}
