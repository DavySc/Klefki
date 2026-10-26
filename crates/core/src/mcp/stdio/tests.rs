//! The proposal's test matrix (`docs/proposals/checks-without-claude.md`), against `fake-mcp-server`, each case as
//! Claude Code 2.1.310 answered it.
use super::*;
use crate::cli::testutil::{fake_mcp_server, gone_soon, pid_alive};
use crate::config::server_env::{self, Inputs, NotStarted, Os};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const LIMIT: Duration = Duration::from_secs(10);

struct Server {
    dir: tempfile::TempDir,
}

impl Server {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn log(&self) -> PathBuf {
        self.dir.path().join("log.jsonl")
    }

    fn dump(&self) -> PathBuf {
        self.dir.path().join("dump.json")
    }

    fn launch(&self, args: &[&str]) -> Launch {
        self.launch_hiding(args, &[])
    }

    fn launch_hiding(&self, args: &[&str], hidden: &[&str]) -> Launch {
        Launch {
            program: fake_mcp_server().to_path_buf(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: self.dir.path().to_path_buf(),
            env: vec![
                ("FAKE_MCP_LOG".into(), self.log().into()),
                ("FAKE_MCP_DUMP".into(), self.dump().into()),
            ],
            hidden: hidden.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    fn check(&self, args: &[&str]) -> Outcome {
        check(&self.launch(args), LIMIT, "2.1.310")
    }

    /// Every message the server got, in order.
    fn got(&self) -> Vec<Value> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.got()
            .iter()
            .map(|m| m["method"].as_str().unwrap_or("(answer)").to_owned())
            .collect()
    }

    fn dumped(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.dump()).unwrap()).unwrap()
    }
}

fn connected(outcome: &Outcome, protocol: &str, tools: usize) {
    assert_eq!(
        (
            outcome.verdict,
            outcome.protocol.as_deref(),
            outcome.tools,
            outcome.reason.as_str()
        ),
        (Verdict::Connected, Some(protocol), tools, ""),
        "{outcome:?}"
    );
}

fn verdict(outcome: &Outcome, verdict: Verdict, reason: &str) {
    assert_eq!(outcome.verdict, verdict, "{outcome:?}");
    assert!(outcome.reason.contains(reason), "{reason:?} in {outcome:?}");
}

const OLDER_HELLO: [&str; 4] = [
    "server/discover",
    "initialize",
    "notifications/initialized",
    "tools/list",
];

#[test]
fn a_server_that_answers_normally_is_connected_after_claude_codes_hello() {
    let s = Server::new();
    let out = s.check(&["ok"]);
    connected(&out, "2025-11-25", 1);
    assert!(out.took < LIMIT);
    assert_eq!(s.methods(), OLDER_HELLO);
    // Byte for byte what Claude Code 2.1.310 sent, but for the order of keys inside `_meta`.
    let got = s.got();
    let client = json!({"name": "claude-code", "title": "Claude Code", "version": "2.1.310",
        "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"});
    let caps = json!({"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}});
    assert_eq!(
        got[0],
        json!({"jsonrpc": "2.0", "id": "server-discover-probe-1", "method": "server/discover", "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": client,
            "io.modelcontextprotocol/clientCapabilities": caps}}})
    );
    assert_eq!(
        got[1],
        json!({"method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": caps,
            "clientInfo": client}, "jsonrpc": "2.0", "id": 0})
    );
    assert_eq!(
        got[2],
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    );
    assert_eq!(
        got[3],
        json!({"method": "tools/list", "jsonrpc": "2.0", "id": 1})
    );
}

#[test]
fn a_server_that_speaks_only_the_newer_protocol_is_connected_without_initialize() {
    let s = Server::new();
    connected(&s.check(&["modernonly"]), "2026-07-28", 1);
    assert_eq!(s.methods(), ["server/discover", "tools/list"]);
    let tools = &s.got()[1];
    assert_eq!(tools["id"], 0);
    assert_eq!(tools["params"]["_meta"], s.got()[0]["params"]["_meta"]);

    let s = Server::new();
    verdict(
        &s.check(&["moderntoolsfail"]),
        Verdict::ToolsFailed,
        "no tools today (-32603)",
    );
    let s = Server::new();
    connected(&s.check(&["modernnotools"]), "2026-07-28", 0);
    assert_eq!(
        s.methods(),
        ["server/discover"],
        "no tools, so none asked for"
    );
}

#[test]
fn an_answer_to_discover_that_is_not_the_newer_protocol_falls_back_to_initialize() {
    for mode in ["emptydiscover", "otherdiscover", "ok"] {
        let s = Server::new();
        connected(&s.check(&[mode]), "2025-11-25", 1);
        assert_eq!(s.methods(), OLDER_HELLO, "{mode}");
    }
}

