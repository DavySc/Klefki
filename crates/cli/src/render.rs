//! What `list` and `status` print: a table for people, JSON for scripts (docs/decisions/006-cli-output.md). Pure
//! functions of the snapshot and the answers, so they are tested without running anything. All text in a snapshot
//! and in the engine's answers is already sanitised by core.
use klefki_core::api::{
    Approval, Hint, NameError, Scope, ServerName, ServerRow, Snapshot, Status, TransportKind,
};
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::fmt::Write as _;

/// The version of the JSON's shape. Raised when a field changes meaning or goes away; new fields keep it.
const SCHEMA: u32 = 1;

/// The live answer for one server.
#[derive(Debug, Clone)]
pub struct Checked {
    pub status: Status,
    pub detail: Option<String>,
    pub hint: Option<Hint>,
}

impl Checked {
    /// The engine did not run the check at all, and says why as "not checked: <why>" (a project folder that is gone,
    /// a name that is not safe to pass on).
    fn refused(&self) -> Option<&str> {
        match self.status {
            Status::Unknown => self.detail.as_deref()?.strip_prefix("not checked: "),
            _ => None,
        }
    }

    /// The check ran, but brought no status back: it timed out, or Claude's output was not understood.
    fn failed(&self) -> bool {
        matches!(self.status, Status::Unknown | Status::Checking) && self.refused().is_none()
    }
}

/// What `status` has to say about one server.
#[derive(Debug, Clone)]
pub enum Outcome {
    Checked(Checked),
    /// Switched off: Claude Code does not start it, so it is not checked, even when named.
    Off,
    /// A project server the user rejected: Claude Code does not run it.
    Rejected,
    /// A project server not named on the command line: checking it may run the repository's command.
    ProjectServer,
    /// A plugin's server: Klefki does not check those (decision 008).
    Plugin,
    /// A server the organisation sets: Klefki does not check those (decision 009).
    Organisation,
    /// A claude.ai connector: Klefki does not check those (decision 011).
    ClaudeAi,
    /// Claude Code loads it nowhere, for this reason (decision 009): another server answers to its name.
    NotLoaded(String),
    /// Asked, but no answer came in time.
    NoAnswer,
}

impl Outcome {
    /// Whether this server counts against `status`'s exit code: it was to be checked and is not connected.
    pub fn is_problem(&self) -> bool {
        match self {
            Outcome::Checked(c) => c.status != Status::Connected,
            Outcome::NoAnswer => true,
            Outcome::Off
            | Outcome::Rejected
            | Outcome::ProjectServer
            | Outcome::Plugin
            | Outcome::Organisation
            | Outcome::ClaudeAi
            | Outcome::NotLoaded(_) => false,
        }
    }

    /// Whether the check itself broke down, saying nothing about the server.
    pub fn tool_failed(&self) -> bool {
        match self {
            Outcome::Checked(c) => c.failed(),
            Outcome::NoAnswer => true,
            Outcome::Off
            | Outcome::Rejected
            | Outcome::ProjectServer
            | Outcome::Plugin
            | Outcome::Organisation
            | Outcome::ClaudeAi
            | Outcome::NotLoaded(_) => false,
        }
    }

    pub fn was_checked(&self) -> bool {
        matches!(self, Outcome::Checked(_) | Outcome::NoAnswer)
    }
}

/// Why `status` leaves this row out, if it does. Named servers are checked unless Claude Code would not run them.
pub fn skip(row: &ServerRow, named: bool) -> Option<Outcome> {
    if let Some(why) = &row.not_loaded {
        Some(Outcome::NotLoaded(why.clone()))
    } else if row.scope == Scope::Plugin {
        Some(Outcome::Plugin)
    } else if row.scope == Scope::Managed {
        Some(Outcome::Organisation)
    } else if row.scope == Scope::ClaudeAi {
        Some(Outcome::ClaudeAi)
    } else if !row.enabled {
        Some(Outcome::Off)
    } else if row.approval == Some(Approval::Rejected) {
        Some(Outcome::Rejected)
    } else if row.scope == Scope::Project && !named {
        Some(Outcome::ProjectServer)
    } else {
        None
    }
}

/// Orders names as people count: `server-9` before `server-10`. Case is ignored first, then decides a tie. This is
/// the order [`ordered`] gives names within a group, where each name is split once.
#[cfg(test)]
fn natural(a: &str, b: &str) -> Ordering {
    by_chunks(&chunks(a), &chunks(b)).then_with(|| a.cmp(b))
}

/// A name, lower-cased, as runs of digits and of other characters.
fn chunks(name: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for c in name.to_lowercase().chars() {
        let digit = c.is_ascii_digit();
        match out.last_mut() {
            Some((d, text)) if *d == digit => text.push(c),
            _ => out.push((digit, c.to_string())),
        }
    }
    out
}

/// Compares runs of digits by their number and other runs as text.
fn by_chunks(x: &[(bool, String)], y: &[(bool, String)]) -> Ordering {
    for ((xd, xs), (yd, ys)) in x.iter().zip(y) {
        let order = if *xd && *yd {
            let (xt, yt) = (xs.trim_start_matches('0'), ys.trim_start_matches('0'));
            xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt))
        } else {
            xs.cmp(ys)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    x.len().cmp(&y.len())
}

