//! Talking to the Claude CLI: finding it, running it safely and reading what it prints.
pub(crate) mod locate;
pub(crate) mod login;
pub(crate) mod parse;
pub(crate) mod runner;
#[cfg(test)]
pub(crate) mod testutil;
