//! Reads servers out of pasted text: `claude mcp add …` and `claude mcp add-json …` commands (one per line), or JSON
//! (`{"mcpServers": {…}}`, a map of names to definitions, a `"name": {…}` fragment, or one definition). Nothing
//! pasted is ever run: the text is only read, and what it would add goes through the same review as every other
//! change.
use crate::api::Scope;
use crate::config::entry::MAX_ENTRY_BYTES;
use crate::sanitize::{clip, sanitize};
use serde_json::{Map, Value};

/// What was found in the pasted text, for the add dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pasted {
    pub servers: Vec<PastedServer>,
    /// How the text was read, where that is worth knowing ("Skipped a line that is not a claude mcp add command").
    pub notes: Vec<String>,
}

/// One server from the pasted text. Sanitised for display, except `json`, which is what would be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedServer {
    /// As pasted, sanitised; may be empty (a lone definition) or not a usable name, so the dialog lets it be changed.
    pub name: String,
    /// The definition, ready for [`Change::Add`](crate::api::Change::Add). Empty when there is nothing to add.
    pub json: String,
    /// What it runs or connects to, on one line: `npx -y pkg`, `http · https://…`.
    pub summary: String,
    /// The scope a command asks for: user, or local (`-s local`, and Claude Code's default). `None` for JSON, which
    /// does not say, and for a command that asks for project scope.
    pub scope: Option<Scope>,
    /// Why it cannot be added as it is; `None` when it can.
    pub problem: Option<String>,
    /// Values that look like secrets, and the `${VAR}` reference to write instead. Never the value itself.
    pub hints: Vec<String>,
    /// It signs in with OAuth (its definition has `oauth` settings), so Claude Code's own command adds it.
    pub oauth: bool,
    /// The command asks for a client secret (`--client-secret`), which is never pasted: the dialog has it finished
    /// in the form, where the secret is typed.
    pub asks_secret: bool,
}

/// Pasted text larger than this is refused: a few servers are a few KiB.
const MAX_PASTE_BYTES: usize = 256 * 1024;
const MAX_SERVERS: usize = 100;
/// A summary longer than this is cut; the review shows everything in full.
const MAX_SUMMARY_CHARS: usize = 300;
const SECRET_AFTER_OPTION: &str = "--client-secret takes no value: Claude Code asks for the secret. The word after it may be the secret, so nothing of this command is shown. Paste it with the name before --client-secret and no secret, and type the secret in the form.";
const PROJECT_SCOPE: &str = "Klefki does not add project servers: they live in the repository's .mcp.json. Choose the user or a project's own list instead.";

impl Pasted {
    /// Reads `text`. Fails only when nothing in it can be read as a server, saying why.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_PASTE_BYTES {
            return Err(format!(
                "That is more than {} KiB; paste the servers you want to add.",
                MAX_PASTE_BYTES / 1024
            ));
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("Paste a claude mcp add command or an mcpServers block.".to_owned());
        }
        let mut pasted = if looks_like_json(trimmed) {
            from_json(trimmed)?
        } else {
            from_commands(text)?
        };
        if pasted.servers.len() > MAX_SERVERS {
            return Err(format!(
                "That holds {} servers; Klefki adds at most {MAX_SERVERS} at a time.",
                pasted.servers.len()
            ));
        }
        let mut seen = Vec::new();
        pasted.notes.retain(|n| {
            let new = !seen.contains(n);
            seen.push(n.clone());
            new
        });
        Ok(pasted)
    }
}

impl PastedServer {
    fn new(name: &str) -> Self {
        Self {
            name: sanitize(name),
            json: String::new(),
            summary: String::new(),
            scope: None,
            problem: None,
            hints: Vec::new(),
            oauth: false,
            asks_secret: false,
        }
    }

    pub(crate) fn refused(name: &str, problem: String) -> Self {
        Self {
            problem: Some(problem),
            ..Self::new(name)
        }
    }

    /// Checks a definition as the engine will, and says what it runs; `entry` is written as given.
    pub(crate) fn with_entry(name: &str, mut entry: Value, notes: &mut Vec<String>) -> Self {
        undo_git_bash_drive(&mut entry, notes);
        let mut server = Self::new(name);
        if let Err(why) = crate::config::entry::validate_new(&entry) {
            server.problem = Some(capitalised(&why));
        }
        server.oauth = entry.get("oauth").is_some();
        server.summary = summary(&entry);
        server.hints = hints(&entry);
        server.json = serde_json::to_string_pretty(&entry).unwrap_or_default();
        if server.json.len() > MAX_ENTRY_BYTES {
            server.problem.get_or_insert(format!(
                "Its definition is longer than {} KiB.",
                MAX_ENTRY_BYTES / 1024
            ));
        }
        server
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(c.as_str());
            if !out.ends_with('.') {
                out.push('.');
            }
            out
        }
        None => String::new(),
    }
}

// --- JSON ------------------------------------------------------------------------------------------------------

/// `{…}`, `[…]`, or a `"name": {…}` fragment. A command can start with a quote too (`"C:\Program Files\…\claude.exe"
/// mcp add …`), so a leading string counts only when a colon follows it.
fn looks_like_json(text: &str) -> bool {
    if text.starts_with(['{', '[']) {
        return true;
    }
    let Some(rest) = text.strip_prefix('"') else {
        return false;
    };
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return rest[i + 1..].trim_start().starts_with(':'),
            _ => {}
        }
    }
    false
}

fn from_json(text: &str) -> Result<Pasted, String> {
    // A `"name": {…},` fragment copied out of a larger file.
    let text = text.trim_end_matches(',').trim_end();
    let parsed = if text.starts_with('"') {
        serde_json::from_str(&format!("{{{text}}}"))
    } else {
        serde_json::from_str(text)
    };
    let value: Value =
        parsed.map_err(|e| format!("This is not valid JSON: {}.", sanitize(&e.to_string())))?;
    let Value::Object(top) = value else {
        return Err(
            "This JSON is not an object of servers, such as {\"mcpServers\": {…}}.".to_owned(),
        );
    };
    let mut notes = Vec::new();
    let list = ["mcpServers", "servers"]
        .into_iter()
        .find_map(|key| match top.get(key) {
            Some(Value::Object(servers)) if !is_definition(&top) => Some((key, servers)),
            _ => None,
        });
    let servers = match list {
        Some((key, servers)) => {
            if top.len() > 1 {
                notes.push(format!(
                    "Only \"{key}\" was read; the rest of the JSON is left out."
                ));
            }
            servers
        }
        None if is_definition(&top) => {
            let entry = typed("", &top, &mut notes);
            let server = PastedServer::with_entry("", entry, &mut notes);
            return Ok(Pasted {
                servers: vec![server],
                notes,
            });
        }
        None => &top,
    };
    if !servers.values().any(Value::is_object) {
        return Err(
            "This JSON holds no server definitions, such as {\"mcpServers\": {\"name\": {\"command\": …}}}."
                .to_owned(),
        );
    }
    let servers = servers
        .iter()
        .map(|(name, entry)| match entry {
            Value::Object(definition) => {
                PastedServer::with_entry(name, typed(name, definition, &mut notes), &mut notes)
            }
            _ => PastedServer::refused(name, "This is not a server definition.".to_owned()),
        })
        .collect();
    Ok(Pasted { servers, notes })
}

