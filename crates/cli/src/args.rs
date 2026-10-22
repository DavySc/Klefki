//! The command line, read by hand: two commands and two options do not need a parser library (start-up time and
//! size are product requirements).
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Cmd {
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
    /// The profiles (decision 012): their servers, and where each is on alone.
    Profiles {
        json: bool,
    },
    /// Use a profile: the change is shown, and saved once confirmed.
    Use {
        profile: String,
        place: Place,
        /// Turn every other server there off.
        only: bool,
        /// Save without asking.
        yes: bool,
    },
    /// Stop using a profile: what it switched is put back, once confirmed.
    Stop {
        profile: String,
        place: Place,
        yes: bool,
    },
    /// Stop the profiles in use in one project and use this one there instead, once confirmed.
    Switch {
        profile: String,
        /// [`Place::Here`] or [`Place::Project`]: a switch is in one project.
        place: Place,
        only: bool,
        yes: bool,
    },
    Help,
    Version,
}

/// Where `use`, `stop` or `switch` acts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// The project of the current folder.
    Here,
    Project(PathBuf),
    /// Every project Claude Code knows.
    Every,
}

/// What was wrong with the command line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Usage {
    /// Nothing at all: the help is the answer.
    NoCommand,
    /// Said as a sentence.
    Bad(String),
}

fn bad(why: String) -> Usage {
    Usage::Bad(why)
}

pub(crate) const HELP: &str = "\
klefki-cli: list Claude Code's MCP servers, check whether they connect, export them,
and use Klefki's profiles.

Examples:
  klefki-cli status                 check every server that is on
  klefki-cli status docs            check one server, a project server too
  klefki-cli status --json | jq '.servers[] | select(.status == \"failed\") | .name'
  klefki-cli export github -o servers.json
  klefki-cli use Writing --only     only the Writing profile's servers in this project
  klefki-cli switch Code --only     from the profile in use here to Code

Usage:
  klefki-cli list   [NAME...] [--json]
  klefki-cli status [NAME...] [--json] [--claude PATH]
  klefki-cli export [NAME...] [-o FILE [--force]]
  klefki-cli profiles [--json]
  klefki-cli use PROFILE [--project DIR | --all] [--only] [--yes]
  klefki-cli switch PROFILE [--project DIR] [--only] [--yes]
  klefki-cli stop PROFILE [--project DIR | --all] [--yes]
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
  profiles List the profiles made in Klefki: their servers, the projects where
           each is in use, and those where its servers are the only ones on.
           Reads only.
  use      Turn a profile's servers on in a project: the one of the current
           folder, the --project folder, or --all projects Claude Code knows.
           Shows the change first and saves it once you say yes (or --yes),
           keeping a backup, as the app does.
  switch   Stop using the profiles in use in a project (the current folder's,
           or --project) and use this one there instead, as one change. Stopping
           it later puts back what was there before all of them.
  stop     Stop using a profile: put back what it switched, here, in the
           --project folder, or --all projects, unless switched again since.

