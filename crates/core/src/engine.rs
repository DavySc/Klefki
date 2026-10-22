//! The engine: one background thread owns the state, a small worker pool runs live status checks, and the UI
//! only sees events. The UI never touches files, processes or the network.
//!
//! This file holds what the UI and the engine exchange ([`Command`], [`Event`], [`Core`]) and the state the
//! engine's parts share ([`Shared`]). The parts: [`command_loop`], the engine thread; [`checks`], the check queue and
//! its workers; [`changes`], preparing and applying plans; [`diagnose`]; [`client_secrets`]; and [`sign_in`].
use crate::api::{ServerId, Snapshot, Status};
use crate::config::ConfigPaths;
use crate::config::inventory::FileKeys;
use crate::diagnose::hints::{HintKind, hints_for};
use crate::secrets::{Secret, SecretStore};
use crate::swap;
use crate::watch::Watch;
use changes::Journal;
use checks::Queue;
use command_loop::engine_main;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::Duration;

mod changes;
mod checks;
mod client_secrets;
mod command_loop;
mod diagnose;
#[cfg(test)]
mod machine;
mod reload;
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
    /// Check one server now, letting it take [`crate::api::LONGER_CHECK`] to connect, more than Claude Code's usual
    /// limit, to tell a server that is slow to start from one that never answers. Its answer always comes with a
    /// [`crate::api::Timing`].
    CheckLonger(ServerId),
    /// The last times Claude Code started this server, read from its own logs (see [`Event::LastStarts`], decision
    /// 013). Starts nothing and writes nothing.
    LastStarts(ServerId),
    /// The line that opens this server in the MCP Inspector, to copy (see [`Event::InspectorCommand`], decision
    /// 014). Reads the definition; starts nothing and writes nothing.
    InspectorCommand(ServerId),
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
    /// The profiles (see [`Event::Profiles`], decision 012). Read only when asked: the Profiles page asks each time it is
    /// opened.
    Profiles,
    /// Keep the profile `name` with `members`, in place of the profile `replaces` when given (to rename it or change its
    /// servers). Answers with [`Event::Profiles`], or [`Event::ProfileRefused`] saying why nothing was kept.
    SaveProfile {
        name: String,
        members: Vec<crate::model::ConnectionId>,
        replaces: Option<String>,
    },
    /// Forget the profile of this name. Answers like [`Command::SaveProfile`].
    DeleteProfile(String),
    /// Look for a newer Klefki release (see [`Event::Update`]), on a thread of its own. Only the release's signed
    /// checksums are fetched, and only when this build has a release key.
    CheckForUpdates,
    /// Open the page of a release that [`Event::Update`] offered.
    OpenReleasePage(crate::update::Version),
    /// Download the release that [`Event::Update`] offered, for this kind of install, and check it against the
    /// release's signed checksums (see [`Event::UpdateDownload`]). [`Core::install_update`] then installs it.
    DownloadUpdate,
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
    /// Copy a user or local server, definition and secrets included, to each of `targets` as `name`, changed as
    /// `adjust` says (its address, and how it signs in). One that signs in with OAuth is copied by Claude Code's own
    /// `claude mcp add-json`, with Klefki's copy of its client secret, or the new client's secret.
    Duplicate {
        id: ServerId,
        name: String,
        targets: Vec<crate::model::Target>,
        adjust: crate::adjust::Adjust,
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
    /// Turn on the servers of the profile `name`, `members` as [`crate::profiles::Profile`] has them, in `project` (a
    /// project as `.claude.json` spells it), or in every project Claude Code knows when `None` (decision 012). With
    /// `only`, also turn off every other server there, so the profile's servers are the only ones Claude Code loads.
    UseProfile {
        name: String,
        members: Vec<crate::model::ConnectionId>,
        project: Option<String>,
        only: bool,
    },
    /// Stop using the profile `name` in `project`, or wherever it was used when `None` (decision 012): each switch
    /// its uses flipped there goes back, while it is still as the profile left it. A switch changed since, or one
    /// another profile in use sets the same way, is left and said. With `delete`, the profile is deleted too once
    /// this is saved.
    StopProfile {
        name: String,
        project: Option<String>,
        delete: bool,
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
    /// When [`Self::nothing_to_do`]: what is already so, one sentence per change that can say it, naming the servers
    /// and where ("loc is already on in a."). Empty otherwise.
    pub already: Vec<String>,
    /// When [`Self::nothing_to_do`]: a profile was used without "only" where other servers are on, so using it with
    /// "only" would change something.
    pub others_on: bool,
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
        /// How long the check took against Claude Code's time limit, when it ran out of time or was given more.
        timing: Option<crate::api::Timing>,
    },
    Diagnosis {
        id: ServerId,
        diagnosis: Diagnosis,
    },
    /// What Claude Code's logs say about the server's last starts, after [`Command::LastStarts`].
    LastStarts {
        id: ServerId,
        starts: crate::api::LastStarts,
    },
    /// The line [`Command::InspectorCommand`] asked for, or why there is none (sanitised).
    InspectorCommand {
        id: ServerId,
        line: Result<crate::inspector::InspectorCommand, String>,
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
    /// The profiles, after [`Command::Profiles`], [`Command::SaveProfile`] or [`Command::DeleteProfile`].
    Profiles(crate::profiles::Profiles),
    /// A profile was not kept or forgotten: why, as a sentence.
    ProfileRefused(String),
    /// Claude Desktop's servers, after [`Command::Desktop`].
    Desktop(crate::desktop::Desktop),
    /// What the Secrets page shows, after [`Command::Secrets`].
    Secrets(crate::secret_uses::Secrets),
    /// Whether [`Command::ForgetUnusedSecret`] deleted the secret, or why not (sanitised).
    UnusedSecretForgotten {
        connection: crate::model::ConnectionId,
        result: Result<(), String>,
    },
    /// What [`Command::CheckForUpdates`] found.
    Update(crate::update::UpdateCheck),
    /// What [`Command::DownloadUpdate`] did.
    UpdateDownload(UpdateDownload),
}

/// The answer to [`Command::DownloadUpdate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDownload {
    /// Downloaded and checked: [`Core::install_update`] installs this version.
    Ready(crate::update::Version),
    /// This copy of Klefki is updated by hand; why, in a sentence.
    Manual(String),
    Failed(String),
}

