//! Removes secrets from diagnostic text before it is shown or copied. Over-redacting is fine; leaking is not.
//! The patterns use the `regex` crate, which runs in linear time, so hostile input cannot make it hang.

use regex::Regex;
use std::sync::LazyLock;

/// Applied in order. Each replacement keeps the label and hides the value, so the text stays readable.
const RULES: &[(&str, &str)] = &[
    // Whole header and environment objects, quoted (JSON) and bare (`Environment: {...}`).
    (
        r#"(?i)("(?:headers|env|environment|http_headers)"\s*:\s*)\{[^{}]*\}"#,
        "${1}{***}",
    ),
    (r#"(?i)(\b(?:headers|env)\s*:\s*)\{[^{}]*\}"#, "${1}{***}"),
    // Authentication headers: everything after the name up to the end of the value.
    (
        r#"(?i)\b(authorization|proxy-authorization|x-api-key|api-key|cookie|set-cookie)(["']?\s*[:=]\s*["']?)[^"',}\r\n]+"#,
        "${1}${2}***",
    ),
    (r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}", "Bearer ***"),
    // Names that say they hold a secret, whatever the separator.
    (
        r#"(?i)\b([a-z0-9_-]*(?:token|secret|password|passwd|api[_-]?key|apikey|private[_-]?key|code[_-]?verifier)["']?\s*[:=]\s*["']?)[^\s"',&}]+"#,
        "${1}***",
    ),
    // OAuth redirect parameters.
    (
        r#"(?i)([?&](?:code|state|code_verifier)=)[^&\s"']+"#,
        "${1}***",
    ),
    // user:password@ in a URL.
    (
        r#"(?i)\b((?:https?|wss?)://)[^/\s:@"']+:[^/\s@"']+@"#,
        "${1}***@",
    ),
    // Token shapes that are secrets wherever they appear.
    (
        r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}",
        "***",
    ),
    (r"\bsk-[A-Za-z0-9_-]{16,}", "sk-***"),
    (r"\bgh[pousr]_[A-Za-z0-9]{20,}", "***"),
    (r"\bxox[abprs]-[A-Za-z0-9-]{10,}", "***"),
    (r"\bAKIA[0-9A-Z]{16}\b", "***"),
    (r"\b[0-9a-fA-F]{32,}\b", "***"),
];

static COMPILED: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|(pattern, with)| (Regex::new(pattern).expect("valid redaction pattern"), *with))
        .collect()
});

/// `Environment: {"HTTP_PROXY":"not set", ...}` is a summary Claude prints for HTTP servers. Whether a variable
/// is set is useful for a proxy problem; its value is not, and may hold credentials.
static ENV_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\benvironment\s*:\s*)(\{[^{}]*\})"#).expect("valid pattern")
});
static ENV_PAIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""([^"]+)"\s*:\s*"([^"]*)""#).expect("valid pattern"));

fn hide_environment_values(text: &str) -> String {
    ENV_SUMMARY
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let pairs = ENV_PAIR.replace_all(&caps[2], |pair: &regex::Captures<'_>| {
                let value = &pair[2];
                if matches!(value, "not set" | "set" | "default" | "***") {
                    pair[0].to_owned()
                } else {
                    format!("\"{}\":\"***\"", &pair[1])
                }
            });
            format!("{}{}", &caps[1], pairs)
        })
        .into_owned()
}

