//! What `list` and `status` print: a table for people, JSON for scripts (docs/decisions/006-cli-output.md). Pure
//! functions of the snapshot and the answers, so they are tested without running anything. All text in a snapshot
//! and in the engine's answers is already sanitised by core.
use klefki_core::api::{Approval, Hint, Scope, ServerRow, Snapshot, Status, TransportKind};
use serde_json::{Value, json};
use std::fmt::Write as _;

/// The live answer for one server.
#[derive(Debug, Clone)]
pub struct Checked {
    pub status: Status,
    pub detail: Option<String>,
    pub hint: Option<Hint>,
}

/// What `status` has to say about one server.
#[derive(Debug, Clone)]
pub enum Outcome {
    Checked(Checked),
    /// Switched off: Claude Code does not start it, so it is not checked.
    Off,
    /// A project server not named on the command line: checking it would run the repository's command.
    ProjectServer,
    /// Asked, but no answer came in time.
    NoAnswer,
}

impl Outcome {
    /// Whether this server counts against `status`'s exit code: it was to be checked and is not connected.
    pub fn is_problem(&self) -> bool {
        match self {
            Outcome::Checked(c) => c.status != Status::Connected,
            Outcome::NoAnswer => true,
            Outcome::Off | Outcome::ProjectServer => false,
        }
    }
}

