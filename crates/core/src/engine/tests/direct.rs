//! Engine tests: Klefki checks a user or local stdio server itself first, and asks Claude Code for every answer other
//! than Connected (decision 015).
use super::*;
use crate::cli::testutil::fake_mcp_server;
use crate::engine::checks::{Job, JobKind, Queue};

/// A stdio server run by `fake-mcp-server` in `mode`, writing what it was started with to `dump` when it starts.
fn server(mode: &str, dump: &Path) -> serde_json::Value {
    serde_json::json!({"type": "stdio", "command": fake_mcp_server(), "args": [mode],
        "env": {"FAKE_MCP_DUMP": dump}})
}

/// Whether the server whose dump is `dump` was started.
fn started(dump: &Path) -> bool {
    dump.exists()
}

/// The answers for `name` that are not "Checking…": status, detail, given by Klefki.
fn answers(events: &[Event], name: &str) -> Vec<(Status, Option<String>, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Status {
                id,
                status,
                detail,
                by_klefki,
                ..
            } if id.name == name && *status != Status::Checking => {
                Some((*status, detail.clone(), *by_klefki))
            }
            _ => None,
        })
        .collect()
}

/// Started with direct checks on (or off) and no background checks: only what a test asks for runs.
fn start_direct(env: &Env, direct_checks: bool) -> Core {
    let core = start_with(
        EngineConfig {
            direct_checks,
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    core
}

fn check(core: &Core, id: ServerId) -> Vec<Event> {
    let name = id.name.clone();
    core.send(Command::Check(id));
    collect(core, |e| !answers(e, &name).is_empty())
}

/// The fake CLI's answer to `mcp get` for every server: Failed, so a Claude Code answer is told from Klefki's.
const CLAUDE_SAYS_FAILED: &str =
    r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"failed.txt"}]}"#;

fn env_with(servers: serde_json::Value) -> Env {
    Env::new(&servers, CLAUDE_SAYS_FAILED, &[("failed.txt", FAILED)])
}

#[test]
fn a_user_server_that_connects_is_connected_by_klefki_without_starting_claude_code() {
    let tmp = tempfile::tempdir().unwrap();
    let dump = tmp.path().join("dump.json");
    let env = env_with(serde_json::json!({"mcpServers": {"s": server("ok", &dump)}}));
    let core = start_direct(&env, true);
    let events = check(&core, user_id("s"));
    assert_eq!(answers(&events, "s"), [(Status::Connected, None, true)]);
    assert!(env.gets().is_empty(), "Claude Code was not asked");
    let dumped: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(
        Path::new(dumped["cwd"].as_str().unwrap()),
        env.dir.path().join("home"),
        "a user server runs in the home folder, as Claude Code runs it"
    );
    assert_eq!(dumped["env"]["CLAUDECODE"], "1");
}

#[test]
fn every_other_answer_is_claude_codes() {
    // Exits at once, fails its hello, fails its tools: each is checked again by Claude Code, and its answer shown.
    for mode in ["exit", "badinit", "toolsfail"] {
        let tmp = tempfile::tempdir().unwrap();
        let dump = tmp.path().join("dump.json");
        let env = env_with(serde_json::json!({"mcpServers": {"s": server(mode, &dump)}}));
        let core = start_direct(&env, true);
        let events = check(&core, user_id("s"));
        assert!(started(&dump), "{mode}: Klefki started it first");
        assert_eq!(
            answers(&events, "s"),
            [(
                Status::Failed,
                Some("CONNECTION_CLOSED: Connection closed".to_owned()),
                false
            )],
            "{mode}"
        );
        assert_eq!(env.gets().len(), 1, "{mode}: Claude Code was asked once");
    }
}

#[test]
fn a_program_klefki_cannot_start_is_left_to_claude_code() {
    let env = env_with(serde_json::json!({"mcpServers": {
        "missing": {"type": "stdio", "command": "klefki-no-such-program"},
        "relative": {"type": "stdio", "command": "./not-here"}}}));
    let core = start_direct(&env, true);
    for name in ["missing", "relative"] {
        let events = check(&core, user_id(name));
        assert_eq!(
            answers(&events, name).last().map(|a| (a.0, a.2)),
            Some((Status::Failed, false)),
            "{name}"
        );
    }
    let mut asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    asked.sort();
    assert_eq!(asked, ["missing", "relative"]);
}

#[test]
fn with_claude_code_only_klefki_starts_nothing_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let dump = tmp.path().join("dump.json");
    let env = env_with(serde_json::json!({"mcpServers": {"s": server("ok", &dump)}}));
    // Off from the start.
    let core = start_direct(&env, false);
    let events = check(&core, user_id("s"));
    assert_eq!(answers(&events, "s").last().map(|a| a.2), Some(false));
    assert!(!started(&dump));
    drop(core);
    // On, then turned off while Klefki runs.
    let core = start_direct(&env, true);
    core.send(Command::CheckWithClaudeOnly(true));
    let events = check(&core, user_id("s"));
    assert_eq!(answers(&events, "s").last().map(|a| a.2), Some(false));
    assert!(!started(&dump), "the setting holds from the next check");
    core.send(Command::CheckWithClaudeOnly(false));
    let events = check(&core, user_id("s"));
    assert_eq!(
        answers(&events, "s").last().cloned(),
        Some((Status::Connected, None, true))
    );
}

