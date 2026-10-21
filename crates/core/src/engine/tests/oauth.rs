//! Engine tests: adding, moving and copying servers that sign in with OAuth, through Claude Code's own commands.
use super::*;

/// Moves sso for the user to the project; Claude Code's remove works (the test plays its part) and every add
/// fails, so putting it back fails too and Klefki keeps its definition. Returns the kept definition's id.
fn keep_sso(env: &Env, core: &Core, key: &str) -> String {
    let plan = plan_for(
        core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key.to_owned()),
        }],
    );
    core.send(Command::Apply(plan.id));
    env.wait_for_run(1);
    let mut config = config_json(env);
    config["mcpServers"].as_object_mut().unwrap().remove("sso");
    write_config(env, &config);
    let events = collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:#?}"));
    assert!(error.contains("Klefki kept"), "{error}");
    let kept = &snapshots(&events).last().unwrap().kept;
    assert_eq!(kept.len(), 1, "{kept:?}");
    kept[0].id.clone()
}

const REMOVE_THEN_ADDS_FAIL: &str = r#"{"rules":[
    {"args":["mcp","remove"],"delay_ms":600,"stdout_file":"removed.txt"},
    {"args":["mcp","add-json"],"exit_code":1,"stdout_file":"failed.txt"}]}"#;

#[test]
fn a_failed_move_that_removed_the_server_is_logged_with_its_backups() {
    // Cross-review of the activity log, B1: the move's backups were dropped on failure, so the change was not
    // logged and its backup showed as one from before the log.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    keep_sso(&env, &core, &key);
    let log = activity(&core);
    let [entry] = &log.entries[..] else {
        panic!("one entry, every backup in it: {log:#?}");
    };
    assert_eq!(entry.summary.len(), 1, "{log:#?}");
    assert!(
        entry
            .failed
            .as_deref()
            .is_some_and(|e| e.contains("Klefki kept")),
        "{log:#?}"
    );
    assert!(!entry.backups.is_empty());
}

#[test]
fn a_plain_move_logs_every_backup_it_made() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("gw"),
            to: Target::Local("/work/a".to_owned()),
        }],
    );
    assert!(!plan.nothing_to_do, "{plan:?}");
    core.send(Command::Apply(plan.id));
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
    let log = activity(&core);
    let [entry] = &log.entries[..] else {
        panic!("one entry, every backup in it: {log:#?}");
    };
    assert_eq!(entry.failed, None);
    assert!(!entry.backups.is_empty(), "{log:#?}");
}

#[test]
fn a_kept_definition_can_be_put_back() {
    // Cross-review B1: the plan looked for the kept definition in a fresh read of the configuration, which
    // never lists them, so Put back always said it was gone.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let kept = keep_sso(&env, &core, &key);
    assert!(config_json(&env)["mcpServers"].get("sso").is_none());
    // Claude Code works again.
    write_scenario(
        env.dir.path(),
        r#"{"rules":[{"args":["mcp","add-json"],"delay_ms":600,"stdout_file":"added.txt"}]}"#,
        &[],
    );
    let plan = plan_for(&core, vec![Change::PutBack { kept: kept.clone() }]);
    assert_eq!(plan.summary, ["Put back sso for the user"]);
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
    assert!(snapshots(&events).last().unwrap().kept.is_empty());
    let last = &env.changes_run()[runs];
    assert_eq!(
        last.0[..5],
        ["mcp", "add-json", "-s", "user", "--client-secret"]
    );
    assert_eq!(last.2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
}

#[test]
fn a_kept_definitions_client_secret_is_not_offered_to_forget() {
    // Put back hands the kept secret to Claude Code again, so it is in use although sso is in no file now.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    keep_sso(&env, &core, &key);
    let sso = user_id("sso").connection;
    assert!(store.get(&sso).unwrap().is_some());
    let report = secrets_report(&core);
    assert!(report.unused.is_empty(), "{report:?}");
    core.send(Command::ForgetUnusedSecret(sso.clone()));
    let refused = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))))
        .into_iter()
        .find_map(|e| match e {
            Event::UnusedSecretForgotten { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
    assert!(refused.is_err());
    assert!(store.get(&sso).unwrap().is_some());
}

#[test]
fn a_kept_definition_can_be_forgotten() {
    // The variant: Forget, which also said it was gone.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let kept = keep_sso(&env, &core, &key);
    let plan = plan_for(&core, vec![Change::ForgetKept { kept }]);
    assert!(plan.destructive);
    let events = apply_and_wait(&core, &plan);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. })),
        "{events:#?}"
    );
    assert!(snapshots(&events).last().unwrap().kept.is_empty());
}

