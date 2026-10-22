//! Engine tests: reading the configuration again only when needed, and the file watcher.
use super::*;

const CHECKABLE: &str = r#"{"rules":[
    {"args":["--version"],"stdout_file":"version.txt"},
    {"args":["mcp","get"],"stdout_file":"connected.txt"}]}"#;

fn checkable_env(claude_json: &serde_json::Value) -> Env {
    Env::new(
        claude_json,
        CHECKABLE,
        &[
            ("version.txt", b"2.1.302 (Claude Code)\n"),
            ("connected.txt", CONNECTED),
        ],
    )
}

#[test]
fn the_cli_version_is_asked_once_not_on_every_refresh() {
    let env = checkable_env(&two_projects("end"));
    let core = env.start();
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::CliVersion(Some(_)))) && final_status(e, "gw").is_some()
    });
    for _ in 0..3 {
        core.send(Command::Refresh { live_all: false });
        collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::CliVersion(Some(_))))
        });
    }
    assert_eq!(env.runs(&["--version"]), 1);
}

#[test]
fn a_failed_version_ask_is_tried_again_on_the_next_refresh() {
    // A first run can fail for reasons that pass (a virus scan on first start, a timeout); it is not an answer.
    let env = Env::new(
        &two_projects("end"),
        r#"{"rules":[{"args":["--version"],"exit_code":1}]}"#,
        &[],
    );
    let core = env.start();
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::CliVersion(None)))
    });
    core.send(Command::Refresh { live_all: false });
    collect(&core, |_| env.runs(&["--version"]) == 2);
}

/// `gw` (user) and `loc` (local, in a project folder that exists), both checkable. Returns the project key.
fn checkable_with_project(loc_off: bool) -> (Env, String) {
    let env = checkable_env(&serde_json::json!({}));
    let project = env.dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let key = project.to_string_lossy().replace('\\', "/");
    let disabled: Vec<&str> = if loc_off { vec!["loc"] } else { vec![] };
    write_config(
        &env,
        &serde_json::json!({
            "mcpServers": {"gw": {"type": "stdio", "command": "node"}},
            "projects": {key.clone(): {
                "mcpServers": {"loc": {"type": "stdio", "command": "x"}},
                "disabledMcpServers": disabled
            }}
        }),
    );
    let key = inventory::normalise_project(&key);
    (env, key)
}

#[test]
fn saving_a_change_neither_re_checks_servers_nor_asks_the_version_again() {
    let (env, key) = checkable_with_project(false);
    let core = env.start();
    collect(&core, |e| {
        final_status(e, "gw") == Some(Status::Connected)
            && final_status(e, "loc") == Some(Status::Connected)
    });
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", &key),
            enabled: false,
        }],
    );
    core.send(Command::Apply(plan.id));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    // Give a stray check time to start, if one were queued.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(env.runs(&["mcp", "get", "gw"]), 1, "gw did not change");
    assert_eq!(env.runs(&["mcp", "get", "loc"]), 1, "loc was switched off");
    assert_eq!(env.runs(&["--version"]), 1);
}

#[test]
fn a_server_switched_on_by_a_change_is_checked() {
    let (env, key) = checkable_with_project(true);
    let core = env.start();
    collect(&core, |e| final_status(e, "gw") == Some(Status::Connected));
    assert_eq!(
        env.runs(&["mcp", "get", "loc"]),
        0,
        "off servers are not checked"
    );
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", &key),
            enabled: true,
        }],
    );
    core.send(Command::Apply(plan.id));
    collect(&core, |e| final_status(e, "loc") == Some(Status::Connected));
    assert_eq!(env.runs(&["mcp", "get", "loc"]), 1);
    assert_eq!(env.runs(&["mcp", "get", "gw"]), 1);
    assert_eq!(env.runs(&["--version"]), 1);
}

#[test]
fn a_server_added_from_outside_appears_without_a_refresh() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    let mut changed = two_projects("end");
    changed["mcpServers"]["added"] =
        serde_json::json!({"type": "http", "url": "https://x.example/mcp"});
    write_config(&env, &changed);
    collect(&core, |e| {
        snapshots(e)
            .iter()
            .any(|s| s.servers.iter().any(|r| r.name == "added"))
    });
}

#[test]
fn a_new_mcp_json_in_a_known_project_appears_without_a_refresh() {
    let env = quiet_env(&serde_json::json!({}));
    let repo = env.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let key = repo.to_string_lossy().replace('\\', "/");
    write_config(&env, &serde_json::json!({"projects": {key: {}}}));
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    std::fs::write(repo.join(".mcp.json"), MCP_JSON).unwrap();
    collect(&core, |e| {
        snapshots(e).iter().any(|s| {
            s.servers
                .iter()
                .any(|r| r.name == "ps" && r.scope == Scope::Project)
        })
    });
}

#[test]
fn klefki_s_own_save_is_read_once_not_again_by_the_watcher() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        }],
    );
    core.send(Command::Apply(plan.id));
    let mut events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Applied { .. }))
    });
    // Several watch intervals and the settle time pass.
    std::thread::sleep(Duration::from_millis(600));
    while let Some(e) = core.try_recv() {
        events.push(e);
    }
    assert_eq!(snapshots(&events).len(), 1, "one reload after the save");
}

#[test]
fn nothing_is_read_again_while_nothing_changes() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    std::thread::sleep(Duration::from_millis(600));
    let mut later = Vec::new();
    while let Some(e) = core.try_recv() {
        later.push(e);
    }
    assert!(snapshots(&later).is_empty(), "{later:#?}");
}

#[test]
fn with_no_configuration_found_the_snapshot_says_so() {
    let env = quiet_env(&serde_json::json!({}));
    let core = start_with(
        EngineConfig {
            paths: None,
            ..env.config()
        },
        || {},
    );
    let events = collect(&core, |e| !snapshots(e).is_empty());
    assert_eq!(
        snapshots(&events)[0].warnings,
        ["Could not determine where Claude Code keeps its configuration."]
    );
}

#[test]
fn the_snapshot_says_what_keeps_a_server_from_starting() {
    // The engine looks programs up on its own PATH and `${VAR}` references in its own variables, never the test's.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "gone": {"command": "mcp-server-nowhere"},
            "token": {"command": "here", "args": ["${KLEFKI_TEST_TOKEN}"]},
            "fine": {"command": "here", "env": {"T": "${KLEFKI_TEST_SET}"}}
        }}),
        r#"{"rules":[]}"#,
        &[],
    );
    let bin = env.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["here", "here.exe"] {
        std::fs::write(bin.join(name), b"stub").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
    }
    let mut config = env.config();
    config.claude = None;
    config.path_var = Some(bin.into());
    config.vars = std::sync::Arc::new(|name: &str| (name == "KLEFKI_TEST_SET").then(|| "x".into()));
    let core = start_with(config, || {});
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let Some(Event::Snapshot(s)) = events.iter().find(|e| matches!(e, Event::Snapshot(_))) else {
        unreachable!()
    };
    use crate::api::PreflightKind;
    let kinds = |name: &str| -> Vec<PreflightKind> {
        let row = s.servers.iter().find(|r| r.name == name).unwrap();
        row.preflight.iter().map(|f| f.kind).collect()
    };
    assert_eq!(kinds("gone"), [PreflightKind::NotFound]);
    assert_eq!(kinds("token"), [PreflightKind::UnsetVariable]);
    assert_eq!(kinds("fine"), []);
}
