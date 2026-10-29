//! The registry declares its config variables on its `BlockInfo`.
//!
//! The declaration is what the admin Variables page lists under the registry
//! block and what `/_health` validates. The other tests set the keys through
//! the config service, which reads a block-scoped key whether or not it is
//! declared, so they would keep passing without it; this one would not.

use wafer_run::Block;
use wafer_site::blocks::registry::{
    config::{ADMIN_EMAIL_KEY, REQUIRED_AUTH_METHOD_KEY},
    handlers::RegistryBlock,
};

#[test]
fn the_registry_declares_its_two_optional_keys() {
    let info = RegistryBlock.info();
    let declared: Vec<(&str, bool, &str)> = info
        .config_keys
        .iter()
        .map(|var| (var.key.as_str(), var.optional, var.default.as_str()))
        .collect();
    assert_eq!(
        declared,
        vec![
            (ADMIN_EMAIL_KEY, true, ""),
            (REQUIRED_AUTH_METHOD_KEY, true, ""),
        ],
        "both registry keys are declared, optional, and default to empty \
         (publishing closed; any auth method)"
    );
}
