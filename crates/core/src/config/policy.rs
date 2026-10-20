//! Which servers Claude Code's policy lets load: `deniedMcpServers` and `allowedMcpServers` (decision 010).
//!
//! The rules come from the user's `settings.json`, a project's `.claude/settings.json` and `settings.local.json`
//! (that project only), and the organisation's settings files. A server a denied rule matches never loads. Where any of
//! these files has `allowedMcpServers` (even an empty list), only servers an allowed rule matches load; the lists of
//! all files count together, unless the organisation sets `allowManagedMcpServersOnly`, when only its own count. The
//! organisation's own servers need no allowed rule, but a denied rule removes them too.
//!
//! A rule has exactly one key: `serverName` (the name exactly), `serverCommand` (a stdio server's command and
//! arguments exactly) or `serverUrl` (a remote server's address, with `*` for any text). An allowed list judges a
//! remote server by its `serverUrl` rules and a stdio server by its `serverCommand` rules; names count only when the
//! list has no rule of the server's own kind. An allowed name has only letters, digits, `-` and `_`. Names merge
//! first: policy judges the server that won its name.
//!
//! The files are untrusted: size-limited, and only lists of objects with these fields are read.
use super::{fsx, inventory, managed};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = inventory::MAX_MCP_JSON;
/// Rules kept per list and file, at most.
const MAX_RULES: usize = 500;

/// Whose file a rule is in: what the message says and what the user can do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    User,
    Project,
    ProjectLocal,
    Organisation,
}

/// What one rule matches.
#[derive(Debug, Clone, PartialEq)]
enum Match {
    Name(String),
    Command(Vec<String>),
    Url(String),
}

/// One entry of `deniedMcpServers` or `allowedMcpServers`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rule {
    what: Match,
    file: PathBuf,
    source: Source,
}

