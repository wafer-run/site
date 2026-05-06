//! Site-owned WAFER blocks.
//!
//! - [`content`] serves `dist/**` as static files without going through
//!   solobase's namespaced storage wrapper. Native-only — backed by
//!   `LocalStorageService`, which doesn't compile on wasm32. The
//!   cloudflare target skips it (v1 limitation; tracked as a follow-up
//!   to dispatch through the configured `StorageService` so R2 works).
//! - [`registry`] is the package registry block; works on both targets.

#[cfg(feature = "target-native")]
pub mod content;
pub mod registry;
