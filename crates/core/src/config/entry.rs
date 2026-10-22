//! One server's definition as the user sees and edits it: checked before it is written, with environment and header
//! values hidden on the way out and put back on the way in, and a line diff for the plan.
use crate::config::secretlike::{plain_setting, rewritten_args, rewritten_url};
use crate::sanitize::sanitize;
use serde_json::{Map, Value};

/// Shown instead of an environment or header value. Sent back unchanged, it means "keep the value that is there".
pub(crate) const MASK: &str = "<hidden>";
/// An edited definition larger than this is refused.
pub(crate) const MAX_ENTRY_BYTES: usize = 64 * 1024;
const MASKED_KEYS: [&str; 2] = ["env", "headers"];

/// `entry` with every environment and header value replaced by [`MASK`].
pub(crate) fn masked(entry: &Value) -> Value {
    let mut out = entry.clone();
    if let Some(object) = out.as_object_mut() {
        for key in MASKED_KEYS {
            if let Some(Value::Object(values)) = object.get_mut(key) {
                for value in values.values_mut() {
                    *value = Value::String(MASK.to_owned());
                }
            }
        }
    }
    out
}

/// The environment and header values of `entry`: what [`masked`] hides, for hiding them in text read elsewhere.
pub(crate) fn values(entry: &Value) -> Vec<String> {
    MASKED_KEYS
        .iter()
        .filter_map(|key| entry.get(key).and_then(Value::as_object))
        .flat_map(|values| values.values().filter_map(Value::as_str).map(str::to_owned))
        .collect()
}

/// What [`values`] gives, and what each `${NAME}` reference stands for where Klefki can see it. Claude Code fills the
/// references in from its own environment, so the server may print the filled-in value (`Bearer abc123` for
/// `Bearer ${TOKEN}`) and not the text the file holds. `vars` is Klefki's own environment, the best it knows of
/// Claude Code's. A reference with a `:-default` stands for its default when nothing is set. A value or default that
/// is plainly a setting (`localhost`, a folder, `debug`) is left: hiding it would blank every path and address in a
/// log, which is what a person reading one needs.
pub(crate) fn values_filled_in(
    entry: &Value,
    vars: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<String> {
    let mut out = values(entry);
    for key in MASKED_KEYS {
        if let Some(values) = entry.get(key).and_then(Value::as_object) {
            for text in values.values().filter_map(Value::as_str) {
                let filled = filled_in(text, vars);
                if filled != text {
                    out.push(filled);
                }
            }
        }
    }
    let mut named: Vec<String> = Vec::new();
    strings(entry, &mut |text| {
        for (name, default) in references(text) {
            if let Some(default) = default.filter(|d| !plain_setting(&name, d)) {
                out.push(default);
            }
            if !named.contains(&name) {
                named.push(name);
            }
        }
    });
    out.extend(named.iter().filter_map(|name| {
        let value = vars(name)?.into_string().ok()?;
        (!plain_setting(name, &value)).then_some(value)
    }));
    out.retain(|value| !value.is_empty());
    out
}

/// Everything in `entry` that must not show in text read elsewhere (a server's log, Claude Code's message, a
/// diagnosis): [`values_filled_in`], and each argument and address part an export replaces by a `${VAR}`.
pub(crate) fn hidden_values(
    entry: &Value,
    vars: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<String> {
    let mut out = values_filled_in(entry, vars);
    let args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut take = |_: &str, secret: &str| {
        out.push(secret.to_owned());
        String::new()
    };
    // Both ways: an address that signs in keeps its host in one, and the whole argument goes in the other.
    rewritten_args(args.iter().copied(), false, &mut take);
    rewritten_args(args.iter().copied(), true, &mut take);
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        rewritten_url(url, &mut take);
    }
    out.retain(|value| !value.is_empty());
    out
}

/// `text` with each `${NAME}` and `${NAME:-default}` replaced by what is set, or by the default. One that is not
/// set and has no default is kept as written, as Claude Code passes it on.
fn filled_in(text: &str, vars: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[at..]);
            return out;
        };
        let (name, default) = split_reference(&after[..end]);
        match vars(name).and_then(|v| v.into_string().ok()).or(default) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[at..at + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The name and the default of each `${NAME}` or `${NAME:-default}` in `text`.
fn references(text: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else { break };
        let (name, default) = split_reference(&after[..end]);
        if !name.is_empty() {
            found.push((name.to_owned(), default));
        }
        rest = &after[end + 1..];
    }
    found
}

