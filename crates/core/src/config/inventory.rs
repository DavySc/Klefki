//! Builds the server list from `.claude.json`, project `.mcp.json` files and Claude's health caches.
//! Everything read here is untrusted: sizes are limited, text is sanitised and secrets are never shown.
use super::{ConfigPaths, fsx};
use crate::api::{Approval, Scope, ServerRow, Snapshot, Status, TransportKind};
use crate::sanitize::{self, sanitize};
use serde::Deserialize;
use serde::de::{Error as _, MapAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

pub(crate) const MAX_CLAUDE_JSON: u64 = 64 * 1024 * 1024;
const MAX_CACHE_JSON: u64 = 4 * 1024 * 1024;
const MAX_MCP_JSON: u64 = 1024 * 1024;
const MAX_MCP_SERVERS: usize = 500;
const MAX_TEXT_CHARS: usize = 512;

pub(crate) fn load(paths: &ConfigPaths) -> Snapshot {
    let mut snap = Snapshot::default();
    let root = match read_json(&paths.claude_json, MAX_CLAUDE_JSON) {
        Ok(Some(v)) => v,
        Ok(None) => {
            snap.warnings.push(format!(
                "{} does not exist; no servers are configured yet.",
                display_path(&paths.claude_json)
            ));
            return snap;
        }
        Err(e) => {
            snap.warnings.push(format!(
                "Could not read {}: {e}",
                display_path(&paths.claude_json)
            ));
            return snap;
        }
    };
    let Some(root) = root.as_object() else {
        snap.warnings.push(format!(
            "{} is not a JSON object.",
            display_path(&paths.claude_json)
        ));
        return snap;
    };

    let hints = Hints::load(paths, &mut snap.warnings);

    let projects = merged_projects(root);
    snap.project_count = projects.len();
    snap.projects = projects.keys().cloned().collect();
    // The on/off toggle is a list of names in each project's entry (decision 002).
    let disabled_by_project: BTreeMap<&String, BTreeSet<String>> = projects
        .iter()
        .map(|(key, project)| {
            let names = string_set(project.values.iter().map(|v| v.get("disabledMcpServers")));
            (key, names)
        })
        .collect();

    if let Some(servers) = root.get("mcpServers").and_then(Value::as_object) {
        for (name, entry) in servers {
            let status = hints.status_of(name);
            let mut r = row(name, entry, Scope::User, None, true, status);
            r.disabled_in = disabled_by_project
                .values()
                .filter(|d| d.contains(name))
                .count();
            // A user server is one row, but the toggle is per project: it only counts as off when it is off
            // everywhere Claude knows about.
            r.enabled = !(snap.project_count > 0 && r.disabled_in == snap.project_count);
            snap.servers.push(r);
        }
    }

    for (key, project) in &projects {
        let disabled = &disabled_by_project[key];
        let mut seen_local = BTreeSet::new();
        for value in &project.values {
            let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
                continue;
            };
            for (name, entry) in servers {
                if seen_local.insert(name.clone()) {
                    snap.servers.push(row(
                        name,
                        entry,
                        Scope::Local,
                        Some(&project.display),
                        !disabled.contains(name),
                        Status::Unknown,
                    ));
                }
            }
        }
        if !is_safe_project_path(&project.raw) {
            snap.warnings.push(format!(
                "Skipped the .mcp.json of {}: not a local absolute path.",
                sanitize(&project.display)
            ));
            continue;
        }
        let mcp_json = Path::new(&project.raw).join(".mcp.json");
        match read_project_servers(&mcp_json) {
            Ok(None) => {}
            Ok(Some((servers, duplicates))) => {
                for name in duplicates {
                    snap.warnings.push(format!(
                        "{} defines {:?} more than once; the last definition wins.",
                        display_path(&mcp_json),
                        sanitize(&name)
                    ));
                }
                let approvals = ApprovalLists::load(project, &mut snap.warnings);
                for (name, entry) in servers {
                    let approval = approvals.of(&name);
                    let hint = if approval == Approval::Pending {
                        Status::PendingApproval
                    } else {
                        Status::Unknown
                    };
                    let mut r = row(
                        &name,
                        &entry,
                        Scope::Project,
                        Some(&project.display),
                        !disabled.contains(&name),
                        hint,
                    );
                    r.approval = Some(approval);
                    r.shadowed = snap.servers.iter().any(|o| {
                        o.scope == Scope::Local
                            && o.name == r.name
                            && o.project.as_deref().map(normalise_project).as_deref() == Some(key)
                    });
                    snap.servers.push(r);
                }
            }
            Err(e) => snap
                .warnings
                .push(format!("Ignored {}: {e}", display_path(&mcp_json))),
        }
    }

    mark_casing_collisions(&mut snap.servers);
    snap
}

