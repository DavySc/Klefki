//! Pre-flight checks: what in a server's definition keeps it from starting, found by reading the definition and
//! looking on the PATH, never by running anything. Claude Code itself reports most of these as a bare "Connection
//! closed": a program that is not installed, a `${VAR}` it passes on as written because nothing sets it (seen with
//! Claude Code 2.1.308), `docker run` without `-i`, a `cmd /c` that Git Bash turned into `cmd C:/`, an empty
//! variable.
//!
//! Looking for a program reads only file metadata. A `.mcp.json` comes from a repository and is untrusted, so for a
//! project server only a bare program name is looked up, in the folders of Klefki's own PATH; a path it names, or a
//! PATH it sets, is never looked at. No definition makes Klefki touch a network path (`\\host\share`, `//host`).
use super::secretlike;
use crate::api::{Preflight, PreflightKind};
use crate::sanitize::{clip, sanitize_line};
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// At most this many names are given in one finding; the rest are counted.
const MAX_NAMED: usize = 5;
const MAX_CHARS: usize = 80;
/// What Windows tries after a program name when `PATHEXT` is not set.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// What the checks look things up in: the environment Klefki was started with, which the Claude Code it starts for
/// a check gets too.
pub(crate) struct Probe<'a> {
    /// Klefki's PATH; `None`: not known, so no program is looked up.
    path: Option<&'a OsStr>,
    vars: &'a dyn Fn(&str) -> Option<OsString>,
    /// Look for programs the way Windows does (`PATHEXT`, no executable bit).
    windows: bool,
    /// Programs already looked up in this reading, by the PATH searched and the name.
    found: RefCell<HashMap<(OsString, String), bool>>,
}

impl<'a> Probe<'a> {
    pub(crate) fn new(
        path: Option<&'a OsStr>,
        vars: &'a dyn Fn(&str) -> Option<OsString>,
        windows: bool,
    ) -> Self {
        Self {
            path,
            vars,
            windows,
            found: RefCell::new(HashMap::new()),
        }
    }

    /// Whether `name`, a program name with no folder in it, is in one of the folders of `path`.
    fn on_path(&self, path: &OsStr, name: &str, pathext: Option<&str>) -> bool {
        let key = (path.to_owned(), name.to_owned());
        if let Some(&known) = self.found.borrow().get(&key) {
            return known;
        }
        let names = self.candidates(name, pathext);
        let found = std::env::split_paths(path)
            .filter(|dir| local_folder(dir))
            .any(|dir| names.iter().any(|n| self.is_program(&dir.join(n))));
        self.found.borrow_mut().insert(key, found);
        found
    }

    /// The file names `name` can be on disk: itself, and on Windows with each of `PATHEXT`'s endings.
    fn candidates(&self, name: &str, pathext: Option<&str>) -> Vec<String> {
        if !self.windows {
            return vec![name.to_owned()];
        }
        let from_vars = (self.vars)("PATHEXT").and_then(|v| v.into_string().ok());
        let pathext = pathext
            .map(str::to_owned)
            .or(from_vars)
            .unwrap_or_else(|| DEFAULT_PATHEXT.to_owned());
        let mut names: Vec<String> = pathext
            .split(';')
            .filter(|ext| ext.starts_with('.') && ext.len() > 1)
            .map(|ext| format!("{name}{ext}"))
            .collect();
        if name.contains('.') {
            names.insert(0, name.to_owned());
        }
        names
    }

    fn is_program(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        if !self.windows {
            use std::os::unix::fs::PermissionsExt;
            return meta.permissions().mode() & 0o111 != 0;
        }
        true
    }
}

/// Whether a folder of a PATH is one to look in: a full path on this machine, not a network one. A relative folder
/// means wherever Klefki happens to be.
fn local_folder(dir: &Path) -> bool {
    dir.is_absolute() && !network_path(&dir.to_string_lossy())
}

/// `\\host\share`, `//host/share`, and Windows' `\\?\`, `\\.\` and NT `\??\` forms (`\??\UNC\host\share`): looking at
/// one can reach another machine.
pub(super) fn network_path(path: &str) -> bool {
    [
        "\\\\", "//", "\\/", "/\\", "\\??\\", "/??/", "\\??/", "/??\\",
    ]
    .iter()
    .any(|start| path.starts_with(start))
}