/// The rows to show, in the order they are shown: the organisation's servers, the user's, then per project its local
/// and project servers, then the plugins'; by name within a group.
pub fn ordered<'a>(rows: impl IntoIterator<Item = &'a ServerRow>) -> Vec<&'a ServerRow> {
    // Each name is split once, not at every comparison.
    let mut keyed: Vec<(&ServerRow, Vec<(bool, String)>)> =
        rows.into_iter().map(|r| (r, chunks(&r.name))).collect();
    keyed.sort_by(|(a, x), (b, y)| {
        let group = |r: &'a ServerRow| {
            (
                r.scope != Scope::Managed,
                r.scope != Scope::User,
                &r.project,
                r.scope,
            )
        };
        group(a)
            .cmp(&group(b))
            .then_with(|| by_chunks(x, y))
            .then_with(|| a.name.cmp(&b.name))
    });
    keyed.into_iter().map(|(r, _)| r).collect()
}

/// A path as shown in a heading: `~/…` under the home folder, which keeps lines short. JSON keeps the full path.
pub fn short_path(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_owned();
    };
    let home = home.replace('\\', "/");
    let home = home.trim_end_matches('/');
    let same = |a: &str, b: &str| {
        if cfg!(windows) || cfg!(target_os = "macos") {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    match path.get(..home.len()) {
        Some(head) if !home.is_empty() && same(head, home) => match &path[home.len()..] {
            "" => "~".to_owned(),
            rest if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_owned(),
        },
        _ => path.to_owned(),
    }
}

fn heading(row: &ServerRow, home: Option<&str>) -> String {
    match (row.scope, &row.project) {
        (Scope::User, _) => "User".to_owned(),
        (scope, Some(project)) => format!("{} in {}", scope.label(), short_path(project, home)),
        (scope, None) => scope.label().to_owned(),
    }
}

/// Writes `rows` under their group headings; `line` writes one row's columns after the indentation.
fn grouped(
    out: &mut String,
    rows: &[&ServerRow],
    home: Option<&str>,
    mut line: impl FnMut(&mut String, &ServerRow),
) {
    let mut last: Option<String> = None;
    for row in rows {
        let head = heading(row, home);
        if last.as_deref() != Some(head.as_str()) {
            if last.is_some() {
                out.push('\n');
            }
            out.push_str(&head);
            out.push('\n');
            last = Some(head);
        }
        out.push_str("  ");
        line(out, row);
        out.push('\n');
    }
}

/// At most this many characters of a name are shown in a table: a name from claude.ai or a file can be any length,
/// and one long name would widen every row. JSON has the whole name.
const NAME_SHOWN: usize = 64;

/// `name` as a table shows it: cut to [`NAME_SHOWN`] characters, the last one "…", when longer.
fn table_name(name: &str) -> std::borrow::Cow<'_, str> {
    match name.char_indices().nth(NAME_SHOWN) {
        Some(_) => {
            let end = name
                .char_indices()
                .nth(NAME_SHOWN - 1)
                .map_or(0, |(i, _)| i);
            format!("{}\u{2026}", &name[..end]).into()
        }
        None => name.into(),
    }
}

fn width(texts: impl Iterator<Item = impl AsRef<str>>) -> usize {
    texts.map(|t| t.as_ref().chars().count()).max().unwrap_or(0)
}

fn pad(out: &mut String, text: &str, width: usize) {
    out.push_str(text);
    for _ in text.chars().count()..width {
        out.push(' ');
    }
}

fn trim_end(out: &mut String) {
    let end = out.trim_end_matches(' ').len();
    out.truncate(end);
}

fn notes(row: &ServerRow, project_count: usize) -> Vec<String> {
    let mut notes = Vec::new();
    match row.approval {
        Some(Approval::Pending) => notes.push("pending approval".to_owned()),
        Some(Approval::Rejected) => notes.push("rejected".to_owned()),
        Some(Approval::Approved) | None => {}
    }
    // The state column says "not loaded"; the note says why.
    if let Some(why) = &row.not_loaded {
        notes.push(why.trim_end_matches('.').to_owned());
    }
    if let Some(file) = &row.managed {
        notes.push(format!("set by your organisation in {file}"));
    }
    if row.scope == Scope::ClaudeAi {
        notes.push("a connector of your claude.ai account".to_owned());
    }
    // Blocked everywhere is the state column's to say, with the reason above.
    if let Some(b) = row.blocked.as_ref().filter(|_| row.not_loaded.is_none()) {
        let what = if b.policy {
            "blocked by policy"
        } else {
            "connectors off"
        };
        notes.push(format!(
            "{what} in {} of {project_count} projects: {}",
            b.projects.len(),
            named(&b.projects)
        ));
    }
    if let Some(why) = &row.may_not_load {
        notes.push(format!("may not load: {}", why.trim_end_matches('.')));
    }
    // A plugin's or the organisation's server, like a user server, is one row for every project.
    let everywhere = row.scope.is_everywhere();
    if let Some(plugin) = &row.plugin {
        notes.push(format!("from the plugin {}", plugin.plugin));
        if !plugin.off_in.is_empty() {
            notes.push(format!(
                "plugin off in {} of {project_count} projects",
                plugin.off_in.len()
            ));
        }
    }
    if everywhere && row.enabled && row.disabled_in > 0 {
        let mut note = format!("off in {} of {project_count} projects", row.disabled_in);
        if !row.disabled_projects.is_empty() {
            let _ = write!(note, ": {}", named(&row.disabled_projects));
        }
        notes.push(note);
    }
    // Not loaded at all: what else it would leave out or be hidden by does not matter.
    let loaded = row.not_loaded.is_none();
    if let Some(same) = row
        .plugin
        .as_ref()
        .and_then(|p| p.same_as.as_deref())
        .filter(|_| loaded)
    {
        notes.push(if row.shadowed {
            format!("left out: same as {same}")
        } else {
            format!(
                "left out in {} of {project_count} projects: same as {same}",
                row.shadowed_in.len()
            )
        });
    } else if loaded && row.shadowed {
        notes.push(if row.scope == Scope::User {
            "shadowed in every project".to_owned()
        } else {
            "shadowed by a local server".to_owned()
        });
    } else if loaded && !row.shadowed_in.is_empty() {
        notes.push(format!(
            "shadowed in {} of {project_count} projects",
            row.shadowed_in.len()
        ));
    }
    if row.casing_collision {
        notes.push("another server's name differs only in case".to_owned());
    }
    if row.scope == Scope::User && !row.relative_paths.is_empty() && loaded {
        notes.push(format!(
            "relative to each session's folder: {}",
            row.relative_paths.join(", ")
        ));
    }
    if row.oauth.is_some() {
        notes.push("OAuth".to_owned());
    }
    notes
}

