//! Engine tests: switching servers on and off, the activity log, and changes Claude Code writes over.
use super::*;

#[test]
fn switching_a_user_server_off_covers_every_known_project() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    assert!(!plan.nothing_to_do);
    assert!(
        plan.warnings.iter().any(|w| w.contains("2 projects")),
        "{:?}",
        plan.warnings
    );
    core.send(Command::Apply(plan.id));
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
            && e.iter().any(|x| {
                matches!(x, Event::Snapshot(s)
                    if s.servers.iter().any(|r| r.name == "gw" && !r.enabled))
            })
    });
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Applied {
            backup: Some(_),
            ..
        }
    )));
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["gw"]));
    assert_eq!(
        disabled_list(&env, "/work/b"),
        serde_json::json!(["other", "gw"])
    );
    // Everything else is as it was, in the same order.
    let now = config_json(&env);
    let keys: Vec<_> = now.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["numStartups", "mcpServers", "projects", "tail"]);
    assert_eq!(now["mcpServers"], two_projects("end")["mcpServers"]);
}

#[test]
fn switching_a_user_server_back_on_removes_it_everywhere() {
    let mut start = two_projects("end");
    start["projects"]["/work/a"]["disabledMcpServers"] = serde_json::json!(["gw"]);
    start["projects"]["/work/b"]["disabledMcpServers"] = serde_json::json!(["gw", "other"]);
    let env = quiet_env(&start);
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: true,
        }],
    );
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!([]));
    assert_eq!(disabled_list(&env, "/work/b"), serde_json::json!(["other"]));
}

#[test]
fn a_local_server_is_switched_in_its_own_project_only() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));
    assert_eq!(disabled_list(&env, "/work/b"), serde_json::json!(["other"]));
}

#[test]
fn several_changes_make_one_plan_and_one_backup() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![
            Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            },
            Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            },
        ],
    );
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    assert_eq!(
        disabled_list(&env, "/work/a"),
        serde_json::json!(["loc", "gw"])
    );
    assert_eq!(
        std::fs::read_dir(env.dir.path().join("backups"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn the_plan_never_shows_server_settings() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    let shown = format!("{plan:?}");
    assert!(shown.contains("gw") && !shown.contains("s3cret"), "{shown}");
}

#[test]
fn a_plan_can_be_applied_once_and_not_after_it_is_discarded() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let change = Change::SetEnabled {
        id: local_id("loc", "/work/a"),
        enabled: false,
    };
    let first = plan_for(&core, vec![change.clone()]);
    core.send(Command::Apply(first.id));
    core.send(Command::Apply(first.id));
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
            && e.iter().any(|x| matches!(x, Event::ApplyFailed { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ApplyFailed { plan, .. } if *plan == first.id))
    );
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));

    let second = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: true,
        }],
    );
    core.send(Command::Discard(second.id));
    core.send(Command::Apply(second.id));
    collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::ApplyFailed { plan, .. } if *plan == second.id))
    });
    assert_eq!(disabled_list(&env, "/work/a"), serde_json::json!(["loc"]));
}

#[test]
fn a_server_that_is_gone_makes_no_plan() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    core.send(Command::Prepare(vec![Change::SetEnabled {
        id: user_id("ghost"),
        enabled: false,
    }]));
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::PlanFailed(_)))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::PlanFailed(m) if m.contains("ghost")))
    );
    assert!(!events.iter().any(|e| matches!(e, Event::PlanReady(_))));
}

#[test]
fn a_change_that_is_already_true_is_reported_as_nothing_to_do() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: true,
        }],
    );
    assert!(plan.nothing_to_do);
    core.send(Command::Apply(plan.id));
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Applied {
            changed: false,
            backup: None,
            ..
        }
    )));
    assert!(!env.dir.path().join("backups").exists());
    // Which server, and where, not only that nothing changes.
    assert_eq!(plan.already, vec!["loc is already on in a.".to_owned()]);
    let off = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    assert!(!off.nothing_to_do && off.already.is_empty(), "{off:?}");
}

