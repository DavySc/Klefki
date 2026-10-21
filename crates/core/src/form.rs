//! The Add dialog's form: a new server's parts as the user types them, turned into a definition that is checked and
//! described the way a pasted one is, and a definition turned back into parts (to finish a pasted server there).
use crate::api::TransportKind;
use crate::config::entry;
use crate::paste::{self, PastedServer};
use crate::sanitize::sanitize;
use crate::words::sentence;
use serde_json::{Map, Value};

/// A new server's parts, as typed. Empty rows of `env` and `headers` are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFields {
    /// Stdio, Http or Sse; the others cannot be added from the form.
    pub transport: TransportKind,
    /// The program and its arguments, written as in a terminal (`npx -y @x/fs "C:\My files"`). Stdio only.
    pub command: String,
    /// The server's address. Http and sse only.
    pub url: String,
    /// Environment variables for the program. Stdio only.
    pub env: Vec<(String, String)>,
    /// Headers sent with every request. Http and sse only.
    pub headers: Vec<(String, String)>,
    /// The provider gave OAuth client settings. Http and sse only; with none filled in (and no client secret)
    /// there are none, as most servers sign the user in without them.
    pub oauth: bool,
    /// Optional: without it Claude Code registers itself with the server.
    pub client_id: String,
    /// A client secret was typed. The secret itself is not a part: it never goes in a definition.
    pub has_secret: bool,
    /// Optional: a fixed port for the sign-in to come back to, for servers that need it registered.
    pub callback_port: String,
    /// Optional, separated by spaces.
    pub scopes: String,
    /// Optional: where the sign-in server describes itself, when it is not found on its own. https only.
    pub metadata_url: String,
}

impl Default for ServerFields {
    fn default() -> Self {
        Self {
            transport: TransportKind::Stdio,
            command: String::new(),
            url: String::new(),
            env: Vec::new(),
            headers: Vec::new(),
            oauth: false,
            client_id: String::new(),
            has_secret: false,
            callback_port: String::new(),
            scopes: String::new(),
            metadata_url: String::new(),
        }
    }
}

/// The part of the form a problem is about, so it is said next to that part.
/// Ordered as the form shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    Command,
    Address,
    Variables,
    Headers,
    ClientId,
    CallbackPort,
    Scopes,
    MetadataUrl,
    /// The definition as a whole: a rule no single part breaks, such as its size.
    Definition,
}

impl ServerFields {
    /// The server these parts make, called `name`, as a pasted one would be: its definition, a one-line summary,
    /// why it cannot be added yet (the first thing to fix; [`problems`](Self::problems) has them all), and hints.
    pub fn read(&self, name: &str) -> PastedServer {
        let mut notes = Vec::new();
        let (entry, problems) = self.definition(&mut notes);
        let mut server = match problems.into_iter().next() {
            None => {
                let mut server = PastedServer::with_entry(name, entry, &mut notes);
                // The form says which values look like secrets next to each one (see [`secret_reference`]).
                server.hints = serde_json::from_str(&server.json)
                    .map(|entry| paste::path_hints(&entry))
                    .unwrap_or_default();
                server
            }
            Some((_, problem)) => PastedServer::refused(name, problem),
        };
        server.hints.extend(notes);
        server
    }

    /// Everything that stops these parts from making a server, each with the part it is about, in the form's order.
    pub fn problems(&self) -> Vec<(Part, String)> {
        let (entry, mut problems) = self.definition(&mut Vec::new());
        if problems.is_empty()
            && let Some(problem) = PastedServer::with_entry("", entry, &mut Vec::new()).problem
        {
            problems.push((Part::Definition, problem));
        }
        problems
    }

