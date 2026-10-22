//! The last times Claude Code started a server, from its own logs (decision 013). Starts nothing and writes nothing.
//!
//! Measured with Claude Code 2.1.308: every start of a server, in a session or by `claude mcp get` or `list`, is
//! logged to `<cache>/claude-cli-nodejs/<folder>/mcp-logs-<server>/<time>.jsonl`, where `<folder>` is the folder
//! Claude Code was started in and `<time>` when that Claude Code started (one file per run, a session's tool calls
//! appended to it). Both names are written with every character but `A-Z a-z 0-9` as `-`, and one longer than 200
//! characters is cut there and given a hash of the whole. Each line is JSON: `debug` or `error` text, `timestamp`
//! and `cwd`. Claude Code deletes the files after its clean-up period (30 days unless set).
//!
//! The files are untrusted, can be large and can be written to while read: only names are listed, at most a few files
//! are read, and of a large one only its first and last [`END`] bytes. Only lines Klefki knows are used (a tool call's
//! line can hold what the tool returned): the start, how it ended, and what the server printed on stderr. That text
//! is sanitised, and the server's own environment and header values and anything that looks like a secret are hidden.
use crate::api::{LastStart, LastStarts, StartOutcome};
use crate::config::inventory::normalise_project;
use crate::diagnose::{hints::hints_for, redact::redact, report::STDERR_MARKERS};
use crate::sanitize::{clip, sanitize, sanitize_line};
use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

/// Starts shown: the newest in each folder, newest first.
pub(crate) const MAX_STARTS: usize = 3;
/// Bytes read from each end of a log file.
pub(crate) const END: u64 = 64 * 1024;
/// Folders Claude Code was started in that are looked at; more is not a real machine.
const MAX_FOLDERS: usize = 5_000;
/// Log files of one server in one folder whose names are looked at.
const MAX_FILES: usize = 20_000;
/// Log files read to find [`MAX_STARTS`] starts, when some turn out to be about other folders.
const MAX_READS: usize = 8;
const MAX_SAID: usize = 20;
const MAX_LINE: usize = 300;
/// Claude Code cuts a folder name here (2.1.308).
const MAX_NAME: usize = 200;
/// A value this short is too common to hide wherever it appears (`true`, `8080`).
const MIN_SECRET: usize = 6;

/// Claude Code's log folder in the cache folder `cache` (`directories`' `cache_dir`): `env-paths`' cache folder for
/// `claude-cli`, which on Windows has a `Cache` folder of its own.
pub(crate) fn root(cache: &Path) -> PathBuf {
    let root = cache.join("claude-cli-nodejs");
    if cfg!(windows) {
        root.join("Cache")
    } else {
        root
    }
}

/// A folder or server name as Claude Code writes it into a folder name.
pub(crate) fn folder_name(text: &str) -> String {
    // JavaScript counts and replaces UTF-16 units: a character outside the BMP is two `-`.
    let units: Vec<u16> = text.encode_utf16().collect();
    let named: String = units
        .iter()
        .map(|&u| match u8::try_from(u) {
            Ok(b) if b.is_ascii_alphanumeric() => char::from(b),
            _ => '-',
        })
        .collect();
    if named.len() <= MAX_NAME {
        return named;
    }
    // `Math.abs(h).toString(36)` of the 32-bit `h = (h << 5) - h + unit`.
    let hash = units.iter().fold(0_i32, |h, &u| {
        h.wrapping_shl(5).wrapping_sub(h).wrapping_add(i32::from(u))
    });
    format!("{}-{}", &named[..MAX_NAME], base36(hash.unsigned_abs()))
}

fn base36(mut n: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// What to look for.
pub(crate) struct Ask<'a> {
    /// Claude Code's log folder ([`root`]).
    pub root: &'a Path,
    /// The server's name as the files spell it.
    pub name: &'a str,
    /// A local or project server loads only in this project (and its subfolders): starts elsewhere are another
    /// server's.
    pub within: Option<&'a str>,
    /// Projects where another server of this name loads instead: starts there are that server's.
    pub elsewhere: &'a [String],
    /// To show folders under it as `~`.
    pub home: Option<&'a Path>,
    /// The server's own environment and header values, hidden wherever they appear.
    pub secrets: &'a [String],
    pub now_ms: u64,
    /// The local offset from UTC in seconds at a moment (seconds since 1970).
    pub offset: &'a dyn Fn(i64) -> i64,
}

