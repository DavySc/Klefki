//! OAuth client secrets, kept in the operating system's own store: Credential Manager on Windows, the Keychain on
//! macOS, the Secret Service (GNOME Keyring, KWallet) on Linux. Service `dev.klefki`, user `conn:<ConnectionId>`.
//! There is no plaintext fallback: without a store, nothing is kept and the user is told why.
//!
//! A [`Secret`] never prints (its debug form is `***`) and its memory is wiped when it is dropped.
use crate::model::ConnectionId;
use std::fmt;
use std::sync::Arc;
use zeroize::Zeroizing;

pub(crate) const SERVICE: &str = "dev.klefki";

/// A secret value, such as an OAuth client secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value, to be typed into: a password field edits it in place, so no plain copy is made.
    pub fn edit(&mut self) -> &mut String {
        &mut self.0
    }

    /// The value, for the one place it is handed on (the store, or `MCP_CLIENT_SECRET`).
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No store on this machine, or it is locked or refused access.
    Unavailable(String),
    /// The store answered, but the operation failed.
    Failed(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(why) => write!(
                f,
                "The system's secret store is not available ({why}). Klefki keeps no plaintext copy of secrets anywhere else."
            ),
            Self::Failed(why) => write!(f, "The system's secret store refused: {why}."),
        }
    }
}

/// The key a connection's secret is stored under.
pub(crate) fn user_for(id: &ConnectionId) -> String {
    format!("conn:{}", id.as_str())
}

/// Where secrets are kept. The real one is [`Keyring`]; tests use an in-memory store and never the real keychain.
pub(crate) trait SecretStore: Send + Sync {
    fn get(&self, id: &ConnectionId) -> Result<Option<Secret>, SecretError>;
    fn set(&self, id: &ConnectionId, secret: &Secret) -> Result<(), SecretError>;
    /// Whether there was one to delete.
    fn delete(&self, id: &ConnectionId) -> Result<bool, SecretError>;
}

/// The operating system's store, through `keyring-core`. Holds its store itself rather than setting a process-wide
/// default, so nothing else in the process can swap it.
pub(crate) struct Keyring(Arc<keyring_core::CredentialStore>);

impl Keyring {
    /// This platform's store. Fails when there is none (a Linux session without a Secret Service, say).
    pub(crate) fn platform() -> Result<Self, SecretError> {
        let unavailable = |e: keyring_core::Error| SecretError::Unavailable(e.to_string());
        #[cfg(windows)]
        let store: Arc<keyring_core::CredentialStore> =
            windows_native_keyring_store::Store::new().map_err(unavailable)?;
        #[cfg(target_os = "macos")]
        let store: Arc<keyring_core::CredentialStore> =
            apple_native_keyring_store::keychain::Store::new().map_err(unavailable)?;
        #[cfg(target_os = "linux")]
        let store: Arc<keyring_core::CredentialStore> =
            zbus_secret_service_keyring_store::Store::new().map_err(unavailable)?;
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        let store: Arc<keyring_core::CredentialStore> = {
            let _ = unavailable;
            return Err(SecretError::Unavailable(
                "no secret store on this platform".to_owned(),
            ));
        };
        Ok(Self(store))
    }

    #[cfg(test)]
    fn with(store: Arc<keyring_core::CredentialStore>) -> Self {
        Self(store)
    }

    fn entry(&self, id: &ConnectionId) -> Result<keyring_core::Entry, SecretError> {
        self.0.build(SERVICE, &user_for(id), None).map_err(failed)
    }
}

/// A store error in words. Never carries secret bytes: errors that hold the stored value are named, not printed.
fn failed(e: keyring_core::Error) -> SecretError {
    use keyring_core::Error as E;
    match e {
        E::NoStorageAccess(why) => SecretError::Unavailable(why.to_string()),
        E::NoDefaultStore => SecretError::Unavailable("no store".to_owned()),
        E::TooLong(what, limit) => SecretError::Failed(format!(
            "the key is longer than the {limit} characters it allows for its {what} (a very long project path)"
        )),
        E::BadEncoding(_) | E::BadDataFormat(..) => {
            SecretError::Failed("the stored value cannot be read".to_owned())
        }
        other => SecretError::Failed(other.to_string()),
    }
}

