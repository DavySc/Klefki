//! The command line that opens a server in the MCP Inspector, the protocol's own debugging tool: pick a server,
//! paste one line, and see its tools and what it answers, outside Claude Code. Klefki only writes the text; it never
//! runs it, because `npx` downloads and starts code, and the Inspector keeps the values it is given.
//!
//! Nothing that may be a secret is in the line: environment and header values are left out whatever they look like
//! (their names are listed to enter in the Inspector), and an argument or address part that looks like a secret
//! becomes [`HIDDEN`]. Text that could do more than it shows (control and bidi characters), or that the shell would
//! read as more than a word, is refused instead of written.
use crate::config::inventory::is_safe_project_path;
use crate::config::relative::relative_paths;
use crate::config::secretlike::{rewritten_args, rewritten_url};
use crate::sanitize::shown;
use serde_json::Value;

/// What a hidden secret is written as, to replace.
pub(crate) const HIDDEN: &str = "REPLACE_ME";
/// The Inspector, as `npx` names it. Version 2 (Node 22.19 or newer): it takes the server on its command line.
const PROGRAM: &str = "npx @modelcontextprotocol/inspector";

/// The line, and what to know before it is used. Text other than the line is sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorCommand {
    /// One line for a shell, on this system's rules for quoting.
    pub text: String,
    /// What the line leaves out or does not do, each a sentence.
    pub notes: Vec<String>,
}

/// The line for `definition`. `project` is the folder of a local or project server, where
/// Claude Code starts it; `windows` quotes for PowerShell and `cmd`, otherwise for a POSIX shell. `Err` says why there
/// is no line.
pub(crate) fn command(
    definition: &Value,
    project: Option<&str>,
    windows: bool,
) -> Result<InspectorCommand, String> {
    let quote = if windows { windows_word } else { posix_word };
    let Some(object) = definition.as_object() else {
        return Err("its definition is not a JSON object".to_owned());
    };
    let mut notes = Vec::new();
    let mut words: Vec<String> = Vec::new();
    match object.get("type").and_then(Value::as_str) {
        None | Some("stdio") => {
            let program = object
                .get("command")
                .and_then(Value::as_str)
                .filter(|c| !c.trim().is_empty())
                .ok_or("it has no command")?;
            if let Some(folder) = project.filter(|p| is_safe_project_path(p)) {
                words.push("--cwd".to_owned());
                words.push(quote(folder)?);
            }
            words.push("--".to_owned());
            words.push(quote(program)?);
            let args: Vec<&str> = object
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut hidden: Vec<String> = Vec::new();
            let args = rewritten_args(args, false, &mut |flag, _| {
                hidden.push(flag.to_owned());
                HIDDEN.to_owned()
            });
            for arg in &args {
                words.push(quote(arg)?);
            }
            hidden_note(&hidden, &mut notes);
            let names = keys(object.get("env"));
            if !names.is_empty() {
                notes.push(format!(
                    "Its environment ({}) is not in the line, so no secret is typed into a shell: enter it in the Inspector, under Environment Variables.",
                    shown(&names.join(", "))
                ));
            }
            if !relative_paths(definition).is_empty() {
                notes.push(
                    "Part of the command is relative to a folder: run the line from the folder you start Claude Code in."
                        .to_owned(),
                );
            }
            references(program, &args, &mut notes);
        }
        Some(kind @ ("http" | "sse")) => {
            let address = object
                .get("url")
                .and_then(Value::as_str)
                .ok_or("it has no address")?;
            let parsed = url::Url::parse(address.trim()).map_err(|_| "its address is not valid")?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err("its address is not http or https".to_owned());
            }
            let mut hidden: Vec<String> = Vec::new();
            let address = match rewritten_url(parsed.as_str(), &mut |part, _| {
                hidden.push(part.to_owned());
                HIDDEN.to_owned()
            }) {
                Some(rewritten) => rewritten,
                None => parsed.to_string(),
            };
            words.push("--transport".to_owned());
            words.push(kind.to_owned());
            words.push("--server-url".to_owned());
            words.push(quote(&address)?);
            hidden_note(&hidden, &mut notes);
            let names = keys(object.get("headers"));
            if !names.is_empty() {
                notes.push(format!(
                    "Its headers ({}) are not in the line, so no token is typed into a shell: add them in the Inspector, under Authentication.",
                    shown(&names.join(", "))
                ));
            }
            if object.get("oauth").is_some() {
                notes.push(
                    "The Inspector signs in on its own, and keeps what that gives it in its own store; it does not use Claude Code's sign-in."
                        .to_owned(),
                );
            }
            references(&address, &[], &mut notes);
        }
        Some("ws") => return Err("the Inspector does not speak WebSocket".to_owned()),
        Some(_) => return Err("its type is not one the Inspector knows".to_owned()),
    }
    notes.push(
        "Needs Node 22.19 or newer. Running the line downloads the Inspector and opens it in your browser; Klefki does not run it."
            .to_owned(),
    );
    Ok(InspectorCommand {
        text: format!("{PROGRAM} {}", words.join(" ")),
        notes,
    })
}

