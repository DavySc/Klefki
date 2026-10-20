//! Turns what the user asked for into document edits. Needs the current inventory to know which projects exist.
use crate::adjust::Adjust;
use crate::api::{
    Approval, Change, Review, Scope, ServerId, ServerRow, Snapshot, Target, TransportKind,
};
use crate::config::docstore::Op;
use crate::config::inventory::{FileKey, FileKeys};
use crate::config::{entry, fsx, inventory, relative, secretlike};
use crate::sanitize::sanitize;
use crate::secrets::Secret;
use crate::swap::{self, Spot, Swap};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

/// A single shown value longer than this cannot be reviewed with care, so the approval is refused.
const MAX_REVIEW_CHARS: usize = 2000;
const MAX_GITIGNORE: u64 = 256 * 1024;

/// What the changes come to.
#[derive(Default)]
pub(crate) struct Resolved {
    /// Edits to `~/.claude.json`.
    pub claude: Vec<Op>,
    /// Edits to a project's `.claude/settings.local.json`, by project path.
    pub settings: BTreeMap<String, Vec<Op>>,
    pub warnings: Vec<String>,
    /// Good to know, not a reason to hesitate (shown more quietly than warnings).
    pub notes: Vec<String>,
    /// One plain sentence per change, for the top of the review ("Turn off github in 3 projects").
    pub summary: Vec<String>,
    /// What the confirm button says: the verb all changes share, else "Apply".
    pub action: String,
    /// Something is removed (a delete), so the confirm button is shown as dangerous.
    pub destructive: bool,
    pub reviews: Vec<Review>,
    /// Definitions that were reviewed; they must be unchanged when the plan is applied.
    pub guards: Vec<Guard>,
    /// Servers Claude Code adds itself, with `claude mcp add-json`: those that sign in with OAuth.
    pub cli: Vec<CliAdd>,
    /// What [`Self::cli`] and [`Self::swaps`] will change in `~/.claude.json`, as edits klefki never makes: they show
    /// the change in the plan, and say whether the file still allows it just before the command runs.
    pub preview: Vec<Op>,
    /// Servers removed and added again (or copied) through Claude Code's own commands: those that sign in with OAuth.
    pub swaps: Vec<Swap>,
    /// Definitions Klefki kept, to add back ([`Change::PutBack`]) or to forget ([`Change::ForgetKept`]).
    pub put_back: Vec<String>,
    pub forget: Vec<String>,
    /// What is already so, one sentence per change that can say it ("loc is already on in a."): shown instead of
    /// the change when it would write nothing.
    pub already: Vec<String>,
    /// A profile was used without `only` where other servers are on: using it with `only` would turn them off.
    pub others_on: bool,
}

impl Resolved {
    /// Every place a server arrives in `.claude.json` under a name it did not have there: added, copied, moved or
    /// renamed, by Klefki or by Claude Code's commands.
    pub(crate) fn arrivals(&self) -> Vec<Spot> {
        let spot = |project: &Option<String>, name: &str| Spot {
            project: project.clone(),
            name: name.to_owned(),
        };
        let written = self.claude.iter().filter_map(|op| match op {
            Op::AddServer { project, name, .. } => Some(spot(project, name)),
            Op::CopyServer { to, new_name, .. } => Some(spot(to, new_name)),
            Op::RenameServer {
                project, new_name, ..
            } => Some(spot(project, new_name)),
            _ => None,
        });
        let added = self.cli.iter().map(|a| spot(&a.project, &a.name));
        let swapped = self
            .swaps
            .iter()
            .filter(|s| !(s.remove && s.from == s.to))
            .map(|s| s.to.clone());
        written.chain(added).chain(swapped).collect()
    }
}

/// A server Claude Code adds with its own command, `claude mcp add-json`, because it signs in with OAuth: Claude
/// Code keeps the client secret in its own secure storage, out of `~/.claude.json`.
pub(crate) struct CliAdd {
    pub name: String,
    /// The definition, on one line, as the command is given it. Holds no secret.
    pub json: String,
    /// `None` for the user. Else the project, spelled as Claude Code knows it: the command adds a local server to
    /// the project it runs in, so it runs in that folder.
    pub project: Option<String>,
    /// Handed to the command only in `MCP_CLIENT_SECRET`.
    pub secret: Option<Secret>,
    /// The client secret is the one Klefki keeps under this server's own place and name, looked up when the plan is
    /// made: a server a restore puts back.
    pub stored: bool,
}

/// An approval is by name. If the repository edits the definition after the user looked at it, the approval
/// would cover something that was never shown, so the plan checks the definition again before writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Guard {
    pub project: String,
    pub name: String,
    digest: String,
}

impl Guard {
    /// Is the server still defined exactly as it was when reviewed?
    pub(crate) fn still_holds(&self) -> bool {
        inventory::project_server(&self.project, &self.name)
            .is_ok_and(|entry| digest(&entry) == self.digest)
    }
}

fn digest(entry: &Value) -> String {
    crate::hex::lower(&Sha256::digest(entry.to_string().as_bytes()))
}

pub(crate) fn resolve(
    changes: &[Change],
    snapshot: &Snapshot,
    keys: &FileKeys,
) -> Result<Resolved, String> {
    let mut out = Resolved::default();
    let mut verbs: Vec<&str> = Vec::new();
    for change in changes {
        let verb = match change {
            Change::SetEnabled { enabled: true, .. } => "Turn on",
            Change::SetEnabled { enabled: false, .. } => "Turn off",
            Change::Approve { .. } => "Approve",
            Change::Delete { .. } => "Delete",
            Change::Replace { .. } => "Save",
            Change::Rename { .. } => "Rename",
            Change::Duplicate { .. } => "Duplicate",
            Change::MigrateLegacyDisabled { .. } => "Migrate",
            Change::Add { .. } => "Add",
            Change::Move { .. } => "Move",
            Change::PutBack { .. } => "Put back",
            Change::ForgetKept { .. } => "Forget",
            Change::UseProfile { .. } => "Use profile",
            Change::Restore { .. } => {
                return Err("A restore is reviewed on its own.".to_owned());
            }
        };
        if !verbs.contains(&verb) {
            verbs.push(verb);
        }
        out.destructive |= matches!(change, Change::Delete { .. } | Change::ForgetKept { .. });
        out.summary.push(summarise(change, snapshot));
        match change {
            Change::SetEnabled { id, enabled } => {
                set_enabled(id, *enabled, snapshot, keys, &mut out)?;
            }
            Change::Approve { id } => approve(id, snapshot, keys, &mut out)?,
            Change::Delete { id } => delete(id, snapshot, keys, &mut out)?,
            Change::Replace { id, json } => replace(id, json, snapshot, keys, &mut out)?,
            Change::Rename { id, name } => rename(id, name, snapshot, keys, &mut out)?,
            Change::Duplicate {
                id,
                name,
                targets,
                adjust,
            } => {
                duplicate(id, name, targets, adjust, snapshot, keys, &mut out)?;
            }
            Change::MigrateLegacyDisabled { project } => {
                migrate_legacy(project, snapshot, &mut out)?;
            }
            Change::Move { id, to } => move_server(id, to, snapshot, keys, &mut out)?,
            Change::PutBack { kept } => {
                find_kept(kept, snapshot)?;
                out.put_back.push(kept.clone());
            }
            Change::ForgetKept { kept } => {
                find_kept(kept, snapshot)?;
                out.forget.push(kept.clone());
                out.warnings.push(
                    "Klefki deletes the only copy it has of this definition. The copy of the configuration it kept first is still in its backups folder."
                        .to_owned(),
                );
            }
            Change::Restore { .. } => unreachable!("refused above"),
            Change::UseProfile {
                name,
                members,
                project,
                only,
            } => use_profile(name, members, project.as_deref(), *only, snapshot, &mut out)?,
            Change::Add {
                name,
                json,
                targets,
                secret,
            } => add(
                name,
                json,
                targets,
                secret.as_ref(),
                snapshot,
                keys,
                &mut out,
            )?,
        }
    }
    out.action = match verbs.as_slice() {
        [one] => (*one).to_owned(),
        _ => "Apply".to_owned(),
    };
    if !out.claude.is_empty()
        || !out.cli.is_empty()
        || !out.swaps.is_empty()
        || !out.put_back.is_empty()
    {
        out.notes.push(
            "A running Claude Code session may need a restart before it notices this.".to_owned(),
        );
    }
    out.warnings
        .extend(relative_in_user_servers(&out, snapshot));
    // Several changes can raise the same point; it is said once, where it first came up.
    for said in [&mut out.warnings, &mut out.notes] {
        let mut seen = std::collections::HashSet::new();
        said.retain(|s| seen.insert(s.clone()));
    }
    Ok(out)
}

/// A warning for each server this plan writes for the user whose command names a path relative to the folder it
/// starts in. A user server runs in every project, and Claude Code starts it in the folder the session started in.
/// Only `.claude.json` edits: what Claude Code's own commands add or move signs in with OAuth, which only an address
/// does.
fn relative_in_user_servers(out: &Resolved, snapshot: &Snapshot) -> Vec<String> {
    // Copied or moved as it is: what its row found.
    let found_in = |project: &Option<String>, name: &str| {
        let scope = if project.is_some() {
            Scope::Local
        } else {
            Scope::User
        };
        let id = crate::model::ConnectionId::new(scope, project.as_deref(), name);
        snapshot
            .servers
            .iter()
            .find(|r| r.connection == id)
            .map(|r| r.relative_paths.clone())
            .unwrap_or_default()
    };
    let mut found: Vec<(&str, Vec<String>)> = Vec::new();
    for op in &out.claude {
        match op {
            Op::AddServer {
                project: None,
                name,
                entry,
            }
            | Op::ReplaceServer {
                project: None,
                name,
                entry,
            } => found.push((name, relative::relative_paths(entry))),
            Op::CopyServer {
                from,
                name,
                to: None,
                new_name,
                ..
            } => found.push((new_name, found_in(from, name))),
            _ => {}
        }
    }
    found
        .into_iter()
        .filter(|(_, paths)| !paths.is_empty())
        .map(|(name, paths)| {
            let named = match paths.as_slice() {
                [rest @ .., last] if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
                _ => paths.concat(),
            };
            format!(
                "{}: use full paths. Claude Code starts user servers in each session's folder, so sessions in other folders can't find {named}.",
                sanitize(name),
            )
        })
        .collect()
}

/// One plain sentence for a change, naming the server and where it applies, as the row shows them (the caller's
/// copy of the name is only used when the row is gone, and the change then fails anyway).
fn summarise(change: &Change, snapshot: &Snapshot) -> String {
    let shown = |id: &ServerId| {
        snapshot.servers.iter().find(|r| r.is(id)).map_or_else(
            || (id.scope, id.project.clone(), sanitize(&id.name)),
            |r| (r.scope, r.project.clone(), r.name.clone()),
        )
    };
    let (id, verb) = match change {
        Change::SetEnabled { id, enabled } => (id, if *enabled { "Turn on" } else { "Turn off" }),
        Change::Approve { id } => (id, "Approve"),
        Change::Delete { id } => (id, "Delete"),
        Change::Replace { id, .. } => (id, "Save changes to"),
        Change::Rename { id, name } => {
            let (scope, project, old) = shown(id);
            let place = match (scope, project.as_deref()) {
                (Scope::User, _) => " (user server)".to_owned(),
                (_, Some(project)) => format!(" in {}", sanitize(&folder_name(project))),
                (_, None) => String::new(),
            };
            return format!("Rename {old} to {}{place}", sanitize(name));
        }
        Change::MigrateLegacyDisabled { project } => {
            let legacy = snapshot
                .legacy_disabled
                .iter()
                .find(|l| l.project == *project);
            let folder = sanitize(&folder_name(project));
            return match legacy.map(|l| l.turns_off.as_slice()) {
                Some([]) | None => format!("Remove the old switch-off list in {folder}"),
                Some(names) => format!(
                    "Turn off {} in {folder}, as the old switch-off list says",
                    some_of(names)
                ),
            };
        }
        Change::Duplicate {
            id, name, targets, ..
        } => {
            return format!(
                "Duplicate {} as {}{}",
                shown(id).2,
                sanitize(name),
                places(targets)
            );
        }
        Change::Add { name, targets, .. } => {
            return format!("Add {}{}", sanitize(name), places(targets));
        }
        Change::Restore { .. } => return "Restore a backup".to_owned(),
        Change::UseProfile {
            name,
            project,
            only,
            ..
        } => {
            let place = match project {
                Some(p) => project_label(p, snapshot),
                None => format!(
                    "every project ({})",
                    count(snapshot.projects.len(), "project")
                ),
            };
            let rest = if *only {
                ", and turn off every other server there"
            } else {
                ""
            };
            return format!("Use the profile {} in {place}{rest}", sanitize(name));
        }
        Change::Move { id, to } => {
            let to = match to {
                Target::User => "the user".to_owned(),
                Target::Local(p) => sanitize(&folder_name(p)),
            };
            return format!("Move {} to {to}", shown(id).2);
        }
        Change::PutBack { kept } | Change::ForgetKept { kept } => {
            let verb = if matches!(change, Change::PutBack { .. }) {
                "Put back"
            } else {
                "Forget the kept copy of"
            };
            return match snapshot.kept.iter().find(|k| k.id == *kept) {
                Some(k) => format!("{verb} {} {}", k.name, k.place),
                None => format!("{verb} a kept definition"),
            };
        }
    };
    let (scope, project, name) = shown(id);
    let place = match (scope, project.as_deref()) {
        (Scope::User, _) => match change {
            Change::SetEnabled { .. } => {
                format!(" in {}", count(snapshot.projects.len(), "project"))
            }
            _ => " (user server)".to_owned(),
        },
        (_, Some(project)) => format!(" in {}", sanitize(&folder_name(project))),
        (_, None) => String::new(),
    };
    format!("{verb} {name}{place}")
}

