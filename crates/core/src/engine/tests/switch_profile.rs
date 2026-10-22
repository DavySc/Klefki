//! Engine tests: switching a project from one profile to another in one change (slice S4 of the on/off plan).
use super::*;

fn switch_profile(from: &[&str], to: &str, members: &[&str], project: &str, only: bool) -> Change {
    Change::SwitchProfile {
        from: from.iter().map(|f| (*f).to_owned()).collect(),
        name: to.to_owned(),
        members: members.iter().map(|m| user_id(m).connection).collect(),
        project: project.to_owned(),
        only,
    }
}

/// What `.claude.json` switches off in `project`, sorted.
fn off_sorted(env: &Env, project: &str) -> Vec<String> {
    let mut off: Vec<String> = disabled_list(env, project)
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
        .unwrap_or_default();
    off.sort();
    off
}

fn snapshot_after(events: &[Event]) -> Arc<Snapshot> {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Snapshot(s) => Some(Arc::clone(s)),
            _ => None,
        })
        .expect("a snapshot")
}

fn undo_in(events: &[Event]) -> Option<crate::undo::Undo> {
    events.iter().find_map(|e| match e {
        Event::Applied { undo, .. } => undo.clone(),
        _ => None,
    })
}

/// The owner's ask (on/off plan S4): from Writing (gw alone) to Research (gw and db alone) in one step. Research is
/// used on the files as they are once Writing is stopped, so stopping Research later goes back to before both; the
/// switch is undone whole, back to Writing.
#[test]
fn a_switch_stops_one_profile_and_uses_the_other_as_one_change() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    applied(&core, &plan);
    assert_eq!(off_sorted(&env, "/work/designs"), ["db", "memory"]);

    let plan = plan_for(
        &core,
        vec![switch_profile(
            &["Writing"],
            "Research",
            &["gw", "db"],
            "/work/designs",
            true,
        )],
    );
    assert_eq!(
        plan.summary,
        ["Switch designs from Writing to Research, and turn off every other server there"]
    );
    assert_eq!(plan.action, "Switch profile");
    assert!(!plan.nothing_to_do);
    assert!(plan.restart.is_some(), "{plan:?}");
    // One line for what is stopped, not one per server that goes back only to change again.
    assert!(
        !plan.notes.iter().any(|n| n.contains("as before Writing")),
        "{:?}",
        plan.notes
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Writing is no longer in use in designs."),
        "{:?}",
        plan.notes
    );
    // memory goes back on as Writing is stopped, then off again as Research is used alone: it stays off, so nothing
    // is said to go off, and the file does not list it again.
    assert!(
        !plan.warnings.iter().any(|w| w.contains("turns off")),
        "{:?}",
        plan.warnings
    );
    assert_eq!(plan.files.len(), 1, "{:?}", plan.files);
    assert!(
        plan.files[0].lines.iter().any(|l| l == "- db")
            && !plan.files[0].lines.iter().any(|l| l.contains("memory")),
        "{:?}",
        plan.files[0].lines
    );
    let events = applied(&core, &plan);
    assert_eq!(off_sorted(&env, "/work/designs"), ["memory"]);
    let snapshot = snapshot_after(&events);
    assert_eq!(
        crate::ops::profile_in_use("Research", &snapshot),
        ["/work/designs"]
    );
    assert!(crate::ops::profile_in_use("Writing", &snapshot).is_empty());
    assert_eq!(
        crate::ops::profiles_in_use_in(&snapshot, "/work/designs"),
        ["Research"]
    );
    // Undone whole: Writing's set again, and Writing in use again.
    let undo = undo_in(&events).expect("a switch can be undone");
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    let events = applied(&core, &plan);
    assert_eq!(off_sorted(&env, "/work/designs"), ["db", "memory"]);
    let snapshot = snapshot_after(&events);
    assert_eq!(
        crate::ops::profiles_in_use_in(&snapshot, "/work/designs"),
        ["Writing"]
    );
    // Switched again and stopped: back to before both, everything on.
    let plan = plan_for(
        &core,
        vec![switch_profile(
            &["Writing"],
            "Research",
            &["gw", "db"],
            "/work/designs",
            true,
        )],
    );
    applied(&core, &plan);
    let plan = plan_for(&core, vec![stop_profile("Research", None, false)]);
    applied(&core, &plan);
    assert!(off_sorted(&env, "/work/designs").is_empty());
    assert_eq!(
        crate::profile_uses::load(&env.dir.path().join("backups")),
        crate::profile_uses::ProfileUses::default()
    );
    // The other project was never touched.
    assert_eq!(disabled_list(&env, "/work/notes"), serde_json::Value::Null);
}

