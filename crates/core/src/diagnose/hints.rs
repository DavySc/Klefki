//! Turns diagnostic text into plain-language explanations. The text is only matched against fixed patterns and
//! never executed or followed. Patterns come from messages seen in real Claude Code output and from the MCP
//! debugging guide (protocol errors -32022, -32021 and -32602).

use crate::engine::Focus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintKind {
    CommandNotFound,
    MissingFile,
    PackageInstallFailed,
    PermissionDenied,
    ConnectionRefused,
    HostNotFound,
    CertificateProblem,
    TimedOut,
    NeedsAuthentication,
    Forbidden,
    UrlNotFound,
    ServerError,
    ProtocolVersion,
    MissingClientCapability,
    InvalidParams,
    ExitedImmediately,
}

impl HintKind {
    #[cfg(test)]
    pub(crate) const ALL: [HintKind; 16] = [
        HintKind::CommandNotFound,
        HintKind::MissingFile,
        HintKind::PackageInstallFailed,
        HintKind::PermissionDenied,
        HintKind::ConnectionRefused,
        HintKind::HostNotFound,
        HintKind::CertificateProblem,
        HintKind::TimedOut,
        HintKind::NeedsAuthentication,
        HintKind::Forbidden,
        HintKind::UrlNotFound,
        HintKind::ServerError,
        HintKind::ProtocolVersion,
        HintKind::MissingClientCapability,
        HintKind::InvalidParams,
        HintKind::ExitedImmediately,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::CommandNotFound => "The command was not found",
            Self::MissingFile => "A file or module is missing",
            Self::PackageInstallFailed => "Installing the package failed",
            Self::PermissionDenied => "Permission was denied",
            Self::ConnectionRefused => "Nothing is listening at that address",
            Self::HostNotFound => "Can't reach the server's host",
            Self::CertificateProblem => "The server's certificate is not trusted",
            Self::TimedOut => "The server took too long to answer",
            Self::NeedsAuthentication => "The server needs you to sign in",
            Self::Forbidden => "The server refused access",
            Self::UrlNotFound => "The URL was not found on the server",
            Self::ServerError => "The server reported an error",
            Self::ProtocolVersion => "Client and server disagree on the protocol version",
            Self::MissingClientCapability => "The server needs a feature this client did not offer",
            Self::InvalidParams => "The request was rejected as malformed",
            Self::ExitedImmediately => "The server stopped before it answered",
        }
    }

    /// What to do, one step per line, the most likely fix first.
    pub(crate) fn help(self) -> &'static [&'static str] {
        match self {
            Self::CommandNotFound => &[
                "Install the program, or put its full path in the command.",
                "If it works in your terminal, the PATH there may differ from the one Claude Code sees.",
            ],
            Self::MissingFile => &[
                "Check the paths in the arguments.",
                "Claude Code may start servers from another working directory than you expect, so use absolute paths.",
            ],
            Self::PackageInstallFailed => &[
                "npx, uvx or a similar launcher could not download or install the server's package.",
                "Check the package name and version, your network or proxy, and the registry settings.",
            ],
            Self::PermissionDenied => &[
                "Check that the program is executable and that your user can read the files it needs.",
            ],
            Self::ConnectionRefused => &[
                "Check that the server is running.",
                "Check the host name and port in the URL.",
            ],
            Self::HostNotFound => &[
                "You are probably offline or not connected to your VPN: check those first.",
                "If both are fine, check the URL for typos.",
            ],
            Self::CertificateProblem => &[
                "On a company network this is often a TLS-inspecting proxy.",
                "Its certificate authority has to be trusted by Node, for example through NODE_EXTRA_CA_CERTS.",
            ],
            Self::TimedOut => &[
                "The server may be slow to start, or blocked by a firewall or proxy.",
                "Try again, and check the server's own logs.",
            ],
            Self::NeedsAuthentication => &[
                "The server rejected the request, or no token is stored for it.",
                "Use Re-authorize to sign in again.",
            ],
            Self::Forbidden => &["Check that your account has access to this server."],
            Self::UrlNotFound => &[
                "The server answered, but not at this path.",
                "Check the path, such as /mcp, against the server's documentation.",
            ],
            Self::ServerError => &[
                "The server may be down or restarting: try again later.",
                "Check the server's own logs.",
            ],
            Self::ProtocolVersion => &[
                "Update the server or Claude Code so they share a protocol version.",
                "The error lists the versions the server supports.",
            ],
            Self::MissingClientCapability => &[
                "The server asked for a client feature, such as elicitation, that Claude Code did not offer.",
                "Use a Claude Code version that supports it, or a server version that does not need it.",
            ],
            Self::InvalidParams => &[
                "This usually means the client and server versions do not match.",
                "Update both, then try again.",
            ],
            Self::ExitedImmediately => &[
                "The program may not exist, or it exited straight away without saying why.",
                "Run Diagnose, or the same command in a terminal, to see what it printed.",
            ],
        }
    }

    /// The part of the server's command or URL the failure is about, and what to say under it.
    pub(crate) fn points_at(self) -> Option<(Focus, &'static str)> {
        match self {
            Self::CommandNotFound => {
                Some((Focus::Command, "not found on the PATH Claude Code uses"))
            }
            Self::ConnectionRefused => Some((Focus::Address, "nothing is listening here")),
            Self::HostNotFound => Some((Focus::Host, "this name could not be looked up")),
            Self::CertificateProblem => Some((Focus::Host, "its certificate is not trusted")),
            Self::TimedOut => Some((Focus::Host, "no answer in time")),
            Self::UrlNotFound => Some((Focus::Path, "not found on this server")),
            _ => None,
        }
    }

    /// The error codes this failure is reported with, as a client or server spells them.
    fn codes(self) -> &'static [&'static str] {
        match self {
            Self::CommandNotFound => &["ENOENT"],
            Self::MissingFile => &["MODULE_NOT_FOUND"],
            Self::PackageInstallFailed => &["ERESOLVE"],
            Self::PermissionDenied => &["EACCES", "EPERM"],
            Self::ConnectionRefused => &["ECONNREFUSED"],
            Self::HostNotFound => &["ENOTFOUND", "EAI_AGAIN"],
            Self::CertificateProblem => &[
                "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
                "SELF_SIGNED_CERT_IN_CHAIN",
                "DEPTH_ZERO_SELF_SIGNED_CERT",
                "CERT_HAS_EXPIRED",
                "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
                "CERT_UNTRUSTED",
            ],
            Self::TimedOut => &["ETIMEDOUT", "ESOCKETTIMEDOUT"],
            Self::NeedsAuthentication => &["401"],
            Self::Forbidden => &["403"],
            Self::UrlNotFound => &["404"],
            Self::ServerError => &["500", "501", "502", "503", "504"],
            Self::ProtocolVersion => &["-32022"],
            Self::MissingClientCapability => &["-32021"],
            Self::InvalidParams => &["-32602"],
            Self::ExitedImmediately => &["CONNECTION_CLOSED"],
        }
    }

    /// The first of this failure's codes that `text` contains as a whole word, to name it by.
    pub(crate) fn code_in(self, text: &str) -> Option<&'static str> {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        self.codes().iter().copied().find(|code| {
            text.match_indices(code).any(|(at, _)| {
                !text[..at].chars().next_back().is_some_and(word)
                    && !text[at + code.len()..].chars().next().is_some_and(word)
            })
        })
    }
}

