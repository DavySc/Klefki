//! Klefki's own updates (decision 009): is a newer release out?
//!
//! The latest release's `SHA256SUMS` and its minisign signature are fetched from GitHub and checked against the
//! release key built into Klefki (`minisign.pub`, read by `build.rs`). The version comes from the signature's trusted
//! comment, which the release workflow writes as `Klefki SHA256SUMS, v1.2.3, commit …`, so an unsigned or altered
//! answer, or an older release served again, is never taken for an update. Without a built-in key nothing is
//! fetched at all.

use std::fmt;
use std::time::Duration;

/// Where the latest release's files are. GitHub redirects `latest/download` to the newest published release.
const LATEST: &str = "https://github.com/DavySc/Klefki/releases/latest/download/";
/// A release's page, with its notes and downloads.
const RELEASE_PAGE: &str = "https://github.com/DavySc/Klefki/releases/tag/v";
/// Far above any real `SHA256SUMS` (one line per file) or signature; anything larger is refused unread.
const MAX_SUMS: u64 = 64 * 1024;
const MAX_SIGNATURE: u64 = 4 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// The public key releases are signed with: the second line of `minisign.pub`, or `None` before one is committed.
pub(crate) const RELEASE_KEY: Option<&str> = option_env!("KLEFKI_RELEASE_KEY");

/// A release version: `major.minor.patch`, numbers only. Pre-releases are never offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.');
        let mut number = || -> Option<u64> {
            let part = parts.next()?;
            // No signs, spaces, leading zeros or suffixes such as `-rc.1`.
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            if part.len() > 1 && part.starts_with('0') {
                return None;
            }
            part.parse().ok()
        };
        let version = Version(number()?, number()?, number()?);
        parts.next().is_none().then_some(version)
    }

    /// The version Klefki was built as.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("the workspace version is major.minor.patch")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// What a check for updates found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheck {
    /// This is the newest release (or newer: a build from main).
    UpToDate,
    /// A newer release is out; its signature checked out.
    Available { version: Version, page: String },
    /// GitHub answered, but has no signed release yet (before the first one, or a release without a signature).
    NoRelease,
    /// The check could not finish, and why, in plain words. Nothing was trusted.
    Failed(String),
    /// This build has no release key, so it never checks (a build from source before the key existed).
    NoKey,
}

/// Fetches a file over https, refusing anything longer than `limit` bytes.
pub(crate) trait Fetch: Send + Sync {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError>;

