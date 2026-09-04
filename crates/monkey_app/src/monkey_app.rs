//! Facade crate: the app modules live here; domain and integration layers
//! are re-exported from the workspace crates so the public
//! `monkey_app::<module>` paths used by the integration tests stay stable.

pub mod cli;
pub mod webhook;
pub mod worker;

pub use monkey_core::{config, db, dispatch, hmac_auth, sandbox};
pub use monkey_engine::adapters;
pub use monkey_github::{gh_proxy, gh_writeback, host_tools};
