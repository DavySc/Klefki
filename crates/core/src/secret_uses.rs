//! Which servers use secrets, and where each one lives: typed into a definition file, filled in by Claude Code from
//! an environment variable, made by a command, or kept in the system's secret store. For the Secrets page. Only
//! names and places leave here, never a value: the page has nothing to leak.
//!
//! What counts as a secret is what the rest of Klefki already hides or points out: the edit form's `${VAR}` hint,
//! the masked command line, and Export's redaction ([`crate::config::secretlike`]).
use crate::api::{Scope, ServerId, Snapshot};
use crate::config::ConfigPaths;
use crate::config::inventory::{self, FileKeys};
use crate::config::secretlike::{
    SCHEMES, has_secret_word, hideable, only_references, rewritten_args, rewritten_url,
    sounds_secret, suggestion, variable,
};
use crate::model::ConnectionId;
use crate::sanitize::shown;
use serde_json::{Map, Value};
use std::collections::HashSet;

/// What the Secrets page shows. Text is sanitised.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Secrets {
    /// Servers that use at least one secret, those with a secret typed into their file first, then in the
    /// snapshot's order.
    pub servers: Vec<ServerSecrets>,
    /// Client secrets in the system's store that no server uses any more: the server was removed or renamed outside
    /// Klefki. Klefki never deletes one by itself ([`crate::api::Command::ForgetUnusedSecret`]).
    pub unused: Vec<UnusedSecret>,
    /// Why the system's secret store could not be looked at; `None` when it was, or when no server signs in with
    /// OAuth and nothing needed it.
    pub store_problem: Option<String>,
    /// Servers whose definition could not be read, and why.
    pub notes: Vec<String>,
    /// The configuration file whose servers were looked at, as it can be shown (`~/.claude.json`). A client secret is
    /// "unused" when no server there uses it: another Claude Code configuration on this computer
    /// (`CLAUDE_CONFIG_DIR`) shares the same store and may still need it.
    pub config: String,
}

impl Secrets {
    /// How many secrets are typed into a file: the ones worth moving out.
    pub fn written(&self) -> usize {
        self.servers
            .iter()
            .flat_map(|s| &s.uses)
            .filter(|u| matches!(u.place, Place::Written { .. }))
            .count()
    }
}

/// One server's secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSecrets {
    pub id: ServerId,
    /// The file its definition is in, as it can be shown: `~/.claude.json`, or the project's `.mcp.json`.
    pub file: String,
    pub uses: Vec<SecretUse>,
}

/// One secret a server uses: what it is, and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretUse {
    pub kind: SecretKind,
    /// The variable, header, flag, parameter or setting it goes by; empty when it has no name of its own.
    pub name: String,
    pub place: Place,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// A value in `env`.
    Env,
    /// A value in `headers`.
    Header,
    /// A command-line argument.
    Argument,
    /// Part of the server's address: its sign-in (`user:password@`) or a query parameter.
    Address,
    /// The command line itself (one with a credential in it).
    Command,
    /// `headersHelper`: a command Claude Code runs for the headers.
    HeadersHelper,
    /// Any other setting, such as one inside `oauth`.
    Setting,
    /// The OAuth client secret, which Klefki keeps.
    ClientSecret,
}

impl SecretKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Env => "Environment variable",
            Self::Header => "Header",
            Self::Argument => "Argument",
            Self::Address => "Address",
            Self::Command => "Command line",
            Self::HeadersHelper => "Headers helper",
            Self::Setting => "Setting",
            Self::ClientSecret => "OAuth client secret",
        }
    }
}

/// Where a secret lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// Typed into the definition file, so anyone who can read the file can read the secret. `instead` is a `${VAR}`
    /// reference to write in its place, for an environment or header value (as the edit form suggests).
    Written { instead: Option<String> },
    /// Claude Code fills it in from these environment variables when it starts the server.
    Environment(Vec<String>),
    /// A command Claude Code runs makes it, each time it connects.
    Helper,
    /// The system's secret store, under the server's connection: `Some(true)` when one is stored there, `None` when
    /// the store could not be asked ([`Secrets::store_problem`]).
    Store(Option<bool>),
}

/// A client secret in the system's store that no server uses. Shown as the server it was kept for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedSecret {
    pub connection: ConnectionId,
    pub scope: Scope,
    pub project: Option<String>,
    pub name: String,
}

