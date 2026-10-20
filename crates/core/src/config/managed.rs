//! The MCP servers an organisation sets for every user of the machine, read-only (decision 009).
//!
//! Claude Code reads them from a folder only an administrator can write: `/etc/claude-code` on Linux,
//! `/Library/Application Support/ClaudeCode` on macOS and `C:\Program Files\ClaudeCode` on Windows. Two places there
//! name servers:
//! - `managed-mcp.json` (`{"mcpServers": {...}}`, any kind of server). While it exists, Claude Code loads only the
//!   organisation's servers: none of the user's, local, project or plugin servers.
//! - `managedMcpServers` in `managed-settings.json` and in each `managed-settings.d/*.json`, read in name order, a
//!   later file's server replacing an earlier one of the same name. Only servers reached by address (`http`, `sse`)
//!   are taken: a settings file may not name a program to run. A server of `managed-mcp.json` replaces one of the
//!   same name from the settings.
//!
//! An organisation's server replaces a user or local server of the same name. Like any other server it is turned off
//! per project by name in `disabledMcpServers`.
//!
//! Everything here is untrusted, like `.mcp.json`: sizes and counts are limited.
use super::{ConfigPaths, fsx, inventory};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Each file is small; Claude Code's own limits are no larger.
const MAX_FILE: u64 = inventory::MAX_MCP_JSON;
/// Files read from `managed-settings.d`, at most.
const MAX_DROP_INS: usize = 64;
/// Servers kept, at most; as for a `.mcp.json`.
const MAX_SERVERS: usize = 500;

pub(crate) const MCP_FILE: &str = "managed-mcp.json";
pub(crate) const SETTINGS_FILE: &str = "managed-settings.json";
pub(crate) const DROP_IN_DIR: &str = "managed-settings.d";

/// The folder Claude Code reads an organisation's settings from on this system.
pub(crate) fn system_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support/ClaudeCode"))
    } else if cfg!(windows) {
        Some(PathBuf::from(r"C:\Program Files\ClaudeCode"))
    } else if cfg!(unix) {
        Some(PathBuf::from("/etc/claude-code"))
    } else {
        None
    }
}

/// A server the organisation sets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ManagedServer {
    pub name: String,
    pub entry: Value,
    /// The file that sets it.
    pub file: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct Managed {
    /// In the order Claude Code lists them: the settings files', then `managed-mcp.json`'s.
    pub servers: Vec<ManagedServer>,
    /// `managed-mcp.json`, when it exists: then Claude Code loads only `servers`.
    pub only: Option<PathBuf>,
    /// `allowAllClaudeAiMcps: true` in a settings file: claude.ai connectors load even with `managed-mcp.json`.
    pub connectors_too: bool,
}

/// Reads what the organisation sets, if `paths` names its folder. Problems are added to `warnings`, a file at a time.
pub(crate) fn load(paths: &ConfigPaths, warnings: &mut Vec<String>) -> Managed {
    let mut out = Managed::default();
    let Some(dir) = &paths.managed_dir else {
        return out;
    };
    for file in settings_files(dir, warnings) {
        match read(&file) {
            Ok(None) => {}
            Ok(Some(doc)) => {
                out.connectors_too |= doc.get("allowAllClaudeAiMcps") == Some(&Value::Bool(true));
                match doc.get("managedMcpServers") {
                    Some(Value::Object(servers)) => {
                        warnings.extend(add(&mut out.servers, servers, &file, by_address));
                    }
                    None => {}
                    Some(_) => warnings.push(format!(
                        "Ignored \"managedMcpServers\" in {}: it is not an object keyed by server name.",
                        show(&file)
                    )),
                }
            }
            Err(e) => warnings.push(format!("Could not read {}: {e}", show(&file))),
        }
    }
    // A server of `managed-mcp.json` takes the place of a settings server of the same name.
    let mcp = dir.join(MCP_FILE);
    match read(&mcp) {
        Ok(None) => {}
        Ok(Some(file)) => {
            // Even an unusable file takes the user's servers away in Claude Code: it is there.
            out.only = Some(mcp.clone());
            match file.get("mcpServers") {
                Some(Value::Object(servers)) => {
                    warnings.extend(add(&mut out.servers, servers, &mcp, |_| Ok(())));
                }
                None => {}
                Some(_) => warnings.push(format!(
                    "Ignored the servers in {}: \"mcpServers\" is not an object.",
                    show(&mcp)
                )),
            }
        }
        Err(e) => {
            out.only = Some(mcp.clone());
            warnings.push(format!("Could not read {}: {e}", show(&mcp)));
        }
    }
    out
}

