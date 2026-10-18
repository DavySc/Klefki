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
            "servers that sign in with OAuth are changed through Claude Code's own command, which Klefki does not run yet"
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