/// The secrets of every server in `snapshot`, from the files (each file read once). Client secrets are filled in
/// by [`with_store`].
pub(crate) fn read(
    paths: Option<&ConfigPaths>,
    home: Option<&std::path::Path>,
    snapshot: &Snapshot,
    keys: &FileKeys,
) -> Secrets {
    let ids: Vec<ServerId> = snapshot.servers.iter().map(|r| r.id()).collect();
    let sources = crate::export::sources(paths, snapshot, keys, &ids);
    let mut secrets = Secrets {
        config: paths.map_or_else(
            || ".claude.json".to_owned(),
            |p| short(&p.claude_json, home),
        ),
        ..Secrets::default()
    };
    for (row, source) in snapshot.servers.iter().zip(sources) {
        let definition = match source.definition {
            Ok(Value::Object(definition)) => definition,
            Ok(_) => continue,
            Err(why) => {
                secrets
                    .notes
                    .push(format!("Could not read {}: {}.", row.name, shown(&why)));
                continue;
            }
        };
        let uses = uses(&definition);
        if uses.is_empty() {
            continue;
        }
        let file = match (row.scope, &row.project) {
            (Scope::Project, Some(project)) => format!("{project}/.mcp.json"),
            _ => secrets.config.clone(),
        };
        secrets.servers.push(ServerSecrets {
            id: row.id(),
            file,
            uses,
        });
    }
    // Typed-in secrets first: they are the ones to do something about. Stable, so the snapshot's order stays within.
    secrets.servers.sort_by_key(|s| {
        !s.uses
            .iter()
            .any(|u| matches!(u.place, Place::Written { .. }))
    });
    secrets
}

/// `path` as it can be shown, with the home folder as `~`.
fn short(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    let path = match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => std::path::Path::new("~").join(rest),
        None => path.to_path_buf(),
    };
    shown(&path.display().to_string())
}

/// `secrets` with what the system's store holds: `stored` is every connection Klefki keeps a client secret for, or
/// why the store could not be listed. `kept` are the connections of definitions Klefki keeps to put back, whose
/// secrets are in use although their server is not in the configuration now.
pub(crate) fn with_store(
    mut secrets: Secrets,
    snapshot: &Snapshot,
    stored: Result<Vec<ConnectionId>, String>,
    kept: &[ConnectionId],
) -> Secrets {
    let stored = match stored {
        Ok(stored) => Some(stored),
        Err(why) => {
            secrets.store_problem = Some(shown(&why));
            None
        }
    };
    let held: Option<HashSet<&ConnectionId>> = stored.as_ref().map(|s| s.iter().collect());
    for server in &mut secrets.servers {
        let stored = held.as_ref().map(|h| h.contains(&server.id.connection));
        // No client secret stored is the usual case (a public client, or one that registers itself): nothing to say.
        server.uses.retain_mut(|u| {
            if u.kind == SecretKind::ClientSecret {
                u.place = Place::Store(stored);
            }
            u.place != Place::Store(Some(false))
        });
    }
    secrets.servers.retain(|s| !s.uses.is_empty());
    let used: HashSet<&ConnectionId> = snapshot
        .servers
        .iter()
        .filter(|r| r.oauth.is_some())
        .map(|r| &r.connection)
        .chain(kept)
        .collect();
    let mut unused: Vec<UnusedSecret> = stored
        .iter()
        .flatten()
        .filter(|c| !used.contains(c))
        .filter_map(|c| {
            let (scope, project, name) = ConnectionId::parse(c.as_str())?;
            Some(UnusedSecret {
                connection: c.clone(),
                scope,
                project: project.map(|p| shown(&inventory::normalise_project(&p))),
                name: shown(&name),
            })
        })
        .collect();
    unused.sort_by(|a, b| (&a.name, &a.project).cmp(&(&b.name, &b.project)));
    unused.dedup_by(|a, b| a.connection == b.connection);
    secrets.unused = unused;
    secrets
}

