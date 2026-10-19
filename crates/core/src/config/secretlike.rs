//! Spotting a secret typed straight into a definition's environment or headers, and what to write instead: a
//! reference such as `${GITHUB_TOKEN}`, which Claude Code fills in from its own environment, so the secret does not
//! sit in `~/.claude.json`. Only a hint; nothing here changes a value or keeps one.

/// The value to write instead of `value` for the variable or header called `name`, when `value` looks like a
/// secret that was typed in. `None` for an empty value, one that is only `${VAR}` references, the hidden
/// placeholder, and anything that does not look like a secret. A value that mixes a reference with text of its own
/// (`postgres://u:pw@${HOST}/db`) is judged by that text.
pub(crate) fn suggestion(name: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == super::entry::MASK {
        return None;
    }
    // "Bearer abc": the scheme stays, the credential goes.
    let (scheme, credential) = match value.split_once(' ') {
        Some((scheme, rest)) if SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) => {
            (Some(scheme), rest.trim())
        }
        _ => (None, value),
    };
    if only_references(name, credential) || !looks_secret(name, credential) {
        return None;
    }
    let reference = format!("${{{}}}", variable(name));
    Some(match scheme {
        Some(scheme) => format!("{scheme} {reference}"),
        None => reference,
    })
}

pub(crate) const SCHEMES: [&str; 3] = ["Bearer", "Basic", "Token"];

/// Whether `value` is nothing but `${NAME}` references (each perhaps with a `:-default`), with no text of its own that
/// could be a secret. A default is text too: one that would be a secret for `name` does not count.
pub(crate) fn only_references(name: &str, value: &str) -> bool {
    let mut rest = value.trim();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let Some(inner) = rest.strip_prefix("${") else {
            return false;
        };
        let Some(end) = inner.find('}') else {
            return false;
        };
        let (variable, default) = match inner[..end].split_once(":-") {
            Some((variable, default)) => (variable, default),
            None => (&inner[..end], ""),
        };
        let named = !variable.is_empty()
            && variable
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        let secret_default = !default.is_empty()
            && (looks_secret(name, default) || (sounds_secret(name) && hideable(default)));
        if !named || secret_default {
            return false;
        }
        rest = inner[end + 1..].trim_start();
    }
    true
}

/// Whether `value`, set under `name`, is plainly a setting rather than a secret, so an export may keep it as the
/// default of its reference (`${LOG_LEVEL:-debug}`): a name that does not sound secret, and a short value that is a
/// number, a word of letters (`debug`, `true`), a path, or an address that neither signs in nor has a query.
pub(crate) fn plain_setting(name: &str, value: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 64
        || value.contains(['}', '$', '"', '\\'])
        || value.contains(char::is_whitespace)
        || sounds_secret(name)
        // Short forms that `sounds_secret` leaves alone, as a flag or header could mean something else by them.
        || ["pass", "pin", "pw", "sig", "salt", "private", "session", "jwt"]
            .iter()
            .any(|p| lower.contains(p))
        || looks_secret(name, value)
    {
        return false;
    }
    // Short, as settings are: a long word or number is more likely a password or a PIN.
    let number = value.len() <= 10
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-');
    let word = value.len() <= 16
        && value
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_');
    let path = value.starts_with(['/', '~', '.']) || value.get(1..3) == Some(":/");
    let address = value.contains("://") && !value.contains(['@', '?', '#']);
    number || word || path || address
}

/// Whether some word of `text` (split at spaces, quotes, brackets and the like) has a known credential's shape
/// (`sk-…`, `ghp_…`) or is an address that signs in: what a shell line or a JSON argument
/// (`--config '{"apiKey":"sk-…"}'`) gives away. Only shapes, so a digest or an id stays readable.
pub(crate) fn has_secret_word(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || "\"'`{}[](),;=<>".contains(c))
        .any(|word| is_known_shape(word) || (word.contains("://") && looks_secret("", word)))
}

