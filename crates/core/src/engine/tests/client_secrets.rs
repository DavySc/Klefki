//! Engine tests: client secrets and the Secrets page.
use super::*;

fn oauth_env() -> Env {
    quiet_env(&serde_json::json!({
        "mcpServers": {
            "sso": {"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "klefki-1"}},
            "plain": {"type": "http", "url": "https://plain.example.com/mcp"}
        },
        "projects": {"/work/a": {}}
    }))
}

fn secret_status(core: &Core, id: &ServerId) -> Result<bool, String> {
    collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::SecretStatus { id: i, .. } if i == id))
    })
    .into_iter()
    .rev()
    .find_map(|e| match e {
        Event::SecretStatus { id: i, stored } if i == *id => Some(stored),
        _ => None,
    })
    .unwrap()
}

#[test]
fn a_client_secret_is_stored_read_and_forgotten_under_its_connection() {
    let env = oauth_env();
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let sso = user_id("sso");
    core.send(Command::SecretStatus(sso.clone()));
    assert_eq!(secret_status(&core, &sso), Ok(false));
    core.send(Command::StoreSecret {
        id: sso.clone(),
        secret: crate::secrets::Secret::new("s3cret-value".into()),
    });
    assert_eq!(secret_status(&core, &sso), Ok(true));
    let key = format!(
        "conn:{}",
        crate::model::ConnectionId::new(Scope::User, None, "sso").as_str()
    );
    assert_eq!(
        store.values.lock().unwrap().get(&key).map(String::as_str),
        Some("s3cret-value"),
        "keyed like v1, under dev.klefki"
    );
    core.send(Command::ForgetSecret(sso.clone()));
    assert_eq!(secret_status(&core, &sso), Ok(false));
    assert!(store.values.lock().unwrap().is_empty());
}

#[test]
fn secrets_are_refused_where_they_do_not_belong_and_the_value_never_travels_back() {
    let env = oauth_env();
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    for (id, why) in [(user_id("plain"), "OAuth"), (user_id("ghost"), "no longer")] {
        core.send(Command::StoreSecret {
            id: id.clone(),
            secret: crate::secrets::Secret::new("s3cret-value".into()),
        });
        let err = secret_status(&core, &id).unwrap_err();
        assert!(err.contains(why), "{err}");
    }
    assert!(store.values.lock().unwrap().is_empty());
    // An empty secret is not a secret.
    core.send(Command::StoreSecret {
        id: user_id("sso"),
        secret: crate::secrets::Secret::new(String::new()),
    });
    assert!(
        secret_status(&core, &user_id("sso"))
            .unwrap_err()
            .contains("empty")
    );
    core.send(Command::StoreSecret {
        id: user_id("sso"),
        secret: crate::secrets::Secret::new("s3cret-value".into()),
    });
    assert_eq!(secret_status(&core, &user_id("sso")), Ok(true));
    let everything = format!("{:?}", events_for(&core, Duration::from_millis(200)));
    assert!(!everything.contains("s3cret"), "{everything}");
}

#[test]
fn without_a_secret_store_nothing_is_kept_anywhere() {
    let env = oauth_env();
    let store = Arc::new(crate::secrets::MemoryStore::default());
    *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable(
        "no Secret Service".into(),
    ));
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    core.send(Command::StoreSecret {
        id: user_id("sso"),
        secret: crate::secrets::Secret::new("s3cret-value".into()),
    });
    let err = secret_status(&core, &user_id("sso")).unwrap_err();
    assert!(err.contains("no plaintext copy"), "{err}");
    // Not written next to the configuration either.
    let dump = walk(env.dir.path());
    assert!(!dump.contains("s3cret"), "{dump}");
}

