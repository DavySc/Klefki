//! Export: server definitions as one `{"mcpServers": {…}}` block, to keep, share or add on another computer. Klefki's
//! Add reads the block back (paste), and so does a `.mcp.json`. Values that may be secrets do not leave: every
//! environment and header value, and each argument, address part or other setting that looks like a secret, becomes
//! a `${VAR}` reference, which Claude Code fills in from its own environment. The block says which variables to set.
//! Nothing is written anywhere: the text is the result.
use crate::api::{Scope, ServerId, Snapshot};
use crate::config::ConfigPaths;
use crate::config::inventory::{self, FileKey, FileKeys};
use crate::config::secretlike::{
    SCHEMES, has_secret_word, only_references, plain_setting, rewritten_args, rewritten_url,
    suggestion, variable,
};
use crate::sanitize::{is_bad, shown};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// What an export gives: the block, and what to know before it is used. Text other than the block is sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    /// `{"mcpServers": {…}}`, pretty-printed. Characters that could disguise text (bidi and control characters) are
    /// written as `\u` escapes, so the block shows what it holds and still means the same.
    pub text: String,
    /// How many servers the block holds.
    pub servers: usize,
    /// How many servers asked for are not in it, because they could not be read (the notes say why).
    pub left_out: usize,
    /// Each variable the export put in for a value, in the order they first appear.
    pub variables: Vec<Variable>,
    /// Servers left out or renamed, and why; servers whose client secret stays behind.
    pub notes: Vec<String>,
}

/// A `${NAME}` reference the export wrote instead of a value: set `NAME` where the block is used, or, for a plain
/// setting, leave it to its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    /// The value it stands for when it is plainly a setting, kept as the reference's default (`${LOG_LEVEL:-debug}`)
    /// so the server works as before without it being set. `None` for anything that may be a secret: it must be set.
    pub default: Option<String>,
    /// What it stands for: `github: env GITHUB_TOKEN`, `docs: argument after --api-key`.
    pub replaces: Vec<String>,
}

/// One server to export: its name as the file spells it, and its definition, or why it could not be read.
pub(crate) struct Source {
    pub name: String,
    pub definition: Result<Value, String>,
}

/// The servers `ids` names, in that order, read from the files (each file once). A server no longer in the snapshot
/// is a [`Source`] that says so.
pub(crate) fn sources(
    paths: Option<&ConfigPaths>,
    snapshot: &Snapshot,
    keys: &FileKeys,
    ids: &[ServerId],
) -> Vec<Source> {
    let found: Vec<Option<(&FileKey, bool)>> = ids
        .iter()
        .map(|id| {
            inventory::locate(snapshot, keys, id)
                .map(|(row, key)| (key, row.scope == Scope::Project))
        })
        .collect();
    let wanted: Vec<(&FileKey, bool)> = found.iter().flatten().copied().collect();
    let mut read = match paths {
        Some(paths) => inventory::server_entries(paths, &wanted),
        None => {
            vec![
                Err("Could not determine where Claude Code keeps its configuration.".to_owned());
                wanted.len()
            ]
        }
    }
    .into_iter();
    ids.iter()
        .zip(found)
        .map(|(id, found)| match found {
            Some((key, _)) => Source {
                name: key.name.clone(),
                definition: read.next().expect("one answer per server read"),
            },
            None => Source {
                name: id.name.clone(),
                definition: Err("it is no longer in the configuration".to_owned()),
            },
        })
        .collect()
}

