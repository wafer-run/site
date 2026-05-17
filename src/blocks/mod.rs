//! Site-owned WAFER blocks.
//!
//! - [`content`] serves the SPA chrome from a configured
//!   [`StorageService`]. Native wires a `LocalStorageService` rooted at
//!   `<repo>/dist`; cloudflare wires the R2-backed service from
//!   `solobase-cloudflare` (folder=`"dist"` to match the deploy upload
//!   prefix). Same code on both targets.
//! - [`registry`] is the package registry block; works on both targets.
//! - [`health`] backs `GET /_health` with a config-validation summary
//!   used by the deploy script to gate post-deploy rollback.

pub mod content;
pub mod health;
pub mod registry;
