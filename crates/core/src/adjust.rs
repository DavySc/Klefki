//! What a copy changes from its original ([`crate::api::Change::Duplicate`]): where its addresses start, and for a
//! server that signs in with OAuth, the callback port and the client. Many servers differ from one another only
//! there, so a copy can be made and adjusted in one step.
use crate::sanitize::sanitize;
use crate::secrets::Secret;
use serde_json::Value;

/// The changes a copy makes to the original's definition. The default changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adjust {
    /// Where the addresses start: each of the server's addresses (`url`, and the sign-in metadata address) that
    /// starts with `from` starts with `to` instead. `from` is what [`shared_base`] gave.
    pub base: Option<Rebase>,
    /// A fixed port for the sign-in to come back to; `Some(None)` lets Claude Code choose one each time.
    pub callback_port: Option<Option<u16>>,
    /// Another OAuth client, with its own client secret. Without one the copy keeps the original's client and
    /// secret.
    pub client: Option<Client>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebase {
    pub from: String,
    pub to: String,
}

/// An OAuth client the copy signs in as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// `None`: no client id, so Claude Code registers itself with the server.
    pub id: Option<String>,
    /// The client's secret, if it has one. Never written in a definition: Claude Code is handed it on its own.
    pub secret: Option<Secret>,
}

impl Adjust {
    pub fn is_empty(&self) -> bool {
        self.base.is_none() && self.callback_port.is_none() && self.client.is_none()
    }

    /// Changes only the sign-in: what the server's client and port are.
    pub(crate) fn touches_sign_in(&self) -> bool {
        self.callback_port.is_some() || self.client.is_some()
    }

    /// `entry` with these changes, or why they do not fit it. `name` names the server in what is said.
    pub(crate) fn apply(&self, entry: &Value, name: &str) -> Result<Value, String> {
        let mut out = entry.clone();
        let name = sanitize(name);
        if let Some(Rebase { from, to }) = &self.base {
            let url = out
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{name} has no address to change"))?;
            let rest = rest_after(url, from).ok_or_else(|| {
                format!("the address of {name} has changed since; review the copy again")
            })?;
            if let Some(why) = base_problem(to, rest.is_empty()) {
                return Err(why.to_owned());
            }
            out["url"] = Value::String(format!("{to}{rest}"));
            // Only an address on the same host and port: `https://acme.com` does not start
            // `https://acme.com.example` or `https://acme.com:9443`.
            if let Some(metadata) = out.pointer_mut("/oauth/authServerMetadataUrl")
                && let Some(rest) = metadata.as_str().and_then(|m| rest_after(m, from))
            {
                *metadata = Value::String(format!("{to}{rest}"));
            }
        }
        if !self.touches_sign_in() {
            return Ok(out);
        }
        let oauth = out
            .get_mut("oauth")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("{name} does not sign in with OAuth"))?;
        match self.callback_port {
            Some(Some(port)) => {
                oauth.insert("callbackPort".into(), port.into());
            }
            Some(None) => {
                oauth.remove("callbackPort");
            }
            None => {}
        }
        if let Some(why) = self.client_problem() {
            return Err(why.to_owned());
        }
        if let Some(client) = &self.client {
            match &client.id {
                Some(id) => {
                    oauth.insert("clientId".into(), id.as_str().into());
                }
                None => {
                    oauth.remove("clientId");
                }
            }
        }
        Ok(out)
    }

    /// What is wrong with the client given, if anything.
    pub(crate) fn client_problem(&self) -> Option<&'static str> {
        let client = self.client.as_ref()?;
        if client.id.is_none() && client.secret.is_some() {
            return Some("a client secret needs the client id it belongs to");
        }
        if client.secret.as_ref().is_some_and(Secret::is_empty) {
            return Some("the client secret is empty; leave it out if the client has none");
        }
        None
    }
}

/// Whether `s` has a part end at `i`: its end, or a `/`, `?` or `#` there.
fn ends_part(s: &str, i: usize) -> bool {
    i == s.len() || s[i..].starts_with(['/', '?', '#'])
}

/// What follows `from` in `address`, when `from` starts it and ends where a part of it does.
fn rest_after<'a>(address: &'a str, from: &str) -> Option<&'a str> {
    let rest = address.strip_prefix(from)?;
    ends_part(address, from.len()).then_some(rest)
}