use regex::RegexSet;
use std::sync::LazyLock;

/// In order of how specific the cause is; the first few that match are shown.
const PATTERNS: &[(HintKind, &str)] = &[
    (
        HintKind::CommandNotFound,
        r"(?i)is not recognized as an internal or external command|\bENOENT\b|command not found|cannot find the (?:file|path) specified",
    ),
    (
        HintKind::MissingFile,
        r"(?i)cannot find module|ModuleNotFoundError|no module named|MODULE_NOT_FOUND|can't open file",
    ),
    (
        HintKind::PackageInstallFailed,
        r"(?i)npm ERR!|npm error|\bERESOLVE\b|failed to install|could not install|uvx?: error",
    ),
    (
        HintKind::PermissionDenied,
        r"(?i)\bEACCES\b|\bEPERM\b|permission denied|access is denied",
    ),
    (
        HintKind::ConnectionRefused,
        r"(?i)ECONNREFUSED|connection refused|actively refused",
    ),
    (
        HintKind::HostNotFound,
        r"(?i)\bENOTFOUND\b|getaddrinfo|name or service not known|could not resolve host|EAI_AGAIN",
    ),
    (
        HintKind::CertificateProblem,
        r"(?i)UNABLE_TO_VERIFY_LEAF_SIGNATURE|SELF_SIGNED_CERT|self[- ]signed certificate|CERT_HAS_EXPIRED|certificate has expired|unable to get local issuer|ERR_TLS_CERT|CERT_UNTRUSTED",
    ),
    (
        HintKind::TimedOut,
        r"(?i)ETIMEDOUT|ESOCKETTIMEDOUT|timed out|timeout after|request timeout",
    ),
    (
        HintKind::NeedsAuthentication,
        r"(?i)\b401\b|unauthorized|no token data found|needs[- ]auth|invalid_token|authentication required|www-authenticate",
    ),
    (HintKind::Forbidden, r"(?i)\b403\b|forbidden"),
    // Only the status code: a bare "not found" also appears in "command not found" and many other messages.
    (HintKind::UrlNotFound, r"\b404\b"),
    (
        HintKind::ServerError,
        r"(?i)\b50[0-4]\b|bad gateway|service unavailable|internal server error",
    ),
    (
        HintKind::ProtocolVersion,
        r"(?i)UnsupportedProtocolVersion|-32022|unsupported protocol version",
    ),
    (
        HintKind::MissingClientCapability,
        r"(?i)MissingRequiredClientCapability|-32021",
    ),
    (HintKind::InvalidParams, r"(?i)-32602|invalid params"),
    (HintKind::ExitedImmediately, r"CONNECTION_CLOSED"),
];

