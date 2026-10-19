//! `klefki-cli`: Klefki without a window. `list` shows the configured MCP servers, `status` asks Claude Code whether
//! they connect. Output and exit codes: docs/decisions/006-cli-output.md.
mod args;
mod check;
mod render;
mod suggest;

use args::{Cmd, Usage};
use klefki_core::api::{self, Scope, ServerRow, Snapshot};
use render::Outcome;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

const OK: u8 = 0;
/// A server checked is not connected.
const PROBLEM: u8 = 1;
const USAGE: u8 = 2;
/// Claude Code could not be used, so nothing is known about the servers: a broken tool, not a broken server.
const UNAVAILABLE: u8 = 3;
/// `export --output` wrote nothing: the file is there, or Klefki does not write there.
const NOT_WRITTEN: u8 = 4;

const SHORT_USAGE: &str = "Usage: klefki-cli list|status|export [NAME...] [--json] [--claude PATH]. See `klefki-cli help`.";

fn main() -> ExitCode {
    let code = match args::parse(std::env::args_os().skip(1)) {
        Ok(Cmd::Help) => out(args::HELP),
        Ok(Cmd::Version) => out(&format!("klefki-cli {}\n", env!("CARGO_PKG_VERSION"))),
        Ok(Cmd::List { names, json }) => list(&names, json),
        Ok(Cmd::Status {
            names,
            json,
            claude,
        }) => status(&names, json, claude),
        Ok(Cmd::Export {
            names,
            output,
            force,
        }) => export(&names, output.as_deref(), force),
        Err(Usage::NoCommand) => {
            eprint!("{}", args::HELP);
            USAGE
        }
        Err(Usage::Bad(why)) => usage(&why),
    };
    ExitCode::from(code)
}

/// The home folder as text, for shortening paths in headings.
fn home() -> Option<String> {
    std::env::home_dir().map(|h| h.to_string_lossy().into_owned())
}

fn list(names: &[String], json: bool) -> u8 {
    let snapshot = api::load_snapshot();
    warn(&snapshot);
    let rows = match select(&snapshot, names) {
        Ok(rows) => rows,
        Err(why) => return usage(&why),
    };
    let text = if json {
        render::list_json(&snapshot, &rows)
    } else {
        render::list_text(&snapshot, &rows, home().as_deref())
    };
    out(&text)
}

/// The block goes to stdout, so `> servers.json` holds only the block, or to the `--output` file; what to know about
/// it goes to stderr.
fn export(names: &[String], output: Option<&std::path::Path>, force: bool) -> u8 {
    let snapshot = api::load_snapshot();
    warn(&snapshot);
    let ids: Vec<_> = match select(&snapshot, names) {
        // Everything means every server Klefki can export: a plugin's come with the plugin. One named is tried,
        // and the export says why it was left out.
        Ok(rows) => rows
            .into_iter()
            .filter(|r| !names.is_empty() || r.scope != Scope::Plugin)
            .map(ServerRow::id)
            .collect(),
        Err(why) => return usage(&why),
    };
    let exported = api::export(&ids);
    if let Some(path) = output {
        if let Err(why) = save(path, &exported.text, force) {
            eprintln!("klefki-cli: {} was not written. {why}", path.display());
            return NOT_WRITTEN;
        }
        let what = match &ids[..] {
            [one] if exported.servers == 1 => one.name.clone(),
            _ => format!("{} servers", exported.servers),
        };
        eprintln!("Saved {what} to {}.", path.display());
    }
    for note in &exported.notes {
        eprintln!("note: {note}");
    }
    let (to_set, defaults): (Vec<_>, Vec<_>) =
        exported.variables.iter().partition(|v| v.default.is_none());
    if !to_set.is_empty() {
        eprintln!("Set these variables where the servers are added:");
        for variable in to_set {
            eprintln!("  {}  ({})", variable.name, variable.replaces.join("; "));
        }
    }
    if !defaults.is_empty() {
        eprintln!("Plain settings kept as defaults (set the variable to change one):");
        for variable in defaults {
            let default = variable.default.as_deref().unwrap_or_default();
            eprintln!(
                "  {}={default}  ({})",
                variable.name,
                variable.replaces.join("; ")
            );
        }
    }
    let code = match output {
        Some(_) => OK,
        None => out(&format!("{}\n", exported.text)),
    };
    match code {
        OK if exported.left_out > 0 => PROBLEM,
        code => code,
    }
}

/// Writes the block to `path`. A file there is replaced with `force`, or when the user, at a terminal, says so.
fn save(path: &std::path::Path, text: &str, force: bool) -> Result<(), String> {
    match api::save_export(path, text, force) {
        Err(api::SaveError::Exists) if asked_to_replace(path) => {
            api::save_export(path, text, true).map_err(|e| e.to_string())
        }
        Err(api::SaveError::Exists) => {
            Err("A file with that name is there already; add --force to replace it.".to_owned())
        }
        other => other.map_err(|e| e.to_string()),
    }
}