/// What keeps the server `entry` defines from starting. `trusted`: the definition is in the user's own files, not a
/// repository's `.mcp.json`.
pub(crate) fn findings(entry: &Value, trusted: bool, probe: &Probe<'_>) -> Vec<Preflight> {
    let Some(obj) = entry.as_object() else {
        return Vec::new();
    };
    let kind = obj.get("type").and_then(Value::as_str);
    let mut out = Vec::new();
    if matches!(kind, None | Some("stdio"))
        && let Some(command) = obj.get("command").and_then(Value::as_str)
    {
        let args: Vec<&str> = obj
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let env = obj.get("env").and_then(Value::as_object);
        out.extend(not_found(command, env, trusted, probe));
        out.extend(mangled_switch(command, &args));
        out.extend(no_input(command, &args));
        let mut texts = vec![command];
        texts.extend(&args);
        texts.extend(env.into_iter().flatten().filter_map(|(_, v)| v.as_str()));
        out.extend(unset(&texts, probe));
        out.extend(empty(env));
    } else if matches!(kind, Some("http" | "sse" | "ws")) {
        let mut texts: Vec<&str> = obj.get("url").and_then(Value::as_str).into_iter().collect();
        texts.extend(
            obj.get("headers")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(_, v)| v.as_str()),
        );
        out.extend(unset(&texts, probe));
    }
    out
}

fn finding(kind: PreflightKind, problem: String, fix: &str) -> Preflight {
    Preflight {
        kind,
        problem: clip(&sanitize_line(&problem), 4 * MAX_CHARS),
        fix: sanitize_line(fix),
    }
}

fn shown(text: &str) -> String {
    clip(&sanitize_line(text), MAX_CHARS)
}

