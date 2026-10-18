//! Turns what the user asked for into document edits. Needs the current inventory to know which projects exist.
use crate::api::{
    Approval, Change, Review, Scope, ServerId, ServerRow, Snapshot, Target, TransportKind,
};
use crate::config::docstore::Op;
use crate::config::inventory::{FileKey, FileKeys};
use crate::config::{entry, fsx, inventory};
use crate::sanitize::sanitize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
    format!("{:x}", Sha256::digest(entry.to_string().as_bytes()))
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
        };
        if !verbs.contains(&verb) {
            verbs.push(verb);
        }
        out.destructive |= matches!(change, Change::Delete { .. });
        out.summary.push(summarise(change, snapshot));
        match change {
            Change::SetEnabled { id, enabled } => {
                set_enabled(id, *enabled, snapshot, keys, &mut out)?;
            }
            Change::Approve { id } => approve(id, snapshot, keys, &mut out)?,
            Change::Delete { id } => delete(id, snapshot, keys, &mut out)?,
            Change::Replace { id, json } => replace(id, json, snapshot, keys, &mut out)?,
            Change::Rename { id, name } => rename(id, name, snapshot, keys, &mut out)?,
            Change::Duplicate { id, name, targets } => {
                duplicate(id, name, targets, snapshot, keys, &mut out)?;
            }
            Change::MigrateLegacyDisabled { project } => {
                migrate_legacy(project, snapshot, &mut out)?;
            }
            Change::Add {
                name,
                json,
                targets,
            } => add(name, json, targets, snapshot, keys, &mut out)?,
        }
    }
    out.action = match verbs.as_slice() {
        [one] => (*one).to_owned(),
        _ => "Apply".to_owned(),
    };
    if !out.claude.is_empty() {
        out.notes.push(
            "A running Claude Code session may need a restart before it notices this.".to_owned(),
        );
    }
    // Several changes can raise the same point; it is said once, where it first came up.
    for said in [&mut out.warnings, &mut out.notes] {
        let mut seen = std::collections::HashSet::new();
        said.retain(|s| seen.insert(s.clone()));
    }
    Ok(out)
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
        Change::Duplicate { id, name, targets } => {
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
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Like [`and_list`], naming the first few and counting the rest, so a long list keeps the summary one line.
/// The app's notice cuts names the same way (`shell::some_names`, which cannot call this); keep the two alike.
fn some_of(items: &[String]) -> String {
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
    inventory::locate(snapshot, keys, id).ok_or_else(|| {
        format!(
            "{} is no longer in the configuration. Refresh and try again.",
            sanitize(&id.name)
        )
    })
}

fn set_enabled(
    id: &ServerId,
    enabled: bool,
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
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
        Scope::User => {
            if snapshot.projects.is_empty() {
                return Err(format!(
                    "Claude Code knows no projects yet, so there is nowhere to switch {shown} {}.",
                    if enabled { "on" } else { "off" }
                ));
            }
            for project in &snapshot.projects {
                out.claude.push(Op::SetDisabled {
                    project: project.clone(),
                    name: file.name.clone(),
                    disabled,
                });
            }
            out.warnings.push(format!(
                "{shown} is a user server, and Claude Code switches it per project. This changes the {} it knows today; a project it first opens later is not covered.",
                count(snapshot.projects.len(), "project")
            ));
        }
    }
    Ok(())
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
    // Checked again after hidden values are put back; this catches mistakes before anything is read.
    entry::validate(&value).map_err(|why| format!("The definition is not usable: {why}."))?;
    out.claude.push(Op::ReplaceServer {
        project: file.project.clone(),
        name: file.name.clone(),
        entry: value,
    });
    Ok(())
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
    snapshot: &Snapshot,
    keys: &FileKeys,
    out: &mut Resolved,
) -> Result<(), String> {
    let (row, file) = find(id, snapshot, keys)?;
    let shown = &row.name;
    let from = match row.scope {
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
    for to in new_places(name, targets, "this copy", snapshot, keys, out)? {
        out.claude.push(Op::CopyServer {
            from: from.clone(),
            name: file.name.clone(),
            to,
            new_name: name.to_owned(),
        });
    }
    out.notes.push(
        "The copy keeps the original's environment and header values; it does not share a sign-in."
            .to_owned(),
    );
    Ok(())
}

/// Where a new server called `name` goes, as the file spells each project: refused when a target is unknown or
/// already has a server by that name (the docstore refuses one added twice in a plan), with a warning wherever
/// Claude Code would use another server of that name instead. `what` names the new server in those warnings.
fn new_places(
    name: &str,
    targets: &[Target],
    what: &str,
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
        .filter_map(|r| Some((r, keys.get(&r.connection)?)))
        .filter(|(_, k)| k.name == name)
        .collect();
    let same_project = |k: Option<&str>, p: &str| {
        k.is_some_and(|q| inventory::normalise_project(q) == inventory::normalise_project(p))
    };
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
/// before anything is written, since Klefki checks it right after and that starts what it runs.
fn add(
    name: &str,
    json: &str,
    targets: &[Target],
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
    entry::validate(&value).map_err(|why| format!("{new} cannot be added: {why}."))?;
    // Typed-in values are not shown (they may be secrets); which of the user's variables are read still is.
    let lines = describe(&shown_values(&value))
        .map_err(|why| format!("{new} cannot be reviewed safely, so it is not added: {why}."))?;
    for to in new_places(name, targets, "this server", snapshot, keys, out)? {
        out.claude.push(Op::AddServer {
            project: to,
            name: name.to_owned(),
            entry: value.clone(),
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
    let note = "Klefki checks a new server once it is added: it starts its command, or connects to its address.";
    if !out.notes.iter().any(|n| n == note) {
        out.notes.push(note.to_owned());
    }
    Ok(())
}

/// `entry` with every environment and header value hidden. A value that reads the user's variables shows only
/// those (`<hidden> ${SUFFIX}`), so the review still says which are read; one made of nothing but references, after
/// an auth scheme (`Bearer ${TOKEN}`), is shown as it is.
fn shown_values(entry: &Value) -> Value {
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
fn describe(entry: &Value) -> Result<Vec<String>, String> {
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
    }
    Ok(lines)
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
