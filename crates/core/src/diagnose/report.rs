//! Turns the client's debug output for one server into something safe and readable: the lines about that
//! server, what the server itself printed, and plain-language hints. Everything here is pure text handling.
use super::hints::{HintKind, hints_for};
use super::redact::redact;
use crate::sanitize::{clip, sanitize};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Report {
    /// The client's log lines about this server, redacted and clipped, oldest first.
    pub client_log: Vec<String>,
    /// What the server process itself printed (its stderr), one line each.
    pub server_said: Vec<String>,
    pub hints: Vec<HintKind>,
}

const MAX_LOG_LINES: usize = 60;
const MAX_LINE_CHARS: usize = 300;
const FALLBACK_LINES: usize = 25;
const MAX_SERVER_LINES: usize = 20;

/// Markers after which the client logs what the stdio server itself printed on stderr.
const STDERR_MARKERS: &[&str] = &[
    "Server stderr: ",
    "stderr of the stdio server before restarting it: ",
];

/// `debug_text` is the whole debug file for one `mcp get`; `extra` is any other error text (such as the
/// `Issue:` line). Both are untrusted: they are sanitised and redacted before anything is derived from them.
pub(crate) fn build(debug_text: &str, server: &str, extra: &str) -> Report {
    let clean = redact(&sanitize(debug_text));
    let extra = redact(&sanitize(extra));

    // The client writes the server's name in quotes, and inside a quoted string with escaped quotes.
    let plain = format!("MCP server \"{server}\"");
    let escaped = format!("MCP server \\\"{server}\\\"");
    let about_server: Vec<&str> = clean
        .lines()
        .filter(|l| l.contains(&plain) || l.contains(&escaped))
        .collect();
    let selected: Vec<&str> = if about_server.is_empty() {
        let all: Vec<&str> = clean.lines().collect();
        all[all.len().saturating_sub(FALLBACK_LINES)..].to_vec()
    } else {
        about_server
    };

    let server_said = server_output(&selected);
    let evidence = format!(
        "{}\n{}\n{}",
        selected.join("\n"),
        server_said.join("\n"),
        extra
    );
    let hints = hints_for(&evidence);

    let client_log = selected[selected.len().saturating_sub(MAX_LOG_LINES)..]
        .iter()
        .map(|l| clip(l, MAX_LINE_CHARS))
        .collect();
    Report {
        client_log,
        server_said,
        hints,
    }
}

/// What the server printed, one line each, without repeats. The client logs it inside a quoted, escaped string.
fn server_output(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let Some((marker, at)) = STDERR_MARKERS
            .iter()
            .find_map(|m| line.find(m).map(|at| (m, at)))
        else {
            continue;
        };
        let rest = line[at + marker.len()..].trim_end();
        // Only the one quote that closes the logged string; an escaped quote inside the message stays.
        let rest = rest.strip_suffix('"').unwrap_or(rest);
        for said in unescape(rest)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            if !out.iter().any(|seen| seen == said) {
                out.push(clip(said, MAX_LINE_CHARS));
            }
            if out.len() >= MAX_SERVER_LINES {
                return out;
            }
        }
    }
    out
}

