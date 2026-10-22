//! Engine tests: Undo for a change that only flips switches (slice S2 of the on/off plan).
use super::*;

/// What the change just applied offers to put back, if anything.
fn undo_in(events: &[Event]) -> Option<crate::undo::Undo> {
    events.iter().find_map(|e| match e {
        Event::Applied { undo, .. } => undo.clone(),
        _ => None,
    })
}

fn undo_of(core: &Core, changes: Vec<Change>) -> crate::undo::Undo {
    let plan = plan_for(core, changes);
    undo_in(&applied(core, &plan)).expect("an undo")
}

fn switch(name: &str, on: bool) -> Change {
    Change::SetEnabled {
        id: user_id(name),
        enabled: on,
    }
}

#[test]
fn a_switch_is_undone_and_the_undo_can_be_undone_again() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let undo = undo_of(&core, vec![switch("memory", false)]);
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["memory"])
    );
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn memory back on in designs and notes"]);
    assert_eq!(plan.action, "Undo");
    assert!(!plan.destructive);
    assert!(!plan.nothing_to_do);
    // The open-session sentence is said, as for any switch.
    assert!(plan.restart.is_some(), "{plan:?}");
    let redo = undo_in(&applied(&core, &plan)).expect("the undo can be undone");
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
    assert_eq!(disabled_list(&env, "/work/notes"), serde_json::json!([]));
    // Undoing the undo turns it off again.
    let plan = plan_for(&core, vec![Change::Undo(redo)]);
    assert_eq!(plan.summary, ["Turn memory back off in designs and notes"]);
    applied(&core, &plan);
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["memory"])
    );
    assert_eq!(
        disabled_list(&env, "/work/notes"),
        serde_json::json!(["memory"])
    );
}

/// The UX pass's case: "Only these servers there" turns many servers off; Undo turns each of them back on, and the
/// profile is no longer in use there.
#[test]
fn using_a_profile_with_only_is_undone_whole() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let undo = undo_of(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["memory", "db"])
    );
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn memory and db back on in designs"]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Klefki's record of what Writing switched goes back as it was."),
        "{:?}",
        plan.notes
    );
    let events = applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
    let backups = env.dir.path().join("backups");
    assert!(crate::profile_uses::load(&backups).of("Writing").is_empty());
    let snapshot = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Snapshot(s) => Some(Arc::clone(s)),
            _ => None,
        })
        .expect("a snapshot");
    assert!(crate::ops::profile_in_use("Writing", &snapshot).is_empty());
    // The other project was never touched.
    assert_eq!(disabled_list(&env, "/work/notes"), serde_json::Value::Null);
}

/// Stopping a profile is undone too: its switches go off again and the profile is in use there once more, so a
/// later Stop puts them back.
#[test]
fn stopping_a_profile_is_undone_and_remembered_again() {
    let env = quiet_env(&designs_config());
    let backups = env.dir.path().join("backups");
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    applied(&core, &plan);
    let remembered = crate::profile_uses::load(&backups).of("Writing").to_vec();
    let undo = undo_of(&core, vec![stop_profile("Writing", None, false)]);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn memory and db back off in designs"]);
    applied(&core, &plan);
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["memory", "db"])
    );
    assert_eq!(
        crate::profile_uses::load(&backups).of("Writing"),
        remembered.as_slice()
    );
    let plan = plan_for(&core, vec![stop_profile("Writing", None, false)]);
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
}

/// A switch made by hand after a profile was used is the user's, so the profile forgets it; undoing that switch
/// gives the profile its record back.
#[test]
fn undoing_a_switch_gives_a_profile_back_what_it_forgot() {
    let env = quiet_env(&designs_config());
    let backups = env.dir.path().join("backups");
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    applied(&core, &plan);
    let remembered = crate::profile_uses::load(&backups).of("Writing").to_vec();
    let undo = undo_of(&core, vec![switch("memory", true)]);
    assert_ne!(
        crate::profile_uses::load(&backups).of("Writing"),
        remembered.as_slice()
    );
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn memory back off in designs"]);
    applied(&core, &plan);
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["db", "memory"])
    );
    assert_eq!(
        crate::profile_uses::load(&backups).of("Writing"),
        remembered.as_slice()
    );
}

