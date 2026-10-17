//! All IO lives here. The UI crate may only use [`api`].
pub mod api;
mod cli;
mod config;
mod engine;
mod fixtures;
mod model;
mod sanitize;