#[test]
fn a_definition_put_back_without_some_settings_is_listed_as_back() {
    // Cross-review round 2: put back, but without alwaysAllow, the notice still said it was out of the
    // configuration, and Put back again failed with "sso already exists there".
    let (env, store, key, _) = oauth_move_env(
        r#"{"rules":[
        {"args":["mcp","remove"],"delay_ms":600,"stdout_file":"removed.txt"},
        {"args":["mcp","add-json","-s","local"],"exit_code":1,"stdout_file":"failed.txt"},
        {"args":["mcp","add-json","-s","user"],"delay_ms":600,"stdout_file":"added.txt"}]}"#,
    );
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let mut config = config_json(&env);
    config["mcpServers"]["sso"]["alwaysAllow"] = serde_json::json!(["search"]);
    write_config(&env, &config);
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    core.send(Command::Apply(plan.id));
    env.wait_for_run(1);
    let mut config = config_json(&env);
    config["mcpServers"].as_object_mut().unwrap().remove("sso");
    write_config(&env, &config);
    // Claude Code puts it back as it writes it: without alwaysAllow.
    env.wait_for_run(3);
    let mut config = config_json(&env);
    config["mcpServers"]["sso"] = serde_json::from_str(SSO_ENTRY).unwrap();
    write_config(&env, &config);
    let events = collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:#?}"));
    assert!(error.contains("left out alwaysAllow"), "{error}");
    assert!(!error.contains("put that back"), "{error}");
    let kept = &snapshots(&events).last().unwrap().kept;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(kept[0].back, "{kept:?}");
    // The variant: one that is not back is not listed as back.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    keep_sso(&env, &core, &key);
    core.send(Command::Refresh { live_all: false });
    let events = collect(&core, |e| !snapshots(e).is_empty());
    assert!(!snapshots(&events).last().unwrap().kept[0].back);
}

#[test]
fn a_move_into_a_missing_folder_is_refused_before_anything_runs() {
    // Cross-review B2: the folder was checked only when the add ran, after the remove had signed sso out.
    let (env, store, key, repo) =
        oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"stdout_file":"removed.txt"}]}"#);
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    std::fs::remove_dir(&repo).unwrap();
    let refused = plan_failure(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key.clone()),
        }],
    );
    assert!(refused.contains("is not there"), "{refused}");
    // Gone after the review: stopped before the remove.
    std::fs::create_dir(&repo).unwrap();
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    std::fs::remove_dir(&repo).unwrap();
    let events = apply_and_wait(&core, &plan);
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:#?}"));
    assert!(error.contains("Nothing was changed"), "{error}");
    assert!(env.changes_run().is_empty(), "{:?}", env.changes_run());
    assert!(config_json(&env)["mcpServers"].get("sso").is_some());
}

#[test]
fn a_move_out_of_a_missing_folder_is_refused_before_anything_runs() {
    // The variant: the folder the server is in is gone.
    let (env, store, key, repo) = oauth_move_env(r#"{"rules":[]}"#);
    let mut config = config_json(&env);
    let sso = config["mcpServers"]["sso"].take();
    config["mcpServers"] = serde_json::json!({});
    config["projects"][&key]["mcpServers"] = serde_json::json!({ "sso": sso });
    write_config(&env, &config);
    store
        .set(
            &crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso"),
            &crate::secrets::Secret::new(CLIENT_SECRET.to_owned()),
        )
        .unwrap();
    std::fs::remove_dir(&repo).unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let refused = plan_failure(
        &core,
        vec![Change::Move {
            id: local_id("sso", &key),
            to: Target::User,
        }],
    );
    assert!(refused.contains("is not there"), "{refused}");
    assert!(env.changes_run().is_empty());
}

#[cfg(unix)]
#[test]
fn a_project_reached_through_a_link_is_refused_before_anything_runs() {
    // Cross-review S3: Claude Code files a local server under the folder itself, so one added from a link
    // would land under another key, and Klefki would not find it.
    let (env, store, _, repo) = oauth_move_env(r#"{"rules":[]}"#);
    let link = env.dir.path().join("link");
    std::os::unix::fs::symlink(&repo, &link).unwrap();
    let key = link.to_string_lossy().into_owned();
    let mut config = config_json(&env);
    config["projects"][&key] = serde_json::json!({});
    write_config(&env, &config);
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let refused = plan_failure(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    assert!(refused.contains("through a link"), "{refused}");
    assert!(env.changes_run().is_empty());
}

#[test]
fn the_review_shows_what_claude_code_writes_and_warns_what_it_drops() {
    // Cross-review B3: the diff showed alwaysAllow kept, and Claude Code dropped it.
    let (env, store, key, _) = oauth_move_env(r#"{"rules":[]}"#);
    let mut config = config_json(&env);
    config["mcpServers"]["sso"]["alwaysAllow"] = serde_json::json!(["search"]);
    write_config(&env, &config);
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    let shown: Vec<&String> = plan.files.iter().flat_map(|f| &f.lines).collect();
    assert!(
        !shown.iter().any(|l| l.contains("alwaysAllow")),
        "{shown:?}"
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("loses alwaysAllow")),
        "{:?}",
        plan.warnings
    );
    // The costliest first: signing in again, before the scope and the dropped settings (cross-review round 2).
    assert!(
        plan.warnings[0].contains("sign in to it again"),
        "{:?}",
        plan.warnings
    );
    // The variant: an edit in place.
    let mut edited: serde_json::Value = serde_json::from_str(SSO_ENTRY).unwrap();
    edited["url"] = serde_json::json!("https://sso.example.com/v2");
    edited["alwaysAllow"] = serde_json::json!(["search"]);
    let plan = plan_for(
        &core,
        vec![Change::Replace {
            id: user_id("sso"),
            json: edited.to_string(),
        }],
    );
    let shown: Vec<&String> = plan.files.iter().flat_map(|f| &f.lines).collect();
    assert!(
        !shown
            .iter()
            .any(|l| l.contains("+") && l.contains("alwaysAllow")),
        "{shown:?}"
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("loses alwaysAllow"))
    );
}

#[test]
fn a_kept_definition_whose_change_got_through_is_not_offered() {
    // Cross-review S1: Klefki stopped after the add but before deleting the recovery file. The next listing
    // must not offer to put back a server that was moved after all.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    keep_sso(&env, &core, &key);
    // The move landed after all, and Klefki's copy of the client secret went along (as if Klefki stopped
    // before it could tidy up).
    let mut config = config_json(&env);
    config["projects"][&key]["mcpServers"] =
        serde_json::json!({"sso": serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()});
    write_config(&env, &config);
    let (from, to) = (
        crate::model::ConnectionId::new(Scope::User, None, "sso"),
        crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso"),
    );
    store
        .set(&to, &crate::secrets::Secret::new(CLIENT_SECRET.to_owned()))
        .unwrap();
    core.send(Command::Refresh { live_all: false });
    // A refresh the watcher started before the write above can still come first, and rightly lists it as kept.
    let events = collect(&core, |e| snapshots(e).iter().any(|s| s.kept.is_empty()));
    assert!(snapshots(&events).last().unwrap().kept.is_empty());
    assert_eq!(
        std::fs::read_dir(env.dir.path().join("recovery"))
            .map(|d| d.count())
            .unwrap_or(0),
        0
    );
    // The copy left where it was is deleted, for a server put there later not to claim it.
    assert!(store.get(&from).unwrap().is_none());
    assert!(store.get(&to).unwrap().is_some());
}

#[test]
fn a_kept_definition_whose_change_got_through_without_its_client_secret_is_still_offered() {
    // The variant: the server is where it was going, but Klefki's copy of its client secret is not, so nothing
    // shows Claude Code has it. Putting it back is the only way left to hand it over, so it is still offered.
    let (env, store, key, _) = oauth_move_env(REMOVE_THEN_ADDS_FAIL);
    std::fs::write(env.dir.path().join("failed.txt"), "simulated failure").unwrap();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let kept = keep_sso(&env, &core, &key);
    let mut config = config_json(&env);
    config["projects"][&key]["mcpServers"] =
        serde_json::json!({"sso": serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()});
    write_config(&env, &config);
    core.send(Command::Refresh { live_all: false });
    let events = collect(&core, |e| !snapshots(e).is_empty());
    let listed = &snapshots(&events).last().unwrap().kept;
    assert_eq!(listed.iter().map(|k| &k.id).collect::<Vec<_>>(), [&kept]);
    let from = crate::model::ConnectionId::new(Scope::User, None, "sso");
    assert!(store.get(&from).unwrap().is_some());
}

#[test]
fn a_slow_change_says_what_claude_code_is_doing() {
    // Cross-review S4: a swap runs two or three of Claude Code's commands, seconds each.
    let (env, store, key, _) =
        oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"stdout_file":"removed.txt"}]}"#);
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    let events = apply_and_wait(&core, &plan);
    let steps: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            Event::Applying { plan: p, step } if *p == plan.id => Some(step),
            _ => None,
        })
        .collect();
    assert_eq!(steps, ["Claude Code is removing sso for the user\u{2026}"]);
}

