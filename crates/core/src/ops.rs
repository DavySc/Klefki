//! Turns what the user asked for into document edits. Needs the current inventory to know which projects exist.
use crate::api::{Approval, Change, Review, Scope, ServerId, ServerRow, Snapshot};
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
    let shown = sanitize(&id.name);
    let disabled = !enabled;
    match row.scope {
        Scope::Local | Scope::Project => {
            let project = row
                .project
                .clone()
                .ok_or_else(|| format!("{shown} has no project"))?;
            out.claude.push(Op::SetDisabled {
                project,
                name: row.name.clone(),
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
                    name: row.name.clone(),
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

/// Where a user or local server lives, for an op; project servers are refused (the repository's file).
fn own_server(id: &ServerId, snapshot: &Snapshot, what: &str) -> Result<Option<String>, String> {
    let row = find(id, snapshot)?;
    let shown = sanitize(&id.name);
    match row.scope {
        Scope::User => Ok(None),
        Scope::Local => row
            .project
            .clone()
            .map(Some)
            .ok_or_else(|| format!("{shown} has no project")),
        Scope::Project => Err(format!(
            "{shown} is defined in the repository's .mcp.json, and Klefki does not {what} files in a repository. Change it there, or switch it off here."
        )),
    }
}

fn delete(id: &ServerId, snapshot: &Snapshot, out: &mut Resolved) -> Result<(), String> {
    let project = own_server(id, snapshot, "delete from")?;
    out.claude.push(Op::RemoveServer {
        project,
        name: id.name.clone(),
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
    let project = own_server(id, snapshot, "edit")?;
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
        project,
        name: id.name.clone(),
        entry: value,
    });
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
    let project = row
        .project
        .clone()
        .ok_or_else(|| format!("{shown} has no project"))?;
    if !inventory::is_safe_project_path(&project) || !Path::new(&project).is_dir() {
        return Err(format!(
            "The project folder of {shown} was not found, so it cannot be approved."
        ));
    }
    let entry = inventory::project_server(&project, &row.name)?;
    let lines = describe(&entry).map_err(|why| {
        format!("{shown} cannot be reviewed safely, so it is not approved: {why}.")
    })?;

    out.reviews.push(Review {
        title: format!("{shown} would be allowed to run:"),
        lines,
    });
    out.guards.push(Guard {
        project: project.clone(),
        name: row.name.clone(),
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
            name: row.name.clone(),
        });
    }
    out.settings.entry(project).or_default().push(Op::Approve {
        name: row.name.clone(),
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
            match object.get("args") {
                None | Some(Value::Null) => {}
                Some(Value::Array(args)) => {
                    for (i, arg) in args.iter().enumerate() {
                        let arg = arg.as_str().ok_or("an argument is not text")?;
                        lines.push(format!("Argument {}: {}", i + 1, shown(arg)?));
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
            }
            Some(_) => return Err("headers is not an object".to_owned()),
        }
        if let Some(helper) = text("headersHelper")? {
            lines.push(format!("Runs this to get headers: {}", shown(helper)?));
        }
    }
    Ok(lines)
}

/// A value exactly as it is, or an error when it cannot be shown that way.
fn shown(value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_REVIEW_CHARS {
        return Err(format!(
            "a value is longer than {MAX_REVIEW_CHARS} characters"
        ));
    }
    if sanitize(value) != value || value.chars().any(char::is_control) {
        return Err("a value contains control or hidden characters".to_owned());
    }
    Ok(value.to_owned())
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
    [
        project.join(".gitignore"),
        project.join(".git").join("info").join("exclude"),
    ]
    .iter()
    .filter_map(|p| fsx::read_limited(p, MAX_GITIGNORE).ok().flatten())
    .any(|text| {
        text.lines()
            .map(|l| l.trim().trim_start_matches('/'))
            .any(|l| COVERING.contains(&l))
    })
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
