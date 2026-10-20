//! The MCP servers of Claude Code's plugins, read-only (decision 008).
//!
//! A plugin is on when `enabledPlugins` in the user's `settings.json` has `"<plugin>@<marketplace>": true`. Its files
//! are in the folder `plugins/installed_plugins.json` names, except for a plugin kept beside its marketplace in a
//! local folder (a `directory` marketplace with a `./` source), which Claude Code reads from that folder. Its servers
//! come from the plugin's `.mcp.json` and from `mcpServers` in `.claude-plugin/plugin.json` (an object, a `./` path to
//! such a file, or a list of both); a file may hold `{"mcpServers": {...}}` or the servers alone. Claude Code names
//! each one `plugin:<plugin>:<server>`, and that is the name its toggle (`disabledMcpServers`), its health caches and
//! its policy rules use.
//!
//! Everything here is untrusted, like `.mcp.json`: sizes and counts are limited, and paths from the files must stay
//! inside the folder they are relative to.
use super::{ConfigPaths, fsx, inventory};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Plugins looked at, at most. Each costs a few small reads at refresh.
const MAX_PLUGINS: usize = 256;
/// Servers kept per plugin, at most; as for a `.mcp.json`.
const MAX_SERVERS: usize = 500;
/// Places listed in a manifest's `mcpServers`, at most.
const MAX_SOURCES: usize = 16;
/// Claude Code's own bookkeeping files and a plugin's manifests are small.
const MAX_FILE: u64 = super::inventory::MAX_MCP_JSON;

/// A server a plugin that is on brings, as its files define it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginServer {
    /// The plugin's name, as `enabledPlugins` spells it before the `@`.
    pub plugin: String,
    pub marketplace: String,
    /// The server's own name in the plugin's files.
    pub server: String,
    /// The definition as the plugin's files write it.
    pub entry: Value,
    /// The plugin's folder, which `${CLAUDE_PLUGIN_ROOT}` stands for.
    pub root: PathBuf,
}

impl PluginServer {
    /// The name Claude Code gives the server.
    pub fn full_name(&self) -> String {
        full_name(&self.plugin, &self.server)
    }

    /// The definition as Claude Code runs it: `${CLAUDE_PLUGIN_ROOT}` replaced by the plugin's folder in every text.
    /// Any other `${…}` comes from Claude Code's own environment, which Klefki does not know, and stays as written.
    pub fn expanded(&self) -> Value {
        let root = self.root.to_string_lossy();
        let mut entry = self.entry.clone();
        expand(&mut entry, &root);
        entry
    }
}

pub(crate) fn full_name(plugin: &str, server: &str) -> String {
    format!("plugin:{plugin}:{server}")
}

fn expand(value: &mut Value, root: &str) {
    match value {
        Value::String(text) if text.contains(ROOT_VAR) => *text = text.replace(ROOT_VAR, root),
        Value::Array(items) => items.iter_mut().for_each(|v| expand(v, root)),
        Value::Object(map) => map.values_mut().for_each(|v| expand(v, root)),
        _ => {}
    }
}

const ROOT_VAR: &str = "${CLAUDE_PLUGIN_ROOT}";

/// The servers of the plugins that are on somewhere, and where.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub servers: Vec<PluginServer>,
    /// Per place, outside every project first and then each project asked for: the servers (indices into
    /// `servers`) of the plugins on there, in the order Claude Code takes them.
    pub on: Vec<Vec<usize>>,
}