#[test]
fn moving_a_user_server_into_one_project_says_where_it_is_no_longer() {
    // Cross-review S5.
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("gw"),
            to: Target::Local("/work/b".into()),
        }],
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w == "gw will only be in b: it is no longer there for you in a."),
        "{:?}",
        plan.warnings
    );
}

#[test]
fn moving_into_a_project_where_it_is_switched_off_says_it_stays_off() {
    // Cross-review round 2: the move landed it switched off in the one place chosen, with no word of it.
    let mut config = two_projects("end");
    config["projects"]["/work/b"]["disabledMcpServers"] = serde_json::json!(["other", "gw"]);
    let env = quiet_env(&config);
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("gw"),
            to: Target::Local("/work/b".into()),
        }],
    );
    assert!(
        plan.warnings.iter().any(|w| w
            == "gw is switched off in b, so it stays off there. Switch it on there to use it."),
        "{:?}",
        plan.warnings
    );
    // The variant: a copy there; and nothing said where it is on.
    let plan = plan_for(
        &core,
        vec![Change::Duplicate {
            id: user_id("gw"),
            name: "gw".into(),
            targets: vec![Target::Local("/work/b".into())],
            adjust: Default::default(),
        }],
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("switched off in b")),
        "{:?}",
        plan.warnings
    );
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("gw"),
            to: Target::Local("/work/a".into()),
        }],
    );
    assert!(
        !plan.warnings.iter().any(|w| w.contains("switched off")),
        "{:?}",
        plan.warnings
    );
}