// ---------------------------------------------------------------- rows

fn row(
    name: &str,
    entry: &Value,
    scope: Scope,
    project: Option<&str>,
    enabled: bool,
    status: Status,
) -> ServerRow {
    let (transport, target) = classify(entry);
    ServerRow {
        name: clip(&sanitize(name)),
        scope,
        project: project.map(|p| clip(&sanitize(p))),
        transport,
        target,
        status,
        enabled,
        disabled_in: 0,
        approval: None,
        shadowed: false,
        casing_collision: false,
    }
}

fn classify(entry: &Value) -> (TransportKind, String) {
    let Some(obj) = entry.as_object() else {
        return (TransportKind::Invalid, "entry is not an object".into());
    };
    match obj.get("type").and_then(Value::as_str) {
        None | Some("stdio") => match obj.get("command").and_then(Value::as_str) {
            Some(cmd) if !cmd.trim().is_empty() => {
                let mut line = cmd.to_owned();
                for arg in obj
                    .get("args")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    line.push(' ');
                    line.push_str(arg);
                }
                (TransportKind::Stdio, clip(&sanitize(&line)))
            }
            _ => (TransportKind::Invalid, "missing command".into()),
        },
        Some(kind @ ("http" | "sse" | "ws")) => {
            let (transport, schemes): (_, &[&str]) = match kind {
                "http" => (TransportKind::Http, &["http", "https"]),
                "sse" => (TransportKind::Sse, &["http", "https"]),
                _ => (TransportKind::Ws, &["ws", "wss"]),
            };
            match obj.get("url").and_then(Value::as_str) {
                Some(url) => match display_url(url, schemes) {
                    Ok(shown) => (transport, shown),
                    Err(why) => (TransportKind::Invalid, why.into()),
                },
                None => (TransportKind::Invalid, "missing url".into()),
            }
        }
        Some(other) => (
            TransportKind::Invalid,
            clip(&format!("unknown type {:?}", sanitize(other))),
        ),
    }
}

/// A URL for display: no credentials and no query string, which may carry tokens.
fn display_url(raw: &str, schemes: &[&str]) -> Result<String, &'static str> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "invalid URL")?;
    if !schemes.contains(&url.scheme()) {
        return Err("URL scheme is not allowed for this type");
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or("URL has no host")?;
    let mut shown = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        shown.push_str(&format!(":{port}"));
    }
    shown.push_str(url.path());
    if url.query().is_some() {
        shown.push_str("?…");
    }
    Ok(clip(&sanitize(&shown)))
}

fn clip(s: &str) -> String {
    sanitize::clip(s, MAX_TEXT_CHARS)
}

fn mark_casing_collisions(rows: &mut [ServerRow]) {
    // Claude resolves one effective set per project: user + that project's local + project servers.
    let user: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].scope == Scope::User)
        .collect();
    let mut per_project: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        if let (false, Some(p)) = (r.scope == Scope::User, &r.project) {
            per_project.entry(normalise_project(p)).or_default().push(i);
        }
    }
    let mut sets = vec![user.clone()];
    for mut set in per_project.into_values() {
        set.extend(&user);
        sets.push(set);
    }
    let mut flagged = BTreeSet::new();
    for set in &sets {
        let mut by_lower: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in set {
            by_lower
                .entry(rows[i].name.to_lowercase())
                .or_default()
                .push(i);
        }
        for group in by_lower.values() {
            let spellings: BTreeSet<&str> = group.iter().map(|&i| rows[i].name.as_str()).collect();
            if spellings.len() > 1 {
                flagged.extend(group.iter().copied());
            }
        }
    }
    for i in flagged {
        rows[i].casing_collision = true;
    }
}

// ---------------------------------------------------------------- projects

struct Project<'a> {
    /// Path spelling used for reading `.mcp.json`.
    raw: String,
    display: String,
    values: Vec<&'a Value>,
}

