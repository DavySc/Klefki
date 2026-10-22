//! Engine tests: the line that opens a server in the MCP Inspector.
use super::*;

fn line(core: &Core, id: ServerId) -> Result<crate::inspector::InspectorCommand, String> {
    core.send(Command::InspectorCommand(id));
    collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::InspectorCommand { .. }))
    })
    .into_iter()
    .find_map(|e| match e {
        Event::InspectorCommand { line, .. } => Some(line),
        _ => None,
    })
    .unwrap()
}

#[test]
fn the_line_is_made_from_the_servers_own_file_and_starts_it_in_its_project() {
    let (env, key, repo) = repo_env(serde_json::json!({
        "mcpServers": {"loc": {"type": "stdio", "command": "x", "args": ["--api-key", "sk-live-0123456789abcdef"], "env": {"K": "v-12345678"}}}
    }));
    let claude_json = env.dir.path().join("cfg").join(".claude.json");
    let before = std::fs::read(&claude_json).unwrap();
    let core = env.start();

    let local = line(&core, local_id("loc", &key)).unwrap();
    assert!(
        local
            .text
            .starts_with("npx @modelcontextprotocol/inspector --cwd "),
        "{}",
        local.text
    );
    assert!(
        local
            .text
            .ends_with(&format!("-- x --api-key {}", crate::inspector::HIDDEN)),
        "{}",
        local.text
    );
    assert!(!local.text.contains("sk-live") && !local.text.contains("v-12345678"));

    // A project server comes from the repository's .mcp.json, and starts in the repository.
    let project = line(&core, project_id(&key, "ps")).unwrap();
    assert!(project.text.contains("--cwd"), "{}", project.text);
    if cfg!(unix) {
        assert!(
            project
                .text
                .contains(&format!("--cwd {} --", repo.display())),
            "{}",
            project.text
        );
    }
    assert!(
        project.text.ends_with("-- node server.js --port 9"),
        "{}",
        project.text
    );

    // Reading is all it does.
    assert_eq!(std::fs::read(&claude_json).unwrap(), before);
}

#[test]
fn a_server_that_is_gone_or_not_in_a_file_of_the_users_says_why_there_is_no_line() {
    let (env, _key, _repo) = repo_env(serde_json::json!({}));
    let core = env.start();
    let why = line(&core, user_id("gone")).unwrap_err();
    assert!(why.contains("no longer in the configuration"), "{why}");
}