/// A long run of letters and digits with both in it, such as a key in an address's path: 24 characters or more.
fn is_long_token(word: &str) -> bool {
    word.len() >= 24
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && word.chars().any(|c| c.is_ascii_digit())
        && word.chars().any(|c| c.is_ascii_alphabetic())
}
const PREFIXES: [&str; 16] = [
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
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
        // An address is a secret only when it signs in: with a password, or with a token as the user name.
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        return authority
            .rsplit_once('@')
            .is_some_and(|(userinfo, _)| userinfo.contains(':') || looks_secret("", userinfo));
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
    const PARTS: [&str; 9] = [
        "token",
        "bearer",
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

/// What a command line's arguments show: each argument as written, except one that looks like a secret, which reads
/// `…`. That is a value after a flag that sounds secret (`--api-key X`, `--token=X`, `API_KEY=X`), unless it is a
/// path or a `${VAR}` reference, and any argument shaped like a known credential (`sk-…`, `ghp_…`) or a URL with a
/// password. Only for showing: the definition itself is never changed.
pub(crate) fn shown_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    rewritten_args(args, false, &mut |_, _| "…".to_owned())
}

/// The arguments [`shown_args`] hides, each replaced by what `hide` gives for it instead of `…`. `hide` gets the name
/// the secret goes by (the flag before it, the `NAME=` or header name, the query parameter; empty when there is none)
/// and the secret itself. With `keep_address`, an address that signs in keeps everything but its user name and
/// password, which become one replacement: what an export writes, so the definition still says where it connects.
pub(crate) fn rewritten_args<'a>(
    args: impl IntoIterator<Item = &'a str>,
    keep_address: bool,
    hide: &mut dyn FnMut(&str, &str) -> String,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut after_secret_flag = false;
    // The flag just before, which names what follows it (`--db`). Only a bare flag: `--token=abc` holds a value.
    let mut flag = "";
    for arg in args {
        let shown = if after_secret_flag && !arg.starts_with('-') && hideable(arg) {
            hide(flag, arg)
        } else if let Some(header) = rewritten_header(arg, hide) {
            header
        } else if let Some((flag, header)) = arg.split_once('=')
            && flag.starts_with('-')
            && let Some(header) = rewritten_header(header, hide)
        {
            format!("{flag}={header}")
        } else if let Some(address) = keep_address
            .then(|| rewritten_address(arg, &mut |_, secret| hide(flag, secret)))
            .flatten()
        {
            address
        } else if let Some((name, value)) = arg.split_once('=')
            && !name.is_empty()
            && !value.is_empty()
            && ((sounds_secret(name) && hideable(value)) || looks_secret(name, value))
        {
            format!("{name}={}", hide(name, value))
        } else if looks_secret("", arg) {
            hide(flag, arg)
        } else if let Some(url) = rewritten_query(arg, hide) {
            url
        } else if has_secret_word(arg) && !only_references("", arg) {
            // A credential inside a longer argument, such as JSON: the whole argument goes.
            hide(flag, arg)
        } else {
            arg.to_owned()
        };
        after_secret_flag = arg.starts_with('-')
            && !arg.contains('=')
            && sounds_secret(arg)
            && !names_a_setting(arg);
        // Letters only: a flag with digits in it may carry a value of its own, which must not become a name.
        flag = if arg.starts_with('-')
            && arg.len() <= 40
            && arg
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_')
        {
            arg
        } else {
            ""
        };
        out.push(shown);
    }
    out
}

/// An address (`scheme://…`) that signs in, with its user name and password replaced by what `hide` gives, and each
/// secret query parameter too: `postgres://${DB}@db.example.com/x`. `None` when the address does not sign in.
pub(crate) fn rewritten_address(
    address: &str,
    hide: &mut dyn FnMut(&str, &str) -> String,
) -> Option<String> {
    let at = address.find("://")? + 3;
    let authority_end = at
        + address[at..]
            .find(['/', '?', '#'])
            .unwrap_or(address.len() - at);
    let (userinfo, _) = address[at..authority_end].rsplit_once('@')?;
    if !looks_secret("", &address[..authority_end])
        || userinfo.split(':').all(|part| only_references("", part))
    {
        return None;
    }
    let signed_out = format!(
        "{}{}{}",
        &address[..at],
        hide("", userinfo),
        &address[at + userinfo.len()..]
    );
    Some(rewritten_query(&signed_out, hide).unwrap_or(signed_out))
}

