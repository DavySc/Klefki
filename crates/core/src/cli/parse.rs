//! Reads the output of `claude mcp get <name>`. The output is untrusted text: only the status and a short
//! issue line are kept, everything else (environment values, headers, URLs) is dropped on purpose.
use crate::api::Status;
use crate::sanitize::{clip, sanitize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checked {
    pub status: Status,
    /// The `Issue:` line, sanitised and clipped.
    pub detail: Option<String>,
}

const MAX_DETAIL_CHARS: usize = 200;

/// What a list said about one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Listed {
    /// Claude's answer: nothing the short timeout can have changed.
    Settled(Checked),
    /// The line is clear, but its answer may come from the short timeout: `get` is asked. `never_connected` when the
    /// server did not even answer the connection (it hangs): its `get` takes the full 30 s, so it goes last.
    Open { never_connected: bool },
}

/// What one `claude mcp list` said about each of `names`. Only a line that says it clearly counts: the server
/// named once, and exactly one status in it. Anything else has no entry, and the server is checked on its own.
///
/// A line reads `<name>: <command and arguments> - <glyph> <status>[ — <issue>]`. The command comes from the
/// configuration and the issue from the server, so either may hold text that looks like a status. A status is
/// taken only when the line has one place where ` - ` is followed by a status word; two make it unclear.
///
/// The list is run with a short `MCP_TIMEOUT`, so that one server that hangs does not hold back the answers of the
/// others. A server that needs longer than that shows as failed for want of time, which `get` (30 s) would not say.
/// So a failure is taken only when it gives a reason that has nothing to do with time.
pub(crate) fn parse_list(stdout: &str, names: &[&str]) -> HashMap<String, Listed> {
    let wanted: HashSet<&str> = names.iter().copied().collect();
    let text = sanitize(stdout);
    // `None` once a name is seen again: it is unclear which line is its own.
    let mut seen: HashMap<&str, Option<Listed>> = HashMap::new();
    for line in text.lines().map(str::trim) {
        let Some((name, rest)) = line.split_once(": ") else {
            continue;
        };
        let Some(&name) = wanted.get(name) else {
            continue;
        };
        let listed = if seen.contains_key(name) {
            None
        } else {
            list_status(rest)
        };
        seen.insert(name, listed);
    }
    seen.into_iter()
        .filter_map(|(name, listed)| Some((name.to_owned(), listed?)))
        .collect()
}

/// The status at the end of one list line (after the name), if there is exactly one.
fn list_status(rest: &str) -> Option<Listed> {
    let mut found = None;
    for (at, _) in rest.match_indices(" - ") {
        let tail = &rest[at + 3..];
        let (status_text, issue) = match tail.split_once(" — ") {
            Some((status, issue)) => (status, Some(issue)),
            None => (tail, None),
        };
        let status = classify(status_text);
        if status == Status::Unknown || !starts_like_status(status_text) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some((status, issue.map(str::trim).filter(|i| !i.is_empty())));
    }
    let (status, issue) = found?;
    let settled = match status {
        Status::Failed | Status::Degraded => issue.is_some_and(|i| !mentions_time(i)),
        _ => true,
    };
    Some(if settled {
        Listed::Settled(Checked {
            status,
            detail: issue.map(|i| clip(i, MAX_DETAIL_CHARS)),
        })
    } else {
        Listed::Open {
            never_connected: issue
                .is_some_and(|i| i.to_lowercase().contains("connection timed out")),
        }
    })
}

/// Whether an issue may be about running out of time ("timed out", "timeout", "in time"). Wide on purpose: a wrong
/// yes only means `get` is asked.
fn mentions_time(issue: &str) -> bool {
    issue.to_lowercase().contains("time")
}

/// Whether the text, after its glyph, begins with one of the words Claude opens a status with.
fn starts_like_status(text: &str) -> bool {
    let text = text
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    [
        "connected",
        "needs authentication",
        "failed to connect",
        "connection error",
        "pending approval",
        "not configured",
    ]
    .iter()
    .any(|start| text.starts_with(start))
}

pub(crate) fn parse_get(stdout: &str) -> Checked {
    // Sanitising first removes terminal escapes, control characters (including the CR of CRLF) and bidi marks.
    let text = sanitize(stdout);
    let mut status_text = None;
    let mut issue = None;
    for line in text.lines().map(str::trim) {
        if status_text.is_none()
            && let Some(rest) = line.strip_prefix("Status:")
        {
            status_text = Some(rest);
            continue;
        }
        if issue.is_none()
            && let Some(rest) = line.strip_prefix("Issue:")
        {
            issue = Some(rest.trim());
        }
        if status_text.is_some() && issue.is_some() {
            break;
        }
    }
    Checked {
        status: status_text.map_or(Status::Unknown, classify),
        detail: issue
            .filter(|i| !i.is_empty())
            .map(|i| clip(i, MAX_DETAIL_CHARS)),
    }
}