/// The definition, with `"type": "http"` put first when it has an address and no type: other clients write remote
/// servers that way, and Claude Code would read it as a program.
fn typed(name: &str, definition: &Map<String, Value>, notes: &mut Vec<String>) -> Value {
    if definition.contains_key("type")
        || definition.contains_key("command")
        || !definition.get("url").is_some_and(Value::is_string)
    {
        return Value::Object(definition.clone());
    }
    let what = if name.is_empty() {
        "The definition".to_owned()
    } else {
        sanitize(name)
    };
    notes.push(format!(
        "{what} has an address but no type, so it is added as http (\"type\": \"http\")."
    ));
    let mut out = Map::new();
    out.insert("type".into(), "http".into());
    out.extend(definition.clone());
    Value::Object(out)
}

/// Is this object one server's definition, rather than a map of them?
fn is_definition(object: &Map<String, Value>) -> bool {
    ["command", "url", "type"]
        .iter()
        .any(|k| object.get(*k).is_some_and(Value::is_string))
}

// --- Commands --------------------------------------------------------------------------------------------------

fn from_commands(text: &str) -> Result<Pasted, String> {
    let lexed = lex(text);
    let mut notes = Vec::new();
    let mut servers = Vec::new();
    for command in &lexed.commands {
        let words: Vec<&str> = command.words.iter().map(String::as_str).collect();
        let adds = match words.as_slice() {
            [claude, "mcp", verb @ ("add" | "add-json"), rest @ ..] if is_claude(claude) => {
                Some((*verb, rest))
            }
            _ => None,
        };
        match (adds, command.shell) {
            (Some(_), Some(what)) => servers.push(PastedServer::refused(
                "",
                format!(
                    "This command uses the shell ({what}), and Klefki never runs pasted text. Paste the claude mcp add command on its own."
                ),
            )),
            (Some(("add", rest)), None) => servers.push(add(rest, &mut notes)),
            (Some((_, rest)), None) => servers.push(add_json(rest, &mut notes)),
            (None, _) => notes.push(format!(
                "Skipped a line that is not a claude mcp add command: {}",
                clip(&sanitize(&command.words.join(" ")), 60)
            )),
        }
    }
    if servers.is_empty() {
        return Err(
            "Nothing here adds a server. Paste a claude mcp add command or an mcpServers block."
                .to_owned(),
        );
    }
    if !lexed.variables.is_empty() {
        let names: Vec<String> = lexed.variables.iter().map(|v| format!("${v}")).collect();
        notes.push(format!(
            "Your shell would fill in {} when the command runs. Claude Code fills {} in instead, from its own environment, when it starts the server.",
            names.join(", "),
            if names.len() == 1 { "it" } else { "them" }
        ));
    }
    Ok(Pasted { servers, notes })
}

fn is_claude(word: &str) -> bool {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    base.eq_ignore_ascii_case("claude") || base.eq_ignore_ascii_case("claude.exe")
}