/// The owner's report: using one profile, then another, in the same project said only "The configuration already
/// looks like this" each time, since a profile used without "only" turns its servers on and they were on already.
/// The review says which servers are on where, for each profile.
#[test]
fn a_profile_already_in_effect_says_which_servers_are_on_where() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let profile =
        |name: &str, members: &[ServerId], project: Option<&str>, only: bool| Change::UseProfile {
            name: name.to_owned(),
            members: members
                .iter()
                .map(|m| crate::model::ConnectionId::new(m.scope, m.project.as_deref(), &m.name))
                .collect(),
            project: project.map(str::to_owned),
            only,
        };
    let work = plan_for(
        &core,
        vec![profile(
            "Work",
            &[user_id("gw"), local_id("loc", "/work/a")],
            Some("/work/a"),
            false,
        )],
    );
    assert!(work.nothing_to_do, "{work:?}");
    assert_eq!(
        work.already,
        vec![
            "The servers of Work, gw and loc, are already on in a. Other servers there are left as they are."
                .to_owned()
        ]
    );
    // Named in the profile's own order, as its card names them.
    let reversed = plan_for(
        &core,
        vec![profile(
            "Work",
            &[local_id("loc", "/work/a"), user_id("gw")],
            Some("/work/a"),
            false,
        )],
    );
    assert!(
        reversed.already[0].starts_with("The servers of Work, loc and gw, "),
        "{reversed:?}"
    );
    let docs = plan_for(
        &core,
        vec![profile("Docs", &[user_id("gw")], Some("/work/a"), false)],
    );
    assert_eq!(
        docs.already,
        vec![
            "The server of Docs, gw, is already on in a. Other servers there are left as they are."
                .to_owned()
        ]
    );
    // Work's servers are all a has: nothing else to turn off. Docs leaves loc on there.
    assert!(!work.others_on && docs.others_on, "{work:?} {docs:?}");
    // Reviewer's case: none of its servers loads there. Said why, with no hint to turn the others off.
    let local = plan_for(
        &core,
        vec![profile(
            "Local",
            &[local_id("loc", "/work/a")],
            Some("/work/b"),
            false,
        )],
    );
    assert!(local.nothing_to_do, "{local:?}");
    assert_eq!(
        local.already,
        vec![
            "None of the servers of Local loads in b, so there is nothing to turn on there.".to_owned(),
            "In Local, these load only in a project of their own, so they stay as they are: loc (a).".to_owned()
        ]
    );
    assert!(!local.others_on, "{local:?}");
    // Alone: in b, gw is the only server, so it is already the only one on.
    let alone = plan_for(
        &core,
        vec![profile("Docs", &[user_id("gw")], Some("/work/b"), true)],
    );
    assert!(alone.nothing_to_do, "{alone:?}");
    assert!(!alone.others_on, "{alone:?}");
    assert_eq!(
        alone.already,
        vec!["The server of Docs, gw, is already the only one on in b.".to_owned()]
    );
    // Every project: said so, not one project's name.
    let everywhere = plan_for(&core, vec![profile("Docs", &[user_id("gw")], None, false)]);
    assert_eq!(
        everywhere.already,
        vec![
            "The server of Docs, gw, is already on in every project Claude Code knows. Other servers there are \
             left as they are."
                .to_owned()
        ]
    );
    // Something to write: nothing said to be so already.
    let changes = plan_for(
        &core,
        vec![profile("Docs", &[user_id("gw")], Some("/work/a"), true)],
    );
    assert!(
        !changes.nothing_to_do && changes.already.is_empty(),
        "{changes:?}"
    );
}

