//! Builds the server list from `.claude.json`, project `.mcp.json` files and Claude's health caches.
//! Everything read here is untrusted: sizes are limited, text is sanitised and secrets are never shown.
use super::{ConfigPaths, fsx};
use crate::api::{Scope, ServerRow, Snapshot, Status, TransportKind};
use crate::sanitize::{self, sanitize};
use serde::Deserialize;
use serde::de::{Error as _, MapAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

const MAX_CLAUDE_JSON: u64 = 64 * 1024 * 1024;
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

    if let Some(servers) = root.get("mcpServers").and_then(Value::as_object) {
        for (name, entry) in servers {
            let status = hints.status_of(name);
            snap.servers
                .push(row(name, entry, Scope::User, None, true, status));
        }
    }

    for (key, project) in merged_projects(root) {
        let disabled_local = string_set(project.values.iter().map(|v| v.get("disabledMcpServers")));
        let disabled_project = string_set(
            project
                .values
                .iter()
                .map(|v| v.get("disabledMcpjsonServers")),
        );
        let mut seen_local = BTreeSet::new();
        for value in &project.values {
            let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
                continue;
            };
            for (name, entry) in servers {
                if seen_local.insert(name.clone()) {
                    let enabled = !disabled_local.contains(name);
                    snap.servers.push(row(
                        name,
                        entry,
                        Scope::Local,
                        Some(&project.display),
                        enabled,
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
                for (name, entry) in servers {
                    let enabled = !disabled_project.contains(&name);
                    let mut r = row(
                        &name,
                        &entry,
                        Scope::Project,
                        Some(&project.display),
                        enabled,
                        Status::Unknown,
                    );
                    r.shadowed = snap.servers.iter().any(|o| {
                        o.scope == Scope::Local
                            && o.name == r.name
                            && o.project.as_deref().map(normalise_project).as_deref() == Some(&key)
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
    fn project_servers_come_from_mcp_json_and_can_be_shadowed_or_disabled() {
        let e = Env::new();
        let p = e.project("app");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"shared":{"type":"http","url":"https://s.example.com"},
                "docs":{"command":"node","args":["docs.js"]},
                "off":{"type":"sse","url":"https://o.example.com"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "mcpServers": {"shared": {"type":"http","url":"https://mine.example.com"}},
                "disabledMcpjsonServers": ["off"]
            }}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::Project, "shared").shadowed);
        assert!(!find(&s, Scope::Local, "shared").shadowed);
        assert!(!find(&s, Scope::Project, "docs").shadowed);
        assert!(!find(&s, Scope::Project, "off").enabled);
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
}