#[test]
fn diagnose_a_check_given_more_time_and_project_servers_stay_with_claude_code() {
    let tmp = tempfile::tempdir().unwrap();
    let dump = tmp.path().join("dump.json");
    let project = tmp.path().join("repo");
    std::fs::create_dir_all(&project).unwrap();
    let repo_dump = tmp.path().join("repo-dump.json");
    std::fs::write(
        project.join(".mcp.json"),
        serde_json::json!({"mcpServers": {"p": server("ok", &repo_dump)}}).to_string(),
    )
    .unwrap();
    let key = project.to_string_lossy().into_owned();
    let env = env_with(serde_json::json!({
        "mcpServers": {"s": server("ok", &dump)},
        "projects": {key.clone(): {"enabledMcpjsonServers": ["p"], "hasTrustDialogAccepted": true}}}));
    let core = start_direct(&env, true);
    core.send(Command::CheckLonger(user_id("s")));
    let events = collect(&core, |e| !answers(e, "s").is_empty());
    assert!(!answers(&events, "s")[0].2);
    core.send(Command::Diagnose(user_id("s")));
    collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::Diagnosis { .. }))
    });
    assert!(!started(&dump), "neither started the server itself");
    let events = check(&core, ServerId::new(Scope::Project, Some(&key), "p"));
    assert_eq!(answers(&events, "p").last().map(|a| a.2), Some(false));
    assert!(
        !started(&repo_dump),
        "a repository's command is run only by Claude Code"
    );
}

#[test]
fn a_local_server_gets_its_projects_settings_only_where_the_project_is_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut projects = serde_json::Map::new();
    let mut folders = Vec::new();
    for (name, trusted) in [("trusted", true), ("untrusted", false)] {
        let folder = tmp.path().join(name);
        std::fs::create_dir_all(folder.join(".claude")).unwrap();
        std::fs::write(
            folder.join(".claude").join("settings.json"),
            r#"{"env": {"FROM_PROJECT": "yes"}}"#,
        )
        .unwrap();
        let key = folder.to_string_lossy().into_owned();
        projects.insert(
            key.clone(),
            serde_json::json!({"hasTrustDialogAccepted": trusted, "mcpServers": {"loc": {
                "command": fake_mcp_server(), "args": ["needsenv", "FROM_PROJECT", "yes"]}}}),
        );
        folders.push(key);
    }
    let env = env_with(serde_json::json!({"projects": projects}));
    let core = start_direct(&env, true);
    let events = check(&core, local_id("loc", &folders[0]));
    assert_eq!(
        answers(&events, "loc"),
        [(Status::Connected, None, true)],
        "trusted: the project's settings reach it"
    );
    let events = check(&core, local_id("loc", &folders[1]));
    assert_eq!(
        answers(&events, "loc").last().map(|a| (a.0, a.2)),
        Some((Status::Failed, false)),
        "untrusted: it fails without them, and Claude Code says so"
    );
}

#[test]
fn a_server_that_never_answers_says_claude_code_is_asked_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let dump = tmp.path().join("dump.json");
    let env = env_with(serde_json::json!({"mcpServers": {"s": server("hang", &dump)}}));
    std::fs::write(
        env.dir.path().join("cfg").join("settings.json"),
        r#"{"env": {"MCP_TIMEOUT": "1000"}}"#,
    )
    .unwrap();
    let core = start_direct(&env, true);
    let events = check(&core, user_id("s"));
    assert!(
        events.iter().any(
            |e| matches!(e, Event::Status { id, status: Status::Checking, detail: Some(d), .. }
            if id.name == "s" && d == "It did not answer within 1 s. Asking Claude Code…")
        ),
        "{events:#?}"
    );
    assert_eq!(
        answers(&events, "s").last().map(|a| (a.0, a.2)),
        Some((Status::Failed, false))
    );
}

#[test]
fn background_checks_klefki_runs_itself_run_three_at_once_and_claude_codes_one() {
    let job = |name: &str, direct: bool| Job {
        id: user_id(name),
        name: crate::model::ServerName::parse(name).unwrap(),
        cwd: PathBuf::new(),
        kind: JobKind::Check,
        announce: false,
        background: true,
        now: false,
        alone: false,
        longer: false,
        folder: None,
        direct,
        definition: 0,
        checking_detail: None,
    };
    let mut queue = Queue {
        jobs: [
            job("c1", false),
            job("c2", false),
            job("d1", true),
            job("d2", true),
            job("d3", true),
        ]
        .into(),
        ..Queue::default()
    };
    let mut taken = Vec::new();
    while let Some(j) = queue.take() {
        taken.push(j.id.name);
    }
    assert_eq!(
        taken,
        ["c1", "d1", "d2"],
        "three at once, one of them Claude Code"
    );
    assert_eq!((queue.background_running, queue.background_claude), (3, 1));
}

#[test]
fn background_checks_of_servers_klefki_starts_are_not_taken_into_a_list() {
    // Four servers in the home folder would be one `claude mcp list`; those Klefki connects to are never in one.
    let tmp = tempfile::tempdir().unwrap();
    let servers: serde_json::Map<String, serde_json::Value> = (0..5)
        .map(|i| {
            (
                format!("s{i}"),
                server("ok", &tmp.path().join(format!("{i}.json"))),
            )
        })
        .collect();
    let env = env_with(serde_json::json!({"mcpServers": servers}));
    let core = start_with(
        EngineConfig {
            direct_checks: true,
            ..env.config()
        },
        || {},
    );
    let events = collect(&core, |e| {
        (0..5).all(|i| !answers(e, &format!("s{i}")).is_empty())
    });
    for i in 0..5 {
        assert_eq!(
            answers(&events, &format!("s{i}")),
            [(Status::Connected, None, true)]
        );
    }
    assert!(env.lists().is_empty() && env.gets().is_empty());
}
