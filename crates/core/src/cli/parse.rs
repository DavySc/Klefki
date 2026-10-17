//! Reads the output of `claude mcp get <name>`. The output is untrusted text: only the status and a short
//! issue line are kept, everything else (environment values, headers, URLs) is dropped on purpose.
use crate::api::Status;
use crate::sanitize::{clip, sanitize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checked {
    pub status: Status,
    /// The `Issue:` line, sanitised and clipped.
    pub detail: Option<String>,
}

const MAX_DETAIL_CHARS: usize = 200;

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
}
