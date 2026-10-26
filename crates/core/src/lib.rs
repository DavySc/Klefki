//! All IO lives here. The UI crate may only use [`api`].
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "tests build sizes and seeded values with `as`; the casts in product code are checked"
    )
)]
mod activity;
mod adjust;
pub mod api;
mod cli;
mod config;
mod desktop;
mod diagnose;
mod engine;
mod export;
mod export_file;
mod fixtures;
mod form;
mod hex;
mod import_file;
mod inspector;
mod install;
mod last_start;
// The direct check, which the engine takes up in the next slice (`docs/proposals/checks-without-claude.md`).
#[cfg(test)]
mod mcp;
mod model;
mod ops;
mod paste;
mod profile_uses;
mod profiles;
mod restore;
mod sanitize;
mod secret_uses;
mod secrets;
mod start;
mod swap;
mod undo;
mod update;
mod watch;
mod words;
