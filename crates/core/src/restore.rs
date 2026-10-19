//! Restoring Claude Code's servers from a backup Klefki kept ([`crate::api::Change::Restore`]).
//!
//! A backup is a whole copy of `.claude.json`, but Claude Code keeps much more in that file than servers: its own
//! settings, project history, trust answers. Writing the copy back would undo all of that, so a restore compares the
//! servers only. Each user and local server goes back to its definition in the backup, one that is gone is put back,
//! and the on/off switches of each project go back to what the backup has. Servers added since are left as they are
//! and named in the review: a restore never removes a server. Everything goes through the usual review and save, so
//! the configuration as it is now is backed up first and the restore can itself be undone from the activity log.
use crate::config::docstore::Op;
use crate::config::{entry, fsx, inventory, secretlike};
use crate::ops::{self, CliAdd, Resolved};
use crate::sanitize::sanitize;
use crate::swap::{self, Spot, Swap};
use serde_json::Value;
use std::path::Path;

/// The servers of one side (the backup or the file now), with each project once, however many ways it is spelled.
#[derive(Default)]
struct Servers {
    user: Vec<(String, Value)>,
    projects: Vec<Project>,
}

struct Project {
    /// As the file spells it (the first spelling, where there are several).
    key: String,
    normal: String,
    servers: Vec<(String, Value)>,
    /// `disabledMcpServers`: the names switched off there.
    off: Vec<String>,
}

impl Servers {
    fn read(root: &Value) -> Self {
        let mut out = Servers {
            user: entries(root.get("mcpServers")),
            projects: Vec::new(),
        };
        for (key, project) in root
            .get("projects")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let normal = inventory::normalise_project(key);
            let servers = entries(project.get("mcpServers"));
            let off: Vec<String> = project
                .get("disabledMcpServers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect();
            match out.projects.iter_mut().find(|p| p.normal == normal) {
                Some(known) => {
                    for (name, value) in servers {
                        if !known.servers.iter().any(|(n, _)| *n == name) {
                            known.servers.push((name, value));
                        }
                    }
                    for name in off {
                        if !known.off.contains(&name) {
                            known.off.push(name);
                        }
                    }
                }
                None => out.projects.push(Project {
                    key: key.clone(),
                    normal,
                    servers,
                    off,
                }),
            }
        }
        out
    }

    fn project(&self, normal: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.normal == normal)
    }
}

fn entries(list: Option<&Value>) -> Vec<(String, Value)> {
    list.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn find<'a>(list: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    list.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

/// What restoring the backup called `backup` would change in `.claude.json`, whose text now is `now`. A backup is
/// named by its file name (the activity log lists its whole path; the folder is always `backup_dir`), and only a
/// backup of `.claude.json` is restored.
pub(crate) fn resolve(backup_dir: &Path, backup: &str, now: &str) -> Result<Resolved, String> {
    let name = backup.rsplit(['/', '\\']).next().unwrap_or_default();
    let at = crate::activity::backup_time(name)
        .filter(|_| name.starts_with("claude.json.") && name.matches('.').count() == 3)
        .ok_or("Only a backup of .claude.json that Klefki kept can be restored.")?;
    let text = fsx::read_limited(&backup_dir.join(name), inventory::MAX_CLAUDE_JSON)
        .map_err(|e| {
            format!(
                "That backup could not be read: {}",
                sanitize(&e.to_string())
            )
        })?
        .ok_or("That backup is no longer kept: Klefki keeps the newest 20 copies of each file.")?;
    let parse = |text: &str, what: &str| -> Result<Value, String> {
        match serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) {
            Ok(root) if root.is_object() => Ok(root),
            Ok(_) => Err(format!("{what} is not a Claude Code configuration.")),
            Err(e) => Err(format!(
                "{what} is not valid JSON: {}",
                sanitize(&e.to_string())
            )),
        }
    };
    let backup = parse(&text, "That backup")?;
    let now = if now.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        parse(now, "The configuration")?
    };
    compare(&backup, &now, &crate::activity::moment(at), &|folder| {
        Path::new(folder).is_dir()
    })
}

