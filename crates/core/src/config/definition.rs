//! One server's definition as the edit form shows it: the fields a person edits, typed, over the JSON the engine
//! sends (values hidden as [`MASK`](super::entry::MASK)). Turning it back into JSON keeps the definition's key order
//! and every key the form does not show, so saving from the form changes only what was edited.
use super::entry;
use crate::api::TransportKind;
use serde_json::{Map, Value};

/// The keys the form shows. Everything else is carried through untouched.
const FORM_KEYS: [&str; 7] = [
    "type",
    "command",
    "args",
    "env",
    "url",
    "headers",
    "headersHelper",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    /// Stdio, Http, Sse or Ws.
    pub transport: TransportKind,
    /// Stdio only: the program, its arguments and its environment.
    pub command: String,
    pub args: Vec<String>,
    /// Name and value, in order. A value of [`Definition::HIDDEN`] keeps the value it has.
    pub env: Vec<(String, String)>,
    /// Remote only: the address, the headers (like `env`) and the command that prints more headers.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub headers_helper: String,
    /// The definition as it was read: its key order and the keys the form does not show.
    base: Map<String, Value>,
}

impl Definition {
    /// What a value the user has not changed reads as.
    pub const HIDDEN: &'static str = entry::MASK;

    /// Reads a definition (the engine's hidden-values JSON). A shape the form cannot show is refused, saying
    /// why; the JSON tab can still edit it.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(json)
            .map_err(|e| format!("The JSON is not valid ({e}); fix it in the JSON tab."))?;
        let Value::Object(base) = value else {
            return Err(only_json_tab("the definition is not an object"));
        };
        let text = |key: &str| -> Result<String, String> {
            match base.get(key) {
                None => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(only_json_tab(&format!("{key} is not text"))),
            }
        };
        let list = |key: &str| -> Result<Vec<String>, String> {
            match base.get(key) {
                None => Ok(Vec::new()),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|i| i.as_str().map(str::to_owned))
                    .collect::<Option<_>>()
                    .ok_or_else(|| {
                        only_json_tab(&format!("{key} holds something other than text"))
                    }),
                Some(_) => Err(only_json_tab(&format!("{key} is not a list"))),
            }
        };
        let pairs = |key: &str| -> Result<Vec<(String, String)>, String> {
            match base.get(key) {
                None => Ok(Vec::new()),
                Some(Value::Object(map)) => map
                    .iter()
                    .map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect::<Option<_>>()
                    .ok_or_else(|| {
                        only_json_tab(&format!("{key} holds something other than text"))
                    }),
                Some(_) => Err(only_json_tab(&format!(
                    "{key} is not a list of names and values"
                ))),
            }
        };
        let transport = match text("type")?.as_str() {
            "" | "stdio" => TransportKind::Stdio,
            "http" => TransportKind::Http,
            "sse" => TransportKind::Sse,
            "ws" => TransportKind::Ws,
            _ => return Err(only_json_tab("its type is not stdio, http, sse or ws")),
        };
        Ok(Self {
            transport,
            command: text("command")?,
            args: list("args")?,
            env: pairs("env")?,
            url: text("url")?,
            headers: pairs("headers")?,
            headers_helper: text("headersHelper")?,
            base,
        })
    }

    /// The definition as JSON, pretty, in the definition's own key order; new keys go at the end.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&Value::Object(self.to_map())).unwrap_or_default()
    }

    /// What stops this definition from being saved, in words; `None` when nothing does. The same checks run again
    /// in the engine before anything is written.
    pub fn problem(&self) -> Option<String> {
        for (what, pairs) in [("env", &self.env), ("headers", &self.headers)] {
            for (i, (name, _)) in pairs.iter().enumerate() {
                if pairs[..i].iter().any(|(n, _)| n == name) {
                    return Some(format!("{what}: {name} appears twice"));
                }
            }
        }
        entry::validate(&Value::Object(self.to_map())).err()
    }

    /// Changed through Claude Code's own command (M2), not this form.
    pub fn signs_in_with_oauth(&self) -> bool {
        self.base.contains_key("oauth")
    }

    fn to_map(&self) -> Map<String, Value> {
        let stdio = self.transport == TransportKind::Stdio;
        let had = |key: &str| self.base.contains_key(key);
        let strings =
            |items: &[String]| Value::Array(items.iter().cloned().map(Value::String).collect());
        let object = |pairs: &[(String, String)]| {
            Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            )
        };
        // Each form key's new value, or `None` to leave it out. Optional keys stay when they were there before, so an
        // unedited definition reads back exactly.
        let wanted = |key: &str| -> Option<Value> {
            match key {
                "type" => (had("type") || !stdio)
                    .then(|| Value::String(type_name(self.transport).to_owned())),
                "command" if stdio => Some(Value::String(self.command.clone())),
                "args" if stdio => {
                    (had("args") || !self.args.is_empty()).then(|| strings(&self.args))
                }
                "env" if stdio => (had("env") || !self.env.is_empty()).then(|| object(&self.env)),
                "url" if !stdio => Some(Value::String(self.url.clone())),
                "headers" if !stdio => {
                    (had("headers") || !self.headers.is_empty()).then(|| object(&self.headers))
                }
                "headersHelper" if !stdio => (had("headersHelper")
                    || !self.headers_helper.is_empty())
                .then(|| Value::String(self.headers_helper.clone())),
                _ => None,
            }
        };
        let mut out = Map::new();
        for (key, value) in &self.base {
            if FORM_KEYS.contains(&key.as_str()) {
                if let Some(v) = wanted(key) {
                    out.insert(key.clone(), v);
                }
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
        for key in FORM_KEYS {
            if !out.contains_key(key)
                && let Some(v) = wanted(key)
            {
                out.insert(key.to_owned(), v);
            }
        }
        out
    }
}

