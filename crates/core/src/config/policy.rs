//! Which servers Claude Code's policy lets load: `deniedMcpServers` and `allowedMcpServers` (decision 010).
//!
//! The rules come from the user's `settings.json`, a project's `.claude/settings.json` and `settings.local.json`
//! (that project only), and the organisation's settings files. A server a denied rule matches never loads. Where any of
//! these files has `allowedMcpServers` (even an empty list), only servers an allowed rule matches load; the lists of
//! all files count together, unless the organisation sets `allowManagedMcpServersOnly`, when only its own count. The
//! organisation's own servers need no allowed rule, but a denied rule removes them too.
//!
//! A rule's fields must all match: `serverName` the name exactly, `serverCommand` a stdio server's command and
//! arguments exactly, `serverUrl` a remote server's address, where `*` stands for any text and the scheme and host are
//! compared in lower case, without a default port. Names merge first: policy judges the server that won its name.
//!
//! The files are untrusted: size-limited, and only lists of objects with these fields are read.
use super::{fsx, inventory, managed};
use serde_json::Value;
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = inventory::MAX_MCP_JSON;
/// Rules kept per list and file, at most.
const MAX_RULES: usize = 500;

/// One entry of `deniedMcpServers` or `allowedMcpServers`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rule {
    name: Option<String>,
    command: Option<Vec<String>>,
    url: Option<String>,
    file: PathBuf,
}

impl Rule {
    fn read(value: &Value, file: &Path) -> Option<Self> {
        let obj = value.as_object()?;
        let name = obj
            .get("serverName")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let command = obj
            .get("serverCommand")
            .and_then(Value::as_array)
            .and_then(|parts| {
                parts
                    .iter()
                    .map(|p| p.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            });
        let url = obj
            .get("serverUrl")
            .and_then(Value::as_str)
            .map(|u| u.trim().to_owned());
        // A rule that names nothing matches nothing.
        (name.is_some() || command.is_some() || url.is_some()).then(|| Self {
            name,
            command,
            url,
            file: file.to_path_buf(),
        })
    }

    fn matches(&self, name: &str, entry: &Value) -> bool {
        if self.name.as_deref().is_some_and(|n| n != name) {
            return false;
        }
        if let Some(rule) = &self.command {
            let Some(command) = command_of(entry) else {
                return false;
            };
            if *rule != command {
                return false;
            }
        }
        if let Some(pattern) = &self.url {
            let Some(url) = url_of(entry) else {
                return false;
            };
            if !usable_pattern(pattern) || !glob(&normal_pattern(pattern), &url) {
                return false;
            }
        }
        true
    }

    /// The rule as its file writes it, for a message: `serverName "x" and serverUrl "https://…"`.
    fn shown(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = &self.name {
            parts.push(format!("serverName {}", Value::String(n.clone())));
        }
        if let Some(c) = &self.command {
            let c: Vec<Value> = c.iter().cloned().map(Value::String).collect();
            parts.push(format!("serverCommand {}", Value::Array(c)));
        }
        if let Some(u) = &self.url {
            parts.push(format!("serverUrl {}", Value::String(u.clone())));
        }
        parts.join(" and ")
    }
}

/// A stdio server's command and arguments.
fn command_of(entry: &Value) -> Option<Vec<String>> {
    let obj = entry.as_object()?;
    if obj.get("url").is_some() && obj.get("command").is_none() {
        return None;
    }
    let mut out = vec![obj.get("command")?.as_str()?.to_owned()];
    if let Some(args) = obj.get("args").and_then(Value::as_array) {
        for a in args {
            out.push(a.as_str()?.to_owned());
        }
    }
    Some(out)
}

/// A remote server's address as a browser writes it: scheme and host in lower case, no default port.
fn url_of(entry: &Value) -> Option<String> {
    let obj = entry.as_object()?;
    if obj.get("command").is_some() {
        return None;
    }
    let raw = obj.get("url")?.as_str()?.trim();
    Some(match url::Url::parse(raw) {
        Ok(u) => u.as_str().to_owned(),
        Err(_) => raw.to_owned(),
    })
}

/// Claude Code compares a pattern with the whole address, so one must start with a scheme (`https://`, `*://`) or be
/// `*`; any other matches nothing.
fn usable_pattern(pattern: &str) -> bool {
    let lower = pattern.to_ascii_lowercase();
    pattern == "*"
        || pattern.starts_with("*://")
        || ["http:", "https:", "ws:", "wss:"]
            .iter()
            .any(|s| lower.starts_with(s))
}

/// A `serverUrl` pattern made comparable with [`url_of`]: its scheme and host in lower case, without a default port.
fn normal_pattern(pattern: &str) -> String {
    if pattern == "*" {
        return pattern.to_owned();
    }
    if !pattern.contains('*')
        && let Ok(u) = url::Url::parse(pattern)
    {
        return u.as_str().to_owned();
    }
    let Some((scheme, rest)) = pattern.split_once("://") else {
        return pattern.to_owned();
    };
    let scheme = scheme.to_ascii_lowercase();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let mut authority = authority.to_ascii_lowercase();
    for (s, port) in [
        ("https", ":443"),
        ("wss", ":443"),
        ("http", ":80"),
        ("ws", ":80"),
    ] {
        if (scheme == s || scheme == "*") && authority.ends_with(port) {
            authority.truncate(authority.len() - port.len());
        }
    }
    // A bare host is written with its root path, as a parsed address is.
    let path = if path.is_empty() && !authority.ends_with('*') {
        "/"
    } else {
        path
    };
    format!("{scheme}://{authority}{path}")
}

/// Whether `text` matches `pattern`, where `*` stands for any text, even none.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// The rules of one file.
#[derive(Debug, Default, Clone)]
struct FileRules {
    denied: Vec<Rule>,
    /// `None`: the file has no `allowedMcpServers`.
    allowed: Option<Vec<Rule>>,
    /// `allowManagedMcpServersOnly: true`, which counts only in the organisation's files.
    only_organisation: bool,
}

fn read_rules(path: &Path, warnings: &mut Vec<String>) -> FileRules {
    let doc = match fsx::read_limited(path, MAX_FILE) {
        Ok(None) => return FileRules::default(),
        Ok(Some(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!(
                    "Could not read the server rules in {}: invalid JSON ({e})",
                    show(path)
                ));
                return FileRules::default();
            }
        },
        Err(e) => {
            warnings.push(format!(
                "Could not read the server rules in {}: {e}",
                show(path)
            ));
            return FileRules::default();
        }
    };
    let list = |key: &str| {
        doc.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .take(MAX_RULES)
                .filter_map(|v| Rule::read(v, path))
                .collect::<Vec<_>>()
        })
    };
    FileRules {
        denied: list("deniedMcpServers").unwrap_or_default(),
        allowed: list("allowedMcpServers"),
        only_organisation: doc.get("allowManagedMcpServersOnly") == Some(&Value::Bool(true)),
    }
}

