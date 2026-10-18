//! Writes to `~/.claude.json`, next to a running Claude Code (`docs/decisions/001-claude-json-locking.md`), and to
//! a project's `.claude/settings.local.json` when the user approves one of its servers.
//!
//! Every change is a [`Plan`] made from [`Op`]s: `prepare` shows what would change, `apply` re-reads the file under
//! Claude's own lock directory, re-applies the ops to that fresh copy, backs the old bytes up, and replaces the file
//! atomically. Key order, unknown keys and untouched values survive because the ops patch the parsed document.
//! Claude may still write without the lock (it gives up waiting after about 1.5 s), so `verify` re-reads a moment
//! later and re-applies if our change is gone.
use super::inventory::{MAX_CLAUDE_JSON, normalise_project};
use super::{entry, fsx, jsonspan};
use crate::sanitize::sanitize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Claude's own staleness rule for its lock directory.
const LOCK_STALE: Duration = Duration::from_secs(10);
const BACKOFF_MS: [u64; 6] = [50, 100, 200, 400, 800, 1500];
const MAX_VERIFY_ROUNDS: usize = 3;
const BOM: &str = "\u{feff}";

/// One edit to the document. Idempotent: applying it twice changes nothing the second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    /// Put `name` in, or take it out of, `projects[<project>].disabledMcpServers`. `project` is a path in any
    /// spelling; every spelling of it present in the file is updated. The project entry must already exist.
    SetDisabled {
        project: String,
        name: String,
        disabled: bool,
    },
    /// Take `name` out of `projects[<project>].disabledMcpjsonServers` (a rejection recorded in `.claude.json`),
    /// for every spelling of the project. Needed so that an approval is not overruled by an old rejection.
    ClearRejection { project: String, name: String },
    /// For the project's `.claude/settings.local.json`: add `name` to `enabledMcpjsonServers` and remove it from
    /// `disabledMcpjsonServers`. The only thing klefki ever writes inside a repository.
    Approve { name: String },
    /// Delete the server `name`: from the top-level `mcpServers` (`project` is `None`), or from
    /// `projects[<project>].mcpServers` in every spelling of the project that has it.
    RemoveServer {
        project: Option<String>,
        name: String,
    },
    /// Replace the definition of server `name` (same places as [`Op::RemoveServer`]). Environment and header
    /// values given as [`entry::MASK`] keep the value they had.
    ReplaceServer {
        project: Option<String>,
        name: String,
        entry: Value,
    },
}

impl Op {
    /// The project this op changes inside, if any.
    fn project(&self) -> Option<&str> {
        match self {
            Op::SetDisabled { project, .. } | Op::ClearRejection { project, .. } => Some(project),
            Op::RemoveServer { project, .. } | Op::ReplaceServer { project, .. } => {
                project.as_deref()
            }
            Op::Approve { .. } => None,
        }
    }

    fn is_server_op(&self) -> bool {
        matches!(self, Op::RemoveServer { .. } | Op::ReplaceServer { .. })
    }

    fn server_name(&self) -> &str {
        match self {
            Op::RemoveServer { name, .. } | Op::ReplaceServer { name, .. } => name,
            Op::SetDisabled { name, .. }
            | Op::ClearRejection { name, .. }
            | Op::Approve { name } => name,
        }
    }
}

/// Which file a [`Docstore`] writes, and so which ops it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    ClaudeJson,
    /// `<project>/.claude/settings.local.json`, created when missing.
    ProjectSettings,
}

/// What a plan changes in one file, in words that are safe to show: names are sanitised and nothing else from the
/// file (in particular no server settings) is included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub path: String,
    /// Header lines start with two spaces, additions with `+ `, removals with `- `.
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    ops: Vec<Op>,
    /// Hash of the file the plan was made from.
    base_hash: String,
    changes: Vec<FileChange>,
}