/// Why `to` cannot replace a base followed by more of the address (`whole` is false), if it cannot: the rest starts
/// with its own `/`, `?` or `#`, so a base ending in `/`, or holding a query or fragment, would make an address that
/// is not the one meant (`…/v2//mcp`, `…/v2?x=1/mcp`).
pub(crate) fn base_problem(to: &str, whole: bool) -> Option<&'static str> {
    if whole {
        return None;
    }
    if to.contains(['?', '#']) {
        return Some("the base address ends before any ? or #");
    }
    if to.ends_with('/') {
        return Some("the base address ends without a /: the rest of each address starts with one");
    }
    None
}

/// The start the addresses `url` and `metadata` (a sign-in metadata address) share, so both can be changed in one
/// go: as far as both run the same up to a `/` (or the end), and never less than the scheme and host. Without
/// `metadata`, or when it is on another host, that is `url` itself.
pub fn shared_base<'a>(url: &'a str, metadata: Option<&str>) -> &'a str {
    let Some(metadata) = metadata else {
        return url;
    };
    // Where the host ends: after `scheme://`, at the first `/`, `?` or `#`.
    let Some(after_scheme) = url.find("://").map(|i| i + 3) else {
        return url;
    };
    let host_end = url[after_scheme..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| after_scheme + i);
    let common = url
        .char_indices()
        .zip(metadata.chars())
        .take_while(|((_, a), b)| a == b)
        .last()
        .map_or(0, |((i, a), _)| i + a.len_utf8());
    (host_end..=common)
        .rev()
        .find(|&i| url.is_char_boundary(i) && ends_part(url, i) && ends_part(metadata, i))
        .map_or(url, |end| &url[..end])
}

/// The Duplicate dialog's address and sign-in fields: the original's values, and what is typed over them. Built from
/// the definition [`crate::api::Event::Entry`] sends, so the address is the one in the file, not the one shown in the
/// list (which leaves out a query string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFields {
    url: String,
    metadata: Option<String>,
    /// Where the addresses start: as the original has it, and as typed.
    from_base: String,
    pub base: String,
    /// The original signs in with OAuth: the client and port can be changed.
    pub oauth: bool,
    client_id_was: String,
    pub client_id: String,
    port_was: String,
    pub callback_port: String,
    /// The new client's secret; only used when the client id is changed.
    pub secret: Secret,
}

impl CopyFields {
    /// The fields for a copy of the definition `json`, or `None` when it has no address to change (a program) or
    /// cannot be read.
    pub fn from_definition(json: &str) -> Option<Self> {
        let entry: Value = serde_json::from_str(json).ok()?;
        if !matches!(
            entry.get("type").and_then(Value::as_str),
            Some("http" | "sse")
        ) {
            return None;
        }
        let url = entry.get("url")?.as_str()?.to_owned();
        let oauth = entry.get("oauth").and_then(Value::as_object);
        let text = |key: &str| oauth.and_then(|o| o.get(key)).and_then(Value::as_str);
        let metadata = text("authServerMetadataUrl").map(str::to_owned);
        let from_base = shared_base(&url, metadata.as_deref()).to_owned();
        let client_id = text("clientId").unwrap_or_default().to_owned();
        let port = oauth
            .and_then(|o| o.get("callbackPort"))
            .and_then(Value::as_u64)
            .map(|p| p.to_string())
            .unwrap_or_default();
        Some(Self {
            base: from_base.clone(),
            from_base,
            metadata,
            url,
            oauth: oauth.is_some(),
            client_id_was: client_id.clone(),
            client_id,
            port_was: port.clone(),
            callback_port: port,
            secret: Secret::new(String::new()),
        })
    }

    /// Puts `https://` before a base typed without a scheme (`eu.acme.com/v1`), as a browser does.
    /// A trailing `/` is dropped when more of the address follows the base, which starts with its own.
    pub fn complete_base(&mut self) {
        let base = self.base.trim();
        if !base.is_empty() && !base.contains("://") && !base.contains(char::is_whitespace) {
            self.base = format!("https://{base}");
        }
        if !self.base_is_whole_address() {
            let base = self.base.trim();
            let kept = base.trim_end_matches('/');
            if kept.contains("://") && !kept.ends_with(':') && kept.len() < base.len() {
                self.base = kept.to_owned();
            }
        }
    }