impl Rule {
    /// `allowed`: an entry of `allowedMcpServers`, whose names are checked more strictly. `None`: Claude Code ignores
    /// the entry.
    fn read(value: &Value, file: &Path, source: Source, allowed: bool) -> Option<Self> {
        let obj = value.as_object()?;
        // Exactly one key: Claude Code ignores an entry with two, even when both would match.
        if obj.len() != 1 {
            return None;
        }
        let (key, value) = obj.iter().next()?;
        let what = match key.as_str() {
            "serverName" => {
                let name = value.as_str()?;
                let usable = if allowed {
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                } else {
                    !name.is_empty() && name.trim() == name
                };
                Match::Name(usable.then(|| name.to_owned())?)
            }
            "serverCommand" => {
                let parts = value
                    .as_array()?
                    .iter()
                    .map(|p| p.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?;
                Match::Command((!parts.is_empty()).then_some(parts)?)
            }
            "serverUrl" => Match::Url(value.as_str().filter(|u| !u.is_empty())?.to_owned()),
            _ => return None,
        };
        Some(Self {
            what,
            file: file.to_path_buf(),
            source,
        })
    }

    fn matches(&self, name: &str, entry: &Value) -> bool {
        match &self.what {
            Match::Name(n) => n == name,
            Match::Command(c) => command_of(entry).is_some_and(|command| *c == command),
            Match::Url(pattern) => url_of(entry).is_some_and(|url| url_matches(pattern, &url)),
        }
    }

    /// The rule in plain words: "the command node a.js".
    fn plain(&self) -> String {
        match &self.what {
            Match::Name(n) => format!("the name {n}"),
            Match::Command(c) => format!("the command {}", c.join(" ")),
            Match::Url(u) => format!("addresses matching {u}"),
        }
    }

    /// The rule as its file writes it: `{"serverName":"x"}`.
    fn raw(&self) -> String {
        let (key, value) = match &self.what {
            Match::Name(n) => ("serverName", Value::String(n.clone())),
            Match::Command(c) => (
                "serverCommand",
                Value::Array(c.iter().cloned().map(Value::String).collect()),
            ),
            Match::Url(u) => ("serverUrl", Value::String(u.clone())),
        };
        serde_json::json!({ key: value }).to_string()
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

/// A remote server's address, in the parts a `serverUrl` pattern is compared with.
#[derive(Debug)]
struct Address {
    scheme: String,
    /// In lower case, without a trailing dot.
    host: String,
    /// The port, or the scheme's default.
    port: Option<u16>,
    /// The path, and the query when there is one.
    path: String,
}

fn url_of(entry: &Value) -> Option<Address> {
    let obj = entry.as_object()?;
    if obj.get("command").is_some() {
        return None;
    }
    let u = url::Url::parse(obj.get("url")?.as_str()?.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let mut path = u.path().to_owned();
    if let Some(q) = u.query() {
        path.push('?');
        path.push_str(q);
    }
    Some(Address {
        scheme: u.scheme().to_owned(),
        host: host.trim_end_matches('.').to_owned(),
        port: u.port_or_known_default(),
        path,
    })
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "https" | "wss" => Some(443),
        "http" | "ws" => Some(80),
        _ => None,
    }
}

/// Whether the `serverUrl` pattern matches `url`, as Claude Code compares them: `*` stands for any text; scheme and
/// host in any case and the host without a trailing dot; the path as written. Without a port, a host written out in
/// full matches the scheme's default port only and a host with `*` every port. Without a path, any path. A pattern
/// that is not `*` and has no `scheme://` matches nothing.
fn url_matches(pattern: &str, url: &Address) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some((scheme, rest)) = pattern.split_once("://") else {
        return false;
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    // An IPv6 host is in brackets, which hold colons of their own.
    let (host, port) = match authority.rfind(':') {
        Some(i) if !authority[i..].contains(']') => (&authority[..i], Some(&authority[i + 1..])),
        _ => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    let port_ok = match port {
        Some(p) => url.port.is_some_and(|n| glob(p, &n.to_string())),
        None => host.contains('*') || url.port == default_port(&url.scheme),
    };
    glob(&scheme.to_ascii_lowercase(), &url.scheme)
        && glob(host, &url.host)
        && port_ok
        && (path.is_empty() || glob(path, &url.path))
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
    /// What turns claude.ai connectors off in this file (decision 011).
    connectors_off: Vec<Off>,
}

/// What in a settings file turns every claude.ai connector off, in the order Claude Code checks (decision 011).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Off {
    /// `disableClaudeAiConnectors: true`, which counts in any file.
    Setting,
    /// `ENABLE_CLAUDEAI_MCP_SERVERS` set to a "no" in `env`.
    Switch,
    /// A `CLAUDE_CODE_USE_…` provider other than Anthropic, set in `env`: that variable.
    Provider(&'static str),
    /// Signing in with an API key or token rather than the claude.ai account: `apiKeyHelper`, or that variable in
    /// `env`.
    Key(&'static str),
}

impl Off {
    /// Claude Code says the first of these that applies.
    fn rank(self) -> u8 {
        match self {
            Self::Setting | Self::Switch => 0,
            Self::Provider(_) => 1,
            Self::Key(_) => 2,
        }
    }
}

/// The variables that make Claude Code use a provider other than Anthropic's own.
const PROVIDERS: [&str; 6] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
];
/// The variables that sign Claude Code in with a key or token, ahead of the claude.ai account.
const KEYS: [&str; 2] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

/// What in the settings `doc` of `source` turns claude.ai connectors off. A project's `env` sets a provider but not
/// the switch or a key; everything else counts in every file (checked with Claude Code 2.1.304).
fn connectors_off(doc: &Value, source: Source) -> Vec<Off> {
    let mut off = Vec::new();
    let env = |key: &str| doc.get("env")?.get(key)?.as_str().map(str::trim);
    let is = |v: &str, words: [&str; 4]| words.contains(&v.to_lowercase().as_str());
    let whole_env = matches!(source, Source::User | Source::Organisation);
    if doc.get("disableClaudeAiConnectors") == Some(&Value::Bool(true)) {
        off.push(Off::Setting);
    }
    if whole_env
        && env("ENABLE_CLAUDEAI_MCP_SERVERS").is_some_and(|v| is(v, ["0", "false", "no", "off"]))
    {
        off.push(Off::Switch);
    }
    if let Some(var) = PROVIDERS
        .into_iter()
        .find(|var| env(var).is_some_and(|v| is(v, ["1", "true", "yes", "on"])))
    {
        off.push(Off::Provider(var));
    }
    if let Some(var) = KEYS
        .into_iter()
        .find(|var| whole_env && env(var).is_some_and(|v| !v.is_empty()))
    {
        off.push(Off::Key(var));
    }
    let helper = doc.get("apiKeyHelper").and_then(Value::as_str);
    if helper.is_some_and(|h| !h.trim().is_empty()) {
        off.push(Off::Key("apiKeyHelper"));
    }
    off
}

fn read_rules(path: &Path, source: Source, warnings: &mut Vec<String>) -> FileRules {
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
    // An entry Claude Code ignores is left out, but the list is still there.
    let list = |key: &str, allowed: bool| {
        doc.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .take(MAX_RULES)
                .filter_map(|v| Rule::read(v, path, source, allowed))
                .collect::<Vec<_>>()
        })
    };
    FileRules {
        denied: list("deniedMcpServers", false).unwrap_or_default(),
        allowed: list("allowedMcpServers", true),
        only_organisation: doc.get("allowManagedMcpServersOnly") == Some(&Value::Bool(true)),
        connectors_off: connectors_off(&doc, source),
    }
}

/// The rules where Claude Code runs: outside every project, or in one.
#[derive(Debug, Default, Clone)]
pub(crate) struct Rules {
    denied: Vec<Rule>,
    /// `None`: no file has an allowed list, so nothing is held back by one.
    allowed: Option<Vec<Rule>>,
    /// The files with an allowed list that counts, and whose they are, for a message.
    allowed_files: Vec<(PathBuf, Source)>,
    /// What turns claude.ai connectors off, in which file and whose, in the order the files are read.
    connectors_off: Vec<(Off, PathBuf, Source)>,
}

/// Whether Claude Code loads a claude.ai connector (decision 011).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForConnector {
    Loads,
    /// It loads only if its address, which Klefki does not know, is on the allowed list: a note, not a warning.
    MayNot(String),
    /// It does not load: why, in plain words with what to do. `policy`: a rule on which servers load, or the
    /// organisation's setting, rather than the user's own choice.
    Held {
        why: String,
        policy: bool,
    },
}

impl Rules {
    fn add(&mut self, file: &Path, source: Source, rules: &FileRules, allow_counts: bool) {
        self.denied.extend(rules.denied.iter().cloned());
        self.connectors_off.extend(
            rules
                .connectors_off
                .iter()
                .map(|&off| (off, file.to_path_buf(), source)),
        );
        if let (true, Some(allowed)) = (allow_counts, &rules.allowed) {
            self.allowed
                .get_or_insert_with(Vec::new)
                .extend(allowed.iter().cloned());
            self.allowed_files.push((file.to_path_buf(), source));
        }
    }

