//! Locating and reading Claude Code's configuration. Read-only for now.
pub(crate) mod definition;
pub(crate) mod docstore;
pub(crate) mod entry;
pub(crate) mod fsx;
pub(crate) mod inventory;
pub(crate) mod jsonspan;
pub(crate) mod managed;
#[cfg(windows)]
pub(crate) mod owner_only;
pub(crate) mod plugins;
pub(crate) mod policy;
pub(crate) mod preflight;
pub(crate) mod relative;
pub(crate) mod secretlike;
pub(crate) mod server_env;
pub(crate) mod timeout;

use std::path::PathBuf;

/// Where Claude Code keeps its state. Honours `CLAUDE_CONFIG_DIR`.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// The `.claude.json` file (user settings, local-scope servers, project state).
    pub claude_json: PathBuf,
    /// The directory with the health and needs-auth caches.
    pub claude_dir: PathBuf,
    /// The folder with the organisation's settings (`managed-mcp.json`, `managed-settings.json`), which Claude Code
    /// reads whatever `CLAUDE_CONFIG_DIR` says. `None`: not looked at.
    pub managed_dir: Option<PathBuf>,
}

impl ConfigPaths {
    pub fn discover() -> Option<Self> {
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            return Some(Self {
                claude_json: dir.join(".claude.json"),
                claude_dir: dir,
                managed_dir: managed_dir(),
            });
        }
        let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
        Some(Self {
            claude_json: home.join(".claude.json"),
            claude_dir: home.join(".claude"),
            managed_dir: managed_dir(),
        })
    }

    /// Paths rooted in one directory, as with `CLAUDE_CONFIG_DIR`.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            claude_json: dir.join(".claude.json"),
            claude_dir: dir,
            managed_dir: None,
        }
    }
}

/// The system's folder for an organisation's settings, or the one `KLEFKI_MANAGED_DIR` names (empty: none), so a
/// test or a try-out never depends on what the machine has.
fn managed_dir() -> Option<PathBuf> {
    match std::env::var_os("KLEFKI_MANAGED_DIR") {
        Some(dir) if dir.is_empty() => None,
        Some(dir) => Some(PathBuf::from(dir)),
        None => managed::system_dir(),
    }
}
