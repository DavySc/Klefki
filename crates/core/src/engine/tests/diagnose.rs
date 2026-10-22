//! Engine tests: hints and Diagnose.
use super::*;

fn hint_of(events: &[Event], name: &str) -> Option<Hint> {
    events.iter().rev().find_map(|e| match e {
        Event::Status { id, hint, .. } if id.name == name => Some(hint.clone()),
        _ => None,
    })?
}

fn pointing_at(focus: Focus) -> Hint {
    Hint {
        title: "t".into(),
        code: None,
        help: Vec::new(),
        points_at: Some((focus, "label".into())),
    }
}

fn spanned<'a>(focus: Focus, target: &'a str, said: Option<&str>) -> Option<&'a str> {
    pointing_at(focus)
        .span(target, said)
        .map(|range| &target[range])
}

#[test]
fn a_hint_points_at_the_part_of_the_url_it_is_about() {
    let url = "https://acme-prod.example.com/gateway/mcp";
    assert_eq!(
        spanned(Focus::Host, url, None),
        Some("acme-prod.example.com")
    );
    assert_eq!(
        spanned(Focus::Address, url, None),
        Some("acme-prod.example.com")
    );
    assert_eq!(spanned(Focus::Path, url, None), Some("/gateway/mcp"));
    // Variants: a port, an IPv6 address, user info, a hidden query, no path at all.
    let port = "http://127.0.0.1:9/mcp";
    assert_eq!(spanned(Focus::Host, port, None), Some("127.0.0.1"));
    assert_eq!(spanned(Focus::Address, port, None), Some("127.0.0.1:9"));
    assert_eq!(
        spanned(Focus::Host, "http://[::1]:8080/mcp", None),
        Some("[::1]")
    );
    assert_eq!(
        spanned(Focus::Address, "http://[::1]:8080/mcp", None),
        Some("[::1]:8080")
    );
    assert_eq!(
        spanned(Focus::Host, "https://me@host.example/x", None),
        Some("host.example")
    );
    assert_eq!(
        spanned(Focus::Path, "https://host.example/mcp?…", None),
        Some("/mcp")
    );
    assert_eq!(spanned(Focus::Path, "https://host.example", None), None);
    // A URL focus never points into a command line, nor a command focus into a URL.
    assert_eq!(spanned(Focus::Host, "npx -y pkg", None), None);
    assert_eq!(spanned(Focus::Command, url, None), None);
}

#[test]
fn a_hint_points_at_the_program_even_when_its_path_has_spaces() {
    assert_eq!(spanned(Focus::Command, "npx -y pkg", None), Some("npx"));
    assert_eq!(
        spanned(
            Focus::Command,
            "npx -y pkg",
            Some("Error: spawn npx ENOENT")
        ),
        Some("npx")
    );
    let spaced = r"C:\Program Files\nodejs\node.exe server.js --port 9";
    assert_eq!(
        spanned(
            Focus::Command,
            spaced,
            Some(
                r"'C:\Program Files\nodejs\node.exe' is not recognized as an internal or external command"
            )
        ),
        Some(r"C:\Program Files\nodejs\node.exe")
    );
    // Without Claude naming it, only the first word is certain.
    assert_eq!(spanned(Focus::Command, spaced, None), Some(r"C:\Program"));
    let none = Hint {
        points_at: None,
        ..pointing_at(Focus::Host)
    };
    assert_eq!(none.span("https://h.example/mcp", None), None);
}

#[test]
fn a_hint_is_named_by_the_code_it_was_reported_with() {
    let enotfound = hint(
        HintKind::HostNotFound,
        "getaddrinfo ENOTFOUND acme.example.com",
    );
    assert_eq!(enotfound.code.as_deref(), Some("ENOTFOUND"));
    let again = hint(
        HintKind::HostNotFound,
        "getaddrinfo EAI_AGAIN acme.example.com",
    );
    assert_eq!(again.code.as_deref(), Some("EAI_AGAIN"));
    // Whole words only: a host or port containing the digits is not a status code.
    let port = hint(
        HintKind::Forbidden,
        "forbidden by http://h.example:14030/mcp",
    );
    assert_eq!(port.code, None);
    assert_eq!(
        hint(HintKind::Forbidden, "HTTP 403 Forbidden")
            .code
            .as_deref(),
        Some("403")
    );
    assert_eq!(
        hint(HintKind::HostNotFound, "could not resolve host").code,
        None
    );
    assert_eq!(enotfound.help.len(), HintKind::HostNotFound.help().len());
    assert_eq!(
        enotfound.points_at,
        Some((Focus::Host, "this name could not be looked up".into()))
    );
}

