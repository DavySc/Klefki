//! Engine tests: live status checks at start-up, on request and after edits.
use super::*;
use crate::engine::checks::{BackgroundSlot, Job, JobKind, list_env};

#[test]
fn servers_claude_already_knows_are_rechecked_at_startup_keeping_their_last_known_status() {
    // Bug: Claude's caches can be days old, and a server it called healthy was shown Connected until F5.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "known": http("https://a.example.com"),
            "fresh": {"type": "stdio", "command": "x"}}}),
        r#"{"rules":[{"args":["mcp","get","known"],"stdout_file":"f.txt"},
                    {"args":["mcp","get","fresh"],"stdout_file":"c.txt"},
                    {"args":["--version"]}]}"#,
        &[("c.txt", CONNECTED), ("f.txt", FAILED)],
    );
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-health-cache.json"),
        r#"{"version":1,"servers":{"known":{"status":"healthy"}}}"#,
    )
    .unwrap();
    let core = env.start();
    let events = collect(&core, |e| {
        final_status(e, "fresh").is_some() && final_status(e, "known").is_some()
    });
    assert!(
        matches!(&events[0], Event::Snapshot(s)
            if s.servers.iter().any(|r| r.name == "known" && r.status == Status::Connected)),
        "the cached status is shown at once: {:?}",
        events[0]
    );
    assert_eq!(
        final_status(&events, "known"),
        Some(Status::Failed),
        "the live answer replaces what the cache said"
    );
    assert!(
        !events.iter().any(
            |e| matches!(e, Event::Status { id, status: Status::Checking, .. }
            if id.name == "known")
        ),
        "a known server keeps its last-known status while it is checked, not a blank \"checking\""
    );
    let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    names.sort();
    assert_eq!(names, vec!["fresh", "known"], "each is checked once");
}

#[test]
fn servers_with_nothing_to_show_are_checked_before_the_ones_claude_knows() {
    // One check at a time, so the fake CLI's log is in queue order.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "a-auth": http("https://a.example.com"),
            "b-known": http("https://b.example.com"),
            "c-fresh": {"type": "stdio", "command": "x"},
            "d-off": http("https://d.example.com")},
            "projects": {"/p": {"disabledMcpServers": ["d-off"]}}}),
        r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
        &[("c.txt", CONNECTED)],
    );
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-health-cache.json"),
        r#"{"version":1,"servers":{"b-known":{"status":"healthy"},"d-off":{"status":"healthy"}}}"#,
    )
    .unwrap();
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-needs-auth-cache.json"),
        r#"{"a-auth":{}}"#,
    )
    .unwrap();
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| {
        ["a-auth", "b-known", "c-fresh"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(
        order,
        vec!["c-fresh", "a-auth", "b-known"],
        "the unknown one first, then the known ones; the one switched off is not checked"
    );
}

/// Claude's needs-auth cache, written now for `names`, as Claude Code writes it.
fn recently_needing_auth(env: &Env, names: &[String]) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let entries: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .map(|n| (n.clone(), serde_json::json!({"timestamp": now})))
        .collect();
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-needs-auth-cache.json"),
        serde_json::Value::Object(entries).to_string(),
    )
    .unwrap();
}

#[test]
fn a_server_claude_checked_recently_is_left_alone_until_f5() {
    // Restores the half of the test this slice replaced: F5 checks a server the caches know.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "recent": http("https://a.example.com"),
            "fresh": {"type": "stdio", "command": "x"}}}),
        r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["recent".to_owned()]);
    let core = env.start();
    let events = collect(&core, |e| final_status(e, "fresh").is_some());
    assert!(matches!(&events[0], Event::Snapshot(s)
            if s.servers.iter().any(|r| r.name == "recent" && r.status == Status::NeedsAuth && !r.status_cached)));
    let names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(
        names,
        ["fresh"],
        "Claude found it needs sign-in minutes ago"
    );

    core.send(Command::Refresh { live_all: true });
    // Both answer again: counting the gets after only one has, the other may not have started yet.
    let events = collect(&core, |e| {
        final_status(e, "recent").is_some() && final_status(e, "fresh").is_some()
    });
    assert_eq!(final_status(&events, "recent"), Some(Status::Connected));
    let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    names.sort();
    assert_eq!(names, ["fresh", "fresh", "recent"], "F5 checks every one");
}

#[test]
fn f5_checks_one_at_a_time_still_check_every_one() {
    // F5 with one check at a time still checks every one.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "recent": http("https://a.example.com"),
            "fresh": {"type": "stdio", "command": "x"}}}),
        r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["recent".to_owned()]);
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| final_status(e, "fresh").is_some());
    core.send(Command::Refresh { live_all: true });
    let events = collect(&core, |e| {
        final_status(e, "recent").is_some() && final_status(e, "fresh").is_some()
    });
    assert_eq!(final_status(&events, "recent"), Some(Status::Connected));
    let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    names.sort();
    assert_eq!(names, ["fresh", "fresh", "recent"], "F5 checks every one");
}

