//! Engine tests: showing, deleting, duplicating, adding and migrating servers, and plans in plain words.
use super::*;

#[test]
fn a_definition_is_shown_with_secrets_hidden() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let (json, editable) = entry_of(&core, user_id("gw"));
    let json = json.unwrap();
    assert!(editable);
    assert!(
        json.contains("<hidden>") && !json.contains("s3cret-value"),
        "{json}"
    );
    assert!(json.contains("\"command\": \"node\""), "{json}");
    let (gone, _) = entry_of(&core, user_id("ghost"));
    assert!(gone.is_err());
}

#[test]
fn deleting_a_user_server_removes_it_and_only_it() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(&core, vec![Change::Delete { id: user_id("gw") }]);
    assert!(
        plan.files[0].lines.contains(&"- gw".to_owned()),
        "{:?}",
        plan.files
    );
    assert!(plan.warnings.iter().any(|w| w.contains("sign-in")));
    let events = apply_and_wait(&core, &plan);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. }))
    );
    let now = config_json(&env);
    assert_eq!(now["mcpServers"], serde_json::json!({}));
    assert_eq!(now["projects"], two_projects("end")["projects"]);
    assert!(
        snapshots(&events)
            .last()
            .unwrap()
            .servers
            .iter()
            .all(|r| r.name != "gw")
    );
    verify_passes(&core);
}

#[test]
fn duplicating_a_user_server_into_projects_copies_it_secrets_and_all() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![duplicate(
            user_id("gw"),
            "gw_copy",
            vec![
                Target::Local("/work/a".into()),
                Target::Local("/work/b".into()),
            ],
        )],
    );
    assert_eq!(plan.summary, ["Duplicate gw as gw_copy in a and b"]);
    assert_eq!(plan.action, "Duplicate");
    assert!(!plan.destructive);
    let shown = format!("{plan:?}");
    assert!(!shown.contains("s3cret-value"), "{shown}");
    assert!(shown.contains("+ gw_copy"), "{shown}");
    let events = apply_and_wait(&core, &plan);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. }))
    );
    let now = config_json(&env);
    let gw = two_projects("end")["mcpServers"]["gw"].clone();
    assert_eq!(now["projects"]["/work/a"]["mcpServers"]["gw_copy"], gw);
    assert_eq!(now["projects"]["/work/b"]["mcpServers"]["gw_copy"], gw);
    assert_eq!(now["mcpServers"], two_projects("end")["mcpServers"]);
    let rows = &snapshots(&events).last().unwrap().servers;
    assert_eq!(rows.iter().filter(|r| r.name == "gw_copy").count(), 2);
    verify_passes(&core);
}

#[test]
fn duplicating_a_local_server_to_the_user_and_its_own_project() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![duplicate(
            local_id("loc", "/work/a"),
            "loc2",
            vec![Target::User, Target::Local("/work/a".into())],
        )],
    );
    assert_eq!(
        plan.summary,
        ["Duplicate loc as loc2 for the user and in a"]
    );
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    let loc = serde_json::json!({"type": "stdio", "command": "x"});
    assert_eq!(now["mcpServers"]["loc2"], loc);
    assert_eq!(now["projects"]["/work/a"]["mcpServers"]["loc2"], loc);
    assert_eq!(now["projects"]["/work/a"]["mcpServers"]["loc"], loc);
}

#[test]
fn moving_a_server_does_not_warn_about_itself() {
    // Found trying Move in the real app: moving plain from the user to a project warned that in that project
    // it "is used instead of the user server plain", the very server being moved.
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("gw"),
            to: Target::Local("/work/b".into()),
        }],
    );
    assert_eq!(plan.summary, ["Move gw to b"]);
    assert!(
        !plan.warnings.iter().any(|w| w.contains("used instead")),
        "{:?}",
        plan.warnings
    );
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    let gw = two_projects("end")["mcpServers"]["gw"].clone();
    assert_eq!(now["projects"]["/work/b"]["mcpServers"]["gw"], gw);
    assert!(now["mcpServers"].get("gw").is_none(), "{now}");
    assert_eq!(now["tail"], "end");
    verify_passes(&core);
    // The nearby variant: from a project to the user.
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: local_id("loc", "/work/a"),
            to: Target::User,
        }],
    );
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert!(
        plan.notes.iter().any(|n| n.contains("on in every project")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn moving_still_warns_about_another_server_of_that_name() {
    let mut config = two_projects("end");
    config["projects"]["/work/b"]["mcpServers"] =
        serde_json::json!({"loc": {"type": "stdio", "command": "y"}});
    let env = quiet_env(&config);
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: local_id("loc", "/work/a"),
            to: Target::User,
        }],
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("In b") && w.contains("local")),
        "{:?}",
        plan.warnings
    );
}