    /// The base field covers the whole address (there is no other address that shares it).
    pub fn base_is_whole_address(&self) -> bool {
        self.from_base == self.url
    }

    /// The addresses the copy gets: its own, and its sign-in metadata address (when it has one) with whether the base
    /// changes it.
    pub fn addresses(&self) -> (String, Option<(String, bool)>) {
        let base = self.base.trim();
        let moved = |address: &str| {
            rest_after(address, &self.from_base).map(|rest| format!("{base}{rest}"))
        };
        let url = moved(&self.url).unwrap_or_else(|| self.url.clone());
        let metadata = self.metadata.as_ref().map(|m| match moved(m) {
            Some(new) => (new, true),
            None => (m.clone(), false),
        });
        (url, metadata)
    }

    /// The client id as typed differs from the original's: the copy signs in as another client, and the client
    /// secret field is for that client.
    pub fn client_changed(&self) -> bool {
        self.oauth && self.client_id.trim() != self.client_id_was
    }

    /// The callback port as typed, when it is a usable one.
    pub fn typed_port(&self) -> Option<u16> {
        self.callback_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
    }

    /// What is wrong with what is typed, by the part it is about, as the Add dialog says it.
    pub fn problems(&self) -> Vec<(crate::form::Part, String)> {
        use crate::form::{Part, ServerFields};
        let (url, metadata) = self.addresses();
        let fields = ServerFields {
            transport: crate::api::TransportKind::Http,
            url,
            oauth: self.oauth,
            client_id: self.client_id.clone(),
            callback_port: self.callback_port.clone(),
            metadata_url: metadata.map(|(m, _)| m).unwrap_or_default(),
            ..ServerFields::default()
        };
        let mut problems: Vec<(Part, String)> = fields
            .problems()
            .into_iter()
            .filter(|(part, _)| *part != Part::Definition)
            .collect();
        if let Some(why) = base_problem(self.base.trim(), self.base_is_whole_address()) {
            problems.retain(|(part, _)| !matches!(part, Part::Address | Part::MetadataUrl));
            problems.insert(0, (Part::Address, crate::form::sentence(why)));
        }
        if self.base.trim().is_empty() {
            // Nor is the metadata address that follows from it: one thing to fix, said once.
            problems.retain(|(part, _)| !matches!(part, Part::Address | Part::MetadataUrl));
            problems.insert(0, (Part::Address, "Type the address.".to_owned()));
        }
        problems
    }