fn max_overlap(gets: &[(String, u64, u64)]) -> usize {
    gets.iter()
        .map(|(_, start, _)| {
            gets.iter()
                .filter(|(_, s, e)| s <= start && start < e)
                .count()
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn checks_nobody_asked_for_run_one_at_a_time() {
    // Each is a large Node process: three at once took klefki from 123 MB to about 700 MB at startup.
    let servers: serde_json::Map<String, serde_json::Value> = (0..4)
        .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
        .collect();
    let env = Env::new(
        &serde_json::json!({ "mcpServers": servers }),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":300}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = env.start();
    collect(&core, |e| {
        (0..4).all(|i| final_status(e, &format!("s{i}")).is_some())
    });
    let gets = env.gets();
    assert_eq!(gets.len(), 4);
    assert_eq!(max_overlap(&gets), 1, "{gets:?}");
}

/// What `claude mcp list` prints for `lines` (name, command, status as Claude words it).
fn list_output(lines: &[(&str, &str)]) -> Vec<u8> {
    let mut text = String::from("Checking MCP server health…\n\n");
    for (name, status) in lines {
        text.push_str(&format!("{name}: x - {status}\n"));
    }
    text.into_bytes()
}

fn servers(n: usize) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = (0..n)
        .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
        .collect();
    serde_json::json!({ "mcpServers": map })
}

const LIST_AND_GET: &str = r#"{"rules":[{"args":["--version"]},
    {"args":["mcp","list"],"stdout_file":"list.txt"},
    {"args":["mcp","get","s2"],"stdout_file":"failed.txt"},
    {"args":["mcp","get","s5"],"stdout_file":"failed.txt"},
    {"args":["mcp","get"],"stdout_file":"c.txt"}]}"#;

fn all_checked(core: &Core, n: usize) -> Vec<Event> {
    collect(core, |e| {
        (0..n).all(|i| final_status(e, &format!("s{i}")).is_some())
    })
}

#[test]
fn background_checks_in_one_folder_share_one_claude_list() {
    let list = list_output(&[
        ("s0", "✔ Connected"),
        (
            "s1",
            "✘ Failed to connect — CONNECTION_CLOSED: Connection closed",
        ),
        ("s2", "! Connected · tools fetch failed — no tools"),
        ("s3", "! Needs authentication"),
        ("s4", "✔ Connected"),
        ("s5", "✔ Connected"),
    ]);
    let env = Env::new(
        &servers(6),
        LIST_AND_GET,
        &[
            ("list.txt", &list),
            ("c.txt", CONNECTED),
            ("failed.txt", FAILED),
        ],
    );
    let core = env.start();
    let events = all_checked(&core, 6);
    let said = |n: &str| status_of(&events, n);
    assert_eq!(said("s0"), Some((Status::Connected, None)));
    assert_eq!(
        said("s1"),
        Some((
            Status::Failed,
            Some("CONNECTION_CLOSED: Connection closed".to_owned())
        ))
    );
    assert_eq!(
        said("s2"),
        Some((Status::Degraded, Some("no tools".to_owned())))
    );
    assert_eq!(said("s3").map(|s| s.0), Some(Status::NeedsAuth));
    assert_eq!(said("s5"), Some((Status::Connected, None)));
    assert_eq!(env.lists().len(), 1, "one list for the folder");
    assert!(env.gets().is_empty(), "no get: {:?}", env.gets());
}

#[test]
fn a_server_the_list_does_not_answer_clearly_is_checked_on_its_own() {
    // s2 is on two lines and s5 on none: Claude is asked about each of them, and the list is not asked again.
    let list = list_output(&[
        ("s0", "✔ Connected"),
        ("s1", "✔ Connected"),
        ("s2", "✔ Connected"),
        ("s2", "✔ Connected"),
        ("s3", "✔ Connected"),
        ("s4", "✔ Connected"),
    ]);
    let env = Env::new(
        &servers(6),
        LIST_AND_GET,
        &[
            ("list.txt", &list),
            ("c.txt", CONNECTED),
            ("failed.txt", FAILED),
        ],
    );
    let core = env.start();
    let events = all_checked(&core, 6);
    assert_eq!(final_status(&events, "s0"), Some(Status::Connected));
    assert_eq!(
        final_status(&events, "s2"),
        Some(Status::Failed),
        "its own get"
    );
    assert_eq!(
        final_status(&events, "s5"),
        Some(Status::Failed),
        "its own get"
    );
    let mut asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    asked.sort();
    assert_eq!(asked, ["s2", "s5"]);
    assert_eq!(env.lists().len(), 1);
}

#[test]
fn when_the_list_fails_every_server_is_checked_with_get() {
    // Variant: a list that exits 1, and one that exits 0 with nothing usable, both leave the servers to `get`.
    for scenario in [
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"exit_code":1},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"stdout_file":"nonsense.txt"},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
    ] {
        let env = Env::new(
            &servers(6),
            scenario,
            &[("c.txt", CONNECTED), ("nonsense.txt", b"something else\n")],
        );
        let core = env.start();
        let events = all_checked(&core, 6);
        assert_eq!(final_status(&events, "s3"), Some(Status::Connected));
        assert_eq!(env.gets().len(), 6, "{scenario}");
        assert_eq!(env.lists().len(), 1, "not asked again: {scenario}");
    }
}

#[test]
fn a_list_answer_a_timeout_may_have_caused_is_checked_with_get() {
    // The list runs with a short timeout so a hanging server cannot hold the others back; what it says
    // about a server that ran out of time is not Claude's answer, which `get` gives with the full time.
    let list = list_output(&[
        ("s0", "✔ Connected"),
        ("s1", "✘ Failed to connect — Request timed out"),
        (
            "s2",
            "✘ Failed to connect — MCP server \"[redacted]\" connection timed out after 3000ms",
        ),
        ("s3", "! Connected · tools fetch failed — Request timed out"),
        ("s4", "✔ Connected"),
        ("s5", "✘ Failed to connect"),
    ]);
    let env = Env::new(
        &servers(6),
        LIST_AND_GET,
        &[
            ("list.txt", &list),
            ("c.txt", CONNECTED),
            ("failed.txt", FAILED),
        ],
    );
    let core = env.start();
    let events = all_checked(&core, 6);
    assert_eq!(
        final_status(&events, "s1"),
        Some(Status::Connected),
        "its get"
    );
    assert_eq!(final_status(&events, "s2"), Some(Status::Failed), "its get");
    assert_eq!(
        final_status(&events, "s3"),
        Some(Status::Connected),
        "its get"
    );
    assert_eq!(final_status(&events, "s5"), Some(Status::Failed), "its get");
    let mut asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    asked.sort();
    assert_eq!(asked, ["s1", "s2", "s3", "s5"]);
    assert_eq!(env.lists().len(), 1);
}