impl SecretStore for Keyring {
    fn get(&self, id: &ConnectionId) -> Result<Option<Secret>, SecretError> {
        match self.entry(id)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(failed(e)),
        }
    }

    fn set(&self, id: &ConnectionId, secret: &Secret) -> Result<(), SecretError> {
        self.entry(id)?
            .set_password(secret.expose())
            .map_err(failed)
    }

    fn delete(&self, id: &ConnectionId) -> Result<bool, SecretError> {
        match self.entry(id)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(e) => Err(failed(e)),
        }
    }
}

/// An in-memory store for tests, which can be told to fail.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryStore {
    pub values: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    pub fail: std::sync::Mutex<Option<SecretError>>,
}

#[cfg(test)]
impl MemoryStore {
    fn check(&self) -> Result<(), SecretError> {
        match &*self.fail.lock().unwrap() {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self, id: &ConnectionId) -> Result<Option<Secret>, SecretError> {
        self.check()?;
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(&user_for(id))
            .cloned()
            .map(Secret::new))
    }

    fn set(&self, id: &ConnectionId, secret: &Secret) -> Result<(), SecretError> {
        self.check()?;
        self.values
            .lock()
            .unwrap()
            .insert(user_for(id), secret.expose().to_owned());
        Ok(())
    }

    fn delete(&self, id: &ConnectionId) -> Result<bool, SecretError> {
        self.check()?;
        Ok(self.values.lock().unwrap().remove(&user_for(id)).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Scope;

    #[test]
    fn a_secret_never_prints() {
        let s = Secret::new("s3cret-value".into());
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(!format!("{:?}", Some(s.clone())).contains("s3cret"));
        assert_eq!(s.expose(), "s3cret-value");
    }

    #[test]
    fn secrets_are_keyed_by_connection_like_the_plan_says() {
        let id = ConnectionId::new(Scope::User, None, "github");
        assert_eq!(
            user_for(&id),
            "conn:eyJzIjoidXNlciIsInAiOm51bGwsIm4iOiJnaXRodWIifQ"
        );
    }

    #[test]
    fn the_keyring_adapter_stores_reads_and_forgets() {
        // keyring-core's own in-memory mock behind the real adapter: no system store is touched.
        let store = Keyring::with(keyring_core::mock::Store::new().unwrap());
        let id = ConnectionId::new(Scope::Local, Some("/w/app"), "acme-prod");
        let other = ConnectionId::new(Scope::Local, Some("/w/other"), "acme-prod");
        assert_eq!(store.get(&id), Ok(None));
        assert_eq!(store.delete(&id), Ok(false));
        store.set(&id, &Secret::new("one".into())).unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().expose(), "one");
        assert_eq!(
            store.get(&other),
            Ok(None),
            "the same name in another project is another connection"
        );
        store.set(&id, &Secret::new("two".into())).unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().expose(), "two");
        assert_eq!(store.delete(&id), Ok(true));
        assert_eq!(store.get(&id), Ok(None));
    }

    #[test]
    fn store_errors_say_what_happened_and_never_carry_the_value() {
        let leaked = failed(keyring_core::Error::BadEncoding(b"s3cret".to_vec()));
        assert!(!format!("{leaked:?} {leaked}").contains("s3cret"));
        assert!(matches!(
            failed(keyring_core::Error::TooLong("username".into(), 513)),
            SecretError::Failed(why) if why.contains("513")
        ));
        assert!(
            SecretError::Unavailable("no Secret Service".into())
                .to_string()
                .contains("no plaintext copy")
        );
    }
}