    /// Why Claude Code does not load the server `name` (as the file spells it) with `entry`, in plain words with what
    /// to do, or `None` when the rules let it. `organisation`: one the organisation sets that needs no allowed rule.
    pub(crate) fn blocks(&self, name: &str, entry: &Value, organisation: bool) -> Option<String> {
        if let Some(rule) = self.denied.iter().find(|r| r.matches(name, entry)) {
            let (who, next) = match rule.source {
                Source::User => ("A rule in your settings", "Remove the rule to let it load."),
                Source::Project => (
                    "A rule in the project's settings",
                    "Remove the rule to let it load.",
                ),
                Source::ProjectLocal => (
                    "A rule in the project's local settings",
                    "Remove the rule to let it load.",
                ),
                Source::Organisation => (
                    "Your organisation's policy",
                    "Your administrator can change this.",
                ),
            };
            return Some(format!(
                "{who} blocks {}. {next} (deniedMcpServers in {}: {})",
                rule.plain(),
                shown(&rule.file, rule.source),
                rule.raw()
            ));
        }
        let allowed = self.allowed.as_ref()?;
        if organisation {
            return None;
        }
        // A server is judged by the rules of its own kind; names count only when there are none.
        let own_kind: Vec<&Rule> = if url_of(entry).is_some() {
            allowed
                .iter()
                .filter(|r| matches!(r.what, Match::Url(_)))
                .collect()
        } else if command_of(entry).is_some() {
            allowed
                .iter()
                .filter(|r| matches!(r.what, Match::Command(_)))
                .collect()
        } else {
            Vec::new()
        };
        let lets = if own_kind.is_empty() {
            allowed
                .iter()
                .any(|r| matches!(r.what, Match::Name(_)) && r.matches(name, entry))
        } else {
            own_kind.iter().any(|r| r.matches(name, entry))
        };
        if lets {
            return None;
        }
        let kind = match own_kind.first().map(|r| &r.what) {
            Some(Match::Url(_)) => " The list lets remote servers load by their address only.",
            Some(Match::Command(_)) => " The list lets local servers load by their command only.",
            _ => "",
        };
        let (own_files, theirs) = self.allowed_lists();
        let next = if own_files.is_empty() {
            "Your administrator can change this.".to_owned()
        } else {
            format!(
                "Add a rule for it to allowedMcpServers in {} to let it load.",
                own_files.join(" or ")
            )
        };
        let mut why =
            format!("This server is not on the list of servers allowed to load.{kind} {next}");
        if !theirs.is_empty() {
            let _ = write!(why, " (allowedMcpServers in {})", theirs.join(", "));
        }
        Some(why)
    }