#[test]
fn a_server_that_never_connected_in_the_list_is_checked_after_one_that_was_only_slow() {
    // A hanging server's own `get` takes 30 s, so the others come first, though it is first in the queue and
    // is the job that ran the list.
    let list = list_output(&[
        (
            "s0",
            "✘ Failed to connect — MCP server \"s0\" connection timed out after 3000ms",
        ),
        ("s1", "✘ Failed to connect — Request timed out"),
        ("s2", "✔ Connected"),
        ("s3", "✔ Connected"),
        ("s4", "✔ Connected"),
        ("s5", "✔ Connected"),
    ]);
    let env = Env::new(
        &servers(6),
        LIST_AND_GET,
        &[
            ("list.txt", &list),
            ("c.txt", CONNECTED),
            ("failed.txt", FAILED),
        ],
    );
    let core = env.start();
    let events = all_checked(&core, 6);
    assert_eq!(
        final_status(&events, "s2"),
        Some(Status::Connected),
        "from the list"
    );
    assert_eq!(
        final_status(&events, "s0"),
        Some(Status::Connected),
        "its own get"
    );
    let asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(asked, ["s1", "s0"], "the slow one first");
    assert_eq!(env.lists().len(), 1);
}

#[test]
fn the_list_is_asked_for_a_short_timeout_and_a_wide_batch_without_the_connectors() {
    let base = [("FAKE", OsStr::new("1"))];
    let env = list_env(base.to_vec());
    assert!(
        env.contains(&("FAKE", OsStr::new("1"))),
        "the usual environment stays"
    );
    for (key, value) in [
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
        ("MCP_TIMEOUT", "3000"),
        ("MCP_SERVER_CONNECTION_BATCH_SIZE", "10"),
    ] {
        assert!(env.contains(&(key, OsStr::new(value))), "{key}");
    }
}

#[test]
fn a_list_that_hangs_is_ended_and_the_servers_are_checked_with_get() {
    let env = Env::new(
        &servers(6),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"hang":true},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let mut config = env.config();
    config.check_timeout = Duration::from_millis(400);
    let core = start_with(config, || {});
    let events = all_checked(&core, 6);
    assert_eq!(final_status(&events, "s4"), Some(Status::Connected));
    assert_eq!(env.gets().len(), 6);
    assert_eq!(env.lists().len(), 1, "not asked again");
}

#[test]
fn servers_a_list_could_not_answer_keep_their_order_and_the_one_that_ran_it_its_place() {
    // Regression (Windows CI): the job that ran the list went to the back, behind servers that were after it, so
    // a server on screen could be checked before an earlier one. The list fails slowly; the servers on screen
    // are named while it runs.
    let env = Env::new(
        &servers(5),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"delay_ms":600,"exit_code":1},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = env.start();
    std::thread::sleep(Duration::from_millis(250));
    core.send(Command::Visible(vec![
        crate::model::ConnectionId::new(Scope::User, None, "s4"),
        crate::model::ConnectionId::new(Scope::User, None, "s3"),
    ]));
    all_checked(&core, 5);
    let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(order, ["s3", "s4", "s0", "s1", "s2"], "{order:?}");
}

#[test]
fn a_few_servers_are_checked_one_by_one() {
    // A list starts every server of the folder; for a few, separate gets are not slower and start only those.
    let env = Env::new(
        &servers(3),
        LIST_AND_GET,
        &[
            ("list.txt", b""),
            ("c.txt", CONNECTED),
            ("failed.txt", FAILED),
        ],
    );
    let core = env.start();
    all_checked(&core, 3);
    assert_eq!(env.gets().len(), 3);
    assert!(env.lists().is_empty());
}

#[test]
fn a_check_the_user_asks_for_is_always_a_get() {
    let list = list_output(
        &(0..6)
            .map(|i| (["s0", "s1", "s2", "s3", "s4", "s5"][i], "✔ Connected"))
            .collect::<Vec<_>>(),
    );
    let env = Env::new(
        &servers(6),
        LIST_AND_GET,
        &[
            ("list.txt", &list),
            ("c.txt", CONNECTED),
            ("failed.txt", FAILED),
        ],
    );
    let core = env.start();
    all_checked(&core, 6);
    core.send(Command::Check(user_id("s5")));
    let events = collect(&core, |e| final_status(e, "s5").is_some());
    assert_eq!(
        final_status(&events, "s5"),
        Some(Status::Failed),
        "its get answered"
    );
    assert_eq!(env.lists().len(), 1);
    assert_eq!(env.gets().len(), 1);
}