/// At most this many projects are named in a note.
const PROJECTS_NAMED: usize = 3;

/// `projects`, at most [`PROJECTS_NAMED`] of them by name, the rest counted.
fn named(projects: &[String]) -> String {
    let mut text = projects[..projects.len().min(PROJECTS_NAMED)].join(", ");
    if projects.len() > PROJECTS_NAMED {
        let _ = write!(text, " and {} more", projects.len() - PROJECTS_NAMED);
    }
    text
}

/// The state column: on, off, or not loaded (blocked, by policy, or connectors off, by the user's settings) wherever
/// it is on.
fn state(row: &ServerRow, project_count: usize) -> &'static str {
    // A plugin's server whose plugin is off in every project reads as off, like a user server off in all.
    let plugin_off = project_count > 0
        && row
            .plugin
            .as_ref()
            .is_some_and(|p| p.off_in.len() == project_count);
    if !row.enabled || plugin_off {
        "off"
    } else if let (Some(_), Some(b)) = (&row.not_loaded, &row.blocked) {
        if b.policy {
            "blocked"
        } else {
            "connectors off"
        }
    } else if row.not_loaded.is_some() {
        "not loaded"
    } else {
        "on"
    }
}

pub const NOTHING: &str = "No MCP servers are configured.\n";

pub fn list_text(snapshot: &Snapshot, rows: &[&ServerRow], home: Option<&str>) -> String {
    if rows.is_empty() {
        return NOTHING.to_owned();
    }
    let name_w = width(rows.iter().map(|r| table_name(&r.name)));
    let kind_w = width(rows.iter().map(|r| r.transport.label()));
    let state_w = width(rows.iter().map(|r| state(r, snapshot.project_count)));
    let mut out = String::new();
    grouped(&mut out, rows, home, |out, row| {
        pad(out, &table_name(&row.name), name_w);
        out.push_str("  ");
        pad(out, state(row, snapshot.project_count), state_w);
        out.push_str("  ");
        pad(out, row.transport.label(), kind_w);
        out.push_str("  ");
        out.push_str(&row.target);
        let notes = notes(row, snapshot.project_count);
        if !notes.is_empty() {
            let _ = write!(out, "  ({})", notes.join("; "));
        }
        trim_end(out);
    });
    out
}

fn scope_key(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "user",
        Scope::Local => "local",
        Scope::Project => "project",
        Scope::Plugin => "plugin",
        Scope::Managed => "organisation",
        Scope::ClaudeAi => "claudeai",
    }
}

/// The JSON value, kept apart from `TransportKind::label()` so a changed table label cannot change what scripts read.
fn transport_key(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Stdio => "stdio",
        TransportKind::Http => "http",
        TransportKind::Sse => "sse",
        TransportKind::Ws => "ws",
        TransportKind::ClaudeAi => "claude.ai",
        TransportKind::Invalid => "invalid",
    }
}

fn approval_key(approval: Approval) -> &'static str {
    match approval {
        Approval::Approved => "approved",
        Approval::Pending => "pending",
        Approval::Rejected => "rejected",
    }
}

pub fn status_key(status: Status) -> &'static str {
    match status {
        Status::Connected => "connected",
        Status::Degraded => "degraded",
        Status::NeedsAuth => "needs_auth",
        Status::Failed => "failed",
        Status::PendingApproval => "pending_approval",
        Status::NotConfigured => "not_configured",
        Status::Checking | Status::Unknown => "unknown",
    }
}

fn row_json(row: &ServerRow, project_count: usize) -> serde_json::Map<String, Value> {
    let mut o = serde_json::Map::new();
    o.insert("name".into(), json!(row.name));
    o.insert("scope".into(), json!(scope_key(row.scope)));
    o.insert("project".into(), json!(row.project));
    o.insert("transport".into(), json!(transport_key(row.transport)));
    o.insert("target".into(), json!(row.target));
    o.insert("enabled".into(), json!(row.enabled));
    if row.scope.is_everywhere() {
        o.insert("disabled_in".into(), json!(row.disabled_in));
        o.insert("disabled_projects".into(), json!(row.disabled_projects));
        o.insert("project_count".into(), json!(project_count));
    }
    o.insert("approval".into(), json!(row.approval.map(approval_key)));
    o.insert("shadowed".into(), json!(row.shadowed));
    if row.scope == Scope::User {
        o.insert("shadowed_in".into(), json!(row.shadowed_in));
        o.insert("relative_paths".into(), json!(row.relative_paths));
    }
    if let Some(plugin) = &row.plugin {
        o.insert("shadowed_in".into(), json!(row.shadowed_in));
        o.insert(
            "plugin".into(),
            json!({"name": plugin.plugin, "marketplace": plugin.marketplace, "same_as": plugin.same_as, "written_target": plugin.written_target, "off_in": plugin.off_in}),
        );
    }
    if row.managed.is_some() {
        o.insert("managed".into(), json!(row.managed));
    }
    o.insert("not_loaded".into(), json!(row.not_loaded));
    if let Some(b) = &row.blocked {
        o.insert(
            "blocked".into(),
            json!({"why": b.why, "projects": b.projects, "policy": b.policy}),
        );
    }
    if row.may_not_load.is_some() {
        o.insert("may_not_load".into(), json!(row.may_not_load));
    }
    o.insert("casing_collision".into(), json!(row.casing_collision));
    o.insert("oauth".into(), json!(row.oauth.is_some()));
    o
}