/// An address with each secret query or fragment parameter replaced by what `hide` gives: [`rewritten_query`] for
/// a definition's `url`, together with the user name and password of one that signs in. `None` when nothing in it
/// is a secret.
pub(crate) fn rewritten_url(
    address: &str,
    hide: &mut dyn FnMut(&str, &str) -> String,
) -> Option<String> {
    rewritten_address(address, hide).or_else(|| rewritten_query(address, hide))
}

/// An HTTP header passed as one argument, as mcp-remote takes them (`--header "Authorization: Bearer <token>"`):
/// `Name: …` when the name sounds secret, keeping a `Bearer`/`Basic`/`Token` scheme. `None` for anything else,
/// including a value that is a `${VAR}` reference.
fn rewritten_header(arg: &str, hide: &mut dyn FnMut(&str, &str) -> String) -> Option<String> {
    let (name, value) = arg.split_once(':')?;
    let value = value.trim();
    let is_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_name || value.is_empty() || value.starts_with("//") {
        return None;
    }
    let (scheme, credential) = match value.split_once(' ') {
        Some((scheme, rest)) if SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) => {
            (Some(scheme), rest.trim())
        }
        _ => (None, value),
    };
    if only_references(name, credential) {
        return None;
    }
    // Any name, when the value has a scheme or a credential's shape.
    if !sounds_secret(name) && scheme.is_none() && !is_known_shape(credential) {
        return None;
    }
    Some(match scheme {
        Some(scheme) => format!("{name}: {scheme} {}", hide(name, credential)),
        None => format!("{name}: {}", hide(name, credential)),
    })
}

/// An address with each part that looks secret replaced by what `hide` gives: a path segment shaped like a key
/// (`/s/sk-live-…/mcp`, named `url token`), and a query or fragment parameter (`name=…`). The other parts stay, as
/// they say which workspace or region it is. `None` when nothing in it is hidden.
fn rewritten_query(arg: &str, hide: &mut dyn FnMut(&str, &str) -> String) -> Option<String> {
    let at = arg.find("://")? + 3;
    let path_at = at + arg[at..].find(['/', '?', '#']).unwrap_or(arg.len() - at);
    let start = path_at
        + arg[path_at..]
            .find(['?', '#'])
            .unwrap_or(arg.len() - path_at);
    let mut out = arg[..path_at].to_owned();
    let mut hid = false;
    for (i, segment) in arg[path_at..start].split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        if !segment.is_empty()
            && !only_references("", segment)
            && (looks_secret("", segment) || is_long_token(segment))
        {
            hid = true;
            out.push_str(&hide("url token", segment));
        } else {
            out.push_str(segment);
        }
    }
    for piece in arg[start..].split_inclusive(['?', '&', '#']) {
        let (pair, separator) = match piece.strip_suffix(['?', '&', '#']) {
            Some(pair) => (pair, &piece[pair.len()..]),
            None => (piece, ""),
        };
        match pair.split_once('=') {
            Some((name, value))
                if !value.is_empty()
                    && !only_references(name, value)
                    && ((sounds_secret(name) && hideable(value)) || looks_secret(name, value)) =>
            {
                hid = true;
                out.push_str(name);
                out.push('=');
                out.push_str(&hide(name, value));
            }
            _ => out.push_str(pair),
        }
        out.push_str(separator);
    }
    hid.then_some(out)
}