#[test]
fn each_folder_gets_its_own_list() {
    let env = Env::new(
        &serde_json::json!({}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","list"],"stdout_file":"list.txt"},{"args":["mcp","get"],"stdout_file":"failed.txt"}]}"#,
        &[
            (
                "list.txt",
                &list_output(&[
                    ("a0", "✔ Connected"),
                    ("a1", "✔ Connected"),
                    ("a2", "✔ Connected"),
                    ("a3", "✔ Connected"),
                ]),
            ),
            ("failed.txt", FAILED),
        ],
    );
    // Two projects, four local servers each, named alike so the one list text answers both.
    let (p1, p2) = (env.dir.path().join("p1"), env.dir.path().join("p2"));
    std::fs::create_dir_all(&p1).unwrap();
    std::fs::create_dir_all(&p2).unwrap();
    let local: serde_json::Map<String, serde_json::Value> = (0..4)
        .map(|i| (format!("a{i}"), serde_json::json!({"command": "x"})))
        .collect();
    std::fs::write(
        env.dir.path().join("cfg").join(".claude.json"),
        serde_json::json!({"projects": {
            p1.to_str().unwrap(): {"mcpServers": local},
            p2.to_str().unwrap(): {"mcpServers": local}}})
        .to_string(),
    )
    .unwrap();
    let core = env.start();
    collect(&core, |e| {
        e.iter()
            .filter(|x| matches!(x, Event::Status { status, .. } if *status != Status::Checking))
            .count()
            >= 8
    });
    let mut folders = env.lists();
    folders.sort();
    assert_eq!(folders.len(), 2, "{folders:?}");
    assert!(
        folders[0].ends_with("p1") && folders[1].ends_with("p2"),
        "{folders:?}"
    );
    assert!(env.gets().is_empty(), "{:?}", env.gets());
}

#[test]
fn a_check_the_user_asks_for_does_not_wait_for_the_background_ones() {
    let servers: serde_json::Map<String, serde_json::Value> = (0..3)
        .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
        .chain([("mine".to_owned(), serde_json::json!({"command": "x"}))])
        .collect();
    let env = Env::new(
        &serde_json::json!({ "mcpServers": servers }),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["mine".to_owned()]);
    let core = env.start();
    collect(&core, |e| {
        e.iter().any(|x| {
            matches!(
                x,
                Event::Status {
                    status: Status::Checking,
                    ..
                }
            )
        })
    });
    core.send(Command::Check(user_id("mine")));
    collect(&core, |e| final_status(e, "mine").is_some());
    let gets = env.gets();
    let done = |n: &str| gets.iter().filter(|g| g.0 == n).count();
    assert_eq!(done("mine"), 1);
    assert!(
        (0..3).filter(|i| done(&format!("s{i}")) == 1).count() < 3,
        "it ran beside the background checks, not after them: {gets:?}"
    );
}

// ---- checks across edits and refreshes

/// The connection of the user server `name`.
fn user_connection(name: &str) -> crate::model::ConnectionId {
    user_id(name).connection
}

/// The events after the first snapshot in which the user server `name`'s target is `target`.
fn after_target<'a>(events: &'a [Event], name: &str, target: &str) -> Option<&'a [Event]> {
    let at = events.iter().position(|e| {
        matches!(e, Event::Snapshot(s)
            if s.servers.iter().any(|r| r.name == name && r.target == target))
    })?;
    Some(&events[at + 1..])
}

#[test]
fn an_edited_server_is_checked_again() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"s": {"command": "old"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED), ("f.txt", FAILED)],
    );
    let core = env.start();
    collect(&core, |e| final_status(e, "s") == Some(Status::Connected));
    // The new definition fails; the old verdict must not stay on it.
    write_scenario(
        env.dir.path(),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"f.txt"}]}"#,
        &[],
    );
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"s": {"command": "new"}}}),
    );
    let events = collect(&core, |e| {
        after_target(e, "s", "new").is_some_and(|a| final_status(a, "s").is_some())
    });
    let after = after_target(&events, "s", "new").unwrap();
    assert_eq!(final_status(after, "s"), Some(Status::Failed), "{after:#?}");
    assert_eq!(env.gets().len(), 2, "{:?}", env.gets());
}

#[test]
fn a_server_whose_environment_alone_changed_is_checked_again() {
    // The variant: nothing the row shows changed, only what the server is started with.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"s": {"command": "x", "env": {"A": "1"}}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED), ("f.txt", FAILED)],
    );
    let core = env.start();
    collect(&core, |e| final_status(e, "s") == Some(Status::Connected));
    write_scenario(
        env.dir.path(),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"f.txt"}]}"#,
        &[],
    );
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"s": {"command": "x", "env": {"A": "2"}}}}),
    );
    collect(&core, |e| final_status(e, "s") == Some(Status::Failed));
    assert_eq!(env.gets().len(), 2, "{:?}", env.gets());
}

#[test]
fn a_server_left_alone_by_an_edit_is_not_checked_again() {
    // The other side: an edit of one server does not re-check the others.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"s": {"command": "x"}, "t": {"command": "y"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = env.start();
    collect(&core, |e| {
        final_status(e, "s").is_some() && final_status(e, "t").is_some()
    });
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"s": {"command": "x2"}, "t": {"command": "y"}}}),
    );
    collect(&core, |e| {
        after_target(e, "s", "x2").is_some_and(|a| final_status(a, "s").is_some())
    });
    let gets: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(gets.iter().filter(|n| *n == "t").count(), 1, "{gets:?}");
    assert_eq!(gets.iter().filter(|n| *n == "s").count(), 2, "{gets:?}");
}

#[test]
fn a_check_running_while_its_server_is_edited_does_not_answer_for_the_new_definition() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"s": {"command": "old"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1500}]}"#,
        &[("c.txt", CONNECTED), ("f.txt", FAILED)],
    );
    let core = env.start();
    collect(&core, |e| {
        status_of(e, "s").is_some_and(|s| s.0 == Status::Checking)
    });
    write_scenario(
        env.dir.path(),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"f.txt"}]}"#,
        &[],
    );
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"s": {"command": "new"}}}),
    );
    let mut events = collect(&core, |e| {
        after_target(e, "s", "new").is_some_and(|a| final_status(a, "s").is_some())
    });
    // Until the first check has ended, so its late answer would be among the events.
    let deadline = Instant::now() + WAIT;
    while env.gets().iter().any(|g| g.2 == u64::MAX) {
        assert!(Instant::now() < deadline, "{:?}", env.gets());
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    while let Some(e) = core.try_recv() {
        events.push(e);
    }
    let after = after_target(&events, "s", "new").unwrap();
    assert!(
        !after.iter().any(|e| matches!(e,
            Event::Status { id, status: Status::Connected, .. } if id.connection == user_connection("s"))),
        "the old definition's verdict: {after:#?}"
    );
    assert_eq!(final_status(after, "s"), Some(Status::Failed), "{after:#?}");
}

#[test]
fn a_check_the_user_asked_for_still_answers_after_f5() {
    // It was running when the refresh came: its answer is about a server that did not change.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"mine": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["mine".to_owned()]);
    let core = env.start();
    core.send(Command::Check(user_id("mine")));
    collect(&core, |e| {
        status_of(e, "mine").is_some_and(|s| s.0 == Status::Checking)
    });
    core.send(Command::Refresh { live_all: false });
    collect(&core, |e| {
        final_status(e, "mine") == Some(Status::Connected)
    });
    assert_eq!(env.gets().len(), 1, "{:?}", env.gets());
}

