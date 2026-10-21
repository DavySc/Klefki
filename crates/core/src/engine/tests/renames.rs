//! Engine tests: renaming a server and duplicates that cannot be made.
use super::*;

/// `Gateway` for the user and `gateway` in /work/a, which differ only in case; both are off where they are used.
fn casing_config() -> serde_json::Value {
    serde_json::json!({
        "numStartups": 3,
        "mcpServers": {
            "Gateway": {"type": "stdio", "command": "node", "env": {"TOKEN": "s3cret-value"}},
            "plain": {"type": "stdio", "command": "p"}
        },
        "projects": {
            "/work/a": {
                "mcpServers": {"gateway": {"type": "stdio", "command": "x", "env": {"KEY": "other-secret"}}},
                "disabledMcpServers": ["gateway", "zz"]
            },
            "/work/b": {"disabledMcpServers": ["Gateway", "other"]}
        }
    })
}

#[test]
fn renaming_a_local_server_changes_its_spelling_and_keeps_it_off() {
    let env = quiet_env(&casing_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![rename(local_id("gateway", "/work/a"), "Gateway")],
    );
    assert_eq!(plan.summary, ["Rename gateway to Gateway in a"]);
    assert_eq!(plan.action, "Rename");
    assert!(!plan.destructive);
    let shown = format!("{plan:?}");
    assert!(!shown.contains("other-secret"), "{shown}");
    let events = apply_and_wait(&core, &plan);
    let now = config_json(&env);
    let was = casing_config();
    let servers = &now["projects"]["/work/a"]["mcpServers"];
    assert_eq!(
        servers["Gateway"],
        was["projects"]["/work/a"]["mcpServers"]["gateway"]
    );
    assert!(servers.get("gateway").is_none(), "{servers}");
    assert_eq!(
        now["projects"]["/work/a"]["disabledMcpServers"],
        serde_json::json!(["Gateway", "zz"])
    );
    // Nothing else moved: the user's servers and the other project.
    assert_eq!(now["mcpServers"], was["mcpServers"]);
    assert_eq!(now["projects"]["/work/b"], was["projects"]["/work/b"]);
    let rows = &snapshots(&events).last().unwrap().servers;
    assert!(rows.iter().all(|r| !r.casing_collision), "{rows:?}");
    verify_passes(&core);
}

#[test]
fn renaming_a_user_server_keeps_it_off_in_every_project_that_had_it_off() {
    let env = quiet_env(&casing_config());
    let core = env.start();
    let plan = plan_for(&core, vec![rename(user_id("Gateway"), "gateway")]);
    assert_eq!(plan.summary, ["Rename Gateway to gateway (user server)"]);
    apply_and_wait(&core, &plan);
    let now = config_json(&env);
    let was = casing_config();
    assert_eq!(now["mcpServers"]["gateway"], was["mcpServers"]["Gateway"]);
    assert!(now["mcpServers"].get("Gateway").is_none());
    assert_eq!(now["mcpServers"]["plain"], was["mcpServers"]["plain"]);
    assert_eq!(
        now["projects"]["/work/b"]["disabledMcpServers"],
        serde_json::json!(["gateway", "other"])
    );
    // The local server of the same spelling keeps its own entry in /work/a.
    assert_eq!(
        now["projects"]["/work/a"]["disabledMcpServers"],
        serde_json::json!(["gateway", "zz"])
    );
    verify_passes(&core);
}

#[test]
fn two_renames_make_one_plan_and_fix_the_collision() {
    let env = quiet_env(&casing_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![
            rename(local_id("gateway", "/work/a"), "gateway2"),
            rename(user_id("Gateway"), "gateway1"),
        ],
    );
    assert_eq!(plan.action, "Rename");
    let events = apply_and_wait(&core, &plan);
    let rows = &snapshots(&events).last().unwrap().servers;
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"gateway1") && names.contains(&"gateway2"),
        "{names:?}"
    );
    assert!(rows.iter().all(|r| !r.casing_collision));
    verify_passes(&core);
}

#[test]
fn a_rename_that_cannot_be_made_says_why() {
    let env = quiet_env(&casing_config());
    let core = env.start();
    for (change, why) in [
        (rename(user_id("Gateway"), "Gateway"), "already"),
        (rename(user_id("Gateway"), "plain"), "already"),
        (rename(local_id("gateway", "/work/a"), "gateway"), "already"),
        (rename(user_id("Gateway"), "bad name"), "name"),
        (rename(user_id("Gateway"), "-x"), "name"),
        (rename(user_id("ghost"), "ok"), "no longer"),
    ] {
        let refused = plan_failure(&core, vec![change.clone()]);
        assert!(refused.contains(why), "{change:?}: {refused}");
    }
    assert_eq!(config_json(&env), casing_config());
}

#[test]
fn a_project_server_is_not_renamed() {
    let (env, key, _repo) = repo_env(serde_json::json!({}));
    let core = env.start();
    let refused = plan_failure(&core, vec![rename(project_id(&key, "ps"), "ps2")]);
    assert!(refused.contains("repository"), "{refused}");
}

#[test]
fn a_server_that_signs_in_with_oauth_is_not_renamed() {
    let mut config = casing_config();
    config["mcpServers"]["Gateway"] = serde_json::json!({
        "type": "http", "url": "https://example.com/mcp",
        "oauth": {"clientId": "abc"}
    });
    let env = quiet_env(&config);
    let core = env.start();
    let refused = plan_failure(&core, vec![rename(user_id("Gateway"), "gateway1")]);
    assert!(refused.contains("OAuth"), "{refused}");
    assert_eq!(config_json(&env), config);
}

#[test]
fn duplicates_that_cannot_be_made_say_why() {
    let env = quiet_env(&two_projects("end"));
    let core = env.start();
    for (change, why) in [
        (
            duplicate(user_id("gw"), "gw", vec![Target::User]),
            "already",
        ),
        (
            duplicate(user_id("gw"), "loc", vec![Target::Local("/work/a".into())]),
            "already",
        ),
        (
            duplicate(user_id("gw"), "bad name", vec![Target::User]),
            "name",
        ),
        (duplicate(user_id("gw"), "-x", vec![Target::User]), "name"),
        (duplicate(user_id("gw"), "ok", vec![]), "where"),
        (
            duplicate(
                user_id("gw"),
                "ok",
                vec![Target::Local("/elsewhere".into())],
            ),
            "project",
        ),
        (
            duplicate(user_id("ghost"), "ok", vec![Target::User]),
            "no longer",
        ),
    ] {
        let refused = plan_failure(&core, vec![change.clone()]);
        assert!(refused.contains(why), "{change:?}: {refused}");
    }
    assert_eq!(config_json(&env), two_projects("end"));
}

#[test]
fn a_project_server_is_not_duplicated_yet() {
    let (env, key, _repo) = repo_env(serde_json::json!({}));
    let core = env.start();
    let refused = plan_failure(
        &core,
        vec![duplicate(project_id(&key, "ps"), "ps2", vec![Target::User])],
    );
    assert!(refused.contains(".mcp.json"), "{refused}");
}