fn type_name(transport: TransportKind) -> &'static str {
    match transport {
        TransportKind::Http => "http",
        TransportKind::Sse => "sse",
        TransportKind::Ws => "ws",
        TransportKind::Stdio | TransportKind::Invalid => "stdio",
    }
}

fn only_json_tab(why: &str) -> String {
    format!("This definition can only be edited in the JSON tab: {why}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pretty(v: &Value) -> String {
        serde_json::to_string_pretty(v).unwrap()
    }

    fn def(v: Value) -> Definition {
        Definition::from_json(&pretty(&v)).unwrap()
    }

    #[test]
    fn an_unedited_definition_reads_back_exactly() {
        // The engine sends pretty JSON; reading it into the form and back must not look like an edit.
        for v in [
            json!({"command": "node"}),
            json!({"type": "stdio", "command": "npx", "args": ["-y", "pkg"], "env": {"A": "<hidden>", "B": "<hidden>"}}),
            json!({"command": "x", "args": [], "env": {}, "cwd": "/tmp", "timeout": 30}),
            json!({"type": "http", "url": "https://h.example/mcp", "headers": {"Authorization": "<hidden>"}, "headersHelper": "tok"}),
            json!({"url": "https://h.example/sse", "type": "sse", "x-extra": {"nested": [1, 2]}}),
            json!({"type": "ws", "url": "wss://h.example/ws", "headersHelper": ""}),
        ] {
            let text = pretty(&v);
            assert_eq!(Definition::from_json(&text).unwrap().to_json(), text, "{v}");
        }
    }

    #[test]
    fn edits_change_only_their_own_keys_in_place() {
        let mut d = def(
            json!({"cwd": "/w", "command": "node", "args": ["a.js"], "env": {"TOKEN": "<hidden>"}, "z": 1}),
        );
        assert_eq!(d.transport, TransportKind::Stdio);
        d.command = "node22".into();
        d.args.push("--x".into());
        d.env.push(("MODE".into(), "dev".into()));
        let out: Value = serde_json::from_str(&d.to_json()).unwrap();
        assert_eq!(
            out,
            json!({"cwd": "/w", "command": "node22", "args": ["a.js", "--x"], "env": {"TOKEN": "<hidden>", "MODE": "dev"}, "z": 1})
        );
        let keys: Vec<&String> = out.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["cwd", "command", "args", "env", "z"], "order kept");
        // Emptied lists that were not there before stay away; ones that were there stay, empty.
        let mut d = def(json!({"command": "x", "args": ["a"]}));
        d.args.clear();
        d.env.clear();
        assert_eq!(
            serde_json::from_str::<Value>(&d.to_json()).unwrap(),
            json!({"command": "x", "args": []})
        );
    }

    #[test]
    fn switching_the_type_keeps_only_what_the_new_type_uses() {
        let mut d = def(
            json!({"command": "node", "args": ["a"], "env": {"A": "<hidden>"}, "note": "kept"}),
        );
        d.transport = TransportKind::Http;
        d.url = "https://h.example/mcp".into();
        d.headers.push(("X-Key".into(), "v".into()));
        assert_eq!(
            serde_json::from_str::<Value>(&d.to_json()).unwrap(),
            json!({"note": "kept", "type": "http", "url": "https://h.example/mcp", "headers": {"X-Key": "v"}})
        );
        let mut back =
            def(json!({"type": "sse", "url": "https://h.example/sse", "headersHelper": "t"}));
        back.transport = TransportKind::Stdio;
        back.command = "node".into();
        assert_eq!(
            serde_json::from_str::<Value>(&back.to_json()).unwrap(),
            json!({"type": "stdio", "command": "node"})
        );
    }

    #[test]
    fn shapes_the_form_cannot_show_are_left_to_the_json_tab() {
        for (bad, why) in [
            (json!([1]), "object"),
            (json!({"type": "pigeon"}), "type"),
            (json!({"command": 1}), "command"),
            (json!({"command": "x", "args": "a b"}), "args"),
            (json!({"command": "x", "args": [1]}), "args"),
            (json!({"command": "x", "env": {"A": 1}}), "env"),
            (
                json!({"type": "http", "url": "https://h", "headers": []}),
                "headers",
            ),
        ] {
            let err = Definition::from_json(&pretty(&bad)).unwrap_err();
            assert!(
                err.contains(why) && err.contains("JSON tab"),
                "{bad}: {err}"
            );
        }
        assert!(
            Definition::from_json("{ not json")
                .unwrap_err()
                .contains("JSON")
        );
    }

    #[test]
    fn problems_are_found_before_anything_is_sent() {
        let ok = def(json!({"command": "node", "env": {"A": "<hidden>"}}));
        assert_eq!(ok.problem(), None);
        let mut d = ok.clone();
        d.command = " ".into();
        assert!(d.problem().unwrap().contains("command"));
        let mut d = ok.clone();
        d.env.push(("A".into(), "again".into()));
        assert!(d.problem().unwrap().contains("twice"), "{:?}", d.problem());
        let mut d = ok.clone();
        d.env.push((String::new(), "v".into()));
        assert!(d.problem().unwrap().contains("name"));
        let mut d = ok.clone();
        d.args.push("a\nb".into());
        assert!(d.problem().unwrap().contains("line breaks"));
        let mut d = ok;
        d.transport = TransportKind::Http;
        d.url = "file:///etc/passwd".into();
        assert!(
            d.problem().unwrap().contains("https://"),
            "{:?}",
            d.problem()
        );
        let oauth =
            def(json!({"type": "http", "url": "https://h.example", "oauth": {"clientId": "c"}}));
        assert!(oauth.signs_in_with_oauth());
        assert!(oauth.problem().unwrap().contains("OAuth"));
    }
}