/// `claude mcp add [options] <name> <commandOrUrl> [args...]`, read the way Claude Code reads it, except that an
/// address without `--transport` is taken as http (which is what it means), an option Claude Code does not have is
/// the program's own once the program is named, and `-e` and `-H` take only the words that look like their values.
fn add(words: &[&str], notes: &mut Vec<String>) -> PastedServer {
    let mut scope = Some(Scope::Local);
    let mut transport: Option<&str> = None;
    let mut env: Vec<(String, String)> = Vec::new();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut positional: Vec<&str> = Vec::new();
    let mut oauth: Option<Map<String, Value>> = None;
    let mut asks_secret = false;
    let mut problem: Option<String> = None;
    let mut i = 0;
    let mut rest_are_args = false;
    while i < words.len() {
        let word = words[i];
        i += 1;
        if rest_are_args || (positional.len() >= 2 && !is_option(word)) {
            positional.push(word);
            continue;
        }
        if word == "--" {
            rest_are_args = true;
            continue;
        }
        if !word.starts_with('-') || word == "-" {
            positional.push(word);
            continue;
        }
        let (option, inline) = match word.split_once('=') {
            Some((o, v)) if o.starts_with("--") => (o, Some(v)),
            _ => (word, None),
        };
        // The option's value: after `=`, or the next word.
        let value = |i: &mut usize| -> Option<&str> {
            if inline.is_some() {
                return inline;
            }
            let v = words.get(*i).copied();
            *i += 1;
            v
        };
        match option {
            "-s" | "--scope" => match value(&mut i) {
                Some("user") => scope = Some(Scope::User),
                Some("local") => scope = Some(Scope::Local),
                Some("project") => {
                    scope = None;
                    notes.push(PROJECT_SCOPE.to_owned());
                }
                other => {
                    problem.get_or_insert(format!(
                        "The scope must be user or local, not {:?}.",
                        sanitize(other.unwrap_or_default())
                    ));
                }
            },
            "-t" | "--transport" => transport = Some(value(&mut i).unwrap_or_default()),
            "-e" | "--env" => {
                let mut first = value(&mut i);
                // Variadic, as in Claude Code: more KEY=value words may follow.
                while let Some(v) = first.take().or_else(|| {
                    let next = words.get(i).filter(|w| looks_like_env(w))?;
                    i += 1;
                    Some(*next)
                }) {
                    match v.split_once('=') {
                        Some((k, val)) if !k.is_empty() => env.push((k.to_owned(), val.to_owned())),
                        _ => {
                            problem.get_or_insert(format!(
                                "-e takes KEY=value, not {:?}.",
                                sanitize(v)
                            ));
                        }
                    }
                }
            }
            "-H" | "--header" => {
                let mut first = value(&mut i);
                while let Some(v) = first.take().or_else(|| {
                    let next = words.get(i).filter(|w| looks_like_header(w))?;
                    i += 1;
                    Some(*next)
                }) {
                    match v.split_once(':') {
                        Some((k, val)) if !k.trim().is_empty() => {
                            headers.push((k.trim().to_owned(), val.trim().to_owned()));
                        }
                        _ => {
                            problem.get_or_insert(format!(
                                "-H takes \"Name: value\", not {:?}.",
                                sanitize(v)
                            ));
                        }
                    }
                }
            }
            "--client-id" => {
                let id = value(&mut i).unwrap_or_default();
                oauth
                    .get_or_insert_default()
                    .insert("clientId".into(), id.into());
            }
            "--callback-port" => {
                let port = value(&mut i).unwrap_or_default();
                match port.parse::<u16>() {
                    Ok(p) if p > 0 => {
                        oauth
                            .get_or_insert_default()
                            .insert("callbackPort".into(), p.into());
                    }
                    _ => {
                        problem.get_or_insert(format!(
                            "--callback-port takes a port from 1 to 65535, not {:?}.",
                            sanitize(port)
                        ));
                    }
                }
            }
            // Claude Code asks for the secret itself; the option takes no value.
            "--client-secret" => {
                if inline.is_some() || words.get(i).is_some_and(|w| may_be_secret(w, "://")) {
                    return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
                }
                oauth.get_or_insert_default();
                asks_secret = true;
            }
            _ => {
                problem.get_or_insert(format!(
                    "Claude Code's mcp add has no option {}.",
                    sanitize(option)
                ));
            }
        }
    }
    if asks_secret && positional.len() != 2 {
        // Nothing of it is shown: a secret may have been typed after --client-secret, among the words.
        return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
    }
    let name = positional.first().copied().unwrap_or_default();
    let mut server = match (positional.get(1), problem) {
        (_, Some(problem)) => PastedServer::refused(name, problem),
        _ if positional.is_empty() => {
            PastedServer::refused("", "The command does not name the server.".to_owned())
        }
        (None, None) => PastedServer::refused(
            name,
            "The command does not say what to run, or which address to connect to.".to_owned(),
        ),
        (Some(&target), None) => {
            let args = &positional[2..];
            let remote = target.starts_with("https://") || target.starts_with("http://");
            let kind = match transport {
                Some(t) => t,
                None if remote => {
                    notes.push(format!(
                        "{} is read as an http server: an address needs --transport http when you give it to Claude Code.",
                        sanitize(name)
                    ));
                    "http"
                }
                None => "stdio",
            };
            match kind {
                "stdio" if oauth.is_some() => PastedServer::refused(
                    name,
                    "Only a server with an address signs in with OAuth (--client-id, --client-secret, --callback-port).".to_owned(),
                ),
                "stdio" => {
                    let mut entry = Map::new();
                    entry.insert("type".into(), "stdio".into());
                    entry.insert("command".into(), target.into());
                    entry.insert(
                        "args".into(),
                        args.iter().map(|a| Value::from(*a)).collect(),
                    );
                    entry.insert("env".into(), pairs(&env));
                    if !headers.is_empty() {
                        PastedServer::refused(
                            name,
                            "-H sets headers for a remote server; this one runs a program."
                                .to_owned(),
                        )
                    } else {
                        PastedServer::with_entry(name, Value::Object(entry), notes)
                    }
                }
                "http" | "sse" if !args.is_empty() => PastedServer::refused(
                    name,
                    format!(
                        "A remote server takes only an address, but {} follows it.",
                        sanitize(&args.join(" "))
                    ),
                ),
                "http" | "sse" if !env.is_empty() => PastedServer::refused(
                    name,
                    "-e sets the environment of a program; this server is an address.".to_owned(),
                ),
                "http" | "sse" => {
                    let mut entry = Map::new();
                    entry.insert("type".into(), kind.into());
                    entry.insert("url".into(), target.into());
                    if !headers.is_empty() {
                        entry.insert("headers".into(), pairs(&headers));
                    }
                    if let Some(oauth) = oauth {
                        entry.insert("oauth".into(), Value::Object(oauth));
                    }
                    PastedServer::with_entry(name, Value::Object(entry), notes)
                }
                other => PastedServer::refused(
                    name,
                    format!(
                        "The transport must be stdio, http or sse, not {:?}.",
                        sanitize(other)
                    ),
                ),
            }
        }
    };
    server.scope = scope;
    server.asks_secret = asks_secret && server.problem.is_none();
    server
}

/// `claude mcp add-json [-s scope] <name> <json>`.
fn add_json(words: &[&str], notes: &mut Vec<String>) -> PastedServer {
    let mut scope = None;
    let mut positional = Vec::new();
    let mut asks_secret = false;
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        i += 1;
        match word.split_once('=') {
            Some(("--scope", v)) => scope = Some(v),
            _ if word == "-s" || word == "--scope" => {
                scope = words.get(i).copied();
                i += 1;
            }
            Some(("--client-secret", _)) => {
                return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
            }
            _ if word == "--client-secret" => {
                if words.get(i).is_some_and(|w| may_be_secret(w, "{")) {
                    return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
                }
                asks_secret = true;
            }
            _ => positional.push(word),
        }
    }
    let scope = match scope {
        None | Some("local") => Some(Scope::Local),
        Some("user") => Some(Scope::User),
        Some("project") => {
            notes.push(PROJECT_SCOPE.to_owned());
            None
        }
        Some(other) => {
            return PastedServer::refused(
                positional.first().copied().unwrap_or_default(),
                format!(
                    "The scope must be user or local, not {:?}.",
                    sanitize(other)
                ),
            );
        }
    };
    if asks_secret && positional.len() != 2 {
        return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
    }
    let mut server = match positional.as_slice() {
        [name, json] => match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(mut entry)) => {
                if asks_secret && !entry.contains_key("oauth") {
                    entry.insert("oauth".into(), Value::Object(Map::new()));
                }
                PastedServer::with_entry(name, Value::Object(entry), notes)
            }
            _ => PastedServer::refused(
                name,
                "The definition after the name is not a JSON object.".to_owned(),
            ),
        },
        [] => PastedServer::refused("", "The command does not name the server.".to_owned()),
        [name] => PastedServer::refused(
            name,
            "The command has no JSON definition after the name.".to_owned(),
        ),
        [name, ..] => PastedServer::refused(
            name,
            "The JSON definition should be one argument: put it in single quotes.".to_owned(),
        ),
    };
    server.scope = scope;
    server.asks_secret = asks_secret && server.problem.is_none();
    server
}

