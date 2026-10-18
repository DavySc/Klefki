//! Spotting a secret typed straight into a definition's environment or headers, and what to write instead: a
//! reference such as `${GITHUB_TOKEN}`, which Claude Code fills in from its own environment, so the secret does not
//! sit in `~/.claude.json`. Only a hint; nothing here changes a value or keeps one.

/// The value to write instead of `value` for the variable or header called `name`, when `value` looks like a
/// secret that was typed in. `None` for an empty value, one that already is a `${VAR}` reference, the hidden
/// placeholder, and anything that does not look like a secret.
pub(crate) fn suggestion(name: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == super::entry::MASK || value.contains("${") {
        return None;
    }
    // "Bearer abc": the scheme stays, the credential goes.
    let (scheme, credential) = match value.split_once(' ') {
        Some((scheme, rest)) if SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) => {
            (Some(scheme), rest.trim())
        }
        _ => (None, value),
    };
    if !looks_secret(name, credential) {
        return None;
    }
    let reference = format!("${{{}}}", variable(name));
    Some(match scheme {
        Some(scheme) => format!("{scheme} {reference}"),
        None => reference,
    })
}

const SCHEMES: [&str; 3] = ["Bearer", "Basic", "Token"];
const PREFIXES: [&str; 14] = [
    "sk-",
    "sk_live_",
    "pk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xox",
    "AIza",
    "npm_",
    "eyJ",
];

fn looks_secret(name: &str, credential: &str) -> bool {
    if credential.len() < 8 || credential.contains(char::is_whitespace) {
        return false;
    }
    if let Some((_, rest)) = credential.split_once("://") {
        // An address is a secret only when it carries a password.
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        return authority
            .rsplit_once('@')
            .is_some_and(|(userinfo, _)| userinfo.contains(':'));
    }
    if credential.starts_with(['/', '~'])
        || credential.contains(['/', '\\']) && !is_base64(credential)
    {
        return false;
    }
    if is_known_shape(credential) {
        return true;
    }
    let plain = credential.chars().all(|c| c.is_ascii_digit())
        || ["true", "false", "yes", "no"]
            .iter()
            .any(|w| credential.eq_ignore_ascii_case(w));
    if plain {
        return false;
    }
    if sounds_secret(name) {
        // "oauth" or "production" is a setting; a credential mixes letters with digits or symbols.
        return credential.chars().any(|c| !c.is_ascii_alphabetic());
    }
    credential.len() >= 32
        && is_base64(credential)
        && credential.chars().any(|c| c.is_ascii_digit())
        && credential.chars().any(|c| c.is_ascii_alphabetic())
}

fn is_base64(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
}

fn is_known_shape(credential: &str) -> bool {
    let long = credential.len() >= 12;
    (long && PREFIXES.iter().any(|p| credential.starts_with(p)))
        || (credential.len() == 20
            && (credential.starts_with("AKIA") || credential.starts_with("ASIA"))
            && credential
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
}

fn sounds_secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // Without separators or invisible characters, so "api_key" and "t\u{202e}oken" read as they are meant.
    let joined: String = lower.chars().filter(char::is_ascii_alphanumeric).collect();
    const PARTS: [&str; 8] = [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "credential",
        "cookie",
        "passphrase",
    ];
    const WORDS: [&str; 4] = ["key", "auth", "pwd", "authorization"];
    PARTS.iter().any(|p| joined.contains(p))
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| WORDS.contains(&w))
}