/// Undoes the JSON-style escaping the client applies when it logs a string.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('r') => out.push('\n'),
            Some('t') => out.push(' '),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STDIO: &str = include_str!("../../../../fixtures/debug/failed-stdio.log");
    const HTTP: &str = include_str!("../../../../fixtures/debug/failed-http.log");

    #[test]
    fn a_stdio_failure_shows_what_the_server_printed() {
        let r = build(STDIO, "bad-stdio", "");
        assert_eq!(
            r.server_said,
            vec![
                "'definitely-not-a-real-command-xyz' is not recognized as an internal or external command,",
                "operable program or batch file.",
            ],
            "the same message appears twice in the log; it is shown once"
        );
        assert_eq!(r.hints, vec![HintKind::CommandNotFound]);
        assert!(
            r.client_log
                .iter()
                .any(|l| l.contains("Connection failed after 246ms (CONNECTION_CLOSED)"))
        );
    }

    #[test]
    fn start_up_noise_is_left_out() {
        for (text, name) in [(STDIO, "bad-stdio"), (HTTP, "remote-down")] {
            let r = build(text, name, "");
            let joined = r.client_log.join("\n");
            for noise in [
                "CA certs",
                "Broken symlink",
                "MDM settings",
                "configureGlobalMTLS",
                "plugins",
            ] {
                assert!(
                    !joined.contains(noise),
                    "{name}: {noise} should be filtered out"
                );
            }
            assert!(
                r.client_log.iter().all(|l| l.contains(name)),
                "{name}: only lines about this server"
            );
        }
    }

    #[test]
    fn an_http_failure_keeps_the_transport_story_and_the_right_hint() {
        let r = build(HTTP, "remote-down", "");
        assert!(r.server_said.is_empty(), "an HTTP server has no stderr");
        let log = r.client_log.join("\n");
        assert!(
            log.contains("Initializing HTTP transport to http://127.0.0.1:9/mcp"),
            "{log}"
        );
        assert!(
            log.contains("HTTP_PROXY"),
            "the proxy environment is part of the story"
        );
        assert!(log.contains("ECONNREFUSED"));
        assert_eq!(
            r.hints,
            vec![HintKind::ConnectionRefused],
            "'No token data found' must not read as an auth problem"
        );
    }

    #[test]
    fn only_the_requested_server_is_reported() {
        let text = "2026-10-17T20:00:00.000Z [DEBUG] MCP server \"alpha\": Starting connection\n\
                    2026-10-17T20:00:00.100Z [DEBUG] MCP server \"beta\": HTTP Connection failed: ECONNREFUSED\n\
                    2026-10-17T20:00:00.200Z [ERROR] \"MCP server \\\"alpha\\\" Server stderr: oops from alpha\"\n";
        let r = build(text, "alpha", "");
        assert_eq!(r.client_log.len(), 2);
        assert!(r.client_log.iter().all(|l| l.contains("alpha")));
        assert_eq!(r.server_said, vec!["oops from alpha"]);
        assert!(
            r.hints.is_empty(),
            "beta's failure is not alpha's: {:?}",
            r.hints
        );
    }

    #[test]
    fn secrets_never_reach_the_report() {
        let text = "[DEBUG] MCP server \"s\": HTTP transport options: {\"headers\":{\"Authorization\":\"Bearer SECRETBEARER123\"}}\n\
                    [ERROR] \"MCP server \\\"s\\\" Server stderr: API_KEY=SECRETENV456 failed\\r\\ntoken=SECRETTOKEN789\"\n\
                    [DEBUG] MCP server \"s\": url https://user:SECRETPW000@host.example.com/mcp\n";
        let r = build(text, "s", "Authorization: Bearer SECRETEXTRA111");
        let everything = format!("{:?}", r);
        for secret in [
            "SECRETBEARER123",
            "SECRETENV456",
            "SECRETTOKEN789",
            "SECRETPW000",
            "SECRETEXTRA111",
        ] {
            assert!(
                !everything.contains(secret),
                "{secret} leaked: {everything}"
            );
        }
    }

    #[test]
    fn without_lines_about_the_server_the_tail_of_the_log_is_shown() {
        let text: String = (0..100)
            .map(|i| format!("2026-10-17T20:00:00.{i:03}Z [DEBUG] unrelated line {i}\n"))
            .collect();
        let r = build(&text, "ghost", "");
        assert_eq!(r.client_log.len(), 25);
        assert!(r.client_log.last().unwrap().contains("unrelated line 99"));
    }

    #[test]
    fn long_logs_keep_the_most_recent_lines_and_clip_long_ones() {
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("[DEBUG] MCP server \"s\": step {i}\n"));
        }
        text.push_str(&format!(
            "[DEBUG] MCP server \"s\": {}\n",
            "y".repeat(100_000)
        ));
        let r = build(&text, "s", "");
        assert!(r.client_log.len() <= 60, "{}", r.client_log.len());
        assert!(
            r.client_log.iter().all(|l| l.chars().count() <= 301),
            "lines are clipped"
        );
        assert!(
            r.client_log.iter().any(|l| l.contains("step 4999")),
            "the most recent lines are kept"
        );
    }

    #[test]
    fn terminal_escapes_and_bidi_are_stripped() {
        let text = "[DEBUG] MCP server \"s\": \u{1b}[31mred\u{1b}[0m \u{202e}evil\n";
        let r = build(text, "s", "");
        assert!(!r.client_log[0].contains('\u{1b}') && !r.client_log[0].contains('\u{202e}'));
        assert!(r.client_log[0].contains("red"));
    }

    #[test]
    fn the_status_detail_also_feeds_the_hints() {
        // No debug lines at all, but `mcp get` printed an Issue line.
        let r = build("", "s", "ECONNREFUSED: Unable to connect");
        assert_eq!(r.hints, vec![HintKind::ConnectionRefused]);
    }

    #[test]
    fn server_output_is_unescaped_deduplicated_and_capped() {
        let many: String = (0..50).map(|i| format!("line{i}\\n")).collect();
        let text = format!("[ERROR] \"MCP server \\\"s\\\" Server stderr: {many}\"\n");
        let r = build(&text, "s", "");
        assert_eq!(r.server_said.len(), 20);
        assert_eq!(r.server_said[0], "line0");
        let quoted = "[ERROR] \"MCP server \\\"s\\\" Server stderr: he said \\\"hi\\\"\"\n";
        assert_eq!(build(quoted, "s", "").server_said, vec!["he said \"hi\""]);
    }

    #[test]
    fn empty_and_garbage_input_do_not_panic() {
        assert_eq!(build("", "s", ""), Report::default());
        build("\u{0}\u{ffff}\n\n\n", "s", "");
        build(&"x\n".repeat(1_000_000), "s", "");
    }
}
