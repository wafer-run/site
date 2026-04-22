//! Site-owned WAFER blocks.
//!
//! - [`content`] serves `dist/**` as static files without going through
//!   solobase's namespaced storage wrapper.
//! - [`registry`] will become the package registry (Task 6+). Currently a
//!   registered-but-empty stub so `/_inspector/blocks` shows it.

pub mod content;
pub mod registry;
