//! The command line, read by hand: two commands and two options do not need a parser library (start-up time and
//! size are product requirements).
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    List {
        names: Vec<String>,
        json: bool,
    },
    Status {
        names: Vec<String>,
        json: bool,
        claude: Option<PathBuf>,
    },
    /// The definitions, as one `{"mcpServers": {…}}` block with secrets replaced by `${VAR}` references.
    Export {
        names: Vec<String>,
        /// Write the block to this file instead of stdout.
        output: Option<PathBuf>,
        /// Replace the file if it is there.
        force: bool,
    },
    Help,
    Version,
}

/// What was wrong with the command line.
#[derive(Debug, PartialEq, Eq)]
pub enum Usage {
    /// Nothing at all: the help is the answer.
    NoCommand,
    /// Said as a sentence.
    Bad(String),
}

fn bad(why: String) -> Usage {
    Usage::Bad(why)
}

pub const HELP: &str = "\
klefki-cli: list Claude Code's MCP servers, check whether they connect, export them.

Examples:
  klefki-cli status                 check every server that is on
  klefki-cli status docs            check one server, a project server too
  klefki-cli status --json | jq '.servers[] | select(.status == \"failed\") | .name'
  klefki-cli export github -o servers.json

Usage:
  klefki-cli list   [NAME...] [--json]
  klefki-cli status [NAME...] [--json] [--claude PATH]
  klefki-cli export [NAME...] [-o FILE [--force]]
  klefki-cli help
  klefki-cli --version

Commands:
  list     Show the configured servers, grouped by scope and project. Reads the
           configuration only; runs nothing.
  status   Ask Claude Code whether each server connects. Checks the user and local
           servers that are on. Project (.mcp.json) servers run the repository's own
           command, so they are checked only when named.
  export   Print the servers' definitions as one {\"mcpServers\": {...}} block, the
           shape a .mcp.json has and Klefki's Add reads. Every environment and
           header value, and anything else that looks like a secret, is written
           as a ${VAR} reference; stderr lists the variables to set. Reads the
           configuration only and runs nothing; writes only the --output file.

Options:
  NAME           Only these servers (by name, in every scope).
  --json         Print JSON for scripts instead of the table (list, status).
  --claude PATH  The Claude CLI to use (default: found like the Klefki app does).
  -o, --output FILE
                 Write the export to FILE instead of stdout. An existing file
                 is kept unless you confirm, or add --force. Claude Code's own
                 files and .mcp.json files are never written.
  -h, --help     Show this help.
  -V, --version  Show the version.

  Servers that are off, or rejected for their project, are never checked.

Exit status:
  0  Done; for status, every server checked is connected; for export, every
     server named is in the block.
  1  status found a server that is not connected; export left one out.
  2  The command line was wrong, or a NAME matches no server.
  3  Claude Code could not be used: not found, or no check got an answer.
  4  export could not write the --output file (it is there, or not allowed).

Reads CLAUDE_CONFIG_DIR like Claude Code does.
";

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cmd, Usage> {
    let mut args = args.into_iter();
    let command = match args.next() {
        None => return Err(Usage::NoCommand),
        Some(c) => text(c)?,
    };
    let kind = match command.as_str() {
        "list" => Kind::List,
        "status" => Kind::Status,
        "export" => Kind::Export,
        "help" | "-h" | "--help" => return Ok(Cmd::Help),
        "-V" | "--version" => return Ok(Cmd::Version),
        other if other.starts_with('-') => return Err(bad(format!("unknown option {other}"))),
        other => {
            return Err(bad(
                match crate::suggest::closest(other, ["list", "status", "export", "help"]) {
                    Some(meant) => format!("unknown command {other:?}; did you mean {meant:?}?"),
                    None => format!("unknown command {other:?}"),
                },
            ));
        }
    };
    let status = kind == Kind::Status;
    let mut names = Vec::new();
    let mut json = false;
    let mut claude = None;
    let mut only_names = false;
    let mut output = None;
    let mut force = false;
    let export = kind == Kind::Export;
    while let Some(arg) = args.next() {
        // A path may not be text the platform can show; it is kept as given.
        if !only_names
            && status
            && let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--claude="))
        {
            claude = Some(claude_path(Some(path.into()))?);
            continue;
        }
        if !only_names
            && export
            && let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--output="))
        {
            set_output(&mut output, Some(path.into()))?;
            continue;
        }
        let arg = text(arg)?;
        match arg.as_str() {
            _ if only_names => names.push(arg),
            "--" => only_names = true,
            "--json" if kind == Kind::Export => {
                return Err(bad("export always prints JSON; leave out --json".to_owned()));
            }
            "--json" => json = true,
            "-h" | "--help" => return Ok(Cmd::Help),
            "--claude" if status => claude = Some(claude_path(args.next())?),
            "--claude" => {
                return Err(bad(format!(
                    "--claude is only for status: {command} starts no program"
                )));
            }
            "-o" | "--output" if export => set_output(&mut output, args.next())?,
            "--force" if export => force = true,
            "-o" | "--output" | "--force" => {
                let option = if arg == "--force" {
                    "--force"
                } else {
                    "--output"
                };
                return Err(bad(format!(
                    "{option} is only for export: {command} writes nothing"
                )));
            }
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(bad(format!("unknown option {option}")));
            }
            _ => names.push(arg),
        }
    }
    Ok(match kind {
        Kind::Status => Cmd::Status {
            names,
            json,
            claude,
        },
        Kind::List => Cmd::List { names, json },
        Kind::Export if force && output.is_none() => {
            return Err(bad(
                "--force is only with --output: it lets the file be replaced".to_owned(),
            ));
        }
        Kind::Export => Cmd::Export {
            names,
            output,
            force,
        },
    })
}