fn secrets_env() -> Env {
    quiet_env(&serde_json::json!({
        "mcpServers": {
            "sso": {"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "klefki-1"}},
            "gh": {"command": "npx", "env": {"GITHUB_TOKEN": "ghp_0123456789abcdefghijABCDEFGHIJ012345", "LOG_LEVEL": "debug"}},
            "ref": {"type": "http", "url": "https://ref.example.com/", "headers": {"Authorization": "Bearer ${REF_TOKEN}"}},
            "plain": {"command": "node", "args": ["server.js"]}
        },
        "projects": {"/work/a": {"mcpServers": {"sso2": {"type": "http", "url": "https://sso2.example.com/", "oauth": {"clientId": "c2"}}}}}
    }))
}

#[test]
fn the_secrets_page_says_where_each_secret_lives_without_any_value() {
    use crate::secret_uses::{Place, SecretKind};
    let env = secrets_env();
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let gone = crate::model::ConnectionId::new(Scope::User, None, "gone");
    for (id, value) in [
        (user_id("sso").connection, "s3cret-one"),
        (gone.clone(), "s3cret-two"),
    ] {
        store
            .set(&id, &crate::secrets::Secret::new(value.into()))
            .unwrap();
    }
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let report = secrets_report(&core);
    let places: Vec<(String, SecretKind, String, Place)> = report
        .servers
        .iter()
        .flat_map(|s| {
            s.uses
                .iter()
                .map(|u| (s.id.name.clone(), u.kind, u.name.clone(), u.place.clone()))
        })
        .collect();
    assert_eq!(
        places,
        [
            // Typed in first.
            (
                "gh".into(),
                SecretKind::Env,
                "GITHUB_TOKEN".into(),
                Place::Written {
                    instead: Some("${GITHUB_TOKEN}".into())
                }
            ),
            (
                "sso".into(),
                SecretKind::ClientSecret,
                "".into(),
                Place::Store(Some(true))
            ),
            (
                "ref".into(),
                SecretKind::Header,
                "Authorization".into(),
                Place::Environment(vec!["REF_TOKEN".into()])
            ),
        ]
    );
    assert!(
        report.servers[0].file.ends_with(".claude.json"),
        "{:?}",
        report.servers[0]
    );
    assert_eq!(report.written(), 1);
    let unused: Vec<&str> = report.unused.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(unused, ["gone"]);
    assert_eq!(report.store_problem, None);
    let everything = format!("{report:?}");
    for value in ["s3cret", "ghp_0123"] {
        assert!(!everything.contains(value), "{everything}");
    }
}

#[test]
fn unused_means_unused_in_this_configuration_and_the_report_names_it() {
    // Cross-review B1: one store, two Claude Code configurations (CLAUDE_CONFIG_DIR). The second one cannot
    // see the first's servers, so the page must say which configuration it looked at.
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let sso = user_id("sso").connection;
    store
        .set(&sso, &crate::secrets::Secret::new("s3cret".into()))
        .unwrap();
    let second = quiet_env(&serde_json::json!({"mcpServers": {"plain": {"command": "node"}}}));
    let core = with_store(&second, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let report = secrets_report(&core);
    let unused: Vec<&str> = report.unused.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(unused, ["sso"]);
    let config = second.dir.path().join("cfg").join(".claude.json");
    assert_eq!(
        report.config,
        crate::sanitize::shown(&config.display().to_string())
    );
}

#[test]
fn a_file_change_reads_the_definitions_again_but_not_the_store() {
    // Cross-review N1/N2: listing the store can ask to unlock it, and Claude Code saves often.
    let env = secrets_env();
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    assert_eq!(secrets_report(&core).store_problem, None);
    *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable("locked".into()));
    let mut config = config_json(&env);
    config["mcpServers"]["new"] =
        serde_json::json!({"command": "x", "env": {"API_TOKEN": "${API_TOKEN}"}});
    write_config(&env, &config);
    core.send(Command::Refresh { live_all: false });
    collect(&core, |e| !snapshots(e).is_empty());
    core.send(Command::SecretsAfterFileChange);
    let again = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))))
        .into_iter()
        .find_map(|e| match e {
            Event::Secrets(s) => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(again.store_problem, None, "the store was not asked again");
    assert!(
        again.servers.iter().any(|s| s.id.name == "new"),
        "{again:?}"
    );
    // Asked on purpose (opening the page, Try again), it is.
    assert!(secrets_report(&core).store_problem.is_some());
}

#[test]
fn an_unused_client_secret_is_forgotten_only_while_nothing_uses_it() {
    let env = secrets_env();
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let gone = crate::model::ConnectionId::new(Scope::User, None, "gone");
    let sso = user_id("sso").connection;
    for id in [&gone, &sso] {
        store
            .set(id, &crate::secrets::Secret::new("s3cret".into()))
            .unwrap();
    }
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let forgotten = |core: &Core, id: &crate::model::ConnectionId| {
        core.send(Command::ForgetUnusedSecret(id.clone()));
        let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))));
        let result = events
            .iter()
            .find_map(|e| match e {
                Event::UnusedSecretForgotten { connection, result } if connection == id => {
                    Some(result.clone())
                }
                _ => None,
            })
            .expect("an answer before the new list");
        let report = events
            .into_iter()
            .find_map(|e| match e {
                Event::Secrets(s) => Some(s),
                _ => None,
            })
            .unwrap();
        (result, report)
    };
    let (result, report) = forgotten(&core, &gone);
    assert_eq!(result, Ok(()));
    assert!(report.unused.is_empty(), "{report:?}");
    assert!(store.get(&gone).unwrap().is_none());
    // A server's own client secret is not Forget's to take, whatever the page showed.
    let (result, _) = forgotten(&core, &sso);
    assert!(result.unwrap_err().contains("uses"));
    assert!(store.get(&sso).unwrap().is_some());
}

#[test]
fn a_store_that_cannot_be_used_is_said_only_when_a_server_needs_it() {
    let store = Arc::new(crate::secrets::MemoryStore::default());
    *store.fail.lock().unwrap() = Some(crate::secrets::SecretError::Unavailable(
        "no Secret Service".into(),
    ));
    let env = secrets_env();
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let report = secrets_report(&core);
    assert!(
        report
            .store_problem
            .as_deref()
            .is_some_and(|p| p.contains("no Secret Service")),
        "{report:?}"
    );
    assert!(
        report
            .servers
            .iter()
            .flat_map(|s| &s.uses)
            .any(|u| u.place == crate::secret_uses::Place::Store(None))
    );
    // Variant: nothing signs in with OAuth, so the store does not matter here.
    let env = quiet_env(&serde_json::json!({
        "mcpServers": {"gh": {"command": "npx", "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}}}
    }));
    let core = with_store(&env, store);
    collect(&core, |e| !snapshots(e).is_empty());
    let report = secrets_report(&core);
    assert_eq!(report.store_problem, None);
    assert_eq!(report.servers.len(), 1);
}