/// ` for the user`, ` in a and b`, ` for the user and in a`.
fn places(targets: &[Target]) -> String {
    let user = targets.contains(&Target::User);
    let folders: Vec<String> = targets
        .iter()
        .filter_map(|t| match t {
            Target::Local(p) => Some(sanitize(&folder_name(p))),
            Target::User => None,
        })
        .collect();
    match (user, folders.is_empty()) {
        (true, true) => " for the user".to_owned(),
        (true, false) => format!(" for the user and in {}", and_list(&folders)),
        (false, _) => format!(" in {}", and_list(&folders)),
    }
}

/// `a`, `a and b`, `a, b and c`.
pub(crate) fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Like [`and_list`], naming the first few and counting the rest, so a long list keeps the summary one line.
/// The app's notice cuts names the same way (`shell::some_names`, which cannot call this); keep the two alike.
pub(crate) fn some_of(items: &[String]) -> String {
    const NAMED: usize = 5;
    if items.len() <= NAMED + 1 {
        return and_list(items);
    }
    format!(
        "{} and {} more",
        items[..NAMED].join(", "),
        items.len() - NAMED
    )
}

/// `1 project`, `3 projects`.
pub(crate) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The last part of a project path: `C:/work/app` -> `app`.
pub(crate) fn folder_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(last) if !last.is_empty() => last.to_owned(),
        // The root itself.
        _ => path.to_owned(),
    }
}

/// The row `id` names and how the file spells it. Messages use the row's own (shown) name, never the caller's
/// copy of it; writes use the file's spellings, so a name with a hidden character, or a long one, changes that
/// server and no other.
fn find<'a>(
    id: &ServerId,
    snapshot: &'a Snapshot,
    keys: &'a FileKeys,
) -> Result<(&'a ServerRow, &'a FileKey), String> {
    if let Some(row) = snapshot
        .servers
        .iter()
        .find(|r| r.is(id) && r.scope.is_read_only())
    {
        return Err(read_only(row));
    }
    inventory::locate(snapshot, keys, id).ok_or_else(|| {
        format!(
            "{} is no longer in the configuration. Refresh and try again.",
            sanitize(&id.name)
        )
    })
}

/// Why a plugin's or the organisation's server cannot be changed here: Claude Code reads it from files that are not
/// the user's.
fn read_only(row: &ServerRow) -> String {
    if row.scope == Scope::ClaudeAi {
        return format!(
            "{} is a connector of your claude.ai account, and is changed on claude.ai. Klefki can switch it on and off.",
            row.name
        );
    }
    if row.scope == Scope::Managed {
        return format!(
            "{} is set by your organisation{}, and only an administrator can change it. Klefki can switch it on and off.",
            row.name,
            row.managed
                .as_deref()
                .map(|f| format!(" in {f}"))
                .unwrap_or_default()
        );
    }
    match &row.plugin {
        Some(p) => format!(
            "{} comes from the plugin {}, and Klefki does not change a plugin's files. Change it in the plugin, or turn the plugin off in Claude Code.",
            row.name, p.plugin
        ),
        None => format!(
            "{} comes from a plugin, and Klefki does not change a plugin's files.",
            row.name
        ),
    }
}

fn set_enabled(
    id: &ServerId,
    enabled: bool,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    switch(id, enabled, snapshot, keys, out)?;
    if let Some(row) = snapshot.servers.iter().find(|r| r.is(id)) {
        let place = match (row.scope, row.project.as_deref()) {
            (Scope::Local | Scope::Project, Some(p)) => project_label(p, snapshot),
            _ => "every project Claude Code knows".to_owned(),
        };
        out.already.push(format!(
            "{} is already {} in {place}.",
            row.name,
            if enabled { "on" } else { "off" }
        ));
        if enabled {
            stays_off(row, out);
        } else {
            loads_instead(row, snapshot, out);
        }
    }
    Ok(())
}

/// At most this many projects are named in a warning; the rest are counted.
const PROJECTS_NAMED: usize = 5;

/// Where `row`'s plugin is off, turning the server on changes nothing: said, so the review states the real outcome.
fn stays_off(row: &ServerRow, out: &mut Resolved) {
    let Some(off) = row
        .plugin
        .as_ref()
        .map(|p| &p.off_in)
        .filter(|o| !o.is_empty())
    else {
        return;
    };
    let mut named: Vec<String> = off
        .iter()
        .take(PROJECTS_NAMED)
        .map(|p| sanitize(&folder_name(p)))
        .collect();
    if off.len() > PROJECTS_NAMED {
        named.push(count(off.len() - PROJECTS_NAMED, "more project"));
    }
    out.warnings.push(format!(
        "Its plugin is off in {}, so Claude Code still does not load {} there.",
        named.join(", "),
        row.name
    ));
}

/// Where Claude Code leaves a plugin's server out because `row` has the same command or address, turning `row` off
/// lets that one load instead: said, so the review states what the change does, not only what it writes.
fn loads_instead(row: &ServerRow, snapshot: &Snapshot, out: &mut Resolved) {
    for other in &snapshot.servers {
        let repeats = other
            .plugin
            .as_ref()
            .and_then(|p| p.same_as.as_deref())
            .is_some_and(|same| same == row.name);
        if repeats && (other.shadowed || !other.shadowed_in.is_empty()) {
            out.warnings.push(format!(
                "{} has the same command or address as {}, so Claude Code leaves it out. Where {} is off, Claude \
                 Code loads {} instead.",
                other.name, row.name, row.name, other.name
            ));
        }
    }
}

fn switch(
    id: &ServerId,
    enabled: bool,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    // A plugin's or the organisation's server: switched like a user server, by the name Claude Code gives it
    // (decisions 008 and 009). Only `.claude.json` changes, never the plugin's or the organisation's files.
    if let Some(row) = snapshot
        .servers
        .iter()
        .find(|r| r.is(id) && r.scope.is_read_only())
    {
        // The name as the files spell it, from its connection; the row's own is cleaned for showing.
        let name = match crate::model::ConnectionId::parse(row.connection.as_str()) {
            Some((scope, None, name)) if scope == row.scope => name,
            _ => return Err(read_only(row)),
        };
        let what = match row.scope {
            Scope::Managed => "a server your organisation sets",
            Scope::ClaudeAi => "a claude.ai connector",
            _ => "a plugin's server",
        };
        return switch_everywhere(&row.name, &name, what, enabled, snapshot, out);
    }
    let (row, file) = find(id, snapshot, keys)?;
    let shown = &row.name;
    let disabled = !enabled;
    match row.scope {
        Scope::Local | Scope::Project => {
            let project = file
                .project
                .clone()
                .ok_or_else(|| format!("{shown} has no project"))?;
            out.claude.push(Op::SetDisabled {
                project,
                name: file.name.clone(),
                disabled,
            });
            if enabled && row.scope == Scope::Project && row.approval != Some(Approval::Approved) {
                out.warnings.push(format!(
                    "{shown} is a project server that you have not approved in Claude Code. Turning it on does not approve it, so Claude will still not run it."
                ));
            }
        }
        Scope::Plugin | Scope::Managed | Scope::ClaudeAi => return Err(read_only(row)),
        Scope::User => {
            switch_everywhere(shown, &file.name, "a user server", enabled, snapshot, out)?;
        }
    }
    Ok(())
}

/// Switches a server that is loaded in every project, `name` as the file spells it, in each project Claude Code knows.
fn switch_everywhere(
    shown: &str,
    name: &str,
    what: &str,
    enabled: bool,
    snapshot: &Snapshot,
    out: &mut Resolved,
) -> Result<(), String> {
    if snapshot.projects.is_empty() {
        return Err(format!(
            "Claude Code knows no projects yet, so there is nowhere to switch {shown} {}.",
            if enabled { "on" } else { "off" }
        ));
    }
    for project in &snapshot.projects {
        out.claude.push(Op::SetDisabled {
            project: project.clone(),
            name: name.to_owned(),
            disabled: !enabled,
        });
    }
    out.warnings.push(format!(
        "{shown} is {what}, and Claude Code switches it per project. This changes the {} it knows today; a project it first opens later is not covered.",
        count(snapshot.projects.len(), "project")
    ));
    Ok(())
}

/// Turns on the servers of a profile in `project`, or in every project Claude Code knows, and with `only` turns off
/// every other server there (decision 012). Each is switched by the name Claude Code gives it, like any toggle: only
/// `disabledMcpServers` changes. The review says where a member will still not load, and why, so it promises only
/// what Claude Code will do.
fn use_profile(
    name: &str,
    members: &[crate::model::ConnectionId],
    project: Option<&str>,
    only: bool,
    snapshot: &Snapshot,
    out: &mut Resolved,
) -> Result<(), String> {
    let shown_profile = sanitize(name);
    let places: Vec<&String> = match project {
        Some(p) => vec![
            snapshot
                .projects
                .iter()
                .find(|known| *known == p)
                .ok_or_else(|| format!("Claude Code does not know the project {}.", sanitize(p)))?,
        ],
        None => snapshot.projects.iter().collect(),
    };
    if places.is_empty() {
        return Err(format!(
            "Claude Code knows no projects yet, so there is nowhere to use {shown_profile}."
        ));
    }
    let mut found = Vec::new();
    // The members that load in a place it is used in, as shown, once each, by their place in the profile.
    let mut here_names: Vec<(usize, String)> = Vec::new();
    // Without `only`: other servers on there now, by (project, name as the file spells it).
    let mut others_on: Vec<(String, String)> = Vec::new();
    let mut elsewhere = Vec::new();
    let mut unapproved = Vec::new();
    let mut turned_off = BTreeSet::new();
    // (project, name as the file spells it) and whether it goes off, in the order the servers are listed.
    let mut switches: Vec<((String, String), bool)> = Vec::new();
    // The members and the places they are turned on in: judged once every switch is known.
    let mut loaded_here: Vec<(&ServerRow, &String)> = Vec::new();
    for row in &snapshot.servers {
        // The scope, project and name as the files spell them.
        let Some((scope, own_project, file_name)) =
            crate::model::ConnectionId::parse(row.connection.as_str())
        else {
            continue;
        };
        let member = members.contains(&row.connection);
        if member {
            found.push(&row.connection);
        }
        let mut here_any = false;
        for place in &places {
            // A local or project server loads in its own project only.
            let here = match scope {
                Scope::Local | Scope::Project => own_project.as_deref() == Some(place.as_str()),
                _ => true,
            };
            if !here {
                continue;
            }
            here_any = true;
            if member || only {
                // Claude Code switches by name: where a member and another server share one, the member wins.
                let key = ((*place).clone(), file_name.clone());
                match switches.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, off)) => *off &= !member,
                    None => switches.push((key, !member)),
                }
            }
            // Said only when it is on there now: one already off is not turned off by this.
            if !member && !off_now(row, scope, place) {
                if only {
                    turned_off.insert((row.name.clone(), (*place).clone(), file_name.clone()));
                } else {
                    others_on.push(((*place).clone(), file_name.clone()));
                }
            }
            if member {
                loaded_here.push((row, *place));
            }
        }
        if let Some(at) = members.iter().position(|m| *m == row.connection)
            && here_any
            && !here_names.iter().any(|(_, n)| *n == row.name)
        {
            here_names.push((at, row.name.clone()));
        }
        if member && !here_any {
            elsewhere.push(format!(
                "{} ({})",
                row.name,
                row.project
                    .as_deref()
                    .map(|p| sanitize(&folder_name(p)))
                    .unwrap_or_default()
            ));
        }
        if member
            && here_any
            && row.scope == Scope::Project
            && row.approval != Some(Approval::Approved)
        {
            unapproved.push(row.name.clone());
        }
    }
    // A member that will still not load, and why → the places (folder names) where. Judged as things will be after
    // this change: what it turns off no longer hides a duplicate.
    let off_after = |place: &str, name: &str| {
        switches
            .iter()
            .any(|((p, n), off)| *off && p == place && n == name)
    };
    let mut held: Vec<((String, String), Vec<String>)> = Vec::new();
    for (row, place) in loaded_here {
        if let Some(why) = held_back(row, place, snapshot, &off_after) {
            let at = project_label(place, snapshot);
            let key = (row.name.clone(), why);
            match held.iter_mut().find(|(k, _)| *k == key) {
                Some((_, at_places)) => at_places.push(at),
                None => held.push((key, vec![at])),
            }
        }
    }
    for ((project, name), disabled) in &switches {
        out.claude.push(Op::SetDisabled {
            project: project.clone(),
            name: name.clone(),
            disabled: *disabled,
        });
    }
    // Only what is really turned off: not a server that shares its name with a member.
    let turned_off: BTreeSet<String> = turned_off
        .into_iter()
        .filter(|(_, place, name)| {
            switches
                .iter()
                .any(|((p, n), off)| *off && p == place && n == name)
        })
        .map(|(shown, ..)| shown)
        .collect();
    let missing: Vec<String> = members
        .iter()
        .filter(|m| !found.contains(m))
        .map(|m| m.shown_name())
        .collect();
    if !missing.is_empty() {
        out.notes.push(format!(
            "{} {} in {shown_profile} but no longer configured, so nothing is switched for {}.",
            some_of(&missing),
            if missing.len() == 1 { "is" } else { "are" },
            if missing.len() == 1 { "it" } else { "them" }
        ));
    }
    if !elsewhere.is_empty() {
        out.notes.push(format!(
            "In {shown_profile}, these load only in a project of their own, so they stay as they are: {}.",
            some_of(&elsewhere)
        ));
    }
    // A server that shares a member's name is on as the member, and "only" would leave it on too.
    // Not when none of its servers loads there: "only" would then turn everything off and nothing on.
    out.others_on |= !here_names.is_empty()
        && others_on
            .iter()
            .any(|key| !switches.iter().any(|(k, _)| k == key));
    // None of its servers loads there: why, rather than a bare "nothing to change".
    if here_names.is_empty() {
        let place = match project {
            Some(p) => project_label(p, snapshot),
            None => "any project Claude Code knows".to_owned(),
        };
        out.already.push(format!(
            "None of the servers of {shown_profile} loads in {place}, so there is nothing to turn on there."
        ));
        out.already.extend(out.notes.iter().cloned());
    }
    for ((server, why), at) in &held {
        let at = if project.is_none() && at.len() == places.len() {
            "any project".to_owned()
        } else {
            some_of(at)
        };
        out.warnings
            .push(format!("{server} will still not load in {at}: {why}"));
    }
    if !unapproved.is_empty() {
        out.warnings.push(format!(
            "{} {} project server{} that you have not approved in Claude Code. Turning {} on does not approve {}, \
             so Claude will still not run {}.",
            and_list(&unapproved),
            if unapproved.len() == 1 { "is a" } else { "are" },
            if unapproved.len() == 1 { "" } else { "s" },
            if unapproved.len() == 1 { "it" } else { "them" },
            if unapproved.len() == 1 { "it" } else { "them" },
            if unapproved.len() == 1 { "it" } else { "them" },
        ));
    }
    if !turned_off.is_empty() {
        let names: Vec<String> = turned_off.into_iter().collect();
        out.warnings.push(format!(
            "This also turns off {}, which {} not in {shown_profile}.",
            some_of(&names),
            if names.len() == 1 { "is" } else { "are" }
        ));
    }
    if project.is_none() {
        out.warnings.push(format!(
            "Claude Code switches servers per project. This changes the {} it knows today; a project it first opens \
             later is not covered.",
            count(snapshot.projects.len(), "project")
        ));
    }
    if !here_names.is_empty() {
        here_names.sort();
        let here_names: Vec<String> = here_names.into_iter().map(|(_, n)| n).collect();
        let one = here_names.len() == 1;
        let place = match project {
            Some(p) => project_label(p, snapshot),
            None => "every project Claude Code knows".to_owned(),
        };
        out.already.push(format!(
            "The {} of {shown_profile}, {}, {} already {} in {place}.{}",
            if one { "server" } else { "servers" },
            some_of(&here_names),
            if one { "is" } else { "are" },
            match (only, one) {
                (true, true) => "the only one on",
                (true, false) => "the only ones on",
                (false, _) => "on",
            },
            if only {
                ""
            } else {
                " Other servers there are left as they are."
            }
        ));
    }
    if found.is_empty() {
        return Err(format!(
            "None of the servers in {shown_profile} is configured any more."
        ));
    }
    Ok(())
}