/// The rules where Claude Code runs: outside every project, or in one.
#[derive(Debug, Default, Clone)]
pub(crate) struct Rules {
    denied: Vec<Rule>,
    /// `None`: no file has an allowed list, so nothing is held back by one.
    allowed: Option<Vec<Rule>>,
    /// The files with an allowed list that counts, for a message.
    allowed_files: Vec<PathBuf>,
}

impl Rules {
    fn add(&mut self, file: &Path, rules: &FileRules, allow_counts: bool) {
        self.denied.extend(rules.denied.iter().cloned());
        if let (true, Some(allowed)) = (allow_counts, &rules.allowed) {
            self.allowed
                .get_or_insert_with(Vec::new)
                .extend(allowed.iter().cloned());
            self.allowed_files.push(file.to_path_buf());
        }
    }

    /// Why Claude Code does not load the server `name` (as the file spells it) with `entry`, or `None` when the rules
    /// let it. `organisation`: one the organisation sets, which needs no allowed rule.
    pub(crate) fn blocks(&self, name: &str, entry: &Value, organisation: bool) -> Option<String> {
        if let Some(rule) = self.denied.iter().find(|r| r.matches(name, entry)) {
            return Some(format!(
                "Blocked by policy: deniedMcpServers in {} has {}.",
                show(&rule.file),
                rule.shown()
            ));
        }
        let allowed = self.allowed.as_ref()?;
        if organisation || allowed.iter().any(|r| r.matches(name, entry)) {
            return None;
        }
        let files: Vec<String> = self.allowed_files.iter().map(|f| show(f)).collect();
        Some(format!(
            "Blocked by policy: no rule in allowedMcpServers ({}) lets it load.",
            files.join(", ")
        ))
    }
}

/// The rules of every place Claude Code runs in.
#[derive(Debug, Default)]
pub(crate) struct Policy {
    /// Outside every project first, then each project asked for, in that order.
    pub places: Vec<Rules>,
}