pub fn list_json(snapshot: &Snapshot, rows: &[&ServerRow]) -> String {
    let servers: Vec<Value> = rows
        .iter()
        .map(|r| Value::Object(row_json(r, snapshot.project_count)))
        .collect();
    to_text(&json!({"schema": SCHEMA, "servers": servers, "warnings": snapshot.warnings}))
}

fn to_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// One line of Claude's message: the table keeps one row per server.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const NOT_CHECKED: &str = "Not checked";
const NO_ANSWER: &str = "Claude Code did not answer in time";

/// A name as it would be typed: quoted when a shell would split or expand it (names are checked to be
/// `[A-Za-z0-9_-]` before they are passed to Claude, but a configuration can hold anything).
pub fn command_word(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain {
        name.to_owned()
    } else if name.starts_with('-') && !name.contains(['\'', '"', ' ']) {
        format!("-- {name}")
    } else {
        format!("\"{}\"", name.replace('"', "\\\""))
    }
}

/// Why Klefki does not pass `name` to Claude Code, as the rule it breaks.
fn name_rule(name: &str, why: NameError) -> String {
    match why {
        NameError::Empty => "a name cannot be empty".to_owned(),
        NameError::TooLong => "a name can be at most 64 characters".to_owned(),
        NameError::BadChar(' ') => "a name cannot hold a space".to_owned(),
        NameError::BadChar(c) => format!("a name can hold only A-Z a-z 0-9 _ -, not {c:?}"),
        NameError::LeadingDash => "a name cannot start with a dash".to_owned(),
        NameError::Reserved => {
            format!("Klefki turns down {name}, a name JavaScript treats as special")
        }
    }
}

/// The status column and the message after it.
fn status_cells(row: &ServerRow, outcome: &Outcome) -> (String, String) {
    match outcome {
        Outcome::Checked(c) => {
            let detail = c.detail.as_deref().map(one_line).unwrap_or_default();
            if let Some(why) = c.refused() {
                (NOT_CHECKED.to_owned(), one_line(why))
            } else if c.failed() {
                ("Check failed".to_owned(), detail)
            } else {
                (c.status.label().to_owned(), detail)
            }
        }
        Outcome::Off => (
            "Off".to_owned(),
            "not checked: it is off; turn it on first".to_owned(),
        ),
        Outcome::Rejected => (
            "Rejected".to_owned(),
            "not checked: rejected for this project, so Claude Code does not run it".to_owned(),
        ),
        Outcome::Plugin => (
            NOT_CHECKED.to_owned(),
            "not checked: it comes from a plugin; Claude Code's /mcp shows its status".to_owned(),
        ),
        Outcome::Organisation => (
            NOT_CHECKED.to_owned(),
            "not checked: your organisation sets it; Claude Code's /mcp shows its status"
                .to_owned(),
        ),
        Outcome::ClaudeAi => (
            NOT_CHECKED.to_owned(),
            "not checked: a connector of your claude.ai account; Claude Code's /mcp shows its status"
                .to_owned(),
        ),
        Outcome::NotLoaded(why) => ("Not loaded".to_owned(), format!("not checked: {why}")),
        Outcome::ProjectServer => {
            let label = match row.status {
                Status::Unknown | Status::Checking => NOT_CHECKED,
                status => status.label(),
            };
            // The name comes from the repository's `.mcp.json`. Only a name Klefki passes to Claude Code can be
            // checked, and such a name is the same word in any shell.
            let how = match ServerName::parse(&row.name) {
                Ok(name) => format!(
                    "to check it: klefki-cli status {}",
                    command_word(name.as_str())
                ),
                Err(why) => format!(
                    "to check it, rename it in the repository's .mcp.json ({})",
                    name_rule(&row.name, why)
                ),
            };
            (
                label.to_owned(),
                format!(
                    "not checked: it may run the repository's own command ({}); {how}",
                    row.target
                ),
            )
        }
        Outcome::NoAnswer => ("No answer".to_owned(), NO_ANSWER.to_owned()),
    }
}