    /// The files with an allowed list that counts, as shown: the ones the user can change, then the organisation's.
    fn allowed_lists(&self) -> (Vec<String>, Vec<String>) {
        let files = |theirs: bool| {
            self.allowed_files
                .iter()
                .filter(|(_, s)| (*s == Source::Organisation) == theirs)
                .map(|(f, s)| shown(f, *s))
                .collect()
        };
        (files(false), files(true))
    }
}

impl Rules {
    /// Whether Claude Code loads the claude.ai connector `name` (decision 011). Klefki does not know a connector's
    /// address, so what needs it is said as unknown: an allowed list with address rules.
    pub(crate) fn for_connector(&self, name: &str) -> ForConnector {
        let first_off = self
            .connectors_off
            .iter()
            .enumerate()
            .min_by_key(|(i, (off, ..))| (off.rank(), *i));
        if let Some((_, (off, file, source))) = first_off {
            return ForConnector::Held {
                why: connectors_off_why(*off, file, *source),
                policy: *source == Source::Organisation,
            };
        }
        let unknown = serde_json::json!({"type": "claudeai-proxy"});
        if self.denied.iter().any(|r| r.matches(name, &unknown)) {
            let why = self.blocks(name, &unknown, false).unwrap_or_default();
            return ForConnector::Held { why, policy: true };
        }
        let Some(allowed) = &self.allowed else {
            return ForConnector::Loads;
        };
        let (own_files, theirs) = self.allowed_lists();
        if allowed.iter().any(|r| matches!(r.what, Match::Url(_))) {
            let mut why = "Your allowed list lets servers load by their address, and Klefki cannot see a \
                           connector's address: Claude Code loads it only if a serverUrl rule matches the address \
                           claude.ai shows for it."
                .to_owned();
            let files: Vec<String> = own_files.into_iter().chain(theirs).collect();
            let _ = write!(why, " (allowedMcpServers in {})", files.join(", "));
            return ForConnector::MayNot(why);
        }
        // A connector's name, with its space, cannot be allowed: only its address can.
        let next = if own_files.is_empty() {
            "Your administrator can change this.".to_owned()
        } else {
            format!(
                "To let it load, add a serverUrl rule with the address claude.ai shows for it to allowedMcpServers in \
                 {}.",
                own_files.join(" or ")
            )
        };
        let mut why = format!(
            "This connector is not on the list of servers allowed to load, which takes a connector by its address \
             only. {next}"
        );
        if !theirs.is_empty() {
            let _ = write!(why, " (allowedMcpServers in {})", theirs.join(", "));
        }
        ForConnector::Held { why, policy: true }
    }
}

/// Why `off`, in `file` of `source`, keeps every claude.ai connector from loading, with what to do.
fn connectors_off_why(off: Off, file: &Path, source: Source) -> String {
    let file = shown(file, source);
    let (whose, theirs) = match source {
        Source::User => ("Your settings", false),
        Source::Project => ("The project's settings", false),
        Source::ProjectLocal => ("The project's local settings", false),
        Source::Organisation => ("Your organisation's settings", true),
    };
    let (what, setting) = match off {
        Off::Setting => (
            format!(
                "{whose} turn off connectors from claude.ai with disableClaudeAiConnectors, in {file}."
            ),
            "disableClaudeAiConnectors",
        ),
        Off::Switch => (
            format!(
                "{whose} turn off connectors from claude.ai with ENABLE_CLAUDEAI_MCP_SERVERS in env, in {file}."
            ),
            "ENABLE_CLAUDEAI_MCP_SERVERS",
        ),
        Off::Provider(var) => (
            format!(
                "{whose} make Claude Code use another provider than Anthropic, with {var} in env, in {file}. \
                 Connectors from claude.ai load only with your claude.ai account."
            ),
            var,
        ),
        Off::Key(var) => {
            let place = if var == "apiKeyHelper" {
                format!("from apiKeyHelper, in {file}")
            } else {
                format!("from {var} in env, in {file}")
            };
            (
                format!(
                    "{whose} sign Claude Code in with a key {place}, ahead of your claude.ai account. Connectors \
                     from claude.ai load only with that account."
                ),
                var,
            )
        }
    };
    if theirs {
        format!("{what} Your administrator can change this.")
    } else {
        format!("{what} Remove {setting} there to let them load.")
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
    let user = read_rules(&user_file, Source::User, warnings);
    let mut organisation = Vec::new();
    let mut only_organisation = false;
    if let Some(dir) = &paths.managed_dir {
        // What goes wrong reading the organisation's files is already said, once, as an organisation note.
        for file in managed::settings_files(dir, &mut Vec::new()) {
            let rules = read_rules(&file, Source::Organisation, &mut Vec::new());
            only_organisation |= rules.only_organisation;
            organisation.push((file, rules));
        }
    }
    let mut outside = Rules::default();
    for (file, rules) in &organisation {
        outside.add(file, Source::Organisation, rules, true);
    }
    outside.add(&user_file, Source::User, &user, !only_organisation);

    let mut places = vec![outside.clone()];
    for project in projects {
        let mut here = outside.clone();
        if inventory::is_safe_project_path(project) {
            let dir = Path::new(project).join(".claude");
            for (name, source) in [
                ("settings.json", Source::Project),
                ("settings.local.json", Source::ProjectLocal),
            ] {
                let file = dir.join(name);
                let rules = read_rules(&file, source, warnings);
                here.add(&file, source, &rules, !only_organisation);
            }
        }
        places.push(here);
    }
    Policy { places }
}

fn show(path: &Path) -> String {
    crate::sanitize::sanitize_line(&path.to_string_lossy())
}

/// A rules file as a message names it: a project's within the project (the message says which), the others with the
/// home folder as `~`.
fn shown(path: &Path, source: Source) -> String {
    match source {
        Source::Project | Source::ProjectLocal => {
            let name = path.file_name().map(|n| n.to_string_lossy());
            format!(".claude/{}", name.unwrap_or_default())
        }
        Source::User | Source::Organisation => {
            let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
            match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
                Some(rest) => show(&Path::new("~").join(rest)),
                None => show(path),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(v: Value) -> Rule {
        Rule::read(&v, Path::new("/s.json"), Source::User, false).unwrap()
    }

    fn allowed_rule(v: &Value) -> Option<Rule> {
        Rule::read(v, Path::new("/s.json"), Source::User, true)
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
            // No path: any path (cross-review of #49, each verified against Claude Code).
            ("https://api.example.com", true),
            ("https://api.example.com:443", true),
            ("https://other.example.com", false),
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
    fn a_port_matches_by_how_the_host_is_written() {
        let at = |url: &str| json!({"type": "http", "url": url});
        for (pattern, url, want) in [
            // A host with `*` in it: every port.
            (
                "https://*.example.com/mcp",
                "https://p.example.com:8443/mcp",
                true,
            ),
            (
                "https://*.example.com/mcp",
                "https://p.example.com/mcp",
                true,
            ),
            // Written out in full: the scheme's default port only.
            (
                "https://p.example.com/*",
                "https://p.example.com:8443/mcp",
                false,
            ),
            (
                "https://p.example.com:8443/*",
                "https://p.example.com:8443/mcp",
                true,
            ),
            (
                "https://p.example.com:8443/*",
                "https://p.example.com/mcp",
                false,
            ),
            // `:*`: any port, the default too.
            (
                "https://plain.example.com:*/*",
                "https://plain.example.com",
                true,
            ),
            (
                "https://plain.example.com:*/*",
                "https://plain.example.com:9/x",
                true,
            ),
            // Any scheme, each on its default port.
            ("*://p.example.com/*", "http://p.example.com/x", true),
            ("*://p.example.com/*", "http://p.example.com:443/x", false),
            // The host's trailing dot does not count, on either side.
            (
                "https://*.example.com/mcp",
                "https://d.example.com./mcp",
                true,
            ),
            (
                "https://d.example.com./mcp",
                "https://d.example.com/mcp",
                true,
            ),
        ] {
            assert_eq!(
                rule(json!({"serverUrl": pattern})).matches("w", &at(url)),
                want,
                "{pattern} {url}"
            );
        }
    }

    // Replaces `every_field_of_a_rule_must_match`, which held the opposite: Claude Code ignores an entry with more
    // than one key, even when every key would match (cross-review of #49, verified live; the docs say "Each entry is
    // an object with a single key").
    #[test]
    fn a_rule_with_more_than_one_key_is_ignored() {
        for v in [
            json!({"serverName": "web", "serverUrl": "https://a.example.com/*"}),
            json!({"serverName": "web", "comment": "x"}),
            json!({}),
            json!({"other": 1}),
            json!({"serverName": ""}),
            json!({"serverCommand": []}),
            json!({"serverName": " web"}),
        ] {
            assert!(
                Rule::read(&v, Path::new("/s"), Source::User, false).is_none(),
                "{v}"
            );
        }
    }

    #[test]
    fn an_allowed_name_has_only_letters_digits_dashes_and_underscores() {
        assert!(allowed_rule(&json!({"serverName": "my-server_2"})).is_some());
        for name in ["plugin:p1:rooted", "p1:rooted", "claude.ai Slack", "a b"] {
            assert!(
                allowed_rule(&json!({"serverName": name})).is_none(),
                "{name}"
            );
            // Any name may be denied.
            assert!(rule(json!({"serverName": name})).matches(name, &json!({"command": "x"})));
        }
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
        // In plain words first, with what to do; the rule as written last.
        assert!(
            why.starts_with("A rule in your settings blocks the name x. Remove the rule"),
            "{why}"
        );
        assert!(
            why.contains(r#"settings.json: {"serverName":"x"})"#),
            "{why}"
        );
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

    fn http(url: &str) -> Value {
        json!({"type": "http", "url": url})
    }

    #[test]
    fn an_allowed_list_judges_a_server_by_the_rules_of_its_own_kind() {
        let env = Env::new();
        let project = env.project();
        // A stdio server needs a command rule once there is one; its name no longer counts.
        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [
                {"serverName": "x"},
                {"serverCommand": ["node", "a.js", "--x"]},
                {"serverName": "plain"}
            ]}),
        );
        let (policy, _) = env.load(&[]);
        let outside = &policy.places[0];
        let why = outside.blocks("x", &stdio("other"), false).unwrap();
        assert!(why.contains("by their command only"), "{why}");
        assert!(
            outside
                .blocks(
                    "y",
                    &json!({"command": "node", "args": ["a.js", "--x"]}),
                    false
                )
                .is_none()
        );
        // A remote server: no address rules, so its name counts.
        assert!(
            outside
                .blocks("plain", &http("https://p.example.com/"), false)
                .is_none()
        );

        // An address rule makes names stop counting for remote servers, not for stdio ones.
        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "plain"}, {"serverUrl": "https://q.example.com/*"}]}),
        );
        let (policy, _) = env.load(&[&project]);
        let outside = &policy.places[0];
        let why = outside
            .blocks("plain", &http("https://p.example.com/"), false)
            .unwrap();
        assert!(why.contains("by their address only"), "{why}");
        assert!(
            outside
                .blocks("z", &http("https://q.example.com/mcp"), false)
                .is_none()
        );
        assert!(outside.blocks("plain", &stdio("p"), false).is_none());