/// What going back from `now` to `backup` (both a whole `.claude.json`) comes to. `when` names the backup in the
/// summary; `folder_exists` says whether a project's folder is there, as Claude Code adds a server that signs in
/// from inside it.
fn compare(
    backup: &Value,
    now: &Value,
    when: &str,
    folder_exists: &dyn Fn(&str) -> bool,
) -> Result<Resolved, String> {
    let then = Servers::read(backup);
    let here = Servers::read(now);
    let mut out = Resolved {
        action: "Restore".to_owned(),
        ..Resolved::default()
    };
    out.summary
        .push(format!("Restore servers as they were on {when}"));
    for (name, entry) in &then.user {
        server(
            None,
            name,
            entry,
            find(&here.user, name),
            folder_exists,
            &mut out,
        );
    }
    let mut left_out = Vec::new();
    for project in &then.projects {
        match here.project(&project.normal) {
            Some(now) => {
                for (name, entry) in &project.servers {
                    let current = find(&now.servers, name);
                    server(
                        Some(&now.key),
                        name,
                        entry,
                        current,
                        folder_exists,
                        &mut out,
                    );
                }
            }
            None if !project.servers.is_empty() => {
                let names: Vec<String> = project.servers.iter().map(|(n, _)| sanitize(n)).collect();
                left_out.push(format!(
                    "{} in {}",
                    ops::and_list(&names),
                    sanitize(&ops::folder_name(&project.key))
                ));
            }
            None => {}
        }
    }
    // Added since: left as they are, switches included.
    let new_user: Vec<&str> = here
        .user
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| find(&then.user, n).is_none())
        .collect();
    let mut added: Vec<String> = new_user
        .iter()
        .map(|n| format!("{} for the user", sanitize(n)))
        .collect();
    for project in &here.projects {
        let before = then.project(&project.normal);
        let new_local: Vec<&str> = project
            .servers
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| before.is_none_or(|b| find(&b.servers, n).is_none()))
            .collect();
        let folder = sanitize(&ops::folder_name(&project.key));
        added.extend(
            new_local
                .iter()
                .map(|n| format!("{} in {folder}", sanitize(n))),
        );
        let Some(before) = before else {
            continue;
        };
        let keeps =
            |n: &String| !new_user.contains(&n.as_str()) && !new_local.contains(&n.as_str());
        let off: Vec<&String> = before
            .off
            .iter()
            .filter(|n| !project.off.contains(n))
            .filter(|n| keeps(n))
            .collect();
        let on: Vec<&String> = project
            .off
            .iter()
            .filter(|n| !before.off.contains(n))
            .filter(|n| keeps(n))
            .collect();
        for (names, disabled) in [(&off, true), (&on, false)] {
            out.claude.extend(names.iter().map(|n| Op::SetDisabled {
                project: project.key.clone(),
                name: (*n).clone(),
                disabled,
            }));
        }
        let shown = |names: &[&String]| {
            ops::some_of(&names.iter().map(|n| sanitize(n)).collect::<Vec<_>>())
        };
        match (off.is_empty(), on.is_empty()) {
            (true, true) => {}
            (false, true) => out
                .summary
                .push(format!("Turn off {} in {folder}", shown(&off))),
            (true, false) => out
                .summary
                .push(format!("Turn on {} in {folder}", shown(&on))),
            (false, false) => out.summary.push(format!(
                "Turn off {} in {folder}, and turn on {} there",
                shown(&off),
                shown(&on)
            )),
        }
    }
    match added.len() {
        0 => {}
        1 => out.summary.push(format!(
            "Leave {} as it is: it was added after this backup",
            added[0]
        )),
        _ => out.summary.push(format!(
            "Leave {} as they are: they were added after this backup",
            ops::some_of(&added)
        )),
    }
    if !left_out.is_empty() {
        out.notes.push(format!(
            "Not restored: {}. Claude Code no longer lists {}.",
            left_out.join("; "),
            if left_out.len() == 1 {
                "that project"
            } else {
                "those projects"
            }
        ));
    }
    let writes = !out.claude.is_empty() || !out.cli.is_empty() || !out.swaps.is_empty();
    if writes {
        out.notes.push(
            "Only servers and where they are switched off are restored. The rest of .claude.json, Claude Code's own settings and history, stays as it is now."
                .to_owned(),
        );
        out.notes.push(
            "Klefki keeps a copy of the configuration as it is now before it restores, so the activity log can undo this restore too."
                .to_owned(),
        );
        out.notes.push(
            "A running Claude Code session may need a restart before it notices this.".to_owned(),
        );
    }
    for said in [&mut out.warnings, &mut out.notes] {
        let mut seen = std::collections::HashSet::new();
        said.retain(|s| seen.insert(s.clone()));
    }
    Ok(out)
}