pub fn status_text(rows: &[(&ServerRow, Outcome)], home: Option<&str>) -> String {
    if rows.is_empty() {
        return NOTHING.to_owned();
    }
    let cells: Vec<(String, String)> = rows.iter().map(|(r, o)| status_cells(r, o)).collect();
    let name_w = width(rows.iter().map(|(r, _)| table_name(&r.name)));
    let label_w = width(cells.iter().map(|(l, _)| l.as_str()));
    let only_rows: Vec<&ServerRow> = rows.iter().map(|(r, _)| *r).collect();
    let mut out = String::new();
    let mut at = 0;
    grouped(&mut out, &only_rows, home, |out, row| {
        let (label, detail) = &cells[at];
        pad(out, &table_name(&row.name), name_w);
        out.push_str("  ");
        pad(out, label, label_w);
        out.push_str("  ");
        out.push_str(detail);
        trim_end(out);
        if let Outcome::Checked(Checked {
            hint: Some(hint), ..
        }) = &rows[at].1
        {
            // Under the status column, like the app's hint under a failed row. The first line names the server, so
            // it reads on its own when a screen reader goes line by line.
            let indent = " ".repeat(2 + name_w + 2);
            let _ = write!(out, "\n{indent}{}: {}", row.name, one_line(&hint.title));
            for step in &hint.help {
                let _ = write!(out, "\n{indent}- {}", one_line(step));
            }
        }
        at += 1;
    });
    out.push('\n');
    out.push_str(&summary(rows));
    out
}

fn summary(rows: &[(&ServerRow, Outcome)]) -> String {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut skipped = 0;
    for (_, outcome) in rows {
        let label = match outcome {
            Outcome::Checked(c) if c.refused().is_some() => "could not be checked",
            Outcome::Checked(c) if c.failed() => "check failed",
            Outcome::Checked(c) => match c.status {
                Status::Connected => "connected",
                Status::Degraded => "connected with failing tools",
                Status::NeedsAuth => "needs auth",
                Status::Failed => "failed",
                Status::PendingApproval => "pending approval",
                Status::NotConfigured => "not configured",
                Status::Checking | Status::Unknown => "check failed",
            },
            Outcome::NoAnswer => "no answer",
            Outcome::Off
            | Outcome::Rejected
            | Outcome::ProjectServer
            | Outcome::Plugin
            | Outcome::Organisation
            | Outcome::ClaudeAi
            | Outcome::NotLoaded(_) => {
                skipped += 1;
                continue;
            }
        };
        match counts.iter_mut().find(|(l, _)| *l == label) {
            Some((_, n)) => *n += 1,
            None => counts.push((label, 1)),
        }
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|&(label, n)| match label {
            "needs auth" if n > 1 => format!("{n} need auth"),
            _ => format!("{n} {label}"),
        })
        .collect();
    if skipped > 0 {
        parts.push(format!("{skipped} not checked"));
    }
    if parts.is_empty() {
        "Nothing was checked.\n".to_owned()
    } else {
        format!("{}.\n", parts.join(", "))
    }
}

