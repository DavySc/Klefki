//! Reads servers out of pasted text: `claude mcp add …` and `claude mcp add-json …` commands (one per line), or JSON
//! (`{"mcpServers": {…}}`, a map of names to definitions, a `"name": {…}` fragment, or one definition). Nothing
//! pasted is ever run: the text is only read, and what it would add goes through the same review as every other
//! change.
use crate::api::Scope;
use crate::config::entry::MAX_ENTRY_BYTES;
use crate::config::secretlike::shown_args;
use crate::sanitize::{clip, sanitize, sanitize_line};
use serde_json::{Map, Value};
use std::collections::HashSet;

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
    /// The word after `--client-secret` was left out, as it may be the secret, and the name with it: the dialog says
    /// so where it offers the form.
    pub word_left_out: bool,
}

/// Pasted text larger than this is refused: a few servers are a few KiB.
const MAX_PASTE_BYTES: usize = 256 * 1024;
const MAX_SERVERS: usize = 100;
/// A summary longer than this is cut; the review shows everything in full.
const MAX_SUMMARY_CHARS: usize = 300;
/// Skipped lines named one by one; any more are counted in one note.
const MAX_SKIPPED_SHOWN: usize = 5;
/// Variable names listed in a note; any more are counted.
const MAX_NAMES_SHOWN: usize = 8;

/// How many of `total` items to name when `max` fit: a count never stands in for a single item, which would take its
/// room.
fn named(total: usize, max: usize) -> usize {
    if total > max + 1 { max } else { total }
}
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
                "That holds more than {MAX_SERVERS} servers; Klefki adds at most {MAX_SERVERS} at a time."
            ));
        }
        let mut seen = HashSet::new();
        pasted.notes.retain(|n| seen.insert(n.clone()));
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
            word_left_out: false,
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
    let lexed = lex_with(text, true);
    let mut notes = Vec::new();
    let mut servers = Vec::new();
    // Each skipped line that names claude once, in order: most likely a mistyped command. Other text is counted.
    let mut skipped: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut other = 0;
    for command in &lexed.commands {
        if servers.len() > MAX_SERVERS {
            // Refused as too many: the rest need not be read.
            break;
        }
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
            (Some((verb, rest)), None) => {
                let mut server = if verb == "add" {
                    add(rest, &mut notes)
                } else {
                    add_json(rest, &mut notes)
                };
                if let Some((what, at)) = command.typography {
                    // First: what it causes (an odd name, a stray word) is not the problem.
                    let word = command.words.get(at).map_or("", String::as_str);
                    server.problem = Some(what.problem(word));
                }
                servers.push(server);
            }
            (None, _) if names_claude(&command.words) => {
                let line = clip(&shown_text(&command.words.join(" ")), 60);
                if seen.insert(line.clone()) {
                    skipped.push(line);
                }
            }
            (None, _) => other += 1,
        }
    }
    let shown = named(skipped.len(), MAX_SKIPPED_SHOWN);
    for line in &skipped[..shown] {
        notes.push(format!(
            "Skipped a line that is not a claude mcp add command: {line}"
        ));
    }
    if skipped.len() > shown {
        notes.push(format!(
            "Skipped {} more lines that are not claude mcp add commands.",
            skipped.len() - shown
        ));
    }
    match other {
        0 => {}
        1 => notes.push("Skipped a line of other text.".to_owned()),
        n => notes.push(format!("Skipped {n} lines of other text.")),
    }
    if servers.is_empty() {
        // A line that names claude is most likely the command meant, mistyped: quoted, it says why nothing was added.
        let meant = lexed
            .commands
            .iter()
            .map(|c| c.words.join(" "))
            .find(|line| line.split_whitespace().any(is_claude));
        return Err(match meant {
            Some(line) => format!(
                "Nothing here adds a server: \"{}\" does not start with claude mcp add.",
                clip(&shown_text(&line), 60)
            ),
            None => {
                "Nothing here adds a server. Paste a claude mcp add command or an mcpServers block."
                    .to_owned()
            }
        });
    }
    if !lexed.variables.is_empty() {
        notes.push(format!(
            "Your shell would fill in {} when the command runs. Claude Code fills {} in instead, from its own environment, when it starts the server.",
            listed(&lexed.variables, |v| format!("${v}")),
            if lexed.variables.len() == 1 { "it" } else { "them" }
        ));
    }
    Ok(Pasted { servers, notes })
}

/// `names`, each written by `show`, for a note: a long list names the first few and counts the rest.
pub(crate) fn listed(names: &[String], show: impl Fn(&str) -> String) -> String {
    let shown = named(names.len(), MAX_NAMES_SHOWN);
    let mut text = names[..shown]
        .iter()
        .map(|n| show(n))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > shown {
        text.push_str(&format!(" and {} more", names.len() - shown));
    }
    text
}

/// Whether a skipped command starts with claude, or has `claude mcp` in it (`sudo claude mcp add …`): most likely a
/// command that was meant, so it is named. A guide's sentences ("Then run this in Claude Code:") are not.
fn names_claude(words: &[String]) -> bool {
    let mut words = words.iter().flat_map(|w| w.split_whitespace());
    let Some(mut previous) = words.next() else {
        return false;
    };
    if is_claude(previous) {
        return true;
    }
    words.any(|word| {
        let found = word == "mcp" && is_claude(previous);
        previous = word;
        found
    })
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
    // A word that may be the secret was left out: nothing but the address is used of the rest.
    let mut hidden = false;
    let mut left_out = false;
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
        let takes_value = matches!(
            option,
            "-s" | "--scope"
                | "-t"
                | "--transport"
                | "-e"
                | "--env"
                | "-H"
                | "--header"
                | "--client-id"
                | "--callback-port"
        );
        if takes_value && inline.is_none() && i >= words.len() {
            // Claude Code stops here too.
            problem.get_or_insert(format!("{option} needs a value after it."));
            continue;
        }
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
                                shown_text(v)
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
                                shown_text(v)
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
                if inline.is_some() {
                    hidden = true;
                } else if words.get(i).is_some_and(|w| may_be_secret(w, "://")) {
                    hidden = true;
                    i += 1;
                }
                oauth.get_or_insert_default();
                asks_secret = true;
            }
            _ => {
                // Without what follows `=`, which may be a value.
                let option = option.split_once('=').map_or(option, |(o, _)| o);
                // Claude Code would take `-H=value` as the value `=value`.
                let example = match option {
                    "-s" => Some("-s user"),
                    "-t" => Some("-t http"),
                    "-e" => Some("-e KEY=value"),
                    "-H" => Some("-H \"Name: value\""),
                    _ => None,
                };
                problem.get_or_insert(match example {
                    Some(example) => {
                        format!("{option} takes its value after a space, as in {example}.")
                    }
                    None => format!(
                        "Claude Code's mcp add has no option {}.",
                        shown_text(option)
                    ),
                });
            }
        }
    }
    if hidden {
        // The name may be the word left out, or the secret itself: only the address is kept.
        let Some(url) = positional.iter().copied().find(|w| w.contains("://")) else {
            return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
        };
        left_out = true;
        positional = vec!["", url];
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
                        if name.is_empty() { "It".to_owned() } else { sanitize(name) }
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
    server.word_left_out = left_out && server.asks_secret;
    server
}