/// `managed-settings.json`, then the `.json` files in `managed-settings.d` by name.
pub(crate) fn settings_files(dir: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    let mut files = vec![dir.join(SETTINGS_FILE)];
    let drop_ins = dir.join(DROP_IN_DIR);
    let entries = match std::fs::read_dir(&drop_ins) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return files,
        Err(e) => {
            warnings.push(format!("Could not read {}: {e}", show(&drop_ins)));
            return files;
        }
    };
    let mut names: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    names.sort();
    if names.len() > MAX_DROP_INS {
        warnings.push(format!(
            "Read only the first {MAX_DROP_INS} files in {}.",
            show(&drop_ins)
        ));
        names.truncate(MAX_DROP_INS);
    }
    files.extend(names);
    files
}

/// Adds `servers` from `file`, a later one of the same name replacing an earlier one in place. Returns a warning for
/// each server `check` refuses.
fn add(
    out: &mut Vec<ManagedServer>,
    servers: &Map<String, Value>,
    file: &Path,
    check: impl Fn(&Value) -> Result<(), &'static str>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (name, entry) in servers {
        if let Err(why) = check(entry) {
            warnings.push(format!(
                "Claude Code ignores the server {:?} in {}: {why}",
                crate::sanitize::sanitize_line(name),
                show(file)
            ));
            continue;
        }
        let server = ManagedServer {
            name: name.clone(),
            entry: entry.clone(),
            file: file.to_path_buf(),
        };
        put(out, server);
    }
    warnings
}

/// Adds `server`, or puts it in the place of the one of the same name.
fn put(out: &mut Vec<ManagedServer>, server: ManagedServer) {
    match out.iter().position(|s| s.name == server.name) {
        Some(i) => out[i] = server,
        None if out.len() < MAX_SERVERS => out.push(server),
        None => {}
    }
}

/// Settings files may add a server only by its address.
fn by_address(entry: &Value) -> Result<(), &'static str> {
    let kind = entry.get("type").and_then(Value::as_str);
    let has_url = entry.get("url").and_then(Value::as_str).is_some();
    if matches!(kind, Some("http" | "sse")) && has_url {
        Ok(())
    } else {
        Err(
            "organisation settings can add only a server reached by address (http or sse), never a program to run.",
        )
    }
}

fn read(path: &Path) -> Result<Option<Value>, String> {
    let Some(text) = fsx::read_limited(path, MAX_FILE).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("invalid JSON ({e})"))
}

