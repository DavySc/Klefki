//! Engine tests: the last starts of a server, from Claude Code's logs (decision 013).
use super::*;
use crate::api::{LastStarts, StartOutcome};

const FAIL: &str = include_str!("../../../../../fixtures/mcp-logs/fail.jsonl");
const OK: &str = include_str!("../../../../../fixtures/mcp-logs/ok.jsonl");

/// Writes a log of `server` started in `folder`, the fixture's folder changed to it and `extra` printed on stderr.
fn log(root: &Path, folder: &str, server: &str, time: &str, text: &str, extra: &str) {
    let dir = root
        .join(crate::last_start::folder_name(folder))
        .join(format!(
            "mcp-logs-{}",
            crate::last_start::folder_name(server)
        ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = text.replace("\"/home/u/proj\"", &serde_json::to_string(folder).unwrap());
    if !extra.is_empty() {
        let line = serde_json::json!({"error": format!("Server stderr: {extra}\n"), "timestamp": "2026-10-22T07:32:16.700Z", "cwd": folder});
        text = text.replacen('\n', &format!("\n{line}\n"), 1);
    }
    std::fs::write(dir.join(format!("{time}.jsonl")), text).unwrap();
}

fn starts_of(core: &Core, id: &ServerId) -> LastStarts {
    core.send(Command::LastStarts(id.clone()));
    let events = collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::LastStarts { id: got, .. } if got == id))
    });
    events
        .into_iter()
        .find_map(|e| match e {
            Event::LastStarts { id: got, starts } if &got == id => Some(starts),
            _ => None,
        })
        .unwrap()
}

#[test]
fn the_last_starts_come_from_claude_codes_logs_with_the_servers_values_hidden() {
    let env = Env::new(
        &serde_json::json!({
            "mcpServers": {
                "db": {"type": "stdio", "command": "node", "env": {"DB_URL": "postgres://db.internal/s3cret-value-123"}},
                "my.server": {"type": "stdio", "command": "a"},
                "my-server": {"type": "stdio", "command": "b"}
            },
            "projects": {"/work/app": {"mcpServers": {"db": {"type": "stdio", "command": "local"}}}}
        }),
        r#"{"rules":[{"args":["--version"]}]}"#,
        &[],
    );
    let root = env.dir.path().join("logs");
    log(
        &root,
        "/work/b",
        "db",
        "2026-10-22T07-32-16-377Z",
        FAIL,
        "cannot reach postgres://db.internal/s3cret-value-123",
    );
    log(&root, "/work/app", "db", "2026-10-22T08-00-00-000Z", OK, "");
    let core = start_with(
        EngineConfig {
            claude_logs: Some(root.clone()),
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));

    // The user server: not where the local one of its name loads.
    let user = starts_of(&core, &ServerId::new(Scope::User, None, "db"));
    assert_eq!(user.problem, None);
    assert_eq!(user.starts.len(), 1, "{user:?}");
    let start = &user.starts[0];
    assert_eq!(start.folder, "/work/b");
    assert!(
        matches!(&start.outcome, StartOutcome::Failed { code: Some(c), .. } if c == "CONNECTION_CLOSED")
    );
    assert!(
        start.server_said.iter().any(|l| l == "cannot reach ***"),
        "{:?}",
        start.server_said
    );
    assert!(!format!("{user:?}").contains("s3cret-value-123"));
    assert!(user.shares_log_with.is_empty());

    // The local server: only in its project.
    let local = starts_of(&core, &ServerId::new(Scope::Local, Some("/work/app"), "db"));
    assert_eq!(local.starts.len(), 1, "{local:?}");
    assert_eq!(local.starts[0].folder, "/work/app");
    assert!(matches!(
        local.starts[0].outcome,
        StartOutcome::Connected { .. }
    ));

    // Two names Claude Code logs in one folder.
    let dotted = starts_of(&core, &ServerId::new(Scope::User, None, "my.server"));
    assert_eq!(dotted.shares_log_with, ["my-server"]);
    assert!(dotted.starts.is_empty());
    assert!(dotted.looked_in.is_some());
}

#[test]
fn without_a_log_folder_or_a_server_it_says_why() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"s": {"type": "stdio", "command": "a"}}}),
        r#"{"rules":[{"args":["--version"]}]}"#,
        &[],
    );
    let core = start_with(
        EngineConfig {
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let none = starts_of(&core, &ServerId::new(Scope::User, None, "s"));
    assert!(none.problem.is_some());
    let gone = starts_of(&core, &ServerId::new(Scope::User, None, "gone"));
    assert!(gone.problem.unwrap().contains("no longer"));
}
