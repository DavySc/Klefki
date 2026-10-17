//! The only surface the UI may use. The UI holds an immutable, sanitised [`Snapshot`].
use std::sync::Arc;

pub use crate::config::ConfigPaths;
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
    NeedsAuth,
    Failed,
    Unknown,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::NeedsAuth => "Needs auth",
            Self::Failed => "Failed",
            Self::Unknown => "Unknown",
        }
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
    pub enabled: bool,
    pub shadowed: bool,
    pub casing_collision: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub cli_version: Option<String>,
    pub servers: Vec<ServerRow>,
    /// Problems found while reading the configuration. Already sanitised.
    pub warnings: Vec<String>,
}

/// Reads the real Claude Code configuration (read-only).
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
