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