        // The lists of a place count together: a project's command rule stops the user's name rule there.
        env.write(
            "proj/.claude/settings.json",
            &json!({"allowedMcpServers": [{"serverCommand": ["other"]}]}),
        );
        let (policy, _) = env.load(&[&project]);
        assert!(
            policy.places[0]
                .blocks("plain", &stdio("p"), false)
                .is_none()
        );
        let why = policy.places[1]
            .blocks("plain", &stdio("p"), false)
            .unwrap();
        assert!(why.contains(".claude/settings.json"), "{why}");
    }

    #[test]
    fn a_plugins_server_cannot_be_allowed_by_name_but_can_be_denied_by_it() {
        let env = Env::new();
        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "plugin:p1:rooted"}, {"serverName": "rooted"}]}),
        );
        let (policy, _) = env.load(&[]);
        // The invalid entry is ignored but the list is there: nothing it names loads.
        assert!(
            policy.places[0]
                .blocks("plugin:p1:rooted", &stdio("r"), false)
                .is_some()
        );
        assert!(
            policy.places[0]
                .blocks("rooted", &stdio("r"), false)
                .is_none()
        );

        env.write(
            "cfg/settings.json",
            &json!({"deniedMcpServers": [{"serverName": "plugin:p1:rooted"}]}),
        );
        let (policy, _) = env.load(&[]);
        assert!(
            policy.places[0]
                .blocks("plugin:p1:rooted", &stdio("r"), false)
                .is_some()
        );
    }

    #[test]
    fn what_to_do_depends_on_whose_rule_it_is() {
        let env = Env::new();
        env.write(
            "org/managed-settings.json",
            &json!({"deniedMcpServers": [{"serverCommand": ["bad"]}], "allowedMcpServers": [{"serverName": "ok"}]}),
        );
        let (policy, _) = env.load(&[]);
        let why = policy.places[0].blocks("x", &stdio("bad"), false).unwrap();
        assert!(why.starts_with("Your organisation's policy blocks the command bad. Your administrator can change this."), "{why}");
        let why = policy.places[0].blocks("x", &stdio("good"), false).unwrap();
        assert!(why.contains("Your administrator can change this."), "{why}");

        env.write(
            "cfg/settings.json",
            &json!({"allowedMcpServers": [{"serverName": "mine"}]}),
        );
        let (policy, _) = env.load(&[]);
        let why = policy.places[0].blocks("x", &stdio("good"), false).unwrap();
        assert!(
            why.contains("Add a rule for it to allowedMcpServers in"),
            "{why}"
        );
        assert!(!why.contains("administrator"), "{why}");
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