/// The release a check found and the update downloaded from it, shared by the engine and [`Core::install_update`].
#[derive(Default)]
struct Updating {
    release: Option<crate::update::Release>,
    staged: Option<crate::install::Staged>,
    /// A download is running: another request waits for it rather than downloading twice.
    downloading: bool,
}

/// The UI's handle to the engine.
pub struct Core {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    /// Opens the system's save dialog; `None` for a detached `Core` that opens none.
    open_dialog: Option<crate::export_file::OpenDialog>,
    /// Opens the system's open dialog; `None` for a detached `Core` that opens none.
    pick_dialog: Option<crate::import_file::PickDialog>,
    updating: Arc<Mutex<Updating>>,
    /// Why this copy of Klefki is updated by hand, if it is (see [`crate::install::InstallKind::Manual`]).
    by_hand: Option<String>,
    /// The engine has a release key, so it checks for updates.
    checks_for_updates: bool,
    /// Where downloaded updates wait (see [`EngineConfig::updates_dir`]).
    updates_dir: Option<PathBuf>,
}

impl Core {
    /// Whether this Klefki checks for updates: only a build with the release key built in (decision 009).
    pub fn checks_for_updates(&self) -> bool {
        self.checks_for_updates
    }

    /// Why this copy of Klefki is updated by hand (the MSI, the zip, the `.deb`); `None` when it installs updates
    /// itself.
    pub fn updated_by_hand(&self) -> Option<&str> {
        self.by_hand.as_deref()
    }

    /// Installs the update [`Command::DownloadUpdate`] got ready, here and now: Klefki calls it when it is closing,
    /// when the engine may already be gone. With `relaunch`, the new version is started and the caller should close
    /// Klefki at once (on Windows the installer closes it anyway). Fails when no update is ready. Without `relaunch`
    /// nothing is on screen any more, so a failure is kept, and the next check reports it.
    pub fn install_update(&self, relaunch: bool) -> Result<crate::update::Version, String> {
        let staged = lock(&self.updating)
            .staged
            .take()
            .ok_or("No update has been downloaded.")?;
        if let Err(why) = crate::install::apply(&staged, relaunch) {
            if !relaunch && let Some(dir) = &self.updates_dir {
                crate::install::note_failure(dir, &why);
            }
            return Err(why);
        }
        Ok(staged.version)
    }

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
            updating: Arc::default(),
            by_hand: None,
            checks_for_updates: false,
            updates_dir: None,
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
    /// The environment variables Klefki was started with, which pre-flight checks look `${VAR}` references up in.
    pub vars: Variables,
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
    /// Fetches Klefki's release files from GitHub. Tests serve files of their own, and never reach the network.
    pub releases: Arc<dyn crate::update::Fetch>,
    /// The public key Klefki's releases are signed with; `None` turns update checks off.
    pub release_key: Option<String>,
    /// How this copy of Klefki was installed, and so how it updates.
    pub install: crate::install::InstallKind,
    /// Where downloaded updates wait to be installed; `None` turns downloading off.
    pub updates_dir: Option<PathBuf>,
    /// Where Claude Code keeps its MCP logs (`claude-cli-nodejs` in the cache folder); `None` when unknown. Tests give a
    /// folder of their own.
    pub claude_logs: Option<PathBuf>,
    /// The version this copy of Klefki is, which a release must be newer than. Tests fix it, so their signed test
    /// release stays newer whatever the workspace's version is.
    pub version: crate::update::Version,
}

/// Looks up an environment variable by name.
pub(crate) type Variables = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// Opens an https link in the user's browser.
pub(crate) type OpenUrl = Arc<dyn Fn(&str) -> io::Result<()> + Send + Sync>;

const MAX_CHECK_OUTPUT: usize = 64 * 1024;
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
    let updating = Arc::<Mutex<Updating>>::default();
    let checks_for_updates = config.release_key.is_some();
    let updates_dir = config.updates_dir.clone();
    let by_hand = match &config.install {
        crate::install::InstallKind::Manual(why) => Some(why.clone()),
        _ => None,
    };
    let shared_updating = Arc::clone(&updating);
    std::thread::spawn(move || engine_main(config, input_rx, input_tx, emitter, shared_updating));
    Core {
        tx,
        rx,
        open_dialog,
        pick_dialog,
        updating,
        by_hand,
        checks_for_updates,
        updates_dir,
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
        self.status_timed(id, status, detail, None);
    }

    fn status_timed(
        &self,
        id: ServerId,
        status: Status,
        detail: Option<String>,
        timing: Option<crate::api::Timing>,
    ) {
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
            timing,
        });
    }
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
    updating: Arc<Mutex<Updating>>,
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

fn backup_dir(shared: &Shared) -> Result<PathBuf, String> {
    shared.config.backup_dir.clone().ok_or_else(|| {
        "There is no private folder for backups, so nothing will be changed.".to_owned()
    })
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The Claude Code CLI the last refresh found, if it found one.
fn claude_program(shared: &Shared) -> Option<PathBuf> {
    shared
        .claude
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
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

/// `said` is the text the failure was recognised in, where its error code is looked up.
pub(crate) fn hint(kind: HintKind, said: &str) -> Hint {
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
mod tests;
