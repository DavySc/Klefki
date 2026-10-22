//! Engine tests: a server switched in one project only (slice S3 of the on/off plan). Claude Code switches every
//! server per project; switching one there leaves every other project as it is, and is undone like any switch.
use super::*;

fn switch_in(id: ServerId, project: &str, on: bool) -> Change {
    Change::SetEnabledIn {
        id,
        project: project.to_owned(),
        enabled: on,
    }
}

fn undo_in(events: &[Event]) -> Option<crate::undo::Undo> {
    events.iter().find_map(|e| match e {
        Event::Applied { undo, .. } => undo.clone(),
        _ => None,
    })
}

#[test]
fn a_user_server_is_switched_off_in_one_project_only_and_undone_there() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![switch_in(user_id("memory"), "/work/designs", false)],
    );
    assert_eq!(plan.summary, ["Turn off memory in designs"]);
    assert_eq!(plan.action, "Turn off");
    assert!(!plan.destructive);
    // Nothing to weigh: the app may save it straight away, with Undo.
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert!(plan.restart.is_some(), "{plan:?}");
    let undo = undo_in(&applied(&core, &plan)).expect("an undo");
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["memory"])
    );
    // Not the other project.
    assert_eq!(disabled_list(&env, "/work/notes"), serde_json::Value::Null);
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn memory back on in designs"]);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
}

#[test]
fn a_user_server_off_everywhere_is_turned_on_in_one_project_only() {
    let mut config = designs_config();
    config["projects"]["/work/designs"]["disabledMcpServers"] = serde_json::json!(["db", "memory"]);
    config["projects"]["/work/notes"]["disabledMcpServers"] = serde_json::json!(["memory"]);
    let env = quiet_env(&config);
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![switch_in(user_id("memory"), "/work/designs", true)],
    );
    assert_eq!(plan.summary, ["Turn on memory in designs"]);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    applied(&core, &plan);
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["db"])
    );
    assert_eq!(
        disabled_list(&env, "/work/notes"),
        serde_json::json!(["memory"])
    );
}

#[test]
fn a_switch_already_as_asked_in_that_project_says_so_and_writes_nothing() {
    let mut config = designs_config();
    config["projects"]["/work/designs"]["disabledMcpServers"] = serde_json::json!(["memory"]);
    let env = quiet_env(&config);
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![switch_in(user_id("memory"), "/work/designs", false)],
    );
    assert!(plan.nothing_to_do);
    assert_eq!(plan.already, ["memory is already off in designs."]);
    // Off in designs only: in notes it is on, and that is not this change's business.
    let plan = plan_for(
        &core,
        vec![switch_in(user_id("memory"), "/work/notes", true)],
    );
    assert!(plan.nothing_to_do);
    assert_eq!(plan.already, ["memory is already on in notes."]);
}

#[test]
fn a_local_server_is_switched_in_its_own_project_and_refused_in_another() {
    let env = quiet_env(&two_projects(""));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![switch_in(local_id("loc", "/work/a"), "/work/a", false)],
    );
    assert_eq!(plan.summary, ["Turn off loc in a"]);
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));
    let why = plan_failure(
        &core,
        vec![switch_in(local_id("loc", "/work/a"), "/work/b", true)],
    );
    assert_eq!(why, "loc loads only in its own project, a.");
    assert_eq!(disabled_list(&env, "/work/b"), serde_json::json!(["other"]));
}

#[test]
fn a_project_claude_code_does_not_know_is_refused() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let why = plan_failure(
        &core,
        vec![switch_in(user_id("memory"), "/work/elsewhere", false)],
    );
    assert_eq!(
        why,
        "Claude Code does not know the project /work/elsewhere."
    );
}

/// Several servers in one project are one change, one Undo.
#[test]
fn several_servers_in_one_project_are_switched_and_undone_together() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![
            switch_in(user_id("memory"), "/work/designs", false),
            switch_in(user_id("db"), "/work/designs", false),
        ],
    );
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    let undo = undo_in(&applied(&core, &plan)).expect("an undo");
    assert_eq!(
        disabled_list(&env, "/work/designs"),
        serde_json::json!(["memory", "db"])
    );
    let plan = plan_for(&core, vec![Change::Undo(undo)]);
    assert_eq!(plan.summary, ["Turn memory and db back on in designs"]);
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/designs"), serde_json::json!([]));
}
