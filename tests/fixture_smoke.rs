//! Smoke-test for the Playwright e2e tarball fixture.
//!
//! Ensures tests/e2e/_fixtures/widget-0.1.0.wafer still passes the registry
//! tarball validator — so that the `admin_publish_via_post_then_browse_appears`
//! Playwright test (when gated-on by TEST_ADMIN_TOKEN) has a valid payload.

#[test]
fn fixture_tarball_validates() {
    let bytes = std::fs::read("tests/e2e/_fixtures/widget-0.1.0.wafer")
        .expect("read fixture");
    let t = wafer_site::blocks::registry::tarball::parse_and_validate(&bytes)
        .expect("parse fixture");
    assert_eq!(t.wafer_toml.package.org, "acme");
    assert_eq!(t.wafer_toml.package.name, "widget");
    assert_eq!(t.wafer_toml.package.version, "0.1.0");
}