#[test]
fn a_copy_under_a_name_used_elsewhere_warns_which_one_claude_uses() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    // A user server called loc: in /work/a the local loc wins.
    let plan = plan_for(
        &core,
        vec![duplicate(
            local_id("loc", "/work/a"),
            "loc",
            vec![Target::User],
        )],
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("local") && w.contains("a")),
        "{:?}",
        plan.warnings
    );
}

const PASTED_FS: &str = r#"{"type": "stdio", "command": "npx", "args": ["-y", "@x/fs", "/tmp"], "env": {"API_KEY": "pasted-secret", "HOME_DIR": "${HOME}", "MIXED": "sk-live-123${SUFFIX}", "DEFAULTED": "${TOK:-ghp_realsecret}"}}"#;

#[test]
fn adding_a_server_writes_it_where_asked_and_shows_everything_it_runs() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![add(
            "fs",
            PASTED_FS,
            vec![Target::User, Target::Local("/work/b".into())],
        )],
    );
    assert_eq!(plan.summary, ["Add fs for the user and in b"]);
    assert_eq!(plan.action, "Add");
    assert!(!plan.destructive);
    let review = &plan.reviews[0];
    assert!(review.title.contains("fs"), "{review:?}");
    for line in [
        "Command: npx",
        "Argument 1: -y",
        "Argument 2: @x/fs",
        "Argument 3: /tmp",
    ] {
        assert!(review.lines.iter().any(|l| l == line), "{line}: {review:?}");
    }
    // The variables it reads are named; what was typed in is not shown anywhere, even next to a reference.
    for name in ["HOME", "SUFFIX", "TOK"] {
        assert!(
            review
                .lines
                .iter()
                .any(|l| l.contains(&format!("variable {name}"))),
            "{name}: {review:?}"
        );
    }
    assert!(
        review
            .lines
            .iter()
            .any(|l| l == "Environment: HOME_DIR=${HOME}"),
        "{review:?}"
    );
    let shown = format!("{plan:?}");
    for secret in ["pasted-secret", "sk-live-123", "ghp_realsecret"] {
        assert!(!shown.contains(secret), "{secret}: {shown}");
    }
    assert!(
        plan.notes.iter().any(|n| n.contains("starts its command")),
        "{:?}",
        plan.notes
    );
    let events = apply_and_wait(&core, &plan);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. }))
    );
    let now = config_json(&env);
    let written: serde_json::Value = serde_json::from_str(PASTED_FS).unwrap();
    assert_eq!(now["mcpServers"]["fs"], written);
    assert_eq!(now["projects"]["/work/b"]["mcpServers"]["fs"], written);
    // In the order pasted.
    let text = std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
    let at = |s: &str| text.find(s).unwrap();
    assert!(at("\"npx\"") < at("\"args\"") && at("\"args\"") < at("\"API_KEY\""));
    // Nothing else moved.
    assert_eq!(
        now["mcpServers"]["gw"],
        two_projects("end")["mcpServers"]["gw"]
    );
    assert_eq!(
        now["projects"]["/work/a"],
        two_projects("end")["projects"]["/work/a"]
    );
    let rows = &snapshots(&events).last().unwrap().servers;
    assert_eq!(rows.iter().filter(|r| r.name == "fs").count(), 2);
    verify_passes(&core);
}

#[test]
fn several_servers_can_be_added_to_one_place_in_one_plan() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![
            add(
                "one",
                r#"{"command": "a"}"#,
                vec![Target::Local("/work/a".into())],
            ),
            add(
                "two",
                r#"{"type": "http", "url": "https://two.example/mcp", "headers": {"Authorization": "Bearer pasted-token"}}"#,
                vec![Target::Local("/work/a".into())],
            ),
        ],
    );
    assert_eq!(plan.summary, ["Add one in a", "Add two in a"]);
    assert_eq!(plan.reviews.len(), 2);
    assert!(
        plan.reviews[1]
            .lines
            .iter()
            .any(|l| l == "Connects to: https://two.example/mcp"),
        "{:?}",
        plan.reviews
    );
    assert!(!format!("{plan:?}").contains("pasted-token"));
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    let servers = &now["projects"]["/work/a"]["mcpServers"];
    assert_eq!(servers["one"], serde_json::json!({"command": "a"}));
    assert_eq!(
        servers["two"]["headers"]["Authorization"],
        "Bearer pasted-token"
    );
    assert_eq!(
        servers["loc"],
        two_projects("end")["projects"]["/work/a"]["mcpServers"]["loc"]
    );
    verify_passes(&core);
}