fn same_dir(a: &str, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

#[test]
fn an_oauth_server_is_moved_by_claude_code_and_keeps_its_client_secret() {
    let (env, store, key, repo) = oauth_move_env(
        r#"{"rules":[
            {"args":["mcp","remove"],"delay_ms":800,"stdout_file":"removed.txt"},
            {"args":["mcp","add-json"],"delay_ms":800,"stdout_file":"added.txt"}]}"#,
    );
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key.clone()),
        }],
    );
    assert!(
        plan.warnings.iter().any(|w| w.contains("sign in")),
        "{:?}",
        plan.warnings
    );
    assert!(env.changes_run().is_empty(), "nothing runs before Apply");
    core.send(Command::Apply(plan.id));
    // Claude Code's part, which the fake CLI cannot play: it removes the entry, then adds it in the project.
    env.wait_for_run(1);
    let mut config = config_json(&env);
    config["mcpServers"].as_object_mut().unwrap().remove("sso");
    write_config(&env, &config);
    env.wait_for_run(2);
    let mut config = config_json(&env);
    config["projects"][&key]["mcpServers"] =
        serde_json::json!({"sso": serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()});
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
    let runs = env.changes_run();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, ["mcp", "remove", "-s", "user", "--", "sso"]);
    assert!(
        same_dir(&runs[0].1, &env.dir.path().join("home")),
        "{runs:?}"
    );
    assert_eq!(runs[0].2, None, "remove gets no secret");
    assert_eq!(
        runs[1].0[..7],
        [
            "mcp",
            "add-json",
            "-s",
            "local",
            "--client-secret",
            "--",
            "sso"
        ]
    );
    assert!(same_dir(&runs[1].1, &repo), "{runs:?}");
    assert_eq!(runs[1].2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
    assert!(
        !std::fs::read_to_string(&env.log)
            .unwrap()
            .contains(CLIENT_SECRET)
    );
    // Klefki's copy moved with it.
    let values = store.values.lock().unwrap().clone();
    let local = crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso");
    assert_eq!(
        values.get(&crate::secrets::user_for(&local)),
        Some(&CLIENT_SECRET.to_owned())
    );
    assert_eq!(values.len(), 1, "{values:?}");
    let config = config_json(&env);
    assert_eq!(config["tail"], 1);
    assert!(config["mcpServers"].get("sso").is_none());
    verify_passes(&core);
}

// ---- a copy made with another address, callback port or client

fn rebase(from: &str, to: &str) -> crate::adjust::Rebase {
    crate::adjust::Rebase {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

#[test]
fn a_copy_with_another_base_address_keeps_its_header_values_and_the_original() {
    let gw = serde_json::json!({"type": "http", "url": "https://mcp.acme.com/v1/mcp", "headers": {"Authorization": "Bearer s3cret-value"}});
    let env = quiet_env(&serde_json::json!({"mcpServers": {"gw": gw.clone()}, "projects": {}}));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Duplicate {
            id: user_id("gw"),
            name: "gw_eu".into(),
            targets: vec![Target::User],
            adjust: crate::adjust::Adjust {
                base: Some(rebase("https://mcp.acme.com/v1", "https://eu.acme.com/v2")),
                ..Default::default()
            },
        }],
    );
    // The new host gets the original's header values, and the review says so first.
    assert_eq!(
        plan.warnings.first().map(String::as_str),
        Some(
            "gw_eu sends the original's header values to eu.acme.com. Check the address before you go on."
        ),
        "{:?}",
        plan.warnings
    );
    // The review shows the new address, never the header's value.
    let shown = format!("{plan:?}");
    assert!(shown.contains("https://eu.acme.com/v2/mcp"), "{shown}");
    assert!(!shown.contains("s3cret-value"), "{shown}");
    // The variant: another path on the same host is nothing to check.
    let same_host = plan_for(
        &core,
        vec![Change::Duplicate {
            id: user_id("gw"),
            name: "gw_v2".into(),
            targets: vec![Target::User],
            adjust: crate::adjust::Adjust {
                base: Some(rebase("https://mcp.acme.com/v1", "https://mcp.acme.com/v2")),
                ..Default::default()
            },
        }],
    );
    assert!(
        !same_host
            .warnings
            .iter()
            .any(|w| w.contains("sends the original's")),
        "{:?}",
        same_host.warnings
    );
    core.send(Command::Discard(same_host.id));
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    assert_eq!(now["mcpServers"]["gw"], gw);
    let mut copy = gw.clone();
    copy["url"] = serde_json::json!("https://eu.acme.com/v2/mcp");
    assert_eq!(now["mcpServers"]["gw_eu"], copy);
    verify_passes(&core);
}

#[test]
fn a_copy_whose_address_does_not_fit_is_refused_before_anything_changes() {
    let gw = serde_json::json!({"type": "http", "url": "https://mcp.acme.com/mcp"});
    let env = quiet_env(
        &serde_json::json!({"mcpServers": {"gw": gw, "st": {"command": "node"}}, "projects": {}}),
    );
    let core = env.start();
    let before = config_json(&env);
    let copy = |id: ServerId, adjust| Change::Duplicate {
        id,
        name: "c".into(),
        targets: vec![Target::User],
        adjust,
    };
    for (change, said) in [
        // Not an address Claude Code takes.
        (
            copy(
                user_id("gw"),
                crate::adjust::Adjust {
                    base: Some(rebase("https://mcp.acme.com", "ftp://x")),
                    ..Default::default()
                },
            ),
            "http",
        ),
        // The address changed since the dialog read it.
        (
            copy(
                user_id("gw"),
                crate::adjust::Adjust {
                    base: Some(rebase("https://old.acme.com", "https://new.acme.com")),
                    ..Default::default()
                },
            ),
            "changed since",
        ),
        // A program has no address, and nothing signs in without OAuth.
        (
            copy(
                user_id("st"),
                crate::adjust::Adjust {
                    base: Some(rebase("a", "https://x")),
                    ..Default::default()
                },
            ),
            "no address",
        ),
        (
            copy(
                user_id("gw"),
                crate::adjust::Adjust {
                    callback_port: Some(Some(9001)),
                    ..Default::default()
                },
            ),
            "does not sign in",
        ),
    ] {
        let refused = plan_failure(&core, vec![change]);
        assert!(refused.contains(said), "{said}: {refused}");
    }
    assert_eq!(config_json(&env), before);
}