/// The names of an `env` or `headers` object, in the file's order.
fn keys(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

fn hidden_note(hidden: &[String], notes: &mut Vec<String>) {
    if hidden.is_empty() {
        return;
    }
    let named: Vec<String> = hidden
        .iter()
        .map(|h| {
            if h.is_empty() {
                "an unnamed value".to_owned()
            } else {
                shown(h)
            }
        })
        .collect();
    notes.push(format!(
        "Left out what looks like a secret ({}); the line says {HIDDEN} there. Replace it before running the line.",
        named.join(", ")
    ));
}

/// Claude Code fills `${NAME}` in from its environment; the Inspector does not.
fn references(program: &str, args: &[String], notes: &mut Vec<String>) {
    if std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .any(|part| part.contains("${"))
    {
        notes.push(
            "It has ${NAME} references. Claude Code fills those in from its environment; the Inspector does not, so write their values in before running the line."
                .to_owned(),
        );
    }
}

/// A character no quoting can make safe to paste: a line break would end the command, other controls and bidi
/// characters can disguise it.
fn unsafe_char(text: &str) -> Option<char> {
    text.chars()
        .find(|&c| c.is_control() || crate::sanitize::is_bad(c))
}

fn plain(c: char, extra: &str) -> bool {
    c.is_ascii_alphanumeric() || "_-./:=+".contains(c) || extra.contains(c)
}

/// One word for a POSIX shell: as it is when plain, else in single quotes, which read nothing (`$`, backticks, `!`).
fn posix_word(text: &str) -> Result<String, String> {
    if let Some(c) = unsafe_char(text) {
        return Err(refused(text, c));
    }
    if !text.is_empty() && text.chars().all(|c| plain(c, "@%,")) {
        return Ok(text.to_owned());
    }
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

/// One word for PowerShell and `cmd` alike: as it is when plain, else in double quotes with the backslashes before
/// the closing one doubled. Neither shell reads `"`, `$`, a backtick, `%`, `^` or `!` the same way inside them, so a
/// word with one is refused rather than guessed at.
fn windows_word(text: &str) -> Result<String, String> {
    if let Some(c) = unsafe_char(text) {
        return Err(refused(text, c));
    }
    if !text.is_empty() && text.chars().all(|c| plain(c, "\\")) {
        return Ok(text.to_owned());
    }
    if let Some(c) = text.chars().find(|c| "\"$`%^!".contains(*c)) {
        return Err(format!(
            "{} has a {c}, which PowerShell and cmd read differently: write the line by hand",
            shown(text)
        ));
    }
    let trailing = text.len() - text.trim_end_matches('\\').len();
    Ok(format!("\"{text}{}\"", "\\".repeat(trailing)))
}

fn refused(text: &str, c: char) -> String {
    format!(
        "{} has a control or invisible character (U+{:04X}) that no quoting makes safe to paste",
        shown(text),
        u32::from(c)
    )
}

#[cfg(test)]
mod tests;
