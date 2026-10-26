//! The only surface the UI may use. The UI holds an immutable, sanitised [`Snapshot`].
use std::sync::Arc;

pub use crate::activity::{Activity, ActivityEntry, Backup};
pub use crate::adjust::{Adjust, Client, CopyFields, Rebase, shared_base};
pub use crate::config::ConfigPaths;
pub use crate::config::definition::Definition;
pub use crate::desktop::{Desktop, DesktopServer};
pub use crate::engine::{
    Change, Command, Core, Detached, Diagnosis, Event, FileDiff, Focus, Hint, PlanView, Review,
    SignInFailure, SignInStep, UpdateDownload,
};
pub use crate::export::{Exported, Variable};
pub use crate::export_file::{
    FileChoice, Owner as DialogOwner, SaveError, ShownDialog, file_name as export_file_name,
};
pub use crate::form::{Part, ServerFields, secret_reference};
pub use crate::import_file::ImportFile;
pub use crate::inspector::InspectorCommand;
pub use crate::model::{ConnectionId, NameError, ServerName, Target};
pub use crate::ops::{Here, in_project, profile_alone_in, profile_in_use, profiles_in_use_in};
pub use crate::paste::{Pasted, PastedServer};
pub use crate::profile_uses::{ProfileUse, ProfileUses, Switch};
pub use crate::profiles::{Profile, Profiles};
pub use crate::secret_uses::{Place, SecretKind, SecretUse, Secrets, ServerSecrets, UnusedSecret};
pub use crate::secrets::{Secret, SecretError};
pub use crate::start::{NOT_HERE as START_NOT_HERE, StartPlan};
pub use crate::undo::{Record as UndoRecord, Undo};
pub use crate::update::{UpdateCheck, Version};

/// How lists, counts, folders and sentences are put in words, as the engine says them.
pub mod words {
    pub use crate::words::{
        and_list, and_list_upto, and_more, capitalized, comma_list_upto, count, duration,
        folder_name, parent_folder, sentence,
    };
}

/// Whether two project keys name the same folder, as the engine compares them: `c:\Repos\x\` is `C:/Repos/x`.
pub fn same_project(a: &str, b: &str) -> bool {
    project_key(a) == project_key(b)
}

/// A project key as [`same_project`] compares it, borrowed when the key is already spelled that way.
pub fn project_key(path: &str) -> std::borrow::Cow<'_, str> {
    crate::config::inventory::project_key(path)
}

/// In the order lists show them: the organisation's first, as they win over every other of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// A server the organisation sets in the machine's managed settings. Read-only: only an administrator changes it.
    Managed,
    User,
    Local,
    Project,
    /// A server a Claude Code plugin brings. Read-only: it is changed in the plugin.
    Plugin,
    /// A connector of the user's claude.ai account, which Claude Code fetches each session (decision 011). Read-only:
    /// it is changed on claude.ai. Klefki knows the ones Claude Code has connected before, by name.
    ClaudeAi,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Local => "Local",
            Self::Project => "Project",
            Self::Plugin => "Plugin",
            Self::ClaudeAi => "claude.ai",
            Self::Managed => "Organisation",
        }
    }

    /// A server Klefki only switches on and off: no file of the user's holds it.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Plugin | Self::Managed | Self::ClaudeAi)
    }

    /// One row for every project, switched per project (`disabledMcpServers`).
    pub fn is_everywhere(self) -> bool {
        matches!(
            self,
            Self::User | Self::Plugin | Self::Managed | Self::ClaudeAi
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Http,
    Sse,
    Ws,
    /// A claude.ai connector, which Claude Code reaches through claude.ai (decision 011).
    ClaudeAi,
    Invalid,
}

impl TransportKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
            Self::Ws => "ws",
            Self::ClaudeAi => "claude.ai",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Connected,
    /// Connected, but listing the server's tools failed.
    Degraded,
    NeedsAuth,
    Failed,
    /// A project server the user has not approved yet. Claude does not run it.
    PendingApproval,
    NotConfigured,
    /// A live check is running.
    Checking,
    /// Nothing is known: not cached by Claude and not checked yet.
    Unknown,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Degraded => "Connected, tools failed",
            Self::NeedsAuth => "Needs auth",
            Self::Failed => "Failed",
            Self::PendingApproval => "Pending approval",
            Self::NotConfigured => "Not configured",
            Self::Checking => "Checking…",
            Self::Unknown => "Not checked",
        }
    }
}

