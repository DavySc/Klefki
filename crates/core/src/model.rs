//! Domain types. Targets for writes are `User | Local` only; project scope is not expressible.
use std::fmt;

/// Where klefki may create a server: for the user, or locally in a project Claude Code knows (by its path). There
/// is deliberately no project scope: that lives in the repository's `.mcp.json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    User,
    Local(String),
}

const RESERVED: &[&str] = &[
    "__proto__",
    "constructor",
    "prototype",
    "toString",
    "hasOwnProperty",
];

/// Which server a secret or a piece of metadata belongs to: its scope, the project as `.claude.json` spells it,
/// and its name, as they are on disk (not as shown). The same encoding as v1 of the MCP manager this replaces, so
/// what v1 stored can be found again: base64url, without padding, of `{"s":<scope>,"p":<project or null>,"n":<name>}`
/// written as JavaScript's `JSON.stringify` writes it. Opaque and safe as a key: only `A-Z a-z 0-9 - _`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(String);

impl ConnectionId {
    /// An empty project is no project, as in v1.
    pub fn new(scope: crate::api::Scope, project: Option<&str>, name: &str) -> Self {
        #[derive(serde::Serialize)]
        struct Parts<'a> {
            s: &'a str,
            p: Option<&'a str>,
            n: &'a str,
        }
        let payload = serde_json::to_string(&Parts {
            s: scope_word(scope),
            p: project.filter(|p| !p.is_empty()),
            n: name,
        })
        .unwrap_or_default();
        Self(base64url(payload.as_bytes()))
    }

    /// The scope, project and name an id was made from; `None` for anything that is not a Claude Code id.
    pub fn parse(id: &str) -> Option<(crate::api::Scope, Option<String>, String)> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Parts {
            s: String,
            p: Option<String>,
            n: String,
        }
        let parts: Parts = serde_json::from_slice(&unbase64url(id)?).ok()?;
        let scope = match parts.s.as_str() {
            "user" => crate::api::Scope::User,
            "local" => crate::api::Scope::Local,
            "project" => crate::api::Scope::Project,
            "plugin" => crate::api::Scope::Plugin,
            "managed" => crate::api::Scope::Managed,
            "claudeai" => crate::api::Scope::ClaudeAi,
            _ => return None,
        };
        (!parts.n.is_empty()).then_some((scope, parts.p.filter(|p| !p.is_empty()), parts.n))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An id read back from where it was stored, such as the secret store's list: `None` unless it is one
    /// [`ConnectionId::new`] makes.
    pub(crate) fn from_stored(id: &str) -> Option<Self> {
        let (scope, project, name) = Self::parse(id)?;
        let id = Self(id.to_owned());
        (Self::new(scope, project.as_deref(), &name) == id).then_some(id)
    }
}

fn scope_word(scope: crate::api::Scope) -> &'static str {
    match scope {
        crate::api::Scope::User => "user",
        crate::api::Scope::Local => "local",
        crate::api::Scope::Project => "project",
        crate::api::Scope::Plugin => "plugin",
        crate::api::Scope::Managed => "managed",
        crate::api::Scope::ClaudeAi => "claudeai",
    }
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// RFC 4648 base64url without padding, as Node's `Buffer.toString('base64url')` writes it.
fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(BASE64URL[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The bytes of unpadded base64url; `None` for anything else.
fn unbase64url(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = BASE64URL.iter().position(|&a| a == c)? as u32;
            n |= v << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// A server name that is safe to pass to the Claude CLI: `^[A-Za-z0-9_-]{1,64}$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    BadChar(char),
    /// A leading `-` would be read as an option by the Claude CLI.
    LeadingDash,
    Reserved,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "name is empty"),
            Self::TooLong => write!(f, "name is longer than 64 characters"),
            Self::BadChar(c) => write!(f, "character {c:?} is not allowed (use A-Z a-z 0-9 _ -)"),
            Self::LeadingDash => write!(f, "name must not start with a dash"),
            Self::Reserved => write!(f, "name is reserved"),
        }
    }
}

