//! One server's definition as the user sees and edits it: checked before it is written, with environment and header
//! values hidden on the way out and put back on the way in, and a line diff for the plan.
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

/// The lines of `old` and `new` (pretty JSON) as a diff: `- ` removed, `+ ` added, two spaces unchanged. Made from a
/// longest common subsequence, which is plenty for definitions of a few dozen lines.
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
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
    let mut out = Vec::new();
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
    out
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
}
