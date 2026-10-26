//! What Claude Code starts for a stdio server: the program, its arguments, the folder and the environment, built the
//! way Claude Code builds them, so that Klefki can start the server itself to check it (`mcp/stdio.rs`). Nothing here
//! starts anything or writes anything.
//!
//! Measured with Claude Code 2.1.310 on Linux (`docs/proposals/checks-without-claude.md`). The environment, later
//! wins:
//! 1. Klefki's own environment, without `MCP_CLIENT_SECRET` and what Claude Code takes out: its own credentials
//!    (`CLAUDE_CODE_OAUTH_TOKEN` and the file-descriptor variables) and every `OTEL_*`.
//! 2. The `env` of the settings files, in Claude Code's order: the user's `settings.json`, then, only when the
//!    project's trust dialog was accepted, its `.claude/settings.json` and `.claude/settings.local.json`, then the
//!    organisation's managed settings. A value that is not text is passed as JavaScript's `String()` writes it
//!    (`5`, `true`, `null`, `[object Object]`).
//! 3. Claude Code's own: `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `AI_AGENT`, `CLAUDE_PROJECT_DIR`,
//!    `CLAUDE_CODE_SESSION_ID`, `NoDefaultCurrentDirectoryInExePath`, `COREPACK_ENABLE_AUTO_PIN`, and on Linux
//!    `LC_CTYPE` unless `LC_ALL` is set.
//! 4. The definition's `env`.
//!
//! `${NAME}` and `${NAME:-default}` in the command, the arguments and the definition's `env` are filled in from 1
//! and 2 only (`${CLAUDECODE:-no}` gives `no`). A name is a letter or `_`, then letters, digits and `_`; anything
//! else, like an unset name with no default, stays as written. A set name wins over its default even when empty.
//! The default runs to the first `}` and is not filled in itself.
//!
//! The program is looked up on the PATH the server is given, never in the working folder (a repository could put an
//! `npx` there). On Windows a `.cmd` or `.bat` needs `cmd.exe`, so such a server is left to Claude Code.
//!
//! The settings files are untrusted: size-limited, and only their `env` is read.
use super::{ConfigPaths, fsx, inventory, managed};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = inventory::MAX_MCP_JSON;
/// Taken out of Klefki's environment: the client secret Klefki passes only to `claude mcp add-json`, and what Claude
/// Code removes before it starts a server (measured: also every `OTEL_*`, in any case).
const REMOVED: [&str; 6] = [
    "MCP_CLIENT_SECRET",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_WEBSOCKET_AUTH_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
];
/// What Windows tries after a program name when `PATHEXT` is not set.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The system whose rules apply: how names compare, how programs are found. Tests pick one to try another's rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    pub(crate) const HERE: Self = if cfg!(windows) {
        Self::Windows
    } else if cfg!(target_os = "macos") {
        Self::Mac
    } else {
        Self::Linux
    };
}

/// Whether Claude Code reads the project's own settings files in `project`, a key of `projects` in `.claude.json`:
/// only once its trust dialog was accepted there. A parent folder's trust does not count (measured).
pub(crate) fn trusted(claude_json: &Value, project: &str) -> bool {
    claude_json
        .get("projects")
        .and_then(|p| p.get(project))
        .and_then(|p| p.get("hasTrustDialogAccepted"))
        == Some(&Value::Bool(true))
}

/// The settings files' `env`, in the order they apply (later wins). `trusted_project`: a project folder whose trust
/// dialog was accepted ([`trusted`]); `None` for a user server or an untrusted project.
pub(crate) fn settings_env(
    paths: &ConfigPaths,
    trusted_project: Option<&Path>,
) -> Vec<(String, String)> {
    let user = paths.claude_dir.join("settings.json");
    let mut files = vec![user.clone()];
    if let Some(folder) =
        trusted_project.filter(|f| inventory::is_safe_project_path(&f.to_string_lossy()))
    {
        for name in ["settings.json", "settings.local.json"] {
            let file = folder.join(".claude").join(name);
            // From the home folder, the project's settings can be the user's own file.
            if file != user {
                files.push(file);
            }
        }
    }
    if let Some(dir) = &paths.managed_dir {
        files.extend(managed::settings_files(dir, &mut Vec::new()));
    }
    files.iter().flat_map(|f| env_of(f)).collect()
}