/// The projects (as [`Snapshot::projects`] spells them) where a profile is on alone now: of the servers Claude Code
/// switches there, its members are on and every other one is off, so using it there with "Only these servers there"
/// would change nothing, and every member there loads: none is held back by policy, by another server of its name,
/// by its plugin, as a duplicate, or as a project server not approved there. Worked out from the files each time, as
/// nothing records which profile was used (decision 012). A project where none of its members loads is never one.
pub fn profile_alone_in(
    members: &[crate::model::ConnectionId],
    snapshot: &Snapshot,
) -> Vec<String> {
    // Each server with the scope, project and name the files spell it with, and whether it is a member: worked out
    // once, not for each project.
    let rows: Vec<(&ServerRow, Scope, Option<String>, String, bool)> = snapshot
        .servers
        .iter()
        .filter_map(|r| {
            let (scope, project, name) = crate::model::ConnectionId::parse(r.connection.as_str())?;
            Some((r, scope, project, name, members.contains(&r.connection)))
        })
        .collect();
    if !rows.iter().any(|r| r.4) {
        return Vec::new();
    }
    snapshot
        .projects
        .iter()
        .filter(|place| {
            let here = crate::sanitize::shown(place.as_str());
            let mut on: BTreeSet<&str> = BTreeSet::new();
            let mut off: BTreeSet<&str> = BTreeSet::new();
            let mut wanted: BTreeSet<&str> = BTreeSet::new();
            let mut members_here: Vec<&ServerRow> = Vec::new();
            for (row, scope, project, name, member) in &rows {
                // A local or project server loads in its own project only.
                if matches!(scope, Scope::Local | Scope::Project)
                    && project.as_deref() != Some(place.as_str())
                {
                    continue;
                }
                if off_in(row, *scope, &here) {
                    off.insert(name);
                } else {
                    on.insert(name);
                }
                // Claude Code switches by name: a name a member has stays on, whichever server it is.
                if *member {
                    wanted.insert(name);
                    members_here.push(row);
                }
            }
            // As things are: nothing changes, so a server is off where its switch is off now.
            let switched_off = |_: &str, name: &str| off.contains(name);
            let loads = |row: &ServerRow| {
                held_back(row, place, snapshot, &switched_off).is_none()
                    && (row.scope != Scope::Project
                        || (row.approval == Some(Approval::Approved) && !row.shadowed))
            };
            !wanted.is_empty()
                && wanted.iter().all(|n| !off.contains(n))
                && on.iter().all(|n| wanted.contains(n))
                && members_here.iter().all(|r| loads(r))
        })
        .cloned()
        .collect()
}

/// Whether `row` is switched off in `place` (a project as `.claude.json` spells it), where it loads, now.
fn off_now(row: &ServerRow, scope: Scope, place: &str) -> bool {
    off_in(row, scope, &crate::sanitize::shown(place))
}

/// [`off_now`], with the project as shown (what `disabled_projects` holds).
fn off_in(row: &ServerRow, scope: Scope, shown_place: &str) -> bool {
    match scope {
        // Its own switch, in its own project.
        Scope::Local | Scope::Project => !row.enabled,
        _ => row.disabled_projects.iter().any(|p| p == shown_place),
    }
}

/// A project by its folder's name, with the folder that holds it when another project's folder has that name too.
fn project_label(path: &str, snapshot: &Snapshot) -> String {
    let name = folder_name(path);
    let shared = snapshot
        .projects
        .iter()
        .filter(|p| folder_name(p) == name)
        .count()
        > 1;
    let parent = path
        .trim_end_matches(['/', '\\'])
        .rsplit_once(['/', '\\'])
        .map(|(parent, _)| folder_name(parent))
        .unwrap_or_default();
    if shared && !parent.is_empty() {
        sanitize(&format!("{name} ({parent})"))
    } else {
        sanitize(&name)
    }
}

/// Why Claude Code will not load `row` in `place` (a project as `.claude.json` spells it) even when it is on there,
/// as the rest of a sentence; `None` when nothing holds it back. `off_after` says whether the change turns a server
/// (by name) off in a place.
fn held_back(
    row: &ServerRow,
    place: &str,
    snapshot: &Snapshot,
    off_after: &dyn Fn(&str, &str) -> bool,
) -> Option<String> {
    let here = crate::sanitize::shown(place);
    if let Some(why) = &row.not_loaded {
        return Some(why.clone());
    }
    if let Some(b) = row.blocked.as_ref().filter(|b| b.projects.contains(&here)) {
        return Some(b.why.clone());
    }
    if let Some(p) = &row.plugin {
        if p.off_in.contains(&here) {
            return Some(format!("its plugin {} is off there.", p.plugin));
        }
        if let Some(same) = p.same_as.as_deref()
            && (row.shadowed || row.shadowed_in.contains(&here))
            && !off_after(place, same)
        {
            return Some(format!(
                "it has the same command or address as {same}, so Claude Code leaves it out."
            ));
        }
    }
    if row.scope == Scope::User && row.shadowed_in.contains(&here) {
        let file_name = crate::model::ConnectionId::parse(row.connection.as_str())?.2;
        let other = snapshot.servers.iter().find(|r| {
            matches!(r.scope, Scope::Local | Scope::Project)
                && crate::model::ConnectionId::parse(r.connection.as_str())
                    .is_some_and(|(_, p, n)| p.as_deref() == Some(place) && n == file_name)
        });
        let kind = match other.map(|r| r.scope) {
            Some(Scope::Project) => "the project server",
            _ => "the local server",
        };
        return Some(format!(
            "{kind} {} has its name there, and Claude Code loads that one instead.",
            row.name
        ));
    }
    None
}

/// Where a user or local server lives, for an op, as the file spells it; project servers are refused (the
/// repository's file).
fn own_server<'a>(
    id: &ServerId,
    snapshot: &'a Snapshot,
    keys: &'a FileKeys,
    what: &str,
) -> Result<&'a FileKey, String> {
    let (row, file) = find(id, snapshot, keys)?;
    match row.scope {
        Scope::User => Ok(file),
        Scope::Local if file.project.is_some() => Ok(file),
        Scope::Local => Err(format!("{} has no project", row.name)),
        Scope::Plugin | Scope::Managed | Scope::ClaudeAi => Err(read_only(row)),
        Scope::Project => Err(format!(
            "{} is defined in the repository's .mcp.json, and Klefki does not {what} files in a repository. Change it there, or switch it off here.",
            row.name
        )),
    }
}

fn delete(
    id: &ServerId,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let file = own_server(id, snapshot, keys, "delete from")?;
    out.claude.push(Op::RemoveServer {
        project: file.project.clone(),
        name: file.name.clone(),
    });
    out.warnings.push(
        "Deleting removes the definition only. A sign-in Claude Code keeps for this server is not removed."
            .to_owned(),
    );
    Ok(())
}

fn replace(
    id: &ServerId,
    json: &str,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let file = own_server(id, snapshot, keys, "edit")?;
    if json.len() > entry::MAX_ENTRY_BYTES {
        return Err(format!(
            "The definition is longer than {} KiB.",
            entry::MAX_ENTRY_BYTES / 1024
        ));
    }
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        format!(
            "The definition is not valid JSON: {}",
            sanitize(&e.to_string())
        )
    })?;
    let (row, _) = find(id, snapshot, keys)?;
    let oauth = row.oauth.is_some() || swap::signs_in(&value);
    // Checked again after hidden values are put back; this catches mistakes before anything is read.
    if oauth {
        entry::validate_new(&value)
    } else {
        entry::validate(&value)
    }
    .map_err(|why| format!("The definition is not usable: {why}."))?;
    if !oauth {
        out.claude.push(Op::ReplaceServer {
            project: file.project.clone(),
            name: file.name.clone(),
            entry: value,
        });
        return Ok(());
    }
    if swap::signs_in(&value) {
        typed_secret_refused(&row.name, &value)?;
    }
    let here = Spot {
        project: file.project.clone(),
        name: file.name.clone(),
    };
    out.swaps.push(Swap {
        from: here.clone(),
        remove: true,
        to: here,
        edited: Some(value),
        adjust: Default::default(),
    });
    Ok(())
}

/// Moves a user or local server, keeping its name, to `to`. One that signs in with OAuth is moved by Claude Code's
/// own commands (see [`Swap`]); any other in one write, where it is added before it is removed.
fn move_server(
    id: &ServerId,
    to: &Target,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let file = own_server(id, snapshot, keys, "move")?;
    let (row, _) = find(id, snapshot, keys)?;
    let here = match (&file.project, to) {
        (None, Target::User) => true,
        (Some(p), Target::Local(q)) => {
            inventory::normalise_project(p) == inventory::normalise_project(q)
        }
        _ => false,
    };
    if here {
        return Err(format!("{} is already there.", row.name));
    }
    let to = new_places(
        &file.name,
        std::slice::from_ref(to),
        "it",
        Some(&row.connection),
        snapshot,
        keys,
        out,
    )?
    .pop()
    .expect("one place asked, one given");
    match (&file.project, &to) {
        (_, None) => out.notes.push(format!(
            "As a user server, {} is on in every project, except where it is switched off.",
            row.name
        )),
        // The main consequence of narrowing it to one project.
        (None, Some(there)) => {
            let others: Vec<String> = snapshot
                .projects
                .iter()
                .filter(|p| inventory::normalise_project(p) != inventory::normalise_project(there))
                .map(|p| sanitize(&folder_name(p)))
                .collect();
            if !others.is_empty() {
                out.warnings.push(format!(
                    "{} will only be in {}: it is no longer there for you in {}.",
                    row.name,
                    sanitize(&folder_name(there)),
                    some_of(&others)
                ));
            }
        }
        _ => {}
    }
    let copy = Op::CopyServer {
        from: file.project.clone(),
        name: file.name.clone(),
        to: to.clone(),
        new_name: file.name.clone(),
        base: None,
    };
    let remove = Op::RemoveServer {
        project: file.project.clone(),
        name: file.name.clone(),
    };
    if row.oauth.is_none() {
        out.claude.extend([copy, remove]);
    } else {
        out.swaps.push(Swap {
            from: Spot {
                project: file.project.clone(),
                name: file.name.clone(),
            },
            remove: true,
            to: Spot {
                project: to,
                name: file.name.clone(),
            },
            edited: None,
            adjust: Default::default(),
        });
    }
    Ok(())
}

