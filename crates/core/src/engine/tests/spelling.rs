//! Engine tests: writes use the names as the file spells them, not the cleaned text shown.
use super::*;

/// "ab", and a different server whose name hides a right-to-left override and so shows as "ab" too; and one
/// whose name is cut for display.
fn lookalike_env() -> (Env, String, String) {
    let hidden = "a\u{202e}b".to_owned();
    let long = format!("srv-{}", "x".repeat(600));
    let env = quiet_env(&serde_json::json!({
        "mcpServers": {"ab": {"command": "x"}, hidden.clone(): {"command": "y"}, long.clone(): {"command": "z"}},
        "projects": {"/work/a": {"disabledMcpServers": []}}
    }));
    (env, hidden, long)
}

/// The row whose connection is `name` as written in the file.
fn row_on_disk(core: &Core, name: &str) -> ServerRow {
    let id = crate::model::ConnectionId::new(Scope::User, None, name);
    let events = collect(core, |e| !snapshots(e).is_empty());
    snapshots(&events)
        .last()
        .unwrap()
        .servers
        .iter()
        .find(|r| r.connection == id)
        .cloned()
        .expect("the row")
}

#[test]
fn switching_off_a_server_writes_its_own_name_not_the_one_shown() {
    // Found reviewing #1 (open item): ops took the cleaned, cut name shown on the row. Turning off the hidden
    // look-alike wrote "ab" and switched off the other server, reporting success.
    let (env, hidden, long) = lookalike_env();
    let core = env.start();
    let lookalike = row_on_disk(&core, &hidden);
    assert_eq!(lookalike.name, "ab", "shown cleaned");
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: lookalike.id(),
            enabled: false,
        }],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!([hidden]));
    // The variant: a name cut for display is written whole. One engine at a time on the file.
    drop(core);
    let core = env.start();
    let cut = row_on_disk(&core, &long);
    assert!(cut.name.ends_with('…'));
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: cut.id(),
            enabled: false,
        }],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(
        disabled_list(&env, "/work/a"),
        serde_json::json!([hidden, long])
    );
}

#[test]
fn deleting_and_editing_reach_the_server_whatever_its_name_shows() {
    let (env, hidden, long) = lookalike_env();
    let core = env.start();
    let events = collect(&core, |e| !snapshots(e).is_empty());
    let rows = &snapshots(&events).last().unwrap().servers;
    let pick = |name: &str| {
        let id = crate::model::ConnectionId::new(Scope::User, None, name);
        rows.iter().find(|r| r.connection == id).cloned().unwrap()
    };
    let (cut, lookalike) = (pick(&long), pick(&hidden));
    // Shown as JSON under the name in the file, not "no longer in the configuration".
    let (json, _) = entry_of(&core, cut.id());
    assert!(json.unwrap().contains("\"z\""));
    let plan = plan_for(&core, vec![Change::Delete { id: cut.id() }]);
    apply_and_wait(&core, &plan);
    let plan = plan_for(
        &core,
        vec![Change::Replace {
            id: lookalike.id(),
            json: "{\n  \"command\": \"y2\"\n}".into(),
        }],
    );
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    assert_eq!(
        now["mcpServers"][&hidden],
        serde_json::json!({"command": "y2"})
    );
    assert_eq!(
        now["mcpServers"]["ab"],
        serde_json::json!({"command": "x"}),
        "the other one untouched"
    );
    assert!(now["mcpServers"].get(&long).is_none());
}

/// The last status, other than Checking, reported for the connection `id`.
fn status_by_connection(
    events: &[Event],
    id: &crate::model::ConnectionId,
) -> Option<(Status, Option<String>)> {
    events.iter().rev().find_map(|e| match e {
        Event::Status {
            id: got,
            status,
            detail,
            ..
        } if got.connection == *id && *status != Status::Checking => {
            Some((*status, detail.clone()))
        }
        _ => None,
    })
}

#[test]
fn a_server_is_checked_under_its_own_name_never_a_look_alikes() {
    // Review of the fix: checks still passed the shown name to `claude mcp get`, so the look-alike was shown
    // with the real "ab"'s result.
    let hidden = "a\u{202e}b";
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"ab": {"type": "stdio", "command": "x"}}}),
        r#"{"rules":[
            {"args":["--version"],"stdout_file":"version.txt"},
            {"args":["mcp","get","ab"],"stdout_file":"connected.txt"}]}"#,
        &[
            ("version.txt", b"2.1.302 (Claude Code)\n"),
            ("connected.txt", CONNECTED),
        ],
    );
    let core = env.start();
    let ab = crate::model::ConnectionId::new(Scope::User, None, "ab");
    let lookalike = crate::model::ConnectionId::new(Scope::User, None, hidden);
    collect(&core, |e| status_by_connection(e, &ab).is_some());
    // The look-alike appears from outside: it is new (not the "ab" already there), so it is checked, and its
    // name cannot be passed to claude, so it is reported unchecked rather than given "ab"'s answer.
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {
            "ab": {"type": "stdio", "command": "x"},
            hidden: {"type": "stdio", "command": "y"}}}),
    );
    let events = collect(&core, |e| status_by_connection(e, &lookalike).is_some());
    let (status, detail) = status_by_connection(&events, &lookalike).unwrap();
    assert_ne!(status, Status::Connected);
    assert!(detail.unwrap_or_default().contains("not checked"));
    assert_eq!(
        env.runs(&["mcp", "get", "ab"]),
        1,
        "\"ab\" itself was not checked again"
    );
}