/// `name` as an environment variable name: upper case letters, digits and `_`, not starting with a digit.
fn variable(name: &str) -> String {
    if name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("proxy-authorization")
    {
        return "AUTH_TOKEN".to_owned();
    }
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    match out.chars().next() {
        None => "SECRET".to_owned(),
        Some(c) if c.is_ascii_digit() => format!("_{out}"),
        Some(_) => out.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggests(name: &str, value: &str) -> Option<String> {
        suggestion(name, value)
    }

    #[test]
    fn a_secret_named_variable_with_a_real_looking_value_gets_a_reference() {
        assert_eq!(
            suggests("GITHUB_TOKEN", "abcd1234efgh5678").as_deref(),
            Some("${GITHUB_TOKEN}")
        );
        assert_eq!(
            suggests("db_password", "hunter2hunter2").as_deref(),
            Some("${DB_PASSWORD}")
        );
        assert_eq!(
            suggests("accessToken", "x7Yq-93kLmQ2").as_deref(),
            Some("${ACCESSTOKEN}")
        );
        assert_eq!(
            suggests("API-KEY", "0a1b2c3d4e5f").as_deref(),
            Some("${API_KEY}")
        );
    }

    #[test]
    fn well_known_token_shapes_are_caught_whatever_the_name_is() {
        for value in [
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0abcdefghij_klmnop",
            "sk-ant-api03-abcdefghijklmnop",
            "xoxb-123456789012-abcdefghijkl",
            "AKIAIOSFODNN7EXAMPLE",
            "glpat-abcdefghij0123456789",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.c2lnbmF0dXJl",
        ] {
            assert_eq!(suggests("X", value).as_deref(), Some("${X}"), "{value}");
        }
    }

    #[test]
    fn a_long_random_looking_value_is_caught_even_under_a_plain_name() {
        let value = "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2RlZg";
        assert_eq!(suggests("DATA", value).as_deref(), Some("${DATA}"));
    }

    #[test]
    fn the_scheme_of_an_authorization_header_stays() {
        assert_eq!(
            suggests("Authorization", "Bearer abcdef0123456789").as_deref(),
            Some("Bearer ${AUTH_TOKEN}")
        );
        assert_eq!(
            suggests("authorization", "Basic dXNlcjpwYXNzd29yZA==").as_deref(),
            Some("Basic ${AUTH_TOKEN}")
        );
        assert_eq!(
            suggests("Cookie", "session=abcdef0123456789").as_deref(),
            Some("${COOKIE}")
        );
        assert_eq!(
            suggests("X-Api-Key", "abcdef0123456789").as_deref(),
            Some("${X_API_KEY}")
        );
    }

    #[test]
    fn credentials_in_an_address_are_caught_but_plain_addresses_are_not() {
        assert!(
            suggests(
                "DATABASE_URL",
                "postgres://admin:s3cretpw@db.example.com/app"
            )
            .is_some()
        );
        assert_eq!(
            suggests("TOKEN_URL", "https://example.com/oauth/token"),
            None
        );
        assert_eq!(suggests("API_TOKEN_URL", "https://example.com/v1"), None);
    }

    #[test]
    fn things_that_only_sound_secret_are_left_alone() {
        for (name, value) in [
            ("TOKEN_LIMIT", "4096"),
            ("USE_AUTH", "true"),
            ("AUTH_MODE", "oauth"),
            ("KEY_FILE", "/home/me/.ssh/key.pem"),
            ("KEY_FILE", "C:\\Users\\me\\key.pem"),
            ("PASSWORD_MIN", "12"),
            ("LOG_LEVEL", "debug"),
            ("NODE_ENV", "production"),
            ("PATH", "/usr/local/bin:/usr/bin"),
            ("HOME", "/home/me"),
            ("MONKEY", "bananabananabanana"),
        ] {
            assert_eq!(suggests(name, value), None, "{name}={value}");
        }
    }

    #[test]
    fn a_reference_an_empty_value_and_the_hidden_placeholder_get_no_hint() {
        assert_eq!(suggests("GITHUB_TOKEN", "${GITHUB_TOKEN}"), None);
        assert_eq!(suggests("Authorization", "Bearer ${TOKEN}"), None);
        assert_eq!(suggests("GITHUB_TOKEN", ""), None);
        assert_eq!(suggests("GITHUB_TOKEN", "   "), None);
        assert_eq!(suggests("GITHUB_TOKEN", "<hidden>"), None);
    }

    #[test]
    fn the_variable_is_always_a_valid_name() {
        assert_eq!(
            suggests("1st token", "abcdef0123456789").as_deref(),
            Some("${_1ST_TOKEN}")
        );
        assert_eq!(
            suggests("my.secret!", "abcdef0123456789").as_deref(),
            Some("${MY_SECRET}")
        );
        for name in ["token é", "--token--", "t\u{202e}oken", "TOKEN\n"] {
            let s = suggests(name, "abcdef0123456789").unwrap_or_default();
            let var = s
                .strip_prefix("${")
                .and_then(|s| s.strip_suffix('}'))
                .unwrap_or("?");
            assert!(
                !var.is_empty()
                    && var
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    && !var.starts_with(|c: char| c.is_ascii_digit()),
                "{name:?} -> {s:?}"
            );
        }
    }

    #[test]
    fn the_suggestion_never_contains_the_value() {
        let value = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let s = suggests("GITHUB_TOKEN", value).unwrap_or_default();
        assert!(!s.contains("abcdefghij"));
    }
}
