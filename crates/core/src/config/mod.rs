//! Locating and reading Claude Code's configuration. Read-only for now.
pub(crate) mod definition;
pub(crate) mod docstore;
pub(crate) mod entry;
pub(crate) mod fsx;
pub(crate) mod inventory;
pub(crate) mod jsonspan;
#[cfg(windows)]
pub(crate) mod owner_only;
pub(crate) mod secretlike;

use std::path::PathBuf;

/// Where Claude Code keeps its state. Honours `CLAUDE_CONFIG_DIR`.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// The `.claude.json` file (user settings, local-scope servers, project state).
    pub claude_json: PathBuf,
    /// The directory with the health and needs-auth caches.
    pub claude_dir: PathBuf,
}

impl ConfigPaths {
    pub fn discover() -> Option<Self> {
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            return Some(Self {
                claude_json: dir.join(".claude.json"),
                claude_dir: dir,
            });
        }
        let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
        Some(Self {
            claude_json: home.join(".claude.json"),
            claude_dir: home.join(".claude"),
        })
    }

    /// Paths rooted in one directory, as with `CLAUDE_CONFIG_DIR`.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            claude_json: dir.join(".claude.json"),
            claude_dir: dir,
        }
    }
}