/// Copies sso (signing in, with Klefki's copy of its client secret) for the user as `sso_eu`, adjusted; Claude
/// Code's add works. Returns the add's argv, the hash of the secret it was handed, and the core.
fn copy_sso(
    adjust: crate::adjust::Adjust,
) -> (Env, Arc<crate::secrets::MemoryStore>, PlanView, Core) {
    let (env, store, _, _) = oauth_move_env(
        r#"{"rules":[{"args":["mcp","add-json"],"delay_ms":800,"stdout_file":"added.txt"}]}"#,
    );
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Duplicate {
            id: user_id("sso"),
            name: "sso_eu".into(),
            targets: vec![Target::User],
            adjust,
        }],
    );
    (env, store, plan, core)
}

/// Applies `plan`, playing Claude Code's part: it writes what it was asked to add.
fn apply_copy(env: &Env, core: &Core, plan: &PlanView) -> serde_json::Value {
    core.send(Command::Apply(plan.id));
    env.wait_for_run(1);
    let run = &env.changes_run()[0];
    let json: serde_json::Value = serde_json::from_str(run.0.last().unwrap()).unwrap();
    let mut config = config_json(env);
    config["mcpServers"]["sso_eu"] = json.clone();
    write_config(env, &config);
    let events = collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. })),
        "{events:#?}"
    );
    json
}

#[test]
fn a_copy_that_signs_in_gets_its_new_address_and_port_and_keeps_the_client_secret() {
    let (env, store, plan, core) = copy_sso(crate::adjust::Adjust {
        base: Some(rebase(
            "https://sso.example.com/mcp",
            "https://eu.example.com/mcp",
        )),
        callback_port: Some(Some(9123)),
        ..Default::default()
    });
    let shown = format!("{plan:?}");
    assert!(shown.contains("https://eu.example.com/mcp"), "{shown}");
    assert!(shown.contains("9123"), "{shown}");
    // Klefki's copy of the original's client secret goes to the new host.
    assert!(
        plan.warnings
            .iter()
            .any(|w| w == "sso_eu sends the original's client secret to eu.example.com. Check the address before you go on."),
        "{:?}",
        plan.warnings
    );
    let added = apply_copy(&env, &core, &plan);
    assert_eq!(
        added,
        serde_json::json!({"type": "http", "url": "https://eu.example.com/mcp", "oauth": {"clientId": "client-7", "callbackPort": 9123}})
    );
    let runs = env.changes_run();
    assert_eq!(runs.len(), 1, "only an add: {runs:?}");
    assert_eq!(runs[0].2.as_deref(), Some(sha256(CLIENT_SECRET).as_str()));
    // The original is as it was; Klefki keeps the same secret for the copy.
    let config = config_json(&env);
    assert_eq!(
        config["mcpServers"]["sso"],
        serde_json::from_str::<serde_json::Value>(SSO_ENTRY).unwrap()
    );
    let values = store.values.lock().unwrap().clone();
    let copy = crate::model::ConnectionId::new(Scope::User, None, "sso_eu");
    assert_eq!(
        values.get(&crate::secrets::user_for(&copy)),
        Some(&CLIENT_SECRET.to_owned())
    );
}

#[test]
fn a_copy_that_signs_in_as_another_client_hands_over_that_clients_secret_only() {
    const OTHER: &str = "Other-Client-S3cret";
    let (env, store, plan, core) = copy_sso(crate::adjust::Adjust {
        base: Some(rebase(
            "https://sso.example.com/mcp",
            "https://eu.example.com/mcp",
        )),
        client: Some(crate::adjust::Client {
            id: Some("client-8".into()),
            secret: Some(crate::secrets::Secret::new(OTHER.to_owned())),
        }),
        ..Default::default()
    });
    let shown = format!("{plan:?}");
    assert!(shown.contains("client-8"), "{shown}");
    // Another client brings its own secret: nothing of the original's goes anywhere new.
    assert!(
        !plan
            .warnings
            .iter()
            .any(|w| w.contains("sends the original's")),
        "{:?}",
        plan.warnings
    );
    assert!(
        !shown.contains(OTHER) && !shown.contains(CLIENT_SECRET),
        "{shown}"
    );
    let added = apply_copy(&env, &core, &plan);
    assert_eq!(added["oauth"]["clientId"], "client-8");
    let runs = env.changes_run();
    assert_eq!(runs[0].2.as_deref(), Some(sha256(OTHER).as_str()));
    let log = std::fs::read_to_string(&env.log).unwrap();
    assert!(!log.contains(OTHER), "{log}");
    let values = store.values.lock().unwrap().clone();
    let copy = crate::model::ConnectionId::new(Scope::User, None, "sso_eu");
    let original = crate::model::ConnectionId::new(Scope::User, None, "sso");
    assert_eq!(
        values.get(&crate::secrets::user_for(&copy)),
        Some(&OTHER.to_owned())
    );
    assert_eq!(
        values.get(&crate::secrets::user_for(&original)),
        Some(&CLIENT_SECRET.to_owned())
    );
}

