//! How long Claude Code waits for an MCP server to connect (`MCP_TIMEOUT`), and where that is set.
//!
//! Measured with Claude Code 2.1.308: 30 s, unless `MCP_TIMEOUT` says otherwise in milliseconds. The value is read
//! the way JavaScript's `parseInt` reads it: spaces, then digits up to the first other character (`3000x` is 3000,
//! `2.5e3` is 2, `60s` is 60); one that gives no number above 0 (`abc`, `0`, `-5`, nothing) leaves the 30 s. `env`
//! in the settings files wins over the environment Claude Code is started with, and among those the organisation's
//! files come first, then the project's `.claude/settings.local.json`, its `.claude/settings.json`, and the user's
//! `settings.json`. The first place that sets it decides, also when its value is not read as a time (`null`, `{}`
//! and `"abc"` in the user's file all gave 30 s over `MCP_TIMEOUT=2000` in the environment).
//!
//! The files are untrusted: size-limited, and only `env.MCP_TIMEOUT` is read from them.
use super::{ConfigPaths, fsx, inventory, managed, policy};
use crate::api::{LimitSetIn, TimeLimit};
use serde_json::Value;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// Claude Code's own limit.
pub(crate) const DEFAULT: Duration = Duration::from_secs(30);
const MAX_FILE: u64 = inventory::MAX_MCP_JSON;
const VAR: &str = "MCP_TIMEOUT";

/// The limit for a Claude Code started in `folder`, with the environment `var` looks names up in.
pub(crate) fn limit(
    paths: Option<&ConfigPaths>,
    folder: Option<&Path>,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> TimeLimit {
    let mut files: Vec<(std::path::PathBuf, LimitSetIn)> = Vec::new();
    if let Some(dir) = paths.and_then(|p| p.managed_dir.as_deref()) {
        for file in managed::settings_files(dir, &mut Vec::new()) {
            let shown = policy::home_as_tilde(&file);
            files.push((file, LimitSetIn::Organisation(shown)));
        }
    }
    let user = paths.map(|p| p.claude_dir.join("settings.json"));
    if let Some(folder) = folder.filter(|f| inventory::is_safe_project_path(&f.to_string_lossy())) {
        for name in ["settings.local.json", "settings.json"] {
            let file = folder.join(".claude").join(name);
            // From the home folder, the project's settings can be the user's own file.
            if user.as_deref() != Some(file.as_path()) {
                let shown = policy::home_as_tilde(&file);
                files.push((file, LimitSetIn::File(shown)));
            }
        }
    }
    let user = user.map(|user| {
        let shown = policy::home_as_tilde(&user);
        files.push((user, LimitSetIn::File(shown.clone())));
        shown
    });
    for (file, set_in) in files {
        if let Some(value) = in_file(&file) {
            return read(value.as_deref(), set_in, user);
        }
    }
    match var(VAR) {
        Some(value) => read(value.to_str(), LimitSetIn::Environment, user),
        None => read(None, LimitSetIn::Default, user),
    }
}

/// `env.MCP_TIMEOUT` of a settings file: `None` when the file does not set it, `Some(None)` when it sets it to
/// something that is no text.
fn in_file(file: &Path) -> Option<Option<String>> {
    let text = fsx::read_limited(file, MAX_FILE).ok()??;
    let doc: Value = serde_json::from_str(&text).ok()?;
    match doc.get("env")?.get(VAR)? {
        Value::String(s) => Some(Some(s.clone())),
        Value::Number(n) => Some(Some(n.to_string())),
        Value::Bool(b) => Some(Some(b.to_string())),
        _ => Some(None),
    }
}

/// The limit `value` sets, set in `set_in`; `user` is the user's settings file as a message names it.
fn read(value: Option<&str>, set_in: LimitSetIn, user: Option<String>) -> TimeLimit {
    let ms = value.and_then(parse_int).filter(|&ms| ms > 0);
    let change_in = match &set_in {
        LimitSetIn::File(file) => Some(file.clone()),
        LimitSetIn::Organisation(_) => None,
        // The user's file wins over the environment, and applies in every folder.
        LimitSetIn::Default | LimitSetIn::Environment => user,
    };
    TimeLimit {
        limit: ms.map_or(DEFAULT, Duration::from_millis),
        unreadable: ms.is_none() && set_in != LimitSetIn::Default,
        set_in,
        change_in,
    }
}

/// A whole number the way JavaScript's `parseInt` finds one at the start of `text`; `None` where it finds none.
fn parse_int(text: &str) -> Option<u64> {
    let text = text.trim_start();
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    if negative {
        // Below 0: no limit Claude Code uses.
        return Some(0);
    }
    Some(digits[..end].bytes().fold(0u64, |n, d| {
        n.saturating_mul(10).saturating_add(u64::from(d - b'0'))
    }))
}

#[cfg(test)]
mod tests;
