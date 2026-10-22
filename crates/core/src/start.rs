//! Starting Claude Code with only a profile's servers (on/off plan S5): `claude --settings` with an
//! `allowedMcpServers` list of their names. Nothing is written, so nothing goes stale, nothing needs putting back, and
//! sessions side by side can each have their own set.
//!
//! Claude Code adds that list to the `allowedMcpServers` of every settings file (the user's, the project's
//! `.claude/settings.json` and `settings.local.json`, the organisation's), a denied rule anywhere still wins, the
//! organisation's own servers need no rule, and with `allowManagedMcpServersOnly` the list counts for nothing (all
//! checked with Claude Code 2.1.308). The list also turns on no server switched off in the project. So the plan reads
//! the files as such a session would and says, by name, each server of the profile that would still not load and
//! each other server that would load too.
use crate::api::{Here, Scope, ServerRow};
use crate::config::{ConfigPaths, inventory, policy};
use crate::model::ConnectionId;
use crate::sanitize::sanitize;
use crate::words::and_list;

/// The program the line starts: Claude Code as its installer puts it on the PATH.
const PROGRAM: &str = "claude";

/// What starting Claude Code with a profile's servers does in one project. Text is sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPlan {
    /// The profile's name.
    pub profile: String,
    /// The project, as [`crate::api::Snapshot::projects`] spells it: the folder to start Claude Code in.
    pub project: String,
    /// The arguments for Claude Code: `--settings` and its JSON. Every name in it has only letters, digits, `-` and
    /// `_`.
    pub args: Vec<String>,
    /// One line to paste in a terminal, on this system's rules for quoting.
    pub line: String,
    /// What to do for the line to work, each a sentence.
    pub to_do: Vec<String>,
    /// The servers of the profile that load, by name, in the profile's order.
    pub loads: Vec<String>,
    /// Each server of the profile that does not load, and why, as a sentence.
    pub missing: Vec<String>,
    /// Each other server that loads too, and why, as a sentence.
    pub extra: Vec<String>,
}

impl StartPlan {
    /// Claude Code loads the profile's servers there, and only those.
    pub fn exact(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty()
    }
}