    /// What the copy changes, when nothing typed is wrong. The client secret is taken along (and left empty here)
    /// only with another client.
    pub fn adjust(&mut self) -> Option<Adjust> {
        if !self.problems().is_empty() {
            return None;
        }
        let base = self.base.trim();
        let port = self.callback_port.trim();
        let id = self.client_id.trim();
        let client_changed = self.client_changed();
        Some(Adjust {
            base: (base != self.from_base).then(|| Rebase {
                from: self.from_base.clone(),
                to: base.to_owned(),
            }),
            callback_port: (self.oauth && port != self.port_was).then(|| self.typed_port()),
            client: client_changed.then(|| Client {
                id: (!id.is_empty()).then(|| id.to_owned()),
                secret: Some(std::mem::replace(
                    &mut self.secret,
                    Secret::new(String::new()),
                ))
                .filter(|s| !s.is_empty()),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::Part;
    use serde_json::json;

    const SSO: &str = r#"{"type": "http", "url": "https://mcp.acme.com/v1/mcp?tenant=7", "headers": {"X": "<hidden>"},
        "oauth": {"clientId": "acme-1", "callbackPort": 8123, "authServerMetadataUrl": "https://mcp.acme.com/v1/.well-known/oauth-authorization-server"}}"#;

    #[test]
    fn the_copy_fields_start_as_the_original_and_change_nothing_untouched() {
        let mut fields = CopyFields::from_definition(SSO).unwrap();
        assert_eq!(fields.base, "https://mcp.acme.com/v1");
        assert!(!fields.base_is_whole_address());
        assert_eq!(fields.client_id, "acme-1");
        assert_eq!(fields.callback_port, "8123");
        assert!(fields.problems().is_empty());
        assert_eq!(fields.adjust(), Some(Adjust::default()));
        // A program has no address; a plain address is the whole base.
        assert_eq!(CopyFields::from_definition(r#"{"command": "node"}"#), None);
        assert_eq!(CopyFields::from_definition("not json"), None);
        let plain =
            CopyFields::from_definition(r#"{"type": "sse", "url": "https://h.example/sse"}"#)
                .unwrap();
        assert!(plain.base_is_whole_address() && !plain.oauth);
    }

    #[test]
    fn the_copy_fields_make_the_adjustment_typed() {
        let mut fields = CopyFields::from_definition(SSO).unwrap();
        fields.base = " https://eu.acme.com/v2 ".into();
        fields.callback_port = "9001".into();
        fields.client_id = "acme-2".into();
        fields.secret.edit().push_str("s2");
        let (url, metadata) = fields.addresses();
        // The query stays: it is in the file, though the list does not show it.
        assert_eq!(url, "https://eu.acme.com/v2/mcp?tenant=7");
        assert_eq!(
            metadata,
            Some((
                "https://eu.acme.com/v2/.well-known/oauth-authorization-server".into(),
                true
            ))
        );
        assert!(fields.client_changed());
        let adjust = fields.adjust().unwrap();
        assert_eq!(
            adjust.base,
            Some(Rebase {
                from: "https://mcp.acme.com/v1".into(),
                to: "https://eu.acme.com/v2".into()
            })
        );
        assert_eq!(adjust.callback_port, Some(Some(9001)));
        assert_eq!(
            adjust.client,
            Some(Client {
                id: Some("acme-2".into()),
                secret: Some(Secret::new("s2".into()))
            })
        );
        // A trailing slash is dropped when left; one typed, or a query or fragment, is said to be wrong.
        fields.base = "https://eu.acme.com/v2/".into();
        let parts: Vec<Part> = fields.problems().into_iter().map(|(p, _)| p).collect();
        assert_eq!(parts, [Part::Address]);
        fields.complete_base();
        assert_eq!(fields.base, "https://eu.acme.com/v2");
        for base in ["https://eu.acme.com/v2?x=1", "https://eu.acme.com#f"] {
            fields.base = base.into();
            fields.complete_base();
            let problems = fields.problems();
            assert!(
                problems
                    .iter()
                    .any(|(p, why)| *p == Part::Address && why.contains("? or #")),
                "{base}: {problems:?}"
            );
        }
        // Typed without its scheme, it gets one when left.
        fields.base = "eu.acme.com".into();
        fields.complete_base();
        assert_eq!(fields.base, "https://eu.acme.com");
        // Taken out of the field, not copied.
        assert!(fields.secret.is_empty());

        // Emptied: no fixed port, no client id (Claude Code registers itself), and no secret with it.
        let mut fields = CopyFields::from_definition(SSO).unwrap();
        fields.callback_port = " ".into();
        fields.client_id.clear();
        let adjust = fields.adjust().unwrap();
        assert_eq!(adjust.callback_port, Some(None));
        assert_eq!(
            adjust.client,
            Some(Client {
                id: None,
                secret: None
            })
        );

        // The same client: its secret field is not used, so the original's secret goes with the copy.
        let mut fields = CopyFields::from_definition(SSO).unwrap();
        fields.secret.edit().push_str("typed");
        assert_eq!(fields.adjust().unwrap().client, None);
    }

    #[test]
    fn the_copy_fields_say_what_is_wrong_where_it_is() {
        let mut fields = CopyFields::from_definition(SSO).unwrap();
        fields.base = String::new();
        fields.callback_port = "70000".into();
        fields.client_id = "a\u{7}b".into();
        let parts: Vec<Part> = fields.problems().into_iter().map(|(p, _)| p).collect();
        assert_eq!(parts, [Part::Address, Part::ClientId, Part::CallbackPort]);
        assert_eq!(fields.adjust(), None);
        let mut fields = CopyFields::from_definition(SSO).unwrap();
        fields.base = "http://eu.acme.com".into();
        // The address may be http (a local server), the sign-in metadata only https.
        let parts: Vec<Part> = fields.problems().into_iter().map(|(p, _)| p).collect();
        assert_eq!(parts, [Part::MetadataUrl]);
    }

    #[test]
    fn the_shared_base_stops_where_the_addresses_part_and_keeps_the_host() {
        let cases = [
            // Same host, the metadata under the server's own path.
            (
                "https://mcp.acme.com/v1/mcp",
                Some("https://mcp.acme.com/v1/.well-known/oauth-authorization-server"),
                "https://mcp.acme.com/v1",
            ),
            // Same host only.
            (
                "https://mcp.acme.com/mcp",
                Some("https://mcp.acme.com/.well-known/oauth-authorization-server"),
                "https://mcp.acme.com",
            ),
            // A host that starts like the other is another host.
            (
                "https://api.acme.com/mcp",
                Some("https://api.acme.community/.well-known/x"),
                "https://api.acme.com/mcp",
            ),
            // Another host: only the server's address.
            (
                "https://mcp.acme.com/mcp",
                Some("https://login.acme.com/.well-known/x"),
                "https://mcp.acme.com/mcp",
            ),
            // A path segment that starts like the other is another segment.
            (
                "https://h.example/team-a/mcp",
                Some("https://h.example/team-ab/meta"),
                "https://h.example",
            ),
            // The metadata under the whole address.
            (
                "https://h.example/mcp",
                Some("https://h.example/mcp/.well-known/x"),
                "https://h.example/mcp",
            ),
            // A port is part of the host.
            (
                "https://h.example:8443/mcp",
                Some("https://h.example:8443/meta"),
                "https://h.example:8443",
            ),
            ("https://h.example/mcp", None, "https://h.example/mcp"),
            (
                "not an address",
                Some("not an address either"),
                "not an address",
            ),
            (
                "https://hé.example/ü/mcp",
                Some("https://hé.example/ü/m"),
                "https://hé.example/ü",
            ),
        ];
        for (url, metadata, base) in cases {
            assert_eq!(shared_base(url, metadata), base, "{url} {metadata:?}");
        }
    }

    fn oauth_entry() -> Value {
        json!({
            "type": "http",
            "url": "https://mcp.acme.com/v1/mcp",
            "headers": {"X-Team": "a"},
            "oauth": {
                "clientId": "acme-1",
                "callbackPort": 8123,
                "scopes": "read",
                "authServerMetadataUrl": "https://mcp.acme.com/v1/.well-known/oauth-authorization-server"
            }
        })
    }

    #[test]
    fn a_new_base_changes_every_address_that_starts_with_it_and_nothing_else() {
        let adjust = Adjust {
            base: Some(Rebase {
                from: "https://mcp.acme.com/v1".into(),
                to: "https://eu.acme.com/v2".into(),
            }),
            ..Adjust::default()
        };
        let mut expected = oauth_entry();
        expected["url"] = json!("https://eu.acme.com/v2/mcp");
        expected["oauth"]["authServerMetadataUrl"] =
            json!("https://eu.acme.com/v2/.well-known/oauth-authorization-server");
        assert_eq!(adjust.apply(&oauth_entry(), "acme").unwrap(), expected);
        // The metadata on another host stays where it is.
        let mut elsewhere = oauth_entry();
        elsewhere["oauth"]["authServerMetadataUrl"] = json!("https://login.acme.com/meta");
        let adjusted = adjust.apply(&elsewhere, "acme").unwrap();
        assert_eq!(adjusted["url"], "https://eu.acme.com/v2/mcp");
        assert_eq!(
            adjusted["oauth"]["authServerMetadataUrl"],
            "https://login.acme.com/meta"
        );
        // An address that no longer starts that way is not guessed at.
        let mut moved = oauth_entry();
        moved["url"] = json!("https://other.example/mcp");
        let refused = adjust.apply(&moved, "acme").unwrap_err();
        assert!(refused.contains("changed since"), "{refused}");
    }

    #[test]
    fn a_base_ending_in_a_slash_or_holding_a_query_is_refused_where_more_follows() {
        // Cross-review of 85b2db7: `…/v2/` made `…/v2//mcp`, and `…/v2?x=1` made `…/v2?x=1/mcp`.
        let entry = oauth_entry();
        for to in [
            "https://eu.acme.com/v2/",
            "https://eu.acme.com/v2?x=1",
            "https://eu.acme.com/v2#f",
        ] {
            let adjust = Adjust {
                base: Some(Rebase {
                    from: "https://mcp.acme.com/v1".into(),
                    to: to.into(),
                }),
                ..Adjust::default()
            };
            let refused = adjust.apply(&entry, "acme").unwrap_err();
            assert!(refused.contains("base address ends"), "{to}: {refused}");
        }
        // The whole address may end in a slash or carry a query: nothing follows it.
        let plain = json!({"type": "http", "url": "https://h.example/mcp"});
        let whole = Adjust {
            base: Some(Rebase {
                from: "https://h.example/mcp".into(),
                to: "https://h.example/mcp/?team=7".into(),
            }),
            ..Adjust::default()
        };
        assert_eq!(
            whole.apply(&plain, "plain").unwrap()["url"],
            "https://h.example/mcp/?team=7"
        );
    }

    #[test]
    fn an_address_on_another_host_or_port_is_not_rebased_by_a_base_it_merely_starts_with() {
        // Cross-review of 85b2db7: with an address without a path, `https://acme.com` was taken as the start of
        // `https://acme.com.evil.example/…` and `https://acme.com:9443/…`.
        for elsewhere in [
            "https://acme.com.evil.example/meta",
            "https://acme.com:9443/meta",
            "https://acme.community/meta",
        ] {
            let entry = json!({"type": "http", "url": "https://acme.com", "oauth": {"authServerMetadataUrl": elsewhere}});
            let adjust = Adjust {
                base: Some(Rebase {
                    from: "https://acme.com".into(),
                    to: "https://eu.acme.com".into(),
                }),
                ..Adjust::default()
            };
            let adjusted = adjust.apply(&entry, "acme").unwrap();
            assert_eq!(adjusted["url"], "https://eu.acme.com");
            assert_eq!(adjusted["oauth"]["authServerMetadataUrl"], elsewhere);
            // The dialog says the same.
            let mut fields = CopyFields::from_definition(&entry.to_string()).unwrap();
            fields.base = "https://eu.acme.com".into();
            assert_eq!(fields.addresses().1, Some((elsewhere.to_owned(), false)));
        }
        // And a server address that only starts like the base is not this server's.
        let entry = json!({"type": "http", "url": "https://acme.com.evil.example/mcp"});
        let adjust = Adjust {
            base: Some(Rebase {
                from: "https://acme.com".into(),
                to: "https://eu.acme.com".into(),
            }),
            ..Adjust::default()
        };
        assert!(adjust.apply(&entry, "acme").is_err());
    }

    #[test]
    fn the_port_and_client_are_set_or_taken_out() {
        let adjust = Adjust {
            callback_port: Some(Some(9001)),
            client: Some(Client {
                id: Some("acme-2".into()),
                secret: Some(Secret::new("s2".into())),
            }),
            ..Adjust::default()
        };
        let adjusted = adjust.apply(&oauth_entry(), "acme").unwrap();
        assert_eq!(adjusted["oauth"]["callbackPort"], 9001);
        assert_eq!(adjusted["oauth"]["clientId"], "acme-2");
        assert!(!adjusted.to_string().contains("s2"), "{adjusted}");
        // Everything else as it was.
        assert_eq!(adjusted["oauth"]["scopes"], "read");
        assert_eq!(adjusted["headers"], oauth_entry()["headers"]);

        let without = Adjust {
            callback_port: Some(None),
            client: Some(Client {
                id: None,
                secret: None,
            }),
            ..Adjust::default()
        };
        let adjusted = without.apply(&oauth_entry(), "acme").unwrap();
        assert!(adjusted["oauth"].get("callbackPort").is_none());
        assert!(adjusted["oauth"].get("clientId").is_none());
        assert_eq!(adjusted["oauth"]["scopes"], "read");
    }

    #[test]
    fn sign_in_changes_need_a_server_that_signs_in_and_a_secret_its_client() {
        let plain = json!({"type": "http", "url": "https://h.example/mcp"});
        let port = Adjust {
            callback_port: Some(Some(9001)),
            ..Adjust::default()
        };
        let refused = port.apply(&plain, "plain").unwrap_err();
        assert!(refused.contains("does not sign in"), "{refused}");
        let orphan = Adjust {
            client: Some(Client {
                id: None,
                secret: Some(Secret::new("s".into())),
            }),
            ..Adjust::default()
        };
        assert!(orphan.apply(&oauth_entry(), "acme").is_err());
        let empty = Adjust {
            client: Some(Client {
                id: Some("c".into()),
                secret: Some(Secret::new(String::new())),
            }),
            ..Adjust::default()
        };
        assert!(empty.apply(&oauth_entry(), "acme").is_err());
        assert!(Adjust::default().is_empty());
        assert_eq!(Adjust::default().apply(&plain, "plain").unwrap(), plain);
    }
}