pub(crate) fn redact(text: &str) -> String {
    let mut out = hide_environment_values(text);
    for (re, with) in COMPILED.iter() {
        out = re.replace_all(&out, *with).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gone(input: &str, secret: &str) {
        let out = redact(input);
        assert!(
            !out.contains(secret),
            "{secret:?} survived in {out:?} (from {input:?})"
        );
    }

    #[test]
    fn authorization_headers_in_every_shape() {
        gone(
            "Authorization: Bearer abc.def.ghi-123456",
            "abc.def.ghi-123456",
        );
        gone("authorization=Basic dXNlcjpwYXNz", "dXNlcjpwYXNz");
        gone(
            r#"{"Authorization":"Bearer TOKEN-VALUE-1234"}"#,
            "TOKEN-VALUE-1234",
        );
        gone("x-api-key: SUPERSECRETKEY99", "SUPERSECRETKEY99");
        gone("Cookie: session=abcdef123456; other=zzz", "abcdef123456");
        gone("proxy-authorization: Basic Zm9vOmJhcg==", "Zm9vOmJhcg==");
    }

    #[test]
    fn bearer_tokens_anywhere() {
        gone(
            "sent header bearer abcdef0123456789xyz to server",
            "abcdef0123456789xyz",
        );
        gone(
            "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl",
            "eyJhbGciOiJIUzI1NiJ9",
        );
    }

    #[test]
    fn secret_looking_names_whatever_the_separator() {
        for (input, secret) in [
            ("API_KEY=LEAK-1234567", "LEAK-1234567"),
            ("MYAPP_API_KEY: LEAK-1234567", "LEAK-1234567"),
            (r#""client_secret":"LEAK-1234567""#, "LEAK-1234567"),
            ("password=hunter2hunter2", "hunter2hunter2"),
            ("access_token=LEAK-1234567&x=1", "LEAK-1234567"),
            ("refresh_token: LEAK-1234567", "LEAK-1234567"),
            ("GITHUB_TOKEN=LEAK-1234567", "LEAK-1234567"),
            ("code_verifier=LEAK-1234567", "LEAK-1234567"),
        ] {
            gone(input, secret);
        }
    }

    #[test]
    fn oauth_query_parameters() {
        gone(
            "https://x.example.com/cb?code=AUTHCODE123&state=STATE456",
            "AUTHCODE123",
        );
        gone(
            "https://x.example.com/cb?code=AUTHCODE123&state=STATE456",
            "STATE456",
        );
    }

    #[test]
    fn url_credentials() {
        let out = redact("connecting to https://user:p4ssw0rd@host.example.com/mcp now");
        assert!(!out.contains("p4ssw0rd") && !out.contains("user:"), "{out}");
        assert!(
            out.contains("host.example.com/mcp"),
            "the rest of the URL stays: {out}"
        );
    }

    #[test]
    fn whole_header_and_environment_objects() {
        let out = redact(
            r#"HTTP transport options: {"url":"http://h","headers":{"User-Agent":"x","X-Custom":"CUSTOMVALUE1"}}"#,
        );
        assert!(!out.contains("CUSTOMVALUE1"), "{out}");
        let out = redact(r#"Environment: {"NODE_OPTIONS":"not set","SOMETHING":"ODDVALUE1"} done"#);
        assert!(!out.contains("ODDVALUE1"), "{out}");
        let out = redact(r#"spawn {"env":{"FOO":"BARVALUE1"},"command":"npx"}"#);
        assert!(!out.contains("BARVALUE1") && out.contains("npx"), "{out}");
    }

    #[test]
    fn a_proxy_environment_summary_keeps_set_or_not_but_never_values() {
        // Claude prints this for HTTP servers; whether a proxy is configured is the useful part.
        let out = redact(
            r#"MCP server "x": Environment: {"NODE_OPTIONS":"not set","HTTP_PROXY":"http://u:p4ss@proxy.corp:8080","HTTPS_PROXY":"set","UV_THREADPOOL_SIZE":"default"}"#,
        );
        assert!(out.contains(r#""NODE_OPTIONS":"not set""#), "{out}");
        assert!(out.contains(r#""HTTPS_PROXY":"set""#), "{out}");
        assert!(out.contains(r#""UV_THREADPOOL_SIZE":"default""#), "{out}");
        assert!(out.contains("HTTP_PROXY"), "the name stays: {out}");
        assert!(
            !out.contains("p4ss") && !out.contains("proxy.corp"),
            "{out}"
        );
    }

    #[test]
    fn well_known_token_shapes() {
        for secret in [
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl",
            "sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUV",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "xoxb-1234567890-abcdefghij",
            "AKIAIOSFODNN7EXAMPLE",
            "3af80102dbd95c6b2d44754855d60231273336825c5374948ed964056cac5f9b",
        ] {
            gone(&format!("value is {secret} here"), secret);
        }
    }

    #[test]
    fn ordinary_diagnostics_are_left_alone() {
        for line in [
            "MCP server \"bad-stdio\": Starting connection with timeout of 30000ms",
            "MCP server \"bad-stdio\": Connection failed after 193ms (CONNECTION_CLOSED): Connection closed",
            "'definitely-not-a-real-command-xyz' is not recognized as an internal or external command",
            "MCP server \"remote\": No token data found",
            "Parsed URL: host=127.0.0.1, port=9, protocol=http:",
            "C:\\Users\\someone\\AppData\\Local\\Temp\\x.log",
            "ECONNREFUSED: Unable to connect. Is the computer able to access the url?",
            "2026-10-17T20:53:30.643Z [DEBUG] Initializing HTTP transport to http://127.0.0.1:9/mcp",
        ] {
            assert_eq!(redact(line), line);
        }
    }

    #[test]
    fn redaction_is_idempotent_and_unicode_safe() {
        let input = "é日本 first, API_KEY=zzzzzzzz https://u:pw@h/x then Authorization: Bearer abcdefghijkl";
        let once = redact(input);
        assert_eq!(redact(&once), once);
        assert!(once.contains("é日本"));
    }

    #[test]
    fn the_hostile_fixture_leaks_nothing() {
        let out = redact(include_str!("../../../../fixtures/mcp-get/hostile.txt"));
        assert!(!out.contains("S3CRET"), "{out}");
    }

    #[test]
    fn a_megabyte_of_hostile_text_is_handled_quickly() {
        let nasty = format!(
            "{}{}",
            "Authorization: Bearer ".repeat(20_000),
            "a=".repeat(200_000)
        );
        let started = std::time::Instant::now();
        let out = redact(&nasty);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        assert!(!out.is_empty());
    }
}