/// Whether the user has approved a project (`.mcp.json`) server. Separate from the on/off toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Approved,
    /// Claude has not asked yet, or the user has not answered; it does not run the server.
    Pending,
    Rejected,
}

impl Approval {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "Approved",
            Self::Pending => "Pending approval",
            Self::Rejected => "Rejected",
        }
    }
}

/// Identifies a row across snapshots, so live results can be matched back to it. Identity is `connection` alone,
/// made from the keys as the file spells them: two names can look the same once cleaned. `scope`, `project` and
/// `name` are for showing, cleaned and cut like the row's; equality, hashing and order ignore them.
#[derive(Debug, Clone)]
pub struct ServerId {
    pub scope: Scope,
    pub project: Option<String>,
    pub name: String,
    pub connection: crate::model::ConnectionId,
}

impl ServerId {
    /// The id of a server whose project key and name the file spells exactly as given.
    pub fn new(scope: Scope, project: Option<&str>, name: &str) -> Self {
        Self {
            scope,
            project: project
                .map(|p| crate::sanitize::shown(&crate::config::inventory::normalise_project(p))),
            name: crate::sanitize::shown(name),
            connection: crate::model::ConnectionId::new(scope, project, name),
        }
    }
}

impl PartialEq for ServerId {
    fn eq(&self, other: &Self) -> bool {
        self.connection == other.connection
    }
}

impl Eq for ServerId {}

impl std::hash::Hash for ServerId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.connection.hash(state);
    }
}

impl PartialOrd for ServerId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ServerId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.connection.cmp(&other.connection)
    }
}

#[derive(Debug, Clone)]
pub struct ServerRow {
    pub name: String,
    pub scope: Scope,
    /// Project path for local/project scope.
    pub project: Option<String>,
    pub transport: TransportKind,
    /// URL for remote servers, the command line for stdio.
    pub target: String,
    pub status: Status,
    /// The on/off toggle (`/mcp disable`). Per project in Claude Code; a user server is only `false` here when
    /// it is off in every known project (see `disabled_in`).
    pub enabled: bool,
    /// User servers only: in how many known projects the toggle is off (out of `Snapshot::project_count`).
    pub disabled_in: usize,
    /// Those projects (as shown), in the inventory's order, where the inventory names them.
    pub disabled_projects: Vec<String>,
    /// Project (`.mcp.json`) servers only.
    pub approval: Option<Approval>,
    /// Another server of the same name is loaded in its place. For a project server: a local one in its project.
    /// For a user server, which is one row for every project: `true` only when it is hidden in every known
    /// project (see `shadowed_in`).
    pub shadowed: bool,
    /// User servers only: the projects (as shown) where a local or approved project server of the same name is
    /// loaded instead, in the inventory's order. Empty for other rows.
    pub shadowed_in: Vec<String>,
    pub casing_collision: bool,
    /// Stdio servers: the parts of the command (the program, arguments, `NAME=value` variables) that name a path
    /// relative to the folder the server is started in, as shown, at most a few. Claude Code starts a server in the
    /// folder the session started in, so on a user server, which every project loads, these work only in a folder
    /// that has them, while klefki's own check runs in the home folder. Empty for other rows.
    pub relative_paths: Vec<String>,
    /// What in the definition keeps the server from starting, found by reading it and looking on the PATH, never by
    /// running anything: a program that is not there, a `${VAR}` that is not set, and the like. For servers in the
    /// user's own files and `.mcp.json` files; empty for other rows. Sanitised.
    pub preflight: Vec<Preflight>,
    /// `status` is what Claude's own cache last said, which can be old, not an answer klefki got itself. Such rows
    /// are checked again in the background; until then the app shows the status as last known.
    pub status_cached: bool,
    /// Which connection this is, from its scope, project key and name as the file has them (the same id as v1's):
    /// the key its secret is stored under.
    pub connection: crate::model::ConnectionId,
    /// Signs in with OAuth: the client id and scopes from its entry (not secrets; the client secret lives in the
    /// keyring and is never read into a row). `None` for a server without an `oauth` entry.
    pub oauth: Option<OAuthInfo>,
    /// Plugin servers only: which plugin brings it. Such a row is read-only: Klefki holds no file key for it, so no
    /// change can name it.
    pub plugin: Option<PluginInfo>,
    /// Organisation servers only: the file that sets it, as shown (decision 009).
    pub managed: Option<String>,
    /// Claude Code loads this server nowhere, though it is configured: why, as a sentence. Set when the
    /// organisation's `managed-mcp.json` decides which servers load, or its server of the same name replaces this one.
    pub not_loaded: Option<String>,
    /// Policy holds this server back in some projects only (decision 010). Where it does so everywhere it would load,
    /// [`ServerRow::not_loaded`] says so instead.
    pub blocked: Option<Blocked>,
    /// Claude Code may not load this server, for a reason Klefki cannot check: why, as a note rather than a warning.
    /// A claude.ai connector under an allowed list of addresses, as Klefki does not know its address (decision 011).
    pub may_not_load: Option<String>,
}