#[test]
fn servers_with_an_empty_project_key_or_name_can_still_be_changed() {
    // Review of the fix: decoding the keyring id lost an empty project ("" became none) and refused an empty
    // name, so these could no longer be changed, and the local "x" was shown as the user "x".
    let env = quiet_env(&serde_json::json!({
        "mcpServers": {"x": {"command": "user-x"}, "": {"command": "junk"}},
        "projects": {"": {"mcpServers": {"x": {"command": "local-x"}}}, "/work/a": {}}
    }));
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    let local_x = ServerId::new(Scope::Local, Some(""), "x");
    let (json, editable) = entry_of(&core, local_x.clone());
    assert!(json.unwrap().contains("local-x"));
    assert!(editable);
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_x,
            enabled: false,
        }],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(disabled_list(&env, ""), serde_json::json!(["x"]));
    let plan = plan_for(&core, vec![Change::Delete { id: user_id("") }]);
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    assert!(now["mcpServers"].get("").is_none());
    assert_eq!(
        now["mcpServers"]["x"],
        serde_json::json!({"command": "user-x"})
    );
}

#[test]
fn the_review_names_the_server_that_is_changed() {
    // An id is matched by its connection alone, so its other fields must not reach what the user reads.
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let mut id = user_id("gw");
    id.name = "github".into();
    id.scope = Scope::Project;
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: id.clone(),
            enabled: false,
        }],
    );
    assert_eq!(plan.summary, ["Turn off gw in 2 projects"]);
    // And the definition shown is the row's, with the row's scope deciding whether it can be edited.
    let (json, editable) = entry_of(&core, id);
    assert!(json.unwrap().contains("node"));
    assert!(
        editable,
        "a user server, whatever the caller's copy of its scope says"
    );
}

#[test]
fn duplicate_compares_names_and_projects_as_the_file_spells_them() {
    let hidden = "a\u{202e}b";
    let long = format!("/work/{}", "y".repeat(600));
    let env = quiet_env(&serde_json::json!({
        "mcpServers": {"gw": {"command": "node"}, hidden: {"command": "y"}},
        "projects": {long.clone(): {"mcpServers": {"x": {"command": "x"}}}}
    }));
    let core = env.start();
    // Only a look-alike exists: "ab" is free.
    let plan = plan_for(
        &core,
        vec![Change::Duplicate {
            id: user_id("gw"),
            name: "ab".into(),
            targets: vec![Target::User],
            adjust: Default::default(),
        }],
    );
    assert!(!plan.nothing_to_do);
    // A project whose key is cut for display still has its "x".
    let refused = plan_failure(
        &core,
        vec![Change::Duplicate {
            id: user_id("gw"),
            name: "x".into(),
            targets: vec![Target::Local(long)],
            adjust: Default::default(),
        }],
    );
    // Said before anything is read (the write would refuse it too, less plainly).
    assert!(
        refused.contains("There is already a server called x in"),
        "{refused}"
    );
}

#[test]
fn a_project_server_whose_name_cannot_be_shown_exactly_is_not_approved() {
    // Review of the fix: the raw name was approved while the review showed the cleaned one.
    let (env, key, repo) = repo_env(serde_json::json!({}));
    std::fs::write(
        repo.join(".mcp.json"),
        r#"{"mcpServers":{"git\u200bhub":{"command":"node","args":["s.js"]}}}"#,
    )
    .unwrap();
    let core = env.start();
    let refused = plan_failure(
        &core,
        vec![Change::Approve {
            id: project_id(&key, "git\u{200b}hub"),
        }],
    );
    assert!(refused.contains("cannot be shown exactly"), "{refused}");
    assert!(!repo.join(".claude").exists());
}

#[test]
fn deleting_a_local_server() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Delete {
            id: local_id("loc", "/work/a"),
        }],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(
        config_json(&env)["projects"]["/work/a"]["mcpServers"],
        serde_json::json!({})
    );
    verify_passes(&core);
}

#[test]
fn editing_round_trips_through_the_hidden_view_and_keeps_the_secret() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let (json, _) = entry_of(&core, user_id("gw"));
    let edited = json.unwrap().replace("\"node\"", "\"node22\"");
    let plan = plan_for(
        &core,
        vec![Change::Replace {
            id: user_id("gw"),
            json: edited,
        }],
    );
    let shown = format!("{plan:?}");
    assert!(
        shown.contains("node22") && !shown.contains("s3cret-value"),
        "{shown}"
    );
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    assert_eq!(now["mcpServers"]["gw"]["command"], "node22");
    assert_eq!(now["mcpServers"]["gw"]["env"]["TOKEN"], "s3cret-value");
}

#[test]
fn an_edit_that_is_not_valid_says_why_and_writes_nothing() {
    let env = quiet_env(&two_projects("end"));
    let before = config_json(&env);
    let core = env.start();
    for (json, why) in [
        ("{not json", "not valid JSON"),
        ("{\"type\": \"http\"}", "url"),
        (
            "{\"command\": \"x\", \"env\": {\"NEW\": \"<hidden>\"}}",
            "no value before",
        ),
    ] {
        let refused = plan_failure(
            &core,
            vec![Change::Replace {
                id: user_id("gw"),
                json: json.into(),
            }],
        );
        assert!(refused.contains(why), "{json}: {refused}");
    }
    assert_eq!(config_json(&env), before);
}