/// Whether Claude Code takes `name` in `allowedMcpServers`: letters, digits, `-` and `_` only (decision 010), and no
/// longer than a name Klefki gives Claude Code anywhere.
fn nameable(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The plan for starting Claude Code in `project` with the servers `members` of the profile `profile`, from the files
/// in `paths` as they are now. `windows` quotes the line for PowerShell, otherwise for a POSIX shell. `Err` says why
/// there is no line.
pub(crate) fn plan(
    paths: &ConfigPaths,
    profile: &str,
    members: &[ConnectionId],
    project: &str,
    windows: bool,
) -> Result<StartPlan, String> {
    let now = inventory::load(paths).snapshot;
    let Some(project) = now
        .projects
        .iter()
        .find(|p| crate::api::same_project(p, project))
        .cloned()
    else {
        return Err(format!(
            "Claude Code does not know {} as a project: start Claude Code there once.",
            sanitize(project)
        ));
    };
    let place = crate::ops::project_label(&project, &now);
    let shown_profile = sanitize(profile);

    // The names to start with, and what is known before reading the files as the session would.
    let mut names: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut organisation = false;
    let mut gone = 0;
    let rows: Vec<&ServerRow> = members
        .iter()
        .filter_map(|id| {
            let row = now.servers.iter().find(|r| r.connection == *id);
            gone += usize::from(row.is_none());
            row
        })
        .collect();
    for row in &rows {
        let Some((scope, own, name)) = ConnectionId::parse(row.connection.as_str()) else {
            continue;
        };
        match scope {
            Scope::Local | Scope::Project if own.as_deref() != Some(project.as_str()) => {
                let theirs = own.map_or_else(String::new, |o| crate::ops::project_label(&o, &now));
                missing.push(format!(
                    "{} is a server of {theirs}, and Claude Code loads it only there.",
                    row.name
                ));
            }
            Scope::Managed => organisation = true,
            Scope::Plugin => missing.push(format!(
                "{} comes from a plugin, and allowedMcpServers can let a plugin's server in by its command only, \
                 which the start command does not hold: it does not load.",
                row.name
            )),
            Scope::ClaudeAi => missing.push(format!(
                "{} is a claude.ai connector, and allowedMcpServers can let a connector in by its address only, \
                 which Klefki does not know: it does not load.",
                row.name
            )),
            _ if !nameable(&name) => missing.push(format!(
                "{}: allowedMcpServers takes only names of letters, digits, - and _, so it does not load. Rename \
                 it to start it this way.",
                row.name
            )),
            _ => {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    if gone > 0 {
        missing.push(if gone == 1 {
            format!("One server of {shown_profile} is no longer configured.")
        } else {
            format!("{gone} servers of {shown_profile} are no longer configured.")
        });
    }
    if names.is_empty() && !organisation {
        return Err(format!(
            "None of the servers of {shown_profile} can be named in a start command.{}",
            missing.iter().map(|m| format!(" {m}")).collect::<String>()
        ));
    }
    let rules = policy::load(paths, &[project.as_str()], Some(&names), &mut Vec::new());
    if rules.only_organisation {
        return Err(
            "Your organisation's settings let only its own list of allowed servers count (allowManagedMcpServersOnly), \
             so Claude Code would ignore the list a start command gives. Your administrator can change this."
                .to_owned(),
        );
    }
    let here_rules = rules.places.get(1).cloned().unwrap_or_default();
    let lists = here_rules.allowed_files();
    let (by_command, by_url) = here_rules.allowed_kinds();

    let started = inventory::load_started_with(paths, &names);
    let before = crate::ops::in_project(&now, &project);
    let after = crate::ops::in_project(&started, &project);
    let loads_in = |here: &Option<Here>| here.as_ref().is_some_and(|h| !h.off && h.held.is_none());
    let mut loads = Vec::new();
    for row in &rows {
        let Some(i) = now
            .servers
            .iter()
            .position(|r| r.connection == row.connection)
        else {
            continue;
        };
        let was = &before[i];
        let will = started
            .servers
            .iter()
            .position(|r| r.connection == row.connection)
            .map(|j| &after[j]);
        let Some(was_here) = was else {
            // Of another project: said above.
            continue;
        };
        if will.is_some_and(loads_in) {
            loads.push(row.name.clone());
            continue;
        }
        if matches!(row.scope, Scope::Plugin | Scope::ClaudeAi) {
            continue;
        }
        let Some((_, _, name)) = ConnectionId::parse(row.connection.as_str()) else {
            continue;
        };
        if !nameable(&name) && row.scope != Scope::Managed {
            continue;
        }
        let why = if was_here.off {
            format!(
                "{} is off in {place}, and a start command turns no server on: turn it on there first.",
                row.name
            )
        } else if let Some(held) = &was_here.held {
            format!("{}: {held}", row.name)
        } else {
            let remote = row.transport != crate::api::TransportKind::Stdio;
            match (remote, by_command, by_url) {
                (false, true, _) => format!(
                    "{}: allowedMcpServers in {} lets local servers in by their command only, so its name in the \
                     start command does not count.",
                    row.name,
                    and_list(&lists)
                ),
                (true, _, true) => format!(
                    "{}: allowedMcpServers in {} lets remote servers in by their address only, so its name in the \
                     start command does not count.",
                    row.name,
                    and_list(&lists)
                ),
                _ => match will.and_then(|w| w.as_ref()).and_then(|w| w.held.clone()) {
                    Some(held) => format!("{}: {held}", row.name),
                    None => format!("{} does not load.", row.name),
                },
            }
        };
        missing.push(why);
    }

    let mut extra = Vec::new();
    for (j, row) in started.servers.iter().enumerate() {
        if members.contains(&row.connection) || !loads_in(&after[j]) {
            continue;
        }
        let same_name = ConnectionId::parse(row.connection.as_str())
            .is_some_and(|(_, _, name)| names.contains(&name));
        extra.push(if same_name {
            format!(
                "{}: this project's own server of that name loads, as Claude Code prefers it to the profile's.",
                row.name
            )
        } else if row.scope == Scope::Managed {
            format!(
                "{}: your organisation's servers load without being on an allowed list.",
                row.name
            )
        } else if row.scope == Scope::ClaudeAi {
            format!(
                "{}: it may load too, if its address matches allowedMcpServers in {}.",
                row.name,
                and_list(&lists)
            )
        } else if lists.is_empty() {
            format!("{} loads too.", row.name)
        } else {
            format!(
                "{}: allowedMcpServers in {} lets it load too.",
                row.name,
                and_list(&lists)
            )
        });
    }

    let settings = serde_json::json!({
        "allowedMcpServers": names
            .iter()
            .map(|n| serde_json::json!({ "serverName": n }))
            .collect::<Vec<_>>()
    })
    .to_string();
    let (line, to_do) = if windows {
        // PowerShell passes what follows `--%` on as written, so Claude Code reads the quotes as the JSON's own
        // (Windows PowerShell 5.1 and PowerShell 7 alike), but only to a program: npm's `claude.ps1` would get `--%`
        // itself, so the line names `claude.exe`, which PowerShell finds only as a program. A folder change on the
        // same line would not stop when it fails, so where to run it is said instead.
        (
            format!(
                "{PROGRAM}.exe --% --settings \"{}\"",
                settings.replace('"', "\\\"")
            ),
            vec![format!(
                "Run it in PowerShell, in {}.",
                sanitize(&project.replace('/', "\\"))
            )],
        )
    } else {
        let folder = crate::inspector::posix_word(&project)?;
        (
            format!("cd {folder} && {PROGRAM} --settings '{settings}'"),
            Vec::new(),
        )
    };
    Ok(StartPlan {
        profile: shown_profile,
        project,
        args: vec!["--settings".to_owned(), settings],
        line,
        to_do,
        loads,
        missing,
        extra,
    })
}

/// Why `run` starts nothing on this system: the line is for pasting instead.
pub const NOT_HERE: &str = "On Windows, klefki-cli does not start Claude Code itself yet: paste the line above in PowerShell.";

/// Starts Claude Code (`program`) in the plan's project with the plan's arguments, attached to this terminal. On Unix
/// this process becomes Claude Code, so it returns only when Claude Code could not be started; elsewhere it starts
/// nothing and says [`NOT_HERE`]. No shell: each argument is its own; the arguments are checked again here, as a
/// plan's fields can be set by anyone.
pub(crate) fn run(
    plan: &StartPlan,
    program: &std::path::Path,
) -> Result<std::convert::Infallible, String> {
    let [flag, settings] = plan.args.as_slice() else {
        return Err("the start command is not one Klefki made".to_owned());
    };
    let parsed = serde_json::from_str::<serde_json::Value>(settings).ok();
    let only_names = parsed
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .is_some_and(|obj| {
            obj.len() == 1
                && obj
                    .get("allowedMcpServers")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|list| {
                        list.iter().all(|r| {
                            r.as_object().is_some_and(|r| {
                                r.len() == 1
                                    && r.get("serverName")
                                        .and_then(serde_json::Value::as_str)
                                        .is_some_and(nameable)
                            })
                        })
                    })
        });
    if flag != "--settings" || !only_names {
        return Err("the start command is not one Klefki made".to_owned());
    }
    let folder = std::path::Path::new(&plan.project);
    if !inventory::is_safe_project_path(&plan.project) || !folder.is_dir() {
        return Err(format!(
            "{} is not a folder Klefki starts Claude Code in",
            sanitize(&plan.project)
        ));
    }
    crate::cli::locate::check(program).map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new(program);
    command
        .args(&plan.args)
        .current_dir(folder)
        // A secret in Klefki's own environment is not Claude Code's to have.
        .env_remove("MCP_CLIENT_SECRET");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let e = command.exec();
        Err(format!("Claude Code could not be started: {e}"))
    }
    // Waiting on Claude Code here would leave it running in the console when the first Ctrl+C, which reaches every
    // process there, ends Klefki (review of #106): not started until that is handled.
    #[cfg(not(unix))]
    {
        drop(command);
        Err(NOT_HERE.to_owned())
    }
}

#[cfg(test)]
mod tests;