/// Something in a server's definition that keeps it from starting, or from working, found before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    pub kind: PreflightKind,
    /// What is wrong, as a sentence.
    pub problem: String,
    /// What to do about it, as a sentence.
    pub fix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightKind {
    /// The program is not on the PATH, or not at the path written.
    NotFound,
    /// `C:/` where `/c` was meant: Git Bash rewrote the switch of a `cmd /c` command.
    MangledSwitch,
    /// `docker run` (or podman's) without `-i`: the container never reads what Claude Code sends.
    NoInput,
    /// A `${VAR}` with no default, and no such variable set: Claude Code passes it on as written.
    UnsetVariable,
    /// An environment variable set to nothing.
    EmptyValue,
}

/// How long a check given more time ([`Command::CheckLonger`]) lets the server take to connect.
pub const LONGER_CHECK: std::time::Duration = std::time::Duration::from_secs(120);

/// How long Claude Code waits for a server to connect before it gives up (`MCP_TIMEOUT`), in the folder a check ran
/// in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeLimit {
    pub limit: std::time::Duration,
    pub set_in: LimitSetIn,
    /// `MCP_TIMEOUT` is set there, but to nothing Claude Code reads as a time (`abc`, `0`), so it waits its default.
    pub unreadable: bool,
    /// The settings file to set `MCP_TIMEOUT` in to change the limit, as a message names it: the one that sets it
    /// now, or the user's `settings.json`. `None` when the organisation sets it, or Claude's folder is unknown.
    pub change_in: Option<String>,
}

/// Where the time limit Claude Code uses comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitSetIn {
    /// Nowhere: Claude Code's own 30 s.
    Default,
    /// The environment Klefki was started with, which the Claude Code it starts gets too.
    Environment,
    /// `env` in a settings file the user can change, as a message names it (`~/.claude/settings.json`,
    /// `.claude/settings.local.json`). Sanitised.
    File(String),
    /// `env` in the organisation's settings, which the user cannot change. Sanitised.
    Organisation(String),
}

/// How long a check took against the time limit: for a check that ran out of time, and for every check given more
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    /// From starting Claude Code to its answer, or to giving up on it.
    pub took: std::time::Duration,
    /// The limit Claude Code uses there in a session.
    pub limit: TimeLimit,
    /// The check was given this long instead ([`Command::CheckLonger`]).
    pub given: Option<std::time::Duration>,
    /// It ran out of time: Claude Code gave up on the server, or Klefki on Claude Code.
    pub timed_out: bool,
}