fn split_reference(inner: &str) -> (&str, Option<String>) {
    match inner.split_once(":-") {
        Some((name, default)) => (name, Some(default.to_owned())),
        None => (inner, None),
    }
}

/// Calls `visit` with every string in `value`, however deep.
fn strings(value: &Value, visit: &mut dyn FnMut(&str)) {
    match value {
        Value::String(s) => visit(s),
        Value::Array(items) => items.iter().for_each(|v| strings(v, visit)),
        Value::Object(map) => map.values().for_each(|v| strings(v, visit)),
        _ => {}
    }
}

/// Puts the values back that the user left as [`MASK`], from `old`. A mask for a name that had no value is an error:
/// there is nothing to keep.
pub(crate) fn unmasked(new: &Value, old: &Value) -> Result<Value, String> {
    let mut out = new.clone();
    let Some(object) = out.as_object_mut() else {
        return Ok(out);
    };
    for key in MASKED_KEYS {
        let Some(Value::Object(values)) = object.get_mut(key) else {
            continue;
        };
        for (name, value) in values.iter_mut() {
            if value.as_str() == Some(MASK) {
                match old.get(key).and_then(|o| o.get(name)) {
                    Some(previous @ Value::String(_)) => *value = previous.clone(),
                    _ => {
                        return Err(format!(
                            "{key}.{} says {MASK} but had no value before; type the value instead",
                            sanitize(name)
                        ));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Checks an edited definition the way Claude Code would need it, and refuses what klefki must not write itself.
pub(crate) fn validate(entry: &Value) -> Result<(), String> {
    let object = entry
        .as_object()
        .ok_or("the definition must be a JSON object")?;
    if object.contains_key("oauth") {
        return Err(
            "servers that sign in with OAuth are changed through Claude Code's own commands"
                .to_owned(),
        );
    }
    let text = |key: &str| -> Result<Option<&str>, String> {
        match object.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(format!("{key} must be text")),
        }
    };
    let kind = text("type")?.unwrap_or("stdio");
    match kind {
        "stdio" => {
            let command = text("command")?.unwrap_or("");
            if command.trim().is_empty() {
                return Err("a stdio server needs a command".to_owned());
            }
            no_line_breaks("command", command)?;
            match object.get("args") {
                None => {}
                Some(Value::Array(args)) => {
                    for arg in args {
                        let arg = arg.as_str().ok_or("every argument must be text")?;
                        no_line_breaks("an argument", arg)?;
                    }
                }
                Some(_) => return Err("args must be a list".to_owned()),
            }
            if object.contains_key("url") {
                return Err("a stdio server has a command, not a url".to_owned());
            }
        }
        "http" | "sse" | "ws" => {
            let raw = text("url")?.ok_or("a remote server needs a url")?;
            let url = url::Url::parse(raw).map_err(|_| "the url is not valid".to_owned())?;
            let allowed: &[&str] = if kind == "ws" {
                &["ws", "wss"]
            } else {
                &["http", "https"]
            };
            if !allowed.contains(&url.scheme()) {
                return Err(format!(
                    "a {kind} server's url must start with {}",
                    allowed.join(":// or ") + "://"
                ));
            }
            if object.contains_key("command") {
                return Err("a remote server has a url, not a command".to_owned());
            }
            if let Some(helper) = text("headersHelper")? {
                no_line_breaks("headersHelper", helper)?;
            }
        }
        other => {
            return Err(format!(
                "type must be stdio, http, sse or ws, not {:?}",
                sanitize(other)
            ));
        }
    }
    for key in MASKED_KEYS {
        match object.get(key) {
            None => {}
            Some(Value::Object(values)) => check_values(key, values)?,
            Some(_) => return Err(format!("{key} must be an object of names and text values")),
        }
    }
    Ok(())
}

/// [`validate`] for a server about to be added, which may sign in with OAuth: such a server is added through Claude
/// Code's own `add-json`, so its `oauth` settings are checked the way that command takes them (it refuses a bad
/// one, and silently drops one on a server that is not http or sse). The client secret never goes in here.
pub(crate) fn validate_new(entry: &Value) -> Result<(), String> {
    let Some(oauth) = entry.get("oauth") else {
        return validate(entry);
    };
    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("stdio");
    if !matches!(kind, "http" | "sse") {
        return Err("only http and sse servers sign in with OAuth".to_owned());
    }
    let oauth = oauth
        .as_object()
        .ok_or("oauth must be an object of settings")?;
    for (key, value) in oauth {
        match key.as_str() {
            "clientId" => match value.as_str() {
                Some(id) if !id.trim().is_empty() && id.len() <= 512 && sanitize(id) == id => {
                    no_line_breaks("the client id", id)?;
                }
                _ => return Err("the client id must be a line of text".to_owned()),
            },
            "callbackPort" => {
                if !value.as_u64().is_some_and(|p| (1..=65535).contains(&p)) {
                    return Err("the callback port must be a number from 1 to 65535".to_owned());
                }
            }
            "scopes" => {
                let scopes = value
                    .as_str()
                    .ok_or("the scopes must be text, separated by spaces")?;
                no_line_breaks("the scopes", scopes)?;
            }
            "authServerMetadataUrl" => {
                let https = value
                    .as_str()
                    .and_then(|u| url::Url::parse(u).ok())
                    .is_some_and(|u| u.scheme() == "https");
                if !https {
                    return Err("the sign-in metadata address must start with https://".to_owned());
                }
            }
            "clientSecret" => {
                return Err(
                    "the client secret does not go in the definition; it is handed to Claude Code on its own"
                        .to_owned(),
                );
            }
            other => {
                return Err(format!(
                    "oauth has no setting {:?} that Claude Code knows",
                    sanitize(other)
                ));
            }
        }
    }
    let mut rest = entry.clone();
    if let Some(object) = rest.as_object_mut() {
        object.remove("oauth");
    }
    validate(&rest)
}

fn check_values(key: &str, values: &Map<String, Value>) -> Result<(), String> {
    for (name, value) in values {
        let valid_name = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
        if !valid_name {
            return Err(format!("{key}: {:?} is not a valid name", sanitize(name)));
        }
        let value = value
            .as_str()
            .ok_or_else(|| format!("{key}.{name} must be text"))?;
        no_line_breaks(&format!("{key}.{name}"), value)?;
    }
    Ok(())
}

fn no_line_breaks(what: &str, value: &str) -> Result<(), String> {
    if value.contains(['\r', '\n', '\0']) {
        return Err(format!("{what} must not contain line breaks"));
    }
    Ok(())
}

/// Most old × new changed lines [`diff_lines`] compares one by one, with a table of about 4 MiB. A longer run is shown
/// as all removed and all added, after a note saying so: still right, only not the shortest diff.
const MAX_DIFF_CELLS: usize = 1 << 20;

/// The lines of `old` and `new` (pretty JSON) as a diff: `- ` removed, `+ ` added, two spaces unchanged. Made from a
/// longest common subsequence of the lines between those both start and end with.
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<String> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let end = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (a, b) = (&old[start..old.len() - end], &new[start..new.len() - end]);
    let mut out: Vec<String> = old[..start].iter().map(|l| format!("  {l}")).collect();
    if a.len().saturating_mul(b.len()) > MAX_DIFF_CELLS {
        out.push("~ (too many changed lines to compare one by one, so all of them show as removed and added)".to_owned());
        // In the order the table gives when no line is shared.
        out.extend(b.iter().map(|l| format!("+ {l}")));
        out.extend(a.iter().map(|l| format!("- {l}")));
    } else {
        changed_lines(a, b, &mut out);
    }
    out.extend(old[old.len() - end..].iter().map(|l| format!("  {l}")));
    out
}

/// The diff of `a` and `b` from a longest common subsequence, onto `out`.
fn changed_lines(a: &[&str], b: &[&str], out: &mut Vec<String>) {
    // lcs[i][j]: length of the longest common subsequence of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push(format!("+ {}", b[j]));
            j += 1;
        } else {
            out.push(format!("- {}", a[i]));
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_are_hidden_and_put_back() {
        let old = json!({"command": "node", "env": {"TOKEN": "s3cret", "MODE": "x"}, "headers": {"A": "b"}});
        let shown = masked(&old);
        assert_eq!(shown["env"]["TOKEN"], MASK);
        assert_eq!(shown["headers"]["A"], MASK);
        assert!(!shown.to_string().contains("s3cret"));
        let mut edited = shown.clone();
        edited["env"]["MODE"] = json!("y");
        edited["args"] = json!(["a.js"]);
        let back = unmasked(&edited, &old).unwrap();
        assert_eq!(back["env"], json!({"TOKEN": "s3cret", "MODE": "y"}));
        assert_eq!(back["headers"], json!({"A": "b"}));
        assert_eq!(back["args"], json!(["a.js"]));
    }

    fn vars(
        set: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        move |name| {
            set.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| std::ffi::OsString::from(*v))
        }
    }

    #[test]
    fn what_a_reference_stands_for_is_a_value_to_hide_too() {
        let entry = json!({
            "command": "node",
            "args": ["--db", "${DB_URL}", "--mode=${MODE:-fast}"],
            "env": {"API_KEY": "${API_KEY}", "AUTH": "Bearer ${TOKEN}", "PLAIN": "x"},
            "headers": {"X-Key": "${KEY:-fallback-key}"},
        });
        let hidden = values_filled_in(
            &entry,
            &vars(&[
                ("API_KEY", "key-from-env"),
                ("TOKEN", "tok-123456"),
                ("DB_URL", "pg://u:pw@h/db"),
            ]),
        );
        for wanted in [
            "${API_KEY}",
            "key-from-env",
            "Bearer tok-123456",
            "tok-123456",
            "pg://u:pw@h/db",
            "fallback-key",
        ] {
            assert!(
                hidden.iter().any(|v| v == wanted),
                "{wanted:?} not in {hidden:?}"
            );
        }
    }

    #[test]
    fn a_plain_setting_behind_a_reference_is_left_readable() {
        let entry = json!({
            "command": "node",
            "args": ["${HOME}/mcp/server.js", "--host", "${HOST:-localhost}", "--mode=${MODE:-fast}"],
            "env": {"A": "x"},
        });
        let hidden = values_filled_in(
            &entry,
            &vars(&[("HOME", "/home/someone"), ("HOST", "db.internal")]),
        );
        for plain in ["localhost", "fast", "/home/someone"] {
            assert!(
                !hidden.iter().any(|v| v == plain),
                "{plain:?} in {hidden:?}"
            );
        }
        // Nearby: the same references with values that are secrets are hidden.
        let hidden = values_filled_in(
            &json!({"args": ["--x", "${DB_PASSWORD:-hunter2pw}", "${API_TOKEN}"]}),
            &vars(&[("API_TOKEN", "tok-0123456789")]),
        );
        assert!(hidden.iter().any(|v| v == "hunter2pw"), "{hidden:?}");
        assert!(hidden.iter().any(|v| v == "tok-0123456789"), "{hidden:?}");
    }

    #[test]
    fn what_an_export_replaces_in_arguments_and_the_address_is_hidden_too() {
        let entry = json!({
            "command": "npx",
            "args": ["-y", "pkg", "--api-key", "plain-key-value-1", "--db=postgres://u:pw1234567@db/x", "--verbose"],
            "url": "https://mcp.example.com/mcp?key=abc123secret&page=2",
        });
        let hidden = hidden_values(&entry, &vars(&[]));
        for secret in ["plain-key-value-1", "pw1234567", "abc123secret"] {
            assert!(
                hidden.iter().any(|v| v.contains(secret)),
                "{secret:?} not in {hidden:?}"
            );
        }
        // Nearby: what is not a secret stays readable.
        for plain in ["pkg", "--verbose", "page=2"] {
            assert!(
                !hidden.iter().any(|v| v == plain),
                "{plain:?} in {hidden:?}"
            );
        }
    }

    #[test]
    fn a_reference_nothing_sets_is_kept_as_written_and_adds_nothing() {
        let entry = json!({"env": {"A": "x-${UNSET}-y", "B": "${BROKEN"}});
        let hidden = values_filled_in(&entry, &vars(&[]));
        assert_eq!(hidden, ["x-${UNSET}-y", "${BROKEN"]);
    }

    #[test]
    fn a_default_is_used_only_when_nothing_is_set() {
        let entry = json!({"env": {"A": "${V:-dflt}"}});
        assert!(values_filled_in(&entry, &vars(&[])).contains(&"dflt".to_owned()));
        let set = values_filled_in(&entry, &vars(&[("V", "real-value")]));
        assert!(set.contains(&"real-value".to_owned()), "{set:?}");
    }

    #[test]
    fn a_mask_without_an_earlier_value_is_refused() {
        let old = json!({"command": "node", "env": {"A": "1"}});
        let new = json!({"command": "node", "env": {"A": MASK, "B": MASK}});
        assert!(unmasked(&new, &old).unwrap_err().contains("env.B"));
        // A mask moved to a header with the same name is not taken from env.
        let new = json!({"command": "node", "headers": {"A": MASK}});
        assert!(unmasked(&new, &old).is_err());
    }

    #[test]
    fn valid_definitions_pass() {
        for ok in [
            json!({"command": "npx", "args": ["-y", "pkg"], "env": {"A_B": "1"}}),
            json!({"type": "stdio", "command": "node"}),
            json!({"type": "http", "url": "https://x.example/mcp", "headers": {"Authorization": "Bearer ${T}"}}),
            json!({"type": "sse", "url": "http://localhost:3000/sse"}),
            json!({"type": "ws", "url": "wss://x.example/"}),
            json!({"type": "http", "url": "https://x.example", "headersHelper": "get-token --x"}),
        ] {
            assert_eq!(validate(&ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn invalid_definitions_say_why() {
        for (bad, why) in [
            (json!([]), "object"),
            (json!({}), "command"),
            (json!({"command": "  "}), "command"),
            (json!({"command": 1}), "text"),
            (json!({"command": "a\nb"}), "line breaks"),
            (json!({"command": "x", "args": "y"}), "list"),
            (json!({"command": "x", "args": [1]}), "argument"),
            (json!({"command": "x", "env": {"A": 1}}), "text"),
            (
                json!({"command": "x", "env": {"A": "1\r\nX: y"}}),
                "line breaks",
            ),
            (json!({"command": "x", "env": {"A B": "1"}}), "valid name"),
            (json!({"command": "x", "env": []}), "object"),
            (json!({"command": "x", "url": "https://x"}), "not a url"),
            (json!({"type": "http"}), "url"),
            (
                json!({"type": "http", "url": "file:///etc/passwd"}),
                "must start with",
            ),
            (
                json!({"type": "ws", "url": "https://x.example"}),
                "must start with",
            ),
            (
                json!({"type": "http", "url": "https://x", "command": "y"}),
                "not a command",
            ),
            (
                json!({"type": "http", "url": "https://x", "headers": {"A": "b\nc"}}),
                "line breaks",
            ),
            (json!({"type": "pigeon"}), "type"),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"clientId": "a"}}),
                "OAuth",
            ),
        ] {
            let err = validate(&bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
    }

    #[test]
    fn a_new_server_may_sign_in_with_oauth_as_claude_code_takes_it() {
        for ok in [
            json!({"type": "http", "url": "https://x.example/mcp", "oauth": {"clientId": "abc"}}),
            json!({"type": "sse", "url": "https://x.example/sse", "oauth": {"clientId": "abc", "callbackPort": 8123}}),
            json!({"type": "http", "url": "https://x.example", "oauth": {"callbackPort": 1}}),
            json!({"type": "http", "url": "https://x.example", "oauth": {"clientId": "a", "scopes": "read write", "authServerMetadataUrl": "https://id.example/.well-known/x"}}),
            json!({"type": "http", "url": "https://x.example", "oauth": {}}),
            // Without oauth it is checked as any definition.
            json!({"command": "node"}),
        ] {
            assert_eq!(validate_new(&ok), Ok(()), "{ok}");
        }
        for (bad, why) in [
            // Claude Code drops oauth for these, so it would be lost without a word.
            (
                json!({"command": "node", "oauth": {"clientId": "a"}}),
                "http and sse",
            ),
            (
                json!({"type": "ws", "url": "wss://x.example", "oauth": {"clientId": "a"}}),
                "http and sse",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": "a"}),
                "object",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"clientId": ""}}),
                "client id",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"clientId": 7}}),
                "client id",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"clientId": "a\nb"}}),
                "client id",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"callbackPort": 0}}),
                "port",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"callbackPort": 70000}}),
                "port",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"callbackPort": "8080"}}),
                "port",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"scopes": ["a"]}}),
                "scopes",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"authServerMetadataUrl": "http://id.example/m"}}),
                "https",
            ),
            // A secret never sits in the definition: it goes to Claude Code on its own.
            (
                json!({"type": "http", "url": "https://x", "oauth": {"clientSecret": "s3cret"}}),
                "client secret",
            ),
            (
                json!({"type": "http", "url": "https://x", "oauth": {"bogus": 1}}),
                "bogus",
            ),
            // The rest is still checked.
            (
                json!({"type": "http", "url": "file:///x", "oauth": {"clientId": "a"}}),
                "must start with",
            ),
        ] {
            let err = validate_new(&bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
            assert!(!err.contains("s3cret"), "{err}");
        }
    }

    #[test]
    fn the_diff_marks_only_what_changed() {
        let old = "{\n  \"command\": \"node\",\n  \"args\": [\n    \"a.js\"\n  ]\n}";
        let new = "{\n  \"command\": \"node\",\n  \"args\": [\n    \"b.js\",\n    \"--x\"\n  ]\n}";
        let want: Vec<String> = [
            ("  ", "{"),
            ("  ", "  \"command\": \"node\","),
            ("  ", "  \"args\": ["),
            ("+ ", "    \"b.js\","),
            ("+ ", "    \"--x\""),
            ("- ", "    \"a.js\""),
            ("  ", "  ]"),
            ("  ", "}"),
        ]
        .iter()
        .map(|(mark, line)| format!("{mark}{line}"))
        .collect();
        assert_eq!(diff_lines(old, new), want);
        assert!(diff_lines(old, old).iter().all(|l| l.starts_with("  ")));
        assert_eq!(diff_lines("", "x"), ["+ x"]);
        assert_eq!(diff_lines("x", ""), ["- x"]);
    }

    #[test]
    fn a_long_definition_is_diffed_quickly() {
        // Found reviewing main: the diff built a table of every old line by every new line, before the plan noticed
        // that nothing changes. Replacing 8,000 arguments took 256 MB, and a definition read from the file has no
        // size limit.
        let definition = |tag: &str| {
            let args: Vec<String> = (0..8_000).map(|i| format!("{tag}{i}")).collect();
            serde_json::to_string_pretty(&json!({"command": "node", "args": args})).unwrap()
        };
        let (old, new) = (definition("a"), definition("b"));
        let started = std::time::Instant::now();
        let count =
            |diff: &[String], mark: &str| diff.iter().filter(|l| l.starts_with(mark)).count();
        // Every argument changes: each old one out, each new one in, the rest kept, and a note that says why the
        // lines are not matched up.
        let diff = diff_lines(&old, &new);
        assert_eq!(
            (count(&diff, "- "), count(&diff, "+ "), count(&diff, "  ")),
            (8_000, 8_000, 5)
        );
        assert_eq!(count(&diff, "~ "), 1);
        // Two edits far apart, in 1,000 arguments: matched up, two lines out and two in. Past the cap (3,000), every
        // line from the first edit to the second shows as changed, with the note.
        for (args, changed, note) in [(1_000, 4, 0), (3_000, 2 * 2_998, 1)] {
            let old = serde_json::to_string_pretty(&json!({"args": (0..args).collect::<Vec<_>>()}))
                .unwrap();
            let edited = old.replacen("    1,", "    x,", 1).replacen(
                &format!("    {},", args - 2),
                "    y,",
                1,
            );
            let diff = diff_lines(&old, &edited);
            assert_eq!(count(&diff, "+ ") + count(&diff, "- "), changed, "{args}");
            assert_eq!(count(&diff, "~ "), note, "{args}");
        }
        // One argument among 8,000 changes: one line out, one in.
        let edited = old.replace("\"a4000\",", "\"z\",");
        let diff = diff_lines(&old, &edited);
        let changed: Vec<_> = diff.iter().filter(|l| !l.starts_with("  ")).collect();
        assert_eq!(changed, ["+     \"z\",", "-     \"a4000\","]);
        assert_eq!(diff.len(), 8_006);
        assert_eq!(count(&diff_lines(&old, &old), "  "), 8_005);
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(1), "{took:?}");
    }

    #[test]
    fn the_diff_keeps_as_many_lines_as_can_be_kept() {
        // Texts of a few lines that repeat, as `{`, `}` and `]` do in JSON: dropping the added lines gives the old
        // text, dropping the removed ones the new, and the kept lines are a longest common subsequence.
        fn longest_common(a: &[&str], b: &[&str]) -> usize {
            let mut table = vec![vec![0; b.len() + 1]; a.len() + 1];
            for i in 0..a.len() {
                for j in 0..b.len() {
                    table[i + 1][j + 1] = if a[i] == b[j] {
                        table[i][j] + 1
                    } else {
                        table[i][j + 1].max(table[i + 1][j])
                    };
                }
            }
            table[a.len()][b.len()]
        }
        let words = ["{", "}", "  ]", "a", "b"];
        let mut seed = 7u64;
        let mut text = |lines: usize| {
            (0..lines)
                .map(|_| {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    words[(seed >> 33) as usize % words.len()]
                })
                .collect::<Vec<_>>()
        };
        for round in 0..2_000 {
            let (a, b) = (text(round % 9), text(round % 7));
            let diff = diff_lines(&a.join("\n"), &b.join("\n"));
            let side = |drop: &str| -> Vec<&str> {
                diff.iter()
                    .filter(|l| !l.starts_with(drop))
                    .map(|l| &l[2..])
                    .collect()
            };
            assert_eq!(side("+ "), a, "{a:?} {b:?} {diff:?}");
            assert_eq!(side("- "), b, "{a:?} {b:?} {diff:?}");
            let kept = diff.iter().filter(|l| l.starts_with("  ")).count();
            assert_eq!(kept, longest_common(&a, &b), "{a:?} {b:?} {diff:?}");
        }
    }
}