pub(crate) fn export(sources: Vec<Source>) -> Exported {
    let mut refs = References::default();
    for source in &sources {
        if let Ok(definition) = &source.definition {
            refs.reserve_written(definition);
        }
    }
    let mut servers = Map::new();
    let mut notes = Vec::new();
    let mut left_out = 0;
    for source in sources {
        let shown_name = shown(&source.name);
        let definition = match source.definition {
            Ok(Value::Object(definition)) => definition,
            Ok(_) => {
                left_out += 1;
                notes.push(format!(
                    "Left out {shown_name}: its definition is not a JSON object."
                ));
                continue;
            }
            Err(why) => {
                left_out += 1;
                notes.push(format!("Left out {shown_name}: {}.", shown(&why)));
                continue;
            }
        };
        let exported = refs.redact(&shown_name, definition);
        let name = match servers.get(&source.name) {
            None => source.name,
            Some(same) if same.as_object() == Some(&exported) => {
                notes.push(format!(
                    "{shown_name} is the same in more than one place; it is in the block once."
                ));
                continue;
            }
            Some(_) => {
                let other = free_name(&source.name, &servers);
                notes.push(format!(
                    "More than one server is called {shown_name}; another one is in the block as {}.",
                    shown(&other)
                ));
                other
            }
        };
        if exported.get("oauth").is_some() {
            notes.push(format!(
                "{} signs in with OAuth: its client secret is not in the block. Type it in when you add it.",
                shown(&name)
            ));
        }
        servers.insert(name, Value::Object(exported));
    }
    notes.append(&mut refs.notes);
    let count = servers.len();
    let block = Value::Object(Map::from_iter([(
        "mcpServers".to_owned(),
        Value::Object(servers),
    )]));
    let text = serde_json::to_string_pretty(&block).expect("a JSON value always serialises");
    Exported {
        text: escaped(&text),
        servers: count,
        left_out,
        variables: refs.variables,
        notes,
    }
}

/// The longest server name Claude Code takes ([`ServerName`](crate::model::ServerName)).
const MAX_NAME_CHARS: usize = 64;

/// `name` with the smallest `_2`, `_3`… that no server in `taken` has, still a valid server name.
fn free_name(name: &str, taken: &Map<String, Value>) -> String {
    (2..)
        .map(|n| {
            let suffix = format!("_{n}");
            let mut base: String = name.to_owned();
            while base.chars().count() + suffix.len() > MAX_NAME_CHARS {
                base.pop();
            }
            base + &suffix
        })
        .find(|candidate| !taken.contains_key(candidate))
        .expect("some suffix is free")
}