/// The kept definition `id` names, or why it cannot be used.
fn find_kept<'a>(id: &str, snapshot: &'a Snapshot) -> Result<&'a crate::api::Kept, String> {
    snapshot
        .kept
        .iter()
        .find(|k| k.id == id)
        .ok_or_else(|| "That kept definition is gone. Refresh and look again.".to_owned())
}

/// Gives a user or local server another name where it is: only its key changes, and the new name is switched off
/// wherever the old one was.
fn rename(
    id: &ServerId,
    name: &str,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let file = own_server(id, snapshot, keys, "change")?;
    let (row, _) = find(id, snapshot, keys)?;
    let shown = &row.name;
    let new = sanitize(name);
    crate::model::ServerName::parse(name)
        .map_err(|e| format!("{:?} cannot be used as a server name: {e}.", sanitize(name)))?;
    let place = match &file.project {
        None => "for the user".to_owned(),
        Some(p) => format!("in {}", sanitize(&folder_name(p))),
    };
    if name == file.name {
        return Err(format!("{shown} is already called {new}."));
    }
    let same_project = |k: &FileKey, p: &str| {
        k.project
            .as_deref()
            .is_some_and(|q| inventory::normalise_project(q) == inventory::normalise_project(p))
    };
    // Every other server called `name` in the file (the new name is plain ASCII, so it is spelled as shown).
    let others: Vec<(&ServerRow, &FileKey)> = snapshot
        .servers
        .iter()
        .filter_map(|r| Some((r, keys.get(&r.connection)?)))
        .filter(|(_, k)| k.name == name)
        .collect();
    let same_place = |(r, k): &(&ServerRow, &FileKey)| match &file.project {
        None => r.scope == Scope::User,
        Some(p) => r.scope == Scope::Local && same_project(k, p),
    };
    if others.iter().any(same_place) {
        return Err(format!("There is already a server called {new} {place}."));
    }
    // Claude Code uses one server per name: local before project before user.
    for (other, k) in others.iter().filter(|o| !same_place(o)) {
        let folder = other
            .project
            .as_deref()
            .map(|p| sanitize(&folder_name(p)))
            .unwrap_or_default();
        match (&file.project, other.scope) {
            (None, Scope::Local | Scope::Project) => out.warnings.push(format!(
                "In {folder}, the {} server {new} is used instead of {new} for the user.",
                if other.scope == Scope::Local {
                    "local"
                } else {
                    "project"
                }
            )),
            (Some(p), Scope::User) => out.warnings.push(format!(
                "In {}, {new} is used instead of the user server {new}.",
                sanitize(&folder_name(p))
            )),
            (Some(p), Scope::Project) if same_project(k, p) => {
                out.warnings.push(format!(
                    "In {folder}, {new} is used instead of the project server {new}."
                ));
            }
            _ => {}
        }
    }
    out.claude.push(Op::RenameServer {
        project: file.project.clone(),
        name: file.name.clone(),
        new_name: name.to_owned(),
    });
    let projects: Vec<String> = match &file.project {
        Some(p) => vec![p.clone()],
        None => snapshot.projects.clone(),
    };
    for project in projects {
        out.claude.push(Op::RenameDisabled {
            project,
            name: file.name.clone(),
            new_name: name.to_owned(),
        });
    }
    if matches!(row.transport, TransportKind::Http | TransportKind::Sse) {
        out.warnings.push(
            "A sign-in Claude Code keeps for this server is under the old name and is not moved."
                .to_owned(),
        );
    }
    Ok(())
}

/// Moves a project's old switch-off list into the one Claude Code reads.
fn migrate_legacy(project: &str, snapshot: &Snapshot, out: &mut Resolved) -> Result<(), String> {
    let folder = sanitize(&folder_name(project));
    let legacy = snapshot
        .legacy_disabled
        .iter()
        .find(|l| l.project == project)
        .ok_or_else(|| {
            format!("{folder} has no old switch-off list any more. Refresh and try again.")
        })?;
    if legacy.malformed {
        return Err(format!(
            "The old switch-off list in {folder} holds something other than server names. Klefki leaves it alone; edit it in .claude.json by hand."
        ));
    }
    out.claude.push(Op::MigrateLegacyDisabled {
        project: project.to_owned(),
    });
    if !legacy.turns_off.is_empty() {
        out.notes.push(
            "Older Claude Code versions kept this list; current ones ignore it, so these servers have been on since."
                .to_owned(),
        );
    }
    Ok(())
}

fn duplicate(
    id: &ServerId,
    name: &str,
    targets: &[Target],
    adjust: &Adjust,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let (row, file) = find(id, snapshot, keys)?;
    let shown = &row.name;
    let from = match row.scope {
        Scope::Plugin | Scope::Managed | Scope::ClaudeAi => return Err(read_only(row)),
        Scope::User => None,
        Scope::Local => Some(
            file.project
                .clone()
                .ok_or_else(|| format!("{shown} has no project"))?,
        ),
        Scope::Project => {
            return Err(format!(
                "{shown} is defined in the repository's .mcp.json; Klefki cannot duplicate project servers yet."
            ));
        }
    };
    crate::model::ServerName::parse(name)
        .map_err(|e| format!("{:?} cannot be used as a server name: {e}.", sanitize(name)))?;
    if targets.is_empty() {
        return Err("Choose where the copy goes.".to_owned());
    }
    adjust_problem(adjust, row)
        .map_err(|why| format!("{shown} cannot be copied that way: {why}."))?;
    for to in new_places(name, targets, "this copy", None, snapshot, keys, out)? {
        if row.oauth.is_none() {
            out.claude.push(Op::CopyServer {
                from: from.clone(),
                name: file.name.clone(),
                to,
                new_name: name.to_owned(),
                base: adjust.base.clone(),
            });
            continue;
        }
        out.swaps.push(Swap {
            from: Spot {
                project: from.clone(),
                name: file.name.clone(),
            },
            remove: false,
            to: Spot {
                project: to,
                name: name.to_owned(),
            },
            edited: None,
            adjust: adjust.clone(),
        });
    }
    out.notes.push(
        if adjust.client.is_some() {
            "The copy keeps the original's environment and header values; it signs in as the client given here, and does not share a sign-in."
        } else {
            "The copy keeps the original's environment and header values; it does not share a sign-in."
        }
        .to_owned(),
    );
    Ok(())
}

/// Why `adjust` does not fit the server in `row`, said before anything is read; the definition it makes is checked
/// in full once the original's is read.
fn adjust_problem(adjust: &Adjust, row: &ServerRow) -> Result<(), String> {
    if let Some(base) = &adjust.base {
        if !matches!(
            row.transport,
            TransportKind::Http | TransportKind::Sse | TransportKind::Ws
        ) {
            return Err("it has no address".to_owned());
        }
        let to = base.to.as_str();
        if to.is_empty() || to.trim() != to || !to.contains("://") {
            return Err(
                "the new address must start with https:// and have no spaces around it".to_owned(),
            );
        }
    }
    if adjust.touches_sign_in() && row.oauth.is_none() {
        return Err("it does not sign in with OAuth".to_owned());
    }
    match adjust.client_problem() {
        Some(why) => Err(why.to_owned()),
        None => Ok(()),
    }
}

/// Where a new server called `name` goes, as the file spells each project: refused when a target is unknown or
/// already has a server by that name (the docstore refuses one added twice in a plan), with a warning wherever
/// Claude Code would use another server of that name instead. `what` names the new server in those warnings.
/// `leaving` is the server a move takes away in the same plan, which is not another server.
fn new_places(
    name: &str,
    targets: &[Target],
    what: &str,
    leaving: Option<&crate::model::ConnectionId>,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<Vec<Option<String>>, String> {
    let new = sanitize(name);
    // Every other server called `name` in the file (the new name is plain ASCII, so it is spelled as shown), with
    // its project key as the file spells it.
    let others: Vec<(&ServerRow, &FileKey)> = snapshot
        .servers
        .iter()
        .filter(|r| Some(&r.connection) != leaving)
        .filter_map(|r| Some((r, keys.get(&r.connection)?)))
        .filter(|(_, k)| k.name == name)
        .collect();
    let same_project = |k: Option<&str>, p: &str| {
        k.is_some_and(|q| inventory::normalise_project(q) == inventory::normalise_project(p))
    };
    // The organisation's servers come first wherever the new one would go (decision 009).
    if let Some(file) = &snapshot.organisation_only {
        out.warnings.push(format!(
            "Your organisation's {file} decides which servers Claude Code loads, so Claude Code will not load {what}."
        ));
    } else if snapshot.servers.iter().any(|r| {
        r.scope == Scope::Managed
            && crate::model::ConnectionId::parse(r.connection.as_str())
                .is_some_and(|(_, _, n)| n == name)
    }) {
        out.warnings.push(format!(
            "Your organisation sets a server called {new}, which Claude Code loads instead of {what}."
        ));
    }
    let mut places = Vec::new();
    for target in targets {
        let to = match target {
            Target::User => None,
            Target::Local(project) => {
                let known = snapshot
                    .projects
                    .iter()
                    .find(|p| {
                        inventory::normalise_project(p) == inventory::normalise_project(project)
                    })
                    .ok_or_else(|| {
                        format!(
                            "{} is not a project Claude Code knows about.",
                            sanitize(project)
                        )
                    })?;
                Some(known.clone())
            }
        };
        let place = match &to {
            None => "for the user".to_owned(),
            Some(p) => format!("in {}", sanitize(&folder_name(p))),
        };
        let same_place = |(r, k): &(&ServerRow, &FileKey)| match &to {
            None => r.scope == Scope::User,
            Some(p) => r.scope == Scope::Local && same_project(k.project.as_deref(), p),
        };
        if others.iter().any(same_place) {
            return Err(format!("There is already a server called {new} {place}."));
        }
        // Claude Code uses one server per name: local before project before user.
        for (other, k) in others.iter().filter(|o| !same_place(o)) {
            let folder = other
                .project
                .as_deref()
                .map(|p| sanitize(&folder_name(p)))
                .unwrap_or_default();
            let scope = match other.scope {
                Scope::User => "user",
                Scope::Local => "local",
                Scope::Project => "project",
                Scope::Plugin => "plugin",
                Scope::Managed => "organisation",
                Scope::ClaudeAi => "claude.ai",
            };
            match (&to, other.scope) {
                (None, Scope::Local | Scope::Project) => out.warnings.push(format!(
                    "In {folder}, the {scope} server {new} is used instead of {what}."
                )),
                (Some(p), Scope::User) => out.warnings.push(format!(
                    "In {}, {what} is used instead of the user server {new}.",
                    sanitize(&folder_name(p))
                )),
                (Some(p), Scope::Project) if same_project(k.project.as_deref(), p) => {
                    out.warnings.push(format!(
                        "In {folder}, {what} is used instead of the project server {new}."
                    ));
                }
                _ => {}
            }
        }
        places.push(to);
    }
    Ok(places)
}

/// Adds a new server from a definition the user gave (pasted, say): checked as an edit is, and shown in full
/// before anything is written, since Klefki checks it right after and that starts what it runs. One that signs in
/// with OAuth is left to Claude Code's own command, with its client secret (see [`CliAdd`]).
fn add(
    name: &str,
    json: &str,
    targets: &[Target],
    secret: Option<&Secret>,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    crate::model::ServerName::parse(name)
        .map_err(|e| format!("{:?} cannot be used as a server name: {e}.", sanitize(name)))?;
    let new = sanitize(name);
    if targets.is_empty() {
        return Err(format!("Choose where {new} goes."));
    }
    if json.len() > entry::MAX_ENTRY_BYTES {
        return Err(format!(
            "The definition of {new} is longer than {} KiB.",
            entry::MAX_ENTRY_BYTES / 1024
        ));
    }
    let value: Value = serde_json::from_str(json).map_err(|e| {
        format!(
            "The definition of {new} is not valid JSON: {}",
            sanitize(&e.to_string())
        )
    })?;
    entry::validate_new(&value).map_err(|why| format!("{new} cannot be added: {why}."))?;
    let oauth = value.get("oauth").is_some();
    match secret {
        Some(_) if !oauth => {
            return Err(format!(
                "{new} does not sign in with OAuth; only a server that signs in with OAuth takes a client secret."
            ));
        }
        Some(s) if s.is_empty() => {
            return Err(format!(
                "The client secret for {new} is empty. Leave it out if the server has none."
            ));
        }
        _ => {}
    }
    if oauth {
        typed_secret_refused(&new, &value)?;
        let lost: Vec<String> = value
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, _)| k)
            .filter(|k| swap::as_claude_writes(&value).get(k.as_str()).is_none())
            .map(|k| sanitize(k))
            .collect();
        if !lost.is_empty() {
            out.warnings.push(format!(
                "Claude Code keeps only the settings it uses, so {new} loses {}.",
                lost.join(", ")
            ));
        }
    }
    // Typed-in values are not shown (they may be secrets); which of the user's variables are read still is.
    let lines = describe(&shown_values(&value))
        .map_err(|why| format!("{new} cannot be reviewed safely, so it is not added: {why}."))?;
    for to in new_places(name, targets, "this server", None, snapshot, keys, out)? {
        let op = Op::AddServer {
            project: to.clone(),
            name: name.to_owned(),
            entry: value.clone(),
        };
        if !oauth {
            out.claude.push(op);
            continue;
        }
        if let Some(project) = &to
            && !Path::new(project).is_dir()
        {
            return Err(format!(
                "The folder of {} is not there, and Claude Code adds a project's server from inside it.",
                sanitize(&folder_name(project))
            ));
        }
        // Shown as Claude Code writes it.
        out.preview.push(Op::AddServer {
            project: to.clone(),
            name: name.to_owned(),
            entry: swap::as_claude_writes(&value),
        });
        out.cli.push(CliAdd {
            name: name.to_owned(),
            json: value.to_string(),
            project: to,
            secret: secret.cloned(),
            stored: false,
        });
    }
    let remote = value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "stdio");
    out.reviews.push(Review {
        title: format!("{new} would {}:", if remote { "connect" } else { "run" }),
        lines,
    });
    if oauth {
        out.notes.push(format!(
            "Claude Code adds it with its own command (claude mcp add-json){}. Sign in to it from Claude Code (/mcp) once it is added.",
            if secret.is_some() {
                ", and keeps the client secret in its own secure storage; Klefki keeps a copy in the system's secret store"
            } else {
                ""
            }
        ));
    }
    let note = "Klefki checks a new server once it is added: it starts its command, or connects to its address.";
    if !out.notes.iter().any(|n| n == note) {
        out.notes.push(note.to_owned());
    }
    Ok(())
}