#[test]
fn an_unknown_host_is_explained_as_the_network_or_vpn_with_claudes_words_kept() {
    // How Claude reports an HTTP server whose host name it cannot look up, e.g. a company server off the VPN.
    let issue = |name: &str, said: &str| {
        format!(
            "{name}:\n  Scope: User config (available in all your projects)\n  Status: ✘ Failed to connect\n  Issue: {said}\n  Type: http\n  URL: https://{name}.example.com/mcp\n"
        )
    };
    let enotfound = issue(
        "acme-prod",
        "ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com",
    );
    // The variant: the lookup failed for now (no answer from the DNS server) rather than for good.
    let eai_again = issue("acme-dev", "getaddrinfo EAI_AGAIN acme-dev.example.com");
    let env = Env::new(
        &serde_json::json!({"mcpServers": {
            "acme-prod": http("https://acme-prod.example.com/mcp"),
            "acme-dev": http("https://acme-dev.example.com/mcp"),
            "azure-devops": {"type": "stdio", "command": "npx"}}}),
        r#"{"rules":[
            {"args":["--version"],"stdout_file":"version.txt"},
            {"args":["mcp","get","acme-prod"],"stdout_file":"prod.txt"},
            {"args":["mcp","get","acme-dev"],"stdout_file":"dev.txt"},
            {"args":["mcp","get","azure-devops"],"stdout_file":"connected.txt"}]}"#,
        &[
            ("version.txt", b"2.1.302 (Claude Code)\n"),
            ("prod.txt", enotfound.as_bytes()),
            ("dev.txt", eai_again.as_bytes()),
            ("connected.txt", CONNECTED),
        ],
    );
    let core = env.start();
    let events = collect(&core, |e| {
        ["acme-prod", "acme-dev", "azure-devops"]
            .iter()
            .all(|n| final_status(e, n).is_some())
    });
    for (name, said) in [
        ("acme-prod", "getaddrinfo ENOTFOUND acme-prod.example.com"),
        ("acme-dev", "getaddrinfo EAI_AGAIN acme-dev.example.com"),
    ] {
        let (status, detail) = status_of(&events, name).unwrap();
        assert_eq!(status, Status::Failed, "{name}");
        assert!(
            detail.as_deref().is_some_and(|d| d.contains(said)),
            "{name}: Claude's own words stay available: {detail:?}"
        );
        let hint = hint_of(&events, name).unwrap_or_else(|| panic!("{name}: no explanation"));
        assert!(
            !hint.title.contains("ENOTFOUND") && !hint.title.contains("getaddrinfo"),
            "{name}: the title is plain language: {hint:?}"
        );
        // The usual causes, being offline or off the VPN, come before the rarer typo in the URL.
        let advice = hint.help.join(" ").to_lowercase();
        let at = |word: &str| advice.find(word).unwrap_or(usize::MAX);
        assert!(
            at("offline") < at("typo") && at("vpn") < at("typo"),
            "{name}: {hint:?}"
        );
    }
    assert_eq!(
        hint_of(&events, "azure-devops"),
        None,
        "a connected server needs no explanation"
    );
}

#[test]
fn a_missing_cli_is_reported_and_nothing_crashes() {
    let env = Env::new(
        &serde_json::json!({"mcpServers": {"a": {"command": "x"}}}),
        r#"{"rules":[]}"#,
        &[],
    );
    let mut config = env.config();
    config.claude = None;
    config.path_var = Some(env.dir.path().join("empty").into());
    let core = start_with(config, || {});
    let events = collect(&core, |e| {
        e.iter().any(|x| matches!(x, Event::CliProblem(_)))
    });
    assert!(matches!(&events[0], Event::Snapshot(s) if s.servers.len() == 1));
    assert!(
        status_of(&events, "a").is_none(),
        "no check can run without the CLI"
    );
}

// ---- Diagnose: the client's own debug output, made safe ----

const DEBUG_STDIO: &[u8] = include_bytes!("../../../../../fixtures/debug/failed-stdio.log");

fn diagnosis_of(events: &[Event], name: &str) -> Option<Diagnosis> {
    events.iter().rev().find_map(|e| match e {
        Event::Diagnosis { id, diagnosis } if id.name == name => Some(diagnosis.clone()),
        _ => None,
    })
}

fn diagnose_env(extra_rules: &str, files: &[(&str, &[u8])], servers: serde_json::Value) -> Env {
    let scenario = format!(r#"{{"rules":[{{"args":["--version"]}}{extra_rules}]}}"#,);
    Env::new(
        &serde_json::json!({ "mcpServers": servers }),
        &scenario,
        files,
    )
}

fn diagnose_args() -> &'static str {
    r#""args":["--debug","mcp","--debug-file","*","mcp","get","bad-stdio"]"#
}