    /// The definition, and what is wrong with each part (nothing, when it can be added).
    fn definition(&self, notes: &mut Vec<String>) -> (Value, Vec<(Part, String)>) {
        let mut entry = Map::new();
        let mut problems = Vec::new();
        match self.transport {
            TransportKind::Stdio => {
                entry.insert("type".into(), "stdio".into());
                match self.command_words(notes) {
                    Ok(words) => {
                        entry.insert("command".into(), words[0].clone().into());
                        entry.insert(
                            "args".into(),
                            words[1..].iter().map(|w| Value::from(w.as_str())).collect(),
                        );
                    }
                    Err(problem) => problems.push((Part::Command, problem)),
                }
                // A program does not sign in with OAuth: settings left from an address are not shown, and not used.
                match pairs("variable", &self.env) {
                    Ok(Some(env)) => {
                        entry.insert("env".into(), env);
                    }
                    Ok(None) => {}
                    Err(problem) => problems.push((Part::Variables, problem)),
                }
            }
            TransportKind::Http | TransportKind::Sse => {
                let url = self.url.trim();
                entry.insert("type".into(), self.transport.label().into());
                entry.insert("url".into(), url.into());
                if let Err(problem) = address_problem(&entry) {
                    problems.push((Part::Address, problem));
                }
                match pairs("header", &self.headers) {
                    Ok(Some(headers)) => {
                        entry.insert("headers".into(), headers);
                    }
                    Ok(None) => {}
                    Err(problem) => problems.push((Part::Headers, problem)),
                }
                if self.oauth {
                    let (oauth, wrong) = self.oauth_settings();
                    problems.extend(wrong);
                    if !oauth.is_empty() || self.has_secret {
                        entry.insert("oauth".into(), Value::Object(oauth));
                    }
                }
            }
            TransportKind::Ws | TransportKind::ClaudeAi | TransportKind::Invalid => {
                problems.push((
                    Part::Address,
                    "Choose a program (stdio), http or sse.".to_owned(),
                ));
            }
        }
        problems.sort_by_key(|(part, _)| *part);
        (Value::Object(entry), problems)
    }

    fn command_words(&self, notes: &mut Vec<String>) -> Result<Vec<String>, String> {
        let lexed = paste::lex(&self.command);
        let words = match lexed.commands.as_slice() {
            [] => return Err("Type the command that starts the server.".to_owned()),
            [one] => match one.shell {
                Some(what) => {
                    return Err(format!(
                        "The command uses the shell ({what}). Claude Code starts the program directly, without a shell."
                    ));
                }
                None => one.words.clone(),
            },
            _ => {
                return Err(
                    "Type one command. Claude Code starts the program directly, without a shell."
                        .to_owned(),
                );
            }
        };
        if !lexed.variables.is_empty() {
            notes.push(format!(
                "Claude Code fills in {} from its own environment when it starts the server.",
                paste::listed(&lexed.variables, |v| format!("${{{v}}}"))
            ));
        }
        Ok(words)
    }

    /// The `oauth` settings filled in, and what is wrong with any of them.
    fn oauth_settings(&self) -> (Map<String, Value>, Vec<(Part, String)>) {
        let mut oauth = Map::new();
        let mut wrong = Vec::new();
        let id = self.client_id.trim();
        if !id.is_empty() {
            oauth.insert("clientId".into(), id.into());
        }
        let port = self.callback_port.trim();
        if !port.is_empty() {
            match port.parse::<u16>().ok().filter(|p| *p > 0) {
                Some(port) => {
                    oauth.insert("callbackPort".into(), port.into());
                }
                None => wrong.push((
                    Part::CallbackPort,
                    "The callback port must be a number from 1 to 65535.".to_owned(),
                )),
            }
        }
        let scopes = self.scopes.split_whitespace().collect::<Vec<_>>().join(" ");
        if !scopes.is_empty() {
            oauth.insert("scopes".into(), scopes.into());
        }
        let metadata = self.metadata_url.trim();
        if !metadata.is_empty() {
            oauth.insert("authServerMetadataUrl".into(), metadata.into());
        }
        // Each setting on its own, by Claude Code's rules, so a problem is said next to its field.
        for (key, part) in [
            ("clientId", Part::ClientId),
            ("scopes", Part::Scopes),
            ("authServerMetadataUrl", Part::MetadataUrl),
        ] {
            let Some(value) = oauth.get(key) else {
                continue;
            };
            let alone = serde_json::json!({"type": "http", "url": "https://x.invalid", "oauth": {key: value}});
            if let Err(why) = entry::validate_new(&alone) {
                wrong.push((part, sentence(&why)));
            }
        }
        (oauth, wrong)
    }