#[test]
fn a_copy_that_signs_in_as_a_client_without_a_secret_gets_none() {
    // The variant: no client id at all, so Claude Code registers itself; and no secret to hand over.
    let (env, store, plan, core) = copy_sso(crate::adjust::Adjust {
        callback_port: Some(None),
        client: Some(crate::adjust::Client {
            id: None,
            secret: None,
        }),
        ..Default::default()
    });
    // Not the note about Klefki lacking the original's secret: the copy is meant to have none.
    assert!(
        !plan.notes.iter().any(|n| n.contains("no copy")),
        "{:?}",
        plan.notes
    );
    let added = apply_copy(&env, &core, &plan);
    assert_eq!(added["oauth"], serde_json::json!({}));
    let runs = env.changes_run();
    assert_eq!(runs[0].2, None, "no secret: {runs:?}");
    assert!(
        !runs[0].0.iter().any(|a| a == "--client-secret"),
        "{runs:?}"
    );
    let values = store.values.lock().unwrap().clone();
    let copy = crate::model::ConnectionId::new(Scope::User, None, "sso_eu");
    assert_eq!(values.get(&crate::secrets::user_for(&copy)), None);
}

#[test]
fn a_copy_whose_new_callback_port_is_taken_is_warned_about() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let (_env, _store, plan, _core) = copy_sso(crate::adjust::Adjust {
        callback_port: Some(Some(port)),
        ..Default::default()
    });
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.starts_with("sso_eu: ") && w.contains(&port.to_string())),
        "{:?}",
        plan.warnings
    );
}

#[test]
fn a_move_whose_remove_leaves_the_server_there_changes_nothing() {
    let (env, store, key, _) =
        oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"stdout_file":"removed.txt"}]}"#);
    let before = config_json(&env);
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    let events = apply_and_wait(&core, &plan);
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:#?}"));
    assert!(error.contains("still there"), "{error}");
    assert!(error.contains("Nothing was changed"), "{error}");
    assert_eq!(config_json(&env), before);
    let runs = env.changes_run();
    assert_eq!(runs.len(), 1, "no add after a failed remove: {runs:?}");
    assert_eq!(store.values.lock().unwrap().len(), 1);
}

#[test]
fn a_move_whose_remove_hangs_is_stopped_and_changes_nothing() {
    let (env, store, key, _) =
        oauth_move_env(r#"{"rules":[{"args":["mcp","remove"],"hang":true}]}"#);
    let before = config_json(&env);
    let mut config = env.config();
    config.secrets = Some(Arc::clone(&store) as Arc<dyn crate::secrets::SecretStore>);
    config.check_timeout = Duration::from_millis(600);
    let core = start_with(config, || {});
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("sso"),
            to: Target::Local(key),
        }],
    );
    let events = apply_and_wait(&core, &plan);
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:#?}"));
    assert!(error.contains("in time"), "{error}");
    assert_eq!(config_json(&env), before);
    assert_eq!(env.changes_run().len(), 1);
}

#[test]
fn an_oauth_server_is_added_by_claude_code_with_its_secret_only_in_the_environment() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
    );
    assert_eq!(plan.summary, ["Add sso2 for the user"]);
    assert_eq!(plan.action, "Add");
    let review = &plan.reviews[0];
    for line in [
        "Connects to: https://sso.example.com/mcp",
        "Signs in with OAuth as client client-7, callback port 8123",
    ] {
        assert!(review.lines.iter().any(|l| l == line), "{line}: {review:?}");
    }
    assert!(
        plan.notes.iter().any(|n| n.contains("Claude Code adds it")),
        "{:?}",
        plan.notes
    );
    assert!(
        plan.files
            .iter()
            .any(|f| f.lines.iter().any(|l| l.contains("sso2"))),
        "{:?}",
        plan.files
    );
    assert!(!format!("{plan:?}").contains(CLIENT_SECRET));
    // Nothing has run yet.
    assert!(env.add_json_runs().is_empty());

    core.send(Command::Apply(plan.id));
    env.claude_writes_when_asked(
        None,
        "sso2",
        r#"{"type": "http", "url": "https://sso.example.com/mcp", "headers": {"X-Team": "${TEAM}"}, "oauth": {"clientId": "client-7", "callbackPort": 8123}}"#,
    );
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
    let runs = env.add_json_runs();
    assert_eq!(runs.len(), 1);
    let argv: Vec<&str> = runs[0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        argv[..7],
        [
            "mcp",
            "add-json",
            "-s",
            "user",
            "--client-secret",
            "--",
            "sso2"
        ]
    );
    assert_eq!(argv.len(), 8);
    let sent: serde_json::Value = serde_json::from_str(argv[7]).unwrap();
    assert_eq!(
        sent,
        serde_json::from_str::<serde_json::Value>(OAUTH_DEF).unwrap()
    );
    // The secret travels only in MCP_CLIENT_SECRET.
    assert_eq!(runs[0]["secret_set"], true);
    assert_eq!(runs[0]["secret_sha256"], sha256(CLIENT_SECRET));
    let log = std::fs::read_to_string(&env.log).unwrap();
    assert!(!log.contains(CLIENT_SECRET), "{log}");
    // Klefki keeps its own copy under the new server's connection, and nowhere else.
    let id = crate::model::ConnectionId::new(Scope::User, None, "sso2");
    assert_eq!(
        store
            .values
            .lock()
            .unwrap()
            .get(&crate::secrets::user_for(&id)),
        Some(&CLIENT_SECRET.to_owned())
    );
    assert!(!walk(env.dir.path()).contains(CLIENT_SECRET));
    let rows = &snapshots(&events).last().unwrap().servers;
    assert!(rows.iter().any(|r| r.name == "sso2" && r.oauth.is_some()));
}

