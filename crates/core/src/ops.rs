//! Turns what the user asked for into document edits. Needs the current inventory to know which projects exist.
use crate::api::{Approval, Change, Scope, ServerId, Snapshot};
use crate::config::docstore::Op;
use crate::sanitize::sanitize;

/// The edits for `changes`, plus warnings the user should see next to the diff.
pub(crate) fn resolve(
    changes: &[Change],
    snapshot: &Snapshot,
) -> Result<(Vec<Op>, Vec<String>), String> {
    let mut ops = Vec::new();
    let mut warnings = Vec::new();
    for change in changes {
        match change {
            Change::SetEnabled { id, enabled } => {
                set_enabled(id, *enabled, snapshot, &mut ops, &mut warnings)?
            }
        }
    }
    if !ops.is_empty() {
        warnings.push(
            "A running Claude Code session may need a restart before it notices this.".to_owned(),
        );
    }
    warnings.dedup();
    Ok((ops, warnings))
}

fn set_enabled(
    id: &ServerId,
    enabled: bool,
    snapshot: &Snapshot,
    ops: &mut Vec<Op>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let shown = sanitize(&id.name);
    let Some(row) = snapshot.servers.iter().find(|r| r.id() == *id) else {
        return Err(format!(
            "{shown} is no longer in the configuration. Refresh and try again."
        ));
    };
    let disabled = !enabled;
    match row.scope {
        Scope::Local | Scope::Project => {
            let project = row
                .project
                .clone()
                .ok_or_else(|| format!("{shown} has no project"))?;
            ops.push(Op::SetDisabled {
                project,
                name: row.name.clone(),
                disabled,
            });
            if enabled && row.scope == Scope::Project && row.approval != Some(Approval::Approved) {
                warnings.push(format!(
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
                ops.push(Op::SetDisabled {
                    project: project.clone(),
                    name: row.name.clone(),
                    disabled,
                });
            }
            warnings.push(format!(
                "{shown} is a user server, and Claude Code switches it per project. This changes the {} projects it knows today; a project it first opens later is not covered.",
                snapshot.projects.len()
            ));
        }
    }
    Ok(())
}