#[test]
fn claude_desktop_is_read_only_when_asked_and_never_written() {
    let env = quiet_env(&two_projects("end"));
    let roaming = env.dir.path().join("Roaming");
    let file = roaming.join("Claude").join(crate::desktop::FILE_NAME);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text = r#"{"mcpServers": {"files": {"command": "npx", "args": ["-y", "fs"]}}}"#;
    std::fs::write(&file, text).unwrap();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    let core = start_with(
        EngineConfig {
            desktop: crate::desktop::DesktopRoots {
                roaming: Some(roaming),
                packages: None,
            },
            ..env.config()
        },
        || {},
    );
    // Starting up reads Claude Code's configuration, not Claude Desktop's.
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    assert!(!events.iter().any(|e| matches!(e, Event::Desktop(_))));
    for _ in 0..2 {
        core.send(Command::Desktop);
        let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Desktop(_))));
        let desktop = events
            .into_iter()
            .find_map(|e| match e {
                Event::Desktop(d) => Some(d),
                _ => None,
            })
            .unwrap();
        let names: Vec<&str> = desktop.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["files"]);
        assert_eq!(desktop.servers[0].target, "npx -y fs");
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    // Nothing else was put next to it either.
    assert_eq!(
        std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn a_saved_change_is_in_the_activity_log_with_its_backup() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    assert_eq!(activity(&core), crate::activity::Activity::default());
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    core.send(Command::Apply(plan.id));
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    let backup = events
        .iter()
        .find_map(|e| match e {
            Event::Applied { backup, .. } => backup.clone(),
            _ => None,
        })
        .unwrap();
    let log = activity(&core);
    assert_eq!(log.problem, None);
    let [entry] = &log.entries[..] else {
        panic!("one entry: {log:?}");
    };
    assert_eq!(entry.summary, plan.summary);
    assert_eq!(entry.failed, None);
    assert_eq!(entry.backups.len(), 1);
    assert_eq!(entry.backups[0].path, backup);
    assert!(entry.backups[0].kept);
    assert_eq!(entry.backups[0].of, ".claude.json");
}

#[test]
fn a_change_that_writes_nothing_is_not_logged() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: true,
        }],
    );
    assert!(plan.nothing_to_do);
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    assert!(activity(&core).entries.is_empty());
    assert!(!env.dir.path().join("activity.jsonl").exists());
}

#[test]
fn a_user_server_cannot_be_switched_when_no_project_is_known() {
    let env =
        quiet_env(&serde_json::json!({"mcpServers": {"gw": {"type": "stdio", "command": "x"}}}));
    let core = env.start();
    core.send(Command::Prepare(vec![Change::SetEnabled {
        id: user_id("gw"),
        enabled: false,
    }]));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::PlanFailed(_)))
    });
}

#[test]
fn toggling_a_project_server_writes_only_the_user_file_and_does_not_approve_it() {
    let env = quiet_env(&serde_json::json!({}));
    let repo = env.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join(".mcp.json"),
        r#"{"mcpServers":{"ps":{"type":"stdio","command":"danger"}}}"#,
    )
    .unwrap();
    let key = repo.to_string_lossy().replace('\\', "/");
    write_config(&env, &serde_json::json!({"projects": {key.clone(): {}}}));
    let core = env.start();
    let id = ServerId::new(Scope::Project, Some(&key), "ps");
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: id.clone(),
            enabled: true,
        }],
    );
    assert!(
        plan.warnings.iter().any(|w| w.contains("not approved")),
        "{:?}",
        plan.warnings
    );
    let plan = plan_for(&core, vec![Change::SetEnabled { id, enabled: false }]);
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    assert_eq!(
        config_json(&env)["projects"][&key]["disabledMcpServers"],
        serde_json::json!(["ps"])
    );
    // The repository was not touched: no approval file, same .mcp.json.
    assert!(!repo.join(".claude").exists());
    assert_eq!(
        std::fs::read_to_string(repo.join(".mcp.json")).unwrap(),
        r#"{"mcpServers":{"ps":{"type":"stdio","command":"danger"}}}"#
    );
}

#[test]
fn a_lock_held_by_claude_stops_the_write_with_a_clear_message() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    let before = std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
    std::fs::create_dir(env.dir.path().join("cfg").join(".claude.json.lock")).unwrap();
    core.send(Command::Apply(plan.id));
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::ApplyFailed { .. }))
    });
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap();
    assert!(error.contains("writing its configuration"), "{error}");
    assert_eq!(
        std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap(),
        before
    );
}