impl ServerName {
    pub fn parse(s: &str) -> Result<Self, NameError> {
        if s.is_empty() {
            return Err(NameError::Empty);
        }
        if s.chars().count() > 64 {
            return Err(NameError::TooLong);
        }
        if let Some(c) = s
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
        {
            return Err(NameError::BadChar(c));
        }
        if s.starts_with('-') {
            return Err(NameError::LeadingDash);
        }
        if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s)) {
            return Err(NameError::Reserved);
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made by v1's own `makeId` (Source code/src/main/services/connections.js), run with Node 22. The two
    /// example names were later renamed, their ids made again with the same encoding (`JSON.stringify`, base64url).
    const V1_IDS: [(&str, Option<&str>, &str, &str); 8] = [
        (
            "user",
            None,
            "github",
            "eyJzIjoidXNlciIsInAiOm51bGwsIm4iOiJnaXRodWIifQ",
        ),
        (
            "local",
            Some("C:\\Users\\me\\Source\\Repos\\x"),
            "REMOTE_1",
            "eyJzIjoibG9jYWwiLCJwIjoiQzpcXFVzZXJzXFxtZVxcU291cmNlXFxSZXBvc1xceCIsIm4iOiJSRU1PVEVfMSJ9",
        ),
        (
            "local",
            Some("C:/Users/me/work"),
            "acme-prod",
            "eyJzIjoibG9jYWwiLCJwIjoiQzovVXNlcnMvbWUvd29yayIsIm4iOiJhY21lLXByb2QifQ",
        ),
        (
            "project",
            Some("/home/me/work/app"),
            "docs",
            "eyJzIjoicHJvamVjdCIsInAiOiIvaG9tZS9tZS93b3JrL2FwcCIsIm4iOiJkb2NzIn0",
        ),
        (
            "local",
            Some("/home/me/wörk/日本"),
            "naïve \"quoted\" \\ name",
            "eyJzIjoibG9jYWwiLCJwIjoiL2hvbWUvbWUvd8O2cmsv5pel5pysIiwibiI6Im5hw692ZSBcInF1b3RlZFwiIFxcIG5hbWUifQ",
        ),
        (
            "user",
            None,
            "tab\there\nnewline\u{1}\u{7f}\u{2028}/slash",
            "eyJzIjoidXNlciIsInAiOm51bGwsIm4iOiJ0YWJcdGhlcmVcbm5ld2xpbmVcdTAwMDF_4oCoL3NsYXNoIn0",
        ),
        (
            "local",
            Some(""),
            "empty-project",
            "eyJzIjoibG9jYWwiLCJwIjpudWxsLCJuIjoiZW1wdHktcHJvamVjdCJ9",
        ),
        (
            "user",
            None,
            "🦀",
            "eyJzIjoidXNlciIsInAiOm51bGwsIm4iOiLwn6aAIn0",
        ),
    ];

    fn scope(s: &str) -> crate::api::Scope {
        match s {
            "user" => crate::api::Scope::User,
            "local" => crate::api::Scope::Local,
            _ => crate::api::Scope::Project,
        }
    }

    #[test]
    fn connection_ids_are_the_ones_v1_made() {
        // The same id keeps v1's stored secrets and metadata usable (PLAN.md: "the same encoding as v1").
        for (s, project, name, want) in V1_IDS {
            assert_eq!(
                ConnectionId::new(scope(s), project, name).as_str(),
                want,
                "{s} {project:?} {name:?}"
            );
        }
    }

    #[test]
    fn a_connection_id_reads_back_into_its_parts() {
        for (s, project, name, id) in V1_IDS {
            let parts = ConnectionId::parse(id).unwrap();
            assert_eq!(
                parts,
                (
                    scope(s),
                    project.filter(|p| !p.is_empty()).map(str::to_owned),
                    name.to_owned()
                )
            );
        }
        // Not ids: other alphabets, padding, missing parts, an unknown scope, a Codex id (v1's other target).
        for bad in [
            "",
            "not base64!",
            "eyJzIjoidXNlciJ9",
            "eyJzIjoidXNlciIsInAiOm51bGwsIm4iOiJnaXRodWIifQ==",
            "eyJ0IjoiY29kZXgiLCJzIjoidXNlciIsInAiOm51bGwsIm4iOiJ4In0",
        ] {
            assert_eq!(ConnectionId::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_connection_id_is_safe_to_use_as_a_key() {
        for (s, project, name, _) in V1_IDS {
            let id = ConnectionId::new(scope(s), project, name);
            assert!(
                id.as_str()
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "{id:?}"
            );
        }
    }

    #[test]
    fn accepts_valid_names() {
        for n in ["gateway", "a", "my_server-2", &"x".repeat(64)] {
            let name = ServerName::parse(n).expect(n);
            assert_eq!(name.as_str(), n);
        }
    }

    #[test]
    fn rejects_empty_and_long() {
        assert_eq!(ServerName::parse(""), Err(NameError::Empty));
        assert_eq!(ServerName::parse(&"x".repeat(65)), Err(NameError::TooLong));
    }

    #[test]
    fn rejects_injection_characters() {
        for n in [
            "a b", "a&b", "a|b", "a^b", "a%b", "a\"b", "a!b", "a\nb", "a/b", "a.b", "é", "a;b",
            "$(x)",
        ] {
            assert!(
                matches!(ServerName::parse(n), Err(NameError::BadChar(_))),
                "{n:?}"
            );
        }
    }

    #[test]
    fn rejects_names_the_cli_would_read_as_options() {
        // Passed as `claude mcp get <name>`, a leading dash would be parsed as a flag.
        for n in ["-h", "--help", "-", "--", "-x", "_-ok-", "-a"] {
            let parsed = ServerName::parse(n);
            if n.starts_with('-') {
                assert_eq!(parsed, Err(NameError::LeadingDash), "{n:?}");
            } else {
                assert!(
                    parsed.is_ok(),
                    "{n:?} should be fine: a dash inside is allowed"
                );
            }
        }
    }

    #[test]
    fn rejects_reserved_case_insensitively() {
        assert_eq!(ServerName::parse("__proto__"), Err(NameError::Reserved));
        assert_eq!(ServerName::parse("Constructor"), Err(NameError::Reserved));
    }
}
