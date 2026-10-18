//! The Add dialog's form: a new server's parts as the user types them, turned into a definition that is checked and
//! described the way a pasted one is, and a definition turned back into parts (to finish a pasted server there).
use crate::api::TransportKind;
use crate::paste::{self, PastedServer};
use crate::sanitize::sanitize;
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
    /// Signs in with OAuth. Http and sse only. The client secret is not a part: it never goes in a definition.
    pub oauth: bool,
    /// Optional: without it Claude Code registers itself with the server.
    pub client_id: String,
    /// Optional: a fixed port for the sign-in to come back to, for servers that need it registered.
    pub callback_port: String,
    /// Optional, separated by spaces.
    pub scopes: String,
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
            callback_port: String::new(),
            scopes: String::new(),
        }
    }
}

impl ServerFields {
    /// The server these parts make, called `name`, as a pasted one would be: its definition, a one-line summary,
    /// why it cannot be added yet (the first thing to fix), and hints about secrets typed into values.
    pub fn read(&self, name: &str) -> PastedServer {
        let mut notes = Vec::new();
        let mut server = match self.definition(&mut notes) {
            Ok(entry) => {
                let mut server = PastedServer::with_entry(name, entry, &mut notes);
                // The form says which values look like secrets next to each one (see [`secret_reference`]).
                server.hints = serde_json::from_str(&server.json)
                    .map(|entry| paste::path_hints(&entry))
                    .unwrap_or_default();
                server
            }
            Err(problem) => PastedServer::refused(name, problem),
        };
        server.hints.extend(notes);
        server
    }

    fn definition(&self, notes: &mut Vec<String>) -> Result<Value, String> {
        let mut entry = Map::new();
        match self.transport {
            TransportKind::Stdio => {
                let lexed = paste::lex(&self.command);
                let words = match lexed.commands.as_slice() {
                    [] => return Err("Type the command that starts the server.".to_owned()),
                    [one] => match one.shell {
                        Some(what) => {
                            return Err(format!(
                                "The command uses the shell ({what}). Claude Code starts the program directly, without a shell."
                            ));
                        }
                        None => &one.words,
                    },
                    _ => {
                        return Err(
                            "Type one command. Claude Code starts the program directly, without a shell."
                                .to_owned(),
                        );
                    }
                };
                if !lexed.variables.is_empty() {
                    let names: Vec<String> = lexed
                        .variables
                        .iter()
                        .map(|v| format!("${{{v}}}"))
                        .collect();
                    notes.push(format!(
                        "Claude Code fills in {} from its own environment when it starts the server.",
                        names.join(", ")
                    ));
                }
                entry.insert("type".into(), "stdio".into());
                entry.insert("command".into(), words[0].clone().into());
                entry.insert(
                    "args".into(),
                    words[1..].iter().map(|w| Value::from(w.as_str())).collect(),
                );
                if self.oauth {
                    return Err("Only a server with an address signs in with OAuth.".to_owned());
                }
                if let Some(env) = pairs("variable", &self.env)? {
                    entry.insert("env".into(), env);
                }
            }
            TransportKind::Http | TransportKind::Sse => {
                let url = self.url.trim();
                if url.is_empty() {
                    return Err("Type the server's address.".to_owned());
                }
                entry.insert("type".into(), self.transport.label().into());
                entry.insert("url".into(), url.into());
                if let Some(headers) = pairs("header", &self.headers)? {
                    entry.insert("headers".into(), headers);
                }
                if self.oauth {
                    entry.insert("oauth".into(), self.oauth_settings()?);
                }
            }
            TransportKind::Ws | TransportKind::Invalid => {
                return Err("Choose a program (stdio), http or sse.".to_owned());
            }
        }
        Ok(Value::Object(entry))
    }

    fn oauth_settings(&self) -> Result<Value, String> {
        let mut oauth = Map::new();
        let id = self.client_id.trim();
        if !id.is_empty() {
            oauth.insert("clientId".into(), id.into());
        }
        let port = self.callback_port.trim();
        if !port.is_empty() {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or("The callback port must be a number from 1 to 65535.")?;
            oauth.insert("callbackPort".into(), port.into());
        }
        let scopes = self.scopes.split_whitespace().collect::<Vec<_>>().join(" ");
        if !scopes.is_empty() {
            oauth.insert("scopes".into(), scopes.into());
        }
        Ok(Value::Object(oauth))
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
                    let known = ["clientId", "callbackPort", "scopes"];
                    if oauth.keys().any(|k| !known.contains(&k.as_str())) {
                        return None;
                    }
                    fields.oauth = true;
                    let get = |key: &str| oauth.get(key);
                    fields.client_id = match get("clientId") {
                        None => String::new(),
                        Some(v) => v.as_str()?.to_owned(),
                    };
                    fields.callback_port = match get("callbackPort") {
                        None => String::new(),
                        Some(v) => v.as_u64()?.to_string(),
                    };
                    fields.scopes = match get("scopes") {
                        None => String::new(),
                        Some(v) => v.as_str()?.to_owned(),
                    };
                }
            }
            _ => return None,
        }
        Some(fields)
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
        // OAuth with nothing filled in: Claude Code registers itself.
        let mut bare = remote("https://x.example");
        bare.transport = TransportKind::Sse;
        bare.oauth = true;
        assert_eq!(
            definition(&bare.read("x")),
            json!({"type": "sse", "url": "https://x.example", "oauth": {}})
        );
    }

    #[test]
    fn what_cannot_be_added_says_what_to_fix() {
        let mut with_oauth = stdio("node");
        with_oauth.oauth = true;
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
            (with_oauth, "address"),
            (nameless, "no name"),
            (twice, "twice"),
            (remote(""), "address"),
            (remote("ftp://x.example"), "must start with"),
            (remote("not a url"), "not valid"),
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
            r#"{"type": "http", "url": "https://x.example", "oauth": {"authServerMetadataUrl": "https://m.example"}}"#,
            r#"{"command": "x", "env": {"A": 1}}"#,
            "not json",
        ] {
            assert_eq!(ServerFields::from_definition(json), None, "{json}");
        }
    }
}