#[test]
fn a_server_that_ignores_discover_is_said_hello_to_the_older_way_after_claude_codes_wait() {
    let s = Server::new();
    let out = s.check(&["silentdiscover"]);
    connected(&out, "2025-11-25", 1);
    assert!(out.took >= DISCOVER_WAIT, "{out:?}");
    assert_eq!(s.methods(), OLDER_HELLO);
    // The time limit counts from the start: one shorter than the wait ends there.
    let s = Server::new();
    let out = check(
        &s.launch(&["silentdiscover"]),
        Duration::from_millis(700),
        "2.1.310",
    );
    verdict(&out, Verdict::TimedOut, "did not answer within 0.7 s");
    assert!(out.took < DISCOVER_WAIT, "{out:?}");
}

#[test]
fn a_server_that_exits_at_once_failed_and_its_error_output_says_why() {
    let s = Server::new();
    let out = s.check(&["exit"]);
    verdict(&out, Verdict::Failed, "closed before it answered");
    assert_eq!(out.stderr, "Error: cannot open database");
    assert_eq!(out.protocol, None);
}

#[test]
fn a_refused_or_unacceptable_hello_failed() {
    for (mode, reason) in [
        (
            "badinit",
            "refused to start: Unsupported protocol version (-32602)",
        ),
        ("weirdversion", "protocol version Claude Code does not"),
        ("noversion", "names no protocol version"),
        ("nocaps", "no capabilities"),
        ("noserverinfo", "does not name the server"),
        ("serverinfonoversion", "does not name the server"),
    ] {
        let s = Server::new();
        verdict(&s.check(&[mode]), Verdict::Failed, reason);
        assert!(!s.methods().contains(&"tools/list".to_owned()), "{mode}");
    }
}

#[test]
fn a_server_whose_tools_cannot_be_listed_is_connected_with_tools_failed() {
    for (mode, reason) in [
        (
            "toolsfail",
            "Listing its tools failed: no tools today (-32603)",
        ),
        ("exitafterinit", "closed before it listed its tools"),
        ("badtool", "not described the way MCP asks"),
    ] {
        let s = Server::new();
        let out = s.check(&[mode]);
        verdict(&out, Verdict::ToolsFailed, reason);
        assert_eq!(out.protocol.as_deref(), Some("2025-11-25"), "{mode}");
    }
    let s = Server::new();
    let out = check(
        &s.launch(&["toolshang"]),
        Duration::from_millis(600),
        "2.1.310",
    );
    verdict(
        &out,
        Verdict::ToolsFailed,
        "did not list its tools within 0.6 s",
    );
}

#[test]
fn a_server_with_no_tools_is_connected_and_is_not_asked_for_them() {
    let s = Server::new();
    connected(&s.check(&["notools"]), "2025-11-25", 0);
    assert_eq!(s.methods(), &OLDER_HELLO[..3]);
}

#[test]
fn older_protocols_noise_long_lines_and_text_ids_are_all_connected() {
    let s = Server::new();
    connected(&s.check(&["oldversion"]), "2024-11-05", 1);
    for mode in ["junk", "huge", "stringid", "spam"] {
        let s = Server::new();
        connected(&s.check(&[mode]), "2025-11-25", 1);
    }
    let s = Server::new();
    connected(&s.check(&["manytools", "250"]), "2025-11-25", 250);
}

#[test]
fn a_server_may_ask_for_its_roots_and_ping_while_it_says_hello() {
    let s = Server::new();
    connected(&s.check(&["roots"]), "2025-11-25", 1);
    let answers: Vec<Value> = s
        .got()
        .into_iter()
        .filter(|m| m.get("method").is_none())
        .collect();
    let uri = url::Url::from_file_path(s.dir.path()).unwrap();
    assert_eq!(
        answers,
        [
            json!({"result": {"roots": [{"uri": uri.as_str()}]}, "jsonrpc": "2.0", "id": "r1"}),
            json!({"result": {}, "jsonrpc": "2.0", "id": "p1"}),
        ]
    );
}

#[test]
fn a_slow_server_is_waited_for_and_one_that_never_answers_times_out_at_the_limit() {
    let s = Server::new();
    connected(
        &check(
            &s.launch(&["slow", "800"]),
            Duration::from_secs(5),
            "2.1.310",
        ),
        "2025-11-25",
        1,
    );
    let s = Server::new();
    let limit = Duration::from_millis(1500);
    let out = check(&s.launch(&["hang"]), limit, "2.1.310");
    verdict(&out, Verdict::TimedOut, "did not answer within 1.5 s");
    assert!(
        out.took >= limit && out.took < limit + Duration::from_secs(2),
        "{out:?}"
    );
}