/// The `env` object of one settings file, each value as text.
fn env_of(file: &Path) -> Vec<(String, String)> {
    let Some(text) = fsx::read_limited(file, MAX_FILE).ok().flatten() else {
        return Vec::new();
    };
    let Ok(Value::Object(mut doc)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    match doc.remove("env") {
        Some(Value::Object(env)) => env.into_iter().map(|(k, v)| (k, js_string(&v))).collect(),
        _ => Vec::new(),
    }
}

/// `value` as JavaScript's `String(value)` writes it, which is what a settings value becomes in the environment.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            // A whole number below 1e21 has no fraction in JavaScript: `1.0` is `1`.
            // `-0` too is `0`.
            Some(f) if n.is_f64() && f == 0.0 => "0".to_owned(),
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        Value::Object(_) => "[object Object]".to_owned(),
        // An array's items are joined with commas, `null` as nothing.
        Value::Array(items) => items
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

/// Environment variables in order, a later one replacing an earlier one of the same name. Windows compares names
/// without regard to case.
struct Env {
    os: Os,
    vars: Vec<(OsString, OsString)>,
}

impl Env {
    fn set(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) {
        let name = name.into();
        let value = value.into();
        match self
            .vars
            .iter()
            .position(|(n, _)| same_name(self.os, n, &name))
        {
            Some(at) => self.vars[at] = (name, value),
            None => self.vars.push((name, value)),
        }
    }

    fn remove(&mut self, name: &str) {
        let os = self.os;
        self.vars
            .retain(|(n, _)| !same_name(os, n, OsStr::new(name)));
    }

    fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars
            .iter()
            .find(|(n, _)| same_name(self.os, n, OsStr::new(name)))
            .map(|(_, v)| v.as_os_str())
    }
}

fn same_name(os: Os, a: &OsStr, b: &OsStr) -> bool {
    if os == Os::Windows {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

/// What a server is started from.
pub(crate) struct Inputs<'a> {
    /// The definition, as the inventory resolved it (the one Claude Code loads).
    pub entry: &'a Value,
    /// Where Claude Code would start it: the home folder for a user server, the project for a local one.
    pub folder: &'a Path,
    /// Klefki's own environment.
    pub klefki_env: &'a [(OsString, OsString)],
    /// [`settings_env`].
    pub settings: &'a [(String, String)],
    /// `claude --version`'s number, for `AI_AGENT`; `None`: not known, so it is not set.
    pub claude_version: Option<&'a str>,
    /// A new id for `CLAUDE_CODE_SESSION_ID` ([`new_session_id`]).
    pub session_id: &'a str,
    pub os: Os,
}

/// A server ready to start: no shell, the program found, every argument its own argv entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment: the child gets this and nothing else.
    pub env: Vec<(OsString, OsString)>,
    /// Values that must not show in what the server prints: the definition's own `env` values, as written and
    /// filled in, and the settings files' values.
    pub hidden: Vec<String>,
}

/// Why a server is not started directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotStarted {
    /// Not a stdio server, or no command: Claude Code checks it.
    NotStdio,
    /// The program needs a shell to run (on Windows a `.cmd` or `.bat`, or a file Windows does not run itself):
    /// Claude Code checks it.
    NeedsShell,
    /// The program was not found, as written (sanitised).
    NotFound(String),
    /// The file found is no program the system starts by itself (no `#!` line), as written (sanitised).
    NotProgram(String),
}