/// The last times Claude Code started a server, from its own logs ([`Command::LastStarts`], decision 013). Read, never
/// written; text is sanitised and what may be a secret is hidden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LastStarts {
    /// The newest start in each folder Claude Code was started in, newest first, at most a few.
    pub starts: Vec<LastStart>,
    /// Other servers whose names Claude Code writes the same in a folder name (`my.server`, `my-server`): their
    /// starts may be among these.
    pub shares_log_with: Vec<String>,
    /// Why nothing could be read, or why the logs are about another server.
    pub problem: Option<String>,
    /// Where Claude Code keeps the logs, as shown, for when there is no start.
    pub looked_in: Option<String>,
}

/// One start of a server, as Claude Code's log for it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastStart {
    /// The folder Claude Code was started in, as shown (`~/code/app`).
    pub folder: String,
    /// That folder as a project of [`Snapshot::projects`] would spell it, to match it with one; never shown.
    pub project: Option<String>,
    /// When, in local time: "Today 14:05".
    pub when: String,
    /// To the second, with its offset from UTC.
    pub exact: String,
    pub outcome: StartOutcome,
    /// What the server printed on stderr, the latest lines last.
    pub server_said: Vec<String>,
    /// What the failure most likely means.
    pub hints: Vec<Hint>,
    /// The log was too large to read whole: only its start and its end were read.
    pub partly: bool,
    /// The log file, as shown.
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// It connected, after this long when the log says.
    Connected { after: Option<std::time::Duration> },
    /// It did not: Claude Code's code for why (`CONNECTION_CLOSED`, `CONNECT_TIMEOUT`, `ENOENT`) and its message.
    Failed {
        code: Option<String>,
        said: String,
        after: Option<std::time::Duration>,
    },
    /// The log says neither: the start is still going on, or it was stopped before it ended.
    Unknown,
}

/// Where policy keeps a server from loading, and why. Sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// The rule and its file, as a sentence (for the first project in `projects`).
    pub why: String,
    /// The projects (as shown), in the inventory's order.
    pub projects: Vec<String>,
    /// A rule on which servers load: policy. `false`: a setting that turns claude.ai connectors off, the user's or
    /// the organisation's (decision 011).
    pub policy: bool,
}

/// Where a plugin server comes from (decision 008). Sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    pub plugin: String,
    pub marketplace: String,
    /// Claude Code leaves this server out because another one has the same command and arguments, or the same
    /// address: that server's name. A user server, or an earlier plugin's, hides it in every project; a local or
    /// approved project server only in its project (see [`ServerRow::shadowed_in`]).
    pub same_as: Option<String>,
    /// The target as the plugin's files write it, when Claude Code runs it otherwise (`${CLAUDE_PLUGIN_ROOT}` in it).
    /// [`ServerRow::target`] is the target as run.
    pub written_target: Option<String>,
    /// The projects (as shown) where its plugin is off, by the project's own `.claude/settings.json` or
    /// `settings.local.json` or, with nothing there, the user's `settings.json`. Claude Code does not load it there.
    pub off_in: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthInfo {
    /// `None` when the client registers itself (no id configured).
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
}

impl ServerRow {
    pub fn id(&self) -> ServerId {
        ServerId {
            scope: self.scope,
            project: self.project.clone(),
            name: self.name.clone(),
            connection: self.connection.clone(),
        }
    }