/// The servers of every plugin that is on, outside every project (the user's `settings.json`) or in one of
/// `projects` (raw paths, as `.claude.json` keys them), where the project's `.claude/settings.json` and then its
/// `settings.local.json` change what the user's says. What could not be read is said in `warnings`, unsanitised:
/// the caller cleans it.
pub(crate) fn load(paths: &ConfigPaths, projects: &[&str], warnings: &mut Vec<String>) -> Loaded {
    let places = 1 + projects.len();
    let empty = || Loaded {
        servers: Vec::new(),
        on: vec![Vec::new(); places],
    };
    let settings = paths.claude_dir.join("settings.json");
    let user: Vec<(String, bool)> = match read(&settings) {
        Ok(Some(v)) => switches(&v),
        Ok(None) => Vec::new(),
        Err(e) => {
            warnings.push(format!(
                "Could not read {} to find the plugins that are on: {e}",
                settings.display()
            ));
            Vec::new()
        }
    };
    let plugins_dir = paths.claude_dir.join("plugins");
    let installed = read(&plugins_dir.join("installed_plugins.json"))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    // A project can only switch on a plugin that is installed: with none, its files are not even opened.
    let any_installed = installed
        .pointer("/plugins")
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty());
    if !any_installed && !user.iter().any(|(_, on)| *on) {
        return empty();
    }
    // The plugins on in each place, by key, in order.
    let mut on_by_place: Vec<Vec<String>> = vec![on_keys(&user)];
    for project in projects {
        let mut merged = user.clone();
        if any_installed && inventory::is_safe_project_path(project) {
            let dir = Path::new(project).join(".claude");
            for file in ["settings.json", "settings.local.json"] {
                let path = dir.join(file);
                match read(&path) {
                    Ok(Some(v)) => overlay(&mut merged, switches(&v)),
                    Ok(None) => {}
                    Err(e) => warnings.push(format!(
                        "Ignored {} for the plugins that are on: {e}",
                        path.display()
                    )),
                }
            }
        }
        on_by_place.push(on_keys(&merged));
    }
    let mut keys: Vec<&String> = Vec::new();
    for key in on_by_place.iter().flatten() {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return empty();
    }
    let marketplaces = read(&plugins_dir.join("known_marketplaces.json"))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let mut servers = Vec::new();
    // Each plugin's servers, as a range of `servers`.
    let mut ranges: HashMap<&String, std::ops::Range<usize>> = HashMap::new();
    for key in keys.into_iter().take(MAX_PLUGINS) {
        let Some((plugin, marketplace)) = key.split_once('@') else {
            continue;
        };
        let start = servers.len();
        let Some(root) = plugin_root(&installed, &marketplaces, plugin, marketplace) else {
            warnings.push(format!(
                "The plugin {key} is on, but its files were not found; its servers are not listed."
            ));
            continue;
        };
        match servers_of(&root) {
            Ok(found) => servers.extend(found.into_iter().map(|(server, entry)| PluginServer {
                plugin: plugin.to_owned(),
                marketplace: marketplace.to_owned(),
                server,
                entry,
                root: root.clone(),
            })),
            Err(e) => warnings.push(format!("Ignored the servers of the plugin {key}: {e}")),
        }
        ranges.insert(key, start..servers.len());
    }
    let on = on_by_place
        .iter()
        .map(|place| {
            place
                .iter()
                .filter_map(|key| ranges.get(key))
                .flat_map(Clone::clone)
                .collect()
        })
        .collect();
    Loaded { servers, on }
}

/// The keys switched on, in order.
fn on_keys(switches: &[(String, bool)]) -> Vec<String> {
    switches
        .iter()
        .filter(|(_, on)| *on)
        .map(|(key, _)| key.clone())
        .collect()
}

/// `later` over `switches`, as Claude Code merges settings: a key already there keeps its place and takes the new
/// value; a new one goes at the end.
fn overlay(switches: &mut Vec<(String, bool)>, later: Vec<(String, bool)>) {
    for (key, on) in later {
        match switches.iter_mut().find(|(k, _)| *k == key) {
            Some((_, was)) => *was = on,
            None => switches.push((key, on)),
        }
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

/// Each `"<plugin>@<marketplace>": true` or `false` in `enabledPlugins`, in file order. Other values are not a
/// switch and are left out.
fn switches(settings: &Value) -> Vec<(String, bool)> {
    let Some(map) = settings.get("enabledPlugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(key, on)| {
            let (plugin, marketplace) = key.split_once('@')?;
            let on = on.as_bool()?;
            (!plugin.is_empty() && !marketplace.is_empty()).then(|| (key.clone(), on))
        })
        .take(MAX_PLUGINS * 4)
        .collect()
}

/// The folder Claude Code reads the plugin from.
fn plugin_root(
    installed: &Value,
    marketplaces: &Value,
    plugin: &str,
    marketplace: &str,
) -> Option<PathBuf> {
    let known = marketplaces.get(marketplace);
    let local = known
        .and_then(|m| m.pointer("/source/source"))
        .and_then(Value::as_str)
        == Some("directory");
    if local
        && let Some(location) = known
            .and_then(|m| m.get("installLocation"))
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        && let Some(source) = local_source(&location, plugin)
    {
        return inside(&location, &source);
    }
    let entries = installed
        .pointer("/plugins")
        .and_then(|p| p.get(format!("{plugin}@{marketplace}")))
        .and_then(Value::as_array)?;
    // A plugin that is on for the user is the user install; another one is used only when there is none.
    let entry = entries
        .iter()
        .find(|e| e.get("scope").and_then(Value::as_str) == Some("user"))
        .or_else(|| entries.first())?;
    entry
        .get("installPath")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && plain(p))
}

