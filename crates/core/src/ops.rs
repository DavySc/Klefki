//! Turns what the user asked for into document edits. Needs the current inventory to know which projects exist.
use crate::api::{
    Approval, Change, Review, Scope, ServerId, ServerRow, Snapshot, Target, TransportKind,
};
use crate::config::docstore::Op;
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

pub(crate) fn resolve(changes: &[Change], snapshot: &Snapshot) -> Result<Resolved, String> {
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
        };
        if !verbs.contains(&verb) {
            verbs.push(verb);
        }
        out.destructive |= matches!(change, Change::Delete { .. });
        out.summary.push(summarise(change, snapshot));
        match change {
            Change::SetEnabled { id, enabled } => set_enabled(id, *enabled, snapshot, &mut out)?,
            Change::Approve { id } => approve(id, snapshot, &mut out)?,
            Change::Delete { id } => delete(id, snapshot, &mut out)?,
            Change::Replace { id, json } => replace(id, json, snapshot, &mut out)?,
            Change::Rename { id, name } => rename(id, name, snapshot, &mut out)?,
            Change::Duplicate { id, name, targets } => {
                duplicate(id, name, targets, snapshot, &mut out)?;
            }
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
    out.warnings.dedup();
    Ok(out)
}

/// One plain sentence for a change, naming the server and where it applies.
fn summarise(change: &Change, snapshot: &Snapshot) -> String {
    let (id, verb) = match change {
        Change::SetEnabled { id, enabled } => (id, if *enabled { "Turn on" } else { "Turn off" }),
        Change::Approve { id } => (id, "Approve"),
        Change::Delete { id } => (id, "Delete"),
        Change::Replace { id, .. } => (id, "Save changes to"),
        Change::Rename { id, name } => {
            let place = match (id.scope, id.project.as_deref()) {
                (Scope::User, _) => " (user server)".to_owned(),
                (_, Some(project)) => format!(" in {}", sanitize(&folder_name(project))),
                (_, None) => String::new(),
            };
            return format!("Rename {} to {}{place}", sanitize(&id.name), sanitize(name));
        }
        Change::Duplicate { id, name, targets } => {
            let user = targets.contains(&Target::User);
            let folders: Vec<String> = targets
                .iter()
                .filter_map(|t| match t {
                    Target::Local(p) => Some(sanitize(&folder_name(p))),
                    Target::User => None,
                })
                .collect();
            let place = match (user, folders.is_empty()) {
                (true, true) => " for the user".to_owned(),
                (true, false) => format!(" for the user and in {}", and_list(&folders)),
                (false, _) => format!(" in {}", and_list(&folders)),
            };
            return format!(
                "Duplicate {} as {}{place}",
                sanitize(&id.name),
                sanitize(name)
            );
        }
    };
    let name = sanitize(&id.name);
    let place = match (id.scope, id.project.as_deref()) {
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

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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

/// A server as the files spell it: its project key and name before they were cleaned and cut for display. Every op
/// is written with these, so a name with a hidden character, or a long one, changes that server and no other.
struct OnDisk {
    project: Option<String>,
    name: String,
}

fn on_disk(row: &ServerRow) -> Result<OnDisk, String> {
    let (_, project, name) = crate::model::ConnectionId::parse(row.connection.as_str())
        .ok_or_else(|| {
            format!(
                "{} cannot be identified in the configuration",
                sanitize(&row.name)
            )
        })?;
    Ok(OnDisk { project, name })
}

fn find<'a>(id: &ServerId, snapshot: &'a Snapshot) -> Result<&'a ServerRow, String> {
    snapshot.servers.iter().find(|r| r.is(id)).ok_or_else(|| {
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
    out: &mut Resolved,
) -> Result<(), String> {
    let row = find(id, snapshot)?;
    let disk = on_disk(row)?;
    let shown = sanitize(&id.name);
    let disabled = !enabled;
    match row.scope {
        Scope::Local | Scope::Project => {
            let project = disk
                .project
                .ok_or_else(|| format!("{shown} has no project"))?;
            out.claude.push(Op::SetDisabled {
                project,
                name: disk.name.clone(),
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
                    name: disk.name.clone(),
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
fn own_server(id: &ServerId, snapshot: &Snapshot, what: &str) -> Result<OnDisk, String> {
    let row = find(id, snapshot)?;
    let shown = sanitize(&id.name);
    let disk = on_disk(row)?;
    match row.scope {
        Scope::User => Ok(OnDisk {
            project: None,
            ..disk
        }),
        Scope::Local if disk.project.is_some() => Ok(disk),
        Scope::Local => Err(format!("{shown} has no project")),
        Scope::Project => Err(format!(
            "{shown} is defined in the repository's .mcp.json, and Klefki does not {what} files in a repository. Change it there, or switch it off here."
        )),
    }
}

fn delete(id: &ServerId, snapshot: &Snapshot, out: &mut Resolved) -> Result<(), String> {
    let disk = own_server(id, snapshot, "delete from")?;
    out.claude.push(Op::RemoveServer {
        project: disk.project,
        name: disk.name,
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
    out: &mut Resolved,
) -> Result<(), String> {
    let disk = own_server(id, snapshot, "edit")?;
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
        project: disk.project,
        name: disk.name,
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
    out: &mut Resolved,
) -> Result<(), String> {
    let disk = own_server(id, snapshot, "change")?;
    let row = find(id, snapshot)?;
    let shown = sanitize(&id.name);
    let new = sanitize(name);
    crate::model::ServerName::parse(name)
        .map_err(|e| format!("{:?} cannot be used as a server name: {e}.", sanitize(name)))?;
    let place = match &disk.project {
        None => "for the user".to_owned(),
        Some(p) => format!("in {}", sanitize(&folder_name(p))),
    };
    if name == disk.name {
        return Err(format!("{shown} is already called {new}."));
    }
    let same_project =
        |q: &str, p: &str| inventory::normalise_project(q) == inventory::normalise_project(p);
    let others: Vec<&ServerRow> = snapshot
        .servers
        .iter()
        .filter(|r| on_disk(r).is_ok_and(|d| d.name == name))
        .collect();
    let same_place = |r: &&ServerRow| match &disk.project {
        None => r.scope == Scope::User,
        Some(p) => {
            r.scope == Scope::Local && r.project.as_deref().is_some_and(|q| same_project(q, p))
        }
    };
    if others.iter().any(same_place) {
        return Err(format!("There is already a server called {new} {place}."));
    }
    // Claude Code uses one server per name: local before project before user.
    for other in others.iter().filter(|r| !same_place(r)) {
        let folder = other
            .project
            .as_deref()
            .map(|p| sanitize(&folder_name(p)))
            .unwrap_or_default();
        match (&disk.project, other.scope) {
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
            (Some(p), Scope::Project)
                if other.project.as_deref().is_some_and(|q| same_project(q, p)) =>
            {
                out.warnings.push(format!(
                    "In {folder}, {new} is used instead of the project server {new}."
                ));
            }
            _ => {}
        }
    }
    out.claude.push(Op::RenameServer {
        project: disk.project.clone(),
        name: disk.name.clone(),
        new_name: name.to_owned(),
    });
    let projects: Vec<String> = match &disk.project {
        Some(p) => vec![p.clone()],
        None => snapshot.projects.clone(),
    };
    for project in projects {
        out.claude.push(Op::RenameDisabled {
            project,
            name: disk.name.clone(),
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

fn duplicate(
    id: &ServerId,
    name: &str,
    targets: &[Target],
    snapshot: &Snapshot,
    out: &mut Resolved,
) -> Result<(), String> {
    let row = find(id, snapshot)?;
    let shown = sanitize(&id.name);
    let disk = on_disk(row)?;
    let from = match row.scope {
        Scope::User => None,
        Scope::Local => Some(
            disk.project
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
    let new = sanitize(name);
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
        let same_place = |r: &&ServerRow| match &to {
            None => r.scope == Scope::User,
            Some(p) => {
                r.scope == Scope::Local
                    && r.project.as_deref().is_some_and(|q| {
                        inventory::normalise_project(q) == inventory::normalise_project(p)
                    })
            }
        };
        let others: Vec<&ServerRow> = snapshot.servers.iter().filter(|r| r.name == name).collect();
        if others.iter().any(same_place) {
            return Err(format!(
                "There is already a server called {new} {}.",
                match &to {
                    None => "for the user".to_owned(),
                    Some(p) => format!("in {}", sanitize(&folder_name(p))),
                }
            ));
        }
        // Claude Code uses one server per name: local before project before user.
        for other in others.iter().filter(|r| !same_place(r)) {
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
                    "In {folder}, the {scope} server {new} is used instead of this copy."
                )),
                (Some(p), Scope::User) => out.warnings.push(format!(
                    "In {}, this copy is used instead of the user server {new}.",
                    sanitize(&folder_name(p))
                )),
                (Some(p), Scope::Project)
                    if other.project.as_deref().is_some_and(|q| {
                        inventory::normalise_project(q) == inventory::normalise_project(p)
                    }) =>
                {
                    out.warnings.push(format!(
                        "In {folder}, this copy is used instead of the project server {new}."
                    ));
                }
                _ => {}
            }
        }
        out.claude.push(Op::CopyServer {
            from: from.clone(),
            name: disk.name.clone(),
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

fn approve(id: &ServerId, snapshot: &Snapshot, out: &mut Resolved) -> Result<(), String> {
    let row = find(id, snapshot)?;
    let shown = sanitize(&id.name);
    if row.scope != Scope::Project {
        return Err(format!(
            "{shown} is not a project server, so it needs no approval."
        ));
    }
    if row.approval == Some(Approval::Approved) {
        return Ok(());
    }
    let OnDisk { project, name } = on_disk(row)?;
    let project = project.ok_or_else(|| format!("{shown} has no project"))?;
    if !inventory::is_safe_project_path(&project) || !Path::new(&project).is_dir() {
        return Err(format!(
            "The project folder of {shown} was not found, so it cannot be approved."
        ));
    }
    let entry = inventory::project_server(&project, &name)?;
    let lines = describe(&entry).map_err(|why| {
        format!("{shown} cannot be reviewed safely, so it is not approved: {why}.")
    })?;

    out.reviews.push(Review {
        title: format!("{shown} would be allowed to run:"),
        lines,
    });
    out.guards.push(Guard {
        project: project.clone(),
        name: name.clone(),
        digest: digest(&entry),
    });
    out.warnings.push(format!(
        "Claude Code will run {shown} from {} whenever you use that project. Only approve it if you trust the repository.",
        sanitize(&project)
    ));
    if !ignored_by_git(Path::new(&project)) {
        out.warnings.push(
            "The approval goes into .claude/settings.local.json, which is not listed in this repository's .gitignore or .git/info/exclude, so it could be committed by accident (a global ignore file may still cover it).".to_owned(),
        );
    }
    if row.approval == Some(Approval::Rejected) {
        out.claude.push(Op::ClearRejection {
            project: project.clone(),
            name: name.clone(),
        });
    }
    out.settings
        .entry(project)
        .or_default()
        .push(Op::Approve { name });
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