    /// Whether this row is the server `id` names. Compares in place, without building an id.
    pub fn is(&self, id: &ServerId) -> bool {
        self.connection == id.connection
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub cli_version: Option<String>,
    /// Projects Claude knows about (one per directory, spellings merged).
    pub project_count: usize,
    /// The same projects as paths, in one normal spelling.
    pub projects: Vec<String>,
    /// Those of [`Snapshot::projects`] whose folder is gone, or is one Klefki does not run anything in (a network
    /// folder): a server cannot be diagnosed there ([`crate::engine::Command::DiagnoseIn`]).
    pub unusable_projects: Vec<String>,
    pub servers: Vec<ServerRow>,
    /// Problems found while reading the configuration. Already sanitised.
    pub warnings: Vec<String>,
    /// Projects whose entry still has the switch-off list of older Claude Code versions.
    pub legacy_disabled: Vec<LegacyDisabled>,
    /// Definitions Klefki kept because a change removed a server and could not add it back. Oldest first.
    pub kept: Vec<Kept>,
    /// Klefki's profiles (decision 012), in their order: for the command palette, and to switch a project to one from
    /// the list seen from that project. Empty when the profiles file cannot be read.
    pub profiles: Vec<crate::profiles::Profile>,
    /// What each profile's uses switched, so that stopping it can put that back (decision 012). Read with the
    /// profiles.
    pub profile_uses: crate::profile_uses::ProfileUses,
    /// The organisation's `managed-mcp.json`, as shown, when it exists: then Claude Code loads only the
    /// organisation's servers (decision 009).
    pub organisation_only: Option<String>,
    /// What Claude Code ignores in the organisation's files. Only an administrator can change it, so it is said
    /// apart from [`Snapshot::warnings`]. Already sanitised.
    pub organisation_notes: Vec<String>,
}

/// A server definition Klefki kept in its private folder after a change removed the server and could not put it back
/// (Claude Code's command failed twice). [`Change::PutBack`] adds it back where it was. Sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub id: String,
    pub name: String,
    /// `for the user`, `in app`.
    pub place: String,
    pub signs_in: bool,
    /// The server is back where it was, without settings Claude Code does not keep; Klefki holds the full
    /// definition until it is forgotten. Nothing to put back.
    pub back: bool,
}

/// A project entry with the old `mcpServersDisabled` list. Claude Code no longer reads it, so the servers it names
/// are not switched off by it. Shown read-only; [`Change::MigrateLegacyDisabled`] moves the names into the list
/// Claude Code reads today (`disabledMcpServers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDisabled {
    /// The project, spelled as in [`Snapshot::projects`]: what the change names.
    pub project: String,
    /// The project path, cleaned for showing.
    pub shown: String,
    /// The names on the old list, cleaned for showing, in file order and once each.
    pub names: Vec<String>,
    /// Those of `names` that are not switched off today: migrating switches them off.
    pub turns_off: Vec<String>,
    /// The old list holds something other than names. Removing it would lose that, so migrating is refused.
    pub malformed: bool,
}

/// Starts the engine against the real Claude Code configuration and the real CLI. It loads the snapshot, finds
/// `claude`, and runs live status checks in the background. `wake` is called whenever events are waiting, so the
/// UI can repaint. `claude_only`: every check is Claude Code's, as [`Command::CheckWithClaudeOnly`] sets it later.
pub fn start(claude_only: bool, wake: impl Fn() + Send + Sync + 'static) -> Core {
    // A build without the release key never checks, so it carries no TLS code either (the key is a constant).
    let releases: std::sync::Arc<dyn crate::update::Fetch> = match crate::update::RELEASE_KEY {
        Some(_) => std::sync::Arc::new(crate::update::Https::default()),
        None => std::sync::Arc::new(crate::update::Offline),
    };
    let config = crate::engine::EngineConfig {
        releases,
        release_key: crate::update::RELEASE_KEY.map(str::to_owned),
        install: crate::install::this_install(),
        updates_dir: data_dir().map(|d| d.join("updates")),
        direct_checks: !claude_only,
        ..real_config(None)
    };
    crate::engine::start_with(config, wake)
}

/// Starts the engine for a command-line run: like [`start`], but with the Claude CLI at `claude` when given (it must
/// be a real executable; a script shim is refused as always), and without watching the files for changes, so the
/// snapshot stays the one first read, and without background checks: only what [`Command::Check`] asks for runs.
pub fn start_headless(
    claude: Option<std::path::PathBuf>,
    wake: impl Fn() + Send + Sync + 'static,
) -> Core {
    let mut config = real_config(claude);
    config.watch_interval = None;
    config.background_checks = false;
    crate::engine::start_with(config, wake)
}

