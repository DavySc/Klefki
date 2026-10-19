//! Claude Desktop's servers, read-only.
//!
//! Claude Desktop keeps its servers in `claude_desktop_config.json`. Klefki never writes that file: there is no CLI to
//! delegate a change to, and Claude Desktop rewrites the file itself. It is read only when the Claude Desktop page
//! asks ([`load`]), so it costs nothing at start-up, and read as untrusted, like `.mcp.json`: size-limited, and
//! every string sanitised.
//!
//! Where the file is:
//! - the application data folder: `%APPDATA%\Claude\` on Windows, `~/Library/Application Support/Claude/` on macOS,
//!   `$XDG_CONFIG_HOME/Claude/` on Linux;
//! - on Windows, for the Microsoft Store (MSIX) install, the package's private copy of that folder:
//!   `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\Claude\`. A packaged app reads its private copy first
//!   and falls back to the real folder only when the private copy has no such file
//!   (<https://learn.microsoft.com/windows/msix/desktop/flexible-virtualization>).
use crate::api::TransportKind;
use crate::config::inventory;
use crate::sanitize::{clip, sanitize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) const FILE_NAME: &str = "claude_desktop_config.json";
/// At most this many setting names are shown per server.
const MAX_NAMES: usize = 64;
/// Paths are copied, so they are kept whole up to a length no real path reaches.
const MAX_PATH: usize = 4096;
/// Package folders looked at, at most: there is one per installed Store app.
const MAX_PACKAGES: usize = 4096;

/// The folders Claude Desktop's configuration can be in. Passed in, so tests never look at the real ones.
#[derive(Debug, Clone, Default)]
pub(crate) struct DesktopRoots {
    /// The folder holding the `Claude` application data folder (`%APPDATA%`, `~/Library/Application Support`,
    /// `~/.config`).
    pub roaming: Option<PathBuf>,
    /// Windows only: `%LOCALAPPDATA%\Packages`, where a Store install keeps its private copy.
    pub packages: Option<PathBuf>,
}

impl DesktopRoots {
    pub(crate) fn discover() -> Self {
        let Some(dirs) = directories::BaseDirs::new() else {
            return Self::default();
        };
        Self {
            roaming: Some(dirs.config_dir().to_path_buf()),
            packages: cfg!(windows).then(|| dirs.data_local_dir().join("Packages")),
        }
    }
}

/// What the Claude Desktop page shows ([`crate::api::Event::Desktop`]). Text is sanitised.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Desktop {
    /// The file Claude Desktop reads, when one was found, as a path to show and copy.
    pub file: Option<String>,
    /// The file is the Microsoft Store install's private copy.
    pub packaged: bool,
    /// Its servers, in file order.
    pub servers: Vec<DesktopServer>,
    /// The file was found but could not be read.
    pub problem: Option<String>,
    /// Names defined more than once in the file; the last definition counts, as in any JSON reader.
    pub repeated: Vec<String>,
    /// Other copies of the file that Claude Desktop does not read while the one above exists.
    pub unused: Vec<String>,
    /// Where Klefki looked, for when nothing was found.
    pub looked_in: Vec<String>,
}

/// One server in Claude Desktop's configuration. Values that may be secrets are never part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopServer {
    pub name: String,
    pub transport: TransportKind,
    /// The command line (secret-looking arguments hidden) or the URL (without credentials or query), as the
    /// Connections list shows them; for an invalid entry, what is wrong with it.
    pub target: String,
    /// The names of its environment settings. Their values are not read into this.
    pub env: Vec<String>,
    /// The names of its headers, for a remote server. Their values are not read into this.
    pub headers: Vec<String>,
    /// Why Claude Desktop does not run this entry although it is in the file, when it does not.
    pub not_used: Option<String>,
}

/// Claude Desktop starts the commands in its file but does not connect to remote servers listed there; those are
/// added as connectors (Anthropic, "Building custom connectors via remote MCP servers").
const REMOTE_NOT_USED: &str = "Claude Desktop does not connect to remote servers listed in this file. Add it in \
                               Claude Desktop under Settings, Connectors.";

