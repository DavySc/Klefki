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

/// The line, what to do before it is used and what to know. Text other than the line is sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorCommand {
    /// One line for a shell, on this system's rules for quoting.
    pub text: String,
    /// What the user must do before the line works, each a sentence that starts with the action: values the line
    /// leaves out and where to enter them.
    pub to_do: Vec<String>,
    /// What the line does not do, each a sentence.
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
    let mut to_do = Vec::new();
    let mut notes = Vec::new();
    let mut words: Vec<String> = Vec::new();
    // What stands for a secret, and the variables the Inspector would not fill in.
    let mut hidden: Vec<String> = Vec::new();
    let mut variables: Vec<String> = Vec::new();
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
            words.push(quote(&filled_in(program, &mut variables))?);
            let args: Vec<&str> = object
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            // An address that signs in keeps its host and path: only the user name and password go.
            let args = rewritten_args(args, true, &mut |flag, _| {
                hidden.push(if flag.is_empty() {
                    "a secret in an argument".to_owned()
                } else {
                    shown(flag)
                });
                HIDDEN.to_owned()
            });
            for arg in &args {
                words.push(quote(&filled_in(arg, &mut variables))?);
            }
            let names = keys(object.get("env"));
            if !names.is_empty() {
                to_do.push(format!(
                    "Enter {} in the Inspector, under Environment Variables: its environment is not in the line, so no secret reaches your shell.",
                    shown(&names.join(", "))
                ));
            }
            if !relative_paths(definition).is_empty() {
                notes.push(
                    "Part of the command is relative to a folder: run the line from the folder you start Claude Code in."
                        .to_owned(),
                );
            }
        }
        Some(kind @ ("http" | "sse")) => {
            let address = object
                .get("url")
                .and_then(Value::as_str)
                .ok_or("it has no address")?
                .trim();
            // Parsed only to check it: written back, it would change `${HOST}` into `${host}` and escape `{`.
            let parsed = url::Url::parse(address).map_err(|_| "its address is not valid")?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err("its address is not http or https".to_owned());
            }
            let address = rewritten_url(address, &mut |part, _| {
                hidden.push(if part.is_empty() {
                    "the user name and password in the address".to_owned()
                } else {
                    shown(part)
                });
                HIDDEN.to_owned()
            })
            .unwrap_or_else(|| address.to_owned());
            words.push("--transport".to_owned());
            words.push(kind.to_owned());
            words.push("--server-url".to_owned());
            words.push(quote(&filled_in(&address, &mut variables))?);
            let names = keys(object.get("headers"));
            if !names.is_empty() {
                to_do.push(format!(
                    "Add {} in the Inspector, under Authentication: its headers are not in the line, so no token reaches your shell.",
                    shown(&names.join(", "))
                ));
            }
            if object.get("oauth").is_some() {
                notes.push(
                    "The Inspector signs in on its own, and keeps what that gives it in its own store; it does not use Claude Code's sign-in."
                        .to_owned(),
                );
            }
        }
        Some("ws") => return Err("the Inspector does not speak WebSocket".to_owned()),
        Some(_) => return Err("its type is not one the Inspector knows".to_owned()),
    }
    if !variables.is_empty() {
        variables.dedup();
        to_do.insert(
            0,
            format!(
                "Replace {HIDDEN} with the value of {}: Claude Code fills in a ${{NAME}} from its environment, the Inspector does not.",
                shown(&variables.join(", "))
            ),
        );
    }
    if !hidden.is_empty() {
        hidden.dedup();
        to_do.insert(
            0,
            format!(
                "Replace {HIDDEN} with {} before running the line: it looked like a secret, so Klefki left it out.",
                hidden.join(", ")
            ),
        );
    }
    notes.push(
        "Needs Node 22.19 or newer. Running the line downloads the Inspector and opens it in your browser; Klefki does not run it."
            .to_owned(),
    );
    Ok(InspectorCommand {
        text: format!("{PROGRAM} {}", words.join(" ")),
        to_do,
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

/// `text` with each `${NAME}` (or `${NAME:-default}`) written as [`HIDDEN`], and the names added to `variables`:
/// the Inspector does not fill them in, and a line that kept them would need a shell-specific escape.
fn filled_in(text: &str, variables: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let after = &rest[at + 2..];
        let named = after.find('}').map(|end| {
            let name = after[..end].split(":-").next().unwrap_or_default();
            (name, end)
        });
        match named {
            Some((name, end))
                if !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                out.push_str(&rest[..at]);
                out.push_str(HIDDEN);
                variables.push(name.to_owned());
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(&rest[..at + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A character no quoting can make safe to paste: a line break would end the command, other controls and bidi
/// characters can disguise it.
fn unsafe_char(text: &str) -> Option<char> {
    text.chars()
        .find(|&c| c.is_control() || crate::sanitize::is_bad(c))
}

/// A character that needs no quoting. `=` does only after the first: zsh, macOS's shell, turns a word that starts
/// with one into a path.
fn plain(index: usize, c: char, extra: &str) -> bool {
    c.is_ascii_alphanumeric()
        || "_-./:+".contains(c)
        || (c == '=' && index > 0)
        || extra.contains(c)
}

fn all_plain(text: &str, extra: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .enumerate()
            .all(|(index, c)| plain(index, c, extra))
}

/// One word for a POSIX shell: as it is when plain, else in single quotes, which read nothing (`$`, backticks, `!`).
fn posix_word(text: &str) -> Result<String, String> {
    if let Some(c) = unsafe_char(text) {
        return Err(refused(text, c));
    }
    if all_plain(text, "@%,") {
        return Ok(text.to_owned());
    }
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

/// One word for PowerShell and `cmd` alike: as it is when plain, else in double quotes with the backslashes before
/// the closing one doubled (PowerShell 7.3 and `cmd` read a doubled trailing backslash differently, which a path
/// does not mind). Neither shell reads `"`, `$`, a backtick, `%`, `^` or `!` the same way inside them, so a
/// word with one is refused rather than guessed at.
fn windows_word(text: &str) -> Result<String, String> {
    if let Some(c) = unsafe_char(text) {
        return Err(refused(text, c));
    }
    if all_plain(text, "\\") {
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
