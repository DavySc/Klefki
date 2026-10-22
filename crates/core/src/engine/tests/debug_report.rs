//! Engine tests: the definition a debug report holds.
use super::*;

fn definition(core: &Core, id: ServerId) -> Result<crate::export::Exported, String> {
    core.send(Command::RedactedDefinition {
        id,
        also: Vec::new(),
    });
    collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::RedactedDefinition { .. }))
    })
    .into_iter()
    .find_map(|e| match e {
        Event::RedactedDefinition { result, .. } => Some(result),
        _ => None,
    })
    .unwrap()
}

#[test]
fn the_definition_keeps_its_shape_and_loses_what_may_be_secret() {
    let (env, key, _repo) = repo_env(serde_json::json!({
        "mcpServers": {"loc": {"type": "stdio", "command": "x", "args": ["--api-key", "sk-live-0123456789abcdef", "--verbose"], "env": {"GITHUB_TOKEN": "ghp_0123456789abcdefghij", "MODE": "x"}}}
    }));
    let claude_json = env.dir.path().join("cfg").join(".claude.json");
    let before = std::fs::read(&claude_json).unwrap();
    let core = env.start();

    let found = definition(&core, local_id("loc", &key)).unwrap();
    assert!(found.text.contains("\"command\": \"x\""), "{}", found.text);
    assert!(found.text.contains("--verbose"), "{}", found.text);
    assert!(
        found.text.contains("GITHUB_TOKEN") && found.text.contains("${GITHUB_TOKEN}"),
        "{}",
        found.text
    );
    assert!(
        !found.text.contains("sk-live") && !found.text.contains("ghp_0123"),
        "{}",
        found.text
    );
    assert_eq!(std::fs::read(&claude_json).unwrap(), before);
}

#[test]
fn a_server_with_no_definition_in_the_users_files_says_why() {
    let (env, _key, _repo) = repo_env(serde_json::json!({}));
    let core = env.start();
    let why = definition(&core, user_id("gone")).unwrap_err();
    assert_eq!(why, "it is no longer in the configuration");
    let plugin = definition(&core, ServerId::new(Scope::Plugin, None, "p")).unwrap_err();
    assert!(plugin.contains("plugin"), "{plugin}");
}

#[test]
fn the_texts_the_report_also_holds_come_back_with_the_servers_secrets_hidden() {
    let (env, key, _repo) = repo_env(serde_json::json!({
        "mcpServers": {"loc": {"type": "stdio", "command": "x", "env": {"K": "plain-secret-value"}}}
    }));
    let core = env.start();
    core.send(Command::RedactedDefinition {
        id: local_id("loc", &key),
        also: vec![
            "Issue: bad key plain-secret-value for postgres://u:pw99887766@db/x".to_owned(),
            "Connection closed".to_owned(),
        ],
    });
    let events = collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::RedactedDefinition { .. }))
    });
    let Some(Event::RedactedDefinition { also, .. }) = events
        .into_iter()
        .find(|e| matches!(e, Event::RedactedDefinition { .. }))
    else {
        panic!("answered");
    };
    assert_eq!(
        also,
        [
            "Issue: bad key *** for postgres://***@db/x",
            "Connection closed"
        ]
    );
    // Also for a server whose definition cannot be shown: the patterns still apply.
    core.send(Command::RedactedDefinition {
        id: user_id("gone"),
        also: vec!["see https://h.example/x?key=abcdef123456".to_owned()],
    });
    let events = collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::RedactedDefinition { .. }))
    });
    let dump = format!("{events:?}");
    assert!(!dump.contains("abcdef123456"), "{dump}");
}