#[test]
fn a_check_still_running_is_announced_again_after_a_refresh() {
    // Cross-review of #14: the app drops "Checking…" on every snapshot, so a check that goes on would look done.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"mine": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1500}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["mine".to_owned()]);
    let core = env.start();
    core.send(Command::Check(user_id("mine")));
    collect(&core, |e| {
        status_of(e, "mine").is_some_and(|s| s.0 == Status::Checking)
    });
    core.send(Command::Refresh { live_all: false });
    let events = collect(&core, |e| final_status(e, "mine").is_some());
    let snapshot = events
        .iter()
        .rposition(|e| matches!(e, Event::Snapshot(_)))
        .expect("the refresh's snapshot");
    assert_eq!(
        status_of(&events[snapshot..], "mine").map(|s| s.0),
        Some(Status::Connected)
    );
    assert!(
        events[snapshot..].iter().any(|e| matches!(e,
            Event::Status { id, status: Status::Checking, .. } if id.name == "mine")),
        "{:#?}",
        &events[snapshot..]
    );
}

#[test]
fn a_check_of_a_server_since_edited_is_not_announced_again() {
    // Cross-review of #14, round 2: its answer will not come, so "Checking…" said again would stay for good.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"mine": {"command": "old"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1500}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = start_with(
        EngineConfig {
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    core.send(Command::Check(user_id("mine")));
    collect(&core, |e| {
        status_of(e, "mine").is_some_and(|s| s.0 == Status::Checking)
    });
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"mine": {"command": "new"}}}),
    );
    collect(&core, |e| {
        after_target(e, "mine", "new").is_some_and(|a| status_of(a, "mine").is_some())
    });
    // An unrelated reload while the stale check still runs.
    core.send(Command::Refresh { live_all: false });
    let deadline = Instant::now() + WAIT;
    while env.gets().iter().any(|g| g.2 == u64::MAX) {
        assert!(Instant::now() < deadline, "{:?}", env.gets());
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    let mut events = Vec::new();
    while let Some(e) = core.try_recv() {
        events.push(e);
    }
    assert!(
        events.iter().any(|e| matches!(e, Event::Snapshot(_))),
        "the refresh came after the edit: {events:#?}"
    );
    assert_ne!(
        status_of(&events, "mine").map(|s| s.0),
        Some(Status::Checking),
        "{events:#?}"
    );
}

#[test]
fn a_check_the_user_asked_for_of_a_server_since_removed_is_not_run() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"busy": {"command": "x"}, "gone": {"command": "y"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["gone".to_owned()]);
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            watch_interval: None,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| {
        status_of(e, "busy").is_some_and(|s| s.0 == Status::Checking)
    });
    core.send(Command::Check(user_id("gone")));
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"busy": {"command": "x"}}}),
    );
    core.send(Command::Refresh { live_all: false });
    collect(&core, |e| final_status(e, "busy").is_some());
    std::thread::sleep(Duration::from_millis(500));
    let gets: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert!(!gets.iter().any(|n| n == "gone"), "{gets:?}");
}

#[test]
fn an_edited_server_that_cannot_be_checked_says_why() {
    // Cross-review of #14: the reason it was not checked stays, not a vaguer "changed since its last check".
    let env = quiet_env(&serde_json::json!({
        "projects": {"/work/missing": {"mcpServers": {"loc": {"command": "old"}}}}
    }));
    let core = env.start();
    collect(&core, |e| status_of(e, "loc").is_some());
    write_config(
        &env,
        &serde_json::json!({"projects": {"/work/missing": {"mcpServers": {"loc": {"command": "new"}}}}}),
    );
    let mut events = collect(&core, |e| {
        after_target(e, "loc", "new").is_some_and(|a| status_of(a, "loc").is_some())
    });
    // Anything said after it would be among these.
    std::thread::sleep(Duration::from_millis(300));
    while let Some(e) = core.try_recv() {
        events.push(e);
    }
    let after = after_target(&events, "loc", "new").unwrap();
    assert_eq!(
        status_of(after, "loc"),
        Some((
            Status::Unknown,
            Some("not checked: the project folder was not found".to_owned())
        )),
        "{after:#?}"
    );
}

#[test]
fn a_check_the_user_asked_for_that_waits_its_turn_survives_f5() {
    // The variant: it had not started yet. A server that is off is never queued again by a refresh.
    let env = Env::new(
        &serde_json::json!({}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":800}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let project = env.dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let key = project.to_string_lossy().replace('\\', "/");
    write_config(
        &env,
        &serde_json::json!({
            "mcpServers": {"busy": {"command": "x"}},
            "projects": {key.clone(): {"mcpServers": {"off": {"command": "y"}}, "disabledMcpServers": ["off"]}}
        }),
    );
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| {
        status_of(e, "busy").is_some_and(|s| s.0 == Status::Checking)
    });
    core.send(Command::Check(local_id("off", &key)));
    core.send(Command::Refresh { live_all: false });
    collect(&core, |e| final_status(e, "off") == Some(Status::Connected));
    assert_eq!(
        env.gets().iter().filter(|g| g.0 == "off").count(),
        1,
        "{:?}",
        env.gets()
    );
}

#[test]
fn background_checks_of_servers_on_screen_go_first() {
    let servers: serde_json::Map<String, serde_json::Value> = (0..5)
        .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
        .collect();
    let env = Env::new(
        &serde_json::json!({ "mcpServers": servers }),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":300}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = env.start();
    core.send(Command::Visible(vec![
        crate::model::ConnectionId::new(Scope::User, None, "s4"),
        crate::model::ConnectionId::new(Scope::User, None, "s3"),
    ]));
    collect(&core, |e| {
        (0..5).all(|i| final_status(e, &format!("s{i}")).is_some())
    });
    let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    // One may already have started before the list arrived.
    let at = |n: &str| order.iter().position(|o| o == n).unwrap();
    assert!(at("s3") <= 2 && at("s4") <= 2, "{order:?}");
    assert!(
        at("s3") < at("s4"),
        "on screen in their own order: {order:?}"
    );
}

#[test]
fn check_now_replaces_the_queued_background_check_of_that_server() {
    // Re-review: the background job stayed queued, so the server was checked twice and the later answer won.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "a": {"command": "x"}, "b": {"command": "x"}, "t": http("https://t.example.com")}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":400}]}"#,
        &[("c.txt", CONNECTED)],
    );
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-health-cache.json"),
        r#"{"version":1,"servers":{"t":{"status":"failed"}}}"#,
    )
    .unwrap();
    let core = env.start();
    collect(&core, |e| {
        e.iter().any(|x| {
            matches!(
                x,
                Event::Status {
                    status: Status::Checking,
                    ..
                }
            )
        })
    });
    core.send(Command::Check(user_id("t")));
    collect(&core, |e| {
        ["a", "b"].iter().all(|n| final_status(e, n).is_some())
    });
    std::thread::sleep(Duration::from_millis(600));
    let asked = env.gets().iter().filter(|g| g.0 == "t").count();
    assert_eq!(asked, 1, "{:?}", env.gets());
}