/// Whether `next`, the word after `--client-secret`, may be a secret typed after it: anything but an option or the
/// address or definition that may follow the option (it has `follows`: `://` or `{`). A name is taken for a secret
/// too, since the two cannot be told apart.
fn may_be_secret(next: &str, follows: &str) -> bool {
    !next.starts_with('-') && !next.contains(follows)
}

fn is_option(word: &str) -> bool {
    matches!(
        word.split_once('=').map_or(word, |(o, _)| o),
        "-s" | "--scope"
            | "-t"
            | "--transport"
            | "-e"
            | "--env"
            | "-H"
            | "--header"
            | "--client-id"
            | "--client-secret"
            | "--callback-port"
    ) || word == "--"
}

/// `KEY=value`, with a name a shell would accept.
fn looks_like_env(word: &&str) -> bool {
    word.split_once('=').is_some_and(|(k, _)| {
        k.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `Name: value`, and not an address (`https://…` has a colon too).
fn looks_like_header(word: &&str) -> bool {
    !word.contains("://")
        && word.split_once(':').is_some_and(|(k, _)| {
            !k.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c))
        })
}

fn pairs(pairs: &[(String, String)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

/// Git Bash rewrites `/c` to `C:/` on the way to a Windows program, so `cmd /c npx …` arrives as `cmd C:/ npx …`.
fn undo_git_bash_drive(entry: &mut Value, notes: &mut Vec<String>) {
    let is_cmd = entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| {
            let base = c.rsplit(['/', '\\']).next().unwrap_or(c);
            base.eq_ignore_ascii_case("cmd") || base.eq_ignore_ascii_case("cmd.exe")
        });
    if !is_cmd {
        return;
    }
    if let Some(first) = entry.pointer_mut("/args/0")
        && let Some(s) = first.as_str()
        && s.len() == 3
        && s.as_bytes()[0].is_ascii_alphabetic()
        && &s[1..] == ":/"
    {
        notes.push(format!(
            "Read {s} after cmd as /c: Git Bash turns /c into a drive on its way to a Windows program."
        ));
        *first = Value::String(format!("/{}", s[..1].to_ascii_lowercase()));
    }
}

/// What a definition runs or connects to, on one line.
fn summary(entry: &Value) -> String {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default();
    let line = match text("type") {
        "" | "stdio" => {
            let mut words = vec![quoted(text("command"))];
            if let Some(Value::Array(args)) = entry.get("args") {
                words.extend(args.iter().map(|a| quoted(a.as_str().unwrap_or("?"))));
            }
            words.join(" ")
        }
        kind => format!("{kind} \u{b7} {}", text("url")),
    };
    clip(&sanitize(&line), MAX_SUMMARY_CHARS)
}

fn quoted(word: &str) -> String {
    if word.is_empty() || word.contains([' ', '\t', '"']) {
        format!("\"{}\"", word.replace('"', "\\\""))
    } else {
        word.to_owned()
    }
}

/// One line per environment or header value that looks like a secret, and per `~` path (only a shell turns `~` into
/// the home folder, and Claude Code starts a server without one).
fn hints(entry: &Value) -> Vec<String> {
    let mut out = path_hints(entry);
    for key in ["env", "headers"] {
        if let Some(Value::Object(values)) = entry.get(key) {
            for (name, value) in values {
                if let Some(instead) = value
                    .as_str()
                    .and_then(|v| crate::config::secretlike::suggestion(name, v))
                {
                    out.push(format!(
                        "{} looks like a secret. Claude Code can fill it in from your environment: write {instead} instead.",
                        sanitize(name)
                    ));
                }
            }
        }
    }
    out
}

/// One line per `~` path in the command or its arguments.
pub(crate) fn path_hints(entry: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let tilde = |s: &str| s == "~" || s.starts_with("~/") || s.starts_with("~\\");
    if entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(tilde)
    {
        out.push("The command starts with ~, which only a shell turns into your home folder; Claude Code starts the server without one. Write the full path.".to_owned());
    }
    if let Some(Value::Array(args)) = entry.get("args") {
        for (i, arg) in args.iter().enumerate() {
            if arg.as_str().is_some_and(tilde) {
                out.push(format!(
                    "Argument {} starts with ~, which only a shell turns into your home folder; Claude Code starts the server without one. Write the full path.",
                    i + 1
                ));
            }
        }
    }
    out
}

// --- Words -----------------------------------------------------------------------------------------------------

pub(crate) struct Lexed {
    pub commands: Vec<Words>,
    /// Shell variables turned into `${NAME}`, each once.
    pub variables: Vec<String>,
}

/// One command's words, after quotes and escapes.
#[derive(Default)]
pub(crate) struct Words {
    pub words: Vec<String>,
    /// What in it needs a shell to run (a pipe, a redirect, a substitution, an open quote): the rest of its line was
    /// not read.
    pub shell: Option<&'static str>,
}

/// Splits pasted shell text into commands and words the way a POSIX shell would, plus the line continuations of
/// PowerShell (`` ` ``) and cmd (`^`). A backslash only escapes what a shell would need escaping, so a Windows path
/// keeps its backslashes. A command that needs a shell to run (pipes, redirects, substitutions) is marked, and the
/// rest of its line skipped; the other lines are still read.
pub(crate) fn lex(text: &str) -> Lexed {
    let mut commands: Vec<Words> = Vec::new();
    let mut variables: Vec<String> = Vec::new();
    let mut current = Words::default();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    let end_word = |current: &mut Words, word: &mut String, in_word: &mut bool| {
        if *in_word {
            current.words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    let end_command = |commands: &mut Vec<Words>, current: &mut Words| {
        if !current.words.is_empty() {
            commands.push(std::mem::take(current));
        }
    };
    while let Some(c) = chars.next() {
        let shell: Option<&'static str> = match c {
            ' ' | '\t' | '\r' => {
                end_word(&mut current, &mut word, &mut in_word);
                None
            }
            '\n' | ';' => {
                end_word(&mut current, &mut word, &mut in_word);
                end_command(&mut commands, &mut current);
                None
            }
            '&' if chars.next_if_eq(&'&').is_some() => {
                end_word(&mut current, &mut word, &mut in_word);
                end_command(&mut commands, &mut current);
                None
            }
            '&' => Some("&"),
            '|' => Some("a pipe, |"),
            '<' | '>' => Some("a redirect"),
            '(' | ')' => Some("parentheses"),
            '#' if !in_word => {
                while chars.next_if(|&n| n != '\n').is_some() {}
                None
            }
            '$' if !in_word && current.words.is_empty() && chars.peek() == Some(&' ') => {
                // A prompt copied with the command.
                None
            }
            '\\' | '`' | '^' if continues_line(&mut chars) => None,
            '`' => Some("a substitution, `"),
            '\\' => {
                in_word = true;
                match chars.next_if(|n| " \t\"'\\$`&|;<>()#".contains(*n)) {
                    Some(n) => word.push(n),
                    None => word.push('\\'),
                }
                None
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break None,
                        Some(n) => word.push(n),
                        None => break Some("a quote that is not closed"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break None,
                        Some('\\') if continues_line(&mut chars) => {}
                        Some('\\') => match chars.next_if(|n| "\"\\$`".contains(*n)) {
                            Some(n) => word.push(n),
                            None => word.push('\\'),
                        },
                        Some('`') => break Some("a substitution, `"),
                        Some('$') => {
                            if let Err(what) = variable(&mut chars, &mut word, &mut variables) {
                                break Some(what);
                            }
                        }
                        Some(n) => word.push(n),
                        None => break Some("a quote that is not closed"),
                    }
                }
            }
            '$' => {
                in_word = true;
                variable(&mut chars, &mut word, &mut variables).err()
            }
            _ => {
                in_word = true;
                word.push(c);
                None
            }
        };
        if let Some(what) = shell {
            current.shell.get_or_insert(what);
            while chars.next_if(|&n| n != '\n').is_some() {}
            end_word(&mut current, &mut word, &mut in_word);
            // Kept even without words, so a line that is only shell is skipped with a note, not silently.
            if current.words.is_empty() {
                current.words.push(String::new());
            }
            end_command(&mut commands, &mut current);
        }
    }
    end_word(&mut current, &mut word, &mut in_word);
    end_command(&mut commands, &mut current);
    Lexed {
        commands,
        variables,
    }
}

/// After `\`, `` ` `` or `^`: is the rest of the line a line break (the command goes on)? Consumes it if so.
fn continues_line(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut ahead = chars.clone();
    ahead.next_if_eq(&'\r');
    if ahead.next_if_eq(&'\n').is_some() {
        *chars = ahead;
        return true;
    }
    false
}

/// After a `$`: `$NAME` and `${NAME}` become `${NAME}`, which Claude Code fills in when it starts the server; a
/// lone `$` stays. `$(…)` would run a command: the error says so.
fn variable(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut String,
    variables: &mut Vec<String>,
) -> Result<(), &'static str> {
    let mut name = String::new();
    match chars.peek() {
        Some('(') => return Err("a substitution, $(…)"),
        Some('{') => {
            chars.next();
            let mut inner = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some('\n') | None => return Err("a ${ that is not closed"),
                    Some(c) => inner.push(c),
                }
            }
            name = inner
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            word.push_str(&format!("${{{inner}}}"));
        }
        Some(&c) if c.is_ascii_alphabetic() || c == '_' => {
            while let Some(c) = chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '_') {
                name.push(c);
            }
            word.push_str(&format!("${{{name}}}"));
        }
        _ => {
            word.push('$');
            return Ok(());
        }
    }
    if !name.is_empty() && !variables.contains(&name) {
        variables.push(name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn one(text: &str) -> PastedServer {
        let pasted = Pasted::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(pasted.servers.len(), 1, "{pasted:?}");
        pasted.servers.into_iter().next().unwrap()
    }

    fn json_of(server: &PastedServer) -> Value {
        serde_json::from_str(&server.json).unwrap_or_else(|e| panic!("{server:?}: {e}"))
    }

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }

    #[test]
    fn a_claude_mcp_add_command_becomes_the_server_claude_would_add() {
        let s = one(
            "claude mcp add --transport stdio --env AIRTABLE_API_KEY=YOUR_KEY airtable -- npx -y airtable-mcp-server",
        );
        assert_eq!(s.name, "airtable");
        // Claude Code's own default.
        assert_eq!(s.scope, Some(Scope::Local));
        let v = json_of(&s);
        assert_eq!(
            v,
            json!({"type": "stdio", "command": "npx", "args": ["-y", "airtable-mcp-server"], "env": {"AIRTABLE_API_KEY": "YOUR_KEY"}})
        );
        // In the order Claude Code writes it.
        assert_eq!(keys(&v), ["type", "command", "args", "env"]);
        assert_eq!(s.problem, None);
        assert_eq!(s.summary, "npx -y airtable-mcp-server");
        let s = one("claude mcp add tiny node");
        assert_eq!(
            json_of(&s),
            json!({"type": "stdio", "command": "node", "args": [], "env": {}})
        );
    }

    #[test]
    fn options_go_anywhere_before_the_double_dash_in_every_spelling() {
        let s = one(
            "claude mcp add -s user --env=A=1 -e B=2 C=3 my-srv --scope=user -- node server.js --port 3000 -e x",
        );
        assert_eq!(s.name, "my-srv");
        assert_eq!(s.scope, Some(Scope::User));
        let v = json_of(&s);
        assert_eq!(v["env"], json!({"A": "1", "B": "2", "C": "3"}));
        assert_eq!(v["command"], "node");
        assert_eq!(v["args"], json!(["server.js", "--port", "3000", "-e", "x"]));
        // Without a double dash, the program's own options after it are its arguments.
        let v = json_of(&one("claude mcp add -s local fs npx -y @x/fs /tmp"));
        assert_eq!(v["args"], json!(["-y", "@x/fs", "/tmp"]));
        assert_eq!(
            one("claude mcp add -s local fs npx").scope,
            Some(Scope::Local)
        );
        assert_eq!(one("claude.exe mcp add fs npx").name, "fs");
        assert_eq!(one("/usr/local/bin/claude mcp add fs npx").name, "fs");
    }

    #[test]
    fn an_address_is_a_remote_server() {
        let s = one("claude mcp add --transport http notion https://mcp.notion.com/mcp");
        let v = json_of(&s);
        assert_eq!(
            v,
            json!({"type": "http", "url": "https://mcp.notion.com/mcp"})
        );
        assert_eq!(s.summary, "http · https://mcp.notion.com/mcp");
        let v = json_of(&one(
            r#"claude mcp add -t sse -H "X-Api-Key: abc" -H "X-Other: y" "X-Third:z" s https://x.example/sse"#,
        ));
        assert_eq!(keys(&v), ["type", "url", "headers"]);
        assert_eq!(
            v["headers"],
            json!({"X-Api-Key": "abc", "X-Other": "y", "X-Third": "z"})
        );
        assert_eq!(v["url"], "https://x.example/sse");
        // Claude Code would take an address without --transport for a program; it is meant as http.
        let pasted = Pasted::parse("claude mcp add gw https://gw.example/mcp").unwrap();
        assert_eq!(json_of(&pasted.servers[0])["type"], "http");
        assert!(
            pasted.notes.iter().any(|n| n.contains("--transport http")),
            "{pasted:?}"
        );
    }

    #[test]
    fn quotes_escapes_and_line_continuations_read_like_a_shell() {
        let v = json_of(&one(
            "claude mcp add q \\\n  -e 'A=two words' \\\r\n  -e \"B=say \\\"hi\\\"\" -- node 'C:\\tools\\s.js' C:\\tools\\t.js a\\ b",
        ));
        assert_eq!(v["env"], json!({"A": "two words", "B": "say \"hi\""}));
        assert_eq!(
            v["args"],
            json!(["C:\\tools\\s.js", "C:\\tools\\t.js", "a b"])
        );
        // PowerShell and cmd continue lines with ` and ^.
        let v = json_of(&one("claude mcp add p `\n  -- npx ^\n  -y pkg"));
        assert_eq!(v["args"], json!(["-y", "pkg"]));
        // A prompt copied with the command is left out; an empty quoted argument is kept.
        let v = json_of(&one("$ claude mcp add e -- node ''"));
        assert_eq!(v["args"], json!([""]));
    }

    #[test]
    fn shell_variables_become_references_claude_code_fills_in() {
        let pasted = Pasted::parse(
            r#"claude mcp add gh -e TOKEN=$GITHUB_TOKEN -e "B=${OTHER}" -e 'C=$LITERAL' -e D=$ -- node $HOME/x.js"#,
        )
        .unwrap();
        let v = json_of(&pasted.servers[0]);
        assert_eq!(
            v["env"],
            json!({"TOKEN": "${GITHUB_TOKEN}", "B": "${OTHER}", "C": "$LITERAL", "D": "$"})
        );
        assert_eq!(v["args"], json!(["${HOME}/x.js"]));
        let note = pasted.notes.join(" ");
        assert!(note.contains("GITHUB_TOKEN"), "{note}");
        assert!(note.contains("HOME"), "{note}");
    }

    #[test]
    fn several_commands_are_several_servers() {
        let pasted = Pasted::parse(
            "# Install the servers\nnpm install -g thing\nclaude mcp add a -- node a.js\n\nclaude mcp add b -- node b.js && claude mcp add c node; claude mcp add d node # trailing comment\n",
        )
        .unwrap();
        let names: Vec<&str> = pasted.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d"]);
        assert!(
            pasted
                .notes
                .iter()
                .any(|n| n.contains("not a claude mcp add command") && n.contains("npm install")),
            "{pasted:?}"
        );
        assert_eq!(json_of(&pasted.servers[3])["args"], json!([]));
    }

    #[test]
    fn add_json_is_read_too() {
        let s = one(
            r#"claude mcp add-json -s user weather '{"type":"stdio","command":"/path/weather","args":["--api-key","abc"],"env":{"CACHE":"1"}}'"#,
        );
        assert_eq!(s.name, "weather");
        assert_eq!(s.scope, Some(Scope::User));
        let v = json_of(&s);
        assert_eq!(keys(&v), ["type", "command", "args", "env"]);
        assert_eq!(v["args"], json!(["--api-key", "abc"]));
        let s = one("claude mcp add-json w 'not json'");
        assert!(s.problem.unwrap().contains("JSON"));
    }

    #[test]
    fn json_is_read_in_every_usual_shape() {
        let block = r#"{"mcpServers": {"fs": {"command": "npx", "args": ["-y", "@x/fs"], "env": {}}, "web": {"type": "http", "url": "https://w.example/mcp"}}}"#;
        let pasted = Pasted::parse(block).unwrap();
        let names: Vec<&str> = pasted.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fs", "web"]);
        // Kept as written: no type is added, keys stay in order.
        let fs = json_of(&pasted.servers[0]);
        assert_eq!(keys(&fs), ["command", "args", "env"]);
        assert_eq!(pasted.servers[1].summary, "http · https://w.example/mcp");
        // VS Code's file says "servers".
        let pasted =
            Pasted::parse(r#"{"inputs": [], "servers": {"v": {"type": "stdio", "command": "x"}}}"#)
                .unwrap();
        assert_eq!(pasted.servers[0].name, "v");
        // A map of names, and a fragment of one copied from a larger file (trailing comma and all).
        assert_eq!(one(r#"{"m": {"command": "x"}}"#).name, "m");
        assert_eq!(one("\n  \"frag\": {\"command\": \"x\"},\n").name, "frag");
        // One definition without a name.
        let s = one(r#"{"type": "http", "url": "https://x.example"}"#);
        assert_eq!(s.name, "");
        assert_eq!(s.scope, None);
        assert_eq!(s.problem, None);
        // A whole ~/.claude.json: only its servers are read, and the note says so.
        let pasted =
            Pasted::parse(r#"{"numStartups": 3, "mcpServers": {"u": {"command": "x"}}}"#).unwrap();
        assert_eq!(pasted.servers[0].name, "u");
        assert!(!pasted.notes.is_empty());
    }

    #[test]
    fn git_bash_turning_slash_c_into_a_drive_is_undone() {
        let pasted = Pasted::parse("claude mcp add w -- cmd C:/ npx -y pkg").unwrap();
        assert_eq!(
            json_of(&pasted.servers[0])["args"],
            json!(["/c", "npx", "-y", "pkg"])
        );
        assert!(pasted.notes.iter().any(|n| n.contains("/c")), "{pasted:?}");
        let pasted =
            Pasted::parse(r#"{"w": {"command": "CMD.EXE", "args": ["C:/", "npx"]}}"#).unwrap();
        assert_eq!(json_of(&pasted.servers[0])["args"], json!(["/c", "npx"]));
        // Only right after cmd: a drive anywhere else is a drive.
        let v = json_of(&one("claude mcp add w -- node C:/"));
        assert_eq!(v["args"], json!(["C:/"]));
    }

    #[test]
    fn a_server_that_signs_in_with_oauth_is_read_to_be_finished_in_the_form() {
        let s = one(
            "claude mcp add --transport http --client-id abc123 --client-secret --callback-port 8080 corp https://corp.example/mcp",
        );
        assert_eq!(s.problem, None, "{s:?}");
        assert!(s.oauth && s.asks_secret);
        assert_eq!(
            json_of(&s),
            json!({"type": "http", "url": "https://corp.example/mcp", "oauth": {"clientId": "abc123", "callbackPort": 8080}})
        );
        let s = one(
            r#"{"o": {"type": "http", "url": "https://o.example", "oauth": {"clientId": "x"}}}"#,
        );
        assert!(s.oauth && s.problem.is_none(), "{s:?}");
        let s = one(
            r#"claude mcp add-json --client-secret -s user o '{"type":"sse","url":"https://o.example"}'"#,
        );
        assert_eq!(json_of(&s)["oauth"], json!({}));
        assert!(s.oauth && s.asks_secret && s.problem.is_none(), "{s:?}");
        assert!(!one("claude mcp add -s user plain node").oauth);
        // Without --client-secret nothing is asked for: it can be added as pasted.
        let s = one("claude mcp add -t http --client-id abc x https://x.example/mcp");
        assert!(s.oauth && !s.asks_secret && s.problem.is_none(), "{s:?}");
        // A secret typed after --client-secret (it takes none) is never shown, not even as the name: also when the
        // name is left out, so the secret would be taken for it, or with the secret after `=`.
        for text in [
            "claude mcp add -t http --client-secret Sup3rSecret corp https://corp.example/mcp",
            r#"claude mcp add-json --client-secret Sup3rSecret o '{"type":"http","url":"https://o.example"}'"#,
            "claude mcp add --transport http --client-secret Sup3rSecret https://mcp.linear.app/mcp",
            "claude mcp add-json --client-secret Sup3rSecret sso",
            "claude mcp add-json --client-secret Sup3rSecret",
            "claude mcp add -t http corp --client-secret Sup3rSecret https://corp.example/mcp",
            "claude mcp add -t http corp https://corp.example/mcp --client-secret Sup3rSecret",
            "claude mcp add -t http --client-secret=Sup3rSecret corp https://corp.example/mcp",
            r#"claude mcp add-json --client-secret=Sup3rSecret o '{"type":"http","url":"https://o.example"}'"#,
            r#"claude mcp add-json o --client-secret Sup3rSecret '{"type":"http","url":"https://o.example"}'"#,
        ] {
            let s = one(text);
            assert!(
                s.problem.as_deref().unwrap().contains("takes no value"),
                "{s:?}"
            );
            assert!(!format!("{s:?}").contains("Sup3rSecret"), "{s:?}");
        }
        // The option itself, before the address or the definition, is fine.
        for text in [
            "claude mcp add -t http corp --client-secret https://corp.example/mcp",
            r#"claude mcp add-json o --client-secret '{"type":"http","url":"https://o.example"}'"#,
        ] {
            let s = one(text);
            assert!(s.oauth && s.problem.is_none(), "{text}: {s:?}");
            assert_eq!(s.name, if text.contains("corp") { "corp" } else { "o" });
        }
        for (text, why) in [
            ("claude mcp add --client-id x corp node", "address"),
            (
                "claude mcp add -t http --callback-port 0 corp https://c.example",
                "port",
            ),
            (
                r#"{"o": {"type": "http", "url": "https://o.example", "oauth": {"clientSecret": "inline"}}}"#,
                "on its own",
            ),
        ] {
            let s = one(text);
            let problem = s.problem.unwrap_or_default();
            assert!(problem.contains(why), "{text}: {problem}");
        }
    }

    #[test]
    fn what_klefki_cannot_add_is_explained_not_dropped() {
        let pasted = Pasted::parse("claude mcp add -s project p node").unwrap();
        assert_eq!(pasted.servers[0].scope, None);
        assert!(
            pasted.notes.iter().any(|n| n.contains(".mcp.json")),
            "{pasted:?}"
        );
        for (text, why) in [
            ("claude mcp add --frobnicate x node", "--frobnicate"),
            ("claude mcp add -t carrier-pigeon x node", "carrier-pigeon"),
            ("claude mcp add x", "what to run"),
            ("claude mcp add", "name"),
            ("claude mcp add -t http x https://a.example extra", "extra"),
            ("claude mcp add -t http x ftp://a.example", "url"),
            ("claude mcp add -e NOEQUALS x node", "NOEQUALS"),
            ("claude mcp add -H nocolon x node", "nocolon"),
        ] {
            let s = one(text);
            let problem = s.problem.unwrap_or_default();
            assert!(problem.contains(why), "{text}: {problem}");
        }
    }

    #[test]
    fn text_that_holds_no_server_is_refused_saying_why() {
        for (text, why) in [
            ("", "Paste"),
            ("   \n  ", "Paste"),
            ("npm install thing", "claude mcp add"),
            ("claude mcp list", "claude mcp add"),
            ("curl -fsSL https://x.example/i.sh | sh", "claude mcp add"),
            ("{\"a\": ", "JSON"),
            ("[1, 2]", "JSON"),
            ("{\"a\": 1}", "server"),
        ] {
            let err = Pasted::parse(text).unwrap_err();
            assert!(err.contains(why), "{text:?}: {err}");
        }
        let big = format!("claude mcp add x -- node {}", "a".repeat(300 * 1024));
        assert!(Pasted::parse(&big).unwrap_err().contains("KiB"));
        let many: String = (0..150)
            .map(|i| format!("claude mcp add s{i} node\n"))
            .collect();
        assert!(Pasted::parse(&many).unwrap_err().contains("100"));
    }

    #[test]
    fn a_command_that_needs_a_shell_is_explained_and_the_other_lines_still_read() {
        for (text, why) in [
            ("claude mcp add x -- node a.js | tee log", "|"),
            ("claude mcp add x -- node $(cat f)", "$("),
            ("claude mcp add x -- node `cat f`", "`"),
            ("claude mcp add x -- node \"`cat f`\"", "`"),
            ("claude mcp add x -- node > log", "redirect"),
            ("claude mcp add x -- node &", "&"),
            ("claude mcp add 'x -- node", "quote"),
            ("claude mcp add x -- node \"${A", "${"),
        ] {
            let s = one(text);
            assert!(s.json.is_empty(), "{text}: {s:?}");
            let problem = s.problem.unwrap_or_default();
            assert!(problem.contains(why), "{text}: {problem}");
        }
        // A README's install line before the add command does not stop the add command being read.
        let pasted = Pasted::parse(
            "curl -fsSL https://x.example/install.sh | sh\nclaude mcp add fs npx -y pkg\nopen 'https://a.example/?a=1&b=2' & echo",
        )
        .unwrap();
        assert_eq!(pasted.servers.len(), 1, "{pasted:?}");
        assert_eq!(pasted.servers[0].name, "fs");
        assert_eq!(pasted.servers[0].problem, None);
        assert_eq!(
            pasted
                .notes
                .iter()
                .filter(|n| n.starts_with("Skipped"))
                .count(),
            2,
            "{pasted:?}"
        );
    }

    #[test]
    fn a_quoted_program_path_is_a_command_not_json() {
        let s = one(r#""C:\Program Files\Claude\claude.exe" mcp add fs npx -y pkg"#);
        assert_eq!(s.name, "fs");
        assert_eq!(json_of(&s)["args"], json!(["-y", "pkg"]));
        assert_eq!(one(r#""claude" mcp add fs npx"#).name, "fs");
        // A fragment still is JSON, escaped quotes and all.
        assert_eq!(one(r#""a\"b": {"command": "x"}"#).name, "a\"b");
    }

    #[test]
    fn an_address_without_a_type_is_added_as_http() {
        let pasted =
            Pasted::parse(r#"{"mcpServers": {"n": {"url": "https://mcp.example/mcp"}}}"#).unwrap();
        let s = &pasted.servers[0];
        assert_eq!(s.problem, None, "{s:?}");
        let v = json_of(s);
        assert_eq!(v, json!({"type": "http", "url": "https://mcp.example/mcp"}));
        assert_eq!(keys(&v), ["type", "url"]);
        assert!(
            pasted.notes.iter().any(|n| n.contains("no type")),
            "{pasted:?}"
        );
        // One with a type, or a command, is kept as written.
        let v = json_of(&one(
            r#"{"s": {"type": "sse", "url": "https://s.example"}}"#,
        ));
        assert_eq!(v["type"], "sse");
        let s = one(r#"{"c": {"command": "x", "url": "https://c.example"}}"#);
        assert!(s.problem.is_some(), "{s:?}");
    }

    #[test]
    fn a_home_folder_tilde_is_pointed_out() {
        let s = one("claude mcp add fs npx -y @x/fs ~/Documents ~ a~b");
        assert_eq!(s.hints.len(), 2, "{s:?}");
        assert!(s.hints[0].contains("Argument 3"), "{s:?}");
        assert!(s.hints[1].contains("Argument 4"), "{s:?}");
        assert!(one(r#"{"t": {"command": "~/bin/server"}}"#).hints[0].contains("command"));
    }

    #[test]
    fn a_quoted_line_continuation_works_with_windows_line_ends() {
        let v = json_of(&one("claude mcp add q -- node \"a\\\r\nb\""));
        assert_eq!(v["args"], json!(["ab"]));
    }

    #[test]
    fn secret_looking_values_get_a_hint_without_the_value() {
        let s = one(
            "claude mcp add -t http -H 'Authorization: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789' gh https://api.example/mcp",
        );
        assert_eq!(s.hints.len(), 1, "{s:?}");
        assert!(s.hints[0].contains("Authorization"), "{s:?}");
        assert!(s.hints[0].contains("${"), "{s:?}");
        assert!(!s.hints[0].contains("ghp_"), "{s:?}");
        let s = one(
            r#"{"g": {"command": "x", "env": {"GITHUB_TOKEN": "ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}}"#,
        );
        assert!(s.hints[0].contains("GITHUB_TOKEN"), "{s:?}");
        assert!(one("claude mcp add -e A=${TOKEN} x node").hints.is_empty());
    }

    #[test]
    fn what_is_shown_is_sanitised() {
        let pasted = Pasted::parse(
            "{\"a\\u202eb\": {\"command\": \"x\\u001b[31m\", \"args\": [\"has space\"]}}",
        )
        .unwrap();
        let s = &pasted.servers[0];
        assert_eq!(s.name, "ab");
        assert!(!s.summary.contains('\u{1b}'), "{s:?}");
        assert!(s.summary.contains("\"has space\""), "{s:?}");
        let s = one(&format!("claude mcp add x -- node {}", "y".repeat(5000)));
        assert!(s.summary.chars().count() < 400, "{}", s.summary.len());
    }

    #[test]
    fn any_text_is_read_without_panicking() {
        // A small fixed generator (no new dependency): the characters the reader treats specially, and others.
        const ALPHABET: &[&str] = &[
            "claude",
            " mcp",
            " add",
            " add-json",
            " -e",
            " -H",
            " -s",
            " -t",
            " --",
            " ",
            "\n",
            "\r",
            "\\",
            "'",
            "\"",
            "`",
            "^",
            "$",
            "${",
            "}",
            "(",
            "&",
            "&&",
            ";",
            "|",
            "#",
            "=",
            ":",
            "{",
            "}",
            "[",
            "]",
            ",",
            "x",
            "C:/",
            "cmd",
            "https://a.example",
            "\u{202e}",
            "\u{1b}",
            "é",
            "\"mcpServers\":",
            "\"command\":",
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut read = 0;
        for _ in 0..20_000 {
            let mut text = String::from(if next() % 2 == 0 {
                "claude mcp add"
            } else {
                ""
            });
            for _ in 0..(next() % 24) {
                text.push_str(ALPHABET[(next() % ALPHABET.len() as u64) as usize]);
            }
            if let Ok(pasted) = Pasted::parse(&text) {
                read += 1;
                for s in &pasted.servers {
                    if !s.json.is_empty() {
                        serde_json::from_str::<Value>(&s.json).unwrap();
                    }
                }
            }
        }
        // The generator reaches past the first refusals.
        assert!(read > 1000, "{read}");
    }
}
