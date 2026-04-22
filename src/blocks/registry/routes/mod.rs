//! Route handler modules for the registry block.
//!
//! Each module implements one or more endpoints. All return 501 (Not Implemented)
//! until their respective Task lands.

pub mod browse;
pub mod cli_login;
pub mod download;
pub mod me;
pub mod packages;
pub mod publish;
pub mod yank;