#[test]
fn a_server_switched_on_twice_is_queued_once() {
    // Variant: reloads before its check ran must not queue it twice.
    let config = |off: bool| {
        serde_json::json!({
            "mcpServers": {"slow": {"command": "x"}, "t": {"command": "x"}},
            "projects": {"/p": {"disabledMcpServers": if off { vec!["t"] } else { vec![] }}}
        })
    };
    let env = Env::new(
        &config(true),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","slow"],"stdout_file":"c.txt","delay_ms":2500},
                    {"args":["mcp","get","t"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = env.start();
    collect(&core, |e| {
        e.iter().any(|x| {
            matches!(
                x,
                Event::Status {
                    status: Status::Checking,
                    ..
                }
            )
        })
    });
    for off in [false, true, false] {
        write_config(&env, &config(off));
        collect(&core, |e| !snapshots(e).is_empty());
    }
    collect(&core, |e| final_status(e, "t").is_some());
    std::thread::sleep(Duration::from_millis(300));
    let asked = env.gets().iter().filter(|g| g.0 == "t").count();
    assert_eq!(asked, 1, "{:?}", env.gets());
}

#[test]
fn f5_checks_the_servers_with_nothing_to_show_first() {
    // Re-review: F5 jobs lost the unknown-first order.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "k0": http("https://a.example.com"), "k1": http("https://b.example.com"),
            "k2": http("https://c.example.com"), "z": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    recently_needing_auth(&env, &["k0".to_owned(), "k1".to_owned(), "k2".to_owned()]);
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| final_status(e, "z").is_some());
    core.send(Command::Refresh { live_all: true });
    collect(&core, |e| {
        ["k0", "k1", "k2", "z"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(order, ["z", "z", "k0", "k1", "k2"]);
}

#[test]
fn checks_asked_for_one_after_another_run_in_the_order_asked() {
    // Performance review: `push_now` put each new ask in front, so klefki-cli's checks ran in reverse.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "s": {"command": "x"}, "k0": {"command": "x"}, "k1": {"command": "x"}, "k2": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","s"],"stdout_file":"c.txt","delay_ms":300},
                    {"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| !snapshots(e).is_empty());
    for name in ["s", "k0", "k1", "k2"] {
        core.send(Command::Check(user_id(name)));
    }
    collect(&core, |e| {
        ["s", "k0", "k1", "k2"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(order, ["s", "k0", "k1", "k2"]);
}

#[test]
fn a_server_asked_for_again_while_waiting_moves_to_the_end_of_the_asks() {
    // Variant: the second ask replaces the first, behind the asks made in between.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "s": {"command": "x"}, "k0": {"command": "x"}, "k1": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","s"],"stdout_file":"c.txt","delay_ms":300},
                    {"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = start_with(
        EngineConfig {
            max_checks: 1,
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| !snapshots(e).is_empty());
    for name in ["s", "k0", "k1", "k0"] {
        core.send(Command::Check(user_id(name)));
    }
    collect(&core, |e| {
        ["s", "k0", "k1"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    std::thread::sleep(Duration::from_millis(200));
    let order: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(order, ["s", "k1", "k0"]);
}

#[test]
fn a_background_check_that_panics_gives_its_slot_back() {
    let queue = Mutex::new(Queue::default());
    let wake = Condvar::new();
    lock(&queue).background_running = 1;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _slot = BackgroundSlot {
            queue: &queue,
            wake: &wake,
        };
        panic!("a check went wrong");
    }));
    assert!(caught.is_err());
    assert_eq!(lock(&queue).background_running, 0);
}

#[test]
fn a_known_server_switched_on_from_outside_is_checked() {
    // Cross-review of this slice: only unknown servers were checked after a change.
    let config = |off: bool| {
        serde_json::json!({
            "mcpServers": {"known": http("https://a.example.com")},
            "projects": {"/p": {"disabledMcpServers": if off { vec!["known"] } else { vec![] }}}
        })
    };
    let env = Env::new(
        &config(true),
        r#"{"rules":[{"args":["mcp","get"],"stdout_file":"f.txt"},{"args":["--version"]}]}"#,
        &[("f.txt", FAILED)],
    );
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-health-cache.json"),
        r#"{"version":1,"servers":{"known":{"status":"healthy"}}}"#,
    )
    .unwrap();
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    assert!(env.gets().is_empty(), "it is off");
    write_config(&env, &config(false));
    let events = collect(&core, |e| final_status(e, "known").is_some());
    assert_eq!(final_status(&events, "known"), Some(Status::Failed));
    assert!(
        !events.iter().any(
            |e| matches!(e, Event::Status { id, status: Status::Checking, .. }
            if id.name == "known")
        ),
        "its last-known status stays while it is checked"
    );
}

#[test]
fn a_known_server_that_cannot_be_checked_keeps_its_last_known_status() {
    // Cross-review: a refused check sent "not checked" over the cache's status. A variant: the unknown one still
    // says why it is not checked.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "a&b": http("https://a.example.com"),
            "c&d": http("https://c.example.com"),
            "ok": http("https://b.example.com")}}),
        r#"{"rules":[{"args":["mcp","get"],"stdout_file":"c.txt"},{"args":["--version"]}]}"#,
        &[("c.txt", CONNECTED)],
    );
    std::fs::write(
        env.dir.path().join("cfg").join("mcp-health-cache.json"),
        r#"{"version":1,"servers":{"a&b":{"status":"healthy"}}}"#,
    )
    .unwrap();
    let core = env.start();
    let events = collect(&core, |e| {
        final_status(e, "ok").is_some() && final_status(e, "c&d").is_some()
    });
    assert_eq!(status_of(&events, "a&b"), None, "{events:?}");
    let (status, detail) = status_of(&events, "c&d").unwrap();
    assert_eq!(status, Status::Unknown);
    assert!(detail.unwrap().contains("not checked"));
}

#[test]
fn project_servers_are_only_checked_on_request_and_run_in_their_folder() {
    let env = Env::new(
        &serde_json::json!({}),
        r#"{"rules":[{"args":["--version"]},
        {"args":["mcp","get","docs"],"stdout_file":"p.txt"}]}"#,
        &[("p.txt", PENDING)],
    );
    let project = env.dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"docs":{"command":"node"}}}"#,
    )
    .unwrap();
    std::fs::write(
        env.dir.path().join("cfg").join(".claude.json"),
        serde_json::json!({"projects": {project.to_string_lossy(): {}}}).to_string(),
    )
    .unwrap();
    let core = env.start();
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    std::thread::sleep(Duration::from_millis(600));
    assert!(env.gets().is_empty(), "nothing runs until the user asks");
    let Event::Snapshot(snap) = &events[0] else {
        panic!()
    };
    let row = snap
        .servers
        .iter()
        .find(|r| r.scope == Scope::Project)
        .unwrap();

    core.send(Command::Check(row.id()));
    let events = collect(&core, |e| final_status(e, "docs").is_some());
    assert_eq!(final_status(&events, "docs"), Some(Status::PendingApproval));
    let log = std::fs::read_to_string(&env.log).unwrap();
    let start: serde_json::Value = log
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|l| l["event"] == "start" && l["argv"][2] == "docs")
        .unwrap();
    assert_eq!(
        std::path::Path::new(start["cwd"].as_str().unwrap()).file_name(),
        project.file_name()
    );
}