/// Asks on the terminal whether to replace `path`; no when nobody is there to answer.
fn asked_to_replace(path: &std::path::Path) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    eprint!("{} is there already. Replace it? [y/N] ", path.display());
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn status(names: &[String], json: bool, claude: Option<std::path::PathBuf>) -> u8 {
    // Names are checked before anything runs: a typo starts no process.
    if let Err(why) = select(&api::load_snapshot(), names) {
        return usage(&why);
    }
    let named = !names.is_empty();
    let live = std::io::stderr().is_terminal();
    let mut shown = 0;
    let answers = check::run(
        claude,
        |snapshot| {
            let rows: Vec<&ServerRow> = select(snapshot, names)
                .unwrap_or_default()
                .into_iter()
                .filter(|row| render::skip(row, named).is_none())
                .collect();
            // A project server's check may run the repository's own command: say which before it runs, as the
            // app shows the command next to its Check button.
            for row in rows.iter().filter(|r| r.scope == Scope::Project) {
                eprintln!(
                    "{} (from {}/.mcp.json) runs: {}",
                    row.name,
                    row.project.as_deref().unwrap_or("?"),
                    row.target
                );
            }
            rows.into_iter().map(ServerRow::id).collect()
        },
        |done, total, waiting| {
            if !live || total == 0 {
                return;
            }
            let line = match (done, waiting) {
                (0, Some(name)) => {
                    format!(
                        "Checking {total} server{}, waiting for {name}…",
                        plural(total)
                    )
                }
                (_, Some(name)) => format!("Checked {done} of {total}, waiting for {name}…"),
                (_, None) => format!("Checked {done} of {total}."),
            };
            let len = line.chars().count();
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{line}{:pad$}", "", pad = shown.max(len) - len);
            let _ = err.flush();
            shown = shown.max(len);
        },
    );
    if live && shown > 0 {
        // The progress line goes away once the answers are in, so only the table stays on screen.
        eprint!("\r{:shown$}\r", "");
    }
    let answers = match answers {
        Ok(answers) => answers,
        Err(problem) => {
            eprintln!("klefki-cli: {problem}");
            eprintln!("Name the Claude CLI with --claude PATH if it is installed somewhere else.");
            return UNAVAILABLE;
        }
    };
    warn(&answers.snapshot);
    let rows: Vec<(&ServerRow, Outcome)> = select(&answers.snapshot, names)
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            let outcome = render::skip(row, named).unwrap_or_else(|| {
                match answers.checked.get(&row.connection) {
                    Some(c) => Outcome::Checked(c.clone()),
                    None => Outcome::NoAnswer,
                }
            });
            (row, outcome)
        })
        .collect();
    let checked: Vec<&Outcome> = rows
        .iter()
        .map(|(_, o)| o)
        .filter(|o| o.was_checked())
        .collect();
    let code = if !checked.is_empty() && checked.iter().all(|o| o.tool_failed()) {
        UNAVAILABLE
    } else if checked.iter().any(|o| o.is_problem()) {
        PROBLEM
    } else {
        OK
    };
    let text = if json {
        render::status_json(&answers.snapshot, answers.cli_version.as_deref(), &rows)
    } else {
        render::status_text(&rows, home().as_deref())
    };
    match out(&text) {
        OK => code,
        failed => failed,
    }
}

/// The rows named (all when none are), in the order they are shown. A name that matches nothing is an error that
/// suggests the closest name.
fn select<'a>(snapshot: &'a Snapshot, names: &[String]) -> Result<Vec<&'a ServerRow>, String> {
    let mut missing = Vec::new();
    for name in names {
        if !snapshot.servers.iter().any(|r| r.name == *name) {
            let close = suggest::closest(name, snapshot.servers.iter().map(|r| r.name.as_str()));
            missing.push(match close {
                Some(meant) => format!("no server is named {name:?}; did you mean {meant:?}?"),
                None => format!("no server is named {name:?}"),
            });
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "{}\n`klefki-cli list` shows every server",
            missing.join("\n")
        ));
    }
    Ok(render::ordered(
        snapshot
            .servers
            .iter()
            .filter(|r| names.is_empty() || names.contains(&r.name)),
    ))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn warn(snapshot: &Snapshot) {
    for warning in &snapshot.warnings {
        eprintln!("warning: {warning}");
    }
}

fn usage(why: &str) -> u8 {
    eprintln!("klefki-cli: {why}");
    eprintln!("{SHORT_USAGE}");
    USAGE
}

/// Writes to stdout. A reader that went away (`klefki-cli list | head -1`) is not an error.
fn out(text: &str) -> u8 {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => OK,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => OK,
        Err(e) => {
            eprintln!("klefki-cli: could not write the output: {e}");
            PROBLEM
        }
    }
}