/// A flag that sounds secret but names where a secret is, or what kind it is: `--token-file`, `--auth-type`.
fn names_a_setting(flag: &str) -> bool {
    let last = flag
        .rsplit(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["file", "path", "dir", "type", "mode", "url"].contains(&last.as_str())
}

/// A value after a secret-sounding name that is worth hiding: not a reference, not a path, not a plain word such
/// as `true`.
fn hideable(value: &str) -> bool {
    !(only_references("", value)
        || value.starts_with(['/', '~', '.'])
        || value.contains('\\')
        || value.get(1..3) == Some(":/")
        || ["true", "false", "yes", "no", "none"]
            .iter()
            .any(|w| value.eq_ignore_ascii_case(w)))
}

/// `name` as an environment variable name: upper case letters, digits and `_`, not starting with a digit.
pub(crate) fn variable(name: &str) -> String {
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

    #[test]
    fn secret_looking_arguments_are_hidden_from_a_shown_command_line() {
        let shown = |args: &[&str]| shown_args(args.iter().copied()).join(" ");
        // Review of #25: `--api-key sk-live-…` was printed as written.
        assert_eq!(
            shown(&["-y", "alpha-mcp", "--api-key", "sk-live-ARGSECRET123456"]),
            "-y alpha-mcp --api-key …"
        );
        assert_eq!(shown(&["--token=abc12345"]), "--token=…");
        assert_eq!(shown(&["--password", "hunter2"]), "--password …");
        assert_eq!(shown(&["--secret", "x"]), "--secret …");
        assert_eq!(shown(&["API_KEY=0a1b2c3d4e"]), "API_KEY=…");
        // Review of #25, round 2: mcp-remote's `--header`, in both spacings, and other secret headers.
        assert_eq!(
            shown(&["--header", "Authorization: Bearer abcdef1234567890XYZ"]),
            "--header Authorization: Bearer …"
        );
        assert_eq!(
            shown(&["--header", "Authorization:Bearer abcdef1234567890XYZ="]),
            "--header Authorization: Bearer …"
        );
        assert_eq!(
            shown(&["--header", "X-Api-Key: k123"]),
            "--header X-Api-Key: …"
        );
        assert_eq!(shown(&["--header", "Cookie:sid=1"]), "--header Cookie: …");
        assert_eq!(
            shown(&["--header", "Authorization: Bearer ${AUTH_TOKEN}"]),
            "--header Authorization: Bearer ${AUTH_TOKEN}"
        );
        assert_eq!(
            shown(&["--header", "Accept: text/event-stream"]),
            "--header Accept: text/event-stream"
        );
        assert_eq!(
            shown(&["https://x.example.com/mcp"]),
            "https://x.example.com/mcp"
        );
        // A credential's shape alone gives it away, wherever it is.
        assert_eq!(shown(&["serve", "ghp_abcdefghijklmnop1234"]), "serve …");
        assert_eq!(
            shown(&["--db", "postgres://me:pw123@db.example.com/x"]),
            "--db …"
        );
        // Not secrets: paths, references, settings, the flag after a secret flag, ordinary words.
        assert_eq!(
            shown(&["--token-file", "/run/secrets/token"]),
            "--token-file /run/secrets/token"
        );
        assert_eq!(shown(&["--token", "${GH_TOKEN}"]), "--token ${GH_TOKEN}");
        assert_eq!(shown(&["--auth", "--verbose"]), "--auth --verbose");
        assert_eq!(shown(&["--use-auth=true"]), "--use-auth=true");
        assert_eq!(
            shown(&["-y", "@modelcontextprotocol/server-filesystem", "C:/Repos"]),
            "-y @modelcontextprotocol/server-filesystem C:/Repos"
        );
        assert_eq!(
            shown(&["--key", "C:\\keys\\a.pem"]),
            "--key C:\\keys\\a.pem"
        );
    }

    #[test]
    fn secrets_the_first_rules_missed_are_hidden_too() {
        let shown = |args: &[&str]| shown_args(args.iter().copied()).join(" ");
        // Found reviewing main: a token used as a URL's user name, as git takes them.
        assert_eq!(
            shown(&["--repo", "https://ghp_abcdefghijklmnop1234@github.com/me/x"]),
            "--repo …"
        );
        assert_eq!(
            shown(&["--repo=https://ghp_abcdefghijklmnop1234@github.com/me/x"]),
            "--repo=…"
        );
        // A secret in a later query parameter; the others stay readable.
        assert_eq!(
            shown(&["https://h.example.com/sse?workspace=acme&api_key=S3CRETVALUE#x"]),
            "https://h.example.com/sse?workspace=acme&api_key=…#x"
        );
        assert_eq!(
            shown(&["--url=https://h.example.com/sse?a=1&token=abc12345"]),
            "--url=https://h.example.com/sse?a=1&token=…"
        );
        assert_eq!(
            shown(&["https://h.example.com/cb#state=1&access_token=abc12345"]),
            "https://h.example.com/cb#state=1&access_token=…"
        );
        // A flag that names a bearer token.
        assert_eq!(shown(&["--oauth2Bearer", "abc12345"]), "--oauth2Bearer …");
        // A header written as one word with its option.
        assert_eq!(
            shown(&["--header=Authorization: Bearer abcdef1234567890XYZ"]),
            "--header=Authorization: Bearer …"
        );
        // A header whose name says nothing, carrying a credential's shape.
        assert_eq!(
            shown(&["--header", "X-Custom: sk-ant-api03-abcdefghijklmnop"]),
            "--header X-Custom: …"
        );
        assert_eq!(
            shown(&["--header", "X-Custom:Bearer ghp_abcdefghijklmnop1234"]),
            "--header X-Custom: Bearer …"
        );
        // Still readable: a plain user name, a query without secrets, an ordinary header.
        assert_eq!(
            shown(&["https://git@github.com/me/x"]),
            "https://git@github.com/me/x"
        );
        assert_eq!(
            shown(&["https://h.example.com/sse?workspace=acme&limit=10"]),
            "https://h.example.com/sse?workspace=acme&limit=10"
        );
        assert_eq!(
            shown(&["--header", "X-Region: eu-west-1"]),
            "--header X-Region: eu-west-1"
        );
    }

    #[test]
    fn a_key_in_an_address_path_or_beside_a_reference_is_hidden_from_a_shown_command_line() {
        // Review of #12.
        let shown = |args: &[&str]| shown_args(args.iter().copied()).join(" ");
        assert_eq!(
            shown(&[
                "mcp-remote",
                "https://mcp.example.com/s/sk-live-abcdef1234567890/sse"
            ]),
            "mcp-remote https://mcp.example.com/s/…/sse"
        );
        assert_eq!(shown(&["--token", "${P}sk-abcdef1234567890"]), "--token …");
        assert_eq!(shown(&["--token", "${P}"]), "--token ${P}");
        assert_eq!(
            shown(&["--config", "{\"apiKey\":\"sk-abcdef1234567890\"}"]),
            "--config …"
        );
        assert_eq!(
            shown(&["https://mcp.example.com/v1/workspaces/team-blue/mcp"]),
            "https://mcp.example.com/v1/workspaces/team-blue/mcp"
        );
    }

    #[test]
    fn a_setting_named_after_a_secret_stays_readable() {
        let shown = |args: &[&str]| shown_args(args.iter().copied()).join(" ");
        // Found reviewing main: the value after these flags is where a secret is kept, or what kind it is, not one.
        assert_eq!(
            shown(&["--token-file", "config/token.json"]),
            "--token-file config/token.json"
        );
        assert_eq!(shown(&["--auth-type", "oauth"]), "--auth-type oauth");
        assert_eq!(shown(&["--auth-mode", "device"]), "--auth-mode device");
        assert_eq!(
            shown(&["--token-url", "https://example.com/oauth/token"]),
            "--token-url https://example.com/oauth/token"
        );
        // A secret after a secret flag is still hidden, slashes and all.
        assert_eq!(shown(&["--token", "ab/cd+ef12=="]), "--token …");
        assert_eq!(shown(&["--api-key", "k-123"]), "--api-key …");
        assert_eq!(shown(&["--password", "p@ss/w0rd!"]), "--password …");
        // A digest is not a header carrying a credential.
        assert_eq!(
            shown(&["sha256:e3b0c44298fc1c149afbf4c8996fb924"]),
            "sha256:e3b0c44298fc1c149afbf4c8996fb924"
        );
    }

    #[test]
    fn a_token_in_an_address_gets_a_reference() {
        assert_eq!(
            suggests(
                "GIT_URL",
                "https://ghp_abcdefghijklmnop1234@github.com/me/x"
            )
            .as_deref(),
            Some("${GIT_URL}")
        );
        assert_eq!(suggests("GIT_URL", "https://git@github.com/me/x"), None);
    }

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
            // Review of #27: Stripe's test and restricted keys, as Diagnose masks them.
            "sk_test_51ABCDEFGHIJKL",
            "rk_live_51ABCDEFGHIJKL",
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
