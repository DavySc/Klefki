//! Engine tests: the definition a debug report holds.
use super::*;

fn definition(core: &Core, id: ServerId) -> Result<crate::export::Exported, String> {
    core.send(Command::RedactedDefinition(id));
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