pub fn status_json(
    snapshot: &Snapshot,
    claude_version: Option<&str>,
    rows: &[(&ServerRow, Outcome)],
) -> String {
    let servers: Vec<Value> = rows
        .iter()
        .map(|(row, outcome)| {
            let mut o = row_json(row, snapshot.project_count);
            let skipped = match outcome {
                Outcome::Off => Some("off"),
                Outcome::Rejected => Some("rejected"),
                Outcome::ProjectServer => Some("project_server"),
                Outcome::Plugin => Some("plugin"),
                Outcome::Organisation => Some("organisation"),
                Outcome::ClaudeAi => Some("claudeai"),
                Outcome::NotLoaded(_) => Some("not_loaded"),
                Outcome::Checked(_) | Outcome::NoAnswer => None,
            };
            let (status, detail, hint) = match outcome {
                Outcome::Checked(c) => (
                    Some(if c.refused().is_some() {
                        "not_checked"
                    } else if c.failed() {
                        "check_failed"
                    } else {
                        status_key(c.status)
                    }),
                    c.detail.clone(),
                    c.hint.as_ref(),
                ),
                Outcome::NoAnswer => (Some("no_answer"), Some(NO_ANSWER.to_owned()), None),
                Outcome::Off
                | Outcome::Rejected
                | Outcome::ProjectServer
                | Outcome::Plugin
                | Outcome::Organisation
                | Outcome::ClaudeAi
                | Outcome::NotLoaded(_) => (None, None, None),
            };
            o.insert("checked".into(), json!(outcome.was_checked()));
            o.insert("skipped".into(), json!(skipped));
            // Only what this run found; a row that was not checked has what was known before in `last_known`.
            o.insert("status".into(), json!(status));
            if skipped.is_some() {
                o.insert("last_known".into(), json!(status_key(row.status)));
            }
            o.insert("detail".into(), json!(detail));
            o.insert(
                "hint".into(),
                match hint {
                    Some(h) => json!({"title": h.title, "code": h.code, "help": h.help}),
                    None => Value::Null,
                },
            );
            Value::Object(o)
        })
        .collect();
    to_text(&json!({
        "schema": SCHEMA,
        "claude_version": claude_version,
        "servers": servers,
        "warnings": snapshot.warnings,
        "organisation_notes": snapshot.organisation_notes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klefki_core::api::fixture_snapshot;

    #[test]
    fn groups_come_organisation_and_user_first_then_each_project_local_before_project() {
        let s = fixture_snapshot();
        let rows = ordered(&s.servers);
        let heads: Vec<String> = rows.iter().map(|r| heading(r, None)).collect();
        let mut seen: Vec<&String> = Vec::new();
        for h in &heads {
            if seen.last() != Some(&h) {
                assert!(!seen.contains(&h), "group {h} split in two: {heads:?}");
                seen.push(h);
            }
        }
        assert_eq!(seen[..2], ["Organisation", "User"]);
        for (i, h) in seen.iter().enumerate() {
            if let Some(project) = h.strip_prefix("Project in ") {
                let local = format!("Local in {project}");
                if let Some(j) = seen.iter().position(|x| **x == local) {
                    assert!(j < i, "{seen:?}");
                }
            }
        }
    }

    #[test]
    fn a_shadowed_server_says_where_another_is_used_instead() {
        let s = fixture_snapshot();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let project = s
            .servers
            .iter()
            .find(|r| r.scope == Scope::Project)
            .unwrap();
        let partly = ServerRow {
            shadowed_in: vec!["/a".into(), "/b".into()],
            shadowed: false,
            ..user.clone()
        };
        let everywhere = ServerRow {
            shadowed: true,
            ..partly.clone()
        };
        let by_local = ServerRow {
            shadowed: true,
            ..project.clone()
        };
        assert!(notes(&partly, 12).contains(&"shadowed in 2 of 12 projects".to_owned()));
        assert!(notes(&everywhere, 2).contains(&"shadowed in every project".to_owned()));
        assert!(notes(&by_local, 2).contains(&"shadowed by a local server".to_owned()));
        let json = row_json(&partly, 12);
        assert_eq!(json["shadowed"], json!(false));
        assert_eq!(json["shadowed_in"], json!(["/a", "/b"]));
        assert!(!row_json(&by_local, 2).contains_key("shadowed_in"));
    }

    #[test]
    fn a_user_server_with_a_relative_path_says_which() {
        let s = fixture_snapshot();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let project = s
            .servers
            .iter()
            .find(|r| r.scope == Scope::Project)
            .unwrap();
        let paths = vec!["build/index.js".to_owned(), "./c.json (in CFG)".to_owned()];
        let rel = ServerRow {
            relative_paths: paths.clone(),
            ..user.clone()
        };
        let in_repo = ServerRow {
            relative_paths: paths,
            ..project.clone()
        };
        assert!(
            notes(&rel, 2).contains(
                &"relative to each session's folder: build/index.js, ./c.json (in CFG)".to_owned()
            ),
            "{:?}",
            notes(&rel, 2)
        );
        assert!(!notes(&in_repo, 2).iter().any(|n| n.contains("relative")));
        // Not where Claude Code does not load it at all: that is what to say.
        let replaced = ServerRow {
            not_loaded: Some("Your organisation sets a server of the same name.".into()),
            ..rel.clone()
        };
        let said = notes(&replaced, 2);
        assert_eq!(said[0], "Your organisation sets a server of the same name");
        // Nor what a server Claude Code does not load would leave out or be hidden by (cross-review of #47).
        let hidden_too = ServerRow {
            shadowed: true,
            shadowed_in: vec!["a".into(), "b".into()],
            ..replaced.clone()
        };
        assert_eq!(notes(&hidden_too, 2), said);
        assert!(!said.iter().any(|n| n.contains("relative")), "{said:?}");
        assert_eq!(
            row_json(&rel, 2)["relative_paths"],
            json!(["build/index.js", "./c.json (in CFG)"])
        );
        assert_eq!(row_json(user, 2)["relative_paths"], json!([]));
        assert!(!row_json(&in_repo, 2).contains_key("relative_paths"));
    }

    #[test]
    fn a_server_policy_holds_back_in_some_projects_says_how_many() {
        let s = fixture_snapshot();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let row = ServerRow {
            blocked: Some(klefki_core::api::Blocked {
                why: "Blocked by policy: deniedMcpServers in /p/.claude/settings.json has serverName \"x\".".into(),
                projects: vec!["/p".into()],
                policy: true,
            }),
            ..user.clone()
        };
        assert!(
            notes(&row, 3).contains(&"blocked by policy in 1 of 3 projects: /p".to_owned()),
            "{:?}",
            notes(&row, 3)
        );
        // Blocked everywhere: the state column says so, and no count.
        let everywhere = ServerRow {
            not_loaded: row.blocked.as_ref().map(|b| b.why.clone()),
            ..row.clone()
        };
        assert_eq!(state(&everywhere, 3), "blocked");
        assert!(
            !notes(&everywhere, 3)
                .iter()
                .any(|n| n.contains("blocked by policy in")),
            "{:?}",
            notes(&everywhere, 3)
        );
        let json = row_json(&row, 3);
        assert_eq!(json["blocked"]["projects"], json!(["/p"]));
        assert!(
            json["blocked"]["why"]
                .as_str()
                .unwrap()
                .contains("deniedMcpServers")
        );
        assert!(!row_json(user, 3).contains_key("blocked"));
    }

    #[test]
    fn a_claude_ai_connector_says_where_it_comes_from_and_is_not_checked() {
        let s = fixture_snapshot();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let row = ServerRow {
            name: "claude.ai Slack".into(),
            scope: Scope::ClaudeAi,
            target: "claude.ai connector".into(),
            plugin: None,
            ..user.clone()
        };
        assert_eq!(notes(&row, 2)[0], "a connector of your claude.ai account");
        assert_eq!(row_json(&row, 2)["scope"], "claudeai");
        assert!(matches!(skip(&row, true), Some(Outcome::ClaudeAi)));
        assert_eq!(heading(&row, None), "claude.ai");
    }

    #[test]
    fn a_connector_turned_off_by_settings_is_not_said_to_be_blocked_by_policy() {
        let s = fixture_snapshot();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let why = "Your settings turn off connectors from claude.ai with disableClaudeAiConnectors, in \
                   .claude/settings.local.json. Remove disableClaudeAiConnectors there to let them load.";
        let row = ServerRow {
            name: "claude.ai Gmail".into(),
            scope: Scope::ClaudeAi,
            transport: TransportKind::ClaudeAi,
            target: "on claude.ai".into(),
            plugin: None,
            enabled: true,
            disabled_in: 1,
            disabled_projects: vec!["/p1".into()],
            blocked: Some(klefki_core::api::Blocked {
                why: why.into(),
                projects: vec!["/p2".into()],
                policy: false,
            }),
            ..user.clone()
        };
        let said = notes(&row, 3);
        assert!(
            said.contains(&"connectors off in 1 of 3 projects: /p2".to_owned()),
            "{said:?}"
        );
        assert!(
            said.contains(&"off in 1 of 3 projects: /p1".to_owned()),
            "{said:?}"
        );
        assert!(!said.iter().any(|n| n.contains("policy")), "{said:?}");
        let json = row_json(&row, 3);
        assert_eq!(json["transport"], "claude.ai");
        assert_eq!(json["blocked"]["policy"], false);
        assert_eq!(json["disabled_projects"], json!(["/p1"]));
        // Off wherever it is on.
        let everywhere = ServerRow {
            not_loaded: Some(why.into()),
            ..row.clone()
        };
        assert_eq!(state(&everywhere, 3), "connectors off");
        // What Klefki cannot check: a note.
        let unsure = ServerRow {
            blocked: None,
            may_not_load: Some("Your allowed list lets servers load by their address.".into()),
            ..row
        };
        assert_eq!(state(&unsure, 3), "on");
        assert!(notes(&unsure, 3).contains(
            &"may not load: Your allowed list lets servers load by their address".to_owned()
        ));
        assert_eq!(
            row_json(&unsure, 3)["may_not_load"],
            "Your allowed list lets servers load by their address."
        );
    }

    #[test]
    fn a_long_name_is_cut_in_the_tables_and_whole_in_json() {
        let s = fixture_snapshot();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let long = format!("claude.ai {}", "é".repeat(300));
        let row = ServerRow {
            name: long.clone(),
            ..user.clone()
        };
        let text = list_text(&s, &[&row], None);
        let line = text.lines().nth(1).unwrap();
        let shown = line.trim_start().split("  ").next().unwrap();
        assert_eq!(shown.chars().count(), NAME_SHOWN, "{line}");
        assert!(shown.ends_with('\u{2026}'), "{shown}");
        let status = status_text(&[(&row, Outcome::Off)], None);
        assert!(!status.contains(&long), "{status}");
        assert_eq!(row_json(&row, 2)["name"], json!(long));
        // Nearby: a name exactly that long is shown whole.
        let exact = "x".repeat(NAME_SHOWN);
        assert_eq!(table_name(&exact), exact);
    }

    #[test]
    fn names_sort_as_people_count() {
        let mut names = vec![
            "server-10",
            "Server-2",
            "server-9",
            "alpha",
            "server-02b",
            "Alpha",
        ];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(
            names,
            [
                "Alpha",
                "alpha",
                "Server-2",
                "server-02b",
                "server-9",
                "server-10"
            ]
        );
    }

    #[test]
    fn rows_in_a_group_sort_as_people_count() {
        // `ordered` splits each name once instead of using `natural`, so it is checked on its own.
        let s = fixture_snapshot();
        let row = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        let names = [
            "server-10",
            "Server-2",
            "server-9",
            "alpha",
            "server-02b",
            "Alpha",
        ];
        let rows: Vec<ServerRow> = names
            .iter()
            .map(|n| ServerRow {
                name: (*n).to_owned(),
                ..row.clone()
            })
            .collect();
        let mut expected = names.to_vec();
        expected.sort_by(|a, b| natural(a, b));
        let got: Vec<&str> = ordered(&rows).iter().map(|r| r.name.as_str()).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn every_row_of_the_fixture_is_listed_once() {
        let s = fixture_snapshot();
        let rows = ordered(&s.servers);
        let text = list_text(&s, &rows, None);
        let lines = text.lines().filter(|l| l.starts_with("  ")).count();
        assert_eq!(lines, s.servers.len(), "{text}");
        assert!(!text.lines().any(|l| l.ends_with(' ')), "{text:?}");
    }

    #[test]
    fn a_plugins_servers_are_listed_after_the_users_say_where_from_and_are_not_checked() {
        let s = fixture_snapshot();
        let rows = ordered(&s.servers);
        let heads: Vec<String> = rows.iter().map(|r| heading(r, None)).collect();
        let first_plugin = heads.iter().position(|h| h == "Plugin").unwrap();
        let organisation = heads.iter().take_while(|h| *h == "Organisation").count();
        assert!(organisation > 0, "{heads:?}");
        assert!(
            heads[organisation..first_plugin]
                .iter()
                .all(|h| h == "User"),
            "{heads:?}"
        );
        let text = list_text(&s, &rows, None);
        assert!(text.contains("\nPlugin\n"), "{text}");
        assert!(
            text.contains("(from the plugin docs-search; plugin off in 1 of 17 projects)"),
            "{text}"
        );
        assert!(
            text.contains("(from the plugin events; left out: same as legacy-events)"),
            "{text}"
        );
        let plugin = s.servers.iter().find(|r| r.scope == Scope::Plugin).unwrap();
        assert!(matches!(skip(plugin, true), Some(Outcome::Plugin)));
        let status = status_text(&[(plugin, Outcome::Plugin)], None);
        assert!(
            status.contains("not checked: it comes from a plugin"),
            "{status}"
        );
        let json: Value = serde_json::from_str(&list_json(&s, &[plugin])).unwrap();
        let row = &json["servers"][0];
        assert_eq!(row["scope"], "plugin");
        assert_eq!(
            row["plugin"],
            json!({
                "name": "docs-search",
                "marketplace": "team-plugins",
                "same_as": null,
                "written_target": "${CLAUDE_PLUGIN_ROOT}/bin/search --stdio",
                "off_in": ["C:/Repos/Gateway"]
            })
        );
        assert_eq!(row["disabled_in"], 0);

        // Cross-review of #46: off in every project reads "off", not "on".
        let mut everywhere = plugin.clone();
        everywhere.plugin.as_mut().unwrap().off_in = vec!["/a".into(), "/b".into()];
        let two = Snapshot {
            project_count: 2,
            ..(*s).clone()
        };
        let line = list_text(&two, &[&everywhere], None);
        assert!(line.contains(" off "), "{line}");
        assert!(!line.contains(" on "), "{line}");
        assert!(list_text(&s, &[plugin], None).contains(" on "));
    }

    #[test]
    fn paths_under_home_are_shortened() {
        assert_eq!(short_path("/home/d/repo", Some("/home/d")), "~/repo");
        assert_eq!(short_path("/home/d", Some("/home/d/")), "~");
        assert_eq!(
            short_path("/home/dx/repo", Some("/home/d")),
            "/home/dx/repo"
        );
        assert_eq!(short_path("/srv/repo", Some("/home/d")), "/srv/repo");
        assert_eq!(short_path("/srv/repo", None), "/srv/repo");
        assert_eq!(
            short_path("C:/Users/d/Repos/x", Some("C:\\Users\\d")),
            "~/Repos/x"
        );
    }

    #[test]
    fn names_in_the_check_command_survive_a_shell() {
        assert_eq!(command_word("docs"), "docs");
        assert_eq!(command_word("my server"), "\"my server\"");
        assert_eq!(command_word("-x"), "-- -x");
        assert_eq!(command_word("a\"b"), "\"a\\\"b\"");
        assert_eq!(command_word(""), "\"\"");
    }

    #[test]
    fn a_project_server_gets_a_check_command_only_when_its_name_can_be_checked() {
        // Core review 3: a name from the repository's `.mcp.json` was quoted in a way a shell still expands, so
        // copying the suggestion could run what the name held. Only a name Klefki passes to Claude Code can be
        // checked, and such a name needs no quoting.
        let s = fixture_snapshot();
        let mut row = s
            .servers
            .iter()
            .find(|r| r.scope == Scope::Project)
            .unwrap()
            .clone();
        let long = "a".repeat(65);
        for (name, said) in [
            ("docs", "; to check it: klefki-cli status docs\n"),
            (
                "my_server-2",
                "; to check it: klefki-cli status my_server-2\n",
            ),
            (
                "x$(echo INJECTED)",
                "(a name can hold only A-Z a-z 0-9 _ -, not '$')\n",
            ),
            ("x`echo INJECTED`", "not '`')\n"),
            ("-a$HOME", "not '$')\n"),
            ("-a;echo INJECTED", "not ';')\n"),
            ("a\"b", "not '\"')\n"),
            ("my server", "(a name cannot hold a space)\n"),
            ("-x", "(a name cannot start with a dash)\n"),
            (
                "constructor",
                "(Klefki turns down constructor, a name JavaScript treats as special)\n",
            ),
            (&long, "(a name can be at most 64 characters)\n"),
        ] {
            row.name = name.to_owned();
            let text = status_text(&[(&row, Outcome::ProjectServer)], None);
            let offered = said.starts_with("; to check it:");
            assert_eq!(
                text.contains("klefki-cli status"),
                offered,
                "{name}: {text}"
            );
            assert!(text.contains(said), "{name}: {text}");
            if !offered {
                assert!(
                    text.contains("; to check it, rename it in the repository's .mcp.json ("),
                    "{name}: {text}"
                );
            }
        }
    }

    fn checked(status: Status, detail: &str) -> Outcome {
        Outcome::Checked(Checked {
            status,
            detail: Some(detail.to_owned()),
            hint: None,
        })
    }

    #[test]
    fn a_refused_check_reads_as_not_checked_with_its_reason() {
        let s = fixture_snapshot();
        let outcome = checked(
            Status::Unknown,
            "not checked: the project folder was not found",
        );
        assert!(outcome.is_problem() && !outcome.tool_failed());
        let text = status_text(&[(&s.servers[0], outcome)], None);
        assert!(
            text.contains("Not checked  the project folder was not found"),
            "{text}"
        );
        assert!(text.contains("1 could not be checked."), "{text}");
    }

    #[test]
    fn a_check_that_ran_without_an_answer_is_not_called_not_checked() {
        // Review of #25: a timeout read "Not checked", which sent people looking for why it was skipped.
        let s = fixture_snapshot();
        for (outcome, label) in [
            (
                checked(Status::Unknown, "check timed out after 30.0s"),
                "Check failed",
            ),
            (Outcome::NoAnswer, "No answer"),
        ] {
            assert!(outcome.is_problem() && outcome.tool_failed());
            let text = status_text(&[(&s.servers[0], outcome)], None);
            assert!(text.contains(label), "{text}");
            assert!(!text.contains(NOT_CHECKED), "{text}");
        }
    }

    #[test]
    fn skipped_servers_are_not_problems() {
        for o in [Outcome::Off, Outcome::Rejected, Outcome::ProjectServer] {
            assert!(!o.is_problem() && !o.tool_failed() && !o.was_checked());
        }
    }
}