fn real_config(claude: Option<std::path::PathBuf>) -> crate::engine::EngineConfig {
    crate::engine::EngineConfig {
        paths: ConfigPaths::discover(),
        claude,
        home: directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()),
        backup_dir: default_backup_dir(),
        verify_delay: std::time::Duration::from_secs(1),
        path_var: std::env::var_os("PATH"),
        vars: std::sync::Arc::new(|name: &str| std::env::var_os(name)),
        cli_env: Vec::new(),
        max_checks: 3,
        check_timeout: std::time::Duration::from_secs(30),
        // Only the app checks servers itself first ([`start`]); the command-line tool asks Claude Code.
        direct_checks: false,
        server_env: std::env::vars_os().collect(),
        background_checks: true,
        watch_interval: Some(std::time::Duration::from_secs(1)),
        watch_settle: std::time::Duration::from_millis(300),
        secrets: None,
        open_url: std::sync::Arc::new(open_https),
        open_dialog: std::sync::Arc::new(open_save_dialog),
        pick_dialog: std::sync::Arc::new(open_pick_dialog),
        sign_in_timeout: std::time::Duration::from_secs(5 * 60),
        changer: None,
        desktop: crate::desktop::DesktopRoots::discover(),
        claude_logs: directories::BaseDirs::new().map(|d| crate::last_start::root(d.cache_dir())),
        // Only the app looks for updates ([`start`]): the command-line tool then carries no TLS code at all.
        releases: std::sync::Arc::new(crate::update::Offline),
        release_key: None,
        install: crate::install::InstallKind::Manual(
            "The command-line tool does not update Klefki.".into(),
        ),
        updates_dir: None,
        version: crate::update::Version::current(),
    }
}

/// Opens the system's own save dialog, starting at `name` in the folder it last used, over `owner` when given: a
/// sheet on macOS, a dialog owned by the window on Windows, and on Linux the portal is told which window it is for.
/// It asks before replacing a file. On macOS the dialog must be opened on the main thread, where the window lives;
/// the answer can be waited for anywhere.
fn open_save_dialog(
    name: &str,
    owner: Option<&dyn crate::export_file::Owner>,
) -> crate::export_file::Answer {
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Save the export")
        .set_file_name(name)
        .add_filter("JSON", &["json"]);
    if let Some(owner) = owner {
        dialog = dialog.set_parent(owner);
    }
    let answer = dialog.save_file();
    Box::pin(async move { answer.await.map(|file| file.path().to_path_buf()) })
}

/// Opens the system's own open dialog, to choose a file to import, over `owner` as [`open_save_dialog`] does.
fn open_pick_dialog(owner: Option<&dyn crate::export_file::Owner>) -> crate::export_file::Answer {
    let mut dialog = rfd::AsyncFileDialog::new().set_title("Import servers");
    // An export can be saved under any name. The macOS panel has no filter menu, so a filter there would only hide
    // such a file.
    if !cfg!(target_os = "macos") {
        dialog = dialog
            .add_filter("JSON", &["json"])
            .add_filter("All files", &["*"]);
    }
    if let Some(owner) = owner {
        dialog = dialog.set_parent(owner);
    }
    let answer = dialog.pick_file();
    Box::pin(async move { answer.await.map(|file| file.path().to_path_buf()) })
}

/// Opens `link` in the user's browser, if it is https (CLAUDE.md: URLs opened are https only). No shell is involved.
fn open_https(link: &str) -> std::io::Result<()> {
    match url::Url::parse(link) {
        Ok(url) if url.scheme() == "https" => open::that_detached(url.as_str()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only https links are opened",
        )),
    }
}

/// Where copies of `.claude.json` are kept before a change. They hold every server's settings, secrets included,
/// so they stay on this machine: on Windows the local, not the roaming, application data folder (which a company
/// may sync to a server or redirect to a network share).
fn default_backup_dir() -> Option<std::path::PathBuf> {
    data_dir().map(|d| d.join("backups"))
}