/// Refuses a secret typed into the environment or headers of a server Claude Code's command adds: the command takes
/// the definition in its arguments, which other programs on the computer can read while it runs. The value is
/// never named, only its variable or header and what to write instead.
pub(crate) fn typed_secret_refused(new: &str, entry: &Value) -> Result<(), String> {
    for (key, what) in [("env", "variable"), ("headers", "header")] {
        let Some(Value::Object(values)) = entry.get(key) else {
            continue;
        };
        for (name, value) in values {
            if let Some(reference) = value.as_str().and_then(|v| secretlike::suggestion(name, v)) {
                return Err(format!(
                    "The {what} {} of {new} looks like a secret. Claude Code's command would show it to other programs on this computer while it adds the server, so write {} instead and set that variable where Claude Code starts.",
                    sanitize(name),
                    sanitize(&reference)
                ));
            }
        }
    }
    Ok(())
}

/// `entry` with every environment and header value hidden. A value that reads the user's variables shows only
/// those (`<hidden> ${SUFFIX}`), so the review still says which are read; one made of nothing but references, after
/// an auth scheme (`Bearer ${TOKEN}`), is shown as it is.
pub(crate) fn shown_values(entry: &Value) -> Value {
    let mut out = entry::masked(entry);
    for key in ["env", "headers"] {
        if let (Some(Value::Object(values)), Some(Value::Object(was))) =
            (out.get_mut(key), entry.get(key))
        {
            for (name, value) in values.iter_mut() {
                let Some(original) = was.get(name).and_then(Value::as_str) else {
                    continue;
                };
                let names = env_references(original);
                if names.is_empty() {
                    continue;
                }
                *value = Value::String(if only_references(original) {
                    original.to_owned()
                } else {
                    let refs: Vec<String> = names.iter().map(|n| format!("${{{n}}}")).collect();
                    format!("{} {}", entry::MASK, refs.join(" "))
                });
            }
        }
    }
    out
}

/// `${A}`, `${A}${B}`, `Bearer ${TOKEN}`: nothing typed in that could be a secret (no default value either).
fn only_references(value: &str) -> bool {
    let mut rest = ["Bearer ", "Basic ", "Token "]
        .iter()
        .find_map(|scheme| value.strip_prefix(scheme))
        .unwrap_or(value);
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let Some(inner) = rest.strip_prefix("${") else {
            return false;
        };
        let Some(end) = inner.find('}') else {
            return false;
        };
        let name = &inner[..end];
        let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return false;
        }
        rest = &inner[end + 1..];
    }
    true
}

fn approve(
    id: &ServerId,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let (row, file) = find(id, snapshot, keys)?;
    let shown = &row.name;
    if row.scope != Scope::Project {
        return Err(format!(
            "{shown} is not a project server, so it needs no approval."
        ));
    }
    if row.approval == Some(Approval::Approved) {
        return Ok(());
    }
    // The approval is by name, and the name shown must be the name written: one with hidden characters, or cut for
    // display, would approve something other than what the review says.
    if self::shown(&file.name).as_deref() != Ok(shown.as_str()) {
        return Err(format!(
            "{shown} cannot be reviewed safely, so it is not approved: its name in .mcp.json cannot be shown exactly."
        ));
    }
    let project = file
        .project
        .clone()
        .ok_or_else(|| format!("{shown} has no project"))?;
    if !inventory::is_safe_project_path(&project) || !Path::new(&project).is_dir() {
        return Err(format!(
            "The project folder of {shown} was not found, so it cannot be approved."
        ));
    }
    let entry = inventory::project_server(&project, &file.name)?;
    let lines = describe(&entry).map_err(|why| {
        format!("{shown} cannot be reviewed safely, so it is not approved: {why}.")
    })?;
    out.reviews.push(Review {
        title: format!("{shown} would be allowed to run:"),
        lines,
    });
    out.guards.push(Guard {
        project: project.clone(),
        name: file.name.clone(),
        digest: digest(&entry),
    });
    out.warnings.push(format!(
        "Claude Code will run {shown} from {} whenever you use that project. Only approve it if you trust the repository.",
        row.project.as_deref().unwrap_or_default()
    ));
    if !ignored_by_git(Path::new(&project)) {
        out.warnings.push(
            "The approval goes into .claude/settings.local.json, which is not listed in this repository's .gitignore or .git/info/exclude, so it could be committed by accident (a global ignore file may still cover it).".to_owned(),
        );
    }
    if row.approval == Some(Approval::Rejected) {
        out.claude.push(Op::ClearRejection {
            project: project.clone(),
            name: file.name.clone(),
        });
    }
    out.settings.entry(project).or_default().push(Op::Approve {
        name: file.name.clone(),
    });
    Ok(())
}

/// Everything the server would run or connect to, one fact per line. Refuses what cannot be shown whole and
/// exactly: unknown shapes, hidden characters, very long values.
pub(crate) fn describe(entry: &Value) -> Result<Vec<String>, String> {
    let object = entry.as_object().ok_or("the definition is not an object")?;
    let text = |key: &str| -> Result<Option<&str>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(format!("{key} is not text")),
        }
    };
    let kind = text("type")?.unwrap_or("stdio");
    let mut lines = Vec::new();
    match kind {
        "stdio" => {
            let command = text("command")?
                .filter(|c| !c.is_empty())
                .ok_or("it has no command")?;
            lines.push(format!("Command: {}", shown(command)?));
            lines.extend(environment_line("The command", "uses", command)?);
            match object.get("args") {
                None | Some(Value::Null) => {}
                Some(Value::Array(args)) => {
                    for (i, arg) in args.iter().enumerate() {
                        let arg = arg.as_str().ok_or("an argument is not text")?;
                        lines.push(format!("Argument {}: {}", i + 1, shown(arg)?));
                        lines.extend(environment_line(
                            &format!("Argument {}", i + 1),
                            "uses",
                            arg,
                        )?);
                    }
                }
                Some(_) => return Err("args is not a list".to_owned()),
            }
            match object.get("env") {
                None | Some(Value::Null) => {}
                Some(Value::Object(env)) => {
                    for (name, value) in env {
                        let value = value.as_str().ok_or("an environment value is not text")?;
                        lines.push(format!("Environment: {}={}", shown(name)?, shown(value)?));
                        lines.extend(environment_line(
                            &format!("Environment {}", shown(name)?),
                            "uses",
                            value,
                        )?);
                    }
                }
                Some(_) => return Err("env is not an object".to_owned()),
            }
        }
        "http" | "sse" | "ws" => {
            let raw = text("url")?.ok_or("it has no address")?;
            let url = url::Url::parse(raw).map_err(|_| "the address is not a valid URL")?;
            if !url.username().is_empty() || url.password().is_some() {
                return Err("the address contains a login".to_owned());
            }
            lines.push(format!("Connects to: {}", shown(raw)?));
            lines.extend(environment_line("The address", "sends", raw)?);
        }
        other => {
            return Err(format!(
                "its type {:?} is not one Claude Code understands",
                sanitize(other)
            ));
        }
    }
    if kind != "stdio" {
        match object.get("headers") {
            None | Some(Value::Null) => {}
            Some(Value::Object(headers)) => {
                if !headers.is_empty() {
                    let names: Result<Vec<_>, _> = headers.keys().map(|k| shown(k)).collect();
                    lines.push(format!("Sends headers: {}", names?.join(", ")));
                }
                // Values are not shown, but Claude Code fills `${VAR}` in from the user's environment, so which of
                // the user's variables would leave the machine is.
                for (name, value) in headers {
                    let value = value.as_str().ok_or("a header value is not text")?;
                    lines.extend(environment_line(
                        &format!("Header {}", shown(name)?),
                        "sends",
                        value,
                    )?);
                }
            }
            Some(_) => return Err("headers is not an object".to_owned()),
        }
        if let Some(helper) = text("headersHelper")? {
            lines.push(format!("Runs this to get headers: {}", shown(helper)?));
            lines.extend(environment_line("The header command", "uses", helper)?);
        }
        if let Some(oauth) = object.get("oauth").and_then(Value::as_object) {
            lines.push(oauth_line(oauth)?);
        }
    }
    Ok(lines)
}

/// `Signs in with OAuth as client a, callback port 8123, scopes "read write"`: the settings Claude Code signs in with.
/// None of them is a secret.
fn oauth_line(oauth: &serde_json::Map<String, Value>) -> Result<String, String> {
    let mut parts = Vec::new();
    if let Some(id) = oauth.get("clientId").and_then(Value::as_str) {
        parts.push(format!("as client {}", shown(id)?));
    }
    if let Some(port) = oauth.get("callbackPort").and_then(Value::as_u64) {
        parts.push(format!("callback port {port}"));
    }
    if let Some(scopes) = oauth.get("scopes").and_then(Value::as_str) {
        parts.push(format!("scopes \"{}\"", shown(scopes)?));
    }
    if let Some(url) = oauth.get("authServerMetadataUrl").and_then(Value::as_str) {
        parts.push(format!("sign-in metadata from {}", shown(url)?));
    }
    Ok(if parts.is_empty() {
        "Signs in with OAuth, registering itself".to_owned()
    } else {
        format!("Signs in with OAuth {}", parts.join(", "))
    })
}

/// The variable names in `${NAME}` and `${NAME:-default}`, as Claude Code expands them, each once, in order. A
/// reference inside a default is named too: it may be read.
fn env_references(value: &str) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    let mut rest = value;
    while let Some(at) = rest.find("${") {
        rest = &rest[at + 2..];
        let end = rest.find(['}', ':']).unwrap_or(rest.len());
        let name = &rest[..end];
        // Only a reference that is closed somewhere is expanded.
        let closed = rest[end..].contains('}');
        if !name.is_empty() && closed && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// "`what` uses (or sends) your environment variable X", when `value` fills any in.
fn environment_line(what: &str, verb: &str, value: &str) -> Result<Option<String>, String> {
    let vars = env_references(value)
        .into_iter()
        .map(shown)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!vars.is_empty()).then(|| {
        format!(
            "{what} {verb} your environment variable{} {}",
            if vars.len() == 1 { "" } else { "s" },
            vars.join(", ")
        )
    }))
}

/// A value exactly as it is, or an error when it cannot be shown that way.
fn shown(value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_REVIEW_CHARS {
        return Err(format!(
            "a value is longer than {MAX_REVIEW_CHARS} characters"
        ));
    }
    if sanitize(value) != value || value.chars().any(|c| c.is_control() || is_invisible(c)) {
        return Err("a value contains control or hidden characters".to_owned());
    }
    Ok(value.to_owned())
}

/// Characters that take no visible space or render blank, so a reviewed value would not read as it is: format
/// characters (zero-width, joiners, bidi, tags, variation selectors), line and paragraph separators, the soft
/// hyphen and the blank Hangul and Mongolian fillers.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}' | '\u{ffa0}'
        | '\u{fff0}'..='\u{fffb}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}')
}