#[test]
fn an_oauth_server_for_a_project_is_added_from_that_project_folder() {
    let (env, key, repo) = oauth_add_env(ADD_JSON_OK);
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![oauth_add(
            "sso2",
            vec![Target::Local(key.clone())],
            Some(CLIENT_SECRET),
        )],
    );
    core.send(Command::Apply(plan.id));
    env.claude_writes_when_asked(Some(&key), "sso2", OAUTH_DEF);
    let events = collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    assert!(
        events.iter().any(|e| matches!(e, Event::Applied { .. })),
        "{events:#?}"
    );
    let run = &env.add_json_runs()[0];
    assert_eq!(run["argv"][2], "-s");
    assert_eq!(run["argv"][3], "local");
    // Claude Code adds a local server to the project it runs in.
    assert_eq!(
        std::fs::canonicalize(run["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&repo).unwrap()
    );
    let id = crate::model::ConnectionId::new(Scope::Local, Some(&key), "sso2");
    assert!(
        store
            .values
            .lock()
            .unwrap()
            .contains_key(&crate::secrets::user_for(&id))
    );
}

#[test]
fn an_oauth_server_without_a_client_secret_is_added_without_one() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(&core, vec![oauth_add("sso2", vec![Target::User], None)]);
    core.send(Command::Apply(plan.id));
    env.claude_writes_when_asked(None, "sso2", OAUTH_DEF);
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    let run = &env.add_json_runs()[0];
    assert_eq!(run["secret_set"], false);
    assert!(
        !run["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--client-secret")
    );
    assert!(store.values.lock().unwrap().is_empty());
}

#[test]
fn the_engine_answers_whether_a_callback_port_is_held_for_that_port() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let core = with_store(&env, Arc::new(crate::secrets::MemoryStore::default()));
    collect(&core, |e| !snapshots(e).is_empty());
    let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let answer = |core: &Core| {
        core.send(Command::CheckCallbackPort(port));
        collect(core, |e| {
            e.iter().any(|x| matches!(x, Event::CallbackPort { .. }))
        })
        .into_iter()
        .find_map(|e| match e {
            Event::CallbackPort { port, warning } => Some((port, warning)),
            _ => None,
        })
        .unwrap()
    };
    let (answered, warning) = answer(&core);
    assert_eq!(answered, port);
    assert!(
        warning
            .as_deref()
            .is_some_and(|w| w.starts_with(&format!("Port {port} is in use"))),
        "{warning:?}"
    );
    drop(held);
    assert_eq!(
        once_let_go(|| answer(&core), |(_, warning)| warning.is_none()),
        (port, None)
    );
}

/// What `ask` answers about a port once the test let it go. Until then another holder can have the same number
/// for a moment: a child process another test starts keeps a copy of the listener until it runs its program, and
/// a listener on 127.0.0.1 leaves `::1` free for another test to take. So it asks again, up to `WAIT`.
fn once_let_go<T>(mut ask: impl FnMut() -> T, free: impl Fn(&T) -> bool) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = ask();
        if free(&answer) || Instant::now() >= deadline {
            return answer;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_callback_port_another_program_holds_is_warned_about_in_the_review_and_the_add_goes_on() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let core = with_store(&env, Arc::new(crate::secrets::MemoryStore::default()));
    collect(&core, |e| !snapshots(e).is_empty());
    let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let add = |json: String| Change::Add {
        name: "sso2".to_owned(),
        json,
        targets: vec![Target::User],
        secret: None,
    };
    let with_port = format!(
        r#"{{"type": "http", "url": "https://sso.example.com/mcp", "oauth": {{"clientId": "c", "callbackPort": {port}}}}}"#
    );
    let said = |plan: &PlanView| {
        plan.warnings
            .iter()
            .filter(|w| w.contains(&format!("Port {port} ")))
            .cloned()
            .collect::<Vec<_>>()
    };
    let plan = plan_for(&core, vec![add(with_port.clone())]);
    assert_eq!(plan.action, "Add");
    assert_eq!(plan.summary, ["Add sso2 for the user"]);
    let warned = said(&plan);
    assert_eq!(warned.len(), 1, "{:?}", plan.warnings);
    assert!(
        warned[0].starts_with(&format!("sso2: Port {port} is in use by another program.")),
        "{warned:?}"
    );
    // A port let go is not warned about, and neither is a server that leaves the port to Claude Code.
    drop(held);
    assert_eq!(
        once_let_go(
            || said(&plan_for(&core, vec![add(with_port.clone())])),
            Vec::is_empty
        ),
        Vec::<String>::new()
    );
    let plan = plan_for(
        &core,
        vec![add(
            r#"{"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "c"}}"#
                .to_owned(),
        )],
    );
    assert!(
        !plan.warnings.iter().any(|w| w.contains("signing in")),
        "{:?}",
        plan.warnings
    );
}

#[test]
fn when_claude_code_refuses_the_add_its_reason_is_shown_and_no_secret_is_kept() {
    let (env, _, _) = oauth_add_env(
        r#"{"rules":[{"args":["mcp","add-json"],"exit_code":1,"stderr_file":"exists.txt"}]}"#,
    );
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
    );
    let events = apply_and_wait(&core, &plan);
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .expect("a failure");
    assert!(error.contains("already exists"), "{error}");
    assert!(error.contains("not added"), "{error}");
    assert!(store.values.lock().unwrap().is_empty());
}

#[test]
fn claude_code_saying_it_added_a_server_that_is_not_there_is_reported() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
    );
    let events = apply_and_wait(&core, &plan);
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::ApplyFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .expect("a failure");
    assert!(error.contains("cannot find it"), "{error}");
    assert!(store.values.lock().unwrap().is_empty());
}