/// Reads the rules for outside every project and for each of `projects` (raw keys; one that is not a local absolute
/// path adds nothing of its own).
pub(crate) fn load(
    paths: &super::ConfigPaths,
    projects: &[&str],
    warnings: &mut Vec<String>,
) -> Policy {
    let user_file = paths.claude_dir.join("settings.json");
    let user = read_rules(&user_file, warnings);
    let mut organisation = Vec::new();
    let mut only_organisation = false;
    if let Some(dir) = &paths.managed_dir {
        for file in managed::settings_files(dir, &mut Vec::new()) {
            let rules = read_rules(&file, warnings);
            only_organisation |= rules.only_organisation;
            organisation.push((file, rules));
        }
    }
    let mut outside = Rules::default();
    for (file, rules) in &organisation {
        outside.add(file, rules, true);
    }
    outside.add(&user_file, &user, !only_organisation);

    let mut places = vec![outside.clone()];
    for project in projects {
        let mut here = outside.clone();
        if inventory::is_safe_project_path(project) {
            let dir = Path::new(project).join(".claude");
            for name in ["settings.json", "settings.local.json"] {
                let file = dir.join(name);
                let rules = read_rules(&file, warnings);
                here.add(&file, &rules, !only_organisation);
            }
        }
        places.push(here);
    }
    Policy { places }
}

