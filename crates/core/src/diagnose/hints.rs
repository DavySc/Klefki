//! Turns diagnostic text into plain-language explanations. The text is only matched against fixed patterns and
//! never executed or followed. Patterns come from messages seen in real Claude Code output and from the MCP
//! debugging guide (protocol errors -32022, -32021 and -32602).

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
            Self::HostNotFound => "The host name does not resolve",
            Self::CertificateProblem => "The server's certificate is not trusted",
            Self::TimedOut => "The server took too long to answer",
            Self::NeedsAuthentication => "The server needs you to sign in",
            Self::Forbidden => "The server refused access",
            Self::UrlNotFound => "The URL was not found on the server",
            Self::ServerError => "The server reported an error",
            Self::ProtocolVersion => "Client and server disagree on the protocol version",
            Self::MissingClientCapability => "The server needs a feature this client did not offer",
            Self::InvalidParams => "The request was rejected as malformed",
            Self::ExitedImmediately => "The server started and then exited",
        }
    }

    pub(crate) fn advice(self) -> &'static str {
        match self {
            Self::CommandNotFound => {
                "Claude could not start the server because the program is not on the PATH it uses. Install it, or put its full path in the command. If it works in your terminal, the PATH there may differ from the one Claude sees."
            }
            Self::MissingFile => {
                "The server started but could not find a file or package it needs. Check the paths in the arguments. Claude may start servers from a different working directory than you expect, so use absolute paths."
            }
            Self::PackageInstallFailed => {
                "npx, uvx or a similar launcher could not download or install the server's package. Check the package name and version, your network or proxy, and the registry settings."
            }
            Self::PermissionDenied => {
                "The server or a file it uses could not be opened because of permissions. Check that the program is executable and that your user can read the files it needs."
            }
            Self::ConnectionRefused => {
                "The server did not accept the connection. Check that it is running, and that the host name, port and path in the URL are right."
            }
            Self::HostNotFound => {
                "The server's host name could not be found. Check the URL for typos, and whether you need a VPN or a different network to reach it."
            }
            Self::CertificateProblem => {
                "The TLS certificate could not be verified. On a company network this is often a TLS-inspecting proxy: its certificate authority has to be trusted by Node, for example through NODE_EXTRA_CA_CERTS."
            }
            Self::TimedOut => {
                "The connection or a request timed out. The server may be slow to start, blocked by a firewall or proxy, or stuck. Try again, and check the server's own logs."
            }
            Self::NeedsAuthentication => {
                "The server rejected the request, or no token is stored for it. Use Re-authorize to sign in again."
            }
            Self::Forbidden => {
                "The server understood the request but forbids it. Check that your account has access to this server."
            }
            Self::UrlNotFound => {
                "The server answered, but not at this path. Check the URL, especially the path such as /mcp, against the server's documentation."
            }
            Self::ServerError => {
                "The server answered with a 5xx error. It may be down or restarting. Try again later, and check the server's own logs."
            }
            Self::ProtocolVersion => {
                "The server does not support the MCP protocol version this client uses. Update the server or Claude Code so they share a version; the error lists the versions the server supports."
            }
            Self::MissingClientCapability => {
                "The server asked for a client capability, such as elicitation, that Claude Code did not declare. Use a client version that supports it, or a server version that does not require it."
            }
            Self::InvalidParams => {
                "The server rejected a request as invalid. This usually means the client and server versions do not match; update both, then try again."
            }
            Self::ExitedImmediately => {
                "Claude connected to the process, but it closed straight away without saying why. Run the same command in a terminal to see what it prints, and check its logs."
            }
        }
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
            assert!(kind.advice().len() >= 40, "{kind:?}");
            assert!(kind.advice().ends_with('.'), "{kind:?}");
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