/// Klefki's own folder for what it keeps (backups, profiles, the Activity log, updates), or the one
/// `KLEFKI_DATA_DIR` names, so a test or a try-out never touches the machine's. Empty: the machine's.
fn data_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("KLEFKI_DATA_DIR") {
        Some(dir) if !dir.is_empty() => Some(std::path::PathBuf::from(dir)),
        _ => directories::ProjectDirs::from("dev", "klefki", "klefki")
            .map(|d| d.data_local_dir().to_path_buf()),
    }
}

/// What Klefki remembers each profile switched (decision 012), for a command-line run, read as the app reads it: put
/// in [`Snapshot::profile_uses`] to tell where a profile is in use. Writes nothing.
pub fn load_profile_uses() -> ProfileUses {
    match default_backup_dir() {
        Some(dir) => crate::profile_uses::load(&dir),
        None => ProfileUses {
            problem: Some(
                "There is no private folder for Klefki's data, so what profiles switched cannot be read."
                    .to_owned(),
            ),
            ..ProfileUses::default()
        },
    }
}

/// The profiles (decision 012) for a command-line run, read as the app reads them. Writes nothing.
pub fn load_profiles() -> Profiles {
    match default_backup_dir() {
        Some(dir) => crate::profiles::load(&dir),
        None => Profiles {
            problem: Some(
                "There is no private folder for Klefki's data, so no profiles can be read."
                    .to_owned(),
            ),
            ..Profiles::default()
        },
    }
}

/// Reads the real Claude Code configuration (read-only), once, without live checks.
pub fn load_snapshot() -> Arc<Snapshot> {
    match ConfigPaths::discover() {
        Some(paths) => load_snapshot_from(&paths),
        None => Arc::new(Snapshot {
            warnings: vec!["Could not determine the home directory.".to_owned()],
            ..Snapshot::default()
        }),
    }
}

/// The definitions of the servers `ids` names, in the real configuration, as one `{"mcpServers": {…}}` block with
/// values that may be secrets replaced by `${VAR}` references: [`Command::Export`] without an engine, for a
/// command-line run. Reads the files; writes nothing.
pub fn export(ids: &[ServerId]) -> Exported {
    let paths = ConfigPaths::discover();
    let inventory = paths
        .as_ref()
        .map(crate::config::inventory::load)
        .unwrap_or_default();
    let sources = crate::export::sources(paths.as_ref(), &inventory.snapshot, &inventory.keys, ids);
    crate::export::export(sources)
}

/// Writes an exported block to `path` for a command-line run, as [`Command::SaveExport`] does: never over an existing
/// file unless `replace`, and never into Claude Code's own configuration or a `.mcp.json`.
pub fn save_export(path: &std::path::Path, text: &str, replace: bool) -> Result<(), SaveError> {
    crate::export_file::save(ConfigPaths::discover().as_ref(), path, text, replace)
}

pub fn load_snapshot_from(paths: &ConfigPaths) -> Arc<Snapshot> {
    Arc::new(crate::config::inventory::load(paths).snapshot)
}

/// What starting Claude Code in `project` with only the servers `members` of the profile `profile` does, from the
/// files as they are now (on/off plan S5). Reads only. `Err` says why there is no start command.
pub fn start_plan(
    profile: &str,
    members: &[ConnectionId],
    project: &str,
) -> Result<StartPlan, String> {
    let paths = ConfigPaths::discover()
        .ok_or("Klefki cannot find your home folder, so it cannot read Claude Code's files.")?;
    crate::start::plan(&paths, profile, members, project, cfg!(windows))
}