/// The server `inputs.entry` defines, as Claude Code would start it.
pub(crate) fn launch(inputs: &Inputs<'_>) -> Result<Launch, NotStarted> {
    let obj = inputs.entry.as_object().ok_or(NotStarted::NotStdio)?;
    if !matches!(
        obj.get("type").and_then(Value::as_str),
        None | Some("stdio")
    ) {
        return Err(NotStarted::NotStdio);
    }
    let command = obj
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or(NotStarted::NotStdio)?;

    let mut env = Env {
        os: inputs.os,
        vars: inputs.klefki_env.to_vec(),
    };
    // A secret in Klefki's environment must not reach a child that was not given it on purpose, nor what Claude
    // Code keeps from its servers.
    for name in REMOVED {
        env.remove(name);
    }
    env.vars.retain(|(n, _)| {
        !n.to_string_lossy()
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case("OTEL_"))
    });
    for (name, value) in inputs.settings {
        env.set(name, value);
    }
    let lookup = Env {
        os: inputs.os,
        vars: env.vars.clone(),
    };
    let fill = |text: &str| {
        expand(text, &|name| {
            lookup.get(name).map(|v| v.to_string_lossy().into_owned())
        })
    };

    let folder = inputs.folder.to_string_lossy().into_owned();
    env.set("CLAUDECODE", "1");
    env.set("CLAUDE_CODE_ENTRYPOINT", "cli");
    if let Some(version) = inputs.claude_version {
        env.set(
            "AI_AGENT",
            format!("claude-code_{}_harness", version.replace('.', "-")),
        );
    }
    env.set("CLAUDE_PROJECT_DIR", &folder);
    env.set("CLAUDE_CODE_SESSION_ID", inputs.session_id);
    env.set("NoDefaultCurrentDirectoryInExePath", "1");
    env.set("COREPACK_ENABLE_AUTO_PIN", "0");
    if inputs.os == Os::Linux && env.get("LC_ALL").is_none() {
        env.set("LC_CTYPE", "C.UTF-8");
    }

    let mut hidden: Vec<String> = inputs.settings.iter().map(|(_, v)| v.clone()).collect();
    if let Some(Value::Object(own)) = obj.get("env") {
        for (name, value) in own {
            let written = js_string(value);
            let filled = fill(&written);
            env.set(name, &filled);
            hidden.push(filled);
            hidden.push(written);
        }
    }
    let args: Vec<String> = obj
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| fill(&js_string(v))).collect())
        .unwrap_or_default();
    let command = fill(command);
    let program = find(&command, inputs.folder, &env)?;
    hidden.retain(|v| !v.is_empty());
    hidden.sort();
    hidden.dedup();
    Ok(Launch {
        program,
        args,
        cwd: inputs.folder.to_path_buf(),
        env: env.vars,
        hidden,
    })
}

/// `${NAME}` and `${NAME:-default}` in `text` filled in from `var`, the way Claude Code does it.
pub(crate) fn expand(text: &str, var: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in pieces(text) {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Reference(r) => match (var(r.name), r.default) {
                (Some(value), _) => out.push_str(&value),
                (None, Some(default)) => out.push_str(default),
                (None, None) => out.push_str(r.written),
            },
        }
    }
    out
}

/// A `${NAME}` or `${NAME:-default}` that Claude Code fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reference<'a> {
    pub name: &'a str,
    /// The text after `:-`, up to the first `}`.
    pub default: Option<&'a str>,
    /// The whole reference, as written.
    pub written: &'a str,
}

/// The references in `text` that Claude Code fills in, in order: what [`expand`] replaces and pre-flight looks up.
pub(crate) fn references(text: &str) -> impl Iterator<Item = Reference<'_>> {
    pieces(text).into_iter().filter_map(|p| match p {
        Piece::Reference(r) => Some(r),
        Piece::Text(_) => None,
    })
}

enum Piece<'a> {
    /// Kept as written, `${…}` that is no reference to fill in included.
    Text(&'a str),
    Reference(Reference<'a>),
}

/// `text` cut into what is kept and the references to fill in.
fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        let inner = &after[..end];
        let (name, default) = match inner.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (inner, None),
        };
        let written = &rest[at..at + 2 + end + 1];
        if is_name(name) {
            out.push(Piece::Text(&rest[..at]));
            out.push(Piece::Reference(Reference {
                name,
                default,
                written,
            }));
        } else {
            out.push(Piece::Text(&rest[..at + written.len()]));
        }
        rest = &after[end + 1..];
    }
    out.push(Piece::Text(rest));
    out
}