#[test]
fn what_cannot_be_added_is_refused_saying_why() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let a = || vec![Target::Local("/work/a".into())];
    for (changes, why) in [
        (vec![add("bad name", r#"{"command": "x"}"#, a())], "name"),
        (vec![add("loc", r#"{"command": "x"}"#, a())], "already"),
        (
            vec![add("gw", r#"{"command": "x"}"#, vec![Target::User])],
            "already",
        ),
        (
            vec![
                add("twice", r#"{"command": "x"}"#, a()),
                add("twice", r#"{"command": "y"}"#, a()),
            ],
            "twice",
        ),
        (
            vec![add(
                "n",
                r#"{"command": "x"}"#,
                vec![Target::Local("/elsewhere".into())],
            )],
            "not a project",
        ),
        (
            vec![add("n", r#"{"command": "x"}"#, vec![])],
            "Choose where",
        ),
        (vec![add("n", "{not json", a())], "JSON"),
        // OAuth is for remote servers: Claude Code would drop it here without a word.
        (
            vec![add(
                "n",
                r#"{"command": "x", "oauth": {"clientId": "c"}}"#,
                a(),
            )],
            "OAuth",
        ),
        (
            vec![add(
                "n",
                r#"{"type": "http", "url": "javascript:alert(1)"}"#,
                a(),
            )],
            "url",
        ),
        (vec![add("n", r#"{"command": "x\u202ey"}"#, a())], "safely"),
    ] {
        let refused = plan_failure(&core, changes.clone());
        assert!(refused.contains(why), "{changes:?}: {refused}");
    }
    assert_eq!(config_json(&env), two_projects("end"));
}

#[test]
fn a_server_added_under_a_name_used_elsewhere_warns_which_one_claude_uses() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![add("loc", r#"{"command": "x"}"#, vec![Target::User])],
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("local") && w.contains("instead of this server")),
        "{:?}",
        plan.warnings
    );
}

/// Two projects, with a local server in /work/a and a user server that both start a script by a relative path.
fn relative_projects() -> serde_json::Value {
    let mut config = two_projects("end");
    config["mcpServers"]["rel"] =
        serde_json::json!({"command": "node", "args": ["build/index.js"]});
    config["projects"]["/work/a"]["mcpServers"]["here"] =
        serde_json::json!({"command": "python", "args": ["server.py"]});
    config
}

fn relative_warnings(plan: &PlanView) -> Vec<&String> {
    plan.warnings
        .iter()
        .filter(|w| w.contains("each session's folder"))
        .collect()
}

#[test]
fn a_server_added_for_the_user_with_a_relative_path_is_warned_about_and_a_local_one_is_not() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let json = r#"{"command": "node", "args": ["build/index.js"], "env": {"CFG": "./c.json"}}"#;
    let plan = plan_for(&core, vec![add("rel", json, vec![Target::User])]);
    let [warning] = relative_warnings(&plan)[..] else {
        panic!("{:?}", plan.warnings)
    };
    assert!(
        warning.starts_with("rel: use full paths.")
            && warning.contains("can't find build/index.js or ./c.json (in CFG)."),
        "{warning}"
    );
    let plan = plan_for(
        &core,
        vec![add("rel", json, vec![Target::Local("/work/b".into())])],
    );
    assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
    let plan = plan_for(
        &core,
        vec![add(
            "abs",
            r#"{"command": "node", "args": ["/opt/x/index.js"]}"#,
            vec![Target::User],
        )],
    );
    assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
}

#[test]
fn a_local_server_moved_or_duplicated_to_the_user_with_a_relative_path_is_warned_about() {
    let env = quiet_env(&relative_projects());
    let core = env.start();
    for change in [
        Change::Move {
            id: local_id("here", "/work/a"),
            to: Target::User,
        },
        duplicate(local_id("here", "/work/a"), "here2", vec![Target::User]),
    ] {
        let plan = plan_for(&core, vec![change]);
        let [warning] = relative_warnings(&plan)[..] else {
            panic!("{:?}", plan.warnings)
        };
        assert!(warning.contains("server.py"), "{warning}");
    }
    // Into another project it is as it was: the session starts in that project's folder.
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: local_id("here", "/work/a"),
            to: Target::Local("/work/b".into()),
        }],
    );
    assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
}

#[test]
fn saving_a_user_server_says_so_while_it_still_has_a_relative_path_and_not_once_fixed() {
    let env = quiet_env(&relative_projects());
    let core = env.start();
    let save = |json: &str| Change::Replace {
        id: user_id("rel"),
        json: json.to_owned(),
    };
    let plan = plan_for(
        &core,
        vec![save(
            r#"{"command": "node", "args": ["build/index.js", "--verbose"]}"#,
        )],
    );
    assert_eq!(relative_warnings(&plan).len(), 1, "{:?}", plan.warnings);
    let plan = plan_for(
        &core,
        vec![save(
            r#"{"command": "node", "args": ["/srv/rel/build/index.js"]}"#,
        )],
    );
    assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
    // Turning it off writes no definition, so it is no moment to bring this up.
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("rel"),
            enabled: false,
        }],
    );
    assert!(relative_warnings(&plan).is_empty(), "{:?}", plan.warnings);
}

#[test]
fn a_point_raised_by_two_changes_apart_is_said_once() {
    // Variant of the migration's repeated note: two deletes with another change between them.
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![
            Change::Delete {
                id: local_id("loc", "/work/a"),
            },
            // Warns that a user server is switched per project.
            Change::SetEnabled {
                id: user_id("gw"),
                enabled: false,
            },
            Change::Delete { id: user_id("gw") },
        ],
    );
    let deleting = plan
        .warnings
        .iter()
        .filter(|w| w.starts_with("Deleting"))
        .count();
    assert_eq!(deleting, 1, "{:?}", plan.warnings);
}

#[test]
fn migrating_the_old_list_switches_its_servers_off_and_removes_it() {
    let env = quiet_env(&legacy_config());
    let core = env.start();
    let plan = plan_for(&core, vec![migrate("/work/a"), migrate("/work/b")]);
    assert_eq!(
        plan.summary,
        [
            "Turn off gw and loc in a, as the old switch-off list says",
            "Turn off gw in b, as the old switch-off list says"
        ]
    );
    assert_eq!(plan.action, "Migrate");
    assert!(!plan.destructive);
    assert!(!plan.nothing_to_do);
    // Said once, however many lists move (it was said once per list).
    let ignored: Vec<&String> = plan.notes.iter().filter(|n| n.contains("ignore")).collect();
    assert_eq!(ignored.len(), 1, "{:?}", plan.notes);
    let shown = format!("{plan:?}");
    assert!(!shown.contains("s3cret-value"), "{shown}");
    let events = apply_and_wait(&core, &plan);
    let now = config_json(&env);
    let was = legacy_config();
    assert_eq!(
        now["projects"]["/work/a"],
        serde_json::json!({
            "mcpServers": {"loc": {"type": "stdio", "command": "x"}},
            "disabledMcpServers": ["old", "gw", "loc"],
            "allowedTools": []
        })
    );
    assert_eq!(
        now["projects"]["/work/b"],
        serde_json::json!({"disabledMcpServers": ["gw"]})
    );
    assert_eq!(now["projects"]["/work/c"], was["projects"]["/work/c"]);
    assert_eq!(now["mcpServers"], was["mcpServers"]);
    let snap = *snapshots(&events).last().unwrap();
    let left: Vec<&str> = snap
        .legacy_disabled
        .iter()
        .map(|l| l.project.as_str())
        .collect();
    assert_eq!(left, ["/work/c"]);
    let loc = snap.servers.iter().find(|r| r.name == "loc").unwrap();
    assert!(!loc.enabled);
    let gw = snap.servers.iter().find(|r| r.name == "gw").unwrap();
    assert_eq!(gw.disabled_in, 2);
    verify_passes(&core);
}

#[test]
fn migrating_a_list_whose_servers_are_already_off_only_removes_it() {
    let mut config = legacy_config();
    config["projects"]["/work/b"]["disabledMcpServers"] = serde_json::json!(["gw"]);
    let env = quiet_env(&config);
    let core = env.start();
    let plan = plan_for(&core, vec![migrate("/work/b")]);
    assert_eq!(plan.summary, ["Remove the old switch-off list in b"]);
    assert!(!plan.nothing_to_do);
    apply_and_wait(&core, &plan);
    assert_eq!(
        config_json(&env)["projects"]["/work/b"],
        serde_json::json!({"disabledMcpServers": ["gw"]})
    );
}

#[test]
fn a_migration_is_refused_for_a_list_of_the_wrong_shape_or_one_that_is_gone() {
    let env = quiet_env(&legacy_config());
    let core = env.start();
    let why = plan_failure(&core, vec![migrate("/work/c")]);
    assert!(why.contains("other than server names"), "{why}");
    let why = plan_failure(&core, vec![migrate("/work/none")]);
    assert!(why.contains("no old switch-off list"), "{why}");
    // Nothing was written.
    assert_eq!(config_json(&env), legacy_config());
}

/// A rename or move used to drop the server from every profile, which then named it "no longer configured"
/// (found in review, PR #57). Its profiles follow it, and the review says so.
/// The palette offers each profile, so every snapshot names them, read anew like the rest: a profile made by hand
/// shows after a refresh.
#[test]
fn each_snapshot_names_the_profiles() {
    let env = quiet_env(&serde_json::json!({"mcpServers": {"gw": {"command": "gw"}}}));
    let backups = env.dir.path().join("backups");
    crate::profiles::save(&backups, "Work", &[user_id("gw").connection], None).unwrap();
    let core = env.start();
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    assert_eq!(snapshots(&events)[0].profiles, ["Work"]);
    crate::profiles::save(&backups, "Notes", &[user_id("gw").connection], None).unwrap();
    core.send(Command::Refresh { live_all: false });
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    assert_eq!(snapshots(&events)[0].profiles, ["Work", "Notes"]);
}

// ---- plain-language plans

#[test]
fn a_plan_says_in_words_what_it_does_and_what_the_button_does() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let off = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    assert_eq!(off.summary, ["Turn off gw in 2 projects"]);
    assert_eq!(off.action, "Turn off");
    assert!(!off.destructive);
    assert!(
        off.sessions
            .as_deref()
            .is_some_and(|s| s.contains("restart")),
        "{:?}",
        off.sessions
    );
    assert!(
        !off.warnings.iter().any(|w| w.contains("restart")),
        "the restart hint is not a warning"
    );
    assert!(
        !off.notes.iter().any(|n| n.contains("restart")),
        "said once, on its own: {:?}",
        off.notes
    );
    let delete = plan_for(
        &core,
        vec![Change::Delete {
            id: local_id("loc", "/work/a"),
        }],
    );
    assert_eq!(delete.summary, ["Delete loc in a"]);
    assert_eq!(delete.action, "Delete");
    assert!(delete.destructive);
    let mixed = plan_for(
        &core,
        vec![
            Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled: false,
            },
            Change::Delete { id: user_id("gw") },
        ],
    );
    assert_eq!(mixed.action, "Apply");
    assert_eq!(
        mixed.summary,
        ["Turn off loc in a", "Delete gw (user server)"]
    );
}

// ---- what a Claude Code session already open gets (it keeps its servers until it restarts)

#[test]
fn a_switch_in_one_project_says_open_sessions_there_keep_their_servers_until_restarted() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    for enabled in [false, true] {
        let plan = plan_for(
            &core,
            vec![Change::SetEnabled {
                id: local_id("loc", "/work/a"),
                enabled,
            }],
        );
        if enabled {
            // Already on: nothing is written, so nothing reaches any session.
            assert!(plan.nothing_to_do);
            assert_eq!(plan.sessions, None);
            continue;
        }
        assert_eq!(
            plan.sessions.as_deref(),
            Some(
                "Claude Code sessions already open in a keep the servers they started with until you restart \
                 them, or switch the same servers there with /mcp. New sessions get this."
            )
        );
    }
}

#[test]
fn a_user_server_switched_in_every_project_names_each_project() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    let said = plan.sessions.expect("a switch is written");
    assert!(said.contains("already open in a and b keep"), "{said}");
    assert!(said.contains("/mcp"), "{said}");
}

#[test]
fn a_change_outside_projects_says_every_open_session_keeps_its_servers() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    // A user server is loaded in every folder: no project to name, and /mcp cannot bring back a deleted one.
    let plan = plan_for(&core, vec![Change::Delete { id: user_id("gw") }]);
    assert_eq!(
        plan.sessions.as_deref(),
        Some(
            "Claude Code sessions already open keep the servers they started with until you restart them. New \
             sessions get this."
        )
    );
    // A local server deleted: its project is named, but switching it there with /mcp is no answer.
    let plan = plan_for(
        &core,
        vec![Change::Delete {
            id: local_id("loc", "/work/a"),
        }],
    );
    let said = plan.sessions.expect("a delete is written");
    assert!(said.contains("already open in a keep"), "{said}");
    assert!(!said.contains("/mcp"), "{said}");
}

#[test]
fn one_project_is_one_project() {
    let env = quiet_env(&serde_json::json!({
        "mcpServers": {"gw": {"type": "stdio", "command": "x"}},
        "projects": {"/work/only": {}}
    }));
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    assert_eq!(plan.summary, ["Turn off gw in 1 project"]);
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("the 1 project it knows")),
        "{:?}",
        plan.warnings
    );
}
