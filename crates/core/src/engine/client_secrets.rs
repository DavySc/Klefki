//! Client secrets in the system's store: one server's, and the list the Secrets page shows.
use super::{Event, Shared, State, lock};
use crate::api::{ServerId, Snapshot};
use crate::sanitize::sanitize;
use crate::secrets::{Secret, SecretStore};
use crate::swap;
use std::sync::Arc;

pub(super) enum SecretOp {
    Status,
    Store(Secret),
    Forget,
}

/// Looks up, stores or forgets a server's client secret, on a thread of its own: a store may be slow, or ask the
/// user (macOS shows a keychain prompt). Only servers that sign in with OAuth have one.
pub(super) fn secret(shared: &Shared, snapshot: &Snapshot, id: ServerId, op: SecretOp) {
    let answer = |stored: Result<bool, String>| {
        shared.emitter.emit(Event::SecretStatus {
            id: id.clone(),
            stored,
        });
    };
    let Some(row) = snapshot.servers.iter().find(|r| r.is(&id)) else {
        return answer(Err(format!(
            "{} is no longer in the configuration.",
            sanitize(&id.name)
        )));
    };
    if row.oauth.is_none() {
        return answer(Err(format!(
            "{} does not sign in with OAuth, so it has no client secret.",
            sanitize(&id.name)
        )));
    }
    if matches!(&op, SecretOp::Store(s) if s.is_empty()) {
        return answer(Err("The client secret is empty.".to_owned()));
    }
    let store = match secret_store(shared) {
        Ok(store) => store,
        Err(e) => return answer(Err(e.to_string())),
    };
    let connection = row.connection.clone();
    let emitter = shared.emitter.clone();
    std::thread::spawn(move || {
        let stored = match op {
            SecretOp::Status => store.get(&connection).map(|s| s.is_some()),
            SecretOp::Store(value) => store.set(&connection, &value).map(|()| true),
            SecretOp::Forget => store.delete(&connection).map(|_| false),
        };
        emitter.emit(Event::SecretStatus {
            id,
            stored: stored.map_err(|e| e.to_string()),
        });
    });
}

/// How the Secrets page asks about the system's store.
pub(super) enum SecretsAsk {
    /// List it again.
    List,
    /// Use its last listing, if there is one.
    Reuse,
    /// Delete this unused client secret, if nothing uses it, then list the store again.
    Forget(crate::model::ConnectionId),
}

pub(super) fn secrets(shared: &Arc<Shared>, state: &State, ask: SecretsAsk) {
    let report = crate::secret_uses::read(
        shared.config.paths.as_ref(),
        shared.config.home.as_deref(),
        &state.snapshot,
        &state.keys,
    );
    let snapshot = Arc::clone(&state.snapshot);
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        // Definitions Klefki keeps to put back still use their client secret.
        let kept: Vec<crate::model::ConnectionId> = shared
            .config
            .backup_dir
            .as_deref()
            .map(swap::Recovery::files)
            .unwrap_or_default()
            .iter()
            .flat_map(swap::Recovery::connections)
            .collect();
        let reused = match ask {
            SecretsAsk::Reuse => lock(&shared.secrets_listed).clone(),
            _ => None,
        };
        let listed = match reused {
            Some(listed) => listed,
            None => list_secrets(&shared, &snapshot, &kept, ask),
        };
        shared
            .emitter
            .emit(Event::Secrets(crate::secret_uses::with_store(
                report, &snapshot, listed, &kept,
            )));
    });
}

/// The connections the system's store keeps a client secret for, or why it could not say; after deleting `ask`'s
/// unused secret. Remembered for [`SecretsAsk::Reuse`].
fn list_secrets(
    shared: &Shared,
    snapshot: &Snapshot,
    kept: &[crate::model::ConnectionId],
    ask: SecretsAsk,
) -> Result<Vec<crate::model::ConnectionId>, String> {
    let store = secret_store(shared);
    if let SecretsAsk::Forget(connection) = ask {
        let used = snapshot
            .servers
            .iter()
            .any(|r| r.connection == connection && r.oauth.is_some())
            || kept.contains(&connection);
        let result = if used {
            Err("A server uses this client secret now, so it was kept.".to_owned())
        } else {
            match &store {
                Ok(store) => store
                    .delete(&connection)
                    .map(|_| ())
                    .map_err(|e| sanitize(&e.to_string())),
                Err(e) => Err(sanitize(&e.to_string())),
            }
        };
        shared
            .emitter
            .emit(Event::UnusedSecretForgotten { connection, result });
    }
    let listed = match &store {
        Ok(store) => store.list().map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    // A store that cannot be opened matters only when a server signs in with OAuth.
    let signs_in = snapshot.servers.iter().any(|r| r.oauth.is_some());
    let listed = match listed {
        Err(_) if !signs_in => Ok(Vec::new()),
        listed => listed,
    };
    *lock(&shared.secrets_listed) = Some(listed.clone());
    listed
}

/// The store client secrets are kept in: the one tests give, else the system's own, opened the first time it is
/// needed and kept.
pub(super) fn secret_store(
    shared: &Shared,
) -> Result<Arc<dyn SecretStore>, crate::secrets::SecretError> {
    let mut held = lock(&shared.secret_store);
    match (&*held, &shared.config.secrets) {
        (Some(store), _) | (None, Some(store)) => Ok(Arc::clone(store)),
        (None, None) => {
            let store: Arc<dyn SecretStore> = Arc::new(crate::secrets::Keyring::platform()?);
            *held = Some(Arc::clone(&store));
            Ok(store)
        }
    }
}

/// Why the secret store cannot be used, and what to do first, in plain words; what the store said after, on its own
/// line.
pub(super) fn store_problem(e: crate::secrets::SecretError) -> String {
    let which = if cfg!(target_os = "linux") {
        " (GNOME Keyring or KWallet)"
    } else {
        ""
    };
    format!(
        "Klefki can't use your system's secret store. Start or unlock it{which} and press Review again.\n{e}"
    )
}