#[test]
fn diagnose_explains_a_failing_stdio_server() {
    let rule = format!(
        r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
        diagnose_args()
    );
    let env = diagnose_env(
        &rule,
        &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
        serde_json::json!({"bad-stdio": {"command": "definitely-not-a-real-command-xyz"}}),
    );
    let mut config = env.config();
    // Keep the startup check out of the way: the server already has a status.
    config.max_checks = 3;
    let core = start_with(config, || {});
    let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
    let row = events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
            _ => None,
        })
        .unwrap();
    core.send(Command::Diagnose(row));
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    let d = diagnosis_of(&events, "bad-stdio").unwrap();
    assert_eq!(d.status, Status::Failed);
    assert_eq!(d.note, None);
    assert_eq!(d.hints.len(), 1, "{:?}", d.hints);
    assert_eq!(d.hints[0].title, "The command was not found");
    assert_eq!(
        d.server_said,
        vec![
            "'definitely-not-a-real-command-xyz' is not recognized as an internal or external command,",
            "operable program or batch file.",
        ]
    );
    assert!(d.client_log.iter().any(|l| l.contains("CONNECTION_CLOSED")));
    assert!(
        d.client_log.iter().all(|l| l.contains("bad-stdio")),
        "no start-up noise"
    );
}

#[test]
fn diagnose_cleans_up_its_private_temp_file() {
    let rule = format!(
        r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
        diagnose_args()
    );
    let env = diagnose_env(
        &rule,
        &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
        serde_json::json!({"bad-stdio": {"command": "x"}}),
    );
    let core = env.start();
    let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
    let id = events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
            _ => None,
        })
        .unwrap();
    core.send(Command::Diagnose(id));
    collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    let log = std::fs::read_to_string(&env.log).unwrap();
    let debug_path = log
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find_map(|l| {
            let argv = l["argv"].as_array()?.clone();
            let at = argv.iter().position(|a| a == "--debug-file")?;
            argv.get(at + 1)?.as_str().map(PathBuf::from)
        })
        .expect("the CLI was asked to write a debug file");
    assert!(!debug_path.exists(), "the debug file is deleted");
    assert!(
        !debug_path.parent().unwrap().exists(),
        "and so is its private folder"
    );
    assert_ne!(
        debug_path.parent().unwrap(),
        env.dir.path(),
        "it never wrote into our own folders"
    );
}

#[test]
fn diagnose_never_lets_a_secret_through() {
    let hostile = b"2026-10-17T20:00:00.000Z [DEBUG] MCP server \"bad-stdio\": HTTP transport options: {\"headers\":{\"Authorization\":\"Bearer ENGINESECRET1\"}}\n\
2026-10-17T20:00:01.000Z [ERROR] \"MCP server \\\"bad-stdio\\\" Server stderr: API_KEY=ENGINESECRET2 broke\"\n";
    let rule = format!(
        r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
        diagnose_args()
    );
    let env = diagnose_env(
        &rule,
        &[("dbg.log", hostile), ("failed.txt", FAILED)],
        serde_json::json!({"bad-stdio": {"command": "x"}}),
    );
    let core = env.start();
    let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
    let id = events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
            _ => None,
        })
        .unwrap();
    core.send(Command::Diagnose(id));
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    let dump = format!("{:?}", diagnosis_of(&events, "bad-stdio").unwrap());
    assert!(!dump.contains("ENGINESECRET"), "{dump}");
}

#[test]
fn diagnose_refuses_a_name_that_is_not_safe_without_running_anything() {
    let env = diagnose_env("", &[], serde_json::json!({"a&b": {"command": "x"}}));
    let core = env.start();
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let Event::Snapshot(snap) = &events[0] else {
        panic!()
    };
    core.send(Command::Diagnose(snap.servers[0].id()));
    let events = collect(&core, |e| diagnosis_of(e, "a&b").is_some());
    let d = diagnosis_of(&events, "a&b").unwrap();
    assert_eq!(d.status, Status::Unknown);
    assert!(d.note.unwrap().contains("not diagnosed"));
    assert!(d.hints.is_empty() && d.client_log.is_empty());
    let log = std::fs::read_to_string(&env.log).unwrap_or_default();
    assert!(!log.contains("--debug"), "nothing was spawned for it");
}