#[test]
fn the_server_and_everything_it_started_are_ended_with_the_check() {
    let s = Server::new();
    let pid_file = s.dir.path().join("grandchild.pid");
    connected(
        &s.check(&["grandchild", &pid_file.to_string_lossy()]),
        "2025-11-25",
        1,
    );
    let server: u32 = s.dumped()["pid"].as_u64().unwrap().try_into().unwrap();
    let grandchild: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(gone_soon(server), "the server is still running");
    assert!(gone_soon(grandchild), "what it started is still running");
    // And when it hangs.
    let s = Server::new();
    let out = check(&s.launch(&["hang"]), Duration::from_millis(500), "2.1.310");
    assert_eq!(out.verdict, Verdict::TimedOut);
    let server: u32 = s.dumped()["pid"].as_u64().unwrap().try_into().unwrap();
    assert!(gone_soon(server));
    assert!(!pid_alive(server));
}

#[test]
fn what_the_server_prints_is_cleaned_and_its_own_values_hidden() {
    let s = Server::new();
    let out = check(
        &s.launch_hiding(&["stderr"], &["s3cr3t-token-value"]),
        LIMIT,
        "2.1.310",
    );
    verdict(&out, Verdict::Failed, "closed before it answered");
    assert_eq!(out.stderr, "starting\ntoken is ***\nfatal: no config");
    // A refusal's text is the server's too.
    let s = Server::new();
    let out = check(
        &s.launch_hiding(&["badinit"], &["Unsupported protocol"]),
        LIMIT,
        "2.1.310",
    );
    verdict(
        &out,
        Verdict::Failed,
        "refused to start: *** version (-32602)",
    );
}

#[test]
fn a_program_that_cannot_be_started_failed() {
    let s = Server::new();
    let mut launch = s.launch(&["ok"]);
    launch.program = s.dir.path().join("not-there");
    let out = check(&launch, LIMIT, "2.1.310");
    verdict(
        &out,
        Verdict::Failed,
        "Could not start it: entity not found",
    );
}

#[test]
fn reading_skips_long_lines_and_what_is_not_a_json_object() {
    let (tx, rx) = mpsc::sync_channel(16);
    // An object line of exactly `len` bytes.
    let object = |len: usize| {
        let mut line = b"{\"s\": \"".to_vec();
        line.extend(vec![b'y'; len - line.len() - 2]);
        line.extend_from_slice(b"\"}\n");
        line
    };
    let mut input = Vec::new();
    input.extend_from_slice(b"not json\n");
    input.extend(object(MAX_LINE + 1));
    input.extend_from_slice(b"{\"a\": 1}\n");
    input.extend(object(MAX_LINE));
    input.extend_from_slice(b"[2]\n\"text\"\n3\n  {\"b\": 2}\n{\"unterminated\": 3}");
    // A reader with a small buffer, so lines cross its fills.
    read_lines(BufReader::with_capacity(7, input.as_slice()), &tx);
    drop(tx);
    let got: Vec<Value> = rx.into_iter().collect();
    assert_eq!(
        got.len(),
        3,
        "{:?}",
        got.iter().map(|v| v.to_string().len()).collect::<Vec<_>>()
    );
    assert_eq!(got[0], json!({"a": 1}));
    assert_eq!(
        got[1]["s"].as_str().map(str::len),
        Some(MAX_LINE - 9),
        "a line of exactly MAX_LINE bytes is read"
    );
    assert_eq!(got[2], json!({"b": 2}));
}

#[test]
fn ids_match_as_claude_code_matches_them() {
    assert!(same_id(&json!(1), &json!(1)));
    assert!(same_id(&json!("1"), &json!(1)));
    assert!(same_id(&json!(DISCOVER_ID), &json!(DISCOVER_ID)));
    assert!(!same_id(&json!("2"), &json!(1)));
    assert!(!same_id(&json!(1), &json!("1")));
    assert!(!same_id(&json!(null), &json!(0)));
}