/// The program is neither on the PATH nor at the full path written.
fn not_found(
    command: &str,
    env: Option<&Map<String, Value>>,
    trusted: bool,
    probe: &Probe<'_>,
) -> Option<Preflight> {
    // Filled in from the environment: what an unset variable does is said by `unset`.
    if command.contains('$') || command.trim().is_empty() || network_path(command) {
        return None;
    }
    if command.contains(['/', '\\']) {
        // A path from the folder the server starts in is the relative-path hint's; a repository's is never looked at.
        if !trusted || !Path::new(command).is_absolute() {
            return None;
        }
        // On Windows, `C:\tools\srv` runs `srv.exe` or `srv.cmd` as well.
        let names = probe.candidates(command, None);
        if names.iter().any(|n| probe.is_program(Path::new(n))) {
            return None;
        }
        return Some(finding(
            PreflightKind::NotFound,
            format!("{} doesn't exist, or isn't a program.", shown(command)),
            "Correct the path, or install the program there.",
        ));
    }
    // `C:x` names a file on a drive, not a program on the PATH.
    if command.contains(':') {
        return None;
    }
    let own = env.and_then(|env| {
        env.iter()
            .find(|(k, _)| {
                if probe.windows {
                    k.eq_ignore_ascii_case("PATH")
                } else {
                    *k == "PATH"
                }
            })
            .map(|(_, v)| v)
    });
    let pathext = env
        .filter(|_| trusted && probe.windows)
        .and_then(|env| env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATHEXT")))
        .and_then(|(_, v)| v.as_str());
    let (path, where_) = match own {
        // Claude Code looks for the program on the PATH the server is given.
        Some(Value::String(p)) if trusted && !p.contains('$') => {
            (OsString::from(p), "on the PATH this server sets")
        }
        Some(_) => return None,
        None => (
            probe.path?.to_owned(),
            "on the PATH Klefki was started with",
        ),
    };
    if probe.on_path(&path, command, pathext) {
        return None;
    }
    Some(finding(
        PreflightKind::NotFound,
        format!("Can't find {} {where_}.", shown(command)),
        if own.is_some() {
            "Install it in one of those folders, or write its full path."
        } else {
            "Install it, or write its full path. Claude Code looks on its own PATH, which a terminal can set \
             differently."
        },
    ))
}

/// The name of the program `command` runs, without folder or `.exe`, in lower case.
fn program(command: &str) -> String {
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let base = base.to_ascii_lowercase();
    base.strip_suffix(".exe")
        .map_or(base.clone(), str::to_owned)
}

/// Git Bash rewrites `/c` to `C:/` on the way to a Windows program, so `cmd /c npx …` was saved as `cmd C:/ npx …`
/// (and `cmd /s /c` as `cmd S:/ C:/`). Pasting puts them back (`paste.rs`); a definition added from Git Bash itself
/// still has them.
fn mangled_switch(command: &str, args: &[&str]) -> Option<Preflight> {
    if program(command) != "cmd" {
        return None;
    }
    let drives: Vec<&str> = args
        .iter()
        .take_while(|a| crate::paste::git_bash_drive(a).is_some())
        .copied()
        .collect();
    if drives.is_empty() {
        return None;
    }
    let switches: Vec<String> = drives
        .iter()
        .filter_map(|d| crate::paste::git_bash_drive(d))
        .collect();
    let (were, them) = if drives.len() == 1 {
        ("was", "it")
    } else {
        ("were", "them")
    };
    Some(finding(
        PreflightKind::MangledSwitch,
        format!(
            "{} {were} probably {}: Git Bash rewrites /c when claude mcp add runs in it, and cmd then can't run \
             the command.",
            drives.join(" "),
            switches.join(" ")
        ),
        &format!(
            "Edit the arguments and change {them} back to {}.",
            switches.join(" ")
        ),
    ))
}

/// `docker run` without `-i` gives the container no input, so it never reads what Claude Code sends and the
/// connection times out.
fn no_input(command: &str, args: &[&str]) -> Option<Preflight> {
    let tool = program(command);
    if tool != "docker" && tool != "podman" {
        return None;
    }
    let mut words = args.iter().enumerate().filter(|(_, a)| !a.starts_with('-'));
    let run = match words.next() {
        Some((i, &"run")) => i,
        Some((_, &"container")) => match words.next() {
            Some((i, &"run")) => i,
            _ => return None,
        },
        _ => return None,
    };
    // Anywhere after `run`: which options take a value is not known here, and a missed warning is better than a
    // false one.
    if args[run + 1..].iter().any(|a| interactive(a)) {
        return None;
    }
    Some(finding(
        PreflightKind::NoInput,
        format!("{tool} run has no -i, so the container never reads what Claude Code sends."),
        "Add -i after run.",
    ))
}

/// `-i`, `--interactive`, `--interactive=true`, or a group of short options with `i` in it (`-it`, `-dit`).
fn interactive(arg: &str) -> bool {
    if let Some(long) = arg.strip_prefix("--") {
        return long == "interactive" || long == "interactive=true";
    }
    arg.strip_prefix('-').is_some_and(|short| {
        !short.is_empty() && short.chars().all(|c| c.is_ascii_alphabetic()) && short.contains('i')
    })
}

/// `${NAME}` references with no default (`${NAME:-value}` has one) and nothing set under that name. Claude Code
/// passes such a reference on as written.
fn unset(texts: &[&str], probe: &Probe<'_>) -> Option<Preflight> {
    let mut names: Vec<&str> = Vec::new();
    for text in texts {
        let mut rest = *text;
        while let Some(at) = rest.find("${") {
            rest = &rest[at + 2..];
            let Some(end) = rest.find('}') else { break };
            let inner = &rest[..end];
            rest = &rest[end + 1..];
            if inner.is_empty() || inner.contains(":-") || names.contains(&inner) {
                continue;
            }
            if (probe.vars)(inner).is_none() {
                names.push(inner);
            }
        }
    }
    if names.is_empty() {
        return None;
    }
    let refs: Vec<String> = names.iter().map(|n| format!("${{{n}}}")).collect();
    // Klefki knows only its own environment: Claude Code started from a terminal may have them.
    let problem = if names.len() == 1 {
        format!(
            "{} isn't set where Klefki was started. Unless it is set where Claude Code starts, Claude Code passes it \
             on as written.",
            shown(&refs[0])
        )
    } else {
        format!(
            "{} aren't set where Klefki was started. Unless they are set where Claude Code starts, Claude Code \
             passes them on as written.",
            listed(&refs)
        )
    };
    let set = format!(
        "Set {} before you start Claude Code and Klefki",
        if names.len() == 1 { "it" } else { "them" }
    );
    // A default is written in the file as it stands: never suggested for a secret, which belongs in the environment.
    let fix = match names.iter().find(|n| !secret_name(n)) {
        Some(plain) => format!("{set}, or give a default: ${{{}:-value}}.", shown(plain)),
        None => format!("{set}."),
    };
    Some(finding(PreflightKind::UnsetVariable, problem, &fix))
}

/// Whether a variable's name says it holds a secret: as [`secretlike::sounds_secret`] says, or a personal access
/// token (`AZURE_DEVOPS_PAT`), which that leaves out. Only here, where it only holds back a suggestion.
fn secret_name(name: &str) -> bool {
    secretlike::sounds_secret(name)
        || name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w.eq_ignore_ascii_case("pat"))
}

/// Environment variables set to nothing.
fn empty(env: Option<&Map<String, Value>>) -> Option<Preflight> {
    let names: Vec<String> = env
        .into_iter()
        .flatten()
        .filter(|(_, v)| v.as_str().is_some_and(|v| v.trim().is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    let problem = match names.as_slice() {
        [] => return None,
        [one] => format!("{} is empty.", shown(one)),
        many => format!("{} are empty.", listed(many)),
    };
    Some(finding(
        PreflightKind::EmptyValue,
        problem,
        "Give it a value, or remove it if the server doesn't need it.",
    ))
}

/// `a, b and c`, at most [`MAX_NAMED`] by name and the rest counted.
fn listed(names: &[String]) -> String {
    let shown_names: Vec<String> = names.iter().map(|n| shown(n)).collect();
    crate::words::and_list_upto(&shown_names, MAX_NAMED, MAX_NAMED)
}

#[cfg(test)]
mod tests;