#[test]
fn diagnose_survives_a_hung_client_and_a_missing_debug_file() {
    let rule = r#",{"args":["--debug","mcp","--debug-file","*","mcp","get","slow"],"hang":true},
            {"args":["--debug","mcp","--debug-file","*","mcp","get","quiet"],"stdout_file":"failed.txt"}"#;
    let env = diagnose_env(
        rule,
        &[("failed.txt", FAILED)],
        serde_json::json!({"slow": {"command": "x"}, "quiet": {"command": "x"}}),
    );
    let mut config = env.config();
    config.check_timeout = Duration::from_secs(3);
    let core = start_with(config, || {});
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let Event::Snapshot(snap) = &events[0] else {
        panic!()
    };
    for r in &snap.servers {
        core.send(Command::Diagnose(r.id()));
    }
    let events = collect(&core, |e| {
        diagnosis_of(e, "slow").is_some() && diagnosis_of(e, "quiet").is_some()
    });
    let slow = diagnosis_of(&events, "slow").unwrap();
    assert_eq!(slow.status, Status::Unknown);
    assert!(slow.note.unwrap().contains("timed out"));
    let quiet = diagnosis_of(&events, "quiet").unwrap();
    assert_eq!(
        quiet.status,
        Status::Failed,
        "the normal output is still read"
    );
    assert!(quiet.note.unwrap().contains("no debug output"));
    assert!(quiet.client_log.is_empty());
}

#[test]
fn diagnose_also_updates_the_servers_status() {
    let rule = format!(
        r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
        diagnose_args()
    );
    let env = diagnose_env(
        &rule,
        &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
        serde_json::json!({"bad-stdio": {"command": "x"}}),
    );
    let core = env.start();
    let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
    let id = events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
            _ => None,
        })
        .unwrap();
    core.send(Command::Diagnose(id));
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    assert_eq!(final_status(&events, "bad-stdio"), Some(Status::Failed));
}

/// What the server's own definition holds is hidden in a diagnosis too, as in the last starts: the leaks a reviewer
/// found with `redact` alone (a plain environment value, an argument an export replaces, an address's password).
#[test]
fn diagnose_hides_the_servers_own_values_and_the_parts_an_export_replaces() {
    let hostile = b"2026-10-17T20:00:00.000Z [ERROR] \"MCP server \\\"bad-stdio\\\" Server stderr: auth with my-plain-secret-value failed\"\n\
2026-10-17T20:00:01.000Z [ERROR] \"MCP server \\\"bad-stdio\\\" Server stderr: Using --key plain-arg-secret-77 now\"\n\
2026-10-17T20:00:02.000Z [ERROR] \"MCP server \\\"bad-stdio\\\" Server stderr: connect to postgres://u:dbpw-1234567@db/x and https://h.example/mcp?key=urlsecret999\"\n\
2026-10-17T20:00:03.000Z [ERROR] \"MCP server \\\"bad-stdio\\\" Server stderr: filled in value fill-from-env-42 and Bearer tok-from-env-abc\"\n";
    let rule = format!(
        r#",{{{},"debug_file_from":"dbg.log","stdout_file":"failed.txt"}}"#,
        diagnose_args()
    );
    let env = diagnose_env(
        &rule,
        &[("dbg.log", hostile), ("failed.txt", FAILED)],
        serde_json::json!({"bad-stdio": {
            "command": "x",
            "args": ["--key", "plain-arg-secret-77", "--db=postgres://u:dbpw-1234567@db/x"],
            "env": {"PLAIN": "my-plain-secret-value", "FILLED": "${FILLED_FROM}", "AUTH": "Bearer ${TOK_FROM}"},
        }}),
    );
    let core = start_with(
        EngineConfig {
            vars: std::sync::Arc::new(|name: &str| match name {
                "FILLED_FROM" => Some("fill-from-env-42".into()),
                "TOK_FROM" => Some("tok-from-env-abc".into()),
                _ => None,
            }),
            ..env.config()
        },
        || {},
    );
    let events = collect(&core, |e| final_status(e, "bad-stdio").is_some());
    let id = events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => s.servers.first().map(|r| r.id()),
            _ => None,
        })
        .unwrap();
    core.send(Command::Diagnose(id));
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    let d = diagnosis_of(&events, "bad-stdio").unwrap();
    let dump = format!("{d:?}");
    for secret in [
        "my-plain-secret-value",
        "plain-arg-secret-77",
        "dbpw-1234567",
        "urlsecret999",
        "fill-from-env-42",
        "tok-from-env-abc",
    ] {
        assert!(!dump.contains(secret), "{secret} shown in {dump}");
    }
    assert!(
        !d.server_said.is_empty() || !d.client_log.is_empty(),
        "{dump}"
    );
}
