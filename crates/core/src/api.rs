//! The only surface the UI may use. The UI holds an immutable, sanitised [`Snapshot`].
use std::sync::Arc;

pub use crate::activity::{Activity, ActivityEntry, Backup};
pub use crate::config::ConfigPaths;
pub use crate::config::definition::Definition;
pub use crate::engine::{
    Change, Command, Core, Detached, Diagnosis, Event, FileDiff, Focus, Hint, PlanView, Review,
    SignInFailure, SignInStep,
};
pub use crate::export::{Exported, Variable};
pub use crate::export_file::{
    FileChoice, Owner as DialogOwner, SaveError, ShownDialog, file_name as export_file_name,
};
pub use crate::form::{Part, ServerFields, secret_reference};
pub use crate::model::{ConnectionId, NameError, ServerName, Target};
pub use crate::paste::{Pasted, PastedServer};
pub use crate::secrets::{Secret, SecretError};

/// Whether two project keys name the same folder, as the engine compares them: `c:\Repos\x\` is `C:/Repos/x`.
pub fn same_project(a: &str, b: &str) -> bool {
    project_key(a) == project_key(b)
}

/// A project key as [`same_project`] compares it, borrowed when the key is already spelled that way.
pub fn project_key(path: &str) -> std::borrow::Cow<'_, str> {
    crate::config::inventory::project_key(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    User,
    Local,
    Project,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Local => "Local",
            Self::Project => "Project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Http,
    Sse,
    Ws,
    Invalid,
}

impl TransportKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
            Self::Ws => "ws",
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
    /// `status` is what Claude's own cache last said, which can be old, not an answer klefki got itself. Such rows
    /// are checked again in the background; until then the app shows the status as last known.
    pub status_cached: bool,
    /// Which connection this is, from its scope, project key and name as the file has them (the same id as v1's):
    /// the key its secret is stored under.
    pub connection: crate::model::ConnectionId,
    /// Signs in with OAuth: the client id and scopes from its entry (not secrets; the client secret lives in the
    /// keyring and is never read into a row). `None` for a server without an `oauth` entry.
    pub oauth: Option<OAuthInfo>,
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
    pub servers: Vec<ServerRow>,
    /// Problems found while reading the configuration. Already sanitised.
    pub warnings: Vec<String>,
    /// Projects whose entry still has the switch-off list of older Claude Code versions.
    pub legacy_disabled: Vec<LegacyDisabled>,
    /// Definitions Klefki kept because a change removed a server and could not add it back. Oldest first.
    pub kept: Vec<Kept>,
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
/// UI can repaint.
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Core {
    crate::engine::start_with(real_config(None), wake)
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
        cli_env: Vec::new(),
        max_checks: 3,
        check_timeout: std::time::Duration::from_secs(30),
        background_checks: true,
        watch_interval: Some(std::time::Duration::from_secs(1)),
        watch_settle: std::time::Duration::from_millis(300),
        secrets: None,
        open_url: std::sync::Arc::new(open_https),
        open_dialog: std::sync::Arc::new(open_save_dialog),
        sign_in_timeout: std::time::Duration::from_secs(5 * 60),
        changer: None,
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
    directories::ProjectDirs::from("dev", "klefki", "klefki")
        .map(|d| d.data_local_dir().join("backups"))
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
                .filter(|r| r.scope != Scope::User)
                .all(|r| r.project.is_some())
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
