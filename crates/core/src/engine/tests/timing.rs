//! Engine tests: how long a check took against Claude Code's time limit, and checks given more time.
use super::*;
use crate::api::{LONGER_CHECK, LimitSetIn, TimeLimit, Timing};

const TIMED_OUT: &[u8] = include_bytes!("../../../../../fixtures/mcp-get/timed-out-stdio.txt");

/// The arguments of a check given more time, as Claude Code is started with them.
const LONGER: &str = r#""--settings","{\"env\":{\"MCP_TIMEOUT\":\"120000\"}}","mcp","get""#;

fn slow_env(rules: &str, files: &[(&str, &[u8])]) -> Env {
    Env::new(
        &serde_json::json!({"mcpServers": {"slow": {"command": "x"}}}),
        &format!(r#"{{"rules":[{{"args":["--version"]}}{rules}]}}"#),
        files,
    )
}

/// Started with no checks of its own: only the ones a test asks for run.
fn start_quiet(config: EngineConfig) -> Core {
    let core = start_with(
        EngineConfig {
            background_checks: false,
            ..config
        },
        || {},
    );
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    core
}

/// The last answer for `name`: its status, detail and timing.
fn answer(events: &[Event], name: &str) -> Option<(Status, Option<String>, Option<Timing>)> {
    events.iter().rev().find_map(|e| match e {
        Event::Status {
            id,
            status,
            detail,
            timing,
            ..
        } if id.name == name && *status != Status::Checking => {
            Some((*status, detail.clone(), timing.clone()))
        }
        _ => None,
    })
}

fn check(core: &Core, command: Command) -> (Status, Option<String>, Option<Timing>, Vec<Event>) {
    core.send(command);
    let events = collect(core, |e| answer(e, "slow").is_some());
    let (status, detail, timing) = answer(&events, "slow").unwrap();
    (status, detail, timing, events)
}

/// Every argument list the fake CLI was started with, but `--version`.
fn runs(env: &Env) -> Vec<Vec<String>> {
    std::fs::read_to_string(&env.log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|l| l["event"] == "start" && l["argv"] != serde_json::json!(["--version"]))
        .map(|l| serde_json::from_value(l["argv"].clone()).unwrap())
        .collect()
}

/// Claude Code's own limit, changed in the test's user settings.
fn default_limit(env: &Env) -> TimeLimit {
    let user = env.dir.path().join("cfg").join("settings.json");
    TimeLimit {
        limit: Duration::from_secs(30),
        set_in: LimitSetIn::Default,
        unreadable: false,
        change_in: Some(crate::config::policy::home_as_tilde(&user)),
    }
}

#[test]
fn a_check_claude_code_gave_up_on_says_how_long_it_took_and_the_limit() {
    let env = slow_env(
        r#",{"args":["mcp","get","slow"],"stdout_file":"t.txt","delay_ms":400}"#,
        &[("t.txt", TIMED_OUT)],
    );
    let core = start_quiet(env.config());
    let (status, detail, timing, _) = check(&core, Command::Check(user_id("slow")));
    assert_eq!(status, Status::Failed);
    assert_eq!(detail.as_deref(), Some("Request timed out"));
    let timing = timing.expect("a timeout comes with its timing");
    assert!(timing.took >= Duration::from_millis(400), "{timing:?}");
    assert!(timing.took < WAIT, "{timing:?}");
    assert_eq!(timing.limit, default_limit(&env));
    assert_eq!(timing.given, None);
    assert!(timing.timed_out);
}

#[test]
fn other_answers_come_without_a_timing() {
    for (file, status) in [(CONNECTED, Status::Connected), (FAILED, Status::Failed)] {
        let env = slow_env(
            r#",{"args":["mcp","get","slow"],"stdout_file":"a.txt"}"#,
            &[("a.txt", file)],
        );
        let core = start_quiet(env.config());
        let (got, _, timing, _) = check(&core, Command::Check(user_id("slow")));
        assert_eq!(got, status);
        assert_eq!(timing, None, "{status:?}");
    }
}

#[test]
fn the_limit_is_read_where_the_check_runs() {
    let env = slow_env(
        r#",{"args":["mcp","get","slow"],"stdout_file":"t.txt"}"#,
        &[("t.txt", TIMED_OUT)],
    );
    let settings = env.dir.path().join("cfg").join("settings.json");
    std::fs::write(&settings, r#"{"env":{"MCP_TIMEOUT":"10000"}}"#).unwrap();
    let core = start_quiet(env.config());
    let (_, _, timing, _) = check(&core, Command::Check(user_id("slow")));
    let limit = timing.unwrap().limit;
    assert_eq!(limit.limit, Duration::from_secs(10));
    assert!(
        matches!(&limit.set_in, LimitSetIn::File(f) if f.ends_with("settings.json")),
        "{limit:?}"
    );

    // A local server runs in its project, where the project's own settings come first.
    let project = env.dir.path().join("p");
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(
        project.join(".claude").join("settings.local.json"),
        r#"{"env":{"MCP_TIMEOUT":"5000"}}"#,
    )
    .unwrap();
    let key = project.to_string_lossy().into_owned();
    let env = Env::new(
        &serde_json::json!({"projects": {&key: {"mcpServers": {"slow": {"command": "x"}}}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","slow"],"stdout_file":"t.txt"}]}"#,
        &[("t.txt", TIMED_OUT)],
    );
    let core = start_quiet(env.config());
    let (_, _, timing, _) = check(&core, Command::Check(local_id("slow", &key)));
    let limit = timing.unwrap().limit;
    assert_eq!(limit.limit, Duration::from_secs(5));
    assert!(
        matches!(&limit.set_in, LimitSetIn::File(f) if f.ends_with("settings.local.json")),
        "{limit:?}"
    );
}

#[test]
fn a_check_given_more_time_runs_with_the_longer_limit_and_always_says_how_long_it_took() {
    let env = slow_env(
        &format!(r#",{{"args":[{LONGER},"slow"],"stdout_file":"c.txt","delay_ms":300}}"#),
        &[("c.txt", CONNECTED)],
    );
    let core = start_quiet(env.config());
    let (status, _, timing, events) = check(&core, Command::CheckLonger(user_id("slow")));
    assert_eq!(status, Status::Connected);
    let timing = timing.expect("a check given more time always says how long it took");
    assert_eq!(timing.given, Some(LONGER_CHECK));
    assert!(!timing.timed_out, "it connected");
    assert_eq!(
        timing.limit,
        default_limit(&env),
        "the limit of a session is kept"
    );
    assert!(timing.took >= Duration::from_millis(300), "{timing:?}");
    assert_eq!(
        runs(&env),
        [[
            "--settings",
            r#"{"env":{"MCP_TIMEOUT":"120000"}}"#,
            "mcp",
            "get",
            "slow"
        ]],
        "passed as settings, which win over the user's own MCP_TIMEOUT"
    );
    let waiting = events.iter().find_map(|e| match e {
        Event::Status {
            status: Status::Checking,
            detail,
            ..
        } => Some(detail.clone()),
        _ => None,
    });
    assert_eq!(
        waiting,
        Some(Some("giving it up to 2 minutes to connect".to_owned())),
        "it says why this check takes longer"
    );

    // A plain check after it runs as before.
    let (_, _, timing, _) = check(&core, Command::Check(user_id("slow")));
    assert_eq!(timing, None);
}

#[test]
fn the_longer_settings_give_the_longer_limit() {
    let settings: serde_json::Value =
        serde_json::from_str(crate::engine::checks::LONGER_SETTINGS).unwrap();
    assert_eq!(
        settings["env"]["MCP_TIMEOUT"],
        LONGER_CHECK.as_millis().to_string()
    );
}

#[test]
fn a_raised_limit_waits_that_much_longer_for_claude_codes_answer() {
    // Claude Code answers after 4.5 s; Klefki's own wait is 3 s.
    let env = slow_env(
        r#",{"args":["mcp","get","slow"],"stdout_file":"c.txt","delay_ms":4500}"#,
        &[("c.txt", CONNECTED)],
    );
    let config = || EngineConfig {
        check_timeout: Duration::from_secs(3),
        ..env.config()
    };
    let core = start_quiet(config());
    let (status, detail, timing, _) = check(&core, Command::Check(user_id("slow")));
    assert_eq!(status, Status::Unknown, "{detail:?}");
    assert!(detail.unwrap().contains("timed out"));
    let timing = timing.expect("running out of Klefki's own time comes with a timing too");
    assert!(timing.took >= Duration::from_secs(3), "{timing:?}");
    assert_eq!(timing.limit, default_limit(&env));
    assert!(timing.timed_out);

    // 40 s instead of 30: Klefki waits 10 s longer, and Claude Code's answer comes.
    std::fs::write(
        env.dir.path().join("cfg").join("settings.json"),
        r#"{"env":{"MCP_TIMEOUT":"40000"}}"#,
    )
    .unwrap();
    let core = start_quiet(config());
    let (status, _, timing, _) = check(&core, Command::Check(user_id("slow")));
    assert_eq!(status, Status::Connected);
    assert_eq!(timing, None);
}

#[test]
fn a_diagnosis_that_timed_out_comes_with_its_timing() {
    let env = slow_env(
        r#",{"args":["--debug","mcp","--debug-file","*","mcp","get","slow"],"stdout_file":"t.txt"}"#,
        &[("t.txt", TIMED_OUT)],
    );
    let core = start_quiet(env.config());
    let (status, _, timing, _) = check(&core, Command::Diagnose(user_id("slow")));
    assert_eq!(status, Status::Failed);
    assert_eq!(timing.unwrap().limit, default_limit(&env));
}