/// The glyph in front of the status varies by platform, so only the words are read. Order matters:
/// the more specific phrases come first.
fn classify(status: &str) -> Status {
    let t = status.to_lowercase();
    if t.contains("tools fetch failed") {
        Status::Degraded
    } else if t.contains("needs authentication") {
        Status::NeedsAuth
    } else if t.contains("failed to connect") || t.contains("connection error") {
        Status::Failed
    } else if t.contains("pending approval") {
        Status::PendingApproval
    } else if t.contains("not configured") {
        Status::NotConfigured
    } else if t
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "connected")
    {
        Status::Connected
    } else {
        Status::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../../../fixtures/mcp-get/", $name))
        };
    }

    const CASES: &[(&str, &str, Status, Option<&str>)] = &[
        (
            "connected-stdio",
            fixture!("connected-stdio.txt"),
            Status::Connected,
            None,
        ),
        (
            "failed-stdio",
            fixture!("failed-stdio.txt"),
            Status::Failed,
            Some("CONNECTION_CLOSED: Connection closed"),
        ),
        (
            "failed-http",
            fixture!("failed-http.txt"),
            Status::Failed,
            Some(
                "ECONNREFUSED: ECONNREFUSED: Unable to connect. Is the computer able to access the url?",
            ),
        ),
        (
            "pending-approval",
            fixture!("pending-approval.txt"),
            Status::PendingApproval,
            None,
        ),
        (
            "needs-auth",
            fixture!("needs-auth.txt"),
            Status::NeedsAuth,
            None,
        ),
        (
            "connected-tools-failed",
            fixture!("connected-tools-failed.txt"),
            Status::Degraded,
            Some("tools/list timed out"),
        ),
        (
            "not-configured",
            fixture!("not-configured.txt"),
            Status::NotConfigured,
            None,
        ),
        (
            "connection-error",
            fixture!("connection-error.txt"),
            Status::Failed,
            None,
        ),
    ];

    #[test]
    fn every_fixture_maps_to_its_status() {
        for (name, text, status, detail) in CASES {
            let got = parse_get(text);
            assert_eq!(got.status, *status, "{name}");
            assert_eq!(got.detail.as_deref(), *detail, "{name}");
        }
    }

    #[test]
    fn crlf_output_parses_the_same() {
        for (name, text, status, detail) in CASES {
            let got = parse_get(&text.replace('\n', "\r\n"));
            assert_eq!(got.status, *status, "{name} (CRLF)");
            assert_eq!(got.detail.as_deref(), *detail, "{name} (CRLF)");
        }
    }

    #[test]
    fn the_status_glyph_does_not_matter() {
        // Windows fallback glyphs, no glyph at all, and a different spacing all read the same.
        for line in [
            "Status: √ Connected",
            "Status:Connected",
            "  Status:   ✔   Connected  ",
            "Status: * Connected",
        ] {
            assert_eq!(
                parse_get(&format!("x:\n{line}\n")).status,
                Status::Connected,
                "{line:?}"
            );
        }
        assert_eq!(
            parse_get("x:\n  Status: × Failed to connect\n").status,
            Status::Failed
        );
    }

    #[test]
    fn the_first_status_line_wins() {
        let text = "x:\n  Status: ✘ Failed to connect\n  Issue: Status: ✔ Connected\n  Status: ✔ Connected\n";
        let got = parse_get(text);
        assert_eq!(got.status, Status::Failed);
        assert_eq!(got.detail.as_deref(), Some("Status: ✔ Connected"));
    }

    #[test]
    fn unrecognised_output_is_unknown_not_a_guess() {
        for text in [
            "",
            "   \n\n",
            "No MCP server named \"x\".",
            "x:\n  Scope: User config\n",
            "x:\n  Status:\n",
            "x:\n  Status: ✔ Fancy new state\n",
            "Status without a colon Connected",
        ] {
            assert_eq!(parse_get(text).status, Status::Unknown, "{text:?}");
        }
    }

    #[test]
    fn hostile_output_is_sanitised_and_leaks_nothing() {
        let got = parse_get(fixture!("hostile.txt"));
        assert_eq!(got.status, Status::Connected);
        let detail = got.detail.clone().expect("issue line");
        assert!(
            !detail.chars().any(|c| c.is_control() || c == '\u{202e}'),
            "{detail:?}"
        );
        assert!(
            detail.chars().count() <= 201,
            "clipped, got {}",
            detail.chars().count()
        );
        assert!(detail.starts_with("evilgnp.exe red"), "{detail:?}");
        assert!(
            !format!("{got:?}").contains("S3CRET"),
            "environment and header values must be dropped"
        );
    }

    #[test]
    fn a_keyword_in_the_issue_text_does_not_change_the_status() {
        let text = "x:\n  Status: ✘ Failed to connect\n  Issue: server said: Connected · Needs authentication\n";
        assert_eq!(parse_get(text).status, Status::Failed);
    }

    #[test]
    fn huge_input_is_handled() {
        let big = format!(
            "x:\n  Status: ✔ Connected\n{}",
            "  Environment: y\n".repeat(200_000)
        );
        assert_eq!(parse_get(&big).status, Status::Connected);
    }

    const LAB_LIST: &str = include_str!("../../../../fixtures/mcp-list/lab-stdio.txt");

    fn listed(text: &str, names: &[&str]) -> HashMap<String, Checked> {
        parse_list(text, names)
            .into_iter()
            .filter_map(|(name, l)| match l {
                Listed::Settled(checked) => Some((name, checked)),
                Listed::Open { .. } => None,
            })
            .collect()
    }

    fn one(text: &str, name: &str) -> Option<Checked> {
        listed(text, &[name]).remove(name)
    }

    #[test]
    fn a_real_list_gives_the_same_answers_as_get_for_each_server() {
        // Each of these was also run through `claude mcp get`: same status, same issue (2.1.303, Windows).
        let want: &[(&str, Status, Option<&str>)] = &[
            ("ok", Status::Connected, None),
            ("noise", Status::Connected, None),
            (
                "exit",
                Status::Failed,
                Some("CONNECTION_CLOSED: Connection closed"),
            ),
            ("badinit", Status::Failed, Some("-32603: nope")),
            (
                "oldproto",
                Status::Failed,
                Some("Server's protocol version is not supported: 1999-01-01"),
            ),
            ("toolsfail", Status::Degraded, Some("no tools")),
            ("exitafterinit", Status::Degraded, Some("Connection closed")),
            (
                "missing",
                Status::Failed,
                Some("CONNECTION_CLOSED: Connection closed"),
            ),
            ("argvar", Status::Connected, None),
        ];
        let names: Vec<&str> = want.iter().map(|w| w.0).collect();
        let got = listed(LAB_LIST, &names);
        for (name, status, detail) in want {
            let c = got
                .get(*name)
                .unwrap_or_else(|| panic!("{name}: no answer"));
            assert_eq!(c.status, *status, "{name}");
            assert_eq!(c.detail.as_deref(), *detail, "{name}");
        }
    }

    #[test]
    fn crlf_list_output_parses_the_same() {
        let names = ["ok", "exit", "toolsfail"];
        assert_eq!(
            listed(&LAB_LIST.replace('\n', "\r\n"), &names),
            listed(LAB_LIST, &names)
        );
        assert_eq!(listed(LAB_LIST, &names).len(), 3);
    }

    #[test]
    fn a_server_the_list_does_not_name_has_no_answer() {
        assert_eq!(one(LAB_LIST, "not-there"), None);
        // A longer name must not answer for a shorter one.
        assert_eq!(one("abc: x - ✔ Connected\n", "ab"), None);
        assert_eq!(
            one("abc: x - ✔ Connected\n", "abc").map(|c| c.status),
            Some(Status::Connected)
        );
    }

    #[test]
    fn a_server_named_twice_has_no_answer() {
        let text = "a: x - ✔ Connected\na: y - ✘ Failed to connect — no\n";
        assert_eq!(one(text, "a"), None);
    }

    #[test]
    fn a_command_that_looks_like_a_status_has_no_answer() {
        // Two statuses in one line: which one is Claude's cannot be told, so Claude is asked about this server.
        for text in [
            "a: sh -c x - ✔ Connected - ✘ Failed to connect — y\n",
            "a: x - ✔ Connected — the server said: y - ✘ Failed to connect\n",
            "a: x - ✔ Connected - ✔ Connected\n",
        ] {
            assert_eq!(one(text, "a"), None, "{text:?}");
        }
    }

    #[test]
    fn a_line_without_a_status_claude_words_that_way_has_no_answer() {
        for text in [
            "a: x\n",
            "a: x - ✔ Fancy new state\n",
            "a: x - ⊘ Disabled for this project (re-enable via /mcp)\n",
            "a:\n",
            "a x - ✔ Connected\n",
        ] {
            assert_eq!(one(text, "a"), None, "{text:?}");
        }
    }

    #[test]
    fn other_lines_are_ignored() {
        let text = "Checking MCP server health…\n\nclaude.ai Notion: https://x.example.com (HTTP) - ! Needs authentication\nh: https://h.example.com (HTTP) - ! Needs authentication\n";
        let got = listed(text, &["h", "Notion"]);
        assert_eq!(got.len(), 1);
        assert_eq!(got["h"].status, Status::NeedsAuth);
    }

    #[test]
    fn a_dash_in_the_command_is_not_a_status() {
        let c = one("a: uvx - run foo - ✔ Connected\n", "a").expect("one status");
        assert_eq!(c.status, Status::Connected);
    }

    #[test]
    fn the_issue_of_a_list_line_is_sanitised_and_clipped() {
        let long = "x".repeat(500);
        let text =
            format!("a: c - ✘ Failed to connect — \u{1b}[31mred\u{1b}[0m \u{202e}evil {long}\n");
        let detail = one(&text, "a").unwrap().detail.unwrap();
        assert!(
            !detail.chars().any(|c| c.is_control() || c == '\u{202e}'),
            "{detail:?}"
        );
        assert!(detail.chars().count() <= 201, "{}", detail.chars().count());
        assert!(detail.starts_with("red"), "{detail:?}");
    }

    const LAB_SHORT: &str = include_str!("../../../../fixtures/mcp-list/lab-short-timeout.txt");

    #[test]
    fn a_list_run_with_a_short_timeout_answers_only_what_the_timeout_cannot_have_changed() {
        // Captured with MCP_TIMEOUT=3000. `get` (30 s) says Connected for slow4 (it answers in 4 s), so a
        // "timed out" is not Claude's answer for it; the others do not depend on the time.
        let names = [
            "ok",
            "exit",
            "hang",
            "badinit",
            "toolsfail",
            "slowtools5",
            "noargs",
            "slowtools3",
            "slow4",
        ];
        let got = listed(LAB_SHORT, &names);
        let mut answered: Vec<&str> = got.keys().map(String::as_str).collect();
        answered.sort_unstable();
        assert_eq!(
            answered,
            ["badinit", "exit", "ok", "slowtools3", "toolsfail"],
            "hang, noargs, slow4 and slowtools5 timed out"
        );
        assert_eq!(got["toolsfail"].status, Status::Degraded);
    }

    #[test]
    fn a_failure_that_says_nothing_about_why_is_not_settled() {
        for text in [
            "a: x - ✘ Failed to connect\n",
            "a: x - ! Connected · tools fetch failed\n",
            "a: x - ✘ Failed to connect — \n",
        ] {
            assert_eq!(one(text, "a"), None, "{text:?}");
        }
    }

    #[test]
    fn however_a_timeout_is_worded_it_is_not_settled() {
        for issue in [
            "Request timed out",
            "connection timed out after 3000ms",
            "Timeout waiting for the server",
            "operation TIMED OUT",
            "the server did not answer in time: timeout",
        ] {
            let text = format!("a: x - ✘ Failed to connect — {issue}\n");
            assert_eq!(one(&text, "a"), None, "{issue}");
        }
    }

    #[test]
    fn an_answer_that_needs_no_connection_is_settled_without_a_reason() {
        for (text, status) in [
            ("a: x - ✔ Connected\n", Status::Connected),
            ("a: x - ! Needs authentication\n", Status::NeedsAuth),
            (
                "a: x - ⏸ Pending approval (run `claude` to approve)\n",
                Status::PendingApproval,
            ),
        ] {
            assert_eq!(one(text, "a").map(|c| c.status), Some(status), "{text:?}");
        }
    }

    #[test]
    fn an_open_answer_says_whether_the_server_ever_answered_the_connection() {
        let got = parse_list(LAB_SHORT, &["hang", "noargs", "slow4", "slowtools5", "ok"]);
        assert_eq!(
            got["hang"],
            Listed::Open {
                never_connected: true
            }
        );
        assert_eq!(
            got["noargs"],
            Listed::Open {
                never_connected: true
            }
        );
        assert_eq!(
            got["slow4"],
            Listed::Open {
                never_connected: false
            }
        );
        assert_eq!(
            got["slowtools5"],
            Listed::Open {
                never_connected: false
            }
        );
        assert!(matches!(got["ok"], Listed::Settled(_)));
        // A failure without a reason is open too, and not taken for a hang.
        let got = parse_list("a: x - ✘ Failed to connect\n", &["a"]);
        assert_eq!(
            got["a"],
            Listed::Open {
                never_connected: false
            }
        );
    }
}
