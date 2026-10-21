//! Engine tests: restoring from a backup in the activity log.
use super::*;

#[test]
fn a_restore_puts_back_a_deleted_server_and_leaves_the_rest_of_the_file_alone() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(&core, vec![Change::Delete { id: user_id("gw") }]);
    applied(&core, &plan);
    let backup = newest_backup(&core);
    // Since then Claude Code went on: it counted a start, and a server was added.
    let mut config = config_json(&env);
    config["numStartups"] = serde_json::json!(9);
    config["mcpServers"]["newer"] = serde_json::json!({"type": "stdio", "command": "n"});
    write_config(&env, &config);
    let plan = plan_for(&core, vec![Change::Restore { backup }]);
    assert_eq!(plan.action, "Restore");
    assert!(!plan.nothing_to_do);
    assert!(
        plan.summary
            .contains(&"Put back gw for the user".to_owned()),
        "{plan:#?}"
    );
    assert!(
        plan.summary.iter().any(|s| s.contains("newer")),
        "what was added since is named: {plan:#?}"
    );
    // The review never shows a server's settings.
    assert!(!format!("{plan:?}").contains("s3cret-value"));
    let events = applied(&core, &plan);
    let config = config_json(&env);
    assert_eq!(
        config["mcpServers"]["gw"],
        two_projects("end")["mcpServers"]["gw"],
        "back, secret and all"
    );
    assert_eq!(
        config["mcpServers"]["newer"]["command"], "n",
        "a restore removes nothing"
    );
    assert_eq!(config["numStartups"], 9, "Claude Code's own settings stay");
    assert!(
        snapshots(&events)
            .last()
            .unwrap()
            .servers
            .iter()
            .any(|r| r.name == "gw")
    );
    // The restore is logged with the backup it took first, so it can be undone the same way.
    let log = activity(&core);
    assert_eq!(log.entries.len(), 2, "{log:#?}");
    assert_eq!(log.entries[0].summary, plan.summary);
    let before = &log.entries[0].backups[0];
    assert!(before.kept);
    let undo = plan_for(
        &core,
        vec![Change::Restore {
            backup: before.path.clone(),
        }],
    );
    assert!(undo.summary.iter().any(|s| s.contains("gw")), "{undo:#?}");
}

#[test]
fn a_restore_puts_back_one_server_and_changes_another_back_in_the_same_list() {
    // Found trying it in the real app: the save refused to add a server and change another in one list.
    let mut config = two_projects("end");
    config["mcpServers"]["tool"] = serde_json::json!({"type": "stdio", "command": "t1"});
    config["projects"]["/work/a"]["mcpServers"]["second"] =
        serde_json::json!({"type": "stdio", "command": "s1"});
    let env = quiet_env(&config);
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    applied(&core, &plan);
    let backup = newest_backup(&core);
    let mut now = config_json(&env);
    now["mcpServers"].as_object_mut().unwrap().remove("gw");
    now["mcpServers"]["tool"]["command"] = serde_json::json!("t2");
    now["projects"]["/work/a"]["mcpServers"]
        .as_object_mut()
        .unwrap()
        .remove("loc");
    now["projects"]["/work/a"]["mcpServers"]["second"]["command"] = serde_json::json!("s2");
    write_config(&env, &now);
    let plan = plan_for(&core, vec![Change::Restore { backup }]);
    applied(&core, &plan);
    let after = config_json(&env);
    assert_eq!(after["mcpServers"], config["mcpServers"]);
    assert_eq!(
        after["projects"]["/work/a"]["mcpServers"]["loc"],
        config["projects"]["/work/a"]["mcpServers"]["loc"]
    );
    assert_eq!(
        after["projects"]["/work/a"]["mcpServers"]["second"]["command"],
        "s1"
    );
}

#[test]
fn restoring_what_is_already_there_writes_nothing() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    applied(&core, &plan);
    let backup = newest_backup(&core);
    let plan = plan_for(&core, vec![Change::Restore { backup }]);
    assert_eq!(
        plan.summary[1..],
        ["Turn on loc in a".to_owned()],
        "{plan:#?}"
    );
    applied(&core, &plan);
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!([]));
    // The backup taken before the restore matches the file before it: restoring it again is the undo; restoring
    // the first one again does nothing.
    let first = activity(&core).entries[1].backups[0].path.clone();
    let again = plan_for(&core, vec![Change::Restore { backup: first }]);
    assert!(again.nothing_to_do, "{again:#?}");
}

#[test]
fn a_restore_is_refused_for_anything_but_a_kept_backup_of_claude_json() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    for backup in [
        "../cfg/.claude.json".to_owned(),
        env.dir
            .path()
            .join("cfg")
            .join(".claude.json")
            .display()
            .to_string(),
        "claude.json.0001760000000000-0.bak".to_owned(),
    ] {
        let why = plan_failure(&core, vec![Change::Restore { backup }]);
        assert!(!why.is_empty());
    }
    let why = plan_failure(
        &core,
        vec![
            Change::Restore {
                backup: "claude.json.0001760000000000-0.bak".to_owned(),
            },
            Change::Delete { id: user_id("gw") },
        ],
    );
    assert!(why.contains("on its own"), "{why}");
    assert!(config_json(&env)["mcpServers"].get("gw").is_some());
}

#[test]
fn a_restore_puts_back_a_server_that_signs_in_with_its_kept_client_secret() {
    let (env, store, _key, _repo) = oauth_move_env(
        r#"{"rules":[{"args":["mcp","add-json"],"delay_ms":600,"stdout_file":"added.txt"}]}"#,
    );
    std::fs::write(
        env.dir.path().join("added.txt"),
        "Added http MCP server sso to user config\n",
    )
    .unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    // A plain change, for a backup that has sso; then sso is removed (by Claude Code, say).
    let mut config = config_json(&env);
    config["mcpServers"]["plain"] = serde_json::json!({"type": "stdio", "command": "p"});
    write_config(&env, &config);
    let plan = plan_for(
        &core,
        vec![Change::Delete {
            id: user_id("plain"),
        }],
    );
    applied(&core, &plan);
    let backup = newest_backup(&core);
    let mut config = config_json(&env);
    config["mcpServers"].as_object_mut().unwrap().remove("sso");
    write_config(&env, &config);
    let plan = plan_for(&core, vec![Change::Restore { backup }]);
    assert!(
        plan.summary
            .contains(&"Put back sso for the user".to_owned()),
        "{plan:#?}"
    );
    let runs = env.changes_run().len();
    core.send(Command::Apply(plan.id));
    env.wait_for_run(runs + 1);
    let mut config = config_json(&env);
    config["mcpServers"]["sso"] = serde_json::from_str(SSO_ENTRY).unwrap();
    write_config(&env, &config);
    let events = collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. })),
        "{events:#?}"
    );
    let run = &env.changes_run()[runs];
    assert_eq!(
        run.0[..5],
        ["mcp", "add-json", "-s", "user", "--client-secret"]
    );
    assert!(!run.0.iter().any(|a| a.contains(CLIENT_SECRET)));
    assert_eq!(run.2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
    assert!(
        config_json(&env)["mcpServers"].get("plain").is_some(),
        "plain is back too"
    );
}