    /// Like [`Fetch::get`], into `to` as it arrives, for files too large to hold in memory.
    fn save(&self, url: &str, limit: u64, to: &mut dyn std::io::Write) -> Result<(), FetchError> {
        let bytes = self.get(url, limit)?;
        to.write_all(&bytes)
            .map_err(|e| FetchError::Failed(format!("could not save it ({e})")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FetchError {
    /// The server answered that there is no such file.
    NotFound,
    /// Anything else, in plain words.
    Failed(String),
}

/// A newer release whose checksums are signed by the release key: what it is, and the SHA-256 of each of its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release {
    pub version: Version,
    pub sums: std::collections::HashMap<String, [u8; 32]>,
}

/// Asks GitHub for the latest release and checks its signature with `key`.
#[cfg(test)]
pub(crate) fn check(fetch: &dyn Fetch, current: Version, key: Option<&str>) -> UpdateCheck {
    check_release(fetch, current, key).0
}

/// [`check`], with the release when it is newer.
pub(crate) fn check_release(
    fetch: &dyn Fetch,
    current: Version,
    key: Option<&str>,
) -> (UpdateCheck, Option<Release>) {
    let Some(key) = key else {
        return (UpdateCheck::NoKey, None);
    };
    let fetched = fetch
        .get(&format!("{LATEST}SHA256SUMS"), MAX_SUMS)
        .and_then(|sums| {
            Ok((
                sums,
                fetch.get(&format!("{LATEST}SHA256SUMS.minisig"), MAX_SIGNATURE)?,
            ))
        });
    let (sums, signature) = match fetched {
        Ok(files) => files,
        Err(FetchError::NotFound) => return (UpdateCheck::NoRelease, None),
        Err(FetchError::Failed(why)) => {
            return (
                UpdateCheck::Failed(format!("Could not check for updates: {why}.")),
                None,
            );
        }
    };
    let version = match signed_version(&sums, &signature, key) {
        Ok(version) => version,
        Err(why) => return (UpdateCheck::Failed(why), None),
    };
    if version <= current {
        return (UpdateCheck::UpToDate, None);
    }
    // Signed, so a list that does not read is a broken release, not an attack; refused all the same.
    let Some(sums) = parse_sums(&sums) else {
        return (
            UpdateCheck::Failed("The release's list of checksums could not be read.".into()),
            None,
        );
    };
    let found = UpdateCheck::Available {
        version,
        page: release_page(version),
    };
    (found, Some(Release { version, sums }))
}

/// `sha256sum` output: one `<64 hex digits>  <name>` (or ` *<name>`) per line. `None` if any line is not.
fn parse_sums(text: &[u8]) -> Option<std::collections::HashMap<String, [u8; 32]>> {
    let text = std::str::from_utf8(text).ok()?;
    let mut sums = std::collections::HashMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (hex, name) = line.split_at_checked(64)?;
        let name = name
            .strip_prefix("  ")
            .or_else(|| name.strip_prefix(" *"))?;
        if name.is_empty() || name.contains(['/', '\\']) {
            return None;
        }
        let mut hash = [0u8; 32];
        for (byte, pair) in hash.iter_mut().zip(hex.as_bytes().chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        sums.insert(name.to_owned(), hash);
    }
    Some(sums)
}

/// The page of a release, with its notes and downloads.
pub(crate) fn release_page(version: Version) -> String {
    format!("{RELEASE_PAGE}{version}")
}

/// The release version `sums` belongs to, if `signature` is a valid signature of it by `key` and its trusted
/// comment names a release.
fn signed_version(sums: &[u8], signature: &[u8], key: &str) -> Result<Version, String> {
    let refused = |why: &str| {
        format!("The release's signature was refused ({why}). Klefki did not trust it.")
    };
    let key = minisign_verify::PublicKey::from_base64(key)
        .map_err(|_| refused("the built-in key is not valid"))?;
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| minisign_verify::Signature::decode(text).ok())
        .ok_or_else(|| refused("it is not a minisign signature"))?;
    // `false`: the release workflow signs with prehashing (minisign's default), never legacy signatures.
    key.verify(sums, &signature, false)
        .map_err(|_| refused("it does not match the checksums"))?;
    release_in_comment(signature.trusted_comment())
        .ok_or_else(|| refused("it names no release version"))
}

/// `1.2.3` from `Klefki SHA256SUMS, v1.2.3, commit …`.
fn release_in_comment(comment: &str) -> Option<Version> {
    let mut fields = comment.split(", ");
    if fields.next()? != "Klefki SHA256SUMS" {
        return None;
    }
    Version::parse(fields.next()?.strip_prefix('v')?)
}

/// Fetches nothing: for the command-line tool, and for tests, which never reach the network.
pub(crate) struct Offline;

impl Fetch for Offline {
    fn get(&self, _url: &str, _limit: u64) -> Result<Vec<u8>, FetchError> {
        Err(FetchError::Failed("offline".into()))
    }
}

/// GitHub as it looks with release 0.2.0 out, signed with a test key: the fetcher and that key.
#[cfg(test)]
pub(crate) fn test_release() -> (std::sync::Arc<dyn Fetch>, String) {
    struct Release;
    impl Fetch for Release {
        fn get(&self, url: &str, _limit: u64) -> Result<Vec<u8>, FetchError> {
            match url.strip_prefix(LATEST) {
                Some("SHA256SUMS") => Ok(include_bytes!("update/SHA256SUMS").to_vec()),
                Some("SHA256SUMS.minisig") => Ok(include_bytes!("update/v0.2.0.minisig").to_vec()),
                // The setup's checksum in `SHA256SUMS` is that of "b".
                _ if url
                    == "https://github.com/DavySc/Klefki/releases/download/v0.2.0/klefki-0.2.0-windows-x64-setup.exe" =>
                {
                    Ok(b"b".to_vec())
                }
                _ => Err(FetchError::NotFound),
            }
        }
    }
    let key = include_str!("update/test.pub")
        .lines()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    (std::sync::Arc::new(Release), key)
}

/// Fetches over https with TLS verification against the system's certificates, through the proxy the environment
/// names, if any. Redirects (GitHub sends downloads to its file host) must stay on https. Set up on first use, so
/// starting Klefki never waits on reading the system's certificates.
#[derive(Default)]
pub(crate) struct Https {
    agent: std::sync::OnceLock<ureq::Agent>,
}

impl Https {
    fn agent(&self) -> &ureq::Agent {
        self.agent.get_or_init(|| {
            let roots = rustls_native_certs::load_native_certs()
                .certs
                .into_iter()
                .map(|der| ureq::tls::Certificate::from_der(der.as_ref()).to_owned())
                .collect::<Vec<_>>();
            let tls = ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .unversioned_rustls_crypto_provider(std::sync::Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .root_certs(ureq::tls::RootCerts::Specific(std::sync::Arc::new(roots)))
                .build();
            ureq::Agent::config_builder()
                .https_only(true)
                .tls_config(tls)
                .max_redirects(5)
                .timeout_global(Some(TIMEOUT))
                .user_agent(concat!("Klefki/", env!("CARGO_PKG_VERSION")))
                .build()
                .new_agent()
        })
    }
}

impl Fetch for Https {
    fn save(&self, url: &str, limit: u64, to: &mut dyn std::io::Write) -> Result<(), FetchError> {
        if !url.starts_with("https://") {
            return Err(FetchError::Failed("only https is used".into()));
        }
        let mut response = self.agent().get(url).call().map_err(|e| plain_error(&e))?;
        let mut body = response.body_mut().with_config().limit(limit).reader();
        std::io::copy(&mut body, to).map_err(|e| match e.downcast::<ureq::Error>() {
            Ok(e) => plain_error(&e),
            Err(e) => FetchError::Failed(format!("the download stopped ({})", e.kind())),
        })?;
        Ok(())
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        if !url.starts_with("https://") {
            return Err(FetchError::Failed("only https is used".into()));
        }
        let mut response = self.agent().get(url).call().map_err(|e| plain_error(&e))?;
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| plain_error(&e))
    }
}

/// A short reason for a failed request, without addresses or headers.
fn plain_error(error: &ureq::Error) -> FetchError {
    FetchError::Failed(match error {
        ureq::Error::StatusCode(404) => return FetchError::NotFound,
        ureq::Error::StatusCode(code) => format!("GitHub answered {code}"),
        ureq::Error::Timeout(_) => "it took too long".into(),
        ureq::Error::Io(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
            "no connection".into()
        }
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "the secure connection failed".into(),
        ureq::Error::BodyExceedsLimit(_) => "the answer was too large".into(),
        ureq::Error::RequireHttpsOnly(_) => "GitHub redirected to an address without https".into(),
        _ => "the request failed".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // A test key pair made with `minisign -G -W` (no password), used only here. The signatures below were made
    // with `minisign -S -W -s test.key -m SHA256SUMS -t "<comment>"` over `SUMS`.
    const KEY: &str = include_str!("update/test.pub");
    const SUMS: &[u8] = include_bytes!("update/SHA256SUMS");

    fn signature(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/src/update/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn key() -> &'static str {
        KEY.lines().nth(1).unwrap()
    }

    #[derive(Default)]
    struct FakeGitHub {
        files: HashMap<String, Result<Vec<u8>, FetchError>>,
        asked: Mutex<Vec<(String, u64)>>,
    }

    impl FakeGitHub {
        fn serving(sums: &[u8], signature: Vec<u8>) -> FakeGitHub {
            let mut fake = FakeGitHub::default();
            fake.files
                .insert(format!("{LATEST}SHA256SUMS"), Ok(sums.to_vec()));
            fake.files
                .insert(format!("{LATEST}SHA256SUMS.minisig"), Ok(signature));
            fake
        }
    }

    impl Fetch for FakeGitHub {
        fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
            self.asked.lock().unwrap().push((url.to_string(), limit));
            self.files
                .get(url)
                .cloned()
                .unwrap_or(Err(FetchError::NotFound))
        }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_newer_signed_release_is_offered_with_its_page() {
        let github = FakeGitHub::serving(SUMS, signature("v0.2.0.minisig"));
        assert_eq!(
            check(&github, v("0.1.0"), Some(key())),
            UpdateCheck::Available {
                version: v("0.2.0"),
                page: "https://github.com/DavySc/Klefki/releases/tag/v0.2.0".into()
            }
        );
        let asked = github.asked.lock().unwrap();
        assert!(
            asked
                .iter()
                .all(|(url, _)| url.starts_with("https://github.com/DavySc/Klefki/releases/"))
        );
        assert!(asked.iter().all(|(_, limit)| *limit <= MAX_SUMS));
    }

    #[test]
    fn a_newer_release_comes_with_the_checksum_of_each_file() {
        let github = FakeGitHub::serving(SUMS, signature("v0.2.0.minisig"));
        let (_, release) = check_release(&github, v("0.1.0"), Some(key()));
        let release = release.unwrap();
        assert_eq!(release.version, v("0.2.0"));
        let deb = release.sums["klefki-0.2.0-linux-amd64.deb"];
        assert_eq!(deb[..4], [0xca, 0x97, 0x81, 0x12], "sha256 of \"a\"");
        assert_eq!(release.sums.len(), 2);
        // Nothing to install when nothing is newer.
        assert_eq!(check_release(&github, v("0.2.0"), Some(key())).1, None);
    }

    #[test]
    fn checksum_lists_parse_strictly() {
        let hash = "ab".repeat(32);
        let sums = parse_sums(format!("{hash}  a.deb\n{hash} *b.exe\n").as_bytes()).unwrap();
        assert_eq!(sums["a.deb"], [0xab; 32]);
        assert_eq!(sums["b.exe"], [0xab; 32]);
        for bad in [
            format!("{hash} a.deb"),
            format!("{}  a.deb", &hash[..62]),
            format!("{}zz  a.deb", &hash[..62]),
            format!("{hash}  ../a.deb"),
            format!("{hash}  dir\\a.deb"),
            format!("{hash}  "),
        ] {
            assert_eq!(parse_sums(bad.as_bytes()), None, "{bad:?}");
        }
    }

    #[test]
    fn the_same_or_an_older_release_is_up_to_date() {
        let github = FakeGitHub::serving(SUMS, signature("v0.2.0.minisig"));
        assert_eq!(
            check(&github, v("0.2.0"), Some(key())),
            UpdateCheck::UpToDate
        );
        assert_eq!(
            check(&github, v("0.10.0"), Some(key())),
            UpdateCheck::UpToDate
        );
        assert_eq!(
            check(&github, v("1.0.0"), Some(key())),
            UpdateCheck::UpToDate
        );
    }

    #[test]
    fn versions_compare_as_numbers_not_text() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.10") > v("0.1.2"));
    }

    #[test]
    fn checksums_changed_after_signing_are_refused() {
        let mut altered = SUMS.to_vec();
        altered[0] ^= 1;
        let github = FakeGitHub::serving(&altered, signature("v0.2.0.minisig"));
        let UpdateCheck::Failed(why) = check(&github, v("0.1.0"), Some(key())) else {
            panic!("altered checksums were accepted");
        };
        assert!(why.contains("does not match"), "{why}");
    }

    #[test]
    fn a_signature_by_another_key_is_refused() {
        let github = FakeGitHub::serving(SUMS, signature("other-key.minisig"));
        assert!(matches!(
            check(&github, v("0.1.0"), Some(key())),
            UpdateCheck::Failed(_)
        ));
    }

    #[test]
    fn a_trusted_comment_without_a_release_version_is_refused() {
        // A manual run on main signs with `main` as the version: never an update.
        for name in [
            "main.minisig",
            "prerelease.minisig",
            "other-comment.minisig",
        ] {
            let github = FakeGitHub::serving(SUMS, signature(name));
            let UpdateCheck::Failed(why) = check(&github, v("0.1.0"), Some(key())) else {
                panic!("{name} was accepted");
            };
            assert!(why.contains("no release version"), "{name}: {why}");
        }
    }

    #[test]
    fn garbage_instead_of_a_signature_is_refused() {
        for junk in [
            &b""[..],
            b"untrusted comment: x\n",
            b"\xff\xfe",
            b"<html>rate limited</html>",
        ] {
            let github = FakeGitHub::serving(SUMS, junk.to_vec());
            assert!(matches!(
                check(&github, v("0.1.0"), Some(key())),
                UpdateCheck::Failed(_)
            ));
        }
    }

    #[test]
    fn without_a_key_nothing_is_fetched() {
        let github = FakeGitHub::serving(SUMS, signature("v0.2.0.minisig"));
        assert_eq!(check(&github, v("0.1.0"), None), UpdateCheck::NoKey);
        assert!(github.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_fetch_failure_says_so_and_trusts_nothing() {
        let mut github = FakeGitHub::serving(SUMS, signature("v0.2.0.minisig"));
        github.files.insert(
            format!("{LATEST}SHA256SUMS.minisig"),
            Err(FetchError::Failed("no connection".into())),
        );
        assert_eq!(
            check(&github, v("0.1.0"), Some(key())),
            UpdateCheck::Failed("Could not check for updates: no connection.".into())
        );
    }

    #[test]
    fn no_published_release_is_an_answer_not_a_failure() {
        // Before the first signed release, GitHub answers 404 for both files; a release without a signature
        // answers 404 for the signature only.
        assert_eq!(
            check(&FakeGitHub::default(), v("0.1.0"), Some(key())),
            UpdateCheck::NoRelease
        );
        let mut github = FakeGitHub::serving(SUMS, Vec::new());
        github.files.remove(&format!("{LATEST}SHA256SUMS.minisig"));
        assert_eq!(
            check(&github, v("0.1.0"), Some(key())),
            UpdateCheck::NoRelease
        );
    }

    #[test]
    fn only_plain_release_versions_parse() {
        assert_eq!(Version::parse("0.1.0"), Some(Version(0, 1, 0)));
        assert_eq!(Version::parse("12.345.6789"), Some(Version(12, 345, 6789)));
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.3-rc.1",
            "1.2.3+b",
            "01.2.3",
            " 1.2.3",
            "1..3",
            "+1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(
            Version::parse("9999999999.0.0"),
            None,
            "too long to be a real version"
        );
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
    }

    #[test]
    fn this_build_has_a_plain_version() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn https_fetch_refuses_plain_http() {
        assert_eq!(
            Https::default().get("http://github.com/", 10),
            Err(FetchError::Failed("only https is used".into()))
        );
    }
}