/// Starts Claude Code as `plan` says, attached to this terminal: the one named `claude`, or the one the app would use.
/// On Unix this process becomes Claude Code, so this returns only why it could not be started; elsewhere it starts
/// nothing yet ([`START_NOT_HERE`]).
pub fn run_claude(
    plan: &StartPlan,
    claude: Option<&std::path::Path>,
) -> Result<std::convert::Infallible, String> {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    let path_var = std::env::var_os("PATH");
    let program = crate::cli::locate::locate(&crate::cli::locate::Search {
        explicit: claude,
        home: home.as_deref(),
        path_var: path_var.as_deref(),
    })
    .map_err(|e| format!("Claude Code could not be found: {e}"))?;
    crate::start::run(plan, &program)
}

/// Demo data from an embedded fixture, passed through the same sanitising path real data uses.
pub fn fixture_snapshot() -> Arc<Snapshot> {
    Arc::new(crate::fixtures::load())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_snapshot_covers_every_scope_and_transport() {
        let s = fixture_snapshot();
        for scope in [Scope::User, Scope::Local, Scope::Project] {
            assert!(s.servers.iter().any(|r| r.scope == scope), "{scope:?}");
        }
        for t in [
            TransportKind::Stdio,
            TransportKind::Http,
            TransportKind::Sse,
            TransportKind::Invalid,
        ] {
            assert!(s.servers.iter().any(|r| r.transport == t), "{t:?}");
        }
        assert!(s.servers.iter().any(|r| r.shadowed));
        assert!(s.servers.iter().any(|r| !r.shadowed_in.is_empty()));
        assert!(s.servers.iter().any(|r| !r.enabled));
        assert!(s.cli_version.is_some());
    }

    #[test]
    fn fixture_text_is_sanitised() {
        let s = fixture_snapshot();
        for r in &s.servers {
            for text in [&r.name, &r.target] {
                assert!(
                    !text.chars().any(|c| c.is_control() && c != '\n'),
                    "{text:?}"
                );
                assert!(!text.contains('\u{202e}'), "{text:?}");
            }
        }
    }

    #[test]
    fn project_rows_always_carry_a_project_path() {
        let s = fixture_snapshot();
        assert!(
            s.servers
                .iter()
                .filter(|r| matches!(r.scope, Scope::Local | Scope::Project))
                .all(|r| r.project.is_some())
        );
        // A plugin's server is one row for every project, like a user server, and says which plugin brings it.
        let plugins: Vec<_> = s
            .servers
            .iter()
            .filter(|r| r.scope == Scope::Plugin)
            .collect();
        assert!(!plugins.is_empty());
        assert!(
            plugins
                .iter()
                .all(|r| r.project.is_none() && r.plugin.is_some())
        );
        assert!(
            s.servers
                .iter()
                .filter(|r| r.scope != Scope::Plugin)
                .all(|r| r.plugin.is_none())
        );
    }

    #[test]
    fn a_server_id_is_its_connection() {
        // Review of the fix: equality and hashing also compared the shown fields, so two ids for one server could
        // differ while `is()` said they matched.
        let a = ServerId::new(Scope::Local, Some("C:\\x"), "s");
        let mut b = a.clone();
        b.name = "other".into();
        b.project = None;
        b.scope = Scope::User;
        assert_eq!(a, b);
        let set: std::collections::HashSet<ServerId> = [a.clone(), b].into_iter().collect();
        assert_eq!(set.len(), 1);
        // The shown fields are cleaned and cut like a row's.
        let hidden = ServerId::new(Scope::User, None, "a\u{202e}b");
        assert_eq!(hidden.name, "ab");
        assert_ne!(hidden, ServerId::new(Scope::User, None, "ab"));
        assert_eq!(a.project.as_deref(), Some("C:/x"));
    }

    #[test]
    fn only_https_links_are_ever_opened() {
        // Refused before any browser is involved.
        for link in [
            "http://example.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "not a link",
            "",
        ] {
            let err = open_https(link).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{link}");
        }
    }

    #[test]
    fn backups_stay_in_the_local_data_folder() {
        let local = directories::BaseDirs::new()
            .unwrap()
            .data_local_dir()
            .to_path_buf();
        let dir = default_backup_dir().unwrap();
        assert!(dir.starts_with(&local), "{dir:?} is not under {local:?}");
    }
}