Options:
  NAME           Only these servers (by name, in every scope).
  --json         Print JSON for scripts instead of the table (list, status).
  --claude PATH  The Claude CLI to use (default: found like the Klefki app does).
  -o, --output FILE
                 Write the export to FILE instead of stdout. An existing file
                 is kept unless you confirm, or add --force. Claude Code's own
                 files and .mcp.json files are never written.
  --project DIR  use, switch, stop: the project (default: this folder's; in a
                 git worktree, its main checkout's).
  --all          use, stop: every project Claude Code knows today.
  --only         use, switch: turn every other server there off.
  -y, --yes      use, switch, stop: save without asking.
  -h, --help     Show this help.
  -V, --version  Show the version.

  Servers that are off, or rejected for their project, are never checked.

Exit status:
  0  Done; for status, every server checked is connected; for export, every
     server named is in the block.
  1  status found a server that is not connected, or a NAME that Claude Code
     does not run (off, rejected, not loaded); export left one out; use,
     switch or stop could not make or save the change.
  2  The command line was wrong, a NAME matches no server, or use, switch or
     stop names a profile or folder that is not there.
  3  Claude Code could not be used: not found, or no check got an answer.
  4  export could not write the --output file (it is there, or not allowed);
     use, switch or stop saved nothing: not confirmed, or nobody at a
     terminal and no --yes.
  5  status was given a NAME that Klefki does not check (a plugin's, the
     organisation's, a claude.ai connector), so nothing is known about it.
     Like any code but 0, `&&` and `set -e` treat it as a failure.

Reads CLAUDE_CONFIG_DIR like Claude Code does.
";

pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cmd, Usage> {
    let mut args = args.into_iter();
    let command = match args.next() {
        None => return Err(Usage::NoCommand),
        Some(c) => text(c)?,
    };
    let kind = match command.as_str() {
        "list" => Kind::List,
        "status" => Kind::Status,
        "export" => Kind::Export,
        "profiles" => Kind::Profiles,
        "use" => Kind::Use,
        "switch" => Kind::Switch,
        "stop" => Kind::Stop,
        "help" | "-h" | "--help" => return Ok(Cmd::Help),
        "-V" | "--version" => return Ok(Cmd::Version),
        other if other.starts_with('-') => return Err(bad(format!("unknown option {other}"))),
        other => {
            return Err(bad(
                match crate::suggest::closest(
                    other,
                    [
                        "list", "status", "export", "profiles", "use", "switch", "stop", "help",
                    ],
                ) {
                    Some(meant) => format!("unknown command {other:?}; did you mean {meant:?}?"),
                    None => format!("unknown command {other:?}"),
                },
            ));
        }
    };
    match Options::read(kind, &command, args)? {
        // `-h` or `--help`: the help is the answer, whatever else is on the line after it.
        None => Ok(Cmd::Help),
        Some(options) => options.into_cmd(kind),
    }
}

/// What follows the command on the line, before it is checked against the command.
#[derive(Default)]
struct Options {
    names: Vec<String>,
    json: bool,
    claude: Option<PathBuf>,
    output: Option<PathBuf>,
    force: bool,
    project: Option<PathBuf>,
    every: bool,
    only: bool,
    yes: bool,
}

impl Options {
    /// Reads the options and names after `command`; an option the command does not take is refused here. `None`
    /// when the help is asked for.
    fn read(
        kind: Kind,
        command: &str,
        mut args: impl Iterator<Item = OsString>,
    ) -> Result<Option<Self>, Usage> {
        let status = kind == Kind::Status;
        let export = kind == Kind::Export;
        let using = matches!(kind, Kind::Use | Kind::Switch | Kind::Stop);
        let mut o = Self::default();
        let mut only_names = false;
        while let Some(arg) = args.next() {
            // A path may not be text the platform can show; it is kept as given.
            if !only_names
                && status
                && let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--claude="))
            {
                o.claude = Some(claude_path(Some(path.into()))?);
                continue;
            }
            if !only_names
                && export
                && let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--output="))
            {
                set_output(&mut o.output, Some(path.into()))?;
                continue;
            }
            if !only_names
                && using
                && let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--project="))
            {
                set_project(&mut o.project, Some(path.into()))?;
                continue;
            }
            let arg = text(arg)?;
            match arg.as_str() {
                _ if only_names => o.names.push(arg),
                "--" => only_names = true,
                "--json" if kind == Kind::Export => {
                    return Err(bad("export always prints JSON; leave out --json".to_owned()));
                }
                "--json" if using => {
                    return Err(bad(format!(
                        "{command} prints the change for people; leave out --json"
                    )));
                }
                "--json" => o.json = true,
                "--project" if using => set_project(&mut o.project, args.next())?,
                "--all" if kind == Kind::Switch => {
                    return Err(bad(
                        "switch works in one project: leave out --all, or use --project".to_owned(),
                    ));
                }
                "--only" if kind == Kind::Stop => {
                    return Err(bad(
                        "stop puts back what the profile switched; --only is for use and switch"
                            .to_owned(),
                    ));
                }
                "--all" if using => o.every = true,
                "--only" if using => o.only = true,
                "-y" | "--yes" if using => o.yes = true,
                "--project" | "--all" | "--only" | "-y" | "--yes" => {
                    return Err(bad(format!(
                        "{arg} is only for use, switch and stop: {command} changes no project"
                    )));
                }
                "-h" | "--help" => return Ok(None),
                "--claude" if status => o.claude = Some(claude_path(args.next())?),
                "--claude" => {
                    return Err(bad(format!(
                        "--claude is only for status: {command} starts no program"
                    )));
                }
                "-o" | "--output" if export => set_output(&mut o.output, args.next())?,
                "--force" if export => o.force = true,
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
                _ => o.names.push(arg),
            }
        }
        Ok(Some(o))
    }

    /// The command, once what was read fits together.
    fn into_cmd(self, kind: Kind) -> Result<Cmd, Usage> {
        let Self {
            names,
            json,
            claude,
            output,
            force,
            project,
            every,
            only,
            yes,
        } = self;
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
            Kind::Profiles if !names.is_empty() => {
                return Err(bad(
                    "profiles takes no names: it lists every profile".to_owned()
                ));
            }
            Kind::Profiles => Cmd::Profiles { json },
            Kind::Use | Kind::Switch | Kind::Stop => {
                profile_cmd(kind, names, project, every, only, yes)?
            }
        })
    }
}