fn show(path: &Path) -> String {
    crate::sanitize::sanitize_line(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(v: Value) -> Rule {
        Rule::read(&v, Path::new("/s.json")).unwrap()
    }

    #[test]
    fn a_name_matches_exactly_and_in_its_case() {
        let r = rule(json!({"serverName": "web"}));
        let entry = json!({"command": "x"});
        assert!(r.matches("web", &entry));
        for other in ["Web", "web2", "we"] {
            assert!(!r.matches(other, &entry), "{other}");
        }
        // No wildcard in a name.
        assert!(!rule(json!({"serverName": "w*"})).matches("web", &entry));
    }

    #[test]
    fn a_command_matches_a_stdio_server_exactly() {
        let r = rule(json!({"serverCommand": ["node", "a.js", "--x"]}));
        assert!(r.matches("s", &json!({"command": "node", "args": ["a.js", "--x"]})));
        assert!(!r.matches("s", &json!({"command": "node", "args": ["a.js"]})));
        assert!(!r.matches(
            "s",
            &json!({"command": "node", "args": ["a.js", "--x", "y"]})
        ));
        assert!(
            !rule(json!({"serverCommand": ["node", "*"]}))
                .matches("s", &json!({"command": "node", "args": ["a.js"]}))
        );
        assert!(!r.matches(
            "s",
            &json!({"type": "http", "url": "https://a.example.com/"})
        ));
    }

    #[test]
    fn an_address_matches_with_wildcards_and_a_normal_scheme_host_and_port() {
        let entry = json!({"type": "http", "url": "https://API.Example.com:443/mcp/"});
        for (pattern, want) in [
            ("https://api.example.com/mcp/", true),
            ("https://API.Example.com:443/mcp/", true),
            ("https://api.example.com/mcp", false),
            ("https://api.example.com/MCP/", false),
            ("https://api.example.com/*", true),
            ("https://API.Example.com:443/*", true),
            ("https://*.com/mcp/", true),
            ("*://api.example.com/*", true),
            ("*", true),
            ("*example.com*", false),
            ("http://api.example.com/*", false),
        ] {
            assert_eq!(
                rule(json!({"serverUrl": pattern})).matches("w", &entry),
                want,
                "{pattern}"
            );
        }
        // Only remote servers have an address.
        assert!(!rule(json!({"serverUrl": "*"})).matches("s", &json!({"command": "x"})));
    }

    #[test]
    fn every_field_of_a_rule_must_match() {
        let r = rule(json!({"serverName": "web", "serverUrl": "https://nomatch/*"}));
        assert!(!r.matches(
            "web",
            &json!({"type": "http", "url": "https://a.example.com/"})
        ));
        assert!(Rule::read(&json!({}), Path::new("/s")).is_none());
        assert!(Rule::read(&json!({"other": 1}), Path::new("/s")).is_none());
    }

    struct Env {
        dir: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }
        fn write(&self, rel: &str, v: &Value) {
            let p = self.dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, v.to_string()).unwrap();
        }
        fn project(&self) -> String {
            let p = self.dir.path().join("proj");
            std::fs::create_dir_all(&p).unwrap();
            p.to_string_lossy().into_owned()
        }
        fn load(&self, projects: &[&str]) -> (Policy, Vec<String>) {
            let mut paths = super::super::ConfigPaths::in_dir(self.dir.path().join("cfg"));
            paths.managed_dir = Some(self.dir.path().join("org"));
            let mut warnings = Vec::new();
            (load(&paths, projects, &mut warnings), warnings)
        }
    }

    fn stdio(command: &str) -> Value {
        json!({"command": command})
    }

    #[test]
    fn a_denied_rule_blocks_and_says_which_rule_and_file() {
        let env = Env::new();
        env.write(
            "cfg/settings.json",
            &json!({"deniedMcpServers": [{"serverName": "x"}]}),
        );
        let (policy, warnings) = env.load(&[]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let why = policy.places[0].blocks("x", &stdio("a"), false).unwrap();
        assert!(
            why.contains("deniedMcpServers") && why.contains("serverName \"x\""),
            "{why}"
        );
        assert!(why.contains("settings.json"), "{why}");
        assert!(policy.places[0].blocks("y", &stdio("a"), false).is_none());
        // The organisation's servers too.
        assert!(policy.places[0].blocks("x", &stdio("a"), true).is_some());
    }

    #[test]
    fn allowed_lists_of_every_file_count_together_and_an_empty_one_blocks_all() {
        let env = Env::new();
        let project = env.project();
        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "other"}]}),
        );
        env.write(
            "proj/.claude/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "loc"}]}),
        );
        let (policy, _) = env.load(&[&project]);
        let (outside, here) = (&policy.places[0], &policy.places[1]);
        assert!(outside.blocks("other", &stdio("o"), false).is_none());
        let why = outside.blocks("loc", &stdio("l"), false).unwrap();
        assert!(why.contains("allowedMcpServers"), "{why}");
        assert!(here.blocks("loc", &stdio("l"), false).is_none());
        assert!(here.blocks("other", &stdio("o"), false).is_none());
        // The organisation's servers need no allowed rule.
        assert!(outside.blocks("org", &stdio("g"), true).is_none());

        env.write("cfg/settings.json", &json!({"allowedMcpServers": []}));
        let (policy, _) = env.load(&[]);
        assert!(policy.places[0].blocks("any", &stdio("a"), false).is_some());
    }

    #[test]
    fn a_projects_local_settings_count_there_only() {
        let env = Env::new();
        let project = env.project();
        env.write(
            "proj/.claude/settings.local.json",
            &json!({"deniedMcpServers": [{"serverName": "loc"}]}),
        );
        let (policy, _) = env.load(&[&project, "relative/path"]);
        assert!(policy.places[0].blocks("loc", &stdio("l"), false).is_none());
        assert!(policy.places[1].blocks("loc", &stdio("l"), false).is_some());
        assert!(policy.places[2].blocks("loc", &stdio("l"), false).is_none());
    }

    #[test]
    fn denied_wins_over_allowed() {
        let env = Env::new();
        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "x"}], "deniedMcpServers": [{"serverName": "x"}]}),
        );
        let (policy, _) = env.load(&[]);
        assert!(
            policy.places[0]
                .blocks("x", &stdio("a"), false)
                .unwrap()
                .contains("denied")
        );
    }

    #[test]
    fn with_allow_managed_only_the_organisations_allowed_lists_alone_count() {
        let env = Env::new();
        let project = env.project();
        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "loc"}]}),
        );
        env.write(
            "org/managed-settings.d/10.json",
            &json!({"allowManagedMcpServersOnly": true}),
        );
        // No organisation list: nothing held back, the user's list does not count.
        let (policy, _) = env.load(&[&project]);
        assert!(
            policy.places[0]
                .blocks("mine2", &stdio("m"), false)
                .is_none()
        );

        env.write(
            "org/managed-settings.json",
            &json!({"allowedMcpServers": [{"serverName": "mine2"}]}),
        );
        let (policy, _) = env.load(&[&project]);
        assert!(
            policy.places[1]
                .blocks("mine2", &stdio("m"), false)
                .is_none()
        );
        let why = policy.places[1].blocks("loc", &stdio("l"), false).unwrap();
        assert!(why.contains("managed-settings.json"), "{why}");
        assert!(!why.contains("cfg"), "{why}");
    }

    #[test]
    fn an_unreadable_rules_file_is_said() {
        let env = Env::new();
        let p = env.dir.path().join("cfg/settings.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{nope").unwrap();
        let (policy, warnings) = env.load(&[]);
        assert!(warnings[0].contains("invalid JSON"), "{warnings:?}");
        assert!(policy.places[0].blocks("x", &stdio("a"), false).is_none());
    }
}