/// The secrets one definition uses, in the order the definition has them. The client secret's place is not known
/// here: [`Place::Store`]`(None)` until [`with_store`].
pub(crate) fn uses(definition: &Map<String, Value>) -> Vec<SecretUse> {
    let mut out = Vec::new();
    for (key, value) in definition {
        match (key.as_str(), value) {
            ("env", Value::Object(values)) => {
                for (name, value) in values {
                    if let Some(place) = value.as_str().and_then(|v| value_place(name, v, false)) {
                        out.push(named(SecretKind::Env, name, place));
                    }
                }
            }
            ("headers", Value::Object(values)) => {
                for (name, value) in values {
                    if let Some(place) = value.as_str().and_then(|v| value_place(name, v, true)) {
                        out.push(named(SecretKind::Header, name, place));
                    }
                }
            }
            ("args", Value::Array(items)) => {
                let args: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                let mut found = Vec::new();
                rewritten_args(args, false, &mut |flag, _| {
                    found.push(flag.to_owned());
                    String::new()
                });
                out.extend(found.iter().map(|flag| {
                    named(SecretKind::Argument, flag, Place::Written { instead: None })
                }));
            }
            ("url", Value::String(url)) => {
                let mut found = Vec::new();
                rewritten_url(url, &mut |name, _| {
                    found.push(name.to_owned());
                    String::new()
                });
                out.extend(found.iter().map(|name| {
                    named(SecretKind::Address, name, Place::Written { instead: None })
                }));
            }
            ("command", Value::String(line)) => {
                if has_secret_word(line) && !only_references("", line) {
                    out.push(named(
                        SecretKind::Command,
                        "",
                        Place::Written { instead: None },
                    ));
                }
            }
            ("headersHelper", Value::String(line)) => {
                // A credential in the line itself is typed into the file; otherwise the command makes the headers.
                let place = if has_secret_word(line) && !only_references("", line) {
                    Place::Written { instead: None }
                } else {
                    Place::Helper
                };
                out.push(named(SecretKind::HeadersHelper, "", place));
            }
            ("oauth", Value::Object(_)) => {
                out.push(named(SecretKind::ClientSecret, "", Place::Store(None)));
                settings(key, value, &mut out);
            }
            ("type", _) => {}
            (key, value) => settings(key, value, &mut out),
        }
    }
    out
}

fn named(kind: SecretKind, name: &str, place: Place) -> SecretUse {
    SecretUse {
        kind,
        name: shown(name),
        place,
    }
}

/// Where an environment or header value under `name` comes from, when it is a secret: `None` for a plain setting.
/// With `scheme`, a leading `Bearer`/`Basic`/`Token` is not part of the secret.
fn value_place(name: &str, value: &str, scheme: bool) -> Option<Place> {
    let (scheme, credential) = match value.trim().split_once(' ') {
        Some((s, rest)) if scheme && SCHEMES.iter().any(|x| x.eq_ignore_ascii_case(s)) => {
            (Some(s), rest.trim())
        }
        _ => (None, value.trim()),
    };
    if credential.is_empty() {
        return None;
    }
    if only_references(name, credential) {
        let variables = references(credential);
        return (sounds_secret(name) || variables.iter().any(|v| sounds_secret(v)))
            .then(|| Place::Environment(variables.iter().map(|v| shown(v)).collect()));
    }
    let instead = suggestion(name, value).or_else(|| {
        (sounds_secret(name) && hideable(credential)).then(|| {
            let reference = format!("${{{}}}", variable(name));
            scheme.map_or_else(|| reference.clone(), |s| format!("{s} {reference}"))
        })
    })?;
    Some(Place::Written {
        instead: Some(shown(&instead)),
    })
}

/// The names of the `${NAME}` references in `value`, in order and once each.
fn references(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = value;
    while let Some(at) = rest.find("${") {
        rest = &rest[at + 2..];
        let end = rest.find([':', '}']).unwrap_or(rest.len());
        let name = &rest[..end];
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
        rest = &rest[end..];
    }
    out
}