/// Claude may record one directory under several spellings (`c:\x`, `C:/x/`); merge them.
fn merged_projects(root: &serde_json::Map<String, Value>) -> BTreeMap<String, Project<'_>> {
    let mut out: BTreeMap<String, Project<'_>> = BTreeMap::new();
    let Some(projects) = root.get("projects").and_then(Value::as_object) else {
        return out;
    };
    for (path, value) in projects {
        if !value.is_object() {
            continue;
        }
        out.entry(normalise_project(path))
            .or_insert_with(|| Project {
                raw: path.clone(),
                display: normalise_project(path),
                values: Vec::new(),
            })
            .values
            .push(value);
    }
    out
}

pub(crate) fn normalise_project(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    while p.len() > 1 && p.ends_with('/') && !p.ends_with(":/") {
        p.pop();
    }
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        p[..1].make_ascii_uppercase();
    }
    p
}

/// Only local absolute paths are opened. UNC paths would make Windows contact (and authenticate to) a remote host.
pub(crate) fn is_safe_project_path(path: &str) -> bool {
    !(path.starts_with("\\\\") || path.starts_with("//") || path.contains('\0'))
        && Path::new(path).is_absolute()
}

/// Names of servers in a project's `.mcp.json`, plus any names defined twice.
type ProjectServers = (Vec<(String, Value)>, Vec<String>);

fn read_project_servers(path: &Path) -> Result<Option<ProjectServers>, String> {
    let Some(text) = fsx::read_limited(path, MAX_MCP_JSON).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    #[derive(Deserialize)]
    struct McpFile {
        #[serde(rename = "mcpServers", default)]
        servers: Option<Pairs>,
    }
    let file: McpFile = serde_json::from_str(&text).map_err(|e| format!("invalid JSON ({e})"))?;
    Ok(Some(
        file.servers
            .map(|p| (p.entries, p.duplicates))
            .unwrap_or_default(),
    ))
}

/// A JSON object read as ordered pairs, so duplicate keys are noticed and the count is capped.
struct Pairs {
    entries: Vec<(String, Value)>,
    duplicates: Vec<String>,
}

impl<'de> Deserialize<'de> for Pairs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Pairs;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of servers")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs, A::Error> {
                let mut entries: Vec<(String, Value)> = Vec::new();
                let mut duplicates = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    if let Some(slot) = entries.iter_mut().find(|(e, _)| *e == k) {
                        slot.1 = v;
                        duplicates.push(k);
                    } else if entries.len() >= MAX_MCP_SERVERS {
                        return Err(A::Error::custom(format!(
                            "more than {MAX_MCP_SERVERS} servers"
                        )));
                    } else {
                        entries.push((k, v));
                    }
                }
                Ok(Pairs {
                    entries,
                    duplicates,
                })
            }
        }
        d.deserialize_map(V)
    }
}

// ---------------------------------------------------------------- approval

/// Who has said yes or no to a project server. Claude keeps this in the project entry of `.claude.json` and,
/// in current versions, in the project's own `.claude/settings.local.json`; both are read (decision 002).
struct ApprovalLists {
    enabled: BTreeSet<String>,
    rejected: BTreeSet<String>,
    all: bool,
}

impl ApprovalLists {
    fn load(project: &Project<'_>, warnings: &mut Vec<String>) -> Self {
        let mut lists = Self {
            enabled: BTreeSet::new(),
            rejected: BTreeSet::new(),
            all: false,
        };
        for value in &project.values {
            lists.absorb(value);
        }
        let path = Path::new(&project.raw)
            .join(".claude")
            .join("settings.local.json");
        match read_json(&path, MAX_MCP_JSON) {
            Ok(Some(value)) => lists.absorb(&value),
            Ok(None) => {}
            Err(e) => warnings.push(format!("Ignored {}: {e}", display_path(&path))),
        }
        lists
    }

    fn absorb(&mut self, source: &Value) {
        self.enabled.extend(string_set(std::iter::once(
            source.get("enabledMcpjsonServers"),
        )));
        self.rejected.extend(string_set(std::iter::once(
            source.get("disabledMcpjsonServers"),
        )));
        self.all |= source.get("enableAllProjectMcpServers") == Some(&Value::Bool(true));
    }

    fn of(&self, name: &str) -> Approval {
        if self.rejected.contains(name) {
            Approval::Rejected
        } else if self.all || self.enabled.contains(name) {
            Approval::Approved
        } else {
            Approval::Pending
        }
    }
}

// ---------------------------------------------------------------- hints