/// The block with every character that could disguise text written as a JSON `\u` escape. Such characters can only
/// be inside strings (JSON's own syntax is ASCII), where the escape means the same character.
fn escaped(text: &str) -> String {
    if !text.chars().any(is_bad) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if is_bad(c) {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The variables an export puts in, so one name never stands for two different values: the same secret in two
/// servers gets one variable, a different one under the same name gets `NAME_2`. Names the definitions already
/// refer to are never reused, as they may hold something else.
#[derive(Default)]
struct References {
    /// Each name in use, with a fingerprint of the value it stands for; `None` for a name already written in a
    /// definition.
    taken: HashMap<String, Option<u64>>,
    variables: Vec<Variable>,
    /// Values that mixed a reference with text of their own, which were replaced whole.
    notes: Vec<String>,
}

impl References {
    /// Takes every `${NAME}` (and `${NAME:-default}`) already in `value`.
    fn reserve_written(&mut self, value: &Value) {
        match value {
            Value::String(s) => {
                let mut rest = s.as_str();
                while let Some(at) = rest.find("${") {
                    rest = &rest[at + 2..];
                    let end = rest.find([':', '}']).unwrap_or(rest.len());
                    self.taken.insert(rest[..end].to_owned(), None);
                    rest = &rest[end..];
                }
            }
            Value::Array(items) => items.iter().for_each(|v| self.reserve_written(v)),
            Value::Object(map) => map.values().for_each(|v| self.reserve_written(v)),
            _ => {}
        }
    }

    /// `${NAME}` for `value`, named after `hint` (a variable, header or flag name), and noted as replacing `what`.
    fn reference(&mut self, hint: &str, value: &str, what: String) -> String {
        self.reference_with(hint, value, what, false)
    }

    /// [`reference`](Self::reference), or `${NAME:-value}` when `keep_plain` and the value is plainly a setting.
    fn reference_with(
        &mut self,
        hint: &str,
        value: &str,
        what: String,
        keep_plain: bool,
    ) -> String {
        let default = (keep_plain && plain_setting(hint, value)).then(|| value.to_owned());
        if value.contains("${") {
            self.notes.push(format!(
                "{what} mixed a reference with text of its own, which may be a secret: all of it is now one variable."
            ));
        }
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let base = variable(hint);
        let name = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}_{n}")))
            .find(|name| match self.taken.get(name) {
                None => true,
                Some(taken) => *taken == Some(fingerprint),
            })
            .expect("some suffix is free");
        self.taken.insert(name.clone(), Some(fingerprint));
        match self.variables.iter_mut().find(|v| v.name == name) {
            Some(known) if !known.replaces.contains(&what) => known.replaces.push(what),
            Some(_) => {}
            None => self.variables.push(Variable {
                name: name.clone(),
                default: default.clone(),
                replaces: vec![what],
            }),
        }
        match default {
            Some(default) => format!("${{{name}:-{default}}}"),
            None => format!("${{{name}}}"),
        }
    }

    /// The definition of `server` with every value that may be a secret replaced by a reference.
    fn redact(&mut self, server: &str, mut definition: Map<String, Value>) -> Map<String, Value> {
        for (key, value) in definition.iter_mut() {
            match (key.as_str(), value) {
                ("env", Value::Object(values)) => {
                    for (name, value) in values.iter_mut() {
                        if let Some(written) =
                            self.whole(value, name, || format!("{server}: env {}", shown(name)))
                        {
                            *value = written;
                        }
                    }
                }
                ("headers", Value::Object(values)) => {
                    for (name, value) in values.iter_mut() {
                        let Value::String(text) = value else {
                            continue;
                        };
                        let what = format!("{server}: header {}", shown(name));
                        // "Bearer abc": the scheme stays, the credential goes.
                        *value = Value::String(match text.trim().split_once(' ') {
                            Some((scheme, credential))
                                if SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) =>
                            {
                                if only_references(name, credential) {
                                    continue;
                                }
                                let reference = self.reference(name, credential.trim(), what);
                                format!("{scheme} {reference}")
                            }
                            _ if only_references(name, text) => continue,
                            _ => self.reference_with(name, text, what, true),
                        });
                    }
                }
                ("args", Value::Array(items)) => {
                    let args: Vec<String> = items
                        .iter()
                        .map(|a| a.as_str().unwrap_or_default().to_owned())
                        .collect();
                    let rewritten = rewritten_args(
                        args.iter().map(String::as_str),
                        true,
                        &mut |flag, secret| {
                            let what = match flag {
                                "" => format!("{server}: an argument"),
                                flag => format!("{server}: argument {}", shown(flag)),
                            };
                            self.reference(flag, secret, what)
                        },
                    );
                    for ((item, before), after) in items.iter_mut().zip(&args).zip(rewritten) {
                        if item.is_string() && *before != after {
                            *item = Value::String(after);
                        }
                    }
                }
                ("url", Value::String(url)) => {
                    if let Some(written) = rewritten_url(url, &mut |name, secret| {
                        let what = match name {
                            "" => format!("{server}: the address's sign-in"),
                            name => format!("{server}: address parameter {}", shown(name)),
                        };
                        self.reference(
                            if name.is_empty() {
                                "url credentials"
                            } else {
                                name
                            },
                            secret,
                            what,
                        )
                    }) {
                        *url = written;
                    }
                }
                ("type", _) => {}
                // What it runs: a program, or for headersHelper a whole shell line, which can hold a token. One with a
                // credential in it is replaced whole, as a partly rewritten shell line is hard to trust.
                (key @ ("command" | "headersHelper"), Value::String(line)) => {
                    if has_secret_word(line) && !only_references("", line) {
                        let what = format!("{server}: {key}");
                        *line = self.reference(key, line, what);
                    }
                }
                (key, value) => self.settings(server, key, value),
            }
        }
        definition
    }

    /// Any other setting, at any depth (`oauth`, settings Klefki does not know): a text value that looks like a
    /// secret for its name becomes a reference.
    fn settings(&mut self, server: &str, key: &str, value: &mut Value) {
        match value {
            Value::String(text) => {
                if suggestion(key, text).is_some() {
                    let what = format!("{server}: setting {}", shown(key));
                    *value = Value::String(self.reference(key, text, what));
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.settings(server, key, item);
                }
            }
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    self.settings(server, key, value);
                }
            }
            _ => {}
        }
    }

    /// An environment value as a reference, unless it already is one: `None` to leave it.
    fn whole(&mut self, value: &Value, name: &str, what: impl FnOnce() -> String) -> Option<Value> {
        let text = match value {
            Value::String(s) if only_references(name, s) => return None,
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Some(Value::String(self.reference_with(
            name,
            &text,
            what(),
            true,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServerName;
    use serde_json::json;

    fn source(name: &str, definition: Value) -> Source {
        Source {
            name: name.to_owned(),
            definition: Ok(definition),
        }
    }

    fn block(exported: &Exported) -> Value {
        serde_json::from_str(&exported.text).expect("the block is JSON")
    }

    fn names(exported: &Exported) -> Vec<&str> {
        exported.variables.iter().map(|v| v.name.as_str()).collect()
    }

    #[test]
    fn every_environment_and_header_value_becomes_a_reference_with_plain_settings_as_defaults() {
        let out = export(vec![
            source(
                "github",
                json!({"command": "npx", "args": ["-y", "gh-mcp"],
                       "env": {"GITHUB_TOKEN": "ghp_abcdefghijklmnop1234", "LOG_LEVEL": "debug", "PORT": 8080}}),
            ),
            source(
                "gateway",
                json!({"type": "http", "url": "https://mcp.example.com/mcp",
                       "headers": {"Authorization": "Bearer sk-abcdef1234567890", "X-Team": "blue"}}),
            ),
        ]);
        assert_eq!(
            block(&out),
            json!({"mcpServers": {
                "github": {"command": "npx", "args": ["-y", "gh-mcp"],
                           "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}", "LOG_LEVEL": "${LOG_LEVEL:-debug}", "PORT": "${PORT:-8080}"}},
                "gateway": {"type": "http", "url": "https://mcp.example.com/mcp",
                            "headers": {"Authorization": "Bearer ${AUTH_TOKEN}", "X-Team": "${X_TEAM:-blue}"}}
            }})
        );
        assert_eq!(out.servers, 2);
        assert_eq!(
            names(&out),
            ["GITHUB_TOKEN", "LOG_LEVEL", "PORT", "AUTH_TOKEN", "X_TEAM"]
        );
        assert_eq!(out.variables[3].replaces, ["gateway: header Authorization"]);
        // Plain settings keep their value as the default, so the server works as before; secrets must be set.
        let defaults: Vec<Option<&str>> =
            out.variables.iter().map(|v| v.default.as_deref()).collect();
        assert_eq!(
            defaults,
            [None, Some("debug"), Some("8080"), None, Some("blue")]
        );
        for secret in ["ghp_abcdefghijklmnop1234", "sk-abcdef1234567890"] {
            assert!(!out.text.contains(secret), "{secret} left in {}", out.text);
        }
    }

    #[test]
    fn references_already_written_stay_and_their_names_are_not_reused() {
        let out = export(vec![
            source(
                "a",
                json!({"command": "x", "env": {"TOKEN": "${TOKEN}", "HOME_DIR": "${HOME:-/home/me}"}}),
            ),
            source(
                "b",
                json!({"command": "y", "env": {"TOKEN": "tok-1234567890", "HOME": "/home/other"}}),
            ),
        ]);
        let block = block(&out);
        assert_eq!(
            block["mcpServers"]["a"]["env"],
            json!({"TOKEN": "${TOKEN}", "HOME_DIR": "${HOME:-/home/me}"})
        );
        // `TOKEN` and `HOME` may hold something else where the block is used: b's values get names of their own.
        assert_eq!(
            block["mcpServers"]["b"]["env"],
            json!({"TOKEN": "${TOKEN_2}", "HOME": "${HOME_2:-/home/other}"})
        );
        assert_eq!(names(&out), ["TOKEN_2", "HOME_2"]);
    }

    #[test]
    fn a_value_that_mixes_a_reference_with_text_is_replaced_whole() {
        // Review of #12: any `${` made the whole value count as a reference, so the text beside it went out.
        let out = export(vec![source(
            "mixed",
            json!({"type": "http", "url": "https://${USER}:hunter2pw@a.example.com/sse",
                   "env": {"DATABASE_URL": "postgres://u:pw999secret@${HOST}/db",
                           "API_KEY": "${API_KEY:-sk-live-abcdef1234567890}",
                           "ONLY": "${A}${B:-plain}"},
                   "headers": {"X-Api-Key": "${PREFIX}sk-abcdef1234567890",
                               "Authorization": "Bearer ${ORG}.sk-abcdef1234567890",
                               "X-Ref": "Bearer ${TOKEN}"},
                   "args": ["--token", "${P}sk-abcdef1234567890", "--key=${K}abcdef123456"]}),
        )]);
        for secret in [
            "hunter2pw",
            "pw999secret",
            "sk-live-abcdef",
            "sk-abcdef1234567890",
            "abcdef123456",
        ] {
            assert!(!out.text.contains(secret), "{secret} left in {}", out.text);
        }
        let block = block(&out);
        let server = &block["mcpServers"]["mixed"];
        // Nothing but references, a plain default included, stays as written.
        assert_eq!(server["env"]["ONLY"], "${A}${B:-plain}");
        assert_eq!(server["headers"]["X-Ref"], "Bearer ${TOKEN}");
        assert_eq!(server["env"]["DATABASE_URL"], "${DATABASE_URL}");
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("mixed: env DATABASE_URL mixed a reference")),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn a_key_in_an_address_path_a_shell_line_or_a_json_argument_becomes_a_reference() {
        // Review of #12: these went out in clear.
        let out = export(vec![
            source(
                "path",
                json!({"type": "http", "url": "https://mcp.example.com/api/mcp/s/sk-live-abcdef1234567890/mcp"}),
            ),
            source(
                "b64",
                json!({"type": "sse", "url": "https://mcp.example.com/s/ZmFrZXNlY3JldHRva2VuMTIzNDU2Nzg5MA/sse"}),
            ),
            source(
                "remote",
                json!({"command": "npx", "args": ["mcp-remote", "https://mcp.example.com/s/sk-live-abcdef1234567890/sse"]}),
            ),
            source(
                "helper",
                json!({"type": "http", "url": "https://h.example.com/mcp",
                "headersHelper": "echo '{\"Authorization\":\"Bearer sk-abcdef1234567890\"}'"}),
            ),
            source(
                "json",
                json!({"command": "x", "args": ["--config", "{\"apiKey\":\"sk-abcdef1234567890\"}"]}),
            ),
            source(
                "plain",
                json!({"type": "http", "url": "https://mcp.example.com/v1/workspaces/team-blue/mcp",
                "headersHelper": "/usr/local/bin/get-headers --profile work"}),
            ),
        ]);
        for secret in ["sk-live-abcdef", "ZmFrZXNl", "sk-abcdef1234567890"] {
            assert!(!out.text.contains(secret), "{secret} left in {}", out.text);
        }
        let block = block(&out);
        let servers = &block["mcpServers"];
        assert_eq!(
            servers["path"]["url"],
            "https://mcp.example.com/api/mcp/s/${URL_TOKEN}/mcp"
        );
        assert_eq!(
            servers["remote"]["args"][1],
            "https://mcp.example.com/s/${URL_TOKEN}/sse"
        );
        assert_eq!(servers["helper"]["headersHelper"], "${HEADERSHELPER}");
        assert_eq!(servers["json"]["args"], json!(["--config", "${CONFIG}"]));
        // Ordinary paths and helpers stay readable.
        assert_eq!(
            servers["plain"]["url"],
            "https://mcp.example.com/v1/workspaces/team-blue/mcp"
        );
        assert_eq!(
            servers["plain"]["headersHelper"],
            "/usr/local/bin/get-headers --profile work"
        );
    }

    #[test]
    fn only_plain_settings_keep_a_default() {
        let out = export(vec![source(
            "s",
            json!({"command": "x", "env": {
                "MODE": "production", "DEBUG": "true", "TIMEOUT": "30", "DIR": "/srv/data",
                "BASE": "https://api.example.com/v1", "DB_PASS": "hunter", "SESSION": "abc",
                "CODE": "12345678901", "WORDY": "correcthorsebatterystaple", "MIXED": "abc123",
                "AUTH_URL": "https://u:p@x.example.com", "QUERY": "https://x.example.com/?k=v",
                // A key in an address's or a file's path, as `url` and arguments already hide it, at any length.
                "ENDPOINT": "https://mcp.example.com/s/ZmFrZXNlY3JldHRva2VuMTIzNDU2Nzg5MA/sse",
                "HOOK": "https://h.io/s/sk-live-abcdef1234567890",
                "SHORT": "https://h.io/ZmFrZXNlY3JldHRva2VuMTIz",
                "FRAGMENT": "https://h.io/#ZmFrZXNlY3JldHRva2VuMTIz",
                "KEYFILE": "/srv/ZmFrZXNlY3JldHRva2VuMTIzNDU2/key"
            }}),
        )]);
        let env = &block(&out)["mcpServers"]["s"]["env"];
        for name in ["MODE", "DEBUG", "TIMEOUT", "DIR", "BASE"] {
            assert!(
                env[name].as_str().unwrap().contains(":-"),
                "{name}: {}",
                env[name]
            );
        }
        for name in [
            "DB_PASS", "SESSION", "CODE", "WORDY", "MIXED", "AUTH_URL", "QUERY", "ENDPOINT",
            "HOOK", "SHORT", "FRAGMENT", "KEYFILE",
        ] {
            assert_eq!(env[name], format!("${{{name}}}"), "{name}");
        }
    }

    #[test]
    fn one_secret_gets_one_variable_and_two_secrets_get_two() {
        let out = export(vec![
            source(
                "a",
                json!({"command": "x", "env": {"API_KEY": "key-1111111111"}}),
            ),
            source(
                "b",
                json!({"command": "y", "env": {"API_KEY": "key-1111111111"}}),
            ),
            source(
                "c",
                json!({"command": "z", "env": {"API_KEY": "key-2222222222"}}),
            ),
        ]);
        let block = block(&out);
        assert_eq!(block["mcpServers"]["a"]["env"]["API_KEY"], "${API_KEY}");
        assert_eq!(block["mcpServers"]["b"]["env"]["API_KEY"], "${API_KEY}");
        assert_eq!(block["mcpServers"]["c"]["env"]["API_KEY"], "${API_KEY_2}");
        assert_eq!(
            out.variables[0].replaces,
            ["a: env API_KEY", "b: env API_KEY"]
        );
        assert_eq!(names(&out), ["API_KEY", "API_KEY_2"]);
    }

    #[test]
    fn secrets_in_arguments_and_addresses_become_references_and_the_rest_stays() {
        let out = export(vec![
            source(
                "tool",
                json!({"command": "npx", "args": [
                    "-y", "alpha-mcp", "--api-key", "sk-live-ARGSECRET123456", "--token=abc12345",
                    "--header", "Authorization: Bearer abcdef1234567890XYZ",
                    "--db", "postgres://me:pw123@db.example.com/x",
                    "https://x.example.com/mcp?region=eu&api_key=k1234567890", "--token-file", "/run/secrets/t"
                ]}),
            ),
            source(
                "remote",
                json!({"type": "sse", "url": "https://u:hunter2pw@mcp.example.com/sse?workspace=me&token=abc123def456"}),
            ),
        ]);
        let block = block(&out);
        assert_eq!(
            block["mcpServers"]["tool"]["args"],
            json!([
                "-y",
                "alpha-mcp",
                "--api-key",
                "${API_KEY}",
                "--token=${TOKEN}",
                "--header",
                "Authorization: Bearer ${AUTH_TOKEN}",
                "--db",
                "postgres://${DB}@db.example.com/x",
                "https://x.example.com/mcp?region=eu&api_key=${API_KEY_2}",
                "--token-file",
                "/run/secrets/t"
            ])
        );
        assert_eq!(
            block["mcpServers"]["remote"]["url"],
            "https://${URL_CREDENTIALS}@mcp.example.com/sse?workspace=me&token=${TOKEN_2}"
        );
        assert_eq!(out.variables[0].replaces, ["tool: argument --api-key"]);
        for secret in [
            "ARGSECRET",
            "abc12345",
            "abcdef1234567890XYZ",
            "pw123",
            "k1234567890",
            "hunter2pw",
            "abc123def456",
        ] {
            assert!(!out.text.contains(secret), "{secret} left in {}", out.text);
        }
    }

    #[test]
    fn a_variable_is_named_after_a_bare_flag_only_never_after_a_value() {
        let out = export(vec![source(
            "tool",
            json!({"command": "x", "args": ["--token=tok-SECRET-98765", "ghp_abcdefghijklmnop1234", "--x9f8e7d6c5b4", "ghp_zyxwvutsrqponmlk9876"]}),
        )]);
        assert_eq!(
            block(&out)["mcpServers"]["tool"]["args"],
            json!([
                "--token=${TOKEN}",
                "${SECRET}",
                "--x9f8e7d6c5b4",
                "${SECRET_2}"
            ])
        );
        for part in ["98765", "X9F8"] {
            assert!(
                !names(&out).iter().any(|n| n.contains(part)),
                "{:?}",
                names(&out)
            );
        }
    }

    #[test]
    fn other_settings_that_look_secret_become_references() {
        let out = export(vec![source(
            "odd",
            json!({"type": "http", "url": "https://mcp.example.com",
                   "oauth": {"clientId": "klefki-app", "callbackPort": 8765, "clientSecret": "cs-0123456789abc"},
                   "custom": {"nested": [{"password": "Pa55word!xyz"}], "mode": "fast"}}),
        )]);
        let block = block(&out);
        assert_eq!(
            block["mcpServers"]["odd"]["oauth"],
            json!({"clientId": "klefki-app", "callbackPort": 8765, "clientSecret": "${CLIENTSECRET}"})
        );
        assert_eq!(
            block["mcpServers"]["odd"]["custom"],
            json!({"nested": [{"password": "${PASSWORD}"}], "mode": "fast"})
        );
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("odd signs in with OAuth")),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn one_name_from_two_places_is_kept_once_when_the_same_and_renamed_when_not() {
        let def = json!({"command": "x"});
        let out = export(vec![
            source("docs", def.clone()),
            source("docs", def),
            source("docs", json!({"command": "y"})),
        ]);
        assert_eq!(
            block(&out)["mcpServers"],
            json!({"docs": {"command": "x"}, "docs_2": {"command": "y"}})
        );
        assert_eq!((out.servers, out.left_out), (2, 0));
        assert_eq!(out.notes.len(), 2, "{:?}", out.notes);
        let long = "n".repeat(64);
        let out = export(vec![
            source(&long, json!({"command": "x"})),
            source(&long, json!({"command": "y"})),
        ]);
        let renamed = format!("{}_2", "n".repeat(62));
        assert!(
            block(&out)["mcpServers"].get(&renamed).is_some(),
            "{}",
            out.text
        );
        assert!(ServerName::parse(&renamed).is_ok());
    }

    #[test]
    fn what_could_not_be_read_is_left_out_with_the_reason() {
        let out = export(vec![
            Source {
                name: "gone".into(),
                definition: Err("the server is no longer in the configuration".into()),
            },
            source("odd", json!(["not", "an", "object"])),
            source("ok", json!({"command": "x"})),
        ]);
        assert_eq!(block(&out), json!({"mcpServers": {"ok": {"command": "x"}}}));
        assert_eq!((out.servers, out.left_out), (1, 2));
        assert_eq!(
            out.notes,
            [
                "Left out gone: the server is no longer in the configuration.",
                "Left out odd: its definition is not a JSON object."
            ]
        );
    }

    #[test]
    fn characters_that_disguise_text_are_escaped_and_mean_the_same() {
        let name = "safe\u{202e}evil";
        let out = export(vec![source(
            name,
            json!({"command": "run\u{1b}[31m\u{85}x"}),
        )]);
        assert!(!out.text.chars().any(is_bad), "{:?}", out.text);
        assert!(out.text.contains("\\u202e"), "{}", out.text);
        let back = block(&out);
        assert_eq!(back["mcpServers"][name]["command"], "run\u{1b}[31m\u{85}x");
        // Notes about it show the name cleaned.
        let out = export(vec![Source {
            name: name.into(),
            definition: Err("x".into()),
        }]);
        assert!(!out.notes[0].chars().any(is_bad));
    }

    #[test]
    fn nothing_selected_gives_an_empty_block() {
        let out = export(Vec::new());
        assert_eq!(block(&out), json!({"mcpServers": {}}));
        assert_eq!(out.servers, 0);
    }

    #[test]
    fn the_block_reads_back_through_paste_with_every_server_and_no_secret() {
        let out = export(vec![
            source(
                "github",
                json!({"command": "npx", "args": ["-y", "gh-mcp"], "env": {"GITHUB_TOKEN": "ghp_abcdefghijklmnop1234"}}),
            ),
            source(
                "gateway",
                json!({"type": "http", "url": "https://mcp.example.com/mcp", "headers": {"Authorization": "Bearer sk-abcdef1234567890"}}),
            ),
        ]);
        let pasted = crate::paste::Pasted::parse(&out.text).expect("Add reads the block");
        let names: Vec<&str> = pasted.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["github", "gateway"]);
        for server in &pasted.servers {
            assert_eq!(server.problem, None, "{server:?}");
            // Already references: Add has no secret to point out.
            assert!(server.hints.is_empty(), "{server:?}");
            assert!(
                !server.json.contains("ghp_") && !server.json.contains("sk-"),
                "{}",
                server.json
            );
        }
    }
}