/// Any other setting, at any depth, whose text looks like a secret for its name.
fn settings(key: &str, value: &Value, out: &mut Vec<SecretUse>) {
    match value {
        Value::String(text) => {
            if suggestion(key, text).is_some() {
                out.push(named(
                    SecretKind::Setting,
                    key,
                    Place::Written { instead: None },
                ));
            }
        }
        Value::Array(items) => items.iter().for_each(|item| settings(key, item, out)),
        Value::Object(map) => map.iter().for_each(|(k, v)| settings(k, v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn of(definition: Value) -> Vec<(SecretKind, String, Place)> {
        uses(definition.as_object().unwrap())
            .into_iter()
            .map(|u| (u.kind, u.name, u.place))
            .collect()
    }

    fn written() -> Place {
        Place::Written { instead: None }
    }

    #[test]
    fn a_typed_in_environment_value_is_written_with_the_reference_to_use_instead() {
        let found = of(json!({
            "command": "npx",
            "env": {
                "GITHUB_TOKEN": "ghp_0123456789abcdefghijABCDEFGHIJ012345",
                "LOG_LEVEL": "debug",
                "API_KEY": "${API_KEY}",
                "HOME_DIR": "${HOME}",
                "DB_PASSWORD": "pw-1234"
            }
        }));
        assert_eq!(
            found,
            [
                (
                    SecretKind::Env,
                    "GITHUB_TOKEN".into(),
                    Place::Written {
                        instead: Some("${GITHUB_TOKEN}".into())
                    }
                ),
                (
                    SecretKind::Env,
                    "API_KEY".into(),
                    Place::Environment(vec!["API_KEY".into()])
                ),
                (
                    SecretKind::Env,
                    "DB_PASSWORD".into(),
                    Place::Written {
                        instead: Some("${DB_PASSWORD}".into())
                    }
                ),
            ],
            "a plain setting and a reference to something that is not a secret are left out"
        );
    }

    #[test]
    fn a_header_counts_its_credential_not_its_scheme() {
        let found = of(json!({
            "type": "http",
            "url": "https://mcp.example.com/",
            "headers": {
                "Authorization": "Bearer ${TOKEN}",
                "X-Api-Version": "2",
                "X-API-Key": "abcd1234efgh",
                "Proxy-Authorization": "Basic short"
            }
        }));
        assert_eq!(
            found,
            [
                (
                    SecretKind::Header,
                    "Authorization".into(),
                    Place::Environment(vec!["TOKEN".into()])
                ),
                (
                    SecretKind::Header,
                    "X-API-Key".into(),
                    Place::Written {
                        instead: Some("${X_API_KEY}".into())
                    }
                ),
                (
                    SecretKind::Header,
                    "Proxy-Authorization".into(),
                    Place::Written {
                        instead: Some("Basic ${AUTH_TOKEN}".into())
                    }
                ),
            ]
        );
    }

    #[test]
    fn a_reference_with_a_secret_default_is_still_typed_in() {
        let found = of(json!({"env": {"TOKEN": "${TOKEN:-sk-abcdefghijklmnop1234}"}}));
        assert!(
            matches!(&found[..], [(SecretKind::Env, n, Place::Written { .. })] if n == "TOKEN"),
            "{found:?}"
        );
        // Variant: a plain default for a secret-sounding variable is the environment's.
        let found = of(json!({"env": {"TOKEN_FILE": "${TOKEN_FILE:-/run/token}"}}));
        assert_eq!(
            found,
            [(
                SecretKind::Env,
                "TOKEN_FILE".into(),
                Place::Environment(vec!["TOKEN_FILE".into()])
            )]
        );
    }

    #[test]
    fn arguments_addresses_and_command_lines_name_where_the_secret_is() {
        let found = of(json!({
            "command": "uvx",
            "args": ["server", "--api-key", "abc123def456", "--verbose", "--token=${T}"],
        }));
        assert_eq!(
            found,
            [(SecretKind::Argument, "--api-key".into(), written())]
        );
        let found = of(json!({
            "type": "sse",
            "url": "https://u:hunter2-pass@mcp.example.com/sse?api_key=abcdef0123456789&x=1"
        }));
        assert_eq!(
            found,
            [
                (SecretKind::Address, "".into(), written()),
                (SecretKind::Address, "api_key".into(), written())
            ]
        );
        let found = of(
            json!({"command": "sh", "headersHelper": "echo Authorization: Bearer ghp_0123456789abcdefghijABCDEFGHIJ012345"}),
        );
        assert_eq!(found, [(SecretKind::HeadersHelper, "".into(), written())]);
        let found = of(
            json!({"type": "http", "url": "https://x.example/", "headersHelper": "get-token --for x"}),
        );
        assert_eq!(
            found,
            [(SecretKind::HeadersHelper, "".into(), Place::Helper)]
        );
    }

    #[test]
    fn oauth_has_a_client_secret_in_the_store_and_a_typed_in_one_is_written() {
        let found = of(
            json!({"type": "http", "url": "https://x.example/", "oauth": {"clientId": "klefki-1"}}),
        );
        assert_eq!(
            found,
            [(SecretKind::ClientSecret, "".into(), Place::Store(None))]
        );
        let found = of(json!({
            "type": "http",
            "url": "https://x.example/",
            "oauth": {"clientId": "klefki-1", "clientSecret": "s3cret-0123456789"}
        }));
        assert_eq!(
            found,
            [
                (SecretKind::ClientSecret, "".into(), Place::Store(None)),
                (SecretKind::Setting, "clientSecret".into(), written())
            ]
        );
    }

    #[test]
    fn the_home_folder_is_shown_as_a_tilde() {
        let home = std::env::temp_dir().join("me");
        let file = home.join(".claude.json");
        assert_eq!(
            short(&file, Some(&home)),
            std::path::Path::new("~")
                .join(".claude.json")
                .display()
                .to_string()
        );
        let elsewhere = std::env::temp_dir().join("cfg").join(".claude.json");
        assert_eq!(
            short(&elsewhere, Some(&home)),
            elsewhere.display().to_string()
        );
        assert_eq!(short(&elsewhere, None), elsewhere.display().to_string());
    }

    #[test]
    fn a_server_without_secrets_has_none_and_names_are_sanitised() {
        assert!(
            of(
                json!({"command": "node", "args": ["server.js"], "env": {"NODE_ENV": "production"}})
            )
            .is_empty()
        );
        let found =
            of(json!({"env": {"TO\u{202e}KEN": "ghp_0123456789abcdefghijABCDEFGHIJ012345"}}));
        assert_eq!(found.len(), 1);
        assert!(!found[0].1.contains('\u{202e}'), "{:?}", found[0].1);
    }

    fn row(scope: Scope, project: Option<&str>, name: &str, oauth: bool) -> crate::api::ServerRow {
        crate::api::ServerRow {
            name: name.into(),
            scope,
            project: project.map(str::to_owned),
            transport: crate::api::TransportKind::Http,
            target: String::new(),
            status: crate::api::Status::Unknown,
            enabled: true,
            disabled_in: 0,
            approval: None,
            shadowed: false,
            shadowed_in: Vec::new(),
            casing_collision: false,
            relative_paths: Vec::new(),
            status_cached: false,
            connection: ConnectionId::new(scope, project, name),
            oauth: oauth.then(|| crate::api::OAuthInfo {
                client_id: Some("c".into()),
                scopes: Vec::new(),
            }),
        }
    }

    fn client_secret_of(id: ServerId) -> ServerSecrets {
        ServerSecrets {
            id,
            file: "~/.claude.json".into(),
            uses: vec![named(SecretKind::ClientSecret, "", Place::Store(None))],
        }
    }

    #[test]
    fn the_store_says_which_client_secrets_are_kept_and_which_nothing_uses() {
        let sso = row(Scope::User, None, "sso", true);
        let other = row(Scope::Local, Some("/w/app"), "other", true);
        let plain = row(Scope::User, None, "plain", false);
        let snapshot = Snapshot {
            servers: vec![sso.clone(), other.clone(), plain.clone()],
            ..Snapshot::default()
        };
        let secrets = Secrets {
            servers: vec![client_secret_of(sso.id()), client_secret_of(other.id())],
            ..Secrets::default()
        };
        let gone = ConnectionId::new(Scope::Local, Some("C:\\w\\old"), "gone");
        let kept = ConnectionId::new(Scope::User, None, "kept");
        let out = with_store(
            secrets.clone(),
            &snapshot,
            Ok(vec![
                sso.connection.clone(),
                gone.clone(),
                kept.clone(),
                plain.connection.clone(),
            ]),
            std::slice::from_ref(&kept),
        );
        assert_eq!(out.servers[0].uses[0].place, Place::Store(Some(true)));
        assert_eq!(
            out.servers.len(),
            1,
            "a server whose only secret would be a client secret it does not have is left out"
        );
        // A kept definition's secret is in use; a server that no longer signs in does not use its old one.
        let unused: Vec<(&str, Option<&str>)> = out
            .unused
            .iter()
            .map(|u| (u.name.as_str(), u.project.as_deref()))
            .collect();
        assert_eq!(unused, [("gone", Some("C:/w/old")), ("plain", None)]);
        assert_eq!(out.store_problem, None);

        let failed = with_store(secrets, &snapshot, Err("locked".into()), &[]);
        assert_eq!(failed.servers[0].uses[0].place, Place::Store(None));
        assert_eq!(failed.servers.len(), 2, "not known is said");
        assert!(failed.unused.is_empty());
        assert_eq!(failed.store_problem.as_deref(), Some("locked"));
    }
}