/// What was switched again since is left as it is, and said; what is still as the change left it goes back.
#[test]
fn a_switch_changed_since_is_left_and_said() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let undo = undo_of(&core, vec![switch("memory", false)]);
    verify_passes(&core);
    // memory is turned back on in designs by hand (or with /mcp in a session).
    let mut now = config_json(&env);
    now["projects"]["/work/designs"]["disabledMcpServers"] = serde_json::json!([]);
    write_config(&env, &now);
    let plan = plan_for(&core, vec![Change::Undo(undo.clone())]);
    assert_eq!(plan.summary, ["Turn memory back on in notes"]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "memory is already back on in designs."),
        "{:?}",
        plan.notes
    );
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/notes"), serde_json::json!([]));
    // Everything already back: nothing to do, and said so.
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert!(plan.nothing_to_do, "{plan:?}");
    assert_eq!(
        plan.already,
        ["memory is already back on in designs and notes."]
    );
}

/// Variant: a project Claude Code no longer knows is left out, and said.
#[test]
fn a_project_gone_since_is_left_out() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let undo = undo_of(&core, vec![switch("db", false)]);
    verify_passes(&core);
    let mut now = config_json(&env);
    now["projects"]
        .as_object_mut()
        .unwrap()
        .remove("/work/notes");
    write_config(&env, &now);
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn db back on in designs"]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "notes is no longer a project Claude Code knows, so it is left out."),
        "{:?}",
        plan.notes
    );
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
    assert_eq!(disabled_list(&env, "/work/notes"), serde_json::Value::Null);
}

/// Only a change that does nothing but flip switches is undone this way: not one that deletes, approves, or deletes
/// a profile, and not one that wrote nothing.
#[test]
fn only_switches_are_offered_an_undo() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    applied(&core, &plan);
    let plan = plan_for(&core, vec![stop_profile("Writing", None, true)]);
    assert_eq!(undo_in(&applied(&core, &plan)), None);
    let plan = plan_for(&core, vec![Change::Delete { id: user_id("db") }]);
    assert_eq!(undo_in(&applied(&core, &plan)), None);
    // Already off everywhere: written nothing, so nothing to undo.
    let plan = plan_for(&core, vec![switch("memory", false)]);
    applied(&core, &plan);
    let plan = plan_for(&core, vec![switch("memory", false)]);
    assert!(plan.nothing_to_do);
}

/// An undo is reviewed on its own, like a restore.
#[test]
fn an_undo_is_reviewed_on_its_own() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let undo = undo_of(&core, vec![switch("memory", false)]);
    let why = plan_failure(&core, vec![Change::Undo(undo), switch("db", false)]);
    assert_eq!(why, "An undo is reviewed on its own.");
}

/// Variant: what a profile remembers that changed since (used again elsewhere) is left as it is, and said; the
/// switches still go back.
#[test]
fn a_profile_record_changed_since_is_left_and_said() {
    let env = quiet_env(&designs_config());
    let backups = env.dir.path().join("backups");
    let core = env.start();
    let undo = undo_of(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/notes"), true)],
    );
    applied(&core, &plan);
    let remembered = crate::profile_uses::load(&backups).of("Writing").to_vec();
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    let events = applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
    assert_eq!(
        crate::profile_uses::load(&backups).of("Writing"),
        remembered.as_slice()
    );
    let notes = events
        .iter()
        .find_map(|e| match e {
            Event::Applied { notes, .. } => Some(notes.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        notes,
        ["What Klefki remembers of Writing changed since, so it is left as it is."]
    );
}