#[test]
fn a_plugins_servers_are_never_checked_not_even_on_request() {
    // Decision 008: their names (`plugin:p:s`) are not ones Klefki passes to `claude`, and they have no file key.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"ok": {"command": "x"}}, "projects": {"/p": {}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let cfg = env.dir.path().join("cfg");
    let root = cfg.join("plugins").join("cache").join("p");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers":{"web":{"type":"http","url":"https://w.example.com"},"cmd":{"command":"y"}}}"#,
    )
    .unwrap();
    std::fs::write(
        cfg.join("plugins").join("installed_plugins.json"),
        serde_json::json!({"plugins": {"p@m": [{"scope": "user", "installPath": root.to_string_lossy()}]}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        cfg.join("settings.json"),
        r#"{"enabledPlugins":{"p@m":true}}"#,
    )
    .unwrap();
    let core = env.start();
    let events = collect(&core, |e| final_status(e, "ok").is_some());
    let snap = events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => Some(Arc::clone(s)),
            _ => None,
        })
        .unwrap();
    let plugin_rows: Vec<&ServerRow> = snap
        .servers
        .iter()
        .filter(|r| r.scope == Scope::Plugin)
        .collect();
    assert_eq!(plugin_rows.len(), 2, "{:#?}", snap.servers);
    for row in &plugin_rows {
        core.send(Command::Check(row.id()));
        core.send(Command::Diagnose(row.id()));
    }
    core.send(Command::Check(ServerId::new(Scope::User, None, "ok")));
    let later = collect(&core, |e| final_status(e, "ok").is_some());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        env.gets().into_iter().map(|g| g.0).collect::<Vec<_>>(),
        ["ok", "ok"],
        "only the user server ran"
    );
    // Not even a "not checked": nothing was tried.
    assert!(
        !events.iter().chain(&later).any(|e| matches!(e,
            Event::Status { id, .. } | Event::Diagnosis { id, .. } if id.scope == Scope::Plugin)),
        "{events:?} {later:?}"
    );
}

#[test]
fn a_server_claude_code_does_not_load_is_never_checked() {
    // Decision 009: the organisation's server of the same name answers to the name, not this one.
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"ok": {"command": "x"}, "mine": {"command": "y"}}, "projects": {"/p": {}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let org = env.dir.path().join("org");
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(
        org.join("managed-settings.json"),
        r#"{"managedMcpServers":{"mine":{"type":"http","url":"https://org.example.com/mcp"}}}"#,
    )
    .unwrap();
    let core = env.start();
    let events = collect(&core, |e| final_status(e, "ok").is_some());
    core.send(Command::Check(ServerId::new(Scope::User, None, "mine")));
    let later = collect(&core, |e| status_of(e, "mine").is_some());
    assert_eq!(
        env.gets().into_iter().map(|g| g.0).collect::<Vec<_>>(),
        ["ok"],
        "only the server Claude Code loads ran"
    );
    let said = status_of(&later, "mine").and_then(|(_, detail)| detail);
    assert!(
        said.as_deref().is_some_and(|d| d.contains("does not load")),
        "{events:?} {later:?}"
    );
}