/// Brings one server back to `entry`, its definition in the backup, from `current`, its definition now (`None` when
/// it is gone). `project` is the project as the file spells it now; `None` for the user. What cannot be restored
/// is left as it is and said, so a restore never stops half way for one server.
fn server(
    project: Option<&str>,
    name: &str,
    entry: &Value,
    current: Option<&Value>,
    folder_exists: &dyn Fn(&str) -> bool,
    out: &mut Resolved,
) {
    if current == Some(entry) {
        return;
    }
    let spot = Spot {
        project: project.map(str::to_owned),
        name: name.to_owned(),
    };
    let words = spot.words();
    let mut skip = |why: &str| {
        out.warnings.push(format!("{words} is not restored: {why}"));
    };
    if hidden(entry) {
        return skip("its backup says <hidden> where a value should be.");
    }
    let signs_in = swap::signs_in(entry);
    let by_claude = signs_in || current.is_some_and(swap::signs_in);
    let valid = if signs_in {
        entry::validate_new(entry)
    } else {
        entry::validate(entry)
    };
    if let Err(why) = valid {
        return skip(&format!("{why}."));
    }
    let lines = match ops::describe(&ops::shown_values(entry)) {
        Ok(lines) => lines,
        Err(why) => return skip(&format!("it cannot be shown in full for review ({why}).")),
    };
    if by_claude {
        // Claude Code's own commands do these, and they are given the name.
        if crate::model::ServerName::parse(name).is_err() {
            return skip("Claude Code's commands cannot be given that name.");
        }
        if signs_in && let Err(why) = ops::typed_secret_refused(&sanitize(name), entry) {
            return skip(&why);
        }
    }
    match current {
        None => {
            let op = Op::AddServer {
                project: spot.project.clone(),
                name: name.to_owned(),
                entry: entry.clone(),
            };
            if by_claude {
                if let Some(folder) = project
                    && !folder_exists(folder)
                {
                    return skip(
                        "its project folder is not there, and Claude Code adds a project's server from inside it.",
                    );
                }
                let lost: Vec<String> = entry
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, _)| k)
                    .filter(|k| swap::as_claude_writes(entry).get(k.as_str()).is_none())
                    .map(|k| sanitize(k))
                    .collect();
                if !lost.is_empty() {
                    out.warnings.push(format!(
                        "Claude Code keeps only the settings it uses, so {words} loses {}.",
                        lost.join(", ")
                    ));
                }
                out.preview.push(Op::AddServer {
                    project: spot.project.clone(),
                    name: name.to_owned(),
                    entry: swap::as_claude_writes(entry),
                });
                out.cli.push(CliAdd {
                    name: name.to_owned(),
                    json: entry.to_string(),
                    project: spot.project.clone(),
                    secret: None,
                    stored: true,
                });
            } else {
                out.claude.push(op);
            }
            out.summary.push(format!("Put back {words}"));
        }
        Some(current) => {
            if by_claude {
                out.swaps.push(Swap {
                    from: spot.clone(),
                    remove: true,
                    to: spot.clone(),
                    edited: Some(entry.clone()),
                });
            } else {
                out.claude.push(Op::ReplaceServer {
                    project: spot.project.clone(),
                    name: name.to_owned(),
                    entry: entry.clone(),
                });
            }
            out.destructive = true;
            out.summary.push(format!(
                "Change {} back {}: its {}",
                sanitize(name),
                swap::place(project),
                ops::and_list(&differences(entry, current))
            ));
            out.warnings.extend(secrets_lost(&words, entry, current));
        }
    }
    let remote = entry
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "stdio");
    out.reviews.push(crate::api::Review {
        title: format!("{words} would {}:", if remote { "connect" } else { "run" }),
        lines,
    });
}

