//! The only surface the UI may use. The UI holds an immutable, sanitised [`Snapshot`].
use std::sync::Arc;

pub use crate::config::ConfigPaths;
pub use crate::engine::{
    Change, Command, Core, Detached, Diagnosis, Event, FileDiff, Hint, PlanView,
};
pub use crate::model::{NameError, ServerName};

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

/// Identifies a row across snapshots, so live results can be matched back to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerId {
    pub scope: Scope,
    pub project: Option<String>,
    pub name: String,
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
    pub shadowed: bool,
    pub casing_collision: bool,
}

impl ServerRow {
    pub fn id(&self) -> ServerId {
        ServerId {
            scope: self.scope,
            project: self.project.clone(),
            name: self.name.clone(),
        }
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
}

/// Starts the engine against the real Claude Code configuration and the real CLI. It loads the snapshot, finds
/// `claude`, and runs live status checks in the background. `wake` is called whenever events are waiting, so the
/// UI can repaint.
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Core {
    crate::engine::start_with(
        crate::engine::EngineConfig {
            paths: ConfigPaths::discover(),
            claude: None,
            home: directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()),
            backup_dir: directories::ProjectDirs::from("dev", "klefki", "klefki")
                .map(|d| d.data_dir().join("backups")),
            verify_delay: std::time::Duration::from_secs(1),
            path_var: std::env::var_os("PATH"),
            cli_env: Vec::new(),
            max_checks: 3,
            check_timeout: std::time::Duration::from_secs(30),
        },
        wake,
    )
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

pub fn load_snapshot_from(paths: &ConfigPaths) -> Arc<Snapshot> {
    Arc::new(crate::config::inventory::load(paths))
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
}