#[test]
fn a_name_that_is_not_safe_never_reaches_the_cli() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "a&b": {"command": "x"}, "--help": {"command": "x"}, "ok": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","ok"],"stdout_file":"c.txt"}]}"#,
        &[("c.txt", CONNECTED)],
    );
    let core = env.start();
    let events = collect(&core, |e| {
        ["a&b", "--help", "ok"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    assert_eq!(final_status(&events, "ok"), Some(Status::Connected));
    for bad in ["a&b", "--help"] {
        let (status, detail) = status_of(&events, bad).unwrap();
        assert_eq!(status, Status::Unknown, "{bad}");
        assert!(detail.unwrap().contains("not checked"), "{bad}");
    }
    let asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
    assert_eq!(
        asked,
        vec!["ok"],
        "only the valid name was ever passed to the CLI"
    );
}

#[test]
fn at_most_three_checks_run_at_once() {
    // Each stub run sleeps a full second so that a slow process start under load cannot hide the overlap.
    let servers: serde_json::Map<String, serde_json::Value> = (0..6)
        .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
        .collect();
    let env = Env::new(
        &serde_json::json!({ "mcpServers": servers }),
        r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":1000}]}"#,
        &[("c.txt", CONNECTED)],
    );
    // Claude checked them all just now, so startup checks none; the pool is used when the user asks (F5).
    // (This test used to count the startup checks, which now run one at a time.)
    recently_needing_auth(&env, &(0..6).map(|i| format!("s{i}")).collect::<Vec<_>>());
    let core = env.start();
    collect(&core, |e| !snapshots(e).is_empty());
    assert!(env.gets().is_empty(), "nothing is checked at startup");
    core.send(Command::Refresh { live_all: true });
    collect(&core, |e| {
        (0..6).all(|i| final_status(e, &format!("s{i}")).is_some())
    });
    let gets = env.gets();
    assert_eq!(gets.len(), 6);
    let max_overlap = gets
        .iter()
        .map(|(_, start, _)| {
            gets.iter()
                .filter(|(_, s, e)| s <= start && start < e)
                .count()
        })
        .max()
        .unwrap();
    assert!(max_overlap <= 3, "{max_overlap} checks overlapped");
    assert!(
        max_overlap >= 2,
        "the pool should actually run in parallel, saw {max_overlap}"
    );
}

#[test]
fn cli_failures_become_unknown_with_a_reason() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "missing": {"command": "x"}, "slow": {"command": "x"}, "garbage": {"command": "x"}}}),
        r#"{"rules":[{"args":["--version"]},
            {"args":["mcp","get","missing"],"stderr_file":"nf.txt","exit_code":1},
            {"args":["mcp","get","slow"],"hang":true},
            {"args":["mcp","get","garbage"],"stdout_file":"g.txt"}]}"#,
        &[
            ("nf.txt", b"No MCP server named \"missing\"."),
            ("g.txt", b"something completely different\n"),
        ],
    );
    let mut config = env.config();
    // Roomy on purpose: with many tests starting processes at once, a normal start can take a second.
    config.check_timeout = Duration::from_secs(3);
    let core = start_with(config, || {});
    let events = collect(&core, |e| {
        ["missing", "slow", "garbage"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    for (name, needle) in [
        ("missing", "No MCP server named"),
        ("slow", "timed out"),
        ("garbage", "unrecognised"),
    ] {
        let (status, detail) = status_of(&events, name).unwrap();
        assert_eq!(status, Status::Unknown, "{name}");
        let detail = detail.unwrap_or_default();
        assert!(detail.contains(needle), "{name}: {detail:?}");
    }
}

/// What an edited server that is not checked again says after `reread` read the files: background checks are off.
fn unchecked_edit_says(
    watch: bool,
    reread: impl FnOnce(&Core),
) -> Option<(Status, Option<String>)> {
    let env = quiet_env(&serde_json::json!({"mcpServers": {"mine": {"command": "old"}}}));
    let core = start_with(
        EngineConfig {
            background_checks: false,
            watch_interval: if watch {
                env.config().watch_interval
            } else {
                None
            },
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| !snapshots(e).is_empty());
    write_config(
        &env,
        &serde_json::json!({"mcpServers": {"mine": {"command": "new"}}}),
    );
    reread(&core);
    let mut events = collect(&core, |e| after_target(e, "mine", "new").is_some());
    // Anything said after it would be among these.
    std::thread::sleep(Duration::from_millis(300));
    while let Some(e) = core.try_recv() {
        events.push(e);
    }
    status_of(after_target(&events, "mine", "new").unwrap(), "mine")
}

#[test]
fn an_edited_server_not_checked_again_no_longer_claims_a_verdict_after_a_refresh() {
    assert_eq!(
        unchecked_edit_says(false, |core| core
            .send(Command::Refresh { live_all: false })),
        Some((
            Status::Unknown,
            Some("changed since its last check".to_owned())
        ))
    );
}

#[test]
fn an_edited_server_not_checked_again_no_longer_claims_a_verdict_when_the_file_changes() {
    assert_eq!(
        unchecked_edit_says(true, |_| {}),
        Some((
            Status::Unknown,
            Some("changed since its last check".to_owned())
        ))
    );
}

#[test]
fn only_the_waiting_background_checks_of_edited_servers_are_dropped() {
    let job = |name: &str, kind, background| Job {
        id: user_id(name),
        name: crate::model::ServerName::parse(name).unwrap(),
        cwd: PathBuf::new(),
        kind,
        announce: false,
        background,
        now: false,
        alone: false,
    };
    let mut queue = Queue {
        jobs: [
            job("edited", JobKind::Check, true),
            job("edited", JobKind::Check, false),
            job("edited", JobKind::Diagnose, true),
            job("other", JobKind::Check, true),
        ]
        .into(),
        ..Queue::default()
    };
    queue.drop_background_checks(&[user_id("edited").connection].into());
    let left: Vec<_> = queue
        .jobs
        .iter()
        .map(|j| (j.id.name.as_str(), j.kind == JobKind::Check, j.background))
        .collect();
    assert_eq!(
        left,
        [
            ("edited", true, false),
            ("edited", false, true),
            ("other", true, true),
        ]
    );
}