    /// Puts `https://` before an address typed without a scheme (`mcp.example.com/mcp`), as a browser does.
    pub fn complete_url(&mut self) {
        let url = self.url.trim();
        if !url.is_empty() && !url.contains("://") && !url.contains(char::is_whitespace) {
            self.url = format!("https://{url}");
        }
    }

    /// A name for the server, from what it connects to or runs: `linear` for `https://mcp.linear.app/mcp`,
    /// `filesystem` for `npx -y @modelcontextprotocol/server-filesystem`. Empty when nothing gives one.
    pub fn suggested_name(&self) -> String {
        let raw = match self.transport {
            TransportKind::Stdio => program_name(&self.command),
            _ => host_name(&self.url),
        };
        let name: String = raw
            .unwrap_or_default()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .take(64)
            .collect();
        name.trim_matches('-').to_ascii_lowercase()
    }

    /// The parts of `json`, a definition, to finish in the form: `None` when it has anything the form does not
    /// show (a header command, a setting it does not know, a WebSocket address), which the form would drop.
    pub fn from_definition(json: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(json).ok()?;
        let object = value.as_object()?;
        let text = |key: &str| object.get(key).and_then(Value::as_str);
        let list = |key: &str| -> Option<Vec<(String, String)>> {
            match object.get(key) {
                None => Some(Vec::new()),
                Some(Value::Object(values)) => values
                    .iter()
                    .map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect(),
                Some(_) => None,
            }
        };
        let mut fields = Self::default();
        match text("type").unwrap_or("stdio") {
            "stdio" => {
                let known = ["type", "command", "args", "env"];
                if object.keys().any(|k| !known.contains(&k.as_str())) {
                    return None;
                }
                let mut words = vec![shell_word(text("command")?)];
                if let Some(args) = object.get("args") {
                    for arg in args.as_array()? {
                        words.push(shell_word(arg.as_str()?));
                    }
                }
                fields.command = words.join(" ");
                fields.env = list("env")?;
            }
            kind @ ("http" | "sse") => {
                let known = ["type", "url", "headers", "oauth"];
                if object.keys().any(|k| !known.contains(&k.as_str())) {
                    return None;
                }
                fields.transport = if kind == "http" {
                    TransportKind::Http
                } else {
                    TransportKind::Sse
                };
                fields.url = text("url")?.to_owned();
                fields.headers = list("headers")?;
                if let Some(oauth) = object.get("oauth") {
                    let oauth = oauth.as_object()?;
                    let known = [
                        "clientId",
                        "callbackPort",
                        "scopes",
                        "authServerMetadataUrl",
                    ];
                    if oauth.keys().any(|k| !known.contains(&k.as_str())) {
                        return None;
                    }
                    fields.oauth = true;
                    let get = |key: &str| -> Option<String> {
                        match oauth.get(key) {
                            None => Some(String::new()),
                            Some(Value::Number(n)) if key == "callbackPort" => {
                                Some(n.as_u64()?.to_string())
                            }
                            Some(v) => Some(v.as_str()?.to_owned()),
                        }
                    };
                    fields.client_id = get("clientId")?;
                    fields.callback_port = get("callbackPort")?;
                    fields.scopes = get("scopes")?;
                    fields.metadata_url = get("authServerMetadataUrl")?;
                }
            }
            _ => return None,
        }
        Some(fields)
    }
}

/// What is wrong with the address of `entry`, a remote definition with only its type and url, said so the user
/// knows what to type.
fn address_problem(entry: &Map<String, Value>) -> Result<(), String> {
    let url = entry.get("url").and_then(Value::as_str).unwrap_or_default();
    if url.is_empty() {
        return Err("Type the server's address.".to_owned());
    }
    if !url.contains("://") {
        return Err("Start the address with https://.".to_owned());
    }
    entry::validate(&Value::Object(entry.clone())).map_err(|why| sentence(&why))
}