/// Finds and reads Claude Desktop's configuration. Writes nothing.
pub(crate) fn load(roots: &DesktopRoots) -> Desktop {
    let mut desktop = Desktop::default();
    let roaming = roots
        .roaming
        .as_ref()
        .map(|r| r.join("Claude").join(FILE_NAME));
    let packaged = roots.packages.as_deref().map(packaged_files);
    if let Some(packages) = &roots.packages {
        desktop.looked_in.push(shown_path(
            &packages
                .join("Claude_*")
                .join("LocalCache")
                .join("Roaming")
                .join("Claude")
                .join(FILE_NAME),
        ));
    }
    if let Some(roaming) = &roaming {
        desktop.looked_in.push(shown_path(roaming));
    }
    // A Store install reads its private copy first, so that one counts when it exists; any other copy is not read.
    let mut found: Vec<PathBuf> = packaged.unwrap_or_default();
    desktop.packaged = !found.is_empty();
    found.extend(roaming.filter(|r| exists(r)));
    let mut found = found.into_iter();
    let Some(file) = found.next() else {
        return desktop;
    };
    desktop.unused = found.map(|p| shown_path(&p)).collect();
    desktop.file = Some(shown_path(&file));
    match inventory::read_project_servers(&file) {
        Ok(Some((entries, repeated))) => {
            desktop.servers = entries
                .iter()
                .map(|(name, entry)| server(name, entry))
                .collect();
            desktop.repeated = repeated.iter().map(|n| crate::sanitize::shown(n)).collect();
        }
        // Gone between the look and the read: as if it was never there.
        Ok(None) => {
            desktop.file = None;
            desktop.packaged = false;
            desktop.unused.clear();
        }
        Err(problem) => desktop.problem = Some(sanitize(&problem)),
    }
    desktop
}

/// The copies of the file in Store install folders (`Claude_<id>`), in name order.
fn packaged_files(packages: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(packages) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .take(MAX_PACKAGES)
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|n| {
            n.to_str().is_some_and(|n| {
                n.get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Claude_"))
            })
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            packages
                .join(n)
                .join("LocalCache")
                .join("Roaming")
                .join("Claude")
                .join(FILE_NAME)
        })
        .filter(|p| exists(p))
        .collect()
}

/// Whether something is at `path`, without following a link: a link is found, and then refused when read.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn server(name: &str, entry: &Value) -> DesktopServer {
    let (mut transport, mut target) = inventory::classify(entry);
    let url = entry.get("url").and_then(Value::as_str);
    let command = entry.get("command").and_then(Value::as_str).is_some();
    // An entry with a command is started, whatever else it has.
    let remote = (url.is_some() && !command)
        || matches!(
            transport,
            TransportKind::Http | TransportKind::Sse | TransportKind::Ws
        );
    // A URL with no type is what a remote entry for Claude Desktop usually looks like: shown as the URL it is, not
    // as a command that is missing.
    if let (Some(url), TransportKind::Invalid) = (url, transport)
        && entry.get("type").is_none()
        && entry.get("command").is_none()
        && let Ok(shown) = inventory::display_url(url, &["http", "https"])
    {
        transport = TransportKind::Http;
        target = shown;
    }
    let names = |key: &str| -> Vec<String> {
        entry
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|o| o.keys())
            .take(MAX_NAMES)
            .map(|k| crate::sanitize::shown(k))
            .collect()
    };
    DesktopServer {
        name: crate::sanitize::shown(name),
        transport,
        target,
        env: names("env"),
        headers: names("headers"),
        not_used: remote.then(|| REMOTE_NOT_USED.to_owned()),
    }
}