/// Whether a value of the backup's environment or headers is [`entry::MASK`], which a write takes to mean "keep the
/// value there now".
fn hidden(entry: &Value) -> bool {
    ["env", "headers"].iter().any(|key| {
        entry
            .get(key)
            .and_then(Value::as_object)
            .is_some_and(|values| values.values().any(|v| v.as_str() == Some(entry::MASK)))
    })
}

/// The settings of a definition that differ between `a` and `b`, by name, in the order they come.
fn differences(a: &Value, b: &Value) -> Vec<String> {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
        return vec!["definition".to_owned()];
    };
    let mut keys: Vec<&String> = a.keys().collect();
    keys.extend(b.keys().filter(|k| !a.contains_key(*k)));
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| sanitize(k))
        .collect()
}

/// For each value that looks like a secret in the environment or headers of `current` and is not the same in the
/// backup's `entry`, what restoring does to it. Names it, never its value.
fn secrets_lost(words: &str, entry: &Value, current: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for (key, what) in [("env", "variable"), ("headers", "header")] {
        let Some(values) = current.get(key).and_then(Value::as_object) else {
            continue;
        };
        for (name, value) in values {
            let Some(text) = value.as_str() else {
                continue;
            };
            let then = entry.get(key).and_then(|v| v.get(name));
            if then == Some(value) || secretlike::suggestion(name, text).is_none() {
                continue;
            }
            out.push(format!(
                "{words}: the {what} {} looks like a secret and {}. The value it has now is kept only in the copy Klefki takes before restoring.",
                sanitize(name),
                if then.is_some() {
                    "goes back to its value in the backup"
                } else {
                    "is not in the backup, so it is removed"
                }
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn restore(backup: &Value, now: &Value) -> Resolved {
        compare(backup, now, "13 Oct 2026 at 09:12", &|_| true).unwrap()
    }

    fn stdio(command: &str) -> Value {
        json!({"type": "stdio", "command": command})
    }

    fn sso() -> Value {
        json!({"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "c-1"}})
    }

    #[test]
    fn what_is_gone_is_put_back_and_what_was_added_since_is_left() {
        let backup = json!({
            "mcpServers": {"gw": stdio("gw")},
            "projects": {"/work/a": {"mcpServers": {"loc": stdio("loc")}}}
        });
        let now = json!({
            "mcpServers": {"newer": stdio("newer")},
            "projects": {"/work/a": {"mcpServers": {"fresh": stdio("fresh")}}}
        });
        let out = restore(&backup, &now);
        assert_eq!(
            out.claude,
            vec![
                Op::AddServer {
                    project: None,
                    name: "gw".into(),
                    entry: stdio("gw")
                },
                Op::AddServer {
                    project: Some("/work/a".into()),
                    name: "loc".into(),
                    entry: stdio("loc")
                },
            ]
        );
        assert_eq!(
            out.summary,
            vec![
                "Restore servers as they were on 13 Oct 2026 at 09:12".to_owned(),
                "Put back gw for the user".to_owned(),
                "Put back loc in a".to_owned(),
                "Leave newer for the user and fresh in a as they are: they were added after this backup"
                    .to_owned(),
            ]
        );
        assert_eq!(out.action, "Restore");
        assert!(!out.destructive, "nothing is removed or overwritten");
        // What a server put back would run is shown, as for any server added.
        assert_eq!(out.reviews.len(), 2);
        assert!(out.reviews[0].lines.iter().any(|l| l.contains("gw")));
    }

    #[test]
    fn a_changed_definition_goes_back_and_says_what_changes() {
        let backup = json!({"mcpServers": {
            "gw": {"type": "stdio", "command": "node", "args": ["a.js"]},
            "same": stdio("same")
        }});
        let now = json!({"mcpServers": {
            "gw": {"type": "stdio", "command": "deno", "args": ["a.js"], "env": {"LOG": "1"}},
            "same": stdio("same")
        }});
        let out = restore(&backup, &now);
        assert_eq!(
            out.claude,
            vec![Op::ReplaceServer {
                project: None,
                name: "gw".into(),
                entry: json!({"type": "stdio", "command": "node", "args": ["a.js"]})
            }]
        );
        assert_eq!(
            out.summary[1..],
            ["Change gw back for the user: its command and env".to_owned()]
        );
        assert!(out.destructive, "the definition now is overwritten");
    }

    #[test]
    fn nothing_differs_nothing_is_written() {
        let both = json!({"mcpServers": {"gw": stdio("gw")}, "projects": {"/w": {"disabledMcpServers": ["gw"]}}});
        let out = restore(&both, &both);
        assert!(out.claude.is_empty() && out.cli.is_empty() && out.swaps.is_empty());
        assert_eq!(out.summary.len(), 1);
    }

    #[test]
    fn switches_go_back_except_for_servers_added_since() {
        let backup = json!({
            "mcpServers": {"gw": stdio("gw"), "tool": stdio("tool")},
            "projects": {
                "C:/work/a": {"disabledMcpServers": ["gw", "repo-server"]},
                "/work/b": {"disabledMcpServers": []}
            }
        });
        // Since: gw was switched on in a, tool and the project's own server switched off; newer added and switched
        // off. The project is spelled another way now.
        let now = json!({
            "mcpServers": {"gw": stdio("gw"), "tool": stdio("tool"), "newer": stdio("newer")},
            "projects": {
                "c:\\work\\a\\": {"disabledMcpServers": ["tool", "newer"]},
                "/work/b": {"disabledMcpServers": ["tool"]}
            }
        });
        let out = restore(&backup, &now);
        let set = |p: &str, n: &str, d: bool| Op::SetDisabled {
            project: p.into(),
            name: n.into(),
            disabled: d,
        };
        assert_eq!(
            out.claude,
            vec![
                set("c:\\work\\a\\", "gw", true),
                set("c:\\work\\a\\", "repo-server", true),
                set("c:\\work\\a\\", "tool", false),
                set("/work/b", "tool", false),
            ]
        );
        assert!(
            out.summary
                .contains(&"Turn off gw and repo-server in a, and turn on tool there".to_owned())
        );
        assert!(out.summary.contains(&"Turn on tool in b".to_owned()));
        assert!(
            !out.claude
                .iter()
                .any(|op| matches!(op, Op::SetDisabled { name, .. } if name == "newer")),
            "a server added since keeps its switch"
        );
    }

    #[test]
    fn a_secret_set_since_is_named_when_it_goes_back_never_its_value() {
        let backup = json!({"mcpServers": {"gw": {"type": "http", "url": "https://x.example.com/",
            "headers": {"Authorization": "Bearer old-0ld-T0ken-value-123456"}}}});
        let now = json!({"mcpServers": {"gw": {"type": "http", "url": "https://x.example.com/",
            "headers": {"Authorization": "Bearer new-N3w-T0ken-value-654321", "X-Team": "red"}},
            "loc": {"type": "stdio", "command": "x", "env": {"API_KEY": "sk-live-abcdefghijklmnop12345"}}}});
        let out = restore(&backup, &now);
        let said = out.warnings.join("\n");
        assert!(said.contains("Authorization"), "{said}");
        assert!(!said.contains("X-Team"), "not a secret: {said}");
        for value in ["old-0ld", "new-N3w", "sk-live"] {
            assert!(!format!("{:?}{:?}", out.summary, out.warnings).contains(value));
        }
        // loc was added since and is left alone, secret and all.
        assert!(!said.contains("API_KEY"), "{said}");
    }

    #[test]
    fn a_server_that_signs_in_goes_back_through_claude_codes_commands() {
        let backup = json!({"mcpServers": {"sso": sso(), "gone": sso()},
            "projects": {"/work/a": {"mcpServers": {"loc": sso()}}}});
        let mut changed = sso();
        changed["url"] = json!("https://sso.example.com/v2/mcp");
        let now = json!({"mcpServers": {"sso": changed}, "projects": {"/work/a": {}}});
        let out = restore(&backup, &now);
        assert!(
            out.claude.is_empty(),
            "Klefki never writes an oauth entry itself"
        );
        assert_eq!(
            out.swaps,
            vec![Swap {
                from: Spot {
                    project: None,
                    name: "sso".into()
                },
                remove: true,
                to: Spot {
                    project: None,
                    name: "sso".into()
                },
                edited: Some(sso()),
            }]
        );
        let added: Vec<(&str, Option<&str>, bool)> = out
            .cli
            .iter()
            .map(|a| (a.name.as_str(), a.project.as_deref(), a.stored))
            .collect();
        assert_eq!(
            added,
            [("gone", None, true), ("loc", Some("/work/a"), true)]
        );
        assert!(
            out.cli.iter().all(|a| a.secret.is_none()),
            "looked up later"
        );
        assert_eq!(out.preview.len(), 2, "what Claude Code will write is shown");
    }

    #[test]
    fn what_cannot_be_put_back_is_left_out_saying_why() {
        let mut with_secret = sso();
        with_secret["headers"] = json!({"X-Api-Key": "abcdef0123456789abcdef0123456789"});
        let backup = json!({
            "mcpServers": {"bad name": sso(), "typed": with_secret, "masked": {"type": "stdio", "command": "x", "env": {"A": "<hidden>"}}},
            "projects": {
                "/gone/project": {"mcpServers": {"old": stdio("old")}},
                "/no/folder": {"mcpServers": {"far": sso()}}
            }
        });
        let now = json!({"projects": {"/no/folder": {}}});
        let out = compare(&backup, &now, "then", &|p| p != "/no/folder").unwrap();
        assert!(
            out.claude.is_empty() && out.cli.is_empty(),
            "{:?}",
            out.claude
        );
        let said = out.warnings.join("\n");
        for name in ["bad name", "typed", "masked", "far"] {
            assert!(said.contains(&format!("{name} ")), "{name}: {said}");
        }
        assert!(!said.contains("abcdef0123"), "{said}");
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("project") && n.contains("old")),
            "{:?}",
            out.notes
        );
    }

    fn backups(dir: &Path) -> std::path::PathBuf {
        let backups = dir.join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        backups
    }

    #[test]
    fn only_a_kept_backup_of_claude_json_in_the_backup_folder_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        let backups = backups(dir.path());
        let good = "claude.json.0001760000000000-0.bak";
        std::fs::write(
            backups.join(good),
            r#"{"mcpServers": {"gw": {"command": "x"}}}"#,
        )
        .unwrap();
        std::fs::write(
            backups.join("settings.local-0123456789ab.0001760000000000-0.bak"),
            "{}",
        )
        .unwrap();
        std::fs::write(dir.path().join("claude.json.0001760000000000-1.bak"), "{}").unwrap();
        let now = r#"{"numStartups": 1}"#;
        // By its full path, as the activity log lists it, or its name.
        let full = backups.join(good).display().to_string();
        for asked in [full.as_str(), good] {
            let out = resolve(&backups, asked, now).unwrap();
            assert_eq!(out.summary[1], "Put back gw for the user");
        }
        for asked in [
            "settings.local-0123456789ab.0001760000000000-0.bak",
            "../claude.json.0001760000000000-1.bak",
            "claude.json.0001760000000000-9.bak",
            "claude.json",
            "",
        ] {
            assert!(resolve(&backups, asked, now).is_err(), "{asked}");
        }
        let gone = resolve(&backups, "claude.json.0001760000000000-9.bak", now)
            .err()
            .unwrap();
        assert!(gone.contains("no longer kept"), "{gone}");
    }

    #[test]
    fn a_backup_that_is_not_a_configuration_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let backups = backups(dir.path());
        for (n, text) in ["not json", "[1, 2]", "{\"mcpServers\": 3}"]
            .iter()
            .enumerate()
        {
            let name = format!("claude.json.0001760000000000-{n}.bak");
            std::fs::write(backups.join(&name), text).unwrap();
            let out = resolve(&backups, &name, "{}");
            if n == 2 {
                // A list of servers that is not one has none to put back.
                assert_eq!(out.unwrap().claude, Vec::new());
            } else {
                assert!(out.is_err(), "{text}");
            }
        }
    }
}