impl Plan {
    pub(crate) fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    /// Nothing would be written.
    pub(crate) fn is_empty(&self) -> bool {
        self.changes.iter().all(|c| c.lines.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoreError {
    /// Claude Code (or another klefki) holds the lock; nothing was written.
    Locked,
    /// The file changed since the plan was shown, in a way that alters what the user approved.
    Changed,
    /// Our change was overwritten and could not be put back.
    Lost,
    /// The file or an op is not in the shape we expect; nothing was written.
    Invalid(String),
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locked => write!(
                f,
                "Claude Code is writing its configuration right now. Nothing was changed; try again in a moment."
            ),
            Self::Changed => write!(
                f,
                "The configuration changed since you looked at this. Nothing was changed; review the change again."
            ),
            Self::Lost => write!(
                f,
                "Claude Code overwrote the change again and again. Close running Claude Code sessions and retry."
            ),
            Self::Invalid(why) => write!(f, "{why}"),
            Self::Io(why) => write!(f, "{why}"),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(sanitize(&e.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Applied {
    /// Whether the file was written at all.
    pub wrote: bool,
    /// Where the previous contents were saved. `None` when nothing was written or the file did not exist.
    pub backup: Option<PathBuf>,
}

#[derive(Clone)]
pub(crate) struct Docstore {
    pub kind: Kind,
    /// The file to change.
    pub path: PathBuf,
    /// Our own rotating backups, kept private. Never Claude's `backups/` folder.
    pub backup_dir: PathBuf,
    pub keep_backups: usize,
    /// How long to wait for the lock before giving up. The decision record proposes about 3 s.
    pub lock_wait: Duration,
    /// How long after a write to look again for an unlocked overwrite by Claude.
    pub verify_delay: Duration,
}

impl Docstore {
    pub(crate) fn new(path: PathBuf, backup_dir: PathBuf) -> Self {
        Self {
            kind: Kind::ClaudeJson,
            path,
            backup_dir,
            keep_backups: 20,
            lock_wait: Duration::from_secs(3),
            verify_delay: Duration::from_secs(1),
        }
    }

    /// A store for the `.claude/settings.local.json` of one project.
    pub(crate) fn project_settings(path: PathBuf, backup_dir: PathBuf) -> Self {
        Self {
            kind: Kind::ProjectSettings,
            ..Self::new(path, backup_dir)
        }
    }

    /// Reads the file and works out what `ops` would change. Writes nothing.
    pub(crate) fn prepare(&self, ops: Vec<Op>) -> Result<Plan, StoreError> {
        self.prepare_current(self.read()?, ops)
    }

    /// [`prepare`](Self::prepare) for a file the caller has just read itself (text without the byte-order mark,
    /// and whether there was one), so it is not read twice.
    pub(crate) fn prepare_text(
        &self,
        text: String,
        bom: bool,
        ops: Vec<Op>,
    ) -> Result<Plan, StoreError> {
        self.prepare_current(
            Current {
                text,
                bom,
                missing: false,
            },
            ops,
        )
    }

    fn prepare_current(&self, current: Current, ops: Vec<Op>) -> Result<Plan, StoreError> {
        let (hash, edited) = alongside(
            current.text.len() >= PARALLEL_FROM,
            || current.hash(),
            || self.edit(&current, &ops),
        );
        let (_, changes) = edited?;
        Ok(Plan {
            ops,
            base_hash: hash,
            changes,
        })
    }

    /// Applies `ops` to the file's text. Returns the replacements to make in it, or `None` when nothing changes,
    /// and what changed. Ops meant for the other kind of file are refused.
    fn edit(
        &self,
        current: &Current,
        ops: &[Op],
    ) -> Result<(Option<Splices>, Vec<FileChange>), StoreError> {
        let (text, lines) = match self.kind {
            // Possibly tens of megabytes: only the bytes of the changed lists are replaced.
            Kind::ClaudeJson => edit_claude_json(&current.text, ops)?,
            // A small file: parse, change, write it out whole.
            Kind::ProjectSettings => {
                let mut doc = current.parse()?;
                let mut lines = Vec::new();
                for op in ops {
                    match op {
                        Op::Approve { name } => approve(&mut doc, name, &mut lines)?,
                        _ => return Err(wrong_file()),
                    }
                }
                let text = if lines.is_empty() {
                    None
                } else {
                    Some(vec![(0..current.text.len(), current.render(&doc)?)])
                };
                (text, lines)
            }
        };
        let path = match self.kind {
            Kind::ClaudeJson => ".claude.json".to_owned(),
            Kind::ProjectSettings => sanitize(&self.path.display().to_string()),
        };
        Ok((text, vec![FileChange { path, lines }]))
    }

    /// Writes the plan's change. The file is re-read under the lock and the ops are applied to that fresh copy; if
    /// the file changed since `prepare` and that alters what the user was shown, nothing is written.
    pub(crate) fn apply(&self, plan: &Plan) -> Result<Applied, StoreError> {
        // The lock directory lives next to the file, so the folder has to exist first.
        if self.kind == Kind::ProjectSettings
            && !plan.is_empty()
            && let Some(dir) = self.path.parent()
        {
            ensure_plain_dir(dir)?;
        }
        let _lock = Lock::acquire(&self.lock_path(), self.lock_wait)?;
        let current = self.read()?;
        let (text, changes) = self.edit(&current, &plan.ops)?;
        if changes != plan.changes && current.hash() != plan.base_hash {
            return Err(StoreError::Changed);
        }
        self.write(&current, text.as_ref())
    }

    /// Waits `verify_delay`, then makes sure the change is still there. Claude may write without our lock, from a
    /// copy it read before ours; if so the ops are applied again (up to three times). Says whether it had to.
    /// Looking needs no lock (every writer replaces the file whole), so in the usual case, the change intact, Claude
    /// is never kept waiting; the lock is taken only to write.
    pub(crate) fn verify(&self, plan: &Plan) -> Result<bool, StoreError> {
        let mut reapplied = false;
        for round in 0..MAX_VERIFY_ROUNDS {
            std::thread::sleep(self.verify_delay);
            if self.edit(&self.read()?, &plan.ops)?.0.is_none() {
                return Ok(reapplied);
            }
            if round + 1 == MAX_VERIFY_ROUNDS {
                break;
            }
            let _lock = Lock::acquire(&self.lock_path(), self.lock_wait)?;
            let current = self.read()?;
            let (edits, _) = self.edit(&current, &plan.ops)?;
            if edits.is_some() {
                self.write(&current, edits.as_ref())?;
                reapplied = true;
            }
        }
        Err(StoreError::Lost)
    }

    fn lock_path(&self) -> PathBuf {
        let mut p: OsString = self.path.clone().into_os_string();
        p.push(".lock");
        PathBuf::from(p)
    }

    fn read(&self) -> Result<Current, StoreError> {
        match fsx::read_limited_bom(&self.path, MAX_CLAUDE_JSON)? {
            Some((text, bom)) => Ok(Current {
                text,
                bom,
                missing: false,
            }),
            // An approval may create the file; the main configuration is never created.
            None if self.kind == Kind::ProjectSettings => Ok(Current {
                text: "{}".to_owned(),
                bom: false,
                missing: true,
            }),
            None => Err(StoreError::Invalid(format!(
                "{} does not exist; there is nothing to change.",
                sanitize(&self.path.display().to_string())
            ))),
        }
    }

    /// Replaces the file with its text after `edits` (plus the byte-order mark it had), written piece by piece so a
    /// large file is not copied in memory first. `None` writes nothing.
    fn write(&self, current: &Current, edits: Option<&Splices>) -> Result<Applied, StoreError> {
        let Some(edits) = edits else {
            return Ok(Applied {
                wrote: false,
                backup: None,
            });
        };
        let dir = self.path.parent().ok_or_else(|| {
            StoreError::Invalid("the configuration file has no parent folder".to_owned())
        })?;
        if self.kind == Kind::ProjectSettings {
            ensure_plain_dir(dir)?;
        }
        let mut tmp = tempfile::Builder::new()
            .prefix(".claude.json.tmp.")
            .tempfile_in(dir)?;
        if current.bom {
            tmp.write_all(BOM.as_bytes())?;
        }
        let mut at = 0;
        for (range, with) in edits {
            tmp.write_all(&current.text.as_bytes()[at..range.start])?;
            tmp.write_all(with.as_bytes())?;
            at = range.end;
        }
        tmp.write_all(&current.text.as_bytes()[at..])?;
        tmp.as_file().sync_all()?;
        let backup = if current.missing {
            None
        } else {
            Some(self.backup(current)?)
        };
        tmp.persist(&self.path).map_err(|e| e.error)?;
        if let Some(backup) = &backup {
            // A hard link shares the old file's permissions until now; it is the only name of that file left.
            make_private(backup);
        }
        self.rotate_backups();
        Ok(Applied {
            wrote: true,
            backup,
        })
    }

    /// Keeps the bytes about to be replaced. A hard link to the current file costs nothing, whatever its size: the
    /// rename that follows leaves the old file to the link alone. Where a link is not possible (another drive, a
    /// file system without links) the text that was read is copied instead.
    fn backup(&self, current: &Current) -> Result<PathBuf, StoreError> {
        create_private_dir(&self.backup_dir)?;
        let ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut n = 0u32;
        loop {
            let path = self
                .backup_dir
                .join(format!("{}.{ms:016}-{n}.bak", self.backup_label()));
            match std::fs::hard_link(&self.path, &path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(_) => match create_private_file(&path) {
                    Ok(mut file) => {
                        if current.bom {
                            file.write_all(BOM.as_bytes())?;
                        }
                        file.write_all(current.text.as_bytes())?;
                        file.sync_all()?;
                        return Ok(path);
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                    Err(e) => return Err(e.into()),
                },
            }
        }
    }

    /// Backups of different files must not push each other out of the rotation.
    fn backup_label(&self) -> String {
        match self.kind {
            Kind::ClaudeJson => "claude.json".to_owned(),
            Kind::ProjectSettings => {
                let digest = Sha256::digest(self.path.to_string_lossy().as_bytes());
                format!("settings.local-{:.12}", format!("{digest:x}"))
            }
        }
    }

    fn rotate_backups(&self) {
        let Ok(entries) = std::fs::read_dir(&self.backup_dir) else {
            return;
        };
        let prefix = format!("{}.", self.backup_label());
        let mut names: Vec<_> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.ends_with(".bak")
            })
            .collect();
        names.sort();
        let surplus = names.len().saturating_sub(self.keep_backups);
        for name in names.into_iter().take(surplus) {
            let _ = std::fs::remove_file(self.backup_dir.join(name));
        }
    }
}

/// The `.claude` folder of a project: created if missing, but never followed when it is a link, because a
/// repository could point it somewhere else.
fn ensure_plain_dir(dir: &Path) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => Err(StoreError::Invalid(
            "the project's .claude folder is a link; refusing to write through it".to_owned(),
        )),
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(StoreError::Invalid(
            "the project's .claude is not a folder".to_owned(),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(std::fs::create_dir(dir)?),
        Err(e) => Err(e.into()),
    }
}

fn wrong_file() -> StoreError {
    StoreError::Invalid("that change does not belong in this file".to_owned())
}

#[cfg(unix)]
fn make_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn make_private(_path: &Path) {}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// The file as read: text without a byte-order mark, plus how to write it back the way we found it.
struct Current {
    text: String,
    bom: bool,
    /// The file does not exist yet; `text` is an empty object.
    missing: bool,
}

impl Current {
    fn parse(&self) -> Result<Value, StoreError> {
        let value: Value = serde_json::from_str(&self.text).map_err(|e| {
            StoreError::Invalid(format!(
                "the configuration is not valid JSON ({}); nothing was changed",
                sanitize(&e.to_string())
            ))
        })?;
        if !value.is_object() {
            return Err(StoreError::Invalid(
                "the configuration is not a JSON object; nothing was changed".to_owned(),
            ));
        }
        Ok(value)
    }

    /// Hash of the exact bytes on disk.
    fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        if self.bom {
            hasher.update(BOM.as_bytes());
        }
        hasher.update(self.text.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Claude writes 2-space JSON with LF and no trailing newline; a file that has CRLF or a final newline keeps
    /// them (and a byte-order mark, which `write` adds back).
    fn render(&self, doc: &Value) -> Result<String, StoreError> {
        let mut out = serde_json::to_string_pretty(doc)
            .map_err(|e| StoreError::Invalid(sanitize(&e.to_string())))?;
        if self.text.ends_with('\n') || self.missing {
            out.push('\n');
        }
        if self.text.contains("\r\n") {
            out = out.replace('\n', "\r\n");
        }
        Ok(out)
    }
}

/// Claude's lock: a directory next to the file. Removed when dropped, but only if we made it.
struct Lock {
    dir: PathBuf,
}

impl Lock {
    fn acquire(dir: &Path, wait: Duration) -> Result<Self, StoreError> {
        let started = Instant::now();
        let mut attempt = 0;
        loop {
            match std::fs::create_dir(dir) {
                Ok(()) => {
                    return Ok(Self {
                        dir: dir.to_owned(),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale(dir) {
                        // Whoever held it is gone. `remove_dir` only removes an empty directory.
                        let _ = std::fs::remove_dir(dir);
                        continue;
                    }
                }
                Err(e) => return Err(e.into()),
            }
            if started.elapsed() >= wait {
                return Err(StoreError::Locked);
            }
            let pause = Duration::from_millis(BACKOFF_MS[attempt.min(BACKOFF_MS.len() - 1)]);
            attempt += 1;
            std::thread::sleep(pause.min(wait.saturating_sub(started.elapsed())));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn is_stale(dir: &Path) -> bool {
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > LOCK_STALE)
}

/// Replacements in a file's text: each range (in file order, not overlapping) becomes the string.
type Splices = Vec<(std::ops::Range<usize>, String)>;

/// The lists a [`Op`] on `.claude.json` changes, inside a project entry.
const DISABLED: &str = "disabledMcpServers";
const REJECTED: &str = "disabledMcpjsonServers";

/// Applies `ops` to the text of `.claude.json` by replacing only the bytes of the lists they change. One pass over
/// the projects, however many ops (switching a user server touches every project). Returns the new text, or `None`
/// when nothing changes, and the plan's lines.
fn edit_claude_json(text: &str, ops: &[Op]) -> Result<(Option<Splices>, Vec<String>), StoreError> {
    // Checking the whole text and finding the lists are independent passes over it: on a large file they run side
    // by side. A broken file is reported as such even when the scan also failed.
    let (valid, edited) = alongside(
        text.len() >= PARALLEL_FROM,
        || serde_json::from_str::<serde::de::IgnoredAny>(text),
        || edit_valid_claude_json(text, ops),
    );
    valid.map_err(|e| {
        StoreError::Invalid(format!(
            "the configuration is not valid JSON ({}); nothing was changed",
            sanitize(&e.to_string())
        ))
    })?;
    edited
}

/// Below this size a second thread costs more than it saves.
const PARALLEL_FROM: usize = 1 << 20;

/// Runs `a` and `b`, on two threads when `parallel`.
fn alongside<A: Send, B>(
    parallel: bool,
    a: impl FnOnce() -> A + Send,
    b: impl FnOnce() -> B,
) -> (A, B) {
    if !parallel {
        return (a(), b());
    }
    std::thread::scope(|scope| {
        let a = scope.spawn(a);
        let b = b();
        // `a` only parses or hashes; a panic there is a bug worth surfacing as one.
        (a.join().unwrap_or_else(|p| std::panic::resume_unwind(p)), b)
    })
}

/// [`edit_claude_json`] for text already known to be valid JSON (or being checked alongside).
fn edit_valid_claude_json(
    text: &str,
    ops: &[Op],
) -> Result<(Option<Splices>, Vec<String>), StoreError> {
    let invalid = |why: String| StoreError::Invalid(why);
    let b = text.as_bytes();
    let start = jsonspan::skip_ws(b, 0);
    if b.get(start) != Some(&b'{') {
        return Err(invalid(
            "the configuration is not a JSON object; nothing was changed".to_owned(),
        ));
    }
    if ops.is_empty() {
        return Ok((None, Vec::new()));
    }
    if ops.iter().any(|op| matches!(op, Op::Approve { .. })) {
        return Err(wrong_file());
    }
    // `None` for an op on a user server (the top-level `mcpServers`).
    let targets: Vec<Option<String>> = ops
        .iter()
        .map(|op| op.project().map(normalise_project))
        .collect();
    let user_servers = targets.iter().any(Option::is_none);
    let local_servers = ops
        .iter()
        .any(|op| op.is_server_op() && op.project().is_some());
    // One pass: the root, `projects` and `mcpServers` as needed, and only the entries the ops are about.
    let root = jsonspan::object_with(text, start, &mut |level, key| match level {
        0 => {
            (key == "projects" && targets.iter().any(Option::is_some))
                || (key == "mcpServers" && user_servers)
        }
        1 => targets
            .iter()
            .flatten()
            .any(|t| *t == normalise_project(key)),
        2 => key == "mcpServers" && local_servers,
        _ => false,
    })
    .map_err(invalid)?;

    let mut out = Edited {
        nl: if text.contains("\r\n") { "\r\n" } else { "\n" },
        splices: Vec::new(),
        lines: Vec::new(),
    };

    if user_servers {
        let here: Vec<&Op> = ops
            .iter()
            .zip(&targets)
            .filter(|(_, t)| t.is_none())
            .map(|(op, _)| op)
            .collect();
        let servers = single(&root, "mcpServers", "the configuration")?;
        edit_servers(
            text,
            servers,
            "  mcpServers".to_owned(),
            &here,
            true,
            &mut out,
        )?;
    }
    if targets.iter().all(Option::is_none) {
        return Ok(out.finish());
    }

    let projects = single(&root, "projects", "the configuration")?
        .and_then(|m| m.object.as_ref())
        .ok_or_else(|| invalid("the configuration has no projects".to_owned()))?;
    let keys: Vec<String> = projects
        .members
        .iter()
        .map(|m| normalise_project(&m.key))
        .collect();
    for (i, member) in projects.members.iter().enumerate() {
        if projects.members[..i].iter().any(|m| m.key == member.key) {
            return Err(invalid(format!(
                "the project {} is listed twice; refusing to guess which one Claude Code uses",
                sanitize(&member.key)
            )));
        }
    }
    if let Some(missing) = targets.iter().flatten().find(|t| !keys.contains(t)) {
        return Err(invalid(format!(
            "{} is not a project Claude Code knows about",
            sanitize(missing)
        )));
    }

    // Servers to delete or replace in a project: an error if no spelling of the project has them.
    let mut server_found = vec![false; ops.len()];
    for (member, key) in projects.members.iter().zip(&keys) {
        let here: Vec<&Op> = ops
            .iter()
            .zip(&targets)
            .filter(|(_, t)| t.as_deref() == Some(key.as_str()))
            .map(|(op, _)| op)
            .collect();
        if here.is_empty() {
            continue;
        }
        let Some(entry) = &member.object else {
            return Err(invalid(format!(
                "the entry for {} is not an object",
                sanitize(&member.key)
            )));
        };
        let shown_key = sanitize(&member.key.replace('"', "\\\""));
        let server_ops: Vec<&Op> = here
            .iter()
            .copied()
            .filter(|op| op.is_server_op())
            .collect();
        if !server_ops.is_empty() {
            let servers = single(
                entry,
                "mcpServers",
                &format!("the entry for {}", sanitize(&member.key)),
            )?;
            let found = edit_servers(
                text,
                servers,
                format!("  projects[\"{shown_key}\"].mcpServers"),
                &server_ops,
                false,
                &mut out,
            )?;
            for (i, op) in ops.iter().enumerate() {
                if found.iter().any(|f| std::ptr::eq(*f, op)) {
                    server_found[i] = true;
                }
            }
        }
        for list in [DISABLED, REJECTED] {
            let list_ops: Vec<&&Op> = here
                .iter()
                .filter(|op| {
                    matches!(
                        (op, list),
                        (Op::SetDisabled { .. }, DISABLED) | (Op::ClearRejection { .. }, REJECTED)
                    )
                })
                .collect();
            if list_ops.is_empty() {
                continue;
            }
            let existing = single(
                entry,
                list,
                &format!("the entry for {}", sanitize(&member.key)),
            )?;
            let mut items = match existing {
                None => None,
                Some(m) => match serde_json::from_str::<Value>(&text[m.value.clone()]) {
                    Ok(Value::Array(items)) => Some(items),
                    Ok(_) if list == DISABLED => {
                        return Err(invalid(format!(
                            "{list} of {} is not a list; refusing to change it",
                            sanitize(&member.key)
                        )));
                    }
                    // The reader ignores a rejection list of the wrong shape, so there is nothing to clear in it.
                    Ok(_) => continue,
                    Err(e) => return Err(invalid(sanitize(&e.to_string()))),
                },
            };
            let header = format!("  projects[\"{shown_key}\"].{list}");
            let mut changed = false;
            for op in list_ops {
                match op {
                    Op::SetDisabled { name, disabled, .. } => {
                        let is_name = |v: &Value| v.as_str() == Some(name.as_str());
                        if *disabled {
                            let list = items.get_or_insert_with(Vec::new);
                            if !list.iter().any(is_name) {
                                list.push(Value::String(name.clone()));
                                out.lines.push(header.clone());
                                out.lines.push(format!("+ {}", sanitize(name)));
                                changed = true;
                            }
                        } else if let Some(list) = &mut items {
                            let before = list.len();
                            list.retain(|v| !is_name(v));
                            if list.len() != before {
                                out.lines.push(header.clone());
                                out.lines.push(format!("- {}", sanitize(name)));
                                changed = true;
                            }
                        }
                    }
                    Op::ClearRejection { name, .. } => {
                        if let Some(list) = &mut items {
                            let before = list.len();
                            list.retain(|v| v.as_str() != Some(name.as_str()));
                            if list.len() != before {
                                out.lines.push(header.clone());
                                out.lines.push(format!("- {}", sanitize(name)));
                                changed = true;
                            }
                        }
                    }
                    Op::Approve { .. } | Op::RemoveServer { .. } | Op::ReplaceServer { .. } => {}
                }
            }
            if !changed {
                continue;
            }
            let items = items.unwrap_or_default();
            out.splices.push(match existing {
                Some(m) => {
                    let indent = jsonspan::line_indent(text, m.key_start);
                    (m.value.clone(), render_list(&items, indent, out.nl)?)
                }
                None => add_member(text, member, entry, list, &items, out.nl)?,
            });
        }
    }
    if let Some(i) =
        (0..ops.len()).find(|&i| ops[i].is_server_op() && targets[i].is_some() && !server_found[i])
    {
        return Err(invalid(format!(
            "{} is no longer in that project's configuration",
            sanitize(ops[i].server_name())
        )));
    }
    Ok(out.finish())
}

/// What an edit of `.claude.json` produces so far.
struct Edited<'n> {
    /// The file's newline, for new lines.
    nl: &'n str,
    splices: Splices,
    lines: Vec<String>,
}

impl Edited<'_> {
    fn finish(mut self) -> (Option<Splices>, Vec<String>) {
        // In file order; a stable sort keeps two insertions at the same place in the order they were made.
        self.splices.sort_by_key(|(range, _)| range.start);
        (
            (!self.splices.is_empty()).then_some(self.splices),
            self.lines,
        )
    }
}

/// Deletes or replaces servers in one `mcpServers` object. Returns the ops that found their server. With
/// `required`, an op whose server is missing is an error right away (user servers have one place only).
fn edit_servers<'o>(
    text: &str,
    servers: Option<&jsonspan::Member>,
    header: String,
    ops: &[&'o Op],
    required: bool,
    out: &mut Edited<'_>,
) -> Result<Vec<&'o Op>, StoreError> {
    let (nl, splices, lines) = (out.nl, &mut out.splices, &mut out.lines);
    let invalid = |why: String| StoreError::Invalid(why);
    let object = match servers {
        Some(m) => Some(
            m.object
                .as_ref()
                .ok_or_else(|| invalid("mcpServers is not an object".to_owned()))?,
        ),
        None => None,
    };
    let mut found = Vec::new();
    let mut removed = Vec::new();
    for op in ops {
        let name = op.server_name();
        let index = object.and_then(|o| {
            let mut hits = o.members.iter().enumerate().filter(|(_, m)| m.key == name);
            let first = hits.next().map(|(i, _)| i);
            hits.next().map_or(first, |_| Some(usize::MAX))
        });
        let index = match index {
            Some(usize::MAX) => {
                return Err(invalid(format!(
                    "{} is defined twice; refusing to guess which one Claude Code uses",
                    sanitize(name)
                )));
            }
            Some(i) => i,
            None if required => {
                return Err(invalid(format!(
                    "{} is no longer in the configuration",
                    sanitize(name)
                )));
            }
            None => continue,
        };
        let object = object.expect("found in it");
        let member = &object.members[index];
        let old: Value = serde_json::from_str(&text[member.value.clone()])
            .map_err(|e| invalid(sanitize(&e.to_string())))?;
        if old.get("oauth").is_some() {
            return Err(invalid(format!(
                "{} signs in with OAuth; it is changed through Claude Code's own command, which Klefki does not run yet",
                sanitize(name)
            )));
        }
        if removed.contains(&index) || splices.iter().any(|(r, _)| *r == member.value) {
            return Err(invalid(format!(
                "{} is changed twice in one plan",
                sanitize(name)
            )));
        }
        found.push(*op);
        lines.push(header.clone());
        match op {
            Op::RemoveServer { .. } => {
                lines.push(format!("- {}", sanitize(name)));
                removed.push(index);
            }
            Op::ReplaceServer { entry: new, .. } => {
                let new = entry::unmasked(new, &old).map_err(invalid)?;
                entry::validate(&new).map_err(invalid)?;
                let pretty =
                    |v: &Value| serde_json::to_string_pretty(&entry::masked(v)).unwrap_or_default();
                let diff = entry::diff_lines(&pretty(&old), &pretty(&new));
                if old == new {
                    // Nothing changes: no lines, no splice.
                    lines.pop();
                    continue;
                }
                lines.push(format!("~ {}", sanitize(name)));
                lines.extend(diff.iter().map(|l| sanitize(l)));
                if diff.iter().all(|l| l.starts_with("  ")) {
                    lines.push("~ (a hidden environment or header value changes)".to_owned());
                }
                let indent = jsonspan::line_indent(text, member.key_start);
                splices.push((member.value.clone(), render_value(&new, indent, nl)?));
            }
            _ => {}
        }
    }
    if let (Some(object), false) = (object, removed.is_empty()) {
        splices.extend(remove_members(object, &removed));
    }
    Ok(found)
}

/// The splices that delete the members at `removed` from `object`, commas included, without overlapping: a member
/// with any member after it goes up to that member's key; a run of members at the end goes from the end of the last
/// member kept; with nothing kept the object becomes `{}`.
fn remove_members(object: &jsonspan::Object, removed: &[usize]) -> Splices {
    let n = object.members.len();
    if (0..n).all(|i| removed.contains(&i)) {
        return vec![(object.open..object.close + 1, "{}".to_owned())];
    }
    // The first index of the run of removed members that reaches the end, if any.
    let mut tail = n;
    while tail > 0 && removed.contains(&(tail - 1)) {
        tail -= 1;
    }
    let mut out = Vec::new();
    for &i in removed {
        if i < tail {
            out.push((
                object.members[i].key_start..object.members[i + 1].key_start,
                String::new(),
            ));
        }
    }
    if tail < n {
        out.push((
            object.members[tail - 1].value.end..object.members[n - 1].value.end,
            String::new(),
        ));
    }
    out
}

/// A value written the way the surrounding file is (see [`render_list`]).
fn render_value(value: &Value, indent: Option<&str>, nl: &str) -> Result<String, StoreError> {
    let out = match indent {
        Some(indent) => {
            serde_json::to_string_pretty(value).map(|s| s.replace('\n', &format!("{nl}{indent}")))
        }
        None => serde_json::to_string(value),
    };
    out.map_err(|e| StoreError::Invalid(sanitize(&e.to_string())))
}

/// The member called `key`, refusing a key that appears twice (a JSON reader would silently keep only one).
fn single<'a>(
    object: &'a jsonspan::Object,
    key: &str,
    owner: &str,
) -> Result<Option<&'a jsonspan::Member>, StoreError> {
    let mut found = object.members.iter().filter(|m| m.key == key);
    let first = found.next();
    if found.next().is_some() {
        return Err(StoreError::Invalid(format!(
            "{owner} has {key} twice; refusing to guess which one Claude Code uses"
        )));
    }
    Ok(first)
}

/// A list written the way the surrounding file is: one item per line, indented one step more than the line its
/// key is on, or on one line when the key shares its line with other text.
fn render_list(items: &[Value], indent: Option<&str>, nl: &str) -> Result<String, StoreError> {
    let value = Value::Array(items.to_vec());
    let out = match indent {
        Some(indent) => {
            serde_json::to_string_pretty(&value).map(|s| s.replace('\n', &format!("{nl}{indent}")))
        }
        None => serde_json::to_string(&value),
    };
    out.map_err(|e| StoreError::Invalid(sanitize(&e.to_string())))
}

/// The splice that adds `"<list>": [...]` as the last member of `entry` (the value of the project `member`).
fn add_member(
    text: &str,
    member: &jsonspan::Member,
    entry: &jsonspan::Object,
    list: &str,
    items: &[Value],
    nl: &str,
) -> Result<(std::ops::Range<usize>, String), StoreError> {
    let key = serde_json::to_string(list).map_err(|e| StoreError::Invalid(e.to_string()))?;
    match entry.members.last() {
        Some(last) => match jsonspan::line_indent(text, last.key_start) {
            Some(indent) => Ok((
                last.value.end..last.value.end,
                format!(
                    ",{nl}{indent}{key}: {}",
                    render_list(items, Some(indent), nl)?
                ),
            )),
            None => Ok((
                last.value.end..last.value.end,
                format!(",{key}:{}", render_list(items, None, nl)?),
            )),
        },
        // An empty entry, `{}`: written out like Claude would, two spaces deeper than the project's key.
        None => match jsonspan::line_indent(text, member.key_start) {
            Some(outer) => {
                let inner = format!("{outer}  ");
                Ok((
                    entry.open..entry.close + 1,
                    format!(
                        "{{{nl}{inner}{key}: {}{nl}{outer}}}",
                        render_list(items, Some(&inner), nl)?
                    ),
                ))
            }
            None => Ok((
                entry.open..entry.close + 1,
                format!("{{{key}:{}}}", render_list(items, None, nl)?),
            )),
        },
    }
}

fn approve(doc: &mut Value, name: &str, lines: &mut Vec<String>) -> Result<(), StoreError> {
    let root = doc
        .as_object_mut()
        .ok_or_else(|| StoreError::Invalid("the settings file is not a JSON object".to_owned()))?;
    let wrong_shape = |key: &str| {
        StoreError::Invalid(format!(
            "{key} in the settings file is not a list; refusing to change it"
        ))
    };
    // Check both lists before changing either, so a refusal leaves the document alone.
    for key in ["enabledMcpjsonServers", "disabledMcpjsonServers"] {
        if root.get(key).is_some_and(|v| !v.is_array()) {
            return Err(wrong_shape(key));
        }
    }
    let is_name = |v: &Value| v.as_str() == Some(name);
    let enabled = root
        .entry("enabledMcpjsonServers")
        .or_insert_with(|| Value::Array(Vec::new()));
    let enabled = enabled
        .as_array_mut()
        .ok_or_else(|| wrong_shape("enabledMcpjsonServers"))?;
    if !enabled.iter().any(is_name) {
        enabled.push(Value::String(name.to_owned()));
        lines.push("  enabledMcpjsonServers".to_owned());
        lines.push(format!("+ {}", sanitize(name)));
    }
    if let Some(rejected) = root
        .get_mut("disabledMcpjsonServers")
        .and_then(Value::as_array_mut)
    {
        let before = rejected.len();
        rejected.retain(|v| !is_name(v));
        if rejected.len() != before {
            lines.push("  disabledMcpjsonServers".to_owned());
            lines.push(format!("- {}", sanitize(name)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    // The Value-based edits this module used before it edited text in place; kept as an oracle for tests.
    fn set_disabled(
        doc: &mut Value,
        project: &str,
        name: &str,
        disabled: bool,
        lines: &mut Vec<String>,
    ) -> Result<(), super::StoreError> {
        let wanted = normalise_project(project);
        let projects = doc
            .get_mut("projects")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                super::StoreError::Invalid("the configuration has no projects".to_owned())
            })?;
        let mut found = false;
        for (key, entry) in projects.iter_mut() {
            if normalise_project(key) != wanted {
                continue;
            }
            found = true;
            let entry = entry.as_object_mut().ok_or_else(|| {
                super::StoreError::Invalid(format!(
                    "the entry for {} is not an object",
                    sanitize(key)
                ))
            })?;
            // (The original created an empty list when enabling; the in-place edit does not, and neither does this.)
            if !disabled && !entry.contains_key("disabledMcpServers") {
                continue;
            }
            let list = entry
                .entry("disabledMcpServers")
                .or_insert_with(|| Value::Array(Vec::new()));
            let list = list.as_array_mut().ok_or_else(|| {
                super::StoreError::Invalid(format!(
                    "disabledMcpServers of {} is not a list; refusing to change it",
                    sanitize(key)
                ))
            })?;
            let is_name = |v: &Value| v.as_str() == Some(name);
            let before = list.len();
            let header = format!(
                "  projects[\"{}\"].disabledMcpServers",
                sanitize(&key.replace('"', "\\\""))
            );
            if disabled {
                if !list.iter().any(is_name) {
                    list.push(Value::String(name.to_owned()));
                    lines.push(header);
                    lines.push(format!("+ {}", sanitize(name)));
                }
            } else {
                list.retain(|v| !is_name(v));
                if list.len() != before {
                    lines.push(header);
                    lines.push(format!("- {}", sanitize(name)));
                }
            }
        }
        if found {
            Ok(())
        } else {
            Err(super::StoreError::Invalid(format!(
                "{} is not a project Claude Code knows about",
                sanitize(&wanted)
            )))
        }
    }

    fn clear_rejection(
        doc: &mut Value,
        project: &str,
        name: &str,
        lines: &mut Vec<String>,
    ) -> Result<(), super::StoreError> {
        let wanted = normalise_project(project);
        let projects = doc
            .get_mut("projects")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                super::StoreError::Invalid("the configuration has no projects".to_owned())
            })?;
        let mut found = false;
        for (key, entry) in projects.iter_mut() {
            if normalise_project(key) != wanted {
                continue;
            }
            found = true;
            // A list of the wrong shape is ignored by the reader too, so there is nothing to clear in it.
            let Some(list) = entry
                .get_mut("disabledMcpjsonServers")
                .and_then(Value::as_array_mut)
            else {
                continue;
            };
            let before = list.len();
            list.retain(|v| v.as_str() != Some(name));
            if list.len() != before {
                lines.push(format!(
                    "  projects[\"{}\"].disabledMcpjsonServers",
                    sanitize(&key.replace('"', "\\\""))
                ));
                lines.push(format!("- {}", sanitize(name)));
            }
        }
        if found {
            Ok(())
        } else {
            Err(super::StoreError::Invalid(format!(
                "{} is not a project Claude Code knows about",
                sanitize(&wanted)
            )))
        }
    }

    use super::*;
    use serde_json::json;

    struct Fixture {
        dir: tempfile::TempDir,
        store: Docstore,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(".claude.json");
            std::fs::write(&path, text).unwrap();
            let mut store = Docstore::new(path, dir.path().join("backups"));
            store.lock_wait = Duration::from_millis(300);
            store.verify_delay = Duration::ZERO;
            Self { dir, store }
        }

        fn text(&self) -> String {
            std::fs::read_to_string(&self.store.path).unwrap()
        }

        fn json(&self) -> Value {
            serde_json::from_str(&self.text()).unwrap()
        }

        fn lock(&self) -> PathBuf {
            self.store.lock_path()
        }

        fn backups(&self) -> Vec<PathBuf> {
            let mut v: Vec<_> = std::fs::read_dir(&self.store.backup_dir)
                .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
                .unwrap_or_default();
            v.sort();
            v
        }

        fn leftovers(&self) -> Vec<String> {
            std::fs::read_dir(self.dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".tmp.") || n.ends_with(".lock"))
                .collect()
        }
    }

    fn pretty(v: &Value) -> String {
        serde_json::to_string_pretty(v).unwrap()
    }

    fn disable(project: &str, name: &str) -> Op {
        Op::SetDisabled {
            project: project.to_owned(),
            name: name.to_owned(),
            disabled: true,
        }
    }

    fn enable(project: &str, name: &str) -> Op {
        Op::SetDisabled {
            project: project.to_owned(),
            name: name.to_owned(),
            disabled: false,
        }
    }

    fn sample() -> Value {
        json!({
            "zeta": 1.5,
            "numStartups": 12,
            "mcpServers": {"gateway": {"command": "node", "env": {"TOKEN": "s3cret"}}},
            "projects": {
                "C:/work/app": {"allowedTools": [], "disabledMcpServers": ["old"], "zz": {"keep": true}},
                "C:/work/other": {"allowedTools": []}
            },
            "alpha": "last"
        })
    }

    #[test]
    fn disabling_appends_the_name_and_changes_nothing_else() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        let mut expected = sample();
        expected["projects"]["C:/work/app"]["disabledMcpServers"] = json!(["old", "gateway"]);
        assert_eq!(f.text(), pretty(&expected));
        // Key order is the file's, not alphabetical.
        let keys: Vec<_> = f.json().as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            ["zeta", "numStartups", "mcpServers", "projects", "alpha"]
        );
    }

    #[test]
    fn enabling_removes_the_name_and_leaves_an_empty_list() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f.store.prepare(vec![enable("C:/work/app", "old")]).unwrap();
        f.store.apply(&plan).unwrap();
        assert_eq!(
            f.json()["projects"]["C:/work/app"]["disabledMcpServers"],
            json!([])
        );
    }