#[derive(PartialEq, Eq)]
enum Kind {
    List,
    Status,
    Export,
}

fn set_output(output: &mut Option<PathBuf>, value: Option<OsString>) -> Result<(), Usage> {
    match value {
        _ if output.is_some() => Err(bad("--output is given once: one file".to_owned())),
        Some(path) if !path.is_empty() => {
            *output = Some(PathBuf::from(path));
            Ok(())
        }
        _ => Err(bad(
            "--output needs the path of the file to write".to_owned()
        )),
    }
}

fn claude_path(value: Option<OsString>) -> Result<PathBuf, Usage> {
    match value {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Err(bad("--claude needs the path of the Claude CLI".to_owned())),
    }
}

fn text(arg: OsString) -> Result<String, Usage> {
    arg.into_string()
        .map_err(|a| bad(format!("{} is not valid text", a.to_string_lossy())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cmd, Usage> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn commands_and_options() {
        assert_eq!(
            p(&["list"]),
            Ok(Cmd::List {
                names: vec![],
                json: false
            })
        );
        assert_eq!(
            p(&["list", "a", "--json", "b"]),
            Ok(Cmd::List {
                names: vec!["a".into(), "b".into()],
                json: true
            })
        );
        assert_eq!(
            p(&["status", "--claude", "/x/claude", "a"]),
            Ok(Cmd::Status {
                names: vec!["a".into()],
                json: false,
                claude: Some("/x/claude".into())
            })
        );
        assert_eq!(
            p(&["status", "--claude=/x/claude"]),
            Ok(Cmd::Status {
                names: vec![],
                json: false,
                claude: Some("/x/claude".into())
            })
        );
        for help in [&["help"][..], &["-h"], &["--help"], &["list", "--help"]] {
            assert_eq!(p(help), Ok(Cmd::Help), "{help:?}");
        }
        assert_eq!(p(&["-V"]), Ok(Cmd::Version));
    }

    #[test]
    fn export_takes_an_output_file() {
        let out = |path: &str, force| Cmd::Export {
            names: vec!["a".into()],
            output: Some(PathBuf::from(path)),
            force,
        };
        assert_eq!(
            p(&["export", "a", "-o", "x.json"]),
            Ok(out("x.json", false))
        );
        assert_eq!(
            p(&["export", "--output", "x.json", "a"]),
            Ok(out("x.json", false))
        );
        assert_eq!(
            p(&["export", "--output=x.json", "a", "--force"]),
            Ok(out("x.json", true))
        );
        let refused = |args: &[&str]| match p(args) {
            Err(Usage::Bad(why)) => why,
            other => panic!("{other:?}"),
        };
        assert!(refused(&["export", "-o"]).contains("--output needs"));
        assert!(refused(&["export", "--output="]).contains("--output needs"));
        assert!(refused(&["export", "--force"]).contains("--force is only with --output"));
        assert!(refused(&["list", "-o", "x"]).contains("--output is only for export"));
        assert!(refused(&["export", "-o", "a", "-o", "b"]).contains("once"));
    }

    #[test]
    fn export_takes_names_only() {
        assert_eq!(
            p(&["export", "a", "b"]),
            Ok(Cmd::Export {
                names: vec!["a".into(), "b".into()],
                output: None,
                force: false,
            })
        );
        assert_eq!(
            p(&["export"]),
            Ok(Cmd::Export {
                names: vec![],
                output: None,
                force: false,
            })
        );
        let refused = |args: &[&str]| match p(args) {
            Err(Usage::Bad(why)) => why,
            other => panic!("{args:?} gave {other:?}"),
        };
        assert!(refused(&["export", "--json"]).contains("always prints JSON"));
        assert!(refused(&["export", "--claude", "/x"]).contains("export starts no program"));
        assert!(refused(&["exprot"]).contains("did you mean \"export\""));
    }

    #[test]
    fn names_after_a_double_dash_may_look_like_options() {
        assert_eq!(
            p(&["list", "--", "--json", "-x"]),
            Ok(Cmd::List {
                names: vec!["--json".into(), "-x".into()],
                json: false
            })
        );
        // A server may be called `-`: on its own it is a name, not an option.
        assert_eq!(
            p(&["list", "-"]),
            Ok(Cmd::List {
                names: vec!["-".into()],
                json: false
            })
        );
    }

    #[test]
    fn mistakes_are_usage_errors() {
        for bad in [
            &[][..],
            &["lst"],
            &["--json"],
            &["list", "--frob"],
            &["list", "--claude", "x"],
            &["status", "--claude"],
            &["status", "--claude="],
            &["status", "--claude", ""],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
    }
}