fn shown_path(path: &Path) -> String {
    clip(&sanitize(&path.display().to_string()), MAX_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Roots {
        dir: tempfile::TempDir,
    }

    impl Roots {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn roots(&self) -> DesktopRoots {
            DesktopRoots {
                roaming: Some(self.dir.path().join("Roaming")),
                packages: Some(self.dir.path().join("Local").join("Packages")),
            }
        }

        fn roaming_file(&self) -> PathBuf {
            self.dir
                .path()
                .join("Roaming")
                .join("Claude")
                .join(FILE_NAME)
        }

        fn packaged_file(&self, package: &str) -> PathBuf {
            self.dir
                .path()
                .join("Local")
                .join("Packages")
                .join(package)
                .join("LocalCache")
                .join("Roaming")
                .join("Claude")
                .join(FILE_NAME)
        }

        fn write(&self, path: &Path, text: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    fn names(d: &Desktop) -> Vec<&str> {
        d.servers.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn nothing_found_says_where_it_looked() {
        let r = Roots::new();
        let d = load(&r.roots());
        assert_eq!(d.file, None);
        assert!(d.servers.is_empty());
        assert_eq!(d.problem, None);
        assert_eq!(d.looked_in.len(), 2, "{:?}", d.looked_in);
        assert!(d.looked_in.iter().any(|p| p.contains("Claude_*")));
        assert!(d.looked_in.iter().all(|p| p.ends_with(FILE_NAME)));
        // Without any folders (no home), nothing is looked at and nothing fails.
        assert_eq!(load(&DesktopRoots::default()), Desktop::default());
    }

    #[test]
    fn servers_are_listed_in_file_order_with_their_command_or_url() {
        let r = Roots::new();
        r.write(
            &r.roaming_file(),
            r#"{
              "globalShortcut": "Ctrl+Space",
              "mcpServers": {
                "zeta": {"command": "npx", "args": ["-y", "@scope/server", "--api-key", "sk-abcdefghijklmnopqrstuvwx"],
                         "env": {"GITHUB_TOKEN": "ghp_secretvalue123", "LOG_LEVEL": "debug"}},
                "alpha": {"type": "http", "url": "https://user:pw@example.com/mcp?token=abc",
                          "headers": {"Authorization": "Bearer xyz"}},
                "broken": {"args": []}
              }
            }"#,
        );
        let d = load(&r.roots());
        assert_eq!(
            d.file.as_deref(),
            Some(shown_path(&r.roaming_file()).as_str())
        );
        assert_eq!(names(&d), ["zeta", "alpha", "broken"]);
        let zeta = &d.servers[0];
        assert_eq!(zeta.transport, TransportKind::Stdio);
        assert!(
            zeta.target.starts_with("npx -y @scope/server --api-key"),
            "{}",
            zeta.target
        );
        assert_eq!(zeta.env, ["GITHUB_TOKEN", "LOG_LEVEL"]);
        let alpha = &d.servers[1];
        assert_eq!(alpha.transport, TransportKind::Http);
        assert_eq!(alpha.target, "https://example.com/mcp?…");
        assert_eq!(alpha.headers, ["Authorization"]);
        assert_eq!(zeta.not_used, None);
        assert_eq!(d.servers[2].transport, TransportKind::Invalid);
        assert_eq!(d.servers[2].not_used, None);
        // No value that may be a secret is anywhere in what is shown.
        let all = format!("{d:?}");
        for secret in [
            "sk-abcdefghijklmnopqrstuvwx",
            "ghp_secretvalue123",
            "debug",
            "Bearer",
            "pw@",
            "token=abc",
        ] {
            assert!(!all.contains(secret), "{secret} in {all}");
        }
    }

    #[test]
    fn remote_entries_are_listed_as_not_used_by_claude_desktop() {
        // Cross-review: Claude Desktop does not connect to remote servers in its file, so listing them as servers
        // it runs was false.
        let r = Roots::new();
        r.write(
            &r.roaming_file(),
            r#"{"mcpServers": {
                "typed": {"type": "http", "url": "https://a.example/mcp"},
                "sse": {"type": "sse", "url": "https://b.example/sse"},
                "bare": {"url": "https://c.example/mcp?key=secret"},
                "bad": {"url": "not a url"},
                "local": {"command": "node", "args": ["s.js"]},
                "both": {"command": "node", "url": "https://d.example/mcp"}
            }}"#,
        );
        let d = load(&r.roots());
        let by = |n: &str| d.servers.iter().find(|s| s.name == n).unwrap();
        for name in ["typed", "sse", "bare", "bad"] {
            let s = by(name);
            assert!(
                s.not_used
                    .as_deref()
                    .is_some_and(|w| w.contains("Connectors")),
                "{name}: {s:?}"
            );
        }
        assert_eq!(by("bare").transport, TransportKind::Http);
        assert_eq!(by("bare").target, "https://c.example/mcp?…");
        assert_eq!(by("bad").transport, TransportKind::Invalid);
        assert_eq!(by("local").not_used, None);
        // Review round 2: a command is started even when a URL is there too.
        assert_eq!(by("both").not_used, None);
        assert_eq!(by("both").transport, TransportKind::Stdio);
    }

    #[test]
    fn the_store_installs_private_copy_wins_over_the_roaming_one() {
        let r = Roots::new();
        r.write(
            &r.roaming_file(),
            r#"{"mcpServers": {"old": {"command": "a"}}}"#,
        );
        r.write(
            &r.packaged_file("Claude_pzs8sxrjxfjjc"),
            r#"{"mcpServers": {"new": {"command": "b"}}}"#,
        );
        // Other Store apps are not looked into.
        r.write(
            &r.packaged_file("Microsoft.Something_8wekyb3d8bbwe"),
            r#"{"mcpServers": {"other": {"command": "c"}}}"#,
        );
        let d = load(&r.roots());
        assert_eq!(
            d.file.as_deref(),
            Some(shown_path(&r.packaged_file("Claude_pzs8sxrjxfjjc")).as_str())
        );
        assert_eq!(names(&d), ["new"]);
        assert!(d.packaged);
        assert_eq!(d.unused, [shown_path(&r.roaming_file())]);
    }

    #[test]
    fn a_store_install_without_its_own_copy_reads_the_roaming_one() {
        let r = Roots::new();
        fs::create_dir_all(r.packaged_file("claude_pzs8sxrjxfjjc").parent().unwrap()).unwrap();
        r.write(
            &r.roaming_file(),
            r#"{"mcpServers": {"old": {"command": "a"}}}"#,
        );
        let d = load(&r.roots());
        assert_eq!(names(&d), ["old"]);
        assert!(!d.packaged);
        assert!(d.unused.is_empty());
    }

    #[test]
    fn a_file_without_servers_is_found_and_empty() {
        let r = Roots::new();
        r.write(&r.roaming_file(), r#"{"preferences": {}}"#);
        let d = load(&r.roots());
        assert!(d.file.is_some());
        assert!(d.servers.is_empty());
        assert_eq!(d.problem, None);
    }

    #[test]
    fn an_unreadable_file_is_a_problem_not_a_crash() {
        for (text, expected) in [
            ("{ not json", "invalid JSON"),
            (r#"{"mcpServers": []}"#, "invalid JSON"),
        ] {
            let r = Roots::new();
            r.write(&r.roaming_file(), text);
            let d = load(&r.roots());
            assert!(d.file.is_some());
            assert!(d.servers.is_empty());
            assert!(
                d.problem.as_deref().is_some_and(|p| p.contains(expected)),
                "{text}: {:?}",
                d.problem
            );
        }
        // Too large to be a configuration: refused before it is read.
        let r = Roots::new();
        r.write(
            &r.roaming_file(),
            &" ".repeat(inventory::MAX_MCP_JSON as usize + 1),
        );
        assert!(
            load(&r.roots())
                .problem
                .is_some_and(|p| p.contains("limit"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_refused_not_followed() {
        let r = Roots::new();
        let elsewhere = r.dir.path().join("elsewhere.json");
        fs::write(&elsewhere, r#"{"mcpServers": {"x": {"command": "a"}}}"#).unwrap();
        fs::create_dir_all(r.roaming_file().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, r.roaming_file()).unwrap();
        let d = load(&r.roots());
        assert!(d.file.is_some());
        assert!(d.servers.is_empty());
        assert!(d.problem.is_some_and(|p| p.contains("symlink")));
    }

    #[test]
    fn names_and_text_from_the_file_are_cleaned() {
        let r = Roots::new();
        r.write(
            &r.roaming_file(),
            "{\"mcpServers\": {\"ev\\u202eil\\u001b[31m\": {\"command\": \"run\\u0007\", \"env\": {\"A\\u202eB\": \"x\"}},\
              \"dup\": {\"command\": \"a\"}, \"dup\": {\"command\": \"b\"}}}",
        );
        let d = load(&r.roots());
        for s in &d.servers {
            for text in std::iter::once(&s.name).chain([&s.target]).chain(&s.env) {
                assert!(
                    !text
                        .chars()
                        .any(|c| c.is_control() || crate::sanitize::is_bad(c)),
                    "{text:?}"
                );
            }
        }
        assert_eq!(names(&d), ["evil", "dup"]);
        assert_eq!(d.servers[1].target, "b");
        assert_eq!(d.repeated, ["dup"]);
    }

    #[test]
    fn many_settings_are_capped() {
        let r = Roots::new();
        let env: serde_json::Map<String, Value> = (0..1000)
            .map(|i| (format!("K{i}"), Value::from("v")))
            .collect();
        r.write(
            &r.roaming_file(),
            &serde_json::json!({"mcpServers": {"s": {"command": "a", "env": env}}}).to_string(),
        );
        assert_eq!(load(&r.roots()).servers[0].env.len(), MAX_NAMES);
    }
}
