//! The parts of Vault every other crate needs: typed configuration and the error taxonomy.
//!
//! Deliberately no tokio, no axum and no sqlx here: configuration and error types are compiled
//! into every crate, so their dependency graph stays the cheapest one in the workspace.

pub mod config;
pub mod delivery;
pub mod error;
pub mod planner;
pub mod target_state;