#[test]
fn tools_are_checked_as_claude_code_checks_them() {
    let ok = json!({"type": "object"});
    let good = [
        json!({"name": "a", "inputSchema": ok}),
        json!({"name": "a", "inputSchema": {"type": "object", "properties": {}, "required": ["x"]}, "extra": 1}),
        json!({"name": "a", "title": "A", "description": "d", "inputSchema": ok, "outputSchema": ok,
            "annotations": {}}),
    ];
    // Each measured as "tools fetch failed" with Claude Code 2.1.310, or the same rule.
    let bad = [
        json!(5),
        json!({"inputSchema": ok}),
        json!({"name": 5, "inputSchema": ok}),
        json!({"name": "a"}),
        json!({"name": "a", "inputSchema": {}}),
        json!({"name": "a", "inputSchema": {"type": "string"}}),
        json!({"name": "a", "inputSchema": "x"}),
        json!({"name": "a", "inputSchema": null}),
        json!({"name": "a", "description": 5, "inputSchema": ok}),
        json!({"name": "a", "inputSchema": {"type": "object", "required": [1]}}),
        json!({"name": "a", "inputSchema": ok, "outputSchema": {}}),
    ];
    for tool in good {
        assert_eq!(good_tools(&json!({"tools": [tool]})), Ok(1), "{tool}");
    }
    for tool in bad {
        assert!(good_tools(&json!({"tools": [tool]})).is_err(), "{tool}");
    }
    assert_eq!(good_tools(&json!({"tools": []})), Ok(0));
    assert!(good_tools(&json!({"tools": {}})).is_err());
    assert!(good_tools(&json!({})).is_err());
}

/// The whole way: what `server_env` builds is what the server gets.
#[test]
fn a_server_started_from_its_definition_gets_claude_codes_environment_and_folder() {
    let s = Server::new();
    let program = fake_mcp_server();
    let folder = s.dir.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    let entry = json!({
        "command": program.file_name().unwrap().to_string_lossy(),
        "args": ["needsenv", "FROM_SETTINGS", "yes"],
        "env": {"PATH": program.parent().unwrap().to_string_lossy(), "FAKE_MCP_DUMP": s.dump().to_string_lossy(),
            "OWN": "${FROM_SETTINGS}"}
    });
    let klefki: Vec<(OsString, OsString)> = vec![
        ("MCP_CLIENT_SECRET".into(), "never".into()),
        ("CLAUDE_CODE_OAUTH_TOKEN".into(), "never".into()),
        ("KLEFKI_VAR".into(), "k".into()),
    ];
    let settings = vec![("FROM_SETTINGS".to_owned(), "yes".to_owned())];
    let launch = server_env::launch(&Inputs {
        entry: &entry,
        folder: &folder,
        klefki_env: &klefki,
        settings: &settings,
        claude_version: Some("2.1.310"),
        session_id: &server_env::new_session_id(),
        os: Os::HERE,
    })
    .unwrap();
    connected(&check(&launch, LIMIT, "2.1.310"), "2025-11-25", 1);
    let dump = s.dumped();
    let env = &dump["env"];
    assert_eq!(env["CLAUDECODE"], "1");
    assert_eq!(env["KLEFKI_VAR"], "k");
    assert_eq!(env["OWN"], "yes");
    assert_eq!(env["CLAUDE_PROJECT_DIR"], folder.to_string_lossy().as_ref());
    assert!(env.get("MCP_CLIENT_SECRET").is_none() && env.get("CLAUDE_CODE_OAUTH_TOKEN").is_none());
    assert_eq!(
        Path::new(dump["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        folder.canonicalize().unwrap()
    );
    assert_eq!(dump["args"], json!(["needsenv", "FROM_SETTINGS", "yes"]));
    // Without the settings value it fails, as in Claude Code.
    let launch = server_env::launch(&Inputs {
        entry: &entry,
        folder: &folder,
        klefki_env: &klefki,
        settings: &[],
        claude_version: Some("2.1.310"),
        session_id: "x",
        os: Os::HERE,
    })
    .unwrap();
    let out = check(&launch, LIMIT, "2.1.310");
    verdict(&out, Verdict::Failed, "closed before it answered");
    assert_eq!(out.stderr, "FROM_SETTINGS is not set");
    // Not started at all: not a stdio server.
    let http = json!({"type": "http", "url": "https://example.com/mcp"});
    let refused = server_env::launch(&Inputs {
        entry: &http,
        folder: &folder,
        klefki_env: &klefki,
        settings: &[],
        claude_version: None,
        session_id: "x",
        os: Os::HERE,
    });
    assert_eq!(refused.err(), Some(NotStarted::NotStdio));
}

/// Found in review: answers to a server that asks and never reads filled the pipe, the write blocked, and the check
/// never ended.
#[test]
fn a_server_that_asks_without_reading_its_answers_still_ends_at_the_limit() {
    for mode in ["floodping", "floodroots"] {
        let s = Server::new();
        let limit = Duration::from_millis(1500);
        let out = check(&s.launch(&[mode]), limit, "2.1.310");
        verdict(&out, Verdict::TimedOut, "did not answer within 1.5 s");
        assert!(out.took < limit + Duration::from_secs(3), "{mode}: {out:?}");
        let server: u32 = s.dumped()["pid"].as_u64().unwrap().try_into().unwrap();
        assert!(gone_soon(server), "{mode}: the server is still running");
    }
}