/// The label of the host that names the service: `linear` in `mcp.linear.app`, `githubcopilot` in
/// `api.githubcopilot.com`. `None` for an address with no such label, such as an IP address.
fn host_name(url: &str) -> Option<String> {
    let url = url.trim();
    let full = if url.contains("://") {
        url.to_owned()
    } else {
        format!("https://{url}")
    };
    let host = url::Url::parse(&full).ok()?.host_str()?.to_owned();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return None;
    }
    let mut labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() > 1 {
        labels.pop();
    }
    labels
        .into_iter()
        .rev()
        .find(|l| !matches!(*l, "mcp" | "api" | "www" | "server" | "co" | "com"))
        .map(str::to_owned)
}

/// The package or program a command runs, without what every server's name has: `filesystem` for
/// `npx -y @modelcontextprotocol/server-filesystem /srv`, `github` for `C:\tools\github-mcp-server.exe`.
fn program_name(command: &str) -> Option<String> {
    let lexed = paste::lex(command);
    let words = &lexed.commands.first()?.words;
    let stem = |w: &str| -> String {
        let file = w.rsplit(['/', '\\']).next().unwrap_or(w);
        let file = file
            .strip_suffix(".exe")
            .or_else(|| file.strip_suffix(".js"))
            .or_else(|| file.strip_suffix(".py"))
            .or_else(|| file.strip_suffix(".jar"))
            .unwrap_or(file);
        file.to_owned()
    };
    let launchers = [
        "npx", "uvx", "bunx", "pnpx", "node", "python", "python3", "py", "deno", "bun", "uv",
        "pnpm", "yarn", "docker", "podman", "java", "dotnet",
    ];
    let program = stem(words.first()?).to_ascii_lowercase();
    let what = if launchers.contains(&program.as_str()) {
        // The first word after the launcher that is not an option or a launcher's own verb.
        let verbs = ["run", "dlx", "exec", "x", "tool"];
        let found = words[1..]
            .iter()
            .find(|w| !verbs.contains(&w.as_str()) && !w.starts_with('-'))?;
        // A package's scope and version are not its name: `@x/fs@1.2` is `fs`.
        let package = found.rsplit('/').next()?;
        let package = package.split_once('@').map_or(package, |(p, _)| p);
        let package = package.split_once(':').map_or(package, |(p, _)| p);
        stem(package)
    } else {
        program
    };
    let mut name = what.to_ascii_lowercase();
    for prefix in ["mcp-server-", "server-", "mcp-"] {
        if let Some(rest) = name.strip_prefix(prefix).filter(|r| !r.is_empty()) {
            name = rest.to_owned();
            break;
        }
    }
    for suffix in ["-mcp-server", "-mcp", "-server", "_mcp"] {
        if let Some(rest) = name.strip_suffix(suffix).filter(|r| !r.is_empty()) {
            name = rest.to_owned();
            break;
        }
    }
    (!name.is_empty()).then_some(name)
}

/// Why the sign-in could not come back to `port` if it started now, or `None` when it could (or for a reason
/// Klefki cannot name). Claude Code listens on 127.0.0.1 at that port while the user signs in, and stops at once
/// when it cannot. The browser is sent to `localhost`, which it may try as `::1` first, so a program there would
/// take the sign-in instead, and Claude Code would wait for nothing: both are tried. It is a warning, not a
/// problem: the port is the one registered with the provider, and the program holding it may be gone by the time
/// the user signs in.
pub(crate) fn callback_port_warning(port: u16) -> Option<String> {
    if port == 0 {
        return None;
    }
    let loopbacks = [
        std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST),
        std::net::Ipv6Addr::LOCALHOST.into(),
    ];
    // Each is let go at once. On Unix, Rust's listener reuses addresses as Node's does; what the native Claude Code
    // sets on Windows is not verified. A computer without IPv6 says nothing about `::1`.
    let errors: Vec<std::io::ErrorKind> = loopbacks
        .iter()
        .filter_map(|at| std::net::TcpListener::bind((*at, port)).err())
        .map(|e| e.kind())
        .collect();
    if errors.contains(&std::io::ErrorKind::AddrInUse) {
        Some(format!(
            "Port {port} is in use by another program. Signing in will fail until that program is closed, or register a different port with the provider."
        ))
    } else if errors.contains(&std::io::ErrorKind::PermissionDenied) {
        Some(format!(
            "Port {port} is reserved on this computer, so signing in would fail. Use another port if the provider lets you register one."
        ))
    } else {
        None
    }
}