/// `use`, `switch` or `stop`: one profile, and one place.
fn profile_cmd(
    kind: Kind,
    names: Vec<String>,
    project: Option<PathBuf>,
    every: bool,
    only: bool,
    yes: bool,
) -> Result<Cmd, Usage> {
    let profile = match <[String; 1]>::try_from(names) {
        Ok([profile]) => profile,
        Err(names) if names.is_empty() => {
            return Err(bad(format!(
                "{} needs the name of a profile; `klefki-cli profiles` lists them",
                kind.word()
            )));
        }
        Err(_) => {
            return Err(bad(format!(
                "{} takes one profile; put a name with spaces in quotes",
                kind.word()
            )));
        }
    };
    let place = match (project, every) {
        (Some(_), true) => {
            return Err(bad(
                "--project or --all, not both: one project, or every one".to_owned(),
            ));
        }
        (Some(path), false) => Place::Project(path),
        (None, true) => Place::Every,
        (None, false) => Place::Here,
    };
    Ok(match kind {
        Kind::Switch => Cmd::Switch {
            profile,
            place,
            only,
            yes,
        },
        Kind::Stop => Cmd::Stop {
            profile,
            place,
            yes,
        },
        _ => Cmd::Use {
            profile,
            place,
            only,
            yes,
        },
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    List,
    Status,
    Export,
    Profiles,
    Use,
    Switch,
    Stop,
}

impl Kind {
    /// The command as typed, for what is said about it.
    fn word(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Status => "status",
            Self::Export => "export",
            Self::Profiles => "profiles",
            Self::Use => "use",
            Self::Switch => "switch",
            Self::Stop => "stop",
        }
    }
}

fn set_project(project: &mut Option<PathBuf>, value: Option<OsString>) -> Result<(), Usage> {
    match value {
        _ if project.is_some() => Err(bad("--project is given once: one project".to_owned())),
        Some(path) if !path.is_empty() => {
            *project = Some(PathBuf::from(path));
            Ok(())
        }
        _ => Err(bad(
            "--project needs the path of a project folder".to_owned()
        )),
    }
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
    fn profiles_lists_and_use_takes_one_profile_and_where() {
        assert_eq!(p(&["profiles"]), Ok(Cmd::Profiles { json: false }));
        assert_eq!(p(&["profiles", "--json"]), Ok(Cmd::Profiles { json: true }));
        let used = |place, only, yes| Cmd::Use {
            profile: "Work".into(),
            place,
            only,
            yes,
        };
        assert_eq!(p(&["use", "Work"]), Ok(used(Place::Here, false, false)));
        assert_eq!(
            p(&["use", "--project", "/w/app", "Work", "--only"]),
            Ok(used(Place::Project("/w/app".into()), true, false))
        );
        assert_eq!(
            p(&["use", "Work", "--project=/w/app", "-y"]),
            Ok(used(Place::Project("/w/app".into()), false, true))
        );
        assert_eq!(
            p(&["use", "--all", "--yes", "--", "--Work"]),
            Ok(Cmd::Use {
                profile: "--Work".into(),
                place: Place::Every,
                only: false,
                yes: true,
            })
        );
        let refused = |args: &[&str]| match p(args) {
            Err(Usage::Bad(why)) => why,
            other => panic!("{args:?} gave {other:?}"),
        };
        assert!(refused(&["use"]).contains("use needs the name of a profile"));
        assert!(refused(&["use", "A", "B"]).contains("one profile"));
        assert!(refused(&["use", "A", "--all", "--project", "/x"]).contains("not both"));
        assert!(refused(&["use", "A", "--project"]).contains("--project needs"));
        assert!(refused(&["use", "A", "--project", "/x", "--project", "/y"]).contains("once"));
        assert!(refused(&["use", "A", "--json"]).contains("--json"));
        assert!(refused(&["profiles", "Work"]).contains("profiles takes no names"));
        for option in ["--only", "--all", "--yes", "--project"] {
            assert!(
                refused(&["list", option, "x"]).contains("only for use"),
                "{option}"
            );
        }
        assert!(refused(&["prfiles"]).contains("did you mean \"profiles\""));
    }

    #[test]
    fn switch_and_stop_take_one_profile_and_where() {
        assert_eq!(
            p(&["switch", "Code", "--only"]),
            Ok(Cmd::Switch {
                profile: "Code".into(),
                place: Place::Here,
                only: true,
                yes: false,
            })
        );
        assert_eq!(
            p(&["switch", "Code", "--project=/w/app", "-y"]),
            Ok(Cmd::Switch {
                profile: "Code".into(),
                place: Place::Project("/w/app".into()),
                only: false,
                yes: true,
            })
        );
        assert_eq!(
            p(&["stop", "Code"]),
            Ok(Cmd::Stop {
                profile: "Code".into(),
                place: Place::Here,
                yes: false,
            })
        );
        assert_eq!(
            p(&["stop", "Code", "--all", "--yes"]),
            Ok(Cmd::Stop {
                profile: "Code".into(),
                place: Place::Every,
                yes: true,
            })
        );
        let refused = |args: &[&str]| match p(args) {
            Err(Usage::Bad(why)) => why,
            other => panic!("{args:?} gave {other:?}"),
        };
        assert!(refused(&["switch"]).contains("switch needs the name of a profile"));
        assert!(refused(&["stop", "A", "B"]).contains("stop takes one profile"));
        assert!(refused(&["switch", "A", "--all"]).contains("one project"));
        assert!(refused(&["stop", "A", "--only"]).contains("--only is for use and switch"));
        assert!(refused(&["stop", "A", "--json"]).contains("stop prints the change"));
        assert!(refused(&["swtich"]).contains("did you mean \"switch\""));
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

    #[test]
    fn an_option_where_it_does_not_belong_is_named_in_the_refusal() {
        let refused = |args: &[&str], why: &str| {
            assert_eq!(p(args), Err(Usage::Bad(why.to_owned())), "{args:?}");
        };
        refused(&["-x"], "unknown option -x");
        refused(
            &["list", "--output=x.json"],
            "unknown option --output=x.json",
        );
        refused(
            &["list", "--force"],
            "--force is only for export: list writes nothing",
        );
        refused(
            &["use", "Writing", "--project="],
            "--project needs the path of a project folder",
        );
    }
}