#[test]
fn a_change_claude_overwrites_a_moment_later_is_put_back() {
    let env = quiet_env(&two_projects("end"));
    let mut config = env.config();
    config.verify_delay = Duration::from_millis(800);
    let core = start_with(config, || {});
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    // Claude saves an older copy without taking the lock, with a newer counter of its own.
    let mut theirs = two_projects("end");
    theirs["numStartups"] = serde_json::json!(4);
    write_config(&env, &theirs);
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Reapplied { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Reapplied { plan: p } if *p == plan.id))
    );
    let now = config_json(&env);
    assert_eq!(now["numStartups"], 4);
    assert_eq!(
        now["projects"]["/work/a"]["disabledMcpServers"],
        serde_json::json!(["loc"])
    );
}

/// Applies `changes` and waits for the answer.
fn apply_now(core: &Core, changes: Vec<Change>) {
    let plan = plan_for(core, changes);
    core.send(Command::Apply(plan.id));
    collect(core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
}

#[test]
fn switching_back_right_away_is_not_undone_by_the_first_change() {
    let env = quiet_env(&two_projects("end"));
    let mut config = env.config();
    config.verify_delay = Duration::from_millis(600);
    let core = start_with(config, || {});
    let loc = local_id("loc", "/work/a");
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: loc.clone(),
            enabled: false,
        }],
    );
    // Within the first change's verify window, the user switches it back on.
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: loc,
            enabled: true,
        }],
    );
    let events = events_for(&core, Duration::from_millis(3000));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Reapplied { .. } | Event::ApplyFailed { .. })),
        "{events:#?}"
    );
    assert_eq!(
        config_json(&env)["projects"]["/work/a"]["disabledMcpServers"],
        serde_json::json!([])
    );
}

#[test]
fn claude_overwriting_after_two_quick_changes_loses_neither() {
    // Found reviewing main: the first change's check stood down for the second, whose check only put its own
    // change back, so the first was silently lost.
    let env = quiet_env(&two_projects("end"));
    let mut config = env.config();
    config.verify_delay = Duration::from_millis(600);
    let core = start_with(config, || {});
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    // Claude saves a copy it read before either change, without the lock.
    let mut theirs = two_projects("end");
    theirs["numStartups"] = serde_json::json!(9);
    write_config(&env, &theirs);
    let events = events_for(&core, Duration::from_millis(3000));
    assert!(
        events.iter().any(|e| matches!(e, Event::Reapplied { .. })),
        "{events:#?}"
    );
    let now = config_json(&env);
    assert_eq!(now["numStartups"], 9, "Claude's own change stays");
    assert_eq!(
        now["projects"]["/work/a"]["disabledMcpServers"],
        serde_json::json!(["loc", "gw"])
    );
}

#[test]
fn a_later_change_that_writes_nothing_still_has_the_earlier_one_checked() {
    // The variant: the second change finds nothing to write, yet it numbers last on the file.
    let env = quiet_env(&two_projects("end"));
    let mut config = env.config();
    config.verify_delay = Duration::from_millis(600);
    let core = start_with(config, || {});
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    // Already off: nothing to write.
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    write_config(&env, &two_projects("end"));
    events_for(&core, Duration::from_millis(3000));
    assert_eq!(
        config_json(&env)["projects"]["/work/a"]["disabledMcpServers"],
        serde_json::json!(["loc"])
    );
}

#[test]
fn a_later_change_to_another_server_still_leaves_both_in_place() {
    let env = quiet_env(&two_projects("end"));
    let mut config = env.config();
    config.verify_delay = Duration::from_millis(600);
    let core = start_with(config, || {});
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    apply_now(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    let events = events_for(&core, Duration::from_millis(3000));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Reapplied { .. } | Event::ApplyFailed { .. })),
        "{events:#?}"
    );
    let disabled = &config_json(&env)["projects"]["/work/a"]["disabledMcpServers"];
    let names: Vec<&str> = disabled
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(names.contains(&"loc") && names.contains(&"gw"), "{names:?}");
}