/// The `${VAR}` reference to write instead of `value`, the value of the variable or header `name`, when it looks
/// like a secret typed in; `None` otherwise. For a "use a reference instead" button.
pub fn secret_reference(name: &str, value: &str) -> Option<String> {
    crate::config::secretlike::suggestion(name, value)
}

/// `word` written so the form's command line reads it back as that one word: as it is when nothing in it is special
/// to a shell, else in double quotes.
fn shell_word(word: &str) -> String {
    let special = |c: char| c.is_whitespace() || "'\"`$&|;<>()#^".contains(c);
    if !word.is_empty() && !word.contains(special) && !word.ends_with('\\') {
        return word.to_owned();
    }
    let mut out = String::from('"');
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        match c {
            '"' | '`' => out.push('\\'),
            '\\' if next.is_none_or(|n| "\"\\$`".contains(n)) => out.push('\\'),
            '$' if next == Some('(') => out.push('\\'),
            _ => {}
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The rows as a JSON object, in order, leaving out empty rows; `None` when there are none.
fn pairs(what: &str, rows: &[(String, String)]) -> Result<Option<Value>, String> {
    let mut out = Map::new();
    for (name, value) in rows {
        let name = name.trim();
        if name.is_empty() {
            if value.is_empty() {
                continue;
            }
            return Err(format!("A {what} has a value but no name."));
        }
        if out.contains_key(name) {
            return Err(format!("The {what} {} is there twice.", sanitize(name)));
        }
        out.insert(name.to_owned(), value.clone().into());
    }
    Ok((!out.is_empty()).then_some(Value::Object(out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stdio(command: &str) -> ServerFields {
        ServerFields {
            command: command.to_owned(),
            ..ServerFields::default()
        }
    }

    fn remote(url: &str) -> ServerFields {
        ServerFields {
            transport: TransportKind::Http,
            url: url.to_owned(),
            ..ServerFields::default()
        }
    }

    fn definition(server: &PastedServer) -> Value {
        assert_eq!(server.problem, None, "{server:?}");
        serde_json::from_str(&server.json).unwrap()
    }

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }

    #[test]
    fn a_program_is_split_into_its_words_as_a_terminal_would() {
        let mut f =
            stdio(r#"npx -y @modelcontextprotocol/server-filesystem "C:\My files" '/tmp/a b'"#);
        f.env = vec![
            ("ROOT".into(), "/srv".into()),
            (String::new(), String::new()),
            ("MODE".into(), "ro".into()),
        ];
        let s = f.read("files");
        assert_eq!(s.name, "files");
        let d = definition(&s);
        assert_eq!(
            d,
            json!({"type": "stdio", "command": "npx",
                   "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:\\My files", "/tmp/a b"],
                   "env": {"ROOT": "/srv", "MODE": "ro"}})
        );
        // In the order Claude Code writes them.
        assert_eq!(keys(&d), ["type", "command", "args", "env"]);
        assert!(s.summary.starts_with("npx -y"), "{}", s.summary);
        // No environment, no empty object.
        assert_eq!(
            keys(&definition(&stdio("node server.js").read("n"))),
            ["type", "command", "args"]
        );
    }

    #[test]
    fn variables_are_left_for_claude_code_to_fill_in() {
        let s = stdio("node $HOME/server.js --token ${API_TOKEN}").read("n");
        assert_eq!(
            definition(&s)["args"],
            json!(["${HOME}/server.js", "--token", "${API_TOKEN}"])
        );
        assert!(
            s.hints
                .iter()
                .any(|h| h.contains("${HOME}") && h.contains("${API_TOKEN}")),
            "{:?}",
            s.hints
        );
        // Many names are counted, not all listed (the note is laid out every frame); every argument is kept.
        let names: String = (0..20).map(|i| format!(" $V{i}")).collect();
        let s = stdio(&format!("node{names}")).read("n");
        assert_eq!(definition(&s)["args"][19], json!("${V19}"));
        let [note] = s.hints.as_slice() else {
            panic!("{:?}", s.hints)
        };
        assert!(
            note.contains("${V7} and 12 more from") && !note.contains("${V8}"),
            "{note}"
        );
    }

    #[test]
    fn an_address_with_headers_and_oauth_becomes_what_claude_code_takes() {
        let mut f = remote("  https://mcp.example.com/mcp ");
        f.headers = vec![("X-Team".into(), "${TEAM}".into())];
        f.oauth = true;
        f.client_id = " client-7 ".into();
        f.callback_port = "8123".into();
        f.scopes = " read   write ".into();
        let d = definition(&f.read("sso"));
        assert_eq!(
            d,
            json!({"type": "http", "url": "https://mcp.example.com/mcp", "headers": {"X-Team": "${TEAM}"},
                   "oauth": {"clientId": "client-7", "callbackPort": 8123, "scopes": "read write"}})
        );
        assert_eq!(keys(&d), ["type", "url", "headers", "oauth"]);
        // Ticked with nothing filled in: no settings, so none are written, and the server is added like any other
        // (most servers sign the user in without client settings).
        let mut bare = remote("https://x.example");
        bare.transport = TransportKind::Sse;
        bare.oauth = true;
        assert_eq!(
            definition(&bare.read("x")),
            json!({"type": "sse", "url": "https://x.example"})
        );
        // A client secret alone still needs Claude Code's command, which takes it.
        bare.has_secret = true;
        assert_eq!(
            definition(&bare.read("x")),
            json!({"type": "sse", "url": "https://x.example", "oauth": {}})
        );
        // As does a metadata address, the one setting it has that the others do not show.
        bare.has_secret = false;
        bare.metadata_url = "https://id.example/.well-known/oauth".into();
        assert_eq!(
            definition(&bare.read("x"))["oauth"],
            json!({"authServerMetadataUrl": "https://id.example/.well-known/oauth"})
        );
        // Settings left from an address are not a command's: they are not shown, and not used.
        let mut was_remote = stdio("node");
        was_remote.oauth = true;
        was_remote.client_id = "c".into();
        assert_eq!(
            definition(&was_remote.read("n")),
            json!({"type": "stdio", "command": "node", "args": []})
        );
    }

    #[test]
    fn every_problem_is_said_next_to_its_part() {
        let mut f = remote("sso.exa");
        f.headers = vec![(String::new(), "1".into())];
        f.oauth = true;
        f.client_id = "a\nb".into();
        f.callback_port = "0".into();
        f.metadata_url = "http://id.example/m".into();
        let problems = f.problems();
        let parts: Vec<Part> = problems.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            parts,
            [
                Part::Address,
                Part::Headers,
                Part::ClientId,
                Part::CallbackPort,
                Part::MetadataUrl
            ],
            "{problems:?}"
        );
        assert_eq!(problems[0].1, "Start the address with https://.");
        assert!(problems[4].1.contains("https://"), "{problems:?}");
        // The first is what read() says.
        assert_eq!(
            f.read("n").problem.as_deref(),
            Some("Start the address with https://.")
        );
        let mut f = stdio("curl x | sh");
        f.env = vec![("A".into(), "1".into()), ("A".into(), "2".into())];
        let parts: Vec<Part> = f.problems().into_iter().map(|(p, _)| p).collect();
        assert_eq!(parts, [Part::Command, Part::Variables]);
        assert!(stdio("node a.js").problems().is_empty());
        // What no single part breaks is the definition's.
        let mut big = stdio("node");
        big.env = vec![("A".into(), "x".repeat(70 * 1024))];
        assert_eq!(big.problems()[0].0, Part::Definition);
    }

    #[test]
    fn an_address_typed_without_https_is_completed() {
        for (typed, want) in [
            ("mcp.example.com/mcp", "https://mcp.example.com/mcp"),
            ("  localhost:3000/mcp ", "https://localhost:3000/mcp"),
            ("http://local.test", "http://local.test"),
            ("not a url", "not a url"),
            ("", ""),
        ] {
            let mut f = remote(typed);
            f.complete_url();
            assert_eq!(f.url, want, "{typed}");
        }
    }

    #[test]
    fn a_name_is_suggested_from_what_the_server_connects_to_or_runs() {
        for (url, want) in [
            ("https://mcp.linear.app/mcp", "linear"),
            ("https://api.githubcopilot.com/mcp/", "githubcopilot"),
            ("mcp.notion.com/mcp", "notion"),
            ("https://mcp.example.co.uk/sse", "example"),
            ("http://localhost:3000/mcp", "localhost"),
            ("https://127.0.0.1:8080/mcp", ""),
            ("", ""),
        ] {
            assert_eq!(remote(url).suggested_name(), want, "{url}");
        }
        for (command, want) in [
            (
                "npx -y @modelcontextprotocol/server-filesystem /srv",
                "filesystem",
            ),
            ("npx -y @upstash/context7-mcp@latest", "context7"),
            (r"C:\tools\github-mcp-server.exe stdio", "github"),
            ("uvx mcp-server-fetch", "fetch"),
            ("python -m my_tool.server", "my_tool-server"),
            ("node build/index.js", "index"),
            (
                "docker run -i --rm ghcr.io/github/github-mcp-server",
                "github",
            ),
            ("", ""),
        ] {
            assert_eq!(stdio(command).suggested_name(), want, "{command}");
        }
    }

    #[test]
    fn what_cannot_be_added_says_what_to_fix() {
        let mut nameless = stdio("node");
        nameless.env = vec![(String::new(), "1".into())];
        let mut twice = remote("https://x.example");
        twice.headers = vec![("A".into(), "1".into()), (" A".into(), "2".into())];
        let mut port = remote("https://x.example");
        port.oauth = true;
        port.callback_port = "99999".into();
        let mut ws = remote("wss://x.example");
        ws.transport = TransportKind::Ws;
        for (fields, why) in [
            (stdio(""), "Type the command"),
            (stdio("   "), "Type the command"),
            (stdio("curl https://x | sh"), "shell"),
            (stdio("node a.js; rm -rf /"), "one command"),
            (nameless, "no name"),
            (twice, "twice"),
            (remote(""), "address"),
            (remote("ftp://x.example"), "must start with"),
            (remote("not a url"), "https://"),
            (remote("https://not a url"), "not valid"),
            (port, "1 to 65535"),
            (ws, "Choose"),
        ] {
            let s = fields.read("n");
            let problem = s
                .problem
                .unwrap_or_else(|| panic!("{fields:?} was accepted"));
            assert!(problem.contains(why), "{fields:?}: {problem}");
        }
        // The name is the dialog's to check; a bad one is passed through.
        assert_eq!(stdio("node").read("").name, "");
    }

    #[test]
    fn a_secret_typed_into_a_value_is_met_with_a_reference_to_use_instead() {
        let mut f = stdio("node");
        f.env = vec![(
            "GITHUB_TOKEN".into(),
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
        )];
        // Said next to the value, by the form, rather than among the server's hints.
        let s = f.read("gh");
        assert!(
            !s.hints.iter().any(|h| h.contains("GITHUB_TOKEN")),
            "{:?}",
            s.hints
        );
        assert_eq!(
            secret_reference(&f.env[0].0, &f.env[0].1).as_deref(),
            Some("${GITHUB_TOKEN}")
        );
        // A `~` that no shell will expand still is a hint.
        let s = stdio("node ~/server.js").read("n");
        assert!(
            s.hints
                .iter()
                .any(|h| h.contains("Argument 1 starts with ~")),
            "{:?}",
            s.hints
        );
        assert_eq!(
            secret_reference("Authorization", "Bearer sk-live-abcdefghijklmnop").as_deref(),
            Some("Bearer ${AUTH_TOKEN}")
        );
        assert_eq!(secret_reference("MODE", "read-only"), None);
    }

    #[test]
    fn a_definition_comes_back_as_the_parts_that_make_it() {
        for json in [
            r#"{"type": "stdio", "command": "npx", "args": ["-y", "C:\\My files", "it's"], "env": {"A": "1"}}"#,
            r#"{"type": "stdio", "command": "C:\\Program Files\\node.exe", "args": []}"#,
            r##"{"type": "stdio", "command": "node", "args": ["a\"b", "x\\", "$(x)", "${HOME}/a", "#x", "", "a`b", "C:\\tools\\x.js", "^"]}"##,
            r#"{"type": "http", "url": "https://x.example", "headers": {"H": "v"}, "oauth": {"clientId": "c", "callbackPort": 8123, "scopes": "a b"}}"#,
            r#"{"type": "sse", "url": "https://x.example"}"#,
            r#"{"type": "http", "url": "https://x.example", "oauth": {"authServerMetadataUrl": "https://m.example/x"}}"#,
        ] {
            let fields = ServerFields::from_definition(json).unwrap_or_else(|| panic!("{json}"));
            let back = definition(&fields.read("n"));
            let want: Value = serde_json::from_str(json).unwrap();
            assert_eq!(back, want, "{fields:?}");
        }
        // What the form cannot show is not taken in, so nothing is dropped without a word.
        for json in [
            r#"{"type": "http", "url": "https://x.example", "headersHelper": "get-token"}"#,
            r#"{"type": "ws", "url": "wss://x.example"}"#,
            r#"{"command": "x", "cwd": "/tmp"}"#,
            r#"{"type": "http", "url": "https://x.example", "oauth": {"clientSecret": "s"}}"#,
            r#"{"command": "x", "env": {"A": 1}}"#,
            "not json",
        ] {
            assert_eq!(ServerFields::from_definition(json), None, "{json}");
        }
    }

    #[test]
    fn a_callback_port_another_program_holds_is_warned_about_and_a_free_one_is_not() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};
        // Held as Claude Code holds it while signing in, and on `::1`, where a browser may take `localhost` first. On
        // Linux a program listening on every address holds it too; macOS lets Claude Code take 127.0.0.1 beside
        // such a program, and the check takes it the same way.
        let mut holders = vec![IpAddr::from(Ipv4Addr::LOCALHOST)];
        // Every computer the tests run on that has IPv6 checks it; one without cannot hold `::1` at all.
        if TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_ok() {
            holders.push(Ipv6Addr::LOCALHOST.into());
        }
        if cfg!(target_os = "linux") {
            holders.push(Ipv4Addr::UNSPECIFIED.into());
        }
        for at in holders {
            let held = TcpListener::bind((at, 0)).unwrap();
            let port = held.local_addr().unwrap().port();
            let warning = callback_port_warning(port).unwrap_or_else(|| panic!("{at}:{port}"));
            assert!(
                warning.starts_with(&format!("Port {port} is in use by another program.")),
                "{warning}"
            );
            assert!(warning.contains("Signing in will fail"), "{warning}");
            drop(held);
            // Another holder can have the number for a moment: a child process another test starts keeps a copy of
            // the listener until it runs its program, and another test can take the address family not held here.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let mut warning = callback_port_warning(port);
            while warning.is_some() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
                warning = callback_port_warning(port);
            }
            assert_eq!(warning, None, "{at}:{port} once let go");
        }
        assert_eq!(callback_port_warning(0), None);
    }
}