/// The rows to show, in the order they are shown: user servers, then per project its local and project servers;
/// by name within a group.
pub fn ordered<'a>(rows: impl IntoIterator<Item = &'a ServerRow>) -> Vec<&'a ServerRow> {
    let mut rows: Vec<&ServerRow> = rows.into_iter().collect();
    rows.sort_by(|a, b| {
        let group = |r: &ServerRow| (r.scope != Scope::User, r.project.clone(), r.scope);
        group(a)
            .cmp(&group(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

fn heading(row: &ServerRow) -> String {
    match (row.scope, &row.project) {
        (Scope::User, _) => "User".to_owned(),
        (scope, Some(project)) => format!("{} in {project}", scope.label()),
        (scope, None) => scope.label().to_owned(),
    }
}

/// Writes `rows` under their group headings; `line` writes one row's columns after the indentation.
fn grouped(out: &mut String, rows: &[&ServerRow], mut line: impl FnMut(&mut String, &ServerRow)) {
    let mut last: Option<String> = None;
    for row in rows {
        let head = heading(row);
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

fn width<'a>(texts: impl Iterator<Item = &'a str>) -> usize {
    texts.map(|t| t.chars().count()).max().unwrap_or(0)
}

fn pad(out: &mut String, text: &str, width: usize) {
    out.push_str(text);
    for _ in text.chars().count()..width {
        out.push(' ');
    }
}

fn notes(row: &ServerRow, project_count: usize) -> Vec<String> {
    let mut notes = Vec::new();
    match row.approval {
        Some(Approval::Pending) => notes.push("pending approval".to_owned()),
        Some(Approval::Rejected) => notes.push("rejected".to_owned()),
        Some(Approval::Approved) | None => {}
    }
    if row.scope == Scope::User && row.enabled && row.disabled_in > 0 {
        notes.push(format!(
            "off in {} of {project_count} projects",
            row.disabled_in
        ));
    }
    if row.shadowed {
        notes.push("shadowed by a closer scope".to_owned());
    }
    if row.casing_collision {
        notes.push("another server's name differs only in case".to_owned());
    }
    if row.oauth.is_some() {
        notes.push("OAuth".to_owned());
    }
    notes
}

pub const NOTHING: &str = "No MCP servers are configured.\n";

pub fn list_text(snapshot: &Snapshot, rows: &[&ServerRow]) -> String {
    if rows.is_empty() {
        return NOTHING.to_owned();
    }
    let name_w = width(rows.iter().map(|r| r.name.as_str()));
    let kind_w = width(rows.iter().map(|r| r.transport.label()));
    let mut out = String::new();
    grouped(&mut out, rows, |out, row| {
        pad(out, &row.name, name_w);
        out.push_str("  ");
        pad(out, if row.enabled { "on" } else { "off" }, 3);
        out.push_str("  ");
        pad(out, row.transport.label(), kind_w);
        out.push_str("  ");
        out.push_str(&row.target);
        let notes = notes(row, snapshot.project_count);
        if !notes.is_empty() {
            let _ = write!(out, "  ({})", notes.join("; "));
        }
        let end = out.trim_end_matches(' ').len();
        out.truncate(end);
    });
    out
}

fn scope_key(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "user",
        Scope::Local => "local",
        Scope::Project => "project",
    }
}

fn transport_key(kind: TransportKind) -> &'static str {
    kind.label()
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
    if row.scope == Scope::User {
        o.insert("disabled_in".into(), json!(row.disabled_in));
        o.insert("project_count".into(), json!(project_count));
    }
    o.insert("approval".into(), json!(row.approval.map(approval_key)));
    o.insert("shadowed".into(), json!(row.shadowed));
    o.insert("casing_collision".into(), json!(row.casing_collision));
    o.insert("oauth".into(), json!(row.oauth.is_some()));
    o
}

pub fn list_json(snapshot: &Snapshot, rows: &[&ServerRow]) -> String {
    let servers: Vec<Value> = rows
        .iter()
        .map(|r| Value::Object(row_json(r, snapshot.project_count)))
        .collect();
    to_text(&json!({"servers": servers, "warnings": snapshot.warnings}))
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

/// The status column and the message after it.
fn status_cells(row: &ServerRow, outcome: &Outcome) -> (String, String) {
    match outcome {
        Outcome::Checked(c) => {
            let detail = c.detail.as_deref().map(one_line).unwrap_or_default();
            match c.status {
                // The engine says why it did not run the check as "not checked: <why>".
                Status::Unknown | Status::Checking => (
                    NOT_CHECKED.to_owned(),
                    detail
                        .strip_prefix("not checked: ")
                        .map(str::to_owned)
                        .unwrap_or(detail),
                ),
                status => (status.label().to_owned(), detail),
            }
        }
        Outcome::Off => ("Off".to_owned(), "not checked".to_owned()),
        Outcome::ProjectServer => {
            let label = match row.status {
                Status::Unknown | Status::Checking => NOT_CHECKED,
                status => status.label(),
            };
            (
                label.to_owned(),
                format!(
                    "not checked: it runs the repository's command; check it with klefki-cli status {}",
                    command_word(&row.name)
                ),
            )
        }
        Outcome::NoAnswer => (
            NOT_CHECKED.to_owned(),
            "Claude Code did not answer in time".to_owned(),
        ),
    }
}

/// A name as it would be typed: quoted when a shell would split or expand it (names are checked to be
/// `[A-Za-z0-9_-]` before they are passed to Claude, but a configuration can hold anything).
fn command_word(name: &str) -> String {
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

pub fn status_text(rows: &[(&ServerRow, Outcome)]) -> String {
    if rows.is_empty() {
        return NOTHING.to_owned();
    }
    let cells: Vec<(String, String)> = rows.iter().map(|(r, o)| status_cells(r, o)).collect();
    let name_w = width(rows.iter().map(|(r, _)| r.name.as_str()));
    let label_w = width(cells.iter().map(|(l, _)| l.as_str()));
    let only_rows: Vec<&ServerRow> = rows.iter().map(|(r, _)| *r).collect();
    let mut out = String::new();
    let mut at = 0;
    grouped(&mut out, &only_rows, |out, row| {
        let (label, detail) = &cells[at];
        pad(out, &row.name, name_w);
        out.push_str("  ");
        pad(out, label, label_w);
        out.push_str("  ");
        out.push_str(detail);
        let end = out.trim_end_matches(' ').len();
        out.truncate(end);
        if let Outcome::Checked(Checked {
            hint: Some(hint), ..
        }) = &rows[at].1
        {
            // Under the status column, like the app's hint under a failed row.
            let indent = " ".repeat(2 + name_w + 2);
            let _ = write!(out, "\n{indent}{}", one_line(&hint.title));
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
    let mut counts: Vec<(Status, usize)> = Vec::new();
    let mut skipped = 0;
    for (_, outcome) in rows {
        let status = match outcome {
            Outcome::Checked(c) if c.status == Status::Checking => Status::Unknown,
            Outcome::Checked(c) => c.status,
            Outcome::NoAnswer => Status::Unknown,
            Outcome::Off | Outcome::ProjectServer => {
                skipped += 1;
                continue;
            }
        };
        match counts.iter_mut().find(|(s, _)| *s == status) {
            Some((_, n)) => *n += 1,
            None => counts.push((status, 1)),
        }
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|&(status, n)| {
            let label = match status {
                Status::Connected => "connected",
                Status::Degraded => "connected with failing tools",
                Status::NeedsAuth if n == 1 => "needs auth",
                Status::NeedsAuth => "need auth",
                Status::Failed => "failed",
                Status::PendingApproval => "pending approval",
                Status::NotConfigured => "not configured",
                Status::Checking | Status::Unknown => "could not be checked",
            };
            format!("{n} {label}")
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
    cli_version: Option<&str>,
    rows: &[(&ServerRow, Outcome)],
) -> String {
    let servers: Vec<Value> = rows
        .iter()
        .map(|(row, outcome)| {
            let mut o = row_json(row, snapshot.project_count);
            let (checked, skipped, status, detail, hint) = match outcome {
                Outcome::Checked(c) => (true, None, c.status, c.detail.clone(), c.hint.as_ref()),
                Outcome::Off => (false, Some("off"), row.status, None, None),
                Outcome::ProjectServer => (false, Some("project_server"), row.status, None, None),
                Outcome::NoAnswer => (
                    true,
                    None,
                    Status::Unknown,
                    Some("Claude Code did not answer in time".to_owned()),
                    None,
                ),
            };
            o.insert("checked".into(), json!(checked));
            if let Some(skipped) = skipped {
                o.insert("skipped".into(), json!(skipped));
            }
            o.insert("status".into(), json!(status_key(status)));
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
        "cli_version": cli_version,
        "servers": servers,
        "warnings": snapshot.warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klefki_core::api::fixture_snapshot;

    #[test]
    fn groups_come_user_first_then_each_project_local_before_project() {
        let s = fixture_snapshot();
        let rows = ordered(&s.servers);
        let heads: Vec<String> = rows.iter().map(|r| heading(r)).collect();
        let mut seen: Vec<&String> = Vec::new();
        for h in &heads {
            if seen.last() != Some(&h) {
                assert!(!seen.contains(&h), "group {h} split in two: {heads:?}");
                seen.push(h);
            }
        }
        assert_eq!(seen[0], "User");
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
    fn every_row_of_the_fixture_is_listed_once() {
        let s = fixture_snapshot();
        let rows = ordered(&s.servers);
        let text = list_text(&s, &rows);
        let lines = text.lines().filter(|l| l.starts_with("  ")).count();
        assert_eq!(lines, s.servers.len(), "{text}");
        assert!(!text.lines().any(|l| l.ends_with(' ')), "{text:?}");
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
    fn a_refused_check_reads_as_not_checked_with_its_reason() {
        let s = fixture_snapshot();
        let row = &s.servers[0];
        let outcome = Outcome::Checked(Checked {
            status: Status::Unknown,
            detail: Some("not checked: the project folder was not found".to_owned()),
            hint: None,
        });
        assert!(outcome.is_problem());
        let text = status_text(&[(row, outcome)]);
        assert!(
            text.contains("Not checked  the project folder was not found"),
            "{text}"
        );
        assert!(text.contains("1 could not be checked."), "{text}");
    }

    #[test]
    fn skipped_servers_are_not_problems() {
        assert!(!Outcome::Off.is_problem());
        assert!(!Outcome::ProjectServer.is_problem());
        assert!(Outcome::NoAnswer.is_problem());
    }
}
