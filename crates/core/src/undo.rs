//! Undo for a change that only flips switches (`disabledMcpServers`): turning servers on or off, and using or
//! stopping a profile. The undo is the inverse switches, made into a plan like any change (Prepare → review →
//! Apply), each switch put back only while it is still as the change left it; and Klefki's record of what profiles
//! switched, put back as it was before the change while it is still as the change left it.
use crate::api::{Change, Snapshot, same_project};
use crate::config::docstore::Op;
use crate::ops::{Resolved, project_label};
use crate::profile_uses::{ProfileUses, Switch};
use crate::sanitize::sanitize;
use crate::words::{and_list, count, folder_name};
use serde_json::Value;

/// What puts back a change that only flipped switches: given with [`crate::api::Event::Applied`], handed back as
/// [`Change::Undo`]. Made by Klefki; nothing in it is trusted when it comes back, as every switch is checked against
/// the file then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    /// Each switch the change flipped, as it left it (`off`): the state before is the other one.
    pub switches: Vec<Switch>,
    /// Profiles whose record of what they switched the change altered.
    pub records: Vec<Record>,
}

/// What Klefki remembered of the switches of `profile` before a change and after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub profile: String,
    pub before: Vec<Switch>,
    pub after: Vec<Switch>,
}

/// The changes whose plan can be undone this way when it writes nothing but switches. Not a profile deleted with
/// its stop: the undo would bring back its switches but not the profile.
pub(crate) fn kind_undoable(changes: &[Change]) -> bool {
    changes.iter().all(|c| {
        matches!(
            c,
            Change::SetEnabled { .. }
                | Change::UseProfile { .. }
                | Change::StopProfile { delete: false, .. }
                | Change::Undo(_)
        )
    })
}

/// The switches `ops` flip in `root` (`.claude.json` as it is now), as they leave them; `None` when an op does
/// anything but switch. A switch already as an op sets it is not flipped.
pub(crate) fn flips(root: &Value, ops: &[Op]) -> Option<Vec<Switch>> {
    let mut out: Vec<Switch> = Vec::new();
    for op in ops {
        let Op::SetDisabled {
            project,
            name,
            disabled,
        } = op
        else {
            return None;
        };
        out.retain(|s| !(s.project == *project && s.name == *name));
        if is_off(root, project, name) != *disabled {
            out.push(Switch {
                project: project.clone(),
                name: name.clone(),
                off: *disabled,
            });
        }
    }
    Some(out)
}

/// The profiles whose record differs between `before` and `after`. None when either could not be read in full.
pub(crate) fn records(before: &ProfileUses, after: &ProfileUses) -> Vec<Record> {
    if before.problem.is_some() || after.problem.is_some() {
        return Vec::new();
    }
    let mut names: Vec<&str> = before.uses.iter().map(|u| u.profile.as_str()).collect();
    for u in &after.uses {
        if !names.contains(&u.profile.as_str()) {
            names.push(&u.profile);
        }
    }
    names
        .into_iter()
        .filter(|p| before.of(p) != after.of(p))
        .map(|p| Record {
            profile: p.to_owned(),
            before: before.of(p).to_vec(),
            after: after.of(p).to_vec(),
        })
        .collect()
}

/// The project entries of `root` that spell `project`.
fn entries<'a>(root: &'a Value, project: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    root.get("projects")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(move |(key, _)| same_project(key, project))
        .map(|(_, entry)| entry)
}

/// Whether `name` is switched off in `project`, in any spelling of it.
fn is_off(root: &Value, project: &str, name: &str) -> bool {
    entries(root, project).any(|entry| {
        entry
            .get("disabledMcpServers")
            .and_then(Value::as_array)
            .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(name)))
    })
}

/// The plan that puts back what `undo` names, against `text`, `.claude.json` as it is now.
pub(crate) fn resolve(undo: &Undo, text: &str, snapshot: &Snapshot) -> Result<Resolved, String> {
    let root: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| {
            format!("The configuration could not be read, so nothing is undone: {e}")
        })?
    };
    let mut out = Resolved {
        action: "Undo".to_owned(),
        ..Resolved::default()
    };
    // By what it goes back to (off or on), then by project: the servers (as shown) put back, and those already back.
    let mut back: Vec<(bool, String, Vec<String>)> = Vec::new();
    let mut already: Vec<(bool, String, Vec<String>)> = Vec::new();
    let mut gone: Vec<String> = Vec::new();
    for s in &undo.switches {
        if entries(&root, &s.project).next().is_none() {
            let folder = sanitize(folder_name(&s.project));
            if !gone.contains(&folder) {
                gone.push(folder);
            }
            continue;
        }
        let to_off = !s.off;
        let list = if is_off(&root, &s.project, &s.name) == s.off {
            out.claude.push(Op::SetDisabled {
                project: s.project.clone(),
                name: s.name.clone(),
                disabled: to_off,
            });
            &mut back
        } else {
            &mut already
        };
        let name = sanitize(&s.name);
        match list
            .iter_mut()
            .find(|(off, p, _)| *off == to_off && *p == s.project)
        {
            Some((_, _, names)) if !names.contains(&name) => names.push(name),
            Some(_) => {}
            None => list.push((to_off, s.project.clone(), vec![name])),
        }
    }
    let state = |off: bool| if off { "off" } else { "on" };
    for (off, names, places) in grouped(&back, snapshot) {
        out.summary.push(format!(
            "Turn {} back {} in {places}",
            and_list(&names),
            state(off)
        ));
    }
    for (off, names, places) in grouped(&already, snapshot) {
        let sentence = format!(
            "{} {} already back {} in {places}.",
            and_list(&names),
            if names.len() == 1 { "is" } else { "are" },
            state(off)
        );
        out.notes.push(sentence.clone());
        out.already.push(sentence);
    }
    if !gone.is_empty() {
        let one = gone.len() == 1;
        out.notes.push(format!(
            "{} {} no longer {} Claude Code knows, so {} left out.",
            and_list(&gone),
            if one { "is" } else { "are" },
            if one { "a project" } else { "projects" },
            if one { "it is" } else { "they are" }
        ));
    }
    for record in &undo.records {
        out.notes.push(format!(
            "Klefki's record of what {} switched goes back as it was.",
            sanitize(&record.profile)
        ));
    }
    if out.summary.is_empty() && !undo.records.is_empty() {
        let profiles: Vec<String> = undo.records.iter().map(|r| sanitize(&r.profile)).collect();
        out.summary.push(format!(
            "Put back what Klefki remembers of {}",
            and_list(&profiles)
        ));
    }
    out.records_back.clone_from(&undo.records);
    (out.sessions, out.restart) = crate::ops::open_sessions(&out).unzip();
    Ok(out)
}

/// Projects that put the same servers back the same way, said once: (off, servers, places).
fn grouped(
    by_project: &[(bool, String, Vec<String>)],
    snapshot: &Snapshot,
) -> Vec<(bool, Vec<String>, String)> {
    let mut groups: Vec<(bool, Vec<String>, Vec<String>)> = Vec::new();
    for (off, project, names) in by_project {
        let label = project_label(project, snapshot);
        match groups.iter_mut().find(|(o, n, _)| o == off && n == names) {
            Some((_, _, places)) => places.push(label),
            None => groups.push((*off, names.clone(), vec![label])),
        }
    }
    groups
        .into_iter()
        .map(|(off, names, places)| {
            let places = if places.len() <= 3 {
                and_list(&places)
            } else {
                count(places.len(), "project")
            };
            (off, names, places)
        })
        .collect()
}