/// Several profiles in use there (one used over another) are all stopped, each on the files as the one before left
/// them: a switch both set the same way goes back too, rather than each keeping it for the other.
#[test]
fn a_switch_stops_every_profile_in_use_there_even_where_they_agree() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    // Writing turns memory and db off; Notes, used over it, turns db back on and finds memory off as it wants it.
    let mut events = Vec::new();
    for (name, members) in [("Writing", &["gw"][..]), ("Notes", &["gw", "db"])] {
        let plan = plan_for(
            &core,
            vec![use_profile(name, members, Some("/work/designs"), true)],
        );
        events = applied(&core, &plan);
    }
    assert_eq!(off_sorted(&env, "/work/designs"), ["memory"]);
    assert_eq!(
        crate::ops::profiles_in_use_in(&snapshot_after(&events), "/work/designs"),
        ["Writing", "Notes"]
    );
    let plan = plan_for(
        &core,
        vec![switch_profile(
            &["Writing", "Notes"],
            "Memory",
            &["gw", "memory"],
            "/work/designs",
            true,
        )],
    );
    assert_eq!(
        plan.summary,
        ["Switch designs from Writing and Notes to Memory, and turn off every other server there"]
    );
    let events = applied(&core, &plan);
    assert_eq!(off_sorted(&env, "/work/designs"), ["db"]);
    let snapshot = snapshot_after(&events);
    assert_eq!(
        crate::ops::profiles_in_use_in(&snapshot, "/work/designs"),
        ["Memory"]
    );
    let plan = plan_for(&core, vec![stop_profile("Memory", None, false)]);
    applied(&core, &plan);
    assert!(
        off_sorted(&env, "/work/designs").is_empty(),
        "before all three: {:?}",
        off_sorted(&env, "/work/designs")
    );
}

/// Without "Only these servers there", the servers the stopped profile turned off come back on and the new one's
/// are turned on beside them. Switching to the profile in use, or from none, is using it.
#[test]
fn a_switch_without_only_and_its_edges() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    applied(&core, &plan);
    let plan = plan_for(
        &core,
        vec![switch_profile(
            &["Writing"],
            "Db",
            &["db"],
            "/work/designs",
            false,
        )],
    );
    assert_eq!(plan.summary, ["Switch designs from Writing to Db"]);
    applied(&core, &plan);
    assert!(off_sorted(&env, "/work/designs").is_empty());

    // From the profile it switches to: that one is left in use, as using it again.
    let plan = plan_for(
        &core,
        vec![switch_profile(
            &["Db", "Db"],
            "Db",
            &["db"],
            "/work/designs",
            true,
        )],
    );
    assert_eq!(
        plan.summary,
        ["Use the profile Db in designs, and turn off every other server there"]
    );
    // From none: using it.
    let plan = plan_for(
        &core,
        vec![switch_profile(&[], "Db", &["db"], "/work/notes", false)],
    );
    assert_eq!(plan.summary, ["Use the profile Db in notes"]);
    // A project Claude Code does not know is refused, as for a use.
    let why = plan_failure(
        &core,
        vec![switch_profile(&["Db"], "Gw", &["gw"], "/work/nope", true)],
    );
    assert!(why.contains("does not know the project"), "{why}");
}
