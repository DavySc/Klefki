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
    Help,
    Version,
}

/// What was wrong with the command line, said as a sentence.
#[derive(Debug, PartialEq, Eq)]
pub struct Usage(pub String);

pub const HELP: &str = "\
klefki-cli: list Claude Code's MCP servers and check whether they connect.

Usage:
  klefki-cli list   [NAME...] [--json]
  klefki-cli status [NAME...] [--json] [--claude PATH]
  klefki-cli help
  klefki-cli --version

Commands:
  list     Show the configured servers, grouped by scope and project. Reads the
           configuration only; runs nothing.
  status   Ask Claude Code whether each server connects. Checks the user and local
           servers that are on. Project (.mcp.json) servers run the repository's own
           command, so they are checked only when named.

Options:
  NAME           Only these servers (by name, in every scope).
  --json         Print JSON for scripts instead of the table.
  --claude PATH  The Claude CLI to use (default: found like the Klefki app does).
  -h, --help     Show this help.
  -V, --version  Show the version.

Exit status:
  0  Done; for status, every server checked is connected.
  1  status found a server that is not connected, or could not run Claude Code.
  2  The command line was wrong, or a NAME matches no server.

Reads CLAUDE_CONFIG_DIR like Claude Code does.
";

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cmd, Usage> {
    let mut args = args.into_iter();
    let command = match args.next() {
        None => return Err(Usage("no command given".to_owned())),
        Some(c) => text(c)?,
    };
    let status = match command.as_str() {
        "list" => false,
        "status" => true,
        "help" | "-h" | "--help" => return Ok(Cmd::Help),
        "-V" | "--version" => return Ok(Cmd::Version),
        other if other.starts_with('-') => return Err(Usage(format!("unknown option {other}"))),
        other => return Err(Usage(format!("unknown command {other:?}"))),
    };
    let mut names = Vec::new();
    let mut json = false;
    let mut claude = None;
    let mut only_names = false;
    while let Some(arg) = args.next() {
        // A path may not be text the platform can show; it is kept as given.
        if !only_names
            && status
            && let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--claude="))
        {
            claude = Some(claude_path(Some(path.into()))?);
            continue;
        }
        let arg = text(arg)?;
        match arg.as_str() {
            _ if only_names => names.push(arg),
            "--" => only_names = true,
            "--json" => json = true,
            "-h" | "--help" => return Ok(Cmd::Help),
            "--claude" if status => claude = Some(claude_path(args.next())?),
            "--claude" => {
                return Err(Usage(
                    "--claude is only for status: list starts no program".to_owned(),
                ));
            }
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(Usage(format!("unknown option {option}")));
            }
            _ => names.push(arg),
        }
    }
    Ok(if status {
        Cmd::Status {
            names,
            json,
            claude,
        }
    } else {
        Cmd::List { names, json }
    })
}

fn claude_path(value: Option<OsString>) -> Result<PathBuf, Usage> {
    match value {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Err(Usage(
            "--claude needs the path of the Claude CLI".to_owned(),
        )),
    }
}

fn text(arg: OsString) -> Result<String, Usage> {
    arg.into_string()
        .map_err(|a| Usage(format!("{} is not valid text", a.to_string_lossy())))
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
