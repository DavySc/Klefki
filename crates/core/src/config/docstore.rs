//! Writes to `~/.claude.json`, next to a running Claude Code (`docs/decisions/001-claude-json-locking.md`), and to
//! a project's `.claude/settings.local.json` when the user approves one of its servers.
//!
//! Every change is a [`Plan`] made from [`Op`]s: `prepare` shows what would change, `apply` re-reads the file under
//! Claude's own lock directory, re-applies the ops to that fresh copy, backs the old bytes up, and replaces the file
//! atomically. Key order, unknown keys and untouched values survive because the ops patch the parsed document.
//! Claude may still write without the lock (it gives up waiting after about 1.5 s), so `verify` re-reads a moment
//! later and re-applies if our change is gone.
use super::fsx;
use super::inventory::{MAX_CLAUDE_JSON, normalise_project};
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
        let current = self.read()?;
        let mut doc = current.parse()?;
        let changes = self.patch(&mut doc, &ops)?;
        Ok(Plan {
            ops,
            base_hash: current.hash(),
            changes,
        })
    }

    /// Applies `ops` to `doc` and reports what changed. Ops meant for the other kind of file are refused.
    fn patch(&self, doc: &mut Value, ops: &[Op]) -> Result<Vec<FileChange>, StoreError> {
        let mut lines = Vec::new();
        for op in ops {
            match (self.kind, op) {
                (
                    Kind::ClaudeJson,
                    Op::SetDisabled {
                        project,
                        name,
                        disabled,
                    },
                ) => set_disabled(doc, project, name, *disabled, &mut lines)?,
                (Kind::ClaudeJson, Op::ClearRejection { project, name }) => {
                    clear_rejection(doc, project, name, &mut lines)?
                }
                (Kind::ProjectSettings, Op::Approve { name }) => approve(doc, name, &mut lines)?,
                _ => {
                    return Err(StoreError::Invalid(
                        "that change does not belong in this file".to_owned(),
                    ));
                }
            }
        }
        let path = match self.kind {
            Kind::ClaudeJson => ".claude.json".to_owned(),
            Kind::ProjectSettings => sanitize(&self.path.display().to_string()),
        };
        Ok(vec![FileChange { path, lines }])
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
        let mut doc = current.parse()?;
        let changes = self.patch(&mut doc, &plan.ops)?;
        if current.hash() != plan.base_hash && changes != plan.changes {
            return Err(StoreError::Changed);
        }
        self.write(&current, &doc, &changes)
    }

    /// Waits `verify_delay`, then makes sure the change is still there. Claude may write without our lock, from a
    /// copy it read before ours; if so the ops are applied again (up to three times). Says whether it had to.
    pub(crate) fn verify(&self, plan: &Plan) -> Result<bool, StoreError> {
        let mut reapplied = false;
        for round in 0..MAX_VERIFY_ROUNDS {
            std::thread::sleep(self.verify_delay);
            let _lock = Lock::acquire(&self.lock_path(), self.lock_wait)?;
            let current = self.read()?;
            let mut doc = current.parse()?;
            let changes = self.patch(&mut doc, &plan.ops)?;
            if changes.iter().all(|c| c.lines.is_empty()) {
                return Ok(reapplied);
            }
            if round + 1 == MAX_VERIFY_ROUNDS {
                break;
            }
            self.write(&current, &doc, &changes)?;
            reapplied = true;
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

    fn write(
        &self,
        current: &Current,
        doc: &Value,
        changes: &[FileChange],
    ) -> Result<Applied, StoreError> {
        if changes.iter().all(|c| c.lines.is_empty()) {
            return Ok(Applied {
                wrote: false,
                backup: None,
            });
        }
        let dir = self.path.parent().ok_or_else(|| {
            StoreError::Invalid("the configuration file has no parent folder".to_owned())
        })?;
        if self.kind == Kind::ProjectSettings {
            ensure_plain_dir(dir)?;
        }
        let backup = if current.missing {
            None
        } else {
            Some(self.backup(current)?)
        };
        let mut tmp = tempfile::Builder::new()
            .prefix(".claude.json.tmp.")
            .tempfile_in(dir)?;
        tmp.write_all(current.render(doc)?.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|e| e.error)?;
        Ok(Applied {
            wrote: true,
            backup,
        })
    }

    /// Saves the bytes about to be replaced, readable by the owner only, and drops the oldest beyond the limit.
    fn backup(&self, current: &Current) -> Result<PathBuf, StoreError> {
        create_private_dir(&self.backup_dir)?;
        let ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut n = 0u32;
        let (path, mut file) = loop {
            let path = self
                .backup_dir
                .join(format!("{}.{ms:016}-{n}.bak", self.backup_label()));
            match create_private_file(&path) {
                Ok(f) => break (path, f),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e.into()),
            }
        };
        file.write_all(current.raw().as_bytes())?;
        file.sync_all()?;
        self.rotate_backups();
        Ok(path)
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

    /// The exact bytes on disk, as text.
    fn raw(&self) -> String {
        if self.bom {
            format!("\u{feff}{}", self.text)
        } else {
            self.text.clone()
        }
    }

    fn hash(&self) -> String {
        format!("{:x}", Sha256::digest(self.raw().as_bytes()))
    }

    /// Claude writes 2-space JSON with LF and no trailing newline; a file that has a BOM, CRLF or a final newline
    /// keeps them.
    fn render(&self, doc: &Value) -> Result<String, StoreError> {
        let mut out = serde_json::to_string_pretty(doc)
            .map_err(|e| StoreError::Invalid(sanitize(&e.to_string())))?;
        if self.text.ends_with('\n') || self.missing {
            out.push('\n');
        }
        if self.text.contains("\r\n") {
            out = out.replace('\n', "\r\n");
        }
        if self.bom {
            out.insert(0, '\u{feff}');
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

fn set_disabled(
    doc: &mut Value,
    project: &str,
    name: &str,
    disabled: bool,
    lines: &mut Vec<String>,
) -> Result<(), StoreError> {
    let wanted = normalise_project(project);
    let projects = doc
        .get_mut("projects")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| StoreError::Invalid("the configuration has no projects".to_owned()))?;
    let mut found = false;
    for (key, entry) in projects.iter_mut() {
        if normalise_project(key) != wanted {
            continue;
        }
        found = true;
        let entry = entry.as_object_mut().ok_or_else(|| {
            StoreError::Invalid(format!("the entry for {} is not an object", sanitize(key)))
        })?;
        let list = entry
            .entry("disabledMcpServers")
            .or_insert_with(|| Value::Array(Vec::new()));
        let list = list.as_array_mut().ok_or_else(|| {
            StoreError::Invalid(format!(
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
        Err(StoreError::Invalid(format!(
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
) -> Result<(), StoreError> {
    let wanted = normalise_project(project);
    let projects = doc
        .get_mut("projects")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| StoreError::Invalid("the configuration has no projects".to_owned()))?;
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
        Err(StoreError::Invalid(format!(
            "{} is not a project Claude Code knows about",
            sanitize(&wanted)
        )))
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
        let original = format!("\u{feff}{}\n", pretty(&sample()).replace('\n', "\r\n"));
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
}