/// A plugin's `./` source in a local marketplace's own list.
fn local_source(location: &Path, plugin: &str) -> Option<String> {
    let list = read(&location.join(".claude-plugin").join("marketplace.json"))
        .ok()
        .flatten()?;
    list.get("plugins")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(plugin))?
        .get("source")?
        .as_str()
        .filter(|s| s.starts_with("./"))
        .map(str::to_owned)
}

/// `rel` under `base`, when it names something inside it: no `..`, no root, no drive.
fn inside(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(path)
}

/// No `..` anywhere in an absolute path from Claude Code's bookkeeping.
fn plain(path: &Path) -> bool {
    !path.components().any(|c| matches!(c, Component::ParentDir))
}

/// The servers of the plugin in `root`: those of `.mcp.json`, then those its manifest lists. A name defined again
/// takes the later definition, in the first one's place: the manifest's wins over `.mcp.json`'s, as in Claude Code.
fn servers_of(root: &Path) -> Result<Vec<(String, Value)>, String> {
    let mut out: Vec<(String, Value)> = Vec::new();
    if let Some(file) = read(&root.join(".mcp.json")).map_err(|e| format!(".mcp.json: {e}"))? {
        add(&mut out, &file)?;
    }
    let manifest = read(&root.join(".claude-plugin").join("plugin.json"))
        .map_err(|e| format!("plugin.json: {e}"))?;
    let listed = manifest.as_ref().and_then(|m| m.get("mcpServers"));
    let sources: Vec<&Value> = match listed {
        Some(Value::Array(items)) => items.iter().take(MAX_SOURCES).collect(),
        Some(other) => vec![other],
        None => Vec::new(),
    };
    for source in sources {
        match source {
            Value::Object(_) => add(&mut out, source)?,
            Value::String(rel) if rel.starts_with("./") => {
                let path =
                    inside(root, rel).ok_or_else(|| format!("{rel}: not inside the plugin"))?;
                if let Some(file) = read(&path).map_err(|e| format!("{rel}: {e}"))? {
                    add(&mut out, &file)?;
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Adds the servers of `file`: its `mcpServers`, or the file itself when it has none.
fn add(out: &mut Vec<(String, Value)>, file: &Value) -> Result<(), String> {
    let servers: &Map<String, Value> = match file.get("mcpServers") {
        Some(Value::Object(m)) => m,
        Some(_) => return Ok(()),
        None => match file.as_object() {
            Some(m) => m,
            None => return Ok(()),
        },
    };
    for (name, entry) in servers {
        if let Some((_, earlier)) = out.iter_mut().find(|(n, _)| n == name) {
            *earlier = entry.clone();
            continue;
        }
        if out.len() >= MAX_SERVERS {
            return Err(format!("more than {MAX_SERVERS} servers"));
        }
        out.push((name.clone(), entry.clone()));
    }
    Ok(())
}

/// What Claude Code compares to find a plugin server that repeats one configured by hand, which it then leaves
/// out: a stdio server's command and arguments, or a remote server's address (scheme and host in lower case, the
/// default port and a trailing `/` dropped; the path and query as written; the type not compared). `None` for an
/// entry that is neither.
pub(crate) fn same_as_key(entry: &Value) -> Option<String> {
    let obj = entry.as_object()?;
    if let Some(command) = obj.get("command").and_then(Value::as_str) {
        let mut parts = vec![Value::String(command.to_owned())];
        if let Some(args) = obj.get("args").and_then(Value::as_array) {
            parts.extend(args.iter().cloned());
        }
        return Some(format!("stdio:{}", Value::Array(parts)));
    }
    let raw = obj.get("url").and_then(Value::as_str)?;
    let url = url::Url::parse(raw.trim()).ok()?;
    let text = url.as_str();
    Some(format!("url:{}", text.strip_suffix('/').unwrap_or(text)))
}

/// [`same_as_key`] with the environment too (but not the variables Claude Code sets for a plugin): what it compares
/// between two plugins, keeping the first.
pub(crate) fn same_as_plugin_key(entry: &Value) -> Option<String> {
    let key = same_as_key(entry)?;
    if !key.starts_with("stdio:") {
        return Some(key);
    }
    let mut env: Vec<(&String, &Value)> = entry
        .get("env")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(k, _)| !k.starts_with("CLAUDE_PLUGIN_"))
        .collect();
    env.sort_by(|a, b| a.0.cmp(b.0));
    if env.is_empty() {
        return Some(key);
    }
    let env: Map<String, Value> = env
        .into_iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Some(format!("{key}:{}", Value::Object(env)))
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
        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }
        fn write(&self, rel: &str, v: &Value) {
            let p = self.path(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, serde_json::to_string(v).unwrap()).unwrap();
        }
        fn load(&self) -> (Vec<PluginServer>, Vec<String>) {
            let mut warnings = Vec::new();
            let loaded = load(&ConfigPaths::in_dir(self.dir.path()), &[], &mut warnings);
            (loaded.servers, warnings)
        }
        /// With these projects: the servers, and per place (outside every project, then each project) the full
        /// names of those loaded there, in order.
        fn load_in(&self, projects: &[&str]) -> (Vec<String>, Vec<Vec<String>>, Vec<String>) {
            let mut warnings = Vec::new();
            let loaded = load(
                &ConfigPaths::in_dir(self.dir.path()),
                projects,
                &mut warnings,
            );
            let on = loaded
                .on
                .iter()
                .map(|place| {
                    place
                        .iter()
                        .map(|&i| loaded.servers[i].full_name())
                        .collect()
                })
                .collect();
            (names(&loaded.servers), on, warnings)
        }
        /// A project folder; `settings` and `local` are its `.claude/settings.json` and `settings.local.json`.
        fn project(&self, name: &str, settings: Option<Value>, local: Option<Value>) -> String {
            for (file, v) in [("settings.json", settings), ("settings.local.json", local)] {
                if let Some(v) = v {
                    self.write(&format!("{name}/.claude/{file}"), &v);
                }
            }
            std::fs::create_dir_all(self.path(name)).unwrap();
            self.path(name).to_string_lossy().into_owned()
        }
        /// A plugin installed from a remote marketplace: its files are in the install folder.
        fn install(&self, key: &str, root: &str) {
            let mut installed =
                std::fs::read_to_string(self.path("plugins/installed_plugins.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .unwrap_or(json!({"version": 2, "plugins": {}}));
            installed["plugins"][key] = json!([{
                "scope": "user",
                "installPath": self.path(root).to_str().unwrap(),
                "version": "1.0.0"
            }]);
            self.write("plugins/installed_plugins.json", &installed);
        }
        fn enable(&self, keys: &[(&str, bool)]) {
            let map: Map<String, Value> = keys
                .iter()
                .map(|(k, on)| ((*k).to_owned(), Value::Bool(*on)))
                .collect();
            self.write("settings.json", &json!({"enabledPlugins": map}));
        }
    }

    fn names(servers: &[PluginServer]) -> Vec<String> {
        servers.iter().map(PluginServer::full_name).collect()
    }

    #[test]
    fn servers_of_plugins_that_are_on_are_named_as_claude_code_names_them() {
        let e = Env::new();
        e.install("p1@mkt", "cache/mkt/p1/1.0.0");
        e.install("p2@mkt", "cache/mkt/p2/1.0.0");
        e.install("off@mkt", "cache/mkt/off/1.0.0");
        e.write(
            "cache/mkt/p1/1.0.0/.mcp.json",
            &json!({"mcpServers": {"alpha": {"command": "node"}, "beta": {"type": "http", "url": "https://x.example.com/mcp"}}}),
        );
        // The servers alone, without `mcpServers` around them.
        e.write(
            "cache/mkt/p2/1.0.0/.mcp.json",
            &json!({"gamma": {"command": "uvx"}}),
        );
        e.write(
            "cache/mkt/off/1.0.0/.mcp.json",
            &json!({"mcpServers": {"x": {"command": "x"}}}),
        );
        e.enable(&[("p1@mkt", true), ("off@mkt", false), ("p2@mkt", true)]);
        let (servers, warnings) = e.load();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            names(&servers),
            ["plugin:p1:alpha", "plugin:p1:beta", "plugin:p2:gamma"]
        );
        assert_eq!(servers[0].marketplace, "mkt");
        assert_eq!(servers[0].entry, json!({"command": "node"}));
    }

    #[test]
    fn a_projects_own_settings_switch_plugins_there_and_the_local_file_wins() {
        let e = Env::new();
        for p in ["p1", "p2", "p3"] {
            e.install(&format!("{p}@m"), p);
            e.write(&format!("{p}/.mcp.json"), &json!({"s": {"command": p}}));
        }
        e.enable(&[("p1@m", true), ("p2@m", false)]);
        let a = e.project(
            "a",
            Some(json!({"enabledPlugins": {"p2@m": true}})),
            Some(json!({"enabledPlugins": {"p3@m": true}})),
        );
        let b = e.project(
            "b",
            Some(json!({"enabledPlugins": {"p2@m": true}})),
            Some(json!({"enabledPlugins": {"p1@m": false, "p2@m": false}})),
        );
        let (servers, on, warnings) = e.load_in(&[&a, &b, "relative/c"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(servers, ["plugin:p1:s", "plugin:p2:s", "plugin:p3:s"]);
        assert_eq!(
            on,
            [
                vec!["plugin:p1:s"],
                vec!["plugin:p1:s", "plugin:p2:s", "plugin:p3:s"],
                vec![],
                // A project that is not a local absolute path: its files are not read.
                vec!["plugin:p1:s"],
            ]
        );
    }

    #[test]
    fn a_plugin_keeps_the_place_it_first_has_in_the_merged_settings() {
        // As Claude Code merges them: a key the user's file has stays where it is there, with the project's value.
        let e = Env::new();
        for p in ["p1", "p2"] {
            e.install(&format!("{p}@m"), p);
            e.write(&format!("{p}/.mcp.json"), &json!({"s": {"command": p}}));
        }
        e.enable(&[("p1@m", false), ("p2@m", true)]);
        let a = e.project("a", Some(json!({"enabledPlugins": {"p1@m": true}})), None);
        let (_, on, _) = e.load_in(&[&a]);
        assert_eq!(on[1], ["plugin:p1:s", "plugin:p2:s"]);
        // A key the user's file does not have comes after its keys.
        e.enable(&[("p2@m", true)]);
        let (_, on, _) = e.load_in(&[&a]);
        assert_eq!(on[1], ["plugin:p2:s", "plugin:p1:s"]);
    }

    #[test]
    fn a_projects_settings_are_read_only_when_a_plugin_is_installed() {
        let e = Env::new();
        let a = e.project("a", None, None);
        std::fs::create_dir_all(e.path("a/.claude")).unwrap();
        std::fs::write(e.path("a/.claude/settings.json"), "{broken").unwrap();
        // Nothing installed: nothing a project could switch on, so nothing read and nothing to say.
        assert_eq!(e.load_in(&[&a]), (vec![], vec![vec![], vec![]], vec![]));

        // Installed: the broken file is named, and the project goes by the user's settings.
        e.install("p1@m", "p1");
        e.write("p1/.mcp.json", &json!({"s": {"command": "x"}}));
        e.enable(&[("p1@m", true)]);
        let (servers, on, warnings) = e.load_in(&[&a]);
        assert_eq!(servers, ["plugin:p1:s"]);
        assert_eq!(on, [vec!["plugin:p1:s"], vec!["plugin:p1:s"]]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("settings.json"), "{warnings:?}");
    }

    #[test]
    fn the_manifest_lists_servers_inline_by_path_or_both() {
        let e = Env::new();
        e.install("p@m", "p");
        e.write(
            "p/.mcp.json",
            &json!({"mcpServers": {"dot": {"command": "a"}}}),
        );
        e.write(
            "p/.claude-plugin/plugin.json",
            &json!({"name": "p", "mcpServers": ["./more.json", {"inline": {"command": "b"}}, "../outside.json"]}),
        );
        e.write(
            "p/more.json",
            &json!({"mcpServers": {"file": {"command": "c"}}}),
        );
        e.write(
            "outside.json",
            &json!({"mcpServers": {"escaped": {"command": "d"}}}),
        );
        e.enable(&[("p@m", true)]);
        let (servers, warnings) = e.load();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            names(&servers),
            ["plugin:p:dot", "plugin:p:file", "plugin:p:inline"]
        );

        e.write(
            "p/.claude-plugin/plugin.json",
            &json!({"mcpServers": "./more.json"}),
        );
        assert_eq!(names(&e.load().0), ["plugin:p:dot", "plugin:p:file"]);
    }

    #[test]
    fn the_manifest_wins_over_mcp_json_for_a_name_both_define() {
        let e = Env::new();
        e.install("p@m", "p");
        e.write(
            "p/.mcp.json",
            &json!({"mcpServers": {"dup": {"command": "echo", "args": ["dup"]}, "only": {"command": "a"}}}),
        );
        e.write(
            "p/.claude-plugin/plugin.json",
            &json!({"mcpServers": {"dup": {"command": "echo", "args": ["manifest"]}}}),
        );
        e.enable(&[("p@m", true)]);
        let (servers, warnings) = e.load();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(names(&servers), ["plugin:p:dup", "plugin:p:only"]);
        assert_eq!(
            servers[0].entry,
            json!({"command": "echo", "args": ["manifest"]})
        );

        // The same through a file the manifest names.
        e.write(
            "p/.claude-plugin/plugin.json",
            &json!({"mcpServers": "./more.json"}),
        );
        e.write(
            "p/more.json",
            &json!({"dup": {"command": "echo", "args": ["file"]}}),
        );
        let (servers, _) = e.load();
        assert_eq!(names(&servers), ["plugin:p:dup", "plugin:p:only"]);
        assert_eq!(
            servers[0].entry,
            json!({"command": "echo", "args": ["file"]})
        );
    }

    #[test]
    fn the_plugin_root_variable_is_expanded_as_claude_code_runs_the_server() {
        let e = Env::new();
        e.install("p@m", "p");
        e.write(
            "p/.mcp.json",
            &json!({
                "rooted": {"command": "node", "args": ["${CLAUDE_PLUGIN_ROOT}/s.js", "--x"],
                           "env": {"DATA": "${CLAUDE_PLUGIN_ROOT}/data"}},
                "remote": {"type": "http", "url": "https://x.example.com/mcp"}
            }),
        );
        e.enable(&[("p@m", true)]);
        let (servers, _) = e.load();
        let root = e.path("p");
        let root = root.to_str().unwrap();
        assert_eq!(
            servers[0].expanded(),
            json!({"command": "node", "args": [format!("{root}/s.js"), "--x"], "env": {"DATA": format!("{root}/data")}})
        );
        // Nothing to expand: the definition as written.
        assert_eq!(servers[1].expanded(), servers[1].entry);
        // Another variable is Claude Code's own environment, which Klefki does not know: left as written.
        let other = PluginServer {
            entry: json!({"command": "${HOME}/x", "args": ["${CLAUDE_PLUGIN_ROOTS}"]}),
            ..servers[0].clone()
        };
        assert_eq!(other.expanded(), other.entry);
    }

    #[test]
    fn a_plugin_in_a_local_marketplace_is_read_where_it_lives_not_from_its_copy() {
        let e = Env::new();
        e.install("p@local", "cache/local/p/1.0.0");
        e.write(
            "cache/local/p/1.0.0/.mcp.json",
            &json!({"stale": {"command": "old"}}),
        );
        e.write(
            "plugins/known_marketplaces.json",
            &json!({"local": {"source": {"source": "directory", "path": e.path("src").to_str().unwrap()},
                              "installLocation": e.path("src").to_str().unwrap()}}),
        );
        e.write(
            "src/.claude-plugin/marketplace.json",
            &json!({"name": "local", "plugins": [{"name": "p", "source": "./p"}]}),
        );
        e.write("src/p/.mcp.json", &json!({"live": {"command": "new"}}));
        e.enable(&[("p@local", true)]);
        let servers = e.load().0;
        assert_eq!(names(&servers), ["plugin:p:live"]);
        // The folder as Claude Code spells it in `${CLAUDE_PLUGIN_ROOT}`: no `./` left in it.
        assert_eq!(
            servers[0].root.to_string_lossy(),
            e.path("src").join("p").to_string_lossy()
        );

        // A source leaving the marketplace's folder is not followed; nor is a remote source, which uses the copy.
        e.write(
            "src/.claude-plugin/marketplace.json",
            &json!({"plugins": [{"name": "p", "source": {"source": "url", "url": "https://x.example.com/p.git"}}]}),
        );
        assert_eq!(names(&e.load().0), ["plugin:p:stale"]);
    }

    #[test]
    fn a_plugin_that_cannot_be_read_is_named_in_a_warning_and_the_others_still_load() {
        let e = Env::new();
        e.install("bad@m", "bad");
        e.install("good@m", "good");
        e.write("good/.mcp.json", &json!({"ok": {"command": "a"}}));
        std::fs::create_dir_all(e.path("bad")).unwrap();
        std::fs::write(e.path("bad/.mcp.json"), "{nope").unwrap();
        e.enable(&[("bad@m", true), ("missing@m", true), ("good@m", true)]);
        let (servers, warnings) = e.load();
        assert_eq!(names(&servers), ["plugin:good:ok"]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("bad@m") && warnings[0].contains("invalid JSON"),
            "{warnings:?}"
        );
        assert!(warnings[1].contains("missing@m"), "{warnings:?}");
    }

    #[test]
    fn untrusted_bookkeeping_is_limited() {
        let e = Env::new();
        // An install path with `..` is not followed.
        e.write(
            "plugins/installed_plugins.json",
            &json!({"plugins": {"p@m": [{"scope": "user", "installPath": e.path("a/../b").to_str().unwrap()}]}}),
        );
        e.write("b/.mcp.json", &json!({"x": {"command": "x"}}));
        e.enable(&[("p@m", true)]);
        let (servers, warnings) = e.load();
        assert!(servers.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        // More servers than a `.mcp.json` may hold.
        e.install("p@m", "big");
        let many: Map<String, Value> = (0..=MAX_SERVERS)
            .map(|i| (format!("s{i}"), json!({"command": "x"})))
            .collect();
        e.write("big/.mcp.json", &Value::Object(many));
        let (servers, warnings) = e.load();
        assert!(servers.is_empty());
        assert!(warnings[0].contains("more than 500"), "{warnings:?}");

        // No settings file: nothing, and nothing to say.
        let empty = Env::new();
        assert_eq!(empty.load(), (Vec::new(), Vec::new()));
    }

    #[test]
    fn the_same_as_key_follows_what_claude_code_compares() {
        let k = |v: Value| same_as_key(&v);
        // Command and arguments; not the environment or the type.
        assert_eq!(
            k(json!({"command": "ls", "args": ["1"], "env": {"A": "1"}})),
            k(json!({"type": "stdio", "command": "ls", "args": ["1"]}))
        );
        assert_ne!(
            k(json!({"command": "ls", "args": ["1"]})),
            k(json!({"command": "ls", "args": ["2"]}))
        );
        // Scheme, host and default port normalised, a trailing slash dropped; path case and query kept.
        assert_eq!(
            k(json!({"type": "http", "url": "HTTPS://Example.INVALID:443/mcp/"})),
            k(json!({"type": "sse", "url": "https://example.invalid/mcp"}))
        );
        assert_ne!(
            k(json!({"url": "https://e.example/MCP"})),
            k(json!({"url": "https://e.example/mcp"}))
        );
        assert_ne!(
            k(json!({"url": "https://e.example/mcp?x=1"})),
            k(json!({"url": "https://e.example/mcp"}))
        );
        assert_eq!(k(json!({"type": "http"})), None);
        // Between plugins the environment counts, except what Claude Code sets for a plugin.
        let p = |v: Value| same_as_plugin_key(&v);
        assert_ne!(
            p(json!({"command": "ls", "env": {"A": "1"}})),
            p(json!({"command": "ls"}))
        );
        assert_eq!(
            p(json!({"command": "ls", "env": {"CLAUDE_PLUGIN_ROOT": "/x", "B": "2", "A": "1"}})),
            p(json!({"command": "ls", "env": {"A": "1", "B": "2"}}))
        );
    }
}