/// A name Claude Code fills in: a letter or `_`, then letters, digits and `_` (`${1X}`, `${HO-ME}` and `${ A }` stay
/// as written).
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The program `command` names: a path (relative to `folder`), or a bare name looked up in the folders of `env`'s
/// PATH, never in the working folder.
fn find(command: &str, folder: &Path, env: &Env) -> Result<PathBuf, NotStarted> {
    let windows = env.os == Os::Windows;
    let not_found = || NotStarted::NotFound(crate::sanitize::shown(command));
    let has_folder = command.contains('/') || (windows && command.contains(['\\', ':']));
    let names = candidates(command, env);
    let dirs: Vec<PathBuf> = if has_folder {
        vec![folder.to_path_buf()]
    } else {
        env.get("PATH")
            .map(|path| {
                split_path(env.os, path)
                    .filter(|d| local_folder(d))
                    .collect()
            })
            .unwrap_or_default()
    };
    for dir in dirs {
        for name in &names {
            let path = dir.join(name);
            if !runnable(&path, windows) {
                continue;
            }
            if windows && !native_program(&path) {
                return Err(NotStarted::NeedsShell);
            }
            // Started by name, a file with no `#!` line would run through `/bin/sh` (glibc's and macOS's `execvp`);
            // Claude Code refuses it (`ENOEXEC`).
            if !windows && !starts_as_program(&path) {
                return Err(NotStarted::NotProgram(crate::sanitize::shown(command)));
            }
            return Ok(path);
        }
    }
    Err(not_found())
}

/// The folders of a PATH: split at `;` on Windows (a folder may be in quotes), at `:` elsewhere.
fn split_path(os: Os, path: &OsStr) -> Box<dyn Iterator<Item = PathBuf> + '_> {
    if os == Os::Windows {
        let path = path.to_string_lossy().into_owned();
        let dirs: Vec<PathBuf> = path
            .split(';')
            .map(|d| PathBuf::from(d.trim_matches('"')))
            .collect();
        Box::new(dirs.into_iter())
    } else {
        Box::new(std::env::split_paths(path))
    }
}

/// The file names `command` can be on disk: itself, and on Windows with each of `PATHEXT`'s endings.
fn candidates(command: &str, env: &Env) -> Vec<String> {
    if env.os != Os::Windows {
        return vec![command.to_owned()];
    }
    let pathext = env
        .get("PATHEXT")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_PATHEXT.to_owned());
    let mut names: Vec<String> = pathext
        .split(';')
        .filter(|ext| ext.starts_with('.') && ext.len() > 1)
        .map(|ext| format!("{command}{ext}"))
        .collect();
    if Path::new(command).extension().is_some() {
        names.insert(0, command.to_owned());
    }
    names
}

/// Whether the file begins as one the system starts itself: a `#!` line, or an ELF or Mach-O program.
fn starts_as_program(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::with_capacity(4);
    let read = std::fs::File::open(path).and_then(|f| f.take(4).read_to_end(&mut head));
    if read.is_err() {
        return false;
    }
    head.starts_with(b"#!")
        || head == b"\x7fELF"
        || [0xfeed_face_u32, 0xfeed_facf, 0xcafe_babe]
            .iter()
            .any(|m| head == m.to_be_bytes() || head == m.to_le_bytes())
}

/// A `.exe` or `.com`, which Windows starts without `cmd.exe`.
fn native_program(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"))
}

fn runnable(path: &Path, windows: bool) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    if !windows {
        use std::os::unix::fs::PermissionsExt;
        return meta.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    let _ = windows;
    true
}

/// A folder of a PATH to look in: a full path on this machine. A relative one (`.`, or an empty entry) means the
/// working folder; a network one could reach another machine.
fn local_folder(dir: &Path) -> bool {
    dir.is_absolute() && !super::preflight::network_path(&dir.to_string_lossy())
}

/// A new id in the form of Claude Code's `CLAUDE_CODE_SESSION_ID` (a random UUID).
pub(crate) fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let half = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(salt);
        h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
        h.write_u32(std::process::id());
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.finish()
    };
    let n = (u128::from(half(1)) << 64) | u128::from(half(2));
    // Version 4, variant 1.
    let n = (n & !(0xf_u128 << 76)) | (0x4_u128 << 76);
    let n = (n & !(0x3_u128 << 62)) | (0x2_u128 << 62);
    let hex = format!("{n:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests;