/// All patterns in one automaton: the text is read once, not once per pattern.
static COMPILED: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new(PATTERNS.iter().map(|(_, pattern)| pattern)).expect("valid hint patterns")
});

const MAX_HINTS: usize = 4;

pub(crate) fn hints_for(text: &str) -> Vec<HintKind> {
    let matched: Vec<HintKind> = COMPILED
        .matches(text)
        .iter()
        .map(|i| PATTERNS[i].0)
        .collect();
    let has = |k: HintKind| matched.contains(&k);

    // One failure can leave traces of several patterns. These rules keep the explanation to the cause.
    let transport_failure = has(HintKind::ConnectionRefused)
        || has(HintKind::HostNotFound)
        || has(HintKind::CertificateProblem)
        || has(HintKind::TimedOut);
    let launch_failure = has(HintKind::CommandNotFound)
        || has(HintKind::MissingFile)
        || has(HintKind::PackageInstallFailed)
        || has(HintKind::PermissionDenied);

    matched
        .iter()
        .copied()
        .filter(|kind| match kind {
            // "No token data found" is logged for every HTTP server before it connects.
            HintKind::NeedsAuthentication => !transport_failure,
            // A package manager's 404 is about the package, not about the server's URL.
            HintKind::UrlNotFound => !has(HintKind::PackageInstallFailed),
            // "Connection closed" is only news when nothing explains why the process exited.
            HintKind::ExitedImmediately => !launch_failure && !transport_failure,
            _ => true,
        })
        .take(MAX_HINTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::HintKind::*;
    use super::*;

    fn kinds(text: &str) -> Vec<HintKind> {
        hints_for(text)
    }

    #[test]
    fn a_missing_command_is_recognised_from_real_output() {
        let real = "MCP server \"bad-stdio\" Server stderr: 'definitely-not-a-real-command-xyz' is not recognized as an internal or external command,\\r\\noperable program or batch file.\nMCP server \"bad-stdio\" Connection failed (CONNECTION_CLOSED): Connection closed";
        assert_eq!(
            kinds(real),
            vec![CommandNotFound],
            "the exit is explained by the cause, not by itself"
        );
        assert_eq!(kinds("Error: spawn npx ENOENT"), vec![CommandNotFound]);
        assert_eq!(kinds("bash: foo: command not found"), vec![CommandNotFound]);
    }

    #[test]
    fn a_refused_connection_is_not_blamed_on_authentication() {
        // Real output for an HTTP server that is down: "No token data found" is printed before the failure.
        let real = "MCP server \"remote-down\": No token data found\nMCP server \"remote-down\": version negotiation probe failed on the http transport\nECONNREFUSED: ECONNREFUSED: Unable to connect. Is the computer able to access the url?";
        assert_eq!(kinds(real), vec![ConnectionRefused]);
    }

    #[test]
    fn each_failure_has_its_own_hint() {
        for (text, want) in [
            ("getaddrinfo ENOTFOUND gateway.example.com", HostNotFound),
            ("Error: getaddrinfo EAI_AGAIN host", HostNotFound),
            (
                "UNABLE_TO_VERIFY_LEAF_SIGNATURE: unable to verify the first certificate",
                CertificateProblem,
            ),
            (
                "self signed certificate in certificate chain",
                CertificateProblem,
            ),
            ("CERT_HAS_EXPIRED", CertificateProblem),
            ("request failed: ETIMEDOUT", TimedOut),
            ("the connection timed out after 30 seconds", TimedOut),
            ("HTTP 401 Unauthorized", NeedsAuthentication),
            (
                "invalid_token: the access token expired",
                NeedsAuthentication,
            ),
            ("HTTP 403 Forbidden", Forbidden),
            ("HTTP 404 Not Found for /mcp", UrlNotFound),
            ("upstream returned 502 Bad Gateway", ServerError),
            (
                "Error: Cannot find module '/srv/app/server.js'",
                MissingFile,
            ),
            ("ModuleNotFoundError: No module named 'mcp'", MissingFile),
            ("EACCES: permission denied, open '/x'", PermissionDenied),
            ("Access is denied.", PermissionDenied),
            ("UnsupportedProtocolVersionError (-32022)", ProtocolVersion),
            (
                "MissingRequiredClientCapabilityError: -32021",
                MissingClientCapability,
            ),
            ("JSON-RPC error -32602: Invalid params", InvalidParams),
        ] {
            assert_eq!(kinds(text), vec![want], "{text:?}");
        }
    }

    #[test]
    fn needs_authentication_when_nothing_else_explains_the_failure() {
        assert_eq!(
            kinds("MCP server \"x\": No token data found\nneeds-auth"),
            vec![NeedsAuthentication]
        );
    }

    #[test]
    fn a_package_manager_failure_is_not_mistaken_for_a_bad_url() {
        let npm = "npm error 404 Not Found - GET https://registry.npmjs.org/@acme%2fnope - Not found\nnpm error code E404";
        assert_eq!(kinds(npm), vec![PackageInstallFailed]);
        assert_eq!(kinds("npm ERR! code ERESOLVE"), vec![PackageInstallFailed]);
    }

    #[test]
    fn an_immediate_exit_with_no_better_explanation_says_so() {
        assert_eq!(
            kinds("MCP server \"x\" Connection failed (CONNECTION_CLOSED): Connection closed"),
            vec![ExitedImmediately]
        );
    }

    #[test]
    fn healthy_and_ordinary_lines_produce_no_hints() {
        for line in [
            "MCP server \"x\": Starting connection with timeout of 30000ms",
            "MCP server \"x\": Initializing HTTP transport to http://127.0.0.1:9/mcp",
            "MCP server \"x\": Node version: v26.3.0, Platform: win32",
            "[DEBUG] Error log sink initialized",
            "MCP server \"x\": Connection established with capabilities: {\"tools\":{}}",
            "",
        ] {
            assert_eq!(kinds(line), vec![], "{line:?}");
        }
    }

    #[test]
    fn matching_ignores_case_and_does_not_mind_noise() {
        assert_eq!(kinds("ECONNREFUSED"), vec![ConnectionRefused]);
        assert_eq!(kinds("econnrefused"), vec![ConnectionRefused]);
        assert_eq!(
            kinds("\u{1b}[31mECONNREFUSED\u{1b}[0m \u{202e}"),
            vec![ConnectionRefused]
        );
    }

    #[test]
    fn at_most_four_distinct_hints_in_a_useful_order() {
        let everything = "ENOENT ECONNREFUSED ENOTFOUND UNABLE_TO_VERIFY_LEAF_SIGNATURE ETIMEDOUT 401 403 404 502 EACCES -32022 -32021 -32602 CONNECTION_CLOSED Cannot find module npm ERR!";
        let got = kinds(everything);
        assert!(got.len() <= 4, "{got:?}");
        let unique: std::collections::BTreeSet<String> =
            got.iter().map(|k| format!("{k:?}")).collect();
        assert_eq!(unique.len(), got.len(), "no duplicates: {got:?}");
        assert_eq!(got[0], CommandNotFound, "the most specific cause first");
    }

    #[test]
    fn every_hint_has_a_title_and_advice_worth_reading() {
        for kind in HintKind::ALL {
            assert!(kind.title().len() >= 8, "{kind:?}");
            // Each step reads on its own, and together they say enough to act on.
            let help = kind.help();
            assert!(!help.is_empty(), "{kind:?}");
            for line in help {
                assert!(line.len() >= 20, "{kind:?}: {line:?}");
                assert!(line.ends_with('.'), "{kind:?}: {line:?}");
            }
            assert!(help.join(" ").len() >= 40, "{kind:?}");
        }
        let titles: std::collections::BTreeSet<_> =
            HintKind::ALL.iter().map(|k| k.title()).collect();
        assert_eq!(titles.len(), HintKind::ALL.len(), "titles are distinct");
    }

    #[test]
    fn garbage_and_huge_input_are_handled() {
        let started = std::time::Instant::now();
        assert!(kinds(&"x".repeat(2_000_000)).is_empty());
        assert!(kinds(&"\u{0}\u{ffff}é日本".repeat(100_000)).is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