fn show(path: &Path) -> String {
    crate::sanitize::sanitize_line(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Env {
        dir: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }
        fn paths(&self) -> ConfigPaths {
            let mut paths = ConfigPaths::in_dir(self.dir.path().join("cfg"));
            paths.managed_dir = Some(self.dir.path().join("org"));
            paths
        }
        fn write(&self, rel: &str, v: &Value) {
            let p = self.dir.path().join("org").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, serde_json::to_string(v).unwrap()).unwrap();
        }
        fn load(&self) -> (Managed, Vec<String>) {
            let mut warnings = Vec::new();
            let managed = load(&self.paths(), &mut warnings);
            (managed, warnings)
        }
    }

    fn names(m: &Managed) -> Vec<&str> {
        m.servers.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn without_an_organisation_folder_there_is_nothing() {
        let env = Env::new();
        let (m, warnings) = env.load();
        assert!(m.servers.is_empty() && m.only.is_none());
        assert!(warnings.is_empty());
        // Paths for one folder, as tests and `CLAUDE_CONFIG_DIR` users build them, never look at the system's.
        assert!(ConfigPaths::in_dir(env.dir.path()).managed_dir.is_none());
    }

    #[test]
    fn managed_mcp_json_names_servers_of_any_kind_and_takes_the_others_away() {
        let env = Env::new();
        env.write(
            MCP_FILE,
            &json!({"mcpServers": {
                "run": {"type": "stdio", "command": "node", "args": ["org.js"]},
                "web": {"type": "http", "url": "https://org.example.com/mcp"},
            }}),
        );
        let (m, warnings) = env.load();
        assert_eq!(names(&m), ["run", "web"]);
        assert_eq!(m.only, Some(env.dir.path().join("org").join(MCP_FILE)));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn an_unreadable_managed_mcp_json_still_takes_the_others_away() {
        let env = Env::new();
        let p = env.dir.path().join("org").join(MCP_FILE);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{not json").unwrap();
        let (m, warnings) = env.load();
        assert!(m.servers.is_empty());
        assert_eq!(m.only, Some(p));
        assert!(warnings[0].contains("invalid JSON"), "{warnings:?}");
    }

    #[test]
    fn settings_take_only_servers_reached_by_address_and_say_which_they_ignore() {
        let env = Env::new();
        env.write(
            SETTINGS_FILE,
            &json!({"managedMcpServers": {
                "web": {"type": "http", "url": "https://org.example.com/mcp"},
                "events": {"type": "sse", "url": "https://org.example.com/sse"},
                "run": {"type": "stdio", "command": "node", "args": ["org.js"]},
                "bare": {"command": "node"},
            }}),
        );
        let (m, warnings) = env.load();
        assert_eq!(names(&m), ["web", "events"]);
        assert!(m.only.is_none());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("\"run\"") && warnings[0].contains("never a program"));
        assert!(warnings[1].contains("\"bare\""));
    }

    #[test]
    fn a_later_drop_in_replaces_a_server_of_the_same_name_where_it_was() {
        let env = Env::new();
        env.write(
            SETTINGS_FILE,
            &json!({"managedMcpServers": {
                "a": {"type": "http", "url": "https://a.example.com/1"},
                "b": {"type": "http", "url": "https://b.example.com/1"},
            }}),
        );
        env.write(
            "managed-settings.d/20-later.json",
            &json!({"managedMcpServers": {"a": {"type": "sse", "url": "https://a.example.com/3"}}}),
        );
        env.write(
            "managed-settings.d/10-first.json",
            &json!({"managedMcpServers": {
                "a": {"type": "http", "url": "https://a.example.com/2"},
                "c": {"type": "http", "url": "https://c.example.com/1"},
            }}),
        );
        // Not a settings file.
        env.write(
            "managed-settings.d/notes.txt",
            &json!({"managedMcpServers": {"x": {}}}),
        );
        let (m, warnings) = env.load();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(names(&m), ["a", "b", "c"]);
        assert_eq!(m.servers[0].entry["url"], "https://a.example.com/3");
        assert!(m.servers[0].file.ends_with("20-later.json"));
    }

    #[test]
    fn managed_mcp_json_replaces_a_settings_server_of_the_same_name() {
        let env = Env::new();
        env.write(
            MCP_FILE,
            &json!({"mcpServers": {"x": {"type": "stdio", "command": "node"}, "y": {"type": "stdio", "command": "y"}}}),
        );
        env.write(
            SETTINGS_FILE,
            &json!({"managedMcpServers": {"x": {"type": "http", "url": "https://x.example.com/mcp"}}}),
        );
        let (m, _) = env.load();
        assert_eq!(names(&m), ["x", "y"]);
        assert_eq!(m.servers[0].entry["type"], "stdio");
        assert!(m.servers[0].file.ends_with(MCP_FILE));
    }

    #[test]
    fn a_settings_file_without_servers_or_with_the_wrong_shape_is_said_once() {
        let env = Env::new();
        env.write(SETTINGS_FILE, &json!({"permissions": {}}));
        env.write(
            "managed-settings.d/a.json",
            &json!({"managedMcpServers": [{"name": "x", "transport": "http"}]}),
        );
        let (m, warnings) = env.load();
        assert!(m.servers.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("keyed by server name"));
    }
}