/// The newest start of the server in each folder, newest first.
pub(crate) fn read(ask: &Ask<'_>) -> LastStarts {
    let mut out = LastStarts {
        looked_in: Some(shown_path(ask.root, ask.home)),
        ..LastStarts::default()
    };
    let logs = format!("mcp-logs-{}", folder_name(ask.name));
    // A subfolder's name starts with its project's, so a local server's starts in a subfolder are found too; the
    // `cwd` in the log decides.
    let prefix = ask.within.map(|p| {
        let name = folder_name(&native(p));
        name[..name.len().min(MAX_NAME)].to_owned()
    });
    let folders = match std::fs::read_dir(ask.root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.problem = Some(format!(
                "Could not read Claude Code's log folder: {}",
                sanitize_line(&e.to_string())
            ));
            return out;
        }
    };
    let mut newest: Vec<(String, PathBuf)> = Vec::new();
    for entry in folders.take(MAX_FOLDERS).flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if prefix.as_deref().is_some_and(|p| !name.starts_with(p)) {
            continue;
        }
        let dir = entry.path().join(&logs);
        if let Some(file) = newest_file(&dir) {
            newest.push((file, dir));
        }
    }
    newest.sort_by(|a, b| b.0.cmp(&a.0));
    for (file, dir) in newest.into_iter().take(MAX_READS) {
        if out.starts.len() == MAX_STARTS {
            break;
        }
        let path = dir.join(&file);
        let Ok((head, tail)) = open(&path).and_then(|(mut f, len)| ends(&mut f, len)) else {
            continue;
        };
        let Some(start) = start_in(&head, tail.as_deref(), ask, &path, &file) else {
            continue;
        };
        out.starts.push(start);
    }
    out
}

/// The newest log file's name in `dir`, by the time in its name.
fn newest_file(dir: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    std::fs::read_dir(dir)
        .ok()?
        .take(MAX_FILES)
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_log_name(n))
        .max()
}

/// `2026-10-22T07-32-16-377Z.jsonl`: these sort by time.
fn is_log_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 30
        && name.ends_with("Z.jsonl")
        && b[..24].iter().enumerate().all(|(i, &c)| match i {
            4 | 7 | 13 | 16 | 19 => c == b'-',
            10 => c == b'T',
            23 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// Opens a log file for reading, never through a link and never something other than a file (a pipe would wait).
fn open(path: &Path) -> io::Result<(File, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok((file, opened.len()))
}

/// The first [`END`] bytes of a file `len` long, and its last [`END`] when it is longer than both: what is read is
/// bounded whatever the size. A file that grows meanwhile is read as it was; one cut short gives what is there.
pub(crate) fn ends<R: Read + Seek>(r: &mut R, len: u64) -> io::Result<(Vec<u8>, Option<Vec<u8>>)> {
    let mut head = Vec::new();
    if len <= 2 * END {
        r.take(2 * END).read_to_end(&mut head)?;
        return Ok((head, None));
    }
    r.take(END).read_to_end(&mut head)?;
    let mut tail = Vec::new();
    r.seek(SeekFrom::Start(len - END))?;
    r.take(END).read_to_end(&mut tail)?;
    Ok((head, Some(tail)))
}

/// One line of a log.
struct Line {
    error: bool,
    text: String,
    at: Option<u64>,
    cwd: Option<String>,
}

/// The lines in `bytes` that are whole JSON objects. A line cut by where reading began or stopped is not.
fn lines(bytes: &[u8], cut_start: bool, out: &mut Vec<Line>) {
    let mut parts = bytes.split(|&b| b == b'\n');
    if cut_start {
        parts.next();
    }
    for part in parts {
        let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(part) else {
            continue;
        };
        let text = |k: &str| o.get(k).and_then(Value::as_str);
        let (error, said) = match (text("error"), text("debug")) {
            (Some(e), _) => (true, e),
            (None, Some(d)) => (false, d),
            _ => continue,
        };
        out.push(Line {
            error,
            text: said.to_owned(),
            at: text("timestamp").and_then(timestamp_ms),
            cwd: text("cwd").map(str::to_owned),
        });
    }
}

static CONNECTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Successfully connected(?: \(transport: [^)]*\))?(?: in (\d+)ms)?")
        .expect("pattern")
});
static FAILED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^Connection failed(?: \(([A-Za-z0-9_]+)\))?: (.*)").expect("pattern")
});
static FAILED_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Connection failed after (\d+)ms").expect("pattern"));

