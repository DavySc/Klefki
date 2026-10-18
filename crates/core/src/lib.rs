//! All IO lives here. The UI crate may only use [`api`].
pub mod api;
mod cli;
mod config;
mod diagnose;
mod engine;
mod fixtures;
mod form;
mod model;
mod ops;
mod paste;
mod sanitize;
mod secrets;
mod watch;