/// Instant status hints from Claude's own caches. Only a hint; live checks come later.
struct Hints {
    health: BTreeMap<String, String>,
    needs_auth: BTreeSet<String>,
}

impl Hints {
    fn load(paths: &ConfigPaths, warnings: &mut Vec<String>) -> Self {
        let mut hints = Self {
            health: BTreeMap::new(),
            needs_auth: BTreeSet::new(),
        };
        let health = paths.claude_dir.join("mcp-health-cache.json");
        match read_json(&health, MAX_CACHE_JSON) {
            Ok(Some(v)) => {
                if let Some(servers) = v.get("servers").and_then(Value::as_object) {
                    for (name, e) in servers {
                        if let Some(s) = e.get("status").and_then(Value::as_str) {
                            hints.health.insert(name.clone(), s.to_owned());
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => warnings.push(format!("Ignored {}: {e}", display_path(&health))),
        }
        let needs = paths.claude_dir.join("mcp-needs-auth-cache.json");
        match read_json(&needs, MAX_CACHE_JSON) {
            Ok(Some(Value::Object(o))) => hints.needs_auth.extend(o.keys().cloned()),
            Ok(_) => {}
            Err(e) => warnings.push(format!("Ignored {}: {e}", display_path(&needs))),
        }
        hints
    }

    fn status_of(&self, name: &str) -> Status {
        if self.needs_auth.contains(name) {
            return Status::NeedsAuth;
        }
        match self.health.get(name).map(String::as_str) {
            Some("healthy") => Status::Connected,
            Some(_) => Status::Failed,
            None => Status::Unknown,
        }
    }
}

// ---------------------------------------------------------------- helpers

fn read_json(path: &Path, max: u64) -> Result<Option<Value>, String> {
    let Some(text) = fsx::read_limited(path, max).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("invalid JSON ({e})"))
}

fn string_set<'a>(values: impl Iterator<Item = Option<&'a Value>>) -> BTreeSet<String> {
    values
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn display_path(path: &Path) -> String {
    sanitize(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

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
            ConfigPaths::in_dir(self.dir.path())
        }
        fn write(&self, rel: &str, v: &Value) {
            self.write_raw(rel, &serde_json::to_string(v).unwrap());
        }
        fn write_raw(&self, rel: &str, text: &str) {
            let p = self.dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        fn project(&self, name: &str) -> PathBuf {
            let p = self.dir.path().join("projects").join(name);
            std::fs::create_dir_all(&p).unwrap();
            p
        }
        fn load(&self) -> Snapshot {
            load(&self.paths())
        }
    }

    fn find<'a>(s: &'a Snapshot, scope: Scope, name: &str) -> &'a ServerRow {
        s.servers
            .iter()
            .find(|r| r.scope == scope && r.name == name)
            .unwrap_or_else(|| panic!("{scope:?}/{name} in {:#?}", s.servers))
    }

    #[test]
    fn lists_user_servers_of_every_transport() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "REMOTE_1": {"type": "http", "url": "https://acme.example.com/gateway/mcp",
                             "oauth": {"clientId": "abc", "callbackPort": 1234}},
                "tool": {"type": "stdio", "command": "npx", "args": ["-y", "pkg"], "env": {"K": "v"}},
                "legacy": {"type": "sse", "url": "http://localhost:8080/sse"},
                "sock": {"type": "ws", "url": "wss://x.example.com/ws"},
                "implicit": {"command": "node"}
            }}),
        );
        let s = e.load();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let sf = find(&s, Scope::User, "REMOTE_1");
        assert_eq!(sf.transport, TransportKind::Http);
        assert_eq!(sf.target, "https://acme.example.com/gateway/mcp");
        assert_eq!(find(&s, Scope::User, "tool").target, "npx -y pkg");
        assert_eq!(
            find(&s, Scope::User, "legacy").transport,
            TransportKind::Sse
        );
        assert_eq!(find(&s, Scope::User, "sock").transport, TransportKind::Ws);
        assert_eq!(
            find(&s, Scope::User, "implicit").transport,
            TransportKind::Stdio
        );
        assert!(s.servers.iter().all(|r| r.enabled));
    }

    #[test]
    fn secrets_never_reach_the_rows() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "a": {"type": "http", "url": "https://user:pw-S3CRET@h.example.com/p?token=S3CRET#S3CRET",
                      "headers": {"Authorization": "Bearer S3CRET"},
                      "oauth": {"clientId": "S3CRET"}},
                "b": {"type": "stdio", "command": "x", "env": {"API_KEY": "S3CRET"}}
            }}),
        );
        let s = e.load();
        let dump = format!("{s:?}");
        assert!(!dump.contains("S3CRET"), "{dump}");
        assert_eq!(
            find(&s, Scope::User, "a").target,
            "https://h.example.com/p?…"
        );
    }

    #[test]
    fn status_comes_from_claudes_caches() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "ok": {"type":"http","url":"https://a.example.com"},
                "bad": {"type":"http","url":"https://b.example.com"},
                "auth": {"type":"http","url":"https://c.example.com"},
                "new": {"type":"http","url":"https://d.example.com"}
            }}),
        );
        e.write(
            "mcp-health-cache.json",
            &json!({"version":1,"servers":{
                "ok":{"status":"healthy"},"bad":{"status":"unhealthy"},"auth":{"status":"healthy"}}}),
        );
        e.write(
            "mcp-needs-auth-cache.json",
            &json!({"auth":{"timestamp":1,"id":"x"}}),
        );
        let s = e.load();
        assert_eq!(find(&s, Scope::User, "ok").status, Status::Connected);
        assert_eq!(find(&s, Scope::User, "bad").status, Status::Failed);
        assert_eq!(find(&s, Scope::User, "auth").status, Status::NeedsAuth);
        assert_eq!(find(&s, Scope::User, "new").status, Status::Unknown);
    }

    #[test]
    fn local_servers_and_disabled_state() {
        let e = Env::new();
        let p = e.project("app");
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "mcpServers": {"on": {"type":"http","url":"https://a.example.com"},
                               "off": {"type":"http","url":"https://b.example.com"}},
                "disabledMcpServers": ["off"]
            }}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::Local, "on").enabled);
        assert!(!find(&s, Scope::Local, "off").enabled);
        assert_eq!(
            find(&s, Scope::Local, "on").project.as_deref(),
            Some(normalise_project(&p.to_string_lossy()).as_str())
        );
    }

    #[test]
    fn project_servers_come_from_mcp_json_and_can_be_shadowed() {
        let e = Env::new();
        let p = e.project("app");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"shared":{"type":"http","url":"https://s.example.com"},
                "docs":{"command":"node","args":["docs.js"]}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "mcpServers": {"shared": {"type":"http","url":"https://mine.example.com"}}
            }}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::Project, "shared").shadowed);
        assert!(!find(&s, Scope::Local, "shared").shadowed);
        assert!(!find(&s, Scope::Project, "docs").shadowed);
    }

    #[test]
    fn duplicate_keys_in_mcp_json_are_reported_and_last_wins() {
        let e = Env::new();
        let p = e.project("dup");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"a":{"command":"first"},"a":{"command":"second"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {}}}),
        );
        let s = e.load();
        assert_eq!(find(&s, Scope::Project, "a").target, "second");
        assert!(
            s.warnings.iter().any(|w| w.contains("more than once")),
            "{:?}",
            s.warnings
        );
        assert_eq!(s.servers.iter().filter(|r| r.name == "a").count(), 1);
    }

    #[test]
    fn hostile_mcp_json_is_contained() {
        let e = Env::new();
        let big = e.project("big");
        std::fs::write(big.join(".mcp.json"), " ".repeat(MAX_MCP_JSON as usize + 1)).unwrap();
        let many = e.project("many");
        let servers: serde_json::Map<String, Value> = (0..=MAX_MCP_SERVERS)
            .map(|i| (format!("s{i}"), json!({"command": "x"})))
            .collect();
        std::fs::write(
            many.join(".mcp.json"),
            json!({"mcpServers": servers}).to_string(),
        )
        .unwrap();
        let deep = e.project("deep");
        std::fs::write(
            deep.join(".mcp.json"),
            format!("{}{}", "[".repeat(10_000), "]".repeat(10_000)),
        )
        .unwrap();
        let junk = e.project("junk");
        std::fs::write(junk.join(".mcp.json"), "not json at all").unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {
                big.to_string_lossy(): {}, many.to_string_lossy(): {},
                deep.to_string_lossy(): {}, junk.to_string_lossy(): {}}}),
        );
        let s = e.load();
        assert!(s.servers.is_empty());
        assert_eq!(s.warnings.len(), 4, "{:?}", s.warnings);
    }

    #[test]
    fn invalid_entries_are_flagged_not_fatal() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "js": {"type":"http","url":"javascript:alert(1)"},
                "file": {"type":"sse","url":"file:///etc/passwd"},
                "httpOnWs": {"type":"ws","url":"https://x.example.com"},
                "nourl": {"type":"http"},
                "nocmd": {"type":"stdio"},
                "alien": {"type":"carrier-pigeon"},
                "scalar": 42
            }}),
        );
        let s = e.load();
        assert_eq!(s.servers.len(), 7);
        assert!(
            s.servers
                .iter()
                .all(|r| r.transport == TransportKind::Invalid),
            "{:?}",
            s.servers
        );
    }

    #[test]
    fn names_that_differ_only_by_case_are_flagged() {
        let e = Env::new();
        let p = e.project("app");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"Gateway": {"command": "a"}, "plain": {"command": "b"}},
                "projects": {p.to_string_lossy(): {"mcpServers": {"gateway": {"command": "c"}}}}
            }),
        );
        let s = e.load();
        assert!(find(&s, Scope::User, "Gateway").casing_collision);
        assert!(find(&s, Scope::Local, "gateway").casing_collision);
        assert!(!find(&s, Scope::User, "plain").casing_collision);
    }

    #[test]
    fn same_name_in_two_scopes_is_not_a_casing_collision() {
        let e = Env::new();
        let p = e.project("app");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"same": {"command": "a"}},
                "projects": {p.to_string_lossy(): {"mcpServers": {"same": {"command": "c"}}}}
            }),
        );
        assert!(e.load().servers.iter().all(|r| !r.casing_collision));
    }

    #[test]
    fn project_spellings_are_merged() {
        assert_eq!(normalise_project("c:\\Users\\x\\"), "C:/Users/x");
        assert_eq!(normalise_project("C:/Users/x"), "C:/Users/x");
        assert_eq!(normalise_project("/home/x/"), "/home/x");
        assert_eq!(normalise_project("/"), "/");
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {
                "c:\\Users\\x": {"mcpServers": {"a": {"command": "one"}}},
                "C:/Users/x/": {"mcpServers": {"a": {"command": "two"}, "b": {"command": "b"}}}}}),
        );
        let s = e.load();
        assert_eq!(s.servers.iter().filter(|r| r.name == "a").count(), 1);
        assert_eq!(s.servers.len(), 2);
    }

    #[test]
    fn only_local_absolute_project_paths_are_opened() {
        for bad in [
            "\\\\evil\\share\\x",
            "//evil/share/x",
            "relative/dir",
            "..\\x",
            "a\0b",
            "",
        ] {
            assert!(!is_safe_project_path(bad), "{bad:?}");
        }
        let e = Env::new();
        assert!(is_safe_project_path(&e.project("ok").to_string_lossy()));
    }

    #[test]
    fn unsafe_project_paths_still_list_local_servers_but_warn() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {"\\\\evil\\share\\proj": {"mcpServers": {"x": {"command": "c"}}}}}),
        );
        let s = e.load();
        assert_eq!(s.servers.len(), 1);
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("not a local absolute path"))
        );
    }

    #[test]
    fn missing_or_corrupt_config_gives_a_warning_not_a_panic() {
        let e = Env::new();
        let s = e.load();
        assert!(s.servers.is_empty() && s.warnings.len() == 1);
        e.write_raw(".claude.json", "{ broken");
        let s = e.load();
        assert!(s.servers.is_empty() && s.warnings.len() == 1);
        e.write_raw(".claude.json", "[1,2,3]");
        assert_eq!(e.load().warnings.len(), 1);
    }

    #[test]
    fn corrupt_cache_files_only_lose_the_hints() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"a": {"command": "x"}}}),
        );
        e.write_raw("mcp-health-cache.json", "garbage");
        let s = e.load();
        assert_eq!(s.servers.len(), 1);
        assert_eq!(s.servers[0].status, Status::Unknown);
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn terminal_escapes_and_bidi_in_config_text_are_sanitised() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "evil\u{1b}[31mname\u{202e}": {"command": "run\u{1b}[0m\u{7}me"}}}),
        );
        let s = e.load();
        assert_eq!(s.servers[0].name, "evilname");
        assert_eq!(s.servers[0].target, "runme");
    }

    #[test]
    fn long_text_is_clipped() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"a": {"command": "x".repeat(10_000)}}}),
        );
        let s = e.load();
        assert!(s.servers[0].target.chars().count() <= MAX_TEXT_CHARS + 1);
    }

    // ---- enabled / disabled and approval, per decision 002 ----

    fn write_local_settings(project: &Path, v: &Value) {
        let dir = project.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.local.json"), v.to_string()).unwrap();
    }

    fn project_with_mcp_json(e: &Env, name: &str, servers: &[&str]) -> PathBuf {
        let p = e.project(name);
        let entries: serde_json::Map<String, Value> = servers
            .iter()
            .map(|s| {
                (
                    (*s).to_owned(),
                    json!({"type": "http", "url": "https://p.example.com"}),
                )
            })
            .collect();
        std::fs::write(
            p.join(".mcp.json"),
            json!({"mcpServers": entries}).to_string(),
        )
        .unwrap();
        p
    }

    #[test]
    fn the_toggle_is_disabledmcpservers_for_every_scope() {
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["proj-on", "proj-off"]);
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"user-srv": {"command": "u"}},
                "projects": {p.to_string_lossy(): {
                    "mcpServers": {"local-on": {"command": "a"}, "local-off": {"command": "b"}},
                    "disabledMcpServers": ["local-off", "proj-off"],
                    "enabledMcpjsonServers": ["proj-on", "proj-off"]
                }}
            }),
        );
        let s = e.load();
        assert!(find(&s, Scope::Local, "local-on").enabled);
        assert!(!find(&s, Scope::Local, "local-off").enabled);
        assert!(find(&s, Scope::Project, "proj-on").enabled);
        assert!(!find(&s, Scope::Project, "proj-off").enabled);
        assert!(find(&s, Scope::User, "user-srv").enabled, "not in the list");
    }

    #[test]
    fn rejecting_a_project_server_is_not_the_toggle() {
        // `disabledMcpjsonServers` is the approval dialog's "no", not the /mcp on/off switch.
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["srv"]);
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {"disabledMcpjsonServers": ["srv"]}}}),
        );
        let s = e.load();
        let r = find(&s, Scope::Project, "srv");
        assert!(r.enabled, "the toggle is untouched");
        assert_eq!(r.approval, Some(Approval::Rejected));
    }

    #[test]
    fn a_user_server_is_disabled_per_project() {
        let e = Env::new();
        let a = e.project("a");
        let b = e.project("b");
        let c = e.project("c");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}, "v": {"command": "y"}},
                "projects": {
                    a.to_string_lossy(): {"disabledMcpServers": ["u", "v"]},
                    b.to_string_lossy(): {"disabledMcpServers": ["u"]},
                    c.to_string_lossy(): {}
                }
            }),
        );
        let s = e.load();
        assert_eq!(s.project_count, 3);
        let u = find(&s, Scope::User, "u");
        assert_eq!(u.disabled_in, 2);
        assert!(u.enabled, "still on in one project");
        assert_eq!(find(&s, Scope::User, "v").disabled_in, 1);
    }

    #[test]
    fn a_user_server_off_in_every_known_project_counts_as_disabled() {
        let e = Env::new();
        let a = e.project("a");
        let b = e.project("b");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {
                    a.to_string_lossy(): {"disabledMcpServers": ["u"]},
                    b.to_string_lossy(): {"disabledMcpServers": ["u"]}
                }
            }),
        );
        let s = e.load();
        let u = find(&s, Scope::User, "u");
        assert_eq!((u.disabled_in, s.project_count), (2, 2));
        assert!(!u.enabled);
    }

    #[test]
    fn with_no_known_projects_a_user_server_is_simply_enabled() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"u": {"command": "x"}}}),
        );
        let s = e.load();
        assert_eq!(s.project_count, 0);
        let u = find(&s, Scope::User, "u");
        assert!(u.enabled);
        assert_eq!(u.disabled_in, 0);
    }

    #[test]
    fn one_project_under_two_spellings_counts_once() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {
                    "c:\\Users\\x": {"disabledMcpServers": ["u"]},
                    "C:/Users/x/": {"disabledMcpServers": ["u"]}
                }
            }),
        );
        let s = e.load();
        assert_eq!(s.project_count, 1);
        assert_eq!(find(&s, Scope::User, "u").disabled_in, 1);
    }

    #[test]
    fn junk_in_the_disabled_list_is_ignored() {
        let e = Env::new();
        let a = e.project("a");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {a.to_string_lossy(): {"disabledMcpServers": [1, null, {"x": 1}, "u"]}}
            }),
        );
        assert_eq!(find(&e.load(), Scope::User, "u").disabled_in, 1);
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {a.to_string_lossy(): {"disabledMcpServers": "u"}}
            }),
        );
        assert_eq!(
            find(&e.load(), Scope::User, "u").disabled_in,
            0,
            "not an array"
        );
    }

    #[test]
    fn approval_is_read_from_the_project_entry_and_from_settings_local_json() {
        let e = Env::new();
        let p = project_with_mcp_json(
            &e,
            "app",
            &["in-entry", "in-local", "neither", "no-way", "both"],
        );
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "enabledMcpjsonServers": ["in-entry", "both"]}}}),
        );
        write_local_settings(
            &p,
            &json!({"enabledMcpjsonServers": ["in-local"], "disabledMcpjsonServers": ["no-way", "both"]}),
        );
        let s = e.load();
        let approval = |n: &str| find(&s, Scope::Project, n).approval;
        assert_eq!(approval("in-entry"), Some(Approval::Approved));
        assert_eq!(approval("in-local"), Some(Approval::Approved));
        assert_eq!(approval("neither"), Some(Approval::Pending));
        assert_eq!(approval("no-way"), Some(Approval::Rejected));
        assert_eq!(
            approval("both"),
            Some(Approval::Rejected),
            "a rejection beats an approval"
        );
    }

    #[test]
    fn approving_everything_covers_servers_not_listed() {
        for place in ["entry", "local"] {
            let e = Env::new();
            let p = project_with_mcp_json(&e, "app", &["x", "y"]);
            if place == "entry" {
                e.write(
                    ".claude.json",
                    &json!({"projects": {p.to_string_lossy(): {"enableAllProjectMcpServers": true}}}),
                );
            } else {
                e.write(
                    ".claude.json",
                    &json!({"projects": {p.to_string_lossy(): {}}}),
                );
                write_local_settings(&p, &json!({"enableAllProjectMcpServers": true}));
            }
            let s = e.load();
            for n in ["x", "y"] {
                assert_eq!(
                    find(&s, Scope::Project, n).approval,
                    Some(Approval::Approved),
                    "{place} {n}"
                );
            }
        }
    }

    #[test]
    fn a_pending_project_server_says_so_and_other_scopes_have_no_approval() {
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["waiting"]);
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {p.to_string_lossy(): {"mcpServers": {"l": {"command": "y"}}}}
            }),
        );
        let s = e.load();
        let w = find(&s, Scope::Project, "waiting");
        assert_eq!(w.approval, Some(Approval::Pending));
        assert_eq!(w.status, Status::PendingApproval);
        assert_eq!(find(&s, Scope::User, "u").approval, None);
        assert_eq!(find(&s, Scope::Local, "l").approval, None);
    }

    #[test]
    fn a_broken_settings_local_json_is_ignored_with_a_warning() {
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["srv"]);
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {"enabledMcpjsonServers": ["srv"]}}}),
        );
        std::fs::create_dir_all(p.join(".claude")).unwrap();
        std::fs::write(p.join(".claude").join("settings.local.json"), "{ not json").unwrap();
        let s = e.load();
        assert_eq!(
            find(&s, Scope::Project, "srv").approval,
            Some(Approval::Approved)
        );
        assert!(
            s.warnings.iter().any(|w| w.contains("settings.local.json")),
            "{:?}",
            s.warnings
        );
        // An oversized file is refused the same way.
        std::fs::write(
            p.join(".claude").join("settings.local.json"),
            " ".repeat(MAX_MCP_JSON as usize + 1),
        )
        .unwrap();
        let s = e.load();
        assert_eq!(
            find(&s, Scope::Project, "srv").approval,
            Some(Approval::Approved)
        );
        assert!(s.warnings.iter().any(|w| w.contains("settings.local.json")));
    }

    #[test]
    fn approval_is_not_read_from_a_project_path_we_refuse_to_open() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {"\\\\evil\\share\\proj": {"enabledMcpjsonServers": ["x"]}}}),
        );
        // Nothing to list (the .mcp.json is never opened) and nothing to crash on.
        let s = e.load();
        assert!(s.servers.is_empty());
        assert_eq!(s.project_count, 1);
    }
}