    #[test]
    fn the_list_is_created_at_the_end_of_an_entry_that_lacks_it() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/other", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        let entry = f.json()["projects"]["C:/work/other"].clone();
        let keys: Vec<_> = entry.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["allowedTools", "disabledMcpServers"]);
        assert_eq!(entry["disabledMcpServers"], json!(["gateway"]));
    }

    #[test]
    fn disabling_twice_adds_once_and_enabling_removes_every_copy() {
        let f = Fixture::new(&pretty(
            &json!({"projects": {"/p": {"disabledMcpServers": ["a", "a", 7, null]}}}),
        ));
        let plan = f
            .store
            .prepare(vec![
                disable("/p", "a"),
                disable("/p", "b"),
                disable("/p", "b"),
            ])
            .unwrap();
        f.store.apply(&plan).unwrap();
        assert_eq!(
            f.json()["projects"]["/p"]["disabledMcpServers"],
            json!(["a", "a", 7, null, "b"])
        );
        let plan = f.store.prepare(vec![enable("/p", "a")]).unwrap();
        f.store.apply(&plan).unwrap();
        // Entries that are not names are not ours to touch.
        assert_eq!(
            f.json()["projects"]["/p"]["disabledMcpServers"],
            json!([7, null, "b"])
        );
    }

    #[test]
    fn every_spelling_of_a_project_is_updated() {
        let f = Fixture::new(&pretty(&json!({"projects": {
            "c:\\Users\\x": {"disabledMcpServers": []},
            "C:/Users/x/": {"disabledMcpServers": ["q"]},
            "C:/Users/y": {}
        }})));
        let plan = f.store.prepare(vec![disable("C:/Users/x", "u")]).unwrap();
        f.store.apply(&plan).unwrap();
        let j = f.json();
        assert_eq!(
            j["projects"]["c:\\Users\\x"]["disabledMcpServers"],
            json!(["u"])
        );
        assert_eq!(
            j["projects"]["C:/Users/x/"]["disabledMcpServers"],
            json!(["q", "u"])
        );
        assert_eq!(j["projects"]["C:/Users/y"], json!({}));
    }

    #[test]
    fn an_unknown_project_is_refused_not_invented() {
        let f = Fixture::new(&pretty(&sample()));
        let before = f.text();
        let err = f
            .store
            .prepare(vec![disable("C:/nowhere", "x")])
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
        assert_eq!(f.text(), before);
    }

    #[test]
    fn a_list_of_the_wrong_shape_is_refused_and_the_file_is_untouched() {
        let text = pretty(&json!({"projects": {"/p": {"disabledMcpServers": "gateway"}}}));
        let f = Fixture::new(&text);
        for ops in [vec![disable("/p", "x")], vec![enable("/p", "x")]] {
            assert!(matches!(f.store.prepare(ops), Err(StoreError::Invalid(_))));
        }
        assert_eq!(f.text(), text);
        assert!(f.backups().is_empty());
    }

    #[test]
    fn a_file_that_is_not_an_object_is_refused() {
        for text in ["[]", "{not json", ""] {
            let f = Fixture::new(text);
            assert!(matches!(
                f.store.prepare(vec![disable("/p", "x")]),
                Err(StoreError::Invalid(_))
            ));
            assert_eq!(f.text(), text);
        }
    }

    #[test]
    fn a_missing_file_is_refused_not_created() {
        let f = Fixture::new("{}");
        std::fs::remove_file(&f.store.path).unwrap();
        assert!(matches!(
            f.store.prepare(vec![disable("/p", "x")]),
            Err(StoreError::Invalid(_))
        ));
        assert!(!f.store.path.exists());
    }

    #[test]
    fn a_change_that_changes_nothing_writes_and_backs_up_nothing() {
        let f = Fixture::new(&pretty(&sample()));
        let before = f.text();
        let plan = f
            .store
            .prepare(vec![enable("C:/work/other", "never-there")])
            .unwrap();
        assert!(plan.is_empty());
        assert_eq!(f.store.apply(&plan).unwrap().backup, None);
        assert_eq!(f.text(), before);
        assert!(f.backups().is_empty());
    }

    #[test]
    fn the_plan_shows_only_names_never_server_settings() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        let shown = format!("{:?}", plan.changes());
        assert!(shown.contains("gateway"));
        assert!(!shown.contains("s3cret"), "{shown}");
        assert_eq!(
            plan.changes()[0].lines,
            [
                "  projects[\"C:/work/app\"].disabledMcpServers",
                "+ gateway"
            ]
        );
    }

    #[test]
    fn names_in_the_plan_are_sanitised() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "a\u{1b}[31mb\u{202e}c")])
            .unwrap();
        let shown = plan.changes()[0].lines.join("\n");
        assert!(
            !shown.contains('\u{1b}') && !shown.contains('\u{202e}'),
            "{shown:?}"
        );
    }

    #[test]
    fn the_old_bytes_are_saved_before_the_file_is_replaced() {
        let text = pretty(&sample());
        let f = Fixture::new(&text);
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        let applied = f.store.apply(&plan).unwrap();
        let backup = applied.backup.unwrap();
        assert_eq!(std::fs::read_to_string(backup).unwrap(), text);
        assert_eq!(f.backups().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn backups_and_their_folder_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        let backup = f.store.apply(&plan).unwrap().backup.unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&backup), 0o600);
        assert_eq!(mode(&f.store.backup_dir), 0o700);
    }

    #[test]
    fn only_the_newest_backups_are_kept() {
        let f = Fixture::new(&pretty(&sample()));
        let mut store = Docstore::new(f.store.path.clone(), f.store.backup_dir.clone());
        store.keep_backups = 3;
        for i in 0..7 {
            let op = if i % 2 == 0 {
                disable("C:/work/app", "g")
            } else {
                enable("C:/work/app", "g")
            };
            let plan = store.prepare(vec![op]).unwrap();
            store.apply(&plan).unwrap();
        }
        let kept = f.backups();
        assert_eq!(kept.len(), 3);
        // The newest backup holds the state before the last write: `g` was disabled by write 7 (i = 6).
        let last = std::fs::read_to_string(kept.last().unwrap()).unwrap();
        let v: Value = serde_json::from_str(&last).unwrap();
        assert_eq!(
            v["projects"]["C:/work/app"]["disabledMcpServers"],
            json!(["old"])
        );
    }

    #[test]
    fn a_held_lock_aborts_without_writing() {
        let text = pretty(&sample());
        let f = Fixture::new(&text);
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        std::fs::create_dir(f.lock()).unwrap();
        assert_eq!(f.store.apply(&plan), Err(StoreError::Locked));
        assert_eq!(f.text(), text);
        assert!(f.backups().is_empty());
        // Someone else's lock is left alone.
        assert!(f.lock().is_dir());
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        std::fs::create_dir(f.lock()).unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        filetime::set_file_mtime(f.lock(), filetime::FileTime::from_system_time(old)).unwrap();
        f.store.apply(&plan).unwrap();
        assert!(
            f.json()["projects"]["C:/work/app"]["disabledMcpServers"]
                .as_array()
                .unwrap()
                .contains(&json!("gateway"))
        );
        assert!(!f.lock().exists(), "our own lock is released");
    }

    #[test]
    fn a_fresh_lock_is_waited_for_and_then_used() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        std::fs::create_dir(f.lock()).unwrap();
        let lock = f.lock();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            std::fs::remove_dir(lock).unwrap();
        });
        f.store.apply(&plan).unwrap();
        releaser.join().unwrap();
        assert!(f.leftovers().is_empty(), "{:?}", f.leftovers());
    }

    #[test]
    fn no_lock_or_temp_file_is_left_after_success_or_failure() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        assert!(f.leftovers().is_empty(), "{:?}", f.leftovers());
        // A failing apply (the file has become invalid) releases the lock too.
        std::fs::write(&f.store.path, "{broken").unwrap();
        assert!(f.store.apply(&plan).is_err());
        assert!(f.leftovers().is_empty(), "{:?}", f.leftovers());
    }

    #[test]
    fn a_change_made_by_claude_in_the_meantime_survives() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        // Claude saves something unrelated after the plan was shown.
        let mut theirs = sample();
        theirs["numStartups"] = json!(13);
        theirs["newKeyFromClaude"] = json!({"x": [1, 2]});
        std::fs::write(&f.store.path, pretty(&theirs)).unwrap();
        f.store.apply(&plan).unwrap();
        let j = f.json();
        assert_eq!(j["numStartups"], 13);
        assert_eq!(j["newKeyFromClaude"], json!({"x": [1, 2]}));
        assert_eq!(
            j["projects"]["C:/work/app"]["disabledMcpServers"],
            json!(["old", "gateway"])
        );
    }

    #[test]
    fn a_change_that_alters_what_the_user_approved_is_rejected() {
        let f = Fixture::new(&pretty(&sample()));
        // The user approved removing `old`.
        let plan = f.store.prepare(vec![enable("C:/work/app", "old")]).unwrap();
        // Meanwhile it was removed and `other` appeared; the plan would now do nothing, which is not what was shown.
        let mut theirs = sample();
        theirs["projects"]["C:/work/app"]["disabledMcpServers"] = json!(["other"]);
        let text = pretty(&theirs);
        std::fs::write(&f.store.path, &text).unwrap();
        assert_eq!(f.store.apply(&plan), Err(StoreError::Changed));
        assert_eq!(f.text(), text);
    }

    #[test]
    fn verify_puts_the_change_back_when_claude_overwrote_it() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        // Claude wrote an older copy without our lock, keeping its own newer change.
        let mut theirs = sample();
        theirs["numStartups"] = json!(99);
        std::fs::write(&f.store.path, pretty(&theirs)).unwrap();
        assert_eq!(f.store.verify(&plan), Ok(true));
        let j = f.json();
        assert_eq!(j["numStartups"], 99);
        assert_eq!(
            j["projects"]["C:/work/app"]["disabledMcpServers"],
            json!(["old", "gateway"])
        );
    }

    #[test]
    fn verify_does_not_write_when_the_change_is_intact() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        let backups = f.backups().len();
        assert_eq!(f.store.verify(&plan), Ok(false));
        assert_eq!(f.backups().len(), backups);
    }

    #[test]
    fn verify_gives_up_when_the_change_keeps_disappearing() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        // A hostile writer: every time we look, our change is gone. Simulated by a file that rejects the op.
        std::fs::write(
            &f.store.path,
            pretty(&json!({"projects": {"C:/work/app": {"disabledMcpServers": 5}}})),
        )
        .unwrap();
        assert!(matches!(f.store.verify(&plan), Err(StoreError::Invalid(_))));
    }

    #[test]
    fn style_is_kept_or_defaults_to_claudes() {
        // Default: 2 spaces, LF, no trailing newline.
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/other", "g")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        assert!(!f.text().ends_with('\n') && !f.text().contains('\r'));
        assert!(f.text().contains("\n  \"zeta\": 1.5"));
        // A file that had a BOM, CRLF and a final newline keeps them.
        // (This input used to end in a bare LF after CRLF lines, which the old full rewrite normalised. Edits are
        // now made in place, so a mixed file stays mixed; that case is covered by the test below.)
        let original = format!("\u{feff}{}\r\n", pretty(&sample()).replace('\n', "\r\n"));
        let f = Fixture::new(&original);
        let plan = f
            .store
            .prepare(vec![disable("C:/work/other", "g")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        let bytes = std::fs::read(&f.store.path).unwrap();
        assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with("}\r\n") || text.ends_with("}\n"));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
    }

    #[test]
    fn mixed_line_endings_outside_the_change_are_left_as_they_are() {
        let original = format!("{}\n", pretty(&sample()).replace('\n', "\r\n"));
        let f = Fixture::new(&original);
        let plan = f.store.prepare(vec![disable("C:/work/app", "g")]).unwrap();
        f.store.apply(&plan).unwrap();
        let text = f.text();
        assert!(text.ends_with("}\n") && !text.ends_with("}\r\n"));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count() + 1);
    }

    #[test]
    fn floats_and_strings_survive_a_round_trip() {
        let text = "{\n  \"f\": 0.1,\n  \"g\": 1.5,\n  \"h\": 1e-7,\n  \"s\": \"é ü \\\"q\\\" \\\\ \\u0001\",\n  \"projects\": {\n    \"/p\": {}\n  }\n}";
        let f = Fixture::new(text);
        let plan = f.store.prepare(vec![disable("/p", "x")]).unwrap();
        f.store.apply(&plan).unwrap();
        let j = f.json();
        assert_eq!(j["f"].as_f64(), Some(0.1));
        assert_eq!(j["h"].as_f64(), Some(1e-7));
        assert_eq!(j["s"], "é ü \"q\" \\ \u{1}");
        assert!(f.text().contains("\"f\": 0.1,"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_is_refused() {
        let f = Fixture::new("{}");
        let real = f.dir.path().join("real.json");
        std::fs::write(&real, pretty(&sample())).unwrap();
        std::fs::remove_file(&f.store.path).unwrap();
        std::os::unix::fs::symlink(&real, &f.store.path).unwrap();
        assert!(f.store.prepare(vec![disable("C:/work/app", "g")]).is_err());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), pretty(&sample()));
    }

    #[test]
    fn two_writers_at_once_both_get_their_change_in() {
        let f = Fixture::new(&pretty(&json!({"projects": {"/p": {}}})));
        let store = std::sync::Arc::new(f.store);
        let mut handles = Vec::new();
        for i in 0..6 {
            let store = std::sync::Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                let plan = store
                    .prepare(vec![disable("/p", &format!("s{i}"))])
                    .unwrap();
                // The lock is polite: contenders wait, and a changed file is re-read.
                loop {
                    match store.apply(&plan) {
                        Err(StoreError::Locked) => continue,
                        other => return other.map(|_| ()),
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let text = std::fs::read_to_string(&store.path).unwrap();
        let j: Value = serde_json::from_str(&text).unwrap();
        let mut names: Vec<_> = j["projects"]["/p"]["disabledMcpServers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["s0", "s1", "s2", "s3", "s4", "s5"]);
        drop(f.dir);
    }

    // ---- approval: the project's .claude/settings.local.json, and rejections in .claude.json

    fn settings_store(dir: &Path) -> Docstore {
        let mut store = Docstore::project_settings(
            dir.join(".claude").join("settings.local.json"),
            dir.join("backups"),
        );
        store.lock_wait = Duration::from_millis(300);
        store.verify_delay = Duration::ZERO;
        store
    }

    fn approve_op(name: &str) -> Op {
        Op::Approve {
            name: name.to_owned(),
        }
    }

    #[test]
    fn approving_creates_the_settings_file_and_folder_when_they_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = settings_store(dir.path());
        let plan = store.prepare(vec![approve_op("ps")]).unwrap();
        assert!(!store.path.exists(), "prepare writes nothing");
        let applied = store.apply(&plan).unwrap();
        assert!(applied.wrote);
        assert_eq!(applied.backup, None, "there was nothing to back up");
        let text = std::fs::read_to_string(&store.path).unwrap();
        assert_eq!(
            text,
            "{\n  \"enabledMcpjsonServers\": [\n    \"ps\"\n  ]\n}\n"
        );
        assert!(
            !store
                .path
                .with_file_name("settings.local.json.lock")
                .exists()
        );
    }

    #[test]
    fn approving_keeps_everything_else_in_an_existing_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        let original = json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "enabledMcpjsonServers": ["a"],
            "disabledMcpjsonServers": ["ps", "z"],
            "zzz": 1.5
        });
        let store = settings_store(dir.path());
        std::fs::write(&store.path, pretty(&original)).unwrap();
        let plan = store.prepare(vec![approve_op("ps")]).unwrap();
        let applied = store.apply(&plan).unwrap();
        let mut expected = original.clone();
        expected["enabledMcpjsonServers"] = json!(["a", "ps"]);
        expected["disabledMcpjsonServers"] = json!(["z"]);
        assert_eq!(
            std::fs::read_to_string(&store.path).unwrap(),
            pretty(&expected)
        );
        // The old file is backed up, and under its own name, not as claude.json.
        let backup = applied.backup.unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), pretty(&original));
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.local-")
        );
        assert_eq!(
            plan.changes()[0].lines,
            [
                "  enabledMcpjsonServers",
                "+ ps",
                "  disabledMcpjsonServers",
                "- ps"
            ]
        );
    }

    #[test]
    fn approving_twice_changes_nothing_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = settings_store(dir.path());
        let plan = store.prepare(vec![approve_op("ps")]).unwrap();
        store.apply(&plan).unwrap();
        let after_first = std::fs::read_to_string(&store.path).unwrap();
        let again = store.prepare(vec![approve_op("ps")]).unwrap();
        assert!(again.is_empty());
        assert!(!store.apply(&again).unwrap().wrote);
        assert_eq!(std::fs::read_to_string(&store.path).unwrap(), after_first);
    }

    #[test]
    fn a_settings_file_of_the_wrong_shape_is_refused_untouched() {
        for original in [
            json!({"enabledMcpjsonServers": "ps"}),
            json!({"disabledMcpjsonServers": {"ps": true}}),
            json!({"enabledMcpjsonServers": [], "disabledMcpjsonServers": 3}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join(".claude")).unwrap();
            let store = settings_store(dir.path());
            std::fs::write(&store.path, pretty(&original)).unwrap();
            assert!(matches!(
                store.prepare(vec![approve_op("ps")]),
                Err(StoreError::Invalid(_))
            ));
            assert_eq!(
                std::fs::read_to_string(&store.path).unwrap(),
                pretty(&original)
            );
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        let store = settings_store(dir.path());
        std::fs::write(&store.path, "[]").unwrap();
        assert!(store.prepare(vec![approve_op("ps")]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_claude_folder_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(".claude")).unwrap();
        let store = settings_store(dir.path());
        let plan = store.prepare(vec![approve_op("ps")]).unwrap();
        assert!(matches!(store.apply(&plan), Err(StoreError::Invalid(_))));
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
        // A link in place of the file itself is refused too.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, "{}").unwrap();
        let store = settings_store(dir.path());
        std::os::unix::fs::symlink(&target, &store.path).unwrap();
        assert!(store.prepare(vec![approve_op("ps")]).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}");
    }

    #[test]
    fn a_settings_store_refuses_claude_json_ops_and_the_other_way_round() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings_store(dir.path());
        assert!(settings.prepare(vec![disable("/p", "x")]).is_err());
        let f = Fixture::new(&pretty(&sample()));
        assert!(f.store.prepare(vec![approve_op("x")]).is_err());
        assert!(
            !dir.path().join(".claude").exists(),
            "a refused plan creates nothing"
        );
    }

    #[test]
    fn settings_and_main_backups_do_not_push_each_other_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = settings_store(dir.path());
        store.keep_backups = 2;
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        std::fs::write(&store.path, "{}").unwrap();
        let mut main = Docstore::new(dir.path().join(".claude.json"), dir.path().join("backups"));
        main.keep_backups = 2;
        std::fs::write(&main.path, pretty(&sample())).unwrap();
        for i in 0..4 {
            let plan = store.prepare(vec![approve_op(&format!("s{i}"))]).unwrap();
            store.apply(&plan).unwrap();
            let op = if i % 2 == 0 {
                disable("C:/work/app", "g")
            } else {
                enable("C:/work/app", "g")
            };
            let plan = main.prepare(vec![op]).unwrap();
            main.apply(&plan).unwrap();
        }
        let names: Vec<String> = std::fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("claude.json."))
                .count(),
            2,
            "{names:?}"
        );
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("settings.local-"))
                .count(),
            2,
            "{names:?}"
        );
    }

    #[test]
    fn clearing_a_rejection_touches_only_that_list_in_every_spelling() {
        let f = Fixture::new(&pretty(&json!({"projects": {
            "C:/p": {"disabledMcpjsonServers": ["ps", "z"], "disabledMcpServers": ["ps"]},
            "c:\\p": {"disabledMcpjsonServers": ["ps"]},
            "C:/q": {"disabledMcpjsonServers": ["ps"]}
        }})));
        let op = Op::ClearRejection {
            project: "C:/p".to_owned(),
            name: "ps".to_owned(),
        };
        let plan = f.store.prepare(vec![op.clone()]).unwrap();
        f.store.apply(&plan).unwrap();
        let j = f.json();
        assert_eq!(
            j["projects"]["C:/p"]["disabledMcpjsonServers"],
            json!(["z"])
        );
        assert_eq!(j["projects"]["c:\\p"]["disabledMcpjsonServers"], json!([]));
        assert_eq!(
            j["projects"]["C:/p"]["disabledMcpServers"],
            json!(["ps"]),
            "the on/off toggle is a different list"
        );
        assert_eq!(
            j["projects"]["C:/q"]["disabledMcpjsonServers"],
            json!(["ps"]),
            "another project"
        );
        assert!(f.store.prepare(vec![op]).unwrap().is_empty());
    }

    #[test]
    fn clearing_a_rejection_that_is_not_there_is_not_an_error() {
        let f = Fixture::new(&pretty(
            &json!({"projects": {"/p": {}, "/q": {"disabledMcpjsonServers": "x"}}}),
        ));
        for project in ["/p", "/q"] {
            let op = Op::ClearRejection {
                project: project.to_owned(),
                name: "ps".to_owned(),
            };
            assert!(f.store.prepare(vec![op]).unwrap().is_empty());
        }
        let op = Op::ClearRejection {
            project: "/nowhere".to_owned(),
            name: "ps".to_owned(),
        };
        assert!(matches!(
            f.store.prepare(vec![op]),
            Err(StoreError::Invalid(_))
        ));
    }

    // ---- editing in place: only the changed list's bytes differ

    #[test]
    fn bytes_outside_the_changed_list_are_kept_exactly() {
        // Not the way serde or Claude would write it: four-space indents, escapes, a float with a trailing zero,
        // a number too big for u64, and an entry spread over one line.
        let text = "{\n    \"tipsHistory\": {\"a\": 1.0, \"b\": 1e2, \"big\": 123456789012345678901234567890},\n    \"name\": \"caf\\u00e9 \\/ x\",\n    \"projects\": {\n        \"/p\": {\n            \"allowedTools\": [ ],\n            \"disabledMcpServers\": [\"old\"]\n        },\n        \"/q\": {\"x\": 1}\n    }\n}";
        let f = Fixture::new(text);
        let plan = f.store.prepare(vec![disable("/p", "gw")]).unwrap();
        f.store.apply(&plan).unwrap();
        let after = f.text();
        let start = text.find("[\"old\"]").unwrap();
        assert_eq!(
            &after[..start],
            &text[..start],
            "everything before the list"
        );
        let tail = &text[start + "[\"old\"]".len()..];
        assert!(after.ends_with(tail), "everything after the list: {after}");
        let list = &after[start..after.len() - tail.len()];
        let parsed: Value = serde_json::from_str(list).unwrap();
        assert_eq!(parsed, json!(["old", "gw"]));
    }

    #[test]
    fn a_list_added_to_a_one_line_entry_keeps_the_rest_of_the_file_exactly() {
        let text = "{\"projects\":{\"/q\":{\"x\":1.50},\"/r\":{}},\"z\":\"\\u00e9\"}";
        let f = Fixture::new(text);
        let plan = f
            .store
            .prepare(vec![disable("/q", "a"), disable("/r", "b")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        assert_eq!(
            f.text(),
            "{\"projects\":{\"/q\":{\"x\":1.50,\"disabledMcpServers\":[\"a\"]},\"/r\":{\"disabledMcpServers\":[\"b\"]}},\"z\":\"\\u00e9\"}"
        );
    }

    #[test]
    fn a_project_listed_twice_under_the_same_spelling_is_refused() {
        // serde would keep only the last one and silently drop the other on write.
        let text = "{\"projects\": {\"/p\": {\"a\": 1}, \"/p\": {\"b\": 2}}}";
        let f = Fixture::new(text);
        assert!(matches!(
            f.store.prepare(vec![disable("/p", "x")]),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(f.text(), text);
    }

    #[test]
    fn a_list_present_twice_in_one_entry_is_refused() {
        let text = "{\"projects\": {\"/p\": {\"disabledMcpServers\": [], \"disabledMcpServers\": [\"y\"]}}}";
        let f = Fixture::new(text);
        assert!(matches!(
            f.store.prepare(vec![disable("/p", "x")]),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn crlf_files_get_crlf_in_the_new_lines_too() {
        let text = pretty(&json!({"projects": {"/p": {"a": 1}, "/e": {}}})).replace('\n', "\r\n");
        let f = Fixture::new(&text);
        let plan = f
            .store
            .prepare(vec![disable("/p", "x"), disable("/e", "y")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        let after = f.text();
        assert_eq!(
            after.matches('\n').count(),
            after.matches("\r\n").count(),
            "{after:?}"
        );
        let expected = pretty(&json!({"projects": {
            "/p": {"a": 1, "disabledMcpServers": ["x"]},
            "/e": {"disabledMcpServers": ["y"]}
        }}))
        .replace('\n', "\r\n");
        assert_eq!(after, expected);
    }

    #[cfg(unix)]
    #[test]
    fn the_backup_is_the_old_file_itself_not_a_copy() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new(&pretty(&sample()));
        let old_inode = std::fs::metadata(&f.store.path).unwrap().ino();
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        let backup = f.store.apply(&plan).unwrap().backup.unwrap();
        assert_eq!(std::fs::metadata(&backup).unwrap().ino(), old_inode);
        assert_ne!(std::fs::metadata(&f.store.path).unwrap().ino(), old_inode);
    }

    /// Same results as the Value-based edits, over many generated files and op sequences: the same document, the
    /// same plan lines, and for files written the way Claude writes them, the same bytes.
    #[test]
    fn in_place_edits_match_the_parsed_edits() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let names = ["a", "b", "gw", "x y", "\u{e9}"];
        let spellings = ["C:/w/p", "c:\\w\\p", "C:/w/p/", "/q", "/r"];
        for round in 0..400 {
            let mut projects = serde_json::Map::new();
            for key in spellings {
                if next(4) == 0 {
                    continue;
                }
                let mut entry = serde_json::Map::new();
                if next(2) == 0 {
                    entry.insert("allowedTools".into(), json!(["Bash"]));
                }
                for list in ["disabledMcpServers", "disabledMcpjsonServers"] {
                    match next(4) {
                        0 => {}
                        1 => {
                            entry.insert(list.into(), json!([]));
                        }
                        _ => {
                            let items: Vec<Value> = (0..next(4))
                                .map(|_| match next(6) {
                                    0 => json!(7),
                                    1 => Value::Null,
                                    _ => json!(names[next(names.len() as u64) as usize]),
                                })
                                .collect();
                            entry.insert(list.into(), Value::Array(items));
                        }
                    }
                }
                if next(3) == 0 {
                    entry.insert("history".into(), json!([{"display": "x", "n": 1.5}]));
                }
                projects.insert(key.into(), Value::Object(entry));
            }
            if projects.is_empty() {
                projects.insert("/q".into(), json!({}));
            }
            let doc = json!({"numStartups": 3, "projects": projects, "tail": [1, {"k": "v"}]});
            let keys: Vec<String> = doc["projects"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let ops: Vec<Op> = (0..1 + next(5))
                .map(|_| {
                    let project = keys[next(keys.len() as u64) as usize].clone();
                    let name = names[next(names.len() as u64) as usize].to_owned();
                    if next(3) == 0 {
                        Op::ClearRejection { project, name }
                    } else {
                        Op::SetDisabled {
                            project,
                            name,
                            disabled: next(2) == 0,
                        }
                    }
                })
                .collect();
            for compact in [false, true] {
                let text = if compact {
                    serde_json::to_string(&doc).unwrap()
                } else {
                    pretty(&doc)
                };
                let mut expected = doc.clone();
                let mut expected_lines = Vec::new();
                let oracle: Result<(), super::StoreError> =
                    ops.iter().try_for_each(|op| match op {
                        Op::SetDisabled {
                            project,
                            name,
                            disabled,
                        } => set_disabled(
                            &mut expected,
                            project,
                            name,
                            *disabled,
                            &mut expected_lines,
                        ),
                        Op::ClearRejection { project, name } => {
                            clear_rejection(&mut expected, project, name, &mut expected_lines)
                        }
                        Op::Approve { .. } | Op::RemoveServer { .. } | Op::ReplaceServer { .. } => {
                            unreachable!("not generated")
                        }
                    });
                let got = edit_claude_json(&text, &ops);
                match (oracle, got) {
                    (Ok(()), Ok((splices, mut lines))) => {
                        let mut new_text = text.clone();
                        for (range, with) in splices.unwrap_or_default().iter().rev() {
                            new_text.replace_range(range.clone(), with);
                        }
                        let parsed: Value = serde_json::from_str(&new_text).unwrap();
                        assert_eq!(
                            parsed, expected,
                            "round {round}, compact {compact}, ops {ops:?}"
                        );
                        if !compact {
                            assert_eq!(new_text, pretty(&expected), "round {round}: bytes");
                        }
                        let mut want = expected_lines.clone();
                        want.sort();
                        lines.sort();
                        assert_eq!(lines, want, "round {round}: plan lines");
                    }
                    (Err(_), Err(_)) => {}
                    (oracle, got) => {
                        panic!("round {round}: oracle {oracle:?}, in place {got:?}, ops {ops:?}")
                    }
                }
            }
        }
    }

    #[test]
    fn verify_reads_without_the_lock_when_the_change_is_intact() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        // Claude is busy saving: an intact change needs nothing from it, so verify must not wait or fail.
        std::fs::create_dir(f.lock()).unwrap();
        assert_eq!(f.store.verify(&plan), Ok(false));
        assert!(f.lock().is_dir(), "someone else's lock is left alone");
    }

    #[test]
    fn verify_still_takes_the_lock_to_put_a_change_back() {
        let f = Fixture::new(&pretty(&sample()));
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        f.store.apply(&plan).unwrap();
        let before = pretty(&sample());
        std::fs::write(&f.store.path, &before).unwrap();
        std::fs::create_dir(f.lock()).unwrap();
        assert_eq!(f.store.verify(&plan), Err(StoreError::Locked));
        assert_eq!(f.text(), before, "nothing written without the lock");
    }

    #[test]
    fn large_files_take_the_two_thread_path_with_the_same_results() {
        let history: Vec<Value> = (0..4000)
            .map(|i| json!({"display": format!("{i} {}", "x".repeat(300))}))
            .collect();
        let mut doc = sample();
        doc["projects"]["C:/work/app"]["history"] = Value::Array(history);
        let text = pretty(&doc);
        assert!(text.len() >= PARALLEL_FROM, "{} bytes", text.len());
        let f = Fixture::new(&text);
        let plan = f
            .store
            .prepare(vec![disable("C:/work/app", "gateway")])
            .unwrap();
        assert_eq!(
            plan.base_hash,
            format!("{:x}", Sha256::digest(text.as_bytes()))
        );
        f.store.apply(&plan).unwrap();
        doc["projects"]["C:/work/app"]["disabledMcpServers"] = json!(["old", "gateway"]);
        assert_eq!(f.text(), pretty(&doc));
        // Broken at the very end: found by the check running alongside the scan.
        let broken = format!("{}]", pretty(&doc));
        std::fs::write(&f.store.path, &broken).unwrap();
        let err = f
            .store
            .prepare(vec![disable("C:/work/app", "x")])
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::Invalid(m) if m.contains("not valid JSON")),
            "{err:?}"
        );
    }

    // ---- deleting and replacing servers

    fn servers_doc() -> Value {
        json!({
            "numStartups": 2,
            "mcpServers": {
                "a": {"type": "stdio", "command": "node", "args": ["a.js"]},
                "b": {"type": "http", "url": "https://b.example/mcp", "headers": {"Authorization": "Bearer s3cret"}},
                "c": {"type": "stdio", "command": "c", "env": {"TOKEN": "t0ken", "MODE": "x"}}
            },
            "projects": {
                "C:/w/p": {"mcpServers": {"loc": {"command": "x"}, "other": {"command": "y"}}},
                "c:\\w\\p": {"mcpServers": {"loc": {"command": "x"}}},
                "/q": {"mcpServers": {}}
            },
            "z": 1.5
        })
    }

    fn remove(project: Option<&str>, name: &str) -> Op {
        Op::RemoveServer {
            project: project.map(str::to_owned),
            name: name.to_owned(),
        }
    }

    fn replace(project: Option<&str>, name: &str, entry: Value) -> Op {
        Op::ReplaceServer {
            project: project.map(str::to_owned),
            name: name.to_owned(),
            entry,
        }
    }

    fn apply_ops(f: &Fixture, ops: Vec<Op>) -> Plan {
        let plan = f.store.prepare(ops).unwrap();
        f.store.apply(&plan).unwrap();
        plan
    }

    #[test]
    fn deleting_user_servers_leaves_everything_else_exactly() {
        for victims in [
            vec!["a"],
            vec!["b"],
            vec!["c"],
            vec!["a", "b"],
            vec!["b", "c"],
            vec!["a", "c"],
            vec!["a", "b", "c"],
        ] {
            let f = Fixture::new(&pretty(&servers_doc()));
            let plan = apply_ops(&f, victims.iter().map(|n| remove(None, n)).collect());
            let mut expected = servers_doc();
            for v in &victims {
                expected["mcpServers"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(*v);
            }
            assert_eq!(f.text(), pretty(&expected), "{victims:?}");
            for v in &victims {
                assert!(plan.changes()[0].lines.contains(&format!("- {v}")));
            }
        }
    }

    #[test]
    fn deleting_from_a_one_line_file_keeps_it_on_one_line() {
        let text = r#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y"},"c":{"command":"z"}},"k":1.50}"#;
        for (victims, want) in [
            (
                vec!["a"],
                r#"{"mcpServers":{"b":{"command":"y"},"c":{"command":"z"}},"k":1.50}"#,
            ),
            (
                vec!["c"],
                r#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y"}},"k":1.50}"#,
            ),
            (
                vec!["b", "c"],
                r#"{"mcpServers":{"a":{"command":"x"}},"k":1.50}"#,
            ),
            (vec!["a", "b", "c"], r#"{"mcpServers":{},"k":1.50}"#),
        ] {
            let f = Fixture::new(text);
            apply_ops(&f, victims.iter().map(|n| remove(None, n)).collect());
            assert_eq!(f.text(), want, "{victims:?}");
        }
    }

    #[test]
    fn deleting_a_local_server_covers_every_spelling_of_its_project() {
        let f = Fixture::new(&pretty(&servers_doc()));
        apply_ops(&f, vec![remove(Some("C:/w/p"), "loc")]);
        let mut expected = servers_doc();
        expected["projects"]["C:/w/p"]["mcpServers"]
            .as_object_mut()
            .unwrap()
            .shift_remove("loc");
        expected["projects"]["c:\\w\\p"]["mcpServers"] = json!({});
        assert_eq!(f.text(), pretty(&expected));
    }

    #[test]
    fn deleting_a_server_that_is_not_there_is_refused() {
        let f = Fixture::new(&pretty(&servers_doc()));
        for op in [
            remove(None, "nope"),
            remove(Some("/q"), "loc"),
            remove(Some("/nowhere"), "a"),
        ] {
            assert!(
                matches!(
                    f.store.prepare(vec![op.clone()]),
                    Err(StoreError::Invalid(_))
                ),
                "{op:?}"
            );
        }
        assert_eq!(f.text(), pretty(&servers_doc()));
    }

    #[test]
    fn a_server_defined_twice_is_not_guessed_at() {
        let text = r#"{"mcpServers": {"a": {"command": "x"}, "a": {"command": "y"}}}"#;
        let f = Fixture::new(text);
        assert!(f.store.prepare(vec![remove(None, "a")]).is_err());
        assert!(
            f.store
                .prepare(vec![replace(None, "a", json!({"command": "z"}))])
                .is_err()
        );
    }

    #[test]
    fn replacing_keeps_hidden_values_and_never_shows_them() {
        let f = Fixture::new(&pretty(&servers_doc()));
        let edited = json!({"type": "stdio", "command": "c", "args": ["--fast"], "env": {"TOKEN": entry::MASK, "MODE": "y"}});
        let plan = apply_ops(&f, vec![replace(None, "c", edited)]);
        let mut expected = servers_doc();
        expected["mcpServers"]["c"] = json!({"type": "stdio", "command": "c", "args": ["--fast"], "env": {"TOKEN": "t0ken", "MODE": "y"}});
        assert_eq!(f.text(), pretty(&expected));
        let shown = plan.changes()[0].lines.join("\n");
        assert!(
            !shown.contains("t0ken") && !shown.contains("s3cret"),
            "{shown}"
        );
        assert!(shown.contains("~ c") && shown.contains("--fast"), "{shown}");
    }

    #[test]
    fn a_changed_hidden_value_is_still_shown_as_a_change() {
        let f = Fixture::new(&pretty(&servers_doc()));
        let edited = json!({"type": "http", "url": "https://b.example/mcp", "headers": {"Authorization": "Bearer new"}});
        let plan = apply_ops(&f, vec![replace(None, "b", edited)]);
        let shown = plan.changes()[0].lines.join("\n");
        assert!(shown.contains("hidden"), "{shown}");
        assert!(!shown.contains("Bearer"), "{shown}");
        assert_eq!(
            f.json()["mcpServers"]["b"]["headers"]["Authorization"],
            "Bearer new"
        );
    }

    #[test]
    fn replacing_with_the_same_definition_changes_nothing() {
        let f = Fixture::new(&pretty(&servers_doc()));
        let same = entry::masked(&servers_doc()["mcpServers"]["c"]);
        let plan = f.store.prepare(vec![replace(None, "c", same)]).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn bad_replacements_are_refused_and_nothing_is_written() {
        let text = pretty(&servers_doc());
        let f = Fixture::new(&text);
        for (name, edited) in [
            ("c", json!({"command": "c", "env": {"NEW": entry::MASK}})),
            ("a", json!({"type": "http"})),
            ("a", json!({"command": "x", "oauth": {"clientId": "y"}})),
            ("nope", json!({"command": "x"})),
        ] {
            assert!(
                f.store
                    .prepare(vec![replace(None, name, edited.clone())])
                    .is_err(),
                "{edited}"
            );
        }
        assert_eq!(f.text(), text);
    }

    #[test]
    fn oauth_servers_are_not_deleted_or_replaced_by_klefki() {
        let text = r#"{"mcpServers": {"o": {"type": "http", "url": "https://o.example", "oauth": {"clientId": "c"}}}}"#;
        let f = Fixture::new(text);
        assert!(f.store.prepare(vec![remove(None, "o")]).is_err());
        assert!(
            f.store
                .prepare(vec![replace(
                    None,
                    "o",
                    json!({"type": "http", "url": "https://p.example"})
                )])
                .is_err()
        );
    }

    #[test]
    fn replacing_a_local_server_and_one_line_values() {
        let f = Fixture::new(&pretty(&servers_doc()));
        apply_ops(
            &f,
            vec![replace(
                Some("C:/w/p"),
                "other",
                json!({"command": "z", "args": ["1"]}),
            )],
        );
        let mut expected = servers_doc();
        expected["projects"]["C:/w/p"]["mcpServers"]["other"] =
            json!({"command": "z", "args": ["1"]});
        assert_eq!(f.text(), pretty(&expected));
        let f = Fixture::new(r#"{"mcpServers":{"a":{"command":"x"},"b":1}}"#);
        apply_ops(&f, vec![replace(None, "a", json!({"command": "y"}))]);
        assert_eq!(f.text(), r#"{"mcpServers":{"a":{"command":"y"},"b":1}}"#);
    }

    /// Any set of deletions from any object gives the same document as deleting from the parsed value, and the same
    /// bytes as writing that value pretty.
    #[test]
    fn any_set_of_deletions_matches_the_parsed_result() {
        for n in 1..=6usize {
            for mask in 1u32..(1 << n) {
                let servers: serde_json::Map<String, Value> = (0..n)
                    .map(|i| (format!("s{i}"), json!({"command": format!("c{i}")})))
                    .collect();
                let doc = json!({"mcpServers": servers, "tail": true});
                let victims: Vec<String> = (0..n)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| format!("s{i}"))
                    .collect();
                for compact in [false, true] {
                    let text = if compact {
                        serde_json::to_string(&doc).unwrap()
                    } else {
                        pretty(&doc)
                    };
                    let f = Fixture::new(&text);
                    apply_ops(&f, victims.iter().map(|v| remove(None, v)).collect());
                    let mut expected = doc.clone();
                    for v in &victims {
                        expected["mcpServers"]
                            .as_object_mut()
                            .unwrap()
                            .shift_remove(v);
                    }
                    let want = if compact {
                        serde_json::to_string(&expected).unwrap()
                    } else {
                        pretty(&expected)
                    };
                    assert_eq!(
                        f.text(),
                        want,
                        "n {n}, victims {victims:?}, compact {compact}"
                    );
                }
            }
        }
    }
}