/// The last start in a log read as `head` (and `tail`), when it is the server's own here.
fn start_in(
    head: &[u8],
    tail: Option<&[u8]>,
    ask: &Ask<'_>,
    path: &Path,
    file: &str,
) -> Option<LastStart> {
    let mut all = Vec::new();
    lines(head, false, &mut all);
    if let Some(tail) = tail {
        lines(tail, true, &mut all);
    }
    let begins = all
        .iter()
        .rposition(|l| !l.error && l.text.starts_with("Starting connection"))
        .unwrap_or(0);
    let run = &all[begins..];
    let cwd = run.iter().chain(&all).find_map(|l| l.cwd.as_deref());
    if !belongs(cwd, ask) {
        return None;
    }
    let mut outcome = StartOutcome::Unknown;
    let mut after = None;
    let mut said: Vec<String> = Vec::new();
    for line in run {
        if let Some(c) = CONNECTED.captures(&line.text).filter(|_| !line.error) {
            outcome = StartOutcome::Connected {
                after: millis(c.get(1)),
            };
        } else if let Some(c) = FAILED_AFTER.captures(&line.text) {
            after = millis(c.get(1));
        } else if let Some(c) = FAILED.captures(&line.text).filter(|_| line.error) {
            outcome = StartOutcome::Failed {
                code: c.get(1).map(|m| m.as_str().to_owned()),
                said: clip(sanitize_line(&scrub(&c[2], ask.secrets)).trim(), MAX_LINE),
                after: after.take(),
            };
        } else if let Some(text) = STDERR_MARKERS
            .iter()
            .find_map(|m| line.text.strip_prefix(m))
        {
            for printed in scrub(text, ask.secrets).lines().map(str::trim) {
                let printed = clip(printed, MAX_LINE);
                if !printed.is_empty() && !said.contains(&printed) {
                    said.push(printed);
                }
            }
        }
    }
    // The latest lines tell most.
    said.drain(..said.len().saturating_sub(MAX_SAID));
    let evidence = match &outcome {
        StartOutcome::Failed {
            code, said: msg, ..
        } => {
            format!(
                "{} {msg}\n{}",
                code.as_deref().unwrap_or(""),
                said.join("\n")
            )
        }
        // Lines a server prints on its way up are not a failure.
        _ => String::new(),
    };
    let hints = hints_for(&evidence)
        .into_iter()
        .map(|kind| crate::engine::hint(kind, &evidence))
        .collect();
    let at = run.iter().find_map(|l| l.at).or_else(|| name_ms(file));
    let (when, exact) = match at {
        Some(at) => (
            crate::activity::when(at, ask.now_ms, ask.offset),
            crate::activity::exact(at, ask.offset),
        ),
        None => ("at an unknown time".to_owned(), String::new()),
    };
    Some(LastStart {
        folder: cwd.map_or_else(
            || "an unknown folder".to_owned(),
            |c| shown_path(Path::new(c), ask.home),
        ),
        when,
        exact,
        outcome,
        server_said: said,
        hints,
        partly: tail.is_some(),
        file: shown_path(path, ask.home),
    })
}

/// Whether a start in folder `cwd` is this server's: inside its project for a local or project server, and not in a
/// project where another server of its name loads instead.
fn belongs(cwd: Option<&str>, ask: &Ask<'_>) -> bool {
    let Some(cwd) = cwd else {
        // Nothing to tell by: the folder's name decided where it could.
        return true;
    };
    let inside = |project: &str| {
        let (c, p) = (key(cwd), key(project));
        c == p || c.starts_with(&format!("{}/", p.trim_end_matches('/')))
    };
    ask.within.is_none_or(inside) && !ask.elsewhere.iter().any(|p| inside(p))
}

/// A folder as compared: `/` for `\`, and on Windows in one case.
fn key(path: &str) -> String {
    let p = normalise_project(path);
    if cfg!(windows) { p.to_lowercase() } else { p }
}

/// A project as Claude Code's working folder spells it: with `\` on Windows. Only the hash of a long name sees it.
fn native(project: &str) -> String {
    if cfg!(windows) {
        project.replace('/', "\\")
    } else {
        project.to_owned()
    }
}

/// Untrusted text made safe to show: the server's own values hidden, then anything that looks like a secret.
fn scrub(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|s| s.chars().count() >= MIN_SECRET) {
        if text.contains(secret.as_str()) {
            text = text.replace(secret.as_str(), "***");
        }
    }
    redact(&sanitize(&text))
}

fn millis(m: Option<regex::Match<'_>>) -> Option<Duration> {
    m.and_then(|m| m.as_str().parse().ok())
        .map(Duration::from_millis)
}

/// `2026-10-22T07:32:16.385Z` in milliseconds since 1970.
fn timestamp_ms(ts: &str) -> Option<u64> {
    let b = ts.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| ts.get(r)?.parse::<u32>().ok();
    let ms = if b.get(19) == Some(&b'.') {
        num(20..23)?
    } else {
        0
    };
    to_ms(
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
        ms,
    )
}

/// The time in a log file's name, `2026-10-22T07-32-16-377Z.jsonl`, when the lines carry none.
fn name_ms(name: &str) -> Option<u64> {
    let num = |r: std::ops::Range<usize>| name.get(r)?.parse::<u32>().ok();
    is_log_name(name).then_some(())?;
    to_ms(
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
        num(20..23)?,
    )
}

fn to_ms(y: u32, mo: u32, d: u32, h: u32, mi: u32, s: u32, ms: u32) -> Option<u64> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = crate::activity::days_from_civil(i64::from(y), mo, d);
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
    u64::try_from(secs)
        .ok()?
        .checked_mul(1000)?
        .checked_add(u64::from(ms))
}

/// A path as shown: `~` for the home folder, cleaned and cut.
fn shown_path(path: &Path, home: Option<&Path>) -> String {
    let text = match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => Path::new("~").join(rest).to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    };
    clip(&sanitize_line(&text), crate::sanitize::MAX_SHOWN_CHARS)
}

#[cfg(test)]
mod tests;