/// `claude mcp add-json [-s scope] <name> <json>`.
fn add_json(words: &[&str], notes: &mut Vec<String>) -> PastedServer {
    let mut scope = None;
    let mut positional = Vec::new();
    let mut asks_secret = false;
    let mut hidden = false;
    let mut left_out = false;
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        i += 1;
        match word.split_once('=') {
            Some(("--scope", v)) => scope = Some(v),
            _ if word == "-s" || word == "--scope" => {
                let Some(value) = words.get(i).copied() else {
                    return PastedServer::refused(
                        positional.first().copied().unwrap_or_default(),
                        format!("{word} needs a value after it."),
                    );
                };
                scope = Some(value);
                i += 1;
            }
            Some(("--client-secret", _)) => {
                hidden = true;
                asks_secret = true;
            }
            _ if word == "--client-secret" => {
                if words.get(i).is_some_and(|w| may_be_secret(w, "{")) {
                    hidden = true;
                    i += 1;
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
    if hidden {
        // The name may be the word left out, or the secret itself: only the definition is kept.
        let Some(json) = positional
            .iter()
            .copied()
            .find(|w| w.trim_start().starts_with('{'))
        else {
            return PastedServer::refused("", SECRET_AFTER_OPTION.to_owned());
        };
        left_out = true;
        positional = vec!["", json];
    }
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
    server.word_left_out = left_out && server.asks_secret;
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

/// Git Bash rewrites `/c` to `C:/` on the way to a Windows program, so `cmd /c npx …` arrives as `cmd C:/ npx …`,
/// and `cmd /s /c` as `cmd S:/ C:/`: every such switch right after cmd is put back.
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
    let Some(Value::Array(args)) = entry.get_mut("args") else {
        return;
    };
    let (mut drives, mut switches) = (Vec::new(), Vec::new());
    for arg in args {
        let Some(s) = arg.as_str() else { break };
        if !(s.len() == 3 && s.as_bytes()[0].is_ascii_alphabetic() && &s[1..] == ":/") {
            break;
        }
        let switch = format!("/{}", s[..1].to_ascii_lowercase());
        drives.push(s.to_owned());
        switches.push(switch.clone());
        *arg = Value::String(switch);
    }
    // One note for them all.
    match (drives.as_slice(), switches.as_slice()) {
        ([], _) => {}
        ([drive], [switch]) => notes.push(format!(
            "Read {drive} after cmd as {switch}: Git Bash turns {switch} into a drive on its way to a Windows program."
        )),
        _ => notes.push(format!(
            "Read {} after cmd as {}: Git Bash turns these switches into drives on their way to a Windows program.",
            drives.join(" "),
            switches.join(" ")
        )),
    }
}

/// What a definition runs or connects to, on one line, masked as the list of servers masks it.
fn summary(entry: &Value) -> String {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default();
    let line = match text("type") {
        "" | "stdio" => {
            let args = match entry.get("args") {
                Some(Value::Array(args)) => {
                    args.iter().map(|a| a.as_str().unwrap_or("?")).collect()
                }
                _ => Vec::new(),
            };
            // Each word cleaned on its own: an escape left open in one must not swallow the next.
            std::iter::once(sanitize_line(text("command")))
                .chain(shown_args(args).iter().map(|a| sanitize_line(a)))
                .map(|w| quoted(&w))
                .collect::<Vec<_>>()
                .join(" ")
        }
        kind => {
            let url = text("url");
            let shown = crate::config::inventory::display_url(url, &["http", "https", "ws", "wss"])
                .unwrap_or_else(|_| shown_text(url));
            format!("{} \u{b7} {shown}", sanitize_line(kind))
        }
    };
    clip(&line, MAX_SUMMARY_CHARS)
}

/// Text quoted back in a message: on one line, each word that looks secret shown as `…`.
fn shown_text(text: &str) -> String {
    sanitize_line(&shown_args(text.split_whitespace()).join(" "))
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
    /// What a web page or word processor put in place of a plain character, and the word it is in: a curly quote
    /// outside straight quotes (a single one only where a quote would start), or a long dash starting a word. Kept
    /// as written; the reason it may not do what it says.
    pub typography: Option<(Typography, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Typography {
    CurlyQuote,
    LongDash,
}

impl Typography {
    /// The problem, naming `word` as the list of servers would show it.
    fn problem(self, word: &str) -> String {
        let word = clip(&shown_text(word), 40);
        match self {
            Self::CurlyQuote => format!(
                "The word {word} has a curly quote (\u{201c} \u{201d} \u{2018} or \u{2019}), which a web page puts in place of a straight quote, so a shell would not read it as a quote. Type straight quotes (\" or ') instead."
            ),
            Self::LongDash => format!(
                "The word {word} starts with a long dash (\u{2013} or \u{2014}), which a web page puts in place of two hyphens. Type it again as --."
            ),
        }
    }
}

/// Splits pasted shell text into commands and words the way a POSIX shell would, plus the line continuations of
/// PowerShell (`` ` ``) and cmd (`^`). A backslash only escapes what a shell would need escaping, so a Windows path
/// keeps its backslashes. A command that needs a shell to run (pipes, redirects, substitutions) is marked, and the
/// rest of its line skipped; the other lines are still read. A non-breaking space separates words, as a web page
/// shows one where a space was meant.
pub(crate) fn lex(text: &str) -> Lexed {
    lex_with(text, false)
}

/// [`lex`], and with `pasted`, a line that does not start with `claude` is kept whole as one word, unread: it is
/// prose or another program's command, and read, an apostrophe (`Don't`) would open a quote running to the end.
fn lex_with(text: &str, pasted: bool) -> Lexed {
    let mut commands: Vec<Words> = Vec::new();
    let mut variables: Vec<String> = Vec::new();
    let mut current = Words::default();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = Cursor { rest: text };
    // When pasting: where the line being read ends, found once per line (a line may hold many commands), and whether
    // no command has started on it yet.
    let mut line_end = 0;
    let mut fresh = true;
    // Where the Markdown inline code holding the command being read ends (`` `claude mcp add …` ``).
    let mut code_end: Option<usize> = None;
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
        if code_end.is_some_and(|end| end == text.len() - chars.rest.len() - c.len_utf8()) {
            // The command ends with its code; the prose after it on the line is not read.
            code_end = None;
            end_word(&mut current, &mut word, &mut in_word);
            end_command(&mut commands, &mut current);
            chars.rest = &text[line_end..];
            continue;
        }
        if pasted && !in_word && current.words.is_empty() && !c.is_whitespace() {
            // Slices of the text, never copies: a line is looked at once per command in it.
            let at = text.len() - chars.rest.len() - c.len_utf8();
            if at >= line_end {
                line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
            }
            let line = &text[at..line_end];
            if fresh {
                if let Some(skip) = marker_len(line) {
                    chars.rest = &text[at + skip..];
                    continue;
                }
                let shows = |s: &str| !sanitize_line(s).trim().is_empty();
                if is_fence(line) || !(shows(&line[..c.len_utf8()]) || shows(line)) {
                    chars.rest = &text[line_end..];
                    continue;
                }
                fresh = false;
            }
            if c == '`'
                && let Some(end) = inline_claude(&line[1..])
            {
                code_end = Some(at + 1 + end);
                continue;
            }
            if !"$#'\"\\`^".contains(c) && !is_claude(first_word(line)) {
                if let Some(start) = run_by_package(line) {
                    // `npx @anthropic-ai/claude-code mcp add …` is `claude mcp add …`.
                    current.words.push("claude".to_owned());
                    chars.rest = &text[at + start..];
                    continue;
                }
                // Up to a claude command chained after it (`cd app && claude mcp add …`), which is read.
                let (skipped, next) = match chained_claude(line) {
                    Some((end, start, Chained::Then)) => (&line[..end], at + start),
                    Some((_, start, Chained::Shell(what))) => {
                        // Read, so that it is refused saying why.
                        current.shell = Some(what);
                        chars.rest = &text[at + start..];
                        continue;
                    }
                    Some((_, start, Chained::Code(end))) => {
                        // The text around it explains it: not a line of its own.
                        code_end = Some(at + end);
                        chars.rest = &text[at + start..];
                        continue;
                    }
                    None => (line, line_end),
                };
                chars.rest = &text[next..];
                let skipped = skipped.trim_end();
                if !skipped.is_empty() {
                    current.words.push(skipped.to_owned());
                }
                end_command(&mut commands, &mut current);
                continue;
            }
        }
        let shell: Option<&'static str> = match c {
            ' ' | '\t' | '\r' | '\u{a0}' | '\u{202f}' => {
                end_word(&mut current, &mut word, &mut in_word);
                None
            }
            '\n' | ';' => {
                end_word(&mut current, &mut word, &mut in_word);
                end_command(&mut commands, &mut current);
                fresh |= c == '\n';
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
            '$' if !in_word && current.words.is_empty() && chars.peek() == Some(' ') => {
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
                // A single curly quote inside a word is an apostrophe (`O’Brien`).
                let found = match c {
                    '\u{201c}' | '\u{201d}' => Some(Typography::CurlyQuote),
                    '\u{2018}' | '\u{2019}' if !in_word || word.ends_with('=') => {
                        Some(Typography::CurlyQuote)
                    }
                    '\u{2013}' | '\u{2014}' if !in_word => Some(Typography::LongDash),
                    _ => None,
                };
                if let Some(found) = found {
                    current
                        .typography
                        .get_or_insert((found, current.words.len()));
                }
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
    let mut seen = HashSet::new();
    variables.retain(|v| seen.insert(v.clone()));
    Lexed {
        commands,
        variables,
    }
}

/// How many bytes at the start of a pasted line are a list or quote marker, or a PowerShell or cmd prompt, copied
/// with the command: `1. `, `2) `, `- `, `* `, `+ `, `> `, `PS C:\…> `, `C:\…> `.
fn marker_len(line: &str) -> Option<usize> {
    let then_space = |marker: &str| {
        let spaces = line[marker.len()..]
            .bytes()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
        (spaces > 0).then(|| marker.len() + spaces)
    };
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits) && matches!(line.as_bytes().get(digits), Some(b'.' | b')')) {
        return then_space(&line[..=digits]);
    }
    if let Some(marker) = ["-", "*", "+", ">"]
        .into_iter()
        .find(|m| line.starts_with(m))
    {
        return then_space(marker);
    }
    let bytes = line.as_bytes();
    let drive = bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\";
    if line.starts_with("PS ") || drive {
        let end = line.find('>')?;
        // `C:\tools\claude.exe … > log` is a command: cmd's prompt is one word.
        if drive && line[..end].contains(char::is_whitespace) {
            return None;
        }
        return then_space(&line[..=end]);
    }
    None
}

/// How a claude command follows other text on a line.
enum Chained {
    /// After `&&` or `;`: it runs on its own, so it is read.
    Then,
    /// After `||`, a pipe or `&`: what it does depends on the shell.
    Shell(&'static str),
    /// In Markdown inline code, which ends at this byte of the line.
    Code(usize),
}

/// Where a claude command after other text starts (`cd app && claude mcp add …`, ``Run `claude mcp add …` ``): the
/// end of the text before it, the start of the command, in bytes, and how it follows.
fn chained_claude(line: &str) -> Option<(usize, usize, Chained)> {
    line.char_indices().find_map(|(i, _)| {
        let rest = &line[i..];
        let (after, how) = if let Some(after) = rest.strip_prefix("&&") {
            (after, Chained::Then)
        } else if let Some(after) = rest.strip_prefix("||") {
            (after, Chained::Shell("||"))
        } else if let Some(after) = rest.strip_prefix(';') {
            (after, Chained::Then)
        } else if let Some(after) = rest.strip_prefix('|') {
            (after, Chained::Shell("a pipe, |"))
        } else if let Some(after) = rest.strip_prefix('&') {
            (after, Chained::Shell("&"))
        } else {
            let after = rest.strip_prefix('`')?;
            let end = inline_claude(after)?;
            (after, Chained::Code(line.len() - after.len() + end))
        };
        // A Markdown table row starts with a pipe, and pipes into nothing.
        if matches!(how, Chained::Shell(_)) && line[..i].trim().is_empty() {
            return None;
        }
        is_claude(first_word(after)).then(|| (i, line.len() - after.len(), how))
    })
}

/// Where the inline code ends, when `code` (the text after an opening backtick) is a claude command closed by a
/// backtick on the same line.
fn inline_claude(code: &str) -> Option<usize> {
    if !is_claude(first_word(code)) {
        return None;
    }
    code.find('`')
}

/// Where `mcp` starts when a package runner runs Claude Code: `npx [-y] @anthropic-ai/claude-code[@version] mcp …`,
/// also with `bunx`, `pnpm dlx` and `yarn dlx`.
fn run_by_package(line: &str) -> Option<usize> {
    const PACKAGE: &str = "@anthropic-ai/claude-code";
    let mut rest = ["npx", "bunx", "pnpx", "pnpm dlx", "yarn dlx"]
        .into_iter()
        .find_map(|runner| line.strip_prefix(runner))
        .filter(|rest| rest.starts_with([' ', '\t']))?;
    loop {
        let word = first_word(rest);
        if word.is_empty() {
            return None;
        }
        let after = &rest[rest.find(word)? + word.len()..];
        if word == "-y" || word == "--yes" {
            rest = after;
            continue;
        }
        let version = word.strip_prefix(PACKAGE)?;
        if !version.is_empty() && !version.starts_with('@') {
            return None;
        }
        return Some(line.len() - after.len());
    }
}

/// The first word of `text`: up to a space or the end of a command, so each word is looked at once.
fn first_word(text: &str) -> &str {
    let text = text.trim_start_matches([' ', '\t']);
    let end = text
        .find(|c: char| c.is_whitespace() || c == ';' || c == '&')
        .unwrap_or(text.len());
    &text[..end]
}

/// A Markdown code fence: three backticks or tildes, maybe with the language.
fn is_fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

/// The text still to read, a character at a time, which can be looked at as a slice.
#[derive(Clone)]
struct Cursor<'a> {
    rest: &'a str,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    fn next_if(&mut self, f: impl FnOnce(&char) -> bool) -> Option<char> {
        let c = self.peek().filter(f)?;
        self.rest = &self.rest[c.len_utf8()..];
        Some(c)
    }

    fn next_if_eq(&mut self, expected: &char) -> Option<char> {
        self.next_if(|c| c == expected)
    }
}

impl Iterator for Cursor<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        self.next_if(|_| true)
    }
}

/// After `\`, `` ` `` or `^`: is the rest of the line a line break (the command goes on)? Consumes it if so.
fn continues_line(chars: &mut Cursor<'_>) -> bool {
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
    chars: &mut Cursor<'_>,
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
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
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
    if !name.is_empty() {
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
    fn many_shell_variables_are_counted_not_all_listed() {
        // Found reviewing performance: every name was listed in one note, which the add dialog lays out every frame.
        let names: String = (0..40).map(|i| format!(" $V{i}")).collect();
        let pasted = Pasted::parse(&format!("claude mcp add s -- node{names}")).unwrap();
        let args = json_of(&pasted.servers[0])["args"].clone();
        assert_eq!(args.as_array().unwrap().len(), 40, "every argument is kept");
        assert_eq!(args[39], json!("${V39}"));
        let [note] = pasted.notes.as_slice() else {
            panic!("{pasted:?}")
        };
        assert!(
            note.contains("$V0, $V1,") && note.contains("and 32 more"),
            "{note}"
        );
        assert!(!note.contains("$V8,") && !note.contains("$V39"), "{note}");
        // A name used again is still one name.
        let pasted = Pasted::parse("claude mcp add s -- node $A $B $A ${B}").unwrap();
        assert!(
            pasted.notes[0].contains("$A, $B when"),
            "{:?}",
            pasted.notes
        );
    }

    #[test]
    fn many_skipped_lines_are_counted_not_all_listed() {
        // Found reviewing performance: one note per skipped line, so a long text pasted around a command gave
        // thousands of notes, each laid out every frame.
        let lines: String = (0..40).map(|i| format!("claude mcp get {i}\n")).collect();
        let prose: String = (0..40).map(|i| format!("not a command {i}\n")).collect();
        let pasted = Pasted::parse(&format!("claude mcp add s -- node\n{lines}{prose}")).unwrap();
        let skipped: Vec<&String> = pasted
            .notes
            .iter()
            .filter(|n| n.starts_with("Skipped"))
            .collect();
        assert_eq!(skipped.len(), 7, "{pasted:?}");
        assert!(skipped[0].ends_with("claude mcp get 0"), "{skipped:?}");
        assert!(skipped[4].ends_with("claude mcp get 4"), "{skipped:?}");
        assert_eq!(
            skipped[5],
            "Skipped 35 more lines that are not claude mcp add commands."
        );
        assert_eq!(skipped[6], "Skipped 40 lines of other text.");
        // The same line many times is one line, as before.
        let same = "claude mcp install thing\n".repeat(40);
        let pasted = Pasted::parse(&format!("{same}claude mcp add s -- node\n")).unwrap();
        assert_eq!(pasted.notes.len(), 1, "{pasted:?}");
    }

    #[test]
    fn a_mistyped_command_is_named_however_much_text_comes_before_it() {
        // Review of #29: a guide pasted whole puts more than five lines before its commands, and the one skipped line
        // the user needs to see, a mistyped command, was only counted.
        let guide =
            |n: usize| -> String { (0..n).map(|i| format!("Step {i} of the guide\n")).collect() };
        for text in [
            format!(
                "{}claude mcp ad foo -- npx foo\nclaude mcp add bar -- npx bar\n",
                guide(6)
            ),
            format!(
                "claude mcp add bar -- npx bar\n{}claude mcp ad foo -- npx foo\n",
                guide(40)
            ),
        ] {
            let pasted = Pasted::parse(&text).unwrap();
            let skipped: Vec<&String> = pasted
                .notes
                .iter()
                .filter(|n| n.starts_with("Skipped"))
                .collect();
            assert!(
                skipped[0].ends_with(": claude mcp ad foo -- npx foo"),
                "{skipped:?}"
            );
            assert_eq!(skipped.len(), 2, "{skipped:?}");
            assert!(
                skipped[1].starts_with("Skipped ") && skipped[1].ends_with(" lines of other text."),
                "{skipped:?}"
            );
        }
    }

    #[test]
    fn one_more_is_named_not_counted() {
        // Review of #29: "1 more" takes the room of the line it hides.
        let lines: String = (0..6).map(|i| format!("claude mcp get {i}\n")).collect();
        let pasted = Pasted::parse(&format!("claude mcp add s -- node\n{lines}")).unwrap();
        assert_eq!(pasted.notes.len(), 6, "{pasted:?}");
        assert!(
            pasted.notes.iter().all(|n| !n.contains("more")),
            "{pasted:?}"
        );
        let names: String = (0..9).map(|i| format!(" $V{i}")).collect();
        let pasted = Pasted::parse(&format!("claude mcp add s -- node{names}")).unwrap();
        assert!(
            pasted.notes[0].contains("$V7, $V8 when") && !pasted.notes[0].contains("more"),
            "{:?}",
            pasted.notes
        );
        // Two hidden are counted.
        let names: String = (0..10).map(|i| format!(" $V{i}")).collect();
        let pasted = Pasted::parse(&format!("claude mcp add s -- node{names}")).unwrap();
        assert!(
            pasted.notes[0].contains("$V7 and 2 more when"),
            "{:?}",
            pasted.notes
        );
    }

    #[test]
    fn the_largest_paste_is_read_at_typing_speed() {
        // Found reviewing performance: skipped lines and variable names were de-duplicated by searching every earlier
        // one, so the largest paste took 1.5 to 2.3 s (release) to read, again after every key typed. The bound is
        // loose, for debug builds on slow machines; the quadratic version took far longer.
        let full = |first: &str, each: &dyn Fn(usize) -> String| {
            let mut text = first.to_owned();
            for i in 0.. {
                let more = each(i);
                if text.len() + more.len() > MAX_PASTE_BYTES {
                    break;
                }
                text.push_str(&more);
            }
            text
        };
        let lines = full("claude mcp add s -- node\n", &|i| format!("l{i}\n"));
        let vars = full("claude mcp add v -- node", &|i| format!(" $V{i}"));
        let started = std::time::Instant::now();
        for text in [&lines, &vars] {
            let pasted = Pasted::parse(text).unwrap();
            assert_eq!(pasted.servers.len(), 1);
            assert!(pasted.notes.len() <= 6, "{}", pasted.notes.len());
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
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
                .any(|n| n == "Skipped a line of other text."),
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
        // Followed by an option, nothing was left out: the name is kept.
        assert!(s.oauth && s.asks_secret && !s.word_left_out);
        assert_eq!(s.name, "corp");
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
        assert!(
            s.oauth && s.asks_secret && !s.word_left_out && s.problem.is_none(),
            "{s:?}"
        );
        assert!(!one("claude mcp add -s user plain node").oauth);
        // Without --client-secret nothing is asked for: it can be added as pasted.
        let s = one("claude mcp add -t http --client-id abc x https://x.example/mcp");
        assert!(s.oauth && !s.asks_secret && s.problem.is_none(), "{s:?}");
        // A word typed after --client-secret (it takes none) may be the secret: it is never shown, not even as
        // the name, which may be that word. What can be told apart (the address, the JSON, the options) is kept,
        // to be finished in the form with the name typed again; without an address or definition it is refused.
        for (text, finish) in [
            (
                "claude mcp add -t http --client-secret Sup3rSecret corp https://corp.example/mcp",
                true,
            ),
            (
                r#"claude mcp add-json --client-secret Sup3rSecret o '{"type":"http","url":"https://o.example"}'"#,
                true,
            ),
            (
                "claude mcp add --transport http --client-secret Sup3rSecret https://mcp.linear.app/mcp",
                true,
            ),
            ("claude mcp add-json --client-secret Sup3rSecret sso", false),
            ("claude mcp add-json --client-secret Sup3rSecret", false),
            (
                "claude mcp add --client-secret Sup3rSecret corp node server.js",
                false,
            ),
            (
                "claude mcp add -t http corp --client-secret Sup3rSecret https://corp.example/mcp",
                true,
            ),
            (
                "claude mcp add -t http corp https://corp.example/mcp --client-secret Sup3rSecret",
                true,
            ),
            (
                "claude mcp add -t http --client-secret=Sup3rSecret corp https://corp.example/mcp",
                true,
            ),
            (
                r#"claude mcp add-json --client-secret=Sup3rSecret o '{"type":"http","url":"https://o.example"}'"#,
                true,
            ),
            (
                r#"claude mcp add-json o --client-secret Sup3rSecret '{"type":"http","url":"https://o.example"}'"#,
                true,
            ),
        ] {
            let pasted = Pasted::parse(text).unwrap();
            let s = &pasted.servers[0];
            assert!(!format!("{pasted:?}").contains("Sup3rSecret"), "{pasted:?}");
            if finish {
                assert!(s.problem.is_none() && s.asks_secret, "{text}: {s:?}");
                assert_eq!(s.name, "", "{text}");
                assert!(
                    json_of(s)["url"].as_str().unwrap().starts_with("https://"),
                    "{text}"
                );
                // Said once, by the dialog, where it offers the form: not again among the notes.
                assert!(s.word_left_out, "{text}: {s:?}");
                assert!(
                    !pasted.notes.iter().any(|n| n.contains("--client-secret")),
                    "{pasted:?}"
                );
            } else {
                assert!(
                    s.problem.as_deref().unwrap().contains("takes no value"),
                    "{s:?}"
                );
            }
        }
        // `--client-secret <name> <address>` is Claude Code's own syntax: it is finished in the form, with the client
        // id kept and the name typed there.
        let s = one(
            "claude mcp add --transport http --client-id abc --client-secret corp https://mcp.corp.example/mcp",
        );
        assert!(
            s.asks_secret && s.word_left_out && s.problem.is_none() && s.name.is_empty(),
            "{s:?}"
        );
        assert_eq!(
            json_of(&s),
            json!({"type": "http", "url": "https://mcp.corp.example/mcp", "oauth": {"clientId": "abc"}})
        );
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
        assert_eq!(pasted.notes, ["Skipped 2 lines of other text."]);
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
    fn prose_with_an_apostrophe_hides_none_of_the_commands_after_it() {
        // Found reviewing main: an apostrophe in a README line opened a quote that ran to the end of the paste.
        for text in [
            "Don't run as root.\nclaude mcp add a -s user node\nclaude mcp add b node",
            "Here's how:\nclaude mcp add a -s user node\nclaude mcp add b node",
            "1. Don't forget the scope:\n$ claude mcp add a -s user node\n$ claude mcp add b node",
            "claude mcp add a -s user node\nThat's it. The next one's for docs:\nclaude mcp add b node",
        ] {
            let pasted = Pasted::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            let names: Vec<_> = pasted.servers.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["a", "b"], "{text}");
            assert!(
                pasted.notes.iter().any(|n| n.ends_with(" of other text.")),
                "{:?}",
                pasted.notes
            );
        }
        // A quote in a claude command still runs over lines, and a quoted program path is still read.
        let s = one("claude mcp add-json x '{\n  \"command\": \"node\"\n}'\nThat's all.");
        assert_eq!(json_of(&s)["command"], "node");
        let s = one(r#""C:\Program Files\claude\claude.exe" mcp add x node"#);
        assert_eq!(json_of(&s)["command"], "node");
    }

    #[test]
    fn every_switch_git_bash_turned_into_a_drive_is_undone() {
        // Found reviewing main: `cmd /s /c npx` arrives as `cmd S:/ C:/ npx`, and only the first was undone.
        let pasted = Pasted::parse("claude mcp add w -- cmd S:/ C:/ npx -y pkg").unwrap();
        assert_eq!(
            json_of(&pasted.servers[0])["args"],
            json!(["/s", "/c", "npx", "-y", "pkg"])
        );
        // One note names them all (review round 1, #30).
        assert_eq!(
            pasted.notes,
            [
                "Read S:/ C:/ after cmd as /s /c: Git Bash turns these switches into drives on their way to a Windows program."
            ]
        );
        // Still only the switches right after cmd.
        let v = json_of(&one("claude mcp add w -- cmd C:/ dir D:/"));
        assert_eq!(v["args"], json!(["/c", "dir", "D:/"]));
    }

    #[test]
    fn an_option_without_its_value_is_a_problem() {
        // Found reviewing main: these were dropped without a word, where Claude Code stops with an error.
        for (text, option) in [
            ("claude mcp add x node -e", "-e"),
            (
                "claude mcp add --transport http x https://h.example.com/mcp -H",
                "-H",
            ),
            ("claude mcp add x node --client-id", "--client-id"),
            ("claude mcp add x node -t", "-t"),
            (r#"claude mcp add-json x '{"command":"node"}' -s"#, "-s"),
        ] {
            let s = one(text);
            let problem = s.problem.unwrap_or_else(|| panic!("no problem for {text}"));
            assert!(problem.contains(option), "{text}: {problem}");
        }
    }

    #[test]
    fn typography_from_a_web_page_is_read_or_named() {
        // Found reviewing main: a page's non-breaking spaces, en dash and curly quotes.
        let s = one("claude\u{a0}mcp\u{a0}add x\u{a0}--\u{a0}npx -y pkg");
        assert_eq!(json_of(&s)["args"], json!(["-y", "pkg"]));
        for (text, says) in [
            (
                "claude mcp add \u{2013}-transport http x https://h.example.com/mcp",
                "--",
            ),
            (
                "claude mcp add \u{2014}transport http x https://h.example.com/mcp",
                "--",
            ),
            (
                "claude mcp add x -- node \u{201c}a b\u{201d}",
                "straight quotes",
            ),
            (
                "claude mcp add x -- node \u{2018}a b\u{2019}",
                "straight quotes",
            ),
        ] {
            let s = one(text);
            let problem = s.problem.unwrap_or_else(|| panic!("no problem for {text}"));
            assert!(problem.contains(says), "{text}: {problem}");
        }
        // Inside straight quotes, they are what was meant.
        let s = one("claude mcp add x -- node \"\u{201c}quoted\u{201d}\u{a0}text\"");
        assert_eq!(
            json_of(&s)["args"],
            json!(["\u{201c}quoted\u{201d}\u{a0}text"])
        );
    }

    #[test]
    fn an_option_written_with_equals_says_to_put_a_space() {
        // Review round 1 (#30): `-H=value` read as "has no option -H"; Claude Code would take `=value` as the value.
        for (text, option, value) in [
            (
                "claude mcp add --transport http foo https://x.example.com -H=Authorization:abc",
                "-H",
                "Authorization:abc",
            ),
            ("claude mcp add foo -e=KEY=1 -- npx foo", "-e", "KEY=1"),
            ("claude mcp add foo -s=user -- npx foo", "-s", "=user"),
            ("claude mcp add foo -t=stdio -- npx foo", "-t", "=stdio"),
        ] {
            let problem = one(text)
                .problem
                .unwrap_or_else(|| panic!("no problem for {text}"));
            assert!(
                problem.starts_with(&format!("{option} takes its value after a space")),
                "{text}: {problem}"
            );
            assert!(!problem.contains(value), "{text}: {problem}");
        }
        // An option Claude Code does not have is still named as one, without its value.
        let s = one("claude mcp add foo -x=SECRETVALUE -- npx foo");
        assert_eq!(
            s.problem.as_deref(),
            Some("Claude Code's mcp add has no option -x.")
        );
    }

    #[test]
    fn a_curly_quote_inside_a_word_is_named_too() {
        // Review round 1 (#30): only a quote starting a word was caught, so `KEY=“abc def”` was offered and ran
        // `def”`; and the problem did not say which word.
        for (text, word) in [
            (
                "claude mcp add foo -e TITLE=\u{201c}abc def\u{201d} -- npx foo",
                "TITLE=\u{201c}abc",
            ),
            (
                "claude mcp add foo -e TITLE=\u{2018}abc\u{2019} -- npx foo",
                "TITLE=\u{2018}abc\u{2019}",
            ),
            // A name that sounds secret keeps its value hidden, as in the list of servers.
            (
                "claude mcp add foo -e KEY=\u{201c}abc def\u{201d} -- npx foo",
                "KEY=…",
            ),
            (
                "claude mcp add github -e OTHER=1 -s user \u{2013}- npx -y pkg",
                "\u{2013}-",
            ),
            ("claude mcp add x -- node \u{201c}a b\u{201d}", "\u{201c}a"),
        ] {
            let problem = one(text)
                .problem
                .unwrap_or_else(|| panic!("no problem for {text}"));
            assert!(problem.contains(word), "{text}: {problem}");
        }
        let s = one(
            "claude mcp add --transport http foo https://x.example.com -H=\u{201c}Authorization: Bearer x\u{201d}",
        );
        assert!(
            s.problem.as_deref().is_some_and(|p| p.contains("curly")),
            "{:?}",
            s.problem
        );
        // An apostrophe inside a word is one.
        let s = one("claude mcp add foo -- node server.js --title O\u{2019}Brien");
        assert_eq!(s.problem, None);
        // The word named shows no secret.
        let s = one(
            "claude mcp add foo -e API_KEY=\u{201c}sk_live_ABCDEFGHIJKLMNOP1234\u{201d} -- npx foo",
        );
        let problem = s.problem.expect("a curly quote is a problem");
        assert!(problem.contains("API_KEY="), "{problem}");
        assert!(!problem.contains("ABCDEFGH"), "{problem}");
    }

    #[test]
    fn a_readme_pasted_with_its_fences_bullets_and_prompts_reads_cleanly() {
        // Review round 1 (#30): a fence gave a note with nothing after its colon, and a numbered, bulleted or quoted
        // command, or one after a PowerShell or cmd prompt, was skipped.
        let pasted = Pasted::parse(
            "```sh\nclaude mcp add a -- npx a\n```\n~~~ bash\n1. claude mcp add b -- npx b\n- claude mcp add c -- npx c\n> claude mcp add d -- npx d\nPS C:\\Users\\me> claude mcp add e -- npx e\n* claude mcp add f -- npx f\nC:\\Users\\me> claude mcp add g -- npx g\n> > 2) claude mcp add h -- npx h\n~~~\n\u{1b}[0m\n",
        )
        .unwrap();
        let names: Vec<&str> = pasted.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e", "f", "g", "h"]);
        assert_eq!(pasted.notes, Vec::<String>::new());
        // A line of other text is still counted, and a line that starts with shell too; a claude line is named
        // without its marker.
        let pasted = Pasted::parse(
            "- npm install -g foo\n| grep x\n- claude mcp list\nclaude mcp add a -- npx a",
        )
        .unwrap();
        assert_eq!(
            pasted.notes,
            [
                "Skipped a line that is not a claude mcp add command: claude mcp list",
                "Skipped 2 lines of other text.",
            ]
        );
    }

    #[test]
    fn a_command_chained_after_another_is_still_read() {
        // Found re-reading this PR: since a line not starting with claude is skipped whole, `cd app && claude mcp
        // add …` lost the command, which main read.
        for text in [
            "cd ~/app && claude mcp add a -- npx a",
            "npm i -g pkg; claude mcp add a -- npx a",
        ] {
            let pasted = Pasted::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(pasted.servers.len(), 1, "{text}");
            assert_eq!(pasted.servers[0].name, "a", "{text}");
            assert_eq!(pasted.notes.len(), 1, "{text}: {:?}", pasted.notes);
        }
        // Prose with a semicolon is still one skipped line.
        let pasted =
            Pasted::parse("Don't run it as root; it needs your PATH.\nclaude mcp add b -- npx b")
                .unwrap();
        assert_eq!(pasted.servers.len(), 1);
        // A Windows path to claude is a command, not a prompt, even with a redirect in it.
        let s = one(r"C:\tools\claude.exe mcp add c -- node x > log.txt");
        assert!(
            s.problem.as_deref().is_some_and(|p| p.contains("redirect")),
            "{:?}",
            s.problem
        );
    }

    #[test]
    fn a_claude_command_after_a_pipe_or_an_or_says_it_uses_the_shell() {
        // Review round 2 (#30): these were refused as "does not start with claude mcp add", which hides the reason.
        for (text, why) in [
            ("false || claude mcp add a -- npx a", "(||)"),
            ("echo y | claude mcp add a -- npx a", "(a pipe, |)"),
            ("sleep 1 & claude mcp add a -- npx a", "(&)"),
        ] {
            let pasted = Pasted::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(pasted.servers.len(), 1, "{text}: {pasted:?}");
            let s = &pasted.servers[0];
            assert!(s.json.is_empty(), "{text}: {s:?}");
            let problem = s.problem.as_deref().unwrap_or_default();
            assert!(
                problem.contains(&format!("uses the shell {why}")),
                "{text}: {problem}"
            );
        }
        // Nearby: a Markdown table row that starts with a pipe is not a pipe into claude.
        let error = Pasted::parse("| claude mcp add a -- npx a | adds a |").unwrap_err();
        assert!(!error.contains("shell"), "{error}");
    }

    #[test]
    fn a_command_in_markdown_inline_code_is_read() {
        // Review round 2 (#30): a raw README writes the command as `claude mcp add …`; that gave only "Nothing here
        // adds a server".
        let pasted = Pasted::parse(
            "`claude mcp add a -- npx a`\n- `claude mcp add b -s user -- npx b`  \nRun `claude mcp add c -- npx \"c d\"` in a terminal, then it's there.\n",
        )
        .unwrap();
        let names: Vec<&str> = pasted.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"], "{pasted:?}");
        assert!(
            pasted.servers.iter().all(|s| s.problem.is_none()),
            "{pasted:?}"
        );
        assert_eq!(json_of(&pasted.servers[2])["args"], json!(["c d"]));
        assert_eq!(pasted.servers[1].scope, Some(Scope::User));
        // Shell inside the code is still the shell, and other code is other text.
        let s = one("`claude mcp add x -- node a.js | tee log`");
        assert!(s.problem.is_some_and(|p| p.contains("|")) && s.json.is_empty());
        let pasted = Pasted::parse("`npm install -g x`\nclaude mcp add y -- npx y").unwrap();
        assert_eq!(pasted.notes, ["Skipped a line of other text."]);
    }

    #[test]
    fn claude_code_run_through_a_package_runner_is_read_as_claude() {
        // Review round 2 (#30): `npx @anthropic-ai/claude-code mcp add …` gave only "Nothing here adds a server".
        for text in [
            "npx @anthropic-ai/claude-code mcp add a -- npx a",
            "npx -y @anthropic-ai/claude-code@latest mcp add a -- npx a",
            "npx --yes @anthropic-ai/claude-code@2.1.0 mcp add a -- npx a",
            "bunx @anthropic-ai/claude-code mcp add a -- npx a",
            "pnpm dlx @anthropic-ai/claude-code mcp add a -- npx a",
            "$ npx @anthropic-ai/claude-code mcp add a -- npx a",
        ] {
            let s = one(text);
            assert_eq!(s.name, "a", "{text}");
            assert_eq!(s.problem, None, "{text}");
            assert_eq!(json_of(&s)["args"], json!(["a"]), "{text}");
        }
        // Nearby: another package is another program.
        for text in [
            "npx @anthropic-ai/claude-code-router mcp add a -- npx a",
            "npx @someone/claude-code mcp add a -- npx a",
            "npxx @anthropic-ai/claude-code mcp add a -- npx a",
        ] {
            assert!(Pasted::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn other_text_is_counted_in_one_note_and_claude_lines_are_named() {
        // Owed from the performance review: a pasted guide named each of its sentences; only a line that starts
        // with claude is likely a mistyped command worth reading.
        let pasted = Pasted::parse(
            "# Setup\nInstall Node first.\nThen run this in Claude Code:\nclaude mcp list\nsudo claude mcp add z -- npx z\nclaude mcp add a -- npx a\nThat's all.\n",
        )
        .unwrap();
        assert_eq!(
            pasted.notes,
            [
                "Skipped a line that is not a claude mcp add command: claude mcp list",
                "Skipped a line that is not a claude mcp add command: sudo claude mcp add z -- npx z",
                "Skipped 3 lines of other text.",
            ]
        );
        // Each line counts, also when it repeats.
        let pasted = Pasted::parse("npm i x\nnpm i x\nclaude mcp add a -- npx a").unwrap();
        assert_eq!(pasted.notes, ["Skipped 2 lines of other text."]);
    }

    #[test]
    fn a_paste_of_many_commands_is_refused_without_reading_them_all() {
        // Not-on-main list: thousands of commands were each read in full, then refused for being over 100.
        let many: String = (0..7_000)
            .map(|i| format!("claude mcp add s{i} -- npx p{i}\n"))
            .collect();
        assert!(many.len() < MAX_PASTE_BYTES);
        let error = Pasted::parse(&many).unwrap_err();
        assert_eq!(
            error,
            "That holds more than 100 servers; Klefki adds at most 100 at a time."
        );
    }

    #[test]
    fn a_full_paste_of_one_long_line_is_read_quickly() {
        // Review round 2 (#30): each command start copied the rest of its line, so one 256 KiB line of prompts,
        // markers or commands took up to 37 s, and the dialog reads the paste again after every key.
        let tail = "claude mcp add z -- npx z";
        for unit in [
            "$ ",
            "> ",
            "- ",
            "1. ",
            "PS C:\\> ",
            "claude mcp add s -- x; ",
            "claude mcp add s -- x && ",
            "cd a && ",
            "x;",
            ";",
            "claude;",
            "\u{1b}[0m && claude mcp add s -- x; ",
        ] {
            let text = unit.repeat((MAX_PASTE_BYTES - tail.len()) / unit.len()) + tail;
            let started = std::time::Instant::now();
            let _ = Pasted::parse(&text);
            let took = started.elapsed();
            assert!(
                took < std::time::Duration::from_secs(2),
                "{unit:?}: {took:?}"
            );
        }
    }

    #[test]
    fn when_nothing_is_added_the_line_meant_is_named() {
        // Review round 1 (#30): a mistyped command gave only "Nothing here adds a server".
        for (text, line) in [
            (
                "claude mcp ad foo -- npx foo",
                "claude mcp ad foo -- npx foo",
            ),
            (
                "Install it:\nsudo claude mcp add foo -- npx foo",
                "sudo claude mcp add foo -- npx foo",
            ),
        ] {
            let error = Pasted::parse(text).unwrap_err();
            assert!(error.contains(line), "{text}: {error}");
        }
        let error =
            Pasted::parse("claude mcp ad foo -e API_KEY=sk_live_ABCDEFGHIJKLMNOP1234 -- npx foo")
                .unwrap_err();
        assert!(!error.contains("ABCDEFGH"), "{error}");
        // With no such line, the general advice.
        let error = Pasted::parse("Just some text.").unwrap_err();
        assert!(
            error.starts_with("Nothing here adds a server. Paste"),
            "{error}"
        );
    }

    #[test]
    fn a_skipped_line_shows_no_secret() {
        // Review round 1 (#30), nearby: the note quoted a skipped line as it was.
        let pasted = Pasted::parse(
            "export OPENAI_API_KEY=sk-proj-ABCDEFGHIJKLMNOP1234\nclaude mcp add a -- npx a",
        )
        .unwrap();
        assert_eq!(pasted.notes, ["Skipped a line of other text."]);
        // A named line is masked.
        let pasted = Pasted::parse(
            "claude mcp ad b -e OPENAI_API_KEY=sk-proj-ABCDEFGHIJKLMNOP1234 -- npx b\nclaude mcp add a -- npx a",
        )
        .unwrap();
        assert_eq!(pasted.notes.len(), 1, "{:?}", pasted.notes);
        assert!(!pasted.notes[0].contains("ABCDEFGH"), "{:?}", pasted.notes);
        assert!(
            pasted.notes[0].contains("OPENAI_API_KEY="),
            "{:?}",
            pasted.notes
        );
    }

    #[test]
    fn the_summary_hides_what_the_list_hides() {
        // Found reviewing main: the preview showed in full what the list of servers masks.
        let s = one("claude mcp add a -- npx -y alpha-mcp --api-key sk-live-ARGSECRET123456");
        assert_eq!(s.summary, "npx -y alpha-mcp --api-key …");
        let s = one(
            "claude mcp add --transport http b https://me:pw123456@h.example.com/mcp?token=SECRET1234",
        );
        assert_eq!(s.summary, "http · https://h.example.com/mcp?…");
        let s = one(r#"{"c": {"type": "sse", "url": "not a url ghp_abcdefghijklmnop1234"}}"#);
        assert!(!s.summary.contains("abcdefghij"), "{}", s.summary);
        // An escape left open in one word hides none of the others.
        let pasted = Pasted::parse(
            r#"{"d": {"command": "node", "args": ["--title=\u001b]", "-e", "evil()", "\u0007", "server.js"]}}"#,
        )
        .unwrap();
        let summary = &pasted.servers[0].summary;
        assert!(summary.contains(" -e evil() "), "{summary:?}");
        assert!(summary.ends_with(" server.js"), "{summary:?}");
    }

    #[test]
    fn a_problem_never_repeats_a_secret() {
        // Found reviewing main: a word Klefki could not place was quoted back whole.
        for text in [
            "claude mcp add x -k=sk-live-ARGSECRET123456 -- node",
            "claude mcp add x -e sk-live-ARGSECRET123456 -- node",
            r#"claude mcp add --transport http x https://h.example.com/mcp -H "Bearer sk-live-ARGSECRET123456""#,
        ] {
            let s = one(text);
            let problem = s.problem.unwrap_or_else(|| panic!("no problem for {text}"));
            assert!(!problem.contains("ARGSECRET"), "{problem}");
        }
        // What is not secret is still quoted back, so the mistake can be found.
        let s = one("claude mcp add x -e VERBOSE -- node");
        assert!(s.problem.unwrap_or_default().contains("VERBOSE"));
        let s = one("claude mcp add x -k=1 -- node");
        assert!(s.problem.unwrap_or_default().contains("no option -k."));
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
