use super::*;
use klefki_core::api::Scope;

fn row(transport: TransportKind) -> ServerRow {
    let mut row = klefki_core::api::fixture_snapshot().servers[0].clone();
    row.name = "slow".to_owned();
    row.scope = Scope::User;
    row.project = None;
    row.transport = transport;
    row.target = match transport {
        TransportKind::Stdio => "npx -y @acme/server".to_owned(),
        _ => "https://mcp.example.com/mcp".to_owned(),
    };
    row
}

fn stdio() -> ServerRow {
    row(TransportKind::Stdio)
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn limit(s: u64, set_in: LimitSetIn) -> TimeLimit {
    TimeLimit {
        limit: secs(s),
        set_in,
        unreadable: false,
        change_in: Some("~/.claude/settings.json".to_owned()),
    }
}

/// A check that ran out of time, unless it was given more and took no longer than that.
fn timing(took: u64, limit: TimeLimit, given: Option<Duration>) -> Timing {
    Timing {
        took: secs(took),
        limit,
        given,
        timed_out: given.is_none_or(|g| secs(took) >= g),
    }
}

fn timed_out_hint() -> Hint {
    Hint {
        title: "The server took too long to answer".to_owned(),
        code: None,
        help: vec![
            "The server may be slow to start, or blocked by a firewall or proxy.".to_owned(),
            "Try again, and check the server's own logs.".to_owned(),
        ],
        points_at: None,
    }
}

fn explained(row: &ServerRow, status: Status, detail: &str, t: &Timing) -> String {
    let hint = (status == Status::Failed).then(timed_out_hint);
    Explanation::for_status_timed(row, status, Some(detail), hint.as_ref(), Some(t))
        .expect("explained")
        .text()
}

#[test]
fn a_timeout_says_how_long_it_ran_how_long_claude_code_waits_and_offers_more_time() {
    let t = timing(28, limit(30, LimitSetIn::Default), None);
    let text = explained(&stdio(), Status::Failed, "Request timed out", &t);
    assert!(
        text.starts_with("error: The server took too long to answer"),
        "{text}"
    );
    assert!(
        text.contains(
            "= help: It gave up after 28 s. Claude Code waits 30 s for a server to connect \
             (Claude Code's default; MCP_TIMEOUT is not set).\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("= help: A first run of npx or uvx downloads"),
        "{text}"
    );
    assert!(
        text.contains("= help: Check again with 2 minutes tells a server that is slow to start"),
        "{text}"
    );
    assert!(
        text.contains("= help: The server may be slow to start"),
        "the hint's own help stays, after: {text}"
    );
    assert!(
        text.contains("= note: Claude Code said: Request timed out"),
        "{text}"
    );
    assert!(offers_longer(&t));
}

#[test]
fn a_remote_server_gets_no_word_about_npx() {
    let t = timing(28, limit(30, LimitSetIn::Default), None);
    let text = explained(
        &row(TransportKind::Http),
        Status::Failed,
        "Request timed out",
        &t,
    );
    assert!(!text.contains("npx"), "{text}");
    assert!(text.contains("Check again with 2 minutes"), "{text}");
}

#[test]
fn the_limit_names_where_it_is_set() {
    for (set_in, unreadable, want) in [
        (
            LimitSetIn::Environment,
            false,
            "(MCP_TIMEOUT in the environment Klefki was started with)",
        ),
        (
            LimitSetIn::File("~/work/app/.claude/settings.json".into()),
            false,
            "(MCP_TIMEOUT in ~/work/app/.claude/settings.json)",
        ),
        (
            LimitSetIn::File("~/.claude/settings.json".into()),
            true,
            "(MCP_TIMEOUT in ~/.claude/settings.json is not a number of milliseconds, so Claude Code uses its default)",
        ),
    ] {
        let mut l = limit(10, set_in);
        l.unreadable = unreadable;
        let text = explained(
            &stdio(),
            Status::Failed,
            "Request timed out",
            &timing(11, l, None),
        );
        assert!(text.contains(want), "{want}\n{text}");
    }
}

#[test]
fn more_time_is_not_offered_where_it_would_not_be_more_or_cannot_be_given() {
    let org = LimitSetIn::Organisation("/etc/claude-code/managed-settings.json".into());
    let t = timing(10, limit(10, org), None);
    assert!(!offers_longer(&t));
    let text = explained(&stdio(), Status::Failed, "Request timed out", &t);
    assert!(!text.contains("Check again with"), "{text}");
    assert!(
        text.contains("Your organisation sets this limit in /etc/claude-code/managed-settings.json, so only it can raise it."),
        "{text}"
    );
    assert!(
        text.contains("(MCP_TIMEOUT in your organisation's /etc/claude-code/"),
        "{text}"
    );
    // Already 2 minutes or more.
    let t = timing(120, limit(120, LimitSetIn::Environment), None);
    assert!(!offers_longer(&t));
    assert!(
        !explained(&stdio(), Status::Failed, "Request timed out", &t).contains("Check again with")
    );
    // Once given more time, not again.
    assert!(!offers_longer(&timing(
        5,
        limit(30, LimitSetIn::Default),
        Some(LONGER_CHECK)
    )));
}

#[test]
fn klefki_giving_up_first_is_explained_too() {
    let t = timing(30, limit(30, LimitSetIn::Default), None);
    let text = explained(&stdio(), Status::Unknown, "check timed out after 30.0s", &t);
    assert!(
        text.starts_with("warning: Claude Code did not answer in time"),
        "{text}"
    );
    assert!(
        text.contains("= help: Klefki stopped waiting for Claude Code's answer after 30 s. Claude Code waits 30 s"),
        "{text}"
    );
    assert!(
        text.contains("= note: Klefki said: check timed out after 30.0s"),
        "{text}"
    );
    // Unknown for another reason has no timing, and nothing to explain.
    assert_eq!(
        Explanation::for_status_timed(&stdio(), Status::Unknown, Some("x"), None, None),
        None
    );
}

#[test]
fn a_server_that_connects_given_more_time_says_how_to_give_it_that_time() {
    let t = timing(41, limit(30, LimitSetIn::Default), Some(LONGER_CHECK));
    let e =
        Explanation::for_status_timed(&stdio(), Status::Connected, None, None, Some(&t)).unwrap();
    let text = e.text();
    assert!(
        text.starts_with("warning: It needs more time to start than Claude Code gives it"),
        "{text}"
    );
    assert!(
        text.contains("= help: Given 2 minutes, it connected after 41 s. Claude Code waits 30 s"),
        "{text}"
    );
    assert!(
        text.contains(
            "= help: To give it that time, set \"MCP_TIMEOUT\": \"70000\" under \"env\" in ~/.claude/settings.json, \
             then start Claude Code again.\n"
        ),
        "41 s and half again is 61.5 s, so 70 s: {text}"
    );
    assert!(text.contains("use Check now to see"), "{text}");
    assert_eq!(Explanation::timing_note(Status::Connected, Some(&t)), None);
}

#[test]
fn where_the_organisation_sets_the_limit_it_is_asked_to_raise_it() {
    let mut l = limit(
        30,
        LimitSetIn::Organisation("/etc/x/managed-settings.json".into()),
    );
    l.change_in = None;
    let t = timing(41, l, Some(LONGER_CHECK));
    let text = Explanation::for_status_timed(&stdio(), Status::Connected, None, None, Some(&t))
        .unwrap()
        .text();
    assert!(
        text.contains("Your organisation sets this limit in /etc/x/managed-settings.json, so only it can raise it: ask for at least 70000 ms."),
        "{text}"
    );
}

#[test]
fn the_suggested_limit_leaves_room_in_whole_ten_seconds() {
    for (took_ms, want) in [
        (0, 10_000),
        (6_000, 10_000),
        (6_700, 20_000),
        (31_000, 50_000),
        (100_000, 150_000),
    ] {
        assert_eq!(
            suggested_ms(Duration::from_millis(took_ms)),
            want,
            "{took_ms}"
        );
    }
}

#[test]
fn a_server_that_connects_in_time_given_more_has_only_a_note() {
    let t = timing(8, limit(30, LimitSetIn::Default), Some(LONGER_CHECK));
    assert_eq!(
        Explanation::for_status_timed(&stdio(), Status::Connected, None, None, Some(&t)),
        None
    );
    let note = Explanation::timing_note(Status::Connected, Some(&t)).unwrap();
    assert!(
        note.starts_with("It connected after 8 s, within the 30 s Claude Code waits"),
        "{note}"
    );
    assert_eq!(
        Explanation::timing_note(
            Status::Connected,
            Some(&timing(8, limit(30, LimitSetIn::Default), None))
        ),
        None,
        "only after a check given more time"
    );
}

#[test]
fn a_server_still_silent_given_more_time_is_called_stuck() {
    let t = timing(120, limit(30, LimitSetIn::Default), Some(LONGER_CHECK));
    let text = explained(&stdio(), Status::Failed, "Request timed out", &t);
    assert!(
        text.contains("= help: Given 2 minutes, it still did not connect: it is probably stuck rather than slow.\n"),
        "{text}"
    );
    assert!(
        text.contains("= help: Diagnose shows what it printed"),
        "{text}"
    );
    assert!(!text.contains("Try again"), "no longer the advice: {text}");
    assert!(!text.contains("Check again with"), "{text}");
}

#[test]
fn a_different_failure_given_more_time_is_explained_as_before() {
    let t = timing(3, limit(30, LimitSetIn::Default), Some(LONGER_CHECK));
    assert!(!t.timed_out);
    let with = Explanation::for_status_timed(
        &stdio(),
        Status::Failed,
        Some("CONNECTION_CLOSED"),
        None,
        Some(&t),
    );
    let without =
        Explanation::for_status(&stdio(), Status::Failed, Some("CONNECTION_CLOSED"), None);
    assert_eq!(with, without);
}