/// Is `.claude/settings.local.json` named by the repository's own ignore files? Only the common spellings are
/// recognised; a global ignore file is not consulted, and the warning says so.
fn ignored_by_git(project: &Path) -> bool {
    const COVERING: [&str; 8] = [
        ".claude/settings.local.json",
        "settings.local.json",
        ".claude/*.local.json",
        "*.local.json",
        ".claude/",
        ".claude",
        ".claude/*",
        "**/settings.local.json",
    ];
    let texts: Vec<String> = [
        project.join(".gitignore"),
        project.join(".git").join("info").join("exclude"),
    ]
    .iter()
    .filter_map(|p| fsx::read_limited(p, MAX_GITIGNORE).ok().flatten())
    .collect();
    let lines = || texts.iter().flat_map(|t| t.lines()).map(str::trim);
    let covered = lines()
        .map(|l| l.trim_start_matches('/'))
        .any(|l| COVERING.contains(&l));
    // A `!` line can bring the file back whatever else is listed. Rule order is not worked out: any negation that
    // could name it means "may be committed".
    let negated = lines().filter_map(|l| l.strip_prefix('!')).any(|l| {
        l.contains(".claude") || l.contains("settings.local") || l.contains(".local.json")
    });
    covered && !negated
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ------------------------------------------------------------ profiles (decision 012)

    /// Users servers `a` and `b`, off in `/q`; a local server `l` in `/q` and `m` in `/r`; the connector
    /// `claude.ai Slack`.
    fn profiles_inventory(dir: &Path) -> inventory::Inventory {
        std::fs::write(
            dir.join(".claude.json"),
            json!({
                "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
                "claudeAiMcpEverConnected": ["claude.ai Slack"],
                "projects": {
                    "/q": {"disabledMcpServers": ["a", "b"], "mcpServers": {"l": {"command": "l"}}},
                    "/r": {"mcpServers": {"m": {"command": "m"}}}
                }
            })
            .to_string(),
        )
        .unwrap();
        inventory::load(&crate::config::ConfigPaths::in_dir(dir))
    }

    fn member(scope: Scope, project: Option<&str>, name: &str) -> crate::model::ConnectionId {
        crate::model::ConnectionId::new(scope, project, name)
    }

    fn use_it(
        inv: &inventory::Inventory,
        members: &[crate::model::ConnectionId],
        project: Option<&str>,
        only: bool,
    ) -> Result<Resolved, String> {
        let change = crate::engine::Change::UseProfile {
            name: "Work".into(),
            members: members.to_vec(),
            project: project.map(str::to_owned),
            only,
        };
        resolve(&[change], &inv.snapshot, &inv.keys)
    }

    fn off(project: &str, name: &str, disabled: bool) -> Op {
        Op::SetDisabled {
            project: project.into(),
            name: name.into(),
            disabled,
        }
    }

    #[test]
    fn using_a_profile_in_a_project_turns_its_servers_on_there_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let inv = profiles_inventory(dir.path());
        let members = [
            member(Scope::User, None, "a"),
            member(Scope::Local, Some("/q"), "l"),
            member(Scope::ClaudeAi, None, "claude.ai Slack"),
        ];
        let out = use_it(&inv, &members, Some("/q"), false).unwrap();
        assert_eq!(
            out.claude,
            [
                off("/q", "a", false),
                off("/q", "l", false),
                off("/q", "claude.ai Slack", false)
            ]
        );
        assert_eq!(out.summary, ["Use the profile Work in q"]);
        assert_eq!(out.action, "Use profile");
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn using_a_profile_alone_turns_off_every_other_server_there() {
        let dir = tempfile::tempdir().unwrap();
        let inv = profiles_inventory(dir.path());
        let members = [member(Scope::User, None, "a")];
        let out = use_it(&inv, &members, Some("/q"), true).unwrap();
        assert_eq!(
            out.claude,
            [
                off("/q", "a", false),
                off("/q", "b", true),
                off("/q", "l", true),
                off("/q", "claude.ai Slack", true)
            ]
        );
        assert_eq!(
            out.summary,
            ["Use the profile Work in q, and turn off every other server there"]
        );
        // Said plainly in the review: what else goes off. Not b, which is off in /q already (this said b too until
        // `using_a_profile_alone_names_only_what_it_really_turns_off`).
        assert!(
            out.warnings
                .iter()
                .any(|w| w.starts_with("This also turns off claude.ai Slack and l")),
            "{:?}",
            out.warnings
        );
    }

    /// Found trying `klefki-cli use`: a server already off there was said to be turned off too.
    #[test]
    fn using_a_profile_alone_names_only_what_it_really_turns_off() {
        let dir = tempfile::tempdir().unwrap();
        let inv = profiles_inventory(dir.path());
        // b is off in /q already; l and the connector are on.
        let out = use_it(&inv, &[member(Scope::User, None, "a")], Some("/q"), true).unwrap();
        assert!(
            out.warnings
                .iter()
                .any(|w| w == "This also turns off claude.ai Slack and l, which are not in Work."),
            "{:?}",
            out.warnings
        );
        // Everything else off already: nothing is said to go off.
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({
                "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
                "projects": {"/q": {"disabledMcpServers": ["b"]}}
            })
            .to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        let out = use_it(&inv, &[member(Scope::User, None, "a")], Some("/q"), true).unwrap();
        assert!(
            !out.warnings
                .iter()
                .any(|w| w.starts_with("This also turns off")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn using_a_profile_everywhere_covers_each_project_and_a_local_server_only_in_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let inv = profiles_inventory(dir.path());
        let members = [
            member(Scope::User, None, "a"),
            member(Scope::Local, Some("/q"), "l"),
        ];
        let out = use_it(&inv, &members, None, false).unwrap();
        assert_eq!(
            out.claude,
            [
                off("/q", "a", false),
                off("/r", "a", false),
                off("/q", "l", false)
            ]
        );
        assert!(
            out.warnings.iter().any(|w| w.contains("first opens later")),
            "{:?}",
            out.warnings
        );
        // In another project, a local server of /q has nothing to switch: said, not dropped.
        let out = use_it(&inv, &members, Some("/r"), false).unwrap();
        assert_eq!(out.claude, [off("/r", "a", false)]);
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("stay as they are: l (q)")),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn using_a_profile_alone_keeps_on_a_name_a_member_shares_with_another_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({
                "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
                "projects": {"/q": {"mcpServers": {"a": {"command": "local-a"}}}}
            })
            .to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        // Claude Code switches by name, so turning the local a off would turn the member off too.
        for members in [
            [member(Scope::User, None, "a")],
            [member(Scope::Local, Some("/q"), "a")],
        ] {
            let out = use_it(&inv, &members, Some("/q"), true).unwrap();
            assert_eq!(out.claude, [off("/q", "a", false), off("/q", "b", true)]);
            assert!(
                out.warnings
                    .iter()
                    .any(|w| w.starts_with("This also turns off b,")),
                "{:?}",
                out.warnings
            );
        }
    }

    /// Which projects a profile is on alone in: worked out from the files, so it is right after any change.
    #[test]
    fn a_profile_is_on_alone_where_its_servers_are_the_only_ones_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({
                "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
                "projects": {
                    "/q": {"disabledMcpServers": ["b"]},
                    "/r": {},
                    "/s": {"disabledMcpServers": ["a", "b"]},
                    "/t": {"disabledMcpServers": ["b", "l"], "mcpServers": {"l": {"command": "l"}}}
                }
            })
            .to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        let a = [member(Scope::User, None, "a")];
        // /r has b on too, /s has a off; in /t the local l is off as well.
        assert_eq!(profile_alone_in(&a, &inv.snapshot), ["/q", "/t"]);
        // A local server counts in its own project only.
        let a_and_l = [
            member(Scope::User, None, "a"),
            member(Scope::Local, Some("/t"), "l"),
        ];
        assert_eq!(profile_alone_in(&a_and_l, &inv.snapshot), ["/q"]);
        // Nothing of it loads anywhere: no project, though every server is off in /s.
        let gone = [member(Scope::User, None, "github")];
        assert!(profile_alone_in(&gone, &inv.snapshot).is_empty());
        assert!(profile_alone_in(&[], &inv.snapshot).is_empty());
    }

    /// Claude Code switches by name: a member that shares its name with another server is on with it. But it loads
    /// only when it is the one of that name Claude Code takes: the local server, not the user one it replaces.
    #[test]
    fn a_profile_is_on_alone_where_a_member_shares_its_name_with_another_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({
                "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
                "projects": {"/q": {"disabledMcpServers": ["b"], "mcpServers": {"a": {"command": "local-a"}}}}
            })
            .to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        assert_eq!(
            profile_alone_in(&[member(Scope::Local, Some("/q"), "a")], &inv.snapshot),
            ["/q"]
        );
        // Found in review of PR #58: the user a does not load in q, the local a does in its place.
        assert!(profile_alone_in(&[member(Scope::User, None, "a")], &inv.snapshot).is_empty());
    }

    /// The general rule: a profile is on alone in a project exactly when using it there alone changes nothing. Tried
    /// over every way of switching the servers of a project off, for several profiles.
    #[test]
    fn a_profile_is_on_alone_exactly_where_using_it_alone_changes_nothing() {
        let names = ["a", "b", "l", "claude.ai Slack"];
        let profiles = [
            vec![member(Scope::User, None, "a")],
            vec![
                member(Scope::User, None, "a"),
                member(Scope::User, None, "b"),
            ],
            vec![member(Scope::Local, Some("/q"), "l")],
            vec![
                member(Scope::ClaudeAi, None, "claude.ai Slack"),
                member(Scope::User, None, "github"),
            ],
            vec![member(Scope::Local, Some("/r"), "m")],
        ];
        for bits in 0..(1u32 << names.len()) {
            let off_in_q: Vec<&str> = names
                .iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, n)| *n)
                .collect();
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(".claude.json"),
                json!({
                    "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
                    "claudeAiMcpEverConnected": ["claude.ai Slack"],
                    "projects": {
                        "/q": {"disabledMcpServers": off_in_q, "mcpServers": {"l": {"command": "l"}}},
                        "/r": {"mcpServers": {"m": {"command": "m"}}}
                    }
                })
                .to_string(),
            )
            .unwrap();
            let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
            for members in &profiles {
                let alone = profile_alone_in(members, &inv.snapshot);
                for place in ["/q", "/r"] {
                    let unchanged = use_it(&inv, members, Some(place), true).is_ok_and(|out| {
                        let found = members.iter().any(|m| {
                            inv.snapshot.servers.iter().any(|r| {
                                &r.connection == m
                                    && crate::model::ConnectionId::parse(m.as_str()).is_some_and(
                                        |(_, p, _)| p.is_none() || p.as_deref() == Some(place),
                                    )
                            })
                        });
                        found
                            && out.claude.iter().all(|op| match op {
                                Op::SetDisabled {
                                    project,
                                    name,
                                    disabled,
                                } => {
                                    project == place
                                        && *disabled
                                            == (place == "/q" && off_in_q.contains(&name.as_str()))
                                }
                                _ => false,
                            })
                    });
                    assert_eq!(
                        alone.iter().any(|p| p == place),
                        unchanged,
                        "{members:?} in {place}, off in /q: {off_in_q:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_project_is_named_by_its_folder_and_the_one_above_when_two_share_a_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({"mcpServers": {"a": {"command": "a"}}, "projects": {"/work/app": {}, "/client/app": {}, "/q": {}}})
                .to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        let out = use_it(
            &inv,
            &[member(Scope::User, None, "a")],
            Some("/work/app"),
            false,
        )
        .unwrap();
        assert_eq!(out.summary, ["Use the profile Work in app (work)"]);
        let out = use_it(&inv, &[member(Scope::User, None, "a")], Some("/q"), false).unwrap();
        assert_eq!(out.summary, ["Use the profile Work in q"]);
    }

    #[test]
    fn a_profile_whose_servers_are_gone_says_so_and_one_with_none_left_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let inv = profiles_inventory(dir.path());
        let gone = member(Scope::User, None, "removed");
        let out = use_it(
            &inv,
            &[member(Scope::User, None, "a"), gone.clone()],
            Some("/q"),
            false,
        )
        .unwrap();
        assert!(
            out.notes
                .iter()
                .any(|n| n.starts_with("removed is in Work but no longer configured")),
            "{:?}",
            out.notes
        );
        let err = use_it(&inv, &[gone], Some("/q"), false).err().unwrap();
        assert!(err.contains("None of the servers in Work"), "{err}");
        let err = use_it(
            &inv,
            &[member(Scope::User, None, "a")],
            Some("/elsewhere"),
            false,
        )
        .err()
        .unwrap();
        assert!(
            err.contains("does not know the project /elsewhere"),
            "{err}"
        );
    }

    #[test]
    fn a_profile_with_an_unapproved_project_server_says_turning_it_on_does_not_approve_it() {
        let dir = tempfile::tempdir().unwrap();
        let q = dir.path().join("q");
        std::fs::create_dir_all(&q).unwrap();
        std::fs::write(
            q.join(".mcp.json"),
            r#"{"mcpServers":{"repo":{"command":"r"}}}"#,
        )
        .unwrap();
        let key = q.to_string_lossy().replace('\\', "/");
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({"projects": {&key: {}}}).to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::Project)
            .unwrap();
        let out = use_it(
            &inv,
            std::slice::from_ref(&row.connection),
            Some(&inv.snapshot.projects[0]),
            false,
        )
        .unwrap();
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("repo is a project server that you have not approved")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn no_change_can_name_a_plugins_server() {
        use crate::engine::Change;
        use crate::model::Target;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins/cache/p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".mcp.json"), r#"{"s":{"command":"x"}}"#).unwrap();
        std::fs::write(
            dir.path().join("plugins/installed_plugins.json"),
            json!({"plugins": {"p@m": [{"scope": "user", "installPath": root.to_string_lossy()}]}})
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"enabledPlugins":{"p@m":true}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join(".claude.json"), r#"{"projects":{"/q":{}}}"#).unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::Plugin)
            .unwrap();
        let id = row.id();
        for change in [
            Change::Approve { id: id.clone() },
            Change::Delete { id: id.clone() },
            Change::Replace {
                id: id.clone(),
                json: r#"{"command":"y"}"#.into(),
            },
            Change::Rename {
                id: id.clone(),
                name: "other".into(),
            },
            Change::Duplicate {
                id: id.clone(),
                name: "copy".into(),
                targets: vec![Target::User],
                adjust: Default::default(),
            },
            Change::Move {
                id: id.clone(),
                to: Target::User,
            },
        ] {
            let Err(why) = resolve(std::slice::from_ref(&change), &inv.snapshot, &inv.keys) else {
                panic!("{change:?} was planned");
            };
            assert!(
                why.contains("comes from the plugin p") && why.contains("plugin:p:s"),
                "{change:?}: {why}"
            );
        }
    }

    /// The organisation's folder `org` in `dir` with `managed-settings.json` setting the server `docs`, and the
    /// `.claude.json` given.
    fn organisation_inventory(dir: &Path, claude_json: Value) -> inventory::Inventory {
        let org = dir.join("org");
        std::fs::create_dir_all(&org).unwrap();
        std::fs::write(
            org.join("managed-settings.json"),
            r#"{"managedMcpServers":{"docs":{"type":"http","url":"https://docs.example.com/mcp"}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join(".claude.json"), claude_json.to_string()).unwrap();
        let mut paths = crate::config::ConfigPaths::in_dir(dir);
        paths.managed_dir = Some(org);
        inventory::load(&paths)
    }

    #[test]
    fn a_claude_ai_connector_is_switched_by_its_full_name_in_every_project_and_nothing_else() {
        use crate::engine::Change;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({"claudeAiMcpEverConnected": ["claude.ai Slack"], "projects": {"/q": {}}})
                .to_string(),
        )
        .unwrap();
        let inv = inventory::load(&crate::config::ConfigPaths::in_dir(dir.path()));
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::ClaudeAi)
            .unwrap();
        let change = Change::SetEnabled {
            id: row.id(),
            enabled: false,
        };
        let out = resolve(&[change], &inv.snapshot, &inv.keys).unwrap();
        assert_eq!(
            out.claude,
            [Op::SetDisabled {
                project: "/q".into(),
                name: "claude.ai Slack".into(),
                disabled: true,
            }]
        );
        let change = Change::Delete { id: row.id() };
        let Err(why) = resolve(&[change], &inv.snapshot, &inv.keys) else {
            panic!("a connector was deleted");
        };
        assert!(why.contains("changed on claude.ai"), "{why}");
    }

    #[test]
    fn an_organisations_server_is_switched_by_name_in_every_project_and_nothing_else() {
        use crate::engine::Change;
        use crate::model::Target;
        let dir = tempfile::tempdir().unwrap();
        let inv = organisation_inventory(dir.path(), json!({"projects": {"/q": {}, "/r": {}}}));
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::Managed)
            .unwrap();
        for enabled in [false, true] {
            let change = Change::SetEnabled {
                id: row.id(),
                enabled,
            };
            let out = resolve(&[change], &inv.snapshot, &inv.keys).unwrap();
            assert_eq!(
                out.claude,
                ["/q", "/r"]
                    .map(|project| Op::SetDisabled {
                        project: project.into(),
                        name: "docs".into(),
                        disabled: !enabled,
                    })
                    .to_vec()
            );
            assert!(out.settings.is_empty());
            assert!(
                out.warnings
                    .iter()
                    .any(|w| w.contains("your organisation sets") && w.contains("2 projects")),
                "{:?}",
                out.warnings
            );
        }
        let id = row.id();
        for change in [
            Change::Approve { id: id.clone() },
            Change::Delete { id: id.clone() },
            Change::Replace {
                id: id.clone(),
                json: r#"{"command":"y"}"#.into(),
            },
            Change::Rename {
                id: id.clone(),
                name: "other".into(),
            },
            Change::Duplicate {
                id: id.clone(),
                name: "copy".into(),
                targets: vec![Target::User],
                adjust: Default::default(),
            },
            Change::Move {
                id: id.clone(),
                to: Target::User,
            },
        ] {
            let Err(why) = resolve(std::slice::from_ref(&change), &inv.snapshot, &inv.keys) else {
                panic!("{change:?} was planned");
            };
            assert!(
                why.contains("set by your organisation")
                    && why.contains("managed-settings.json")
                    && why.contains("administrator"),
                "{change:?}: {why}"
            );
        }
    }

    #[test]
    fn adding_a_server_the_organisation_takes_the_place_of_says_so() {
        use crate::engine::Change;
        use crate::model::Target;
        let add = |inv: &inventory::Inventory, name: &str| {
            resolve(
                &[Change::Add {
                    name: name.into(),
                    json: r#"{"type":"http","url":"https://mine.example.com/mcp"}"#.into(),
                    targets: vec![Target::User],
                    secret: None,
                }],
                &inv.snapshot,
                &inv.keys,
            )
            .unwrap()
            .warnings
        };
        let dir = tempfile::tempdir().unwrap();
        let inv = organisation_inventory(dir.path(), json!({"projects": {"/q": {}}}));
        let warnings = add(&inv, "docs");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Your organisation sets a server called docs")),
            "{warnings:?}"
        );
        assert!(!add(&inv, "mine").iter().any(|w| w.contains("organisation")),);

        // With managed-mcp.json, Claude Code loads none of the user's servers.
        std::fs::write(
            dir.path().join("org/managed-mcp.json"),
            r#"{"mcpServers":{}}"#,
        )
        .unwrap();
        let mut paths = crate::config::ConfigPaths::in_dir(dir.path());
        paths.managed_dir = Some(dir.path().join("org"));
        let inv = inventory::load(&paths);
        let warnings = add(&inv, "mine");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("managed-mcp.json decides") && w.contains("will not load")),
            "{warnings:?}"
        );
    }

    /// A plugin `p@m` that is on, with one server `s`, and the projects given.
    fn plugin_inventory(dir: &Path, claude_json: Value) -> inventory::Inventory {
        let root = dir.join("plugins/cache/p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".mcp.json"), r#"{"s":{"command":"x"}}"#).unwrap();
        std::fs::write(
            dir.join("plugins/installed_plugins.json"),
            json!({"plugins": {"p@m": [{"scope": "user", "installPath": root.to_string_lossy()}]}})
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"enabledPlugins":{"p@m":true}}"#,
        )
        .unwrap();
        std::fs::write(dir.join(".claude.json"), claude_json.to_string()).unwrap();
        inventory::load(&crate::config::ConfigPaths::in_dir(dir))
    }

    #[test]
    fn a_plugins_server_is_switched_by_its_full_name_in_every_project() {
        use crate::engine::Change;
        let dir = tempfile::tempdir().unwrap();
        let inv = plugin_inventory(dir.path(), json!({"projects": {"/q": {}, "/r": {}}}));
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::Plugin)
            .unwrap();
        for enabled in [false, true] {
            let change = Change::SetEnabled {
                id: row.id(),
                enabled,
            };
            let out = resolve(&[change], &inv.snapshot, &inv.keys).unwrap();
            assert_eq!(
                out.claude,
                ["/q", "/r"]
                    .map(|project| Op::SetDisabled {
                        project: project.into(),
                        name: "plugin:p:s".into(),
                        disabled: !enabled,
                    })
                    .to_vec()
            );
            assert!(out.settings.is_empty());
            // As for a user server: per project, and a project opened later is not covered.
            assert!(
                out.warnings
                    .iter()
                    .any(|w| w.contains("2 projects") && w.contains("later")),
                "{:?}",
                out.warnings
            );
        }

        // No project yet: nowhere to switch it.
        let dir = tempfile::tempdir().unwrap();
        let inv = plugin_inventory(dir.path(), json!({}));
        let row = &inv.snapshot.servers[0];
        let why = resolve(
            &[Change::SetEnabled {
                id: row.id(),
                enabled: false,
            }],
            &inv.snapshot,
            &inv.keys,
        )
        .err()
        .unwrap();
        assert!(why.contains("no projects"), "{why}");
    }

    #[test]
    fn turning_on_a_server_whose_plugin_is_off_in_a_project_says_it_stays_off_there() {
        // Cross-review of #46: the review said it was switched on everywhere.
        use crate::engine::Change;
        let dir = tempfile::tempdir().unwrap();
        let (app, web) = (dir.path().join("app"), dir.path().join("web"));
        for p in [&app, &web] {
            std::fs::create_dir_all(p.join(".claude")).unwrap();
        }
        std::fs::write(
            app.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"p@m":false}}"#,
        )
        .unwrap();
        let cfg = dir.path().join("cfg");
        std::fs::create_dir_all(cfg.join("plugins")).unwrap();
        let inv = plugin_inventory(
            &cfg,
            json!({"projects": {app.to_string_lossy(): {}, web.to_string_lossy(): {}}}),
        );
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::Plugin)
            .unwrap();
        let warnings = |enabled: bool| {
            resolve(
                &[Change::SetEnabled {
                    id: row.id(),
                    enabled,
                }],
                &inv.snapshot,
                &inv.keys,
            )
            .unwrap()
            .warnings
        };
        let on = warnings(true);
        assert!(
            on.iter().any(|w| w
                == "Its plugin is off in app, so Claude Code still does not load plugin:p:s there."),
            "{on:?}"
        );
        // Turning it off: nothing to say about that.
        assert!(!warnings(false).iter().any(|w| w.contains("Its plugin")));
    }

    #[test]
    fn turning_off_a_server_a_plugins_repeats_says_that_one_loads_instead() {
        use crate::engine::Change;
        let dir = tempfile::tempdir().unwrap();
        // The plugin's server `s` runs `x`, as the user's `man` does: Claude Code leaves it out.
        let inv = plugin_inventory(
            dir.path(),
            json!({"mcpServers": {"man": {"command": "x"}, "other": {"command": "y"}}, "projects": {"/q": {}}}),
        );
        let row = |name: &str| {
            inv.snapshot
                .servers
                .iter()
                .find(|r| r.name == name)
                .unwrap()
        };
        let off = |name: &str, enabled: bool| {
            resolve(
                &[Change::SetEnabled {
                    id: row(name).id(),
                    enabled,
                }],
                &inv.snapshot,
                &inv.keys,
            )
            .unwrap()
            .warnings
        };
        let warnings = off("man", false);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("plugin:p:s") && w.contains("instead")),
            "{warnings:?}"
        );
        // Turning it on, or off a server nothing repeats, says nothing of the kind.
        for warnings in [off("man", true), off("other", false)] {
            assert!(
                !warnings.iter().any(|w| w.contains("instead")),
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn a_long_list_of_names_is_cut_to_the_first_few() {
        let names = |n: usize| (0..n).map(|i| format!("s{i}")).collect::<Vec<_>>();
        assert_eq!(some_of(&names(1)), "s0");
        assert_eq!(some_of(&names(6)), "s0, s1, s2, s3, s4 and s5");
        assert_eq!(some_of(&names(9)), "s0, s1, s2, s3, s4 and 4 more");
    }

    fn d(v: Value) -> Result<Vec<String>, String> {
        describe(&v)
    }

    #[test]
    fn a_stdio_server_is_shown_command_arguments_and_environment_in_full() {
        let lines = d(json!({
            "command": "node",
            "args": ["server.js", "--flag", "a b"],
            "env": {"NODE_OPTIONS": "--require ./evil.js", "TOKEN": "${TOKEN}"}
        }))
        .unwrap();
        assert_eq!(
            lines,
            [
                "Command: node",
                "Argument 1: server.js",
                "Argument 2: --flag",
                "Argument 3: a b",
                "Environment: NODE_OPTIONS=--require ./evil.js",
                "Environment: TOKEN=${TOKEN}",
                // Added when every place that expands `${VAR}` began to say so (only headers did before).
                "Environment TOKEN uses your environment variable TOKEN",
            ]
        );
    }

    #[test]
    fn a_remote_server_shows_its_address_header_names_and_any_helper_command() {
        let lines = d(json!({
            "type": "http",
            "url": "https://example.com/mcp?x=1",
            "headers": {"Authorization": "Bearer abc"},
            "headersHelper": "sh -c 'get-token'"
        }))
        .unwrap();
        assert_eq!(
            lines,
            [
                "Connects to: https://example.com/mcp?x=1",
                "Sends headers: Authorization",
                "Runs this to get headers: sh -c 'get-token'",
            ]
        );
        assert!(
            !lines.join("\n").contains("abc"),
            "header values are not shown"
        );
    }

    #[test]
    fn every_place_that_fills_in_your_environment_says_which_variables() {
        // Found reviewing main: only headers were checked, but Claude Code expands `${VAR}` in the address, the
        // command, its arguments, its environment and the header helper too.
        let lines = d(json!({
            "type": "http",
            "url": "https://example.com/mcp?k=${ANTHROPIC_API_KEY}",
            "headersHelper": "get-token --user ${USER}"
        }))
        .unwrap();
        assert!(
            lines.contains(
                &"The address sends your environment variable ANTHROPIC_API_KEY".to_owned()
            ),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"The header command uses your environment variable USER".to_owned()),
            "{lines:?}"
        );
        let lines = d(json!({
            "command": "${HOME}/bin/server",
            "args": ["--token", "${GH_TOKEN}", "plain"],
            "env": {"API_KEY": "${MY_SECRET:-none}", "MODE": "x"}
        }))
        .unwrap();
        for want in [
            "The command uses your environment variable HOME",
            "Argument 2 uses your environment variable GH_TOKEN",
            "Environment API_KEY uses your environment variable MY_SECRET",
        ] {
            assert!(lines.contains(&want.to_owned()), "{want}: {lines:?}");
        }
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("your environment"))
                .count(),
            3
        );
    }

    #[test]
    fn a_variable_inside_a_default_is_named_too() {
        // `${A:-${B}}`: both may be read, so both are named, once each and in order.
        assert_eq!(env_references("${A:-${B}}"), ["A", "B"]);
        assert_eq!(env_references("x ${A} ${A} ${B:-y}"), ["A", "B"]);
        assert!(env_references("${} ${:-x} $NOT ${UNCLOSED").is_empty());
        assert!(env_references("no variables").is_empty());
    }

    #[test]
    fn headers_that_fill_in_your_environment_say_which_variables() {
        // Claude Code expands `${VAR}` in headers: the repository chooses what of yours is sent, and where.
        let lines = d(json!({
            "type": "http",
            "url": "https://example.com/mcp",
            "headers": {
                "X-Leak": "Bearer ${ANTHROPIC_API_KEY}",
                "X-Two": "${A:-fallback} and ${B}",
                "X-Plain": "secret-literal"
            }
        }))
        .unwrap();
        assert_eq!(
            lines,
            [
                "Connects to: https://example.com/mcp",
                "Sends headers: X-Leak, X-Two, X-Plain",
                "Header X-Leak sends your environment variable ANTHROPIC_API_KEY",
                "Header X-Two sends your environment variables A, B",
            ]
        );
        assert!(!lines.join("\n").contains("secret-literal"));
        // A name that cannot be shown exactly is refused like any other value.
        assert!(
            d(json!({"type": "http", "url": "https://e.com", "headers": {"X": "${A\u{200b}B}"}}))
                .is_err()
        );
    }

    #[test]
    fn invisible_characters_are_refused_too() {
        for c in [
            '\u{200b}',
            '\u{200c}',
            '\u{200d}',
            '\u{2060}',
            '\u{feff}',
            '\u{ad}',
            '\u{2028}',
            '\u{2029}',
            '\u{e0041}',
            '\u{3164}',
            '\u{115f}',
            '\u{180e}',
        ] {
            let bad = json!({"command": "node", "args": [format!("a{c}b")]});
            assert!(d(bad).is_err(), "U+{:04X}", c as u32);
        }
        // Ordinary non-ASCII text is fine.
        assert!(d(json!({"command": "node", "args": ["café", "日本"]})).is_ok());
    }

    #[test]
    fn a_negated_ignore_line_means_the_file_may_be_committed() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            ".claude/\n!.claude/settings.local.json\n",
            "*.local.json\n!/.claude/settings.local.json\n",
        ] {
            std::fs::write(dir.path().join(".gitignore"), text).unwrap();
            assert!(!ignored_by_git(dir.path()), "{text:?}");
        }
    }

    #[test]
    fn what_cannot_be_shown_whole_and_exactly_is_refused() {
        for bad in [
            json!("text"),
            json!({}),
            json!({"command": ""}),
            json!({"command": 5}),
            json!({"command": "node", "args": "x"}),
            json!({"command": "node", "args": [1]}),
            json!({"command": "node", "env": {"A": 1}}),
            json!({"command": "no\u{1b}[2Kde"}),
            json!({"command": "node", "args": ["a\u{202e}b"]}),
            json!({"command": "node", "args": ["a\nb"]}),
            json!({"command": "x".repeat(MAX_REVIEW_CHARS + 1)}),
            json!({"type": "http", "url": "https://user:pw@example.com/"}),
            json!({"type": "http", "url": "not a url"}),
            json!({"type": "http"}),
            json!({"type": "carrier-pigeon", "url": "https://example.com"}),
        ] {
            assert!(d(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_gitignore_check_recognises_the_usual_spellings_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!ignored_by_git(dir.path()));
        for (file, text, expected) in [
            (".gitignore", "target\n", false),
            (".gitignore", "# .claude/settings.local.json\n", false),
            (".gitignore", ".claude/settings.local.json\n", true),
            (".gitignore", "/.claude/settings.local.json\n", true),
            (".gitignore", "node_modules\n  *.local.json  \n", true),
            (".gitignore", ".claude/settings.json\n", false),
        ] {
            std::fs::write(dir.path().join(file), text).unwrap();
            assert_eq!(ignored_by_git(dir.path()), expected, "{text:?}");
        }
        std::fs::remove_file(dir.path().join(".gitignore")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git").join("info")).unwrap();
        std::fs::write(
            dir.path().join(".git").join("info").join("exclude"),
            ".claude/\n",
        )
        .unwrap();
        assert!(ignored_by_git(dir.path()));
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count(1, "project"), "1 project");
        assert_eq!(count(0, "project"), "0 projects");
        assert_eq!(count(3, "project"), "3 projects");
    }

    #[test]
    fn folder_names_come_from_any_spelling() {
        for (path, want) in [
            ("C:/work/app", "app"),
            ("c:\\work\\app\\", "app"),
            ("/home/u/repo/", "repo"),
            ("/", "/"),
            ("C:/", "C:"),
        ] {
            assert_eq!(folder_name(path), want, "{path}");
        }
    }
}

#[cfg(test)]
mod profile_review_regressions {
    //! Found in review of PR #57: the review of using a profile must say what Claude Code will really do.
    use super::*;
    use serde_json::json;

    fn inv(dir: &Path, claude: serde_json::Value) -> inventory::Inventory {
        std::fs::write(dir.join(".claude.json"), claude.to_string()).unwrap();
        inventory::load(&crate::config::ConfigPaths::in_dir(dir))
    }

    fn use_profile(
        inv: &inventory::Inventory,
        members: Vec<crate::model::ConnectionId>,
        only: bool,
    ) -> Resolved {
        resolve(
            &[crate::engine::Change::UseProfile {
                name: "Work".into(),
                members,
                project: Some("/q".into()),
                only,
            }],
            &inv.snapshot,
            &inv.keys,
        )
        .unwrap()
    }

    /// The profile holds the user server `a`; in /q a local server `a` (another command) is what Claude Code loads.
    #[test]
    fn a_member_another_server_of_its_name_replaces_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let inv = inv(
            dir.path(),
            json!({
                "mcpServers": {"a": {"command": "user-a"}, "b": {"command": "b"}},
                "projects": {"/q": {"mcpServers": {"a": {"command": "local-a"}}}, "/r": {}}
            }),
        );
        let out = use_profile(
            &inv,
            vec![crate::model::ConnectionId::new(Scope::User, None, "a")],
            true,
        );
        assert!(
            out.warnings.contains(
                &"a will still not load in q: the local server a has its name there, and Claude Code loads that \
                  one instead."
                    .to_owned()
            ),
            "{:?}",
            out.warnings
        );
        // Not counted as turned off either: it stays on, under the member's name.
        assert!(
            out.warnings
                .iter()
                .any(|w| w.starts_with("This also turns off b,")),
            "{:?}",
            out.warnings
        );
        // In every project, only where it is replaced.
        let everywhere = resolve(
            &[crate::engine::Change::UseProfile {
                name: "Work".into(),
                members: vec![crate::model::ConnectionId::new(Scope::User, None, "a")],
                project: None,
                only: false,
            }],
            &inv.snapshot,
            &inv.keys,
        )
        .unwrap();
        assert!(
            everywhere
                .warnings
                .iter()
                .any(|w| w.starts_with("a will still not load in q:")),
            "{:?}",
            everywhere.warnings
        );
    }

    /// Found in review of PR #58: "Only these servers are on in q" was said while policy kept the only member from
    /// loading there, so Claude Code loaded nothing.
    #[test]
    fn a_profile_is_not_on_alone_where_policy_keeps_a_member_from_loading() {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({
            "mcpServers": {"a": {"command": "a"}, "b": {"command": "b"}},
            "projects": {"/q": {"disabledMcpServers": ["b"]}}
        });
        let a = [crate::model::ConnectionId::new(Scope::User, None, "a")];
        // Without the rule, it is on alone in q.
        let inv0 = inv(dir.path(), config.clone());
        assert_eq!(profile_alone_in(&a, &inv0.snapshot), ["/q"]);
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"deniedMcpServers":[{"serverName":"a"}]}"#,
        )
        .unwrap();
        let inv = inv(dir.path(), config);
        assert!(profile_alone_in(&a, &inv.snapshot).is_empty());
    }

    /// The variant: a project server not approved for its project is not loaded, so not on alone either; approved,
    /// it is.
    #[test]
    fn a_profile_is_not_on_alone_where_a_member_is_a_project_server_not_approved() {
        let dir = tempfile::tempdir().unwrap();
        let q = dir.path().join("q");
        std::fs::create_dir_all(&q).unwrap();
        std::fs::write(
            q.join(".mcp.json"),
            r#"{"mcpServers":{"repo":{"command":"r"}}}"#,
        )
        .unwrap();
        let key = q.to_string_lossy().replace('\\', "/");
        let alone = |entry: serde_json::Value| {
            let inv = inv(dir.path(), json!({"projects": {&key: entry}}));
            let row = inv
                .snapshot
                .servers
                .iter()
                .find(|r| r.scope == Scope::Project)
                .unwrap();
            profile_alone_in(std::slice::from_ref(&row.connection), &inv.snapshot)
        };
        assert!(alone(json!({})).is_empty());
        assert!(alone(json!({"disabledMcpjsonServers": ["repo"]})).is_empty());
        assert_eq!(alone(json!({"enabledMcpjsonServers": ["repo"]})).len(), 1);
    }

    /// A member that policy blocks stays off whatever disabledMcpServers says.
    #[test]
    fn a_member_policy_blocks_is_said() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"deniedMcpServers":[{"serverName":"a"}]}"#,
        )
        .unwrap();
        let inv = inv(
            dir.path(),
            json!({
                "mcpServers": {"a": {"command": "a"}},
                "projects": {"/q": {"disabledMcpServers": ["a"]}}
            }),
        );
        let row = inv.snapshot.servers.iter().find(|r| r.name == "a").unwrap();
        assert!(
            row.blocked.is_some() || row.not_loaded.is_some(),
            "fixture: a not seen as blocked: {row:?}"
        );
        let out = use_profile(&inv, vec![row.connection.clone()], false);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.starts_with("a will still not load in q: ")
                    && w.contains("deniedMcpServers")),
            "{:?}",
            out.warnings
        );
    }

    /// The decision says gone members are named in the review.
    #[test]
    fn a_member_no_longer_configured_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let inv = inv(
            dir.path(),
            json!({"mcpServers": {"a": {"command": "a"}}, "projects": {"/q": {}}}),
        );
        let out = use_profile(
            &inv,
            vec![
                crate::model::ConnectionId::new(Scope::User, None, "a"),
                crate::model::ConnectionId::new(Scope::User, None, "github"),
            ],
            false,
        );
        assert!(
            out.notes
                .iter()
                .any(|n| n.starts_with("github is in Work but no longer configured")),
            "{:?}",
            out.notes
        );
    }

    /// A plugin's server whose plugin is off in the project does not load there, whatever its own switch says.
    #[test]
    fn a_member_whose_plugin_is_off_there_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins/cache/p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".mcp.json"), r#"{"s":{"command":"x"}}"#).unwrap();
        std::fs::write(
            dir.path().join("plugins/installed_plugins.json"),
            json!({"plugins": {"p@m": [{"scope": "user", "installPath": root.to_string_lossy()}]}})
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"enabledPlugins":{"p@m":true}}"#,
        )
        .unwrap();
        // Its own project turns the plugin off.
        let q = dir.path().join("q");
        std::fs::create_dir_all(q.join(".claude")).unwrap();
        std::fs::write(
            q.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"p@m":false}}"#,
        )
        .unwrap();
        let key = q.to_string_lossy().replace('\\', "/");
        let inv = inv(dir.path(), json!({"projects": {&key: {}}}));
        let row = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::Plugin)
            .unwrap();
        assert!(
            row.plugin.as_ref().is_some_and(|p| !p.off_in.is_empty()),
            "fixture: {row:?}"
        );
        let out = resolve(
            &[crate::engine::Change::UseProfile {
                name: "Work".into(),
                members: vec![row.connection.clone()],
                project: Some(inv.snapshot.projects[0].clone()),
                only: false,
            }],
            &inv.snapshot,
            &inv.keys,
        )
        .unwrap();
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("will still not load in q: its plugin p is off there.")),
            "{:?}",
            out.warnings
        );
    }

    /// Two plugins bring servers of the same command: Claude Code loads the first and leaves the second out.
    fn two_plugins(dir: &Path) -> inventory::Inventory {
        let mut plugins = serde_json::Map::new();
        let mut enabled = serde_json::Map::new();
        for p in ["p1", "p2"] {
            let root = dir.join("plugins/cache").join(p);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join(".mcp.json"),
                json!({format!("dup{}", &p[1..]): {"command": "echo", "args": ["dup"]}})
                    .to_string(),
            )
            .unwrap();
            plugins.insert(
                format!("{p}@m"),
                json!([{"scope": "user", "installPath": root.to_string_lossy()}]),
            );
            enabled.insert(format!("{p}@m"), json!(true));
        }
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(
            dir.join("plugins/installed_plugins.json"),
            json!({"plugins": plugins}).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("settings.json"),
            json!({"enabledPlugins": enabled}).to_string(),
        )
        .unwrap();
        inv(dir, json!({"projects": {"/q": {}}}))
    }

    /// With "only", the server a duplicate repeats may be the one this change turns off: then the duplicate loads,
    /// and the review must not say it will not (found in re-review, PR #57).
    #[test]
    fn a_duplicate_is_judged_after_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let inv = two_plugins(dir.path());
        let dup = inv
            .snapshot
            .servers
            .iter()
            .find(|r| r.plugin.as_ref().is_some_and(|p| p.same_as.is_some()))
            .unwrap_or_else(|| panic!("fixture: no duplicate in {:?}", inv.snapshot.servers));
        let warned = |out: &Resolved| {
            out.warnings
                .iter()
                .any(|w| w.contains("same command or address"))
        };
        // Alone: what it repeats goes off, so it loads.
        let out = use_profile(&inv, vec![dup.connection.clone()], true);
        assert!(!warned(&out), "{:?}", out.warnings);
        // Without "only", what it repeats stays on, and it stays left out.
        let out = use_profile(&inv, vec![dup.connection.clone()], false);
        assert!(warned(&out), "{:?}", out.warnings);
    }
}