#[test]
fn without_a_secret_store_an_oauth_server_with_a_secret_is_not_added() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let store = Arc::new(crate::secrets::MemoryStore::default());
    *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable(
        "no Secret Service".into(),
    ));
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let refused = plan_failure(
        &core,
        vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
    );
    // First what happened and what to do, in plain words; what the store said comes after, on its own line.
    let (plain, detail) = refused.split_once('\n').expect("a detail line");
    assert!(
        plain.starts_with("Klefki can't use your system's secret store."),
        "{refused}"
    );
    assert!(plain.contains("press Review again"), "{refused}");
    assert!(plain.contains("leave the client secret empty"), "{refused}");
    assert_eq!(
        plain.contains("GNOME Keyring"),
        cfg!(target_os = "linux"),
        "{refused}"
    );
    assert!(!plain.contains("Secret Service"), "{refused}");
    assert!(detail.contains("no Secret Service"), "{refused}");
    assert!(detail.contains("no plaintext copy"), "{refused}");
    assert!(env.add_json_runs().is_empty());
    // A store that is there but refuses says the same next step.
    *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Failed("denied".into()));
    let refused = plan_failure(
        &core,
        vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
    );
    let (plain, detail) = refused.split_once('\n').expect("a detail line");
    assert!(plain.contains("leave the client secret empty"), "{refused}");
    assert!(detail.contains("denied"), "{refused}");
    assert!(env.add_json_runs().is_empty());
}

#[test]
fn an_oauth_add_that_cannot_be_made_is_refused_before_anything_runs() {
    let (env, key, _) = oauth_add_env(ADD_JSON_OK);
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    let gone = env
        .dir
        .path()
        .join("gone")
        .to_string_lossy()
        .replace('\\', "/");
    let mut config = config_json(&env);
    config["projects"][&gone] = serde_json::json!({});
    write_config(&env, &config);
    collect(&core, |e| {
        snapshots(e).iter().any(|s| s.projects.len() == 2)
    });
    let plain = || Change::Add {
        name: "p".into(),
        json: r#"{"type": "http", "url": "https://p.example"}"#.into(),
        targets: vec![Target::User],
        secret: Some(crate::secrets::Secret::new("x".into())),
    };
    for (change, why) in [
        (plain(), "only a server that signs in with OAuth"),
        (oauth_add("sso2", vec![Target::User], Some("")), "empty"),
        (oauth_add("sso2", vec![Target::Local(gone.clone())], None), "folder"),
        (
            Change::Add {
                name: "s".into(),
                json: r#"{"type": "http", "url": "https://s.example", "oauth": {"clientSecret": "inline-s3cret"}}"#.into(),
                targets: vec![Target::User],
                secret: None,
            },
            "handed to Claude Code on its own",
        ),
        // Claude Code's command takes the definition in its arguments, which other programs can read.
        (
            Change::Add {
                name: "h".into(),
                json: r#"{"type": "http", "url": "https://h.example", "headers": {"Authorization": "Bearer ghp_inlineS3cretValue123"}, "oauth": {}}"#.into(),
                targets: vec![Target::User],
                secret: None,
            },
            "Bearer ${AUTH_TOKEN}",
        ),
        (
            Change::Add {
                name: "e".into(),
                json: r#"{"type": "sse", "url": "https://e.example", "env": {"API_TOKEN": "inline-s3cret-42"}, "oauth": {"clientId": "c"}}"#.into(),
                targets: vec![Target::User],
                secret: None,
            },
            "${API_TOKEN}",
        ),
    ] {
        let refused = plan_failure(&core, vec![change.clone()]);
        assert!(refused.contains(why), "{change:?}: {refused}");
        assert!(!refused.contains("inline-s3cret"), "{refused}");
        assert!(!refused.contains("inlineS3cret"), "{refused}");
    }
    let _ = key;
    assert!(env.add_json_runs().is_empty());
    // A reference is what goes instead, and an ordinary value is no secret.
    plan_for(
        &core,
        vec![Change::Add {
            name: "h".into(),
            json: r#"{"type": "http", "url": "https://h.example", "headers": {"Authorization": "Bearer ${GITHUB_TOKEN}", "X-Team": "engineering"}, "oauth": {}}"#.into(),
            targets: vec![Target::User],
            secret: None,
        }],
    );
}

#[test]
fn an_oauth_server_is_not_added_without_claude_codes_command() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let mut config = env.config();
    config.claude = Some(
        env.dir
            .path()
            .join(if cfg!(windows) { "nope.exe" } else { "nope" }),
    );
    let core = start_with(config, || {});
    collect(&core, |e| !snapshots(e).is_empty());
    let refused = plan_failure(&core, vec![oauth_add("sso2", vec![Target::User], None)]);
    assert!(refused.contains("Claude Code's command"), "{refused}");
}

#[test]
fn a_name_taken_after_review_stops_the_oauth_add_before_it_runs() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(&core, vec![oauth_add("sso2", vec![Target::User], None)]);
    let mut config = config_json(&env);
    config["mcpServers"]["sso2"] = serde_json::json!({"command": "someone-else"});
    write_config(&env, &config);
    let events = apply_and_wait(&core, &plan);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ApplyFailed { .. })),
        "{events:#?}"
    );
    assert!(env.add_json_runs().is_empty());
    assert_eq!(
        config_json(&env)["mcpServers"]["sso2"]["command"],
        "someone-else"
    );
}

// Signing in (decision 005).
