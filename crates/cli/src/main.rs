//! `klefki-cli`: Klefki without a window. `list` shows the configured MCP servers, `status` asks Claude Code whether
//! they connect. Output and exit codes: docs/decisions/006-cli-output.md.
mod args;
mod check;
mod render;

use args::{Cmd, Usage};
use klefki_core::api::{self, Scope, ServerRow, Snapshot};
use render::Outcome;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

const OK: u8 = 0;
const PROBLEM: u8 = 1;
const USAGE: u8 = 2;

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
        Err(Usage(why)) => usage(&why),
    };
    ExitCode::from(code)
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
        render::list_text(&snapshot, &rows)
    };
    out(&text)
}

fn status(names: &[String], json: bool, claude: Option<std::path::PathBuf>) -> u8 {
    // Names are checked before anything runs: a typo starts no process.
    if let Err(why) = select(&api::load_snapshot(), names) {
        return usage(&why);
    }
    let live = std::io::stderr().is_terminal();
    let answers = check::run(
        claude,
        |snapshot| {
            select(snapshot, names)
                .unwrap_or_default()
                .into_iter()
                .filter(|row| to_check(row, names))
                .map(ServerRow::id)
                .collect()
        },
        |done, total| {
            if live && total > 0 {
                let mut err = std::io::stderr().lock();
                let _ = if done == 0 {
                    write!(err, "Checking {total} server{}…", plural(total))
                } else {
                    write!(err, "\rChecked {done} of {total}…        ")
                };
                let _ = err.flush();
            }
        },
    );
    if live {
        // The progress line goes away once the answers are in, so only the table stays on screen.
        eprint!("\r{:40}\r", "");
    }
    let answers = match answers {
        Ok(answers) => answers,
        Err(problem) => {
            eprintln!("klefki-cli: {problem}");
            eprintln!("Name the Claude CLI with --claude PATH if it is installed somewhere else.");
            return PROBLEM;
        }
    };
    warn(&answers.snapshot);
    let rows = select(&answers.snapshot, names).unwrap_or_default();
    let rows: Vec<(&ServerRow, Outcome)> = rows
        .into_iter()
        .map(|row| {
            let outcome = if !to_check(row, names) {
                if row.scope == Scope::Project && row.enabled {
                    Outcome::ProjectServer
                } else {
                    Outcome::Off
                }
            } else {
                match answers.checked.get(&row.connection) {
                    Some(c) => Outcome::Checked(c.clone()),
                    None => Outcome::NoAnswer,
                }
            };
            (row, outcome)
        })
        .collect();
    let problem = rows.iter().any(|(_, o)| o.is_problem());
    let text = if json {
        render::status_json(&answers.snapshot, answers.cli_version.as_deref(), &rows)
    } else {
        render::status_text(&rows)
    };
    match out(&text) {
        OK if problem => PROBLEM,
        code => code,
    }
}

/// Whether `status` checks this row: a server named on the command line always; otherwise one that is on and not a
/// project server (checking that runs the repository's own command, so only on request, as in the app).
fn to_check(row: &ServerRow, names: &[String]) -> bool {
    !names.is_empty() || (row.enabled && row.scope != Scope::Project)
}

/// The rows named (all when none are), in the order they are shown. A name that matches nothing is an error that
/// points at a name differing only in case.
fn select<'a>(snapshot: &'a Snapshot, names: &[String]) -> Result<Vec<&'a ServerRow>, String> {
    let mut missing = Vec::new();
    for name in names {
        if !snapshot.servers.iter().any(|r| r.name == *name) {
            let close = snapshot
                .servers
                .iter()
                .find(|r| r.name.to_lowercase() == name.to_lowercase());
            missing.push(match close {
                Some(r) => format!("no server is named {name:?}; did you mean {:?}?", r.name),
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
    eprintln!("Usage: klefki-cli list|status [NAME...] [--json]. See `klefki-cli help`.");
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
