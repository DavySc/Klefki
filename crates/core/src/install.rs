//! Installing an update that [`crate::update`] found (decision 009, slice 6b): which file this copy of Klefki needs,
//! downloading it while checking it against the release's signed checksums, and putting it in place.
//!
//! Only three kinds of install update themselves: the per-user Windows setup, the macOS app and the AppImage. The MSI
//! (an administrator's), the zip (unpacked by hand) and the `.deb` (apt, with sudo) are told where to get it instead.

use crate::update::{Fetch, FetchError, Release, Version};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a release's files are downloaded from.
const DOWNLOAD: &str = "https://github.com/DavySc/Klefki/releases/download/v";
/// Far above any real package (about 25 MB); a larger download is stopped.
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;

/// How this copy of Klefki was installed, and so how it updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Windows, by the per-user setup: the new setup runs over it.
    WindowsSetup,
    /// macOS, the app bundle at this path: replaced by the new one.
    MacApp(PathBuf),
    /// Linux, the AppImage at this path: replaced by the new one.
    AppImage(PathBuf),
    /// Installed another way, updated by hand; why, in a sentence.
    Manual(String),
}

impl InstallKind {
    /// Works out how the program at `exe` was installed. `appimage` holds the `APPIMAGE` and `APPDIR` variables the
    /// AppImage runtime sets (the AppImage, and where it is mounted), and `uninstaller` whether the Windows setup's `uninstall.exe` is next to `exe` (the MSI and the zip have
    /// none; the setup installs per user, wherever the user chose).
    pub(crate) fn detect(
        os: &str,
        exe: &Path,
        appimage: Option<(&Path, &Path)>,
        uninstaller: bool,
    ) -> InstallKind {
        let manual = |why: &str| InstallKind::Manual(why.to_owned());
        match os {
            "windows" => {
                let path = exe.to_string_lossy().replace('\\', "/").to_lowercase();
                if uninstaller {
                    InstallKind::WindowsSetup
                } else if path.contains("/program files/") || path.contains("/program files (x86)/")
                {
                    manual(
                        "Klefki was installed with the MSI, so whoever manages this computer updates it.",
                    )
                } else {
                    manual(
                        "This copy of Klefki was unpacked from the zip, so it is updated by hand.",
                    )
                }
            }
            "macos" => {
                let bundle = exe
                    .parent()
                    .filter(|dir| dir.ends_with("Contents/MacOS"))
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .filter(|app| app.extension().is_some_and(|e| e == "app"));
                match bundle {
                    // Gatekeeper runs an app never moved out of Downloads from a read-only copy.
                    Some(app) if app.to_string_lossy().contains("/AppTranslocation/") => manual(
                        "macOS runs this Klefki from a temporary copy. Move Klefki to your Applications folder to \
                         get updates.",
                    ),
                    Some(app) => InstallKind::MacApp(app.to_path_buf()),
                    None => manual(
                        "This copy of Klefki is not an app bundle, so it is updated by hand.",
                    ),
                }
            }
            // Linux paths, compared as text so the same rule holds wherever this runs. The variables are passed on to
            // every program started from an AppImage, so they are Klefki's only when it runs from that mount.
            "linux" => match appimage {
                Some((image, mount))
                    if image.to_string_lossy().starts_with('/')
                        && !mount.as_os_str().is_empty()
                        && exe.to_string_lossy().starts_with(&format!(
                            "{}/",
                            mount.to_string_lossy().trim_end_matches('/')
                        )) =>
                {
                    InstallKind::AppImage(image.to_path_buf())
                }
                _ => manual(
                    "Klefki was installed from the .deb package: update it with apt, as the release notes say.",
                ),
            },
            _ => manual("Klefki cannot update itself on this system."),
        }
    }

    /// The release file this kind of install updates from.
    pub(crate) fn package(&self, version: Version) -> Option<String> {
        match self {
            InstallKind::WindowsSetup => Some(format!("klefki-{version}-windows-x64-setup.exe")),
            InstallKind::MacApp(_) => Some(format!("klefki-{version}-macos-universal.app.zip")),
            InstallKind::AppImage(_) => Some(format!("klefki-{version}-linux-x86_64.AppImage")),
            InstallKind::Manual(_) => None,
        }
    }
}

/// How the running Klefki was installed.
pub(crate) fn this_install() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Manual("Klefki could not tell where it is installed.".into());
    };
    // Set by the AppImage runtime to the AppImage it started from and where it mounted it.
    let image = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let mount = std::env::var_os("APPDIR").map(PathBuf::from);
    let uninstaller = exe
        .parent()
        .is_some_and(|dir| dir.join("uninstall.exe").is_file());
    let appimage = image.as_deref().zip(mount.as_deref());
    InstallKind::detect(std::env::consts::OS, &exe, appimage, uninstaller)
}

/// A downloaded update whose checksum matched, ready to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Staged {
    pub version: Version,
    pub kind: InstallKind,
    pub file: PathBuf,
}

/// Whether Klefki can put a new version where this one is: it must be able to create files next to the app or the
/// AppImage, which a standard user cannot in `/Applications`, nor anyone on a disk image. Checked before downloading,
/// so nothing is downloaded that cannot be installed.
pub(crate) fn can_replace(kind: &InstallKind) -> Result<(), String> {
    let (path, what) = match kind {
        InstallKind::MacApp(bundle) => (
            bundle,
            "Move Klefki to a folder you can change, such as your own Applications folder, or update it by hand.",
        ),
        InstallKind::AppImage(image) => (
            image,
            "Move the AppImage to a folder you can change, or update it by hand.",
        ),
        InstallKind::WindowsSetup => return Ok(()),
        InstallKind::Manual(why) => return Err(why.clone()),
    };
    let writable = path.parent().is_some_and(|dir| {
        tempfile::Builder::new()
            .prefix(".klefki-check-")
            .tempfile_in(dir)
            .is_ok()
    });
    if writable {
        Ok(())
    } else {
        Err(format!(
            "Klefki cannot replace itself in this folder. {what}"
        ))
    }
}

/// Where an install that failed as Klefki closed leaves its reason, for the next start to show.
const FAILED_NOTE: &str = "install-failed.txt";

/// Keeps why installing failed as Klefki closed (nothing is on screen then).
pub(crate) fn note_failure(dir: &Path, why: &str) {
    if crate::config::docstore::create_private_dir(dir).is_ok() {
        let _ = std::fs::remove_file(dir.join(FAILED_NOTE));
        if let Ok(mut file) = crate::config::docstore::create_private_file(&dir.join(FAILED_NOTE)) {
            let _ = file.write_all(why.as_bytes());
        }
    }
}

/// Why installing failed when Klefki last closed, once: the note is removed.
pub(crate) fn take_failure(dir: &Path) -> Option<String> {
    use std::io::Read;
    let path = dir.join(FAILED_NOTE);
    let mut text = String::new();
    std::fs::File::open(&path)
        .ok()?
        .take(4096)
        .read_to_string(&mut text)
        .ok();
    let _ = std::fs::remove_file(&path);
    Some(crate::sanitize::sanitize(&text)).filter(|why| !why.trim().is_empty())
}

/// Downloads this install's package of `release` into `dir` and checks it against the signed checksum. A file that
/// does not match is deleted, never kept.
pub(crate) fn download(
    fetch: &dyn Fetch,
    release: &Release,
    kind: &InstallKind,
    dir: &Path,
) -> Result<Staged, String> {
    let Some(name) = kind.package(release.version) else {
        return Err(match kind {
            InstallKind::Manual(why) => why.clone(),
            _ => "Klefki cannot update itself here.".into(),
        });
    };
    let expected = release
        .sums
        .get(&name)
        .ok_or_else(|| format!("The release has no {name}."))?;
    crate::config::docstore::create_private_dir(dir)
        .map_err(|e| format!("Could not make the folder for the download ({e})."))?;
    // Packages of earlier updates (or a partial one) are not kept.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("klefki-")
                && entry.file_type().is_ok_and(|t| t.is_file())
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let file = dir.join(&name);
    let mut out = HashingFile {
        file: crate::config::docstore::create_private_file(&file)
            .map_err(|e| format!("Could not save the download ({e})."))?,
        hash: Sha256::new(),
    };
    let url = format!("{DOWNLOAD}{}/{name}", release.version);
    let saved = fetch
        .save(&url, MAX_PACKAGE, &mut out)
        .map_err(|e| match e {
            FetchError::NotFound => format!("The release has no {name} to download."),
            FetchError::Failed(why) => format!("Could not download the update: {why}."),
        })
        .and_then(|()| {
            out.file
                .flush()
                .map_err(|e| format!("Could not save the download ({e})."))
        });
    let matches = saved.is_ok() && out.hash.finalize().as_slice() == expected;
    drop(out.file);
    if !matches {
        let _ = std::fs::remove_file(&file);
        saved?;
        return Err(format!(
            "The download did not match the release's signed checksum, so it was deleted ({name})."
        ));
    }
    Ok(Staged {
        version: release.version,
        kind: kind.clone(),
        file,
    })
}

struct HashingFile {
    file: std::fs::File,
    hash: Sha256,
}

impl Write for HashingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Puts a downloaded update in place. With `relaunch`, the new version is started afterwards; the caller then
/// closes Klefki. Without it (Klefki is closing), it is only installed.
pub(crate) fn apply(staged: &Staged, relaunch: bool) -> Result<(), String> {
    match &staged.kind {
        // The setup closes a running Klefki itself. `/P` shows its progress without asking anything, `/S` shows
        // nothing, and `/R` starts Klefki again when it is done (cargo-packager's NSIS installer).
        InstallKind::WindowsSetup => {
            // `/NS`: the shortcuts the user kept, moved or deleted stay as they are.
            let args: &[&str] = if relaunch {
                &["/P", "/R", "/NS"]
            } else {
                &["/S", "/NS"]
            };
            Command::new(&staged.file)
                .args(args)
                .spawn()
                .map(drop)
                .map_err(|e| {
                    format!("Could not start the installer ({e}). Windows may have blocked it.")
                })
        }
        InstallKind::MacApp(bundle) => {
            replace_bundle(&staged.file, bundle)?;
            if relaunch {
                Command::new("/usr/bin/open")
                    .arg("-n")
                    .arg(bundle)
                    .spawn()
                    .map_err(|e| {
                        format!("Klefki was updated, but could not be started again ({e}).")
                    })?;
            }
            Ok(())
        }
        InstallKind::AppImage(path) => {
            replace_file(&staged.file, path)?;
            if relaunch {
                Command::new(path).spawn().map_err(|e| {
                    format!("Klefki was updated, but could not be started again ({e}).")
                })?;
            }
            Ok(())
        }
        InstallKind::Manual(why) => Err(why.clone()),
    }
}

/// Replaces the AppImage at `path` with `new`: copied next to it first, so the swap is a rename on one disk, and a
/// failure leaves the old one as it was.
fn replace_file(new: &Path, path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("The AppImage has no folder.")?;
    let cannot = |e: std::io::Error| {
        format!(
            "Could not replace {} ({e}). Download the new version by hand.",
            path.display()
        )
    };
    let next = tempfile::Builder::new()
        .prefix(".klefki-update-")
        .tempfile_in(dir)
        .map_err(cannot)?;
    std::fs::copy(new, next.path()).map_err(cannot)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(next.path(), std::fs::Permissions::from_mode(0o755))
            .map_err(cannot)?;
    }
    next.persist(path).map_err(|e| cannot(e.error))?;
    let _ = std::fs::remove_file(new);
    Ok(())
}

/// Replaces the app bundle at `bundle` with the one in the zip `new`. Unpacked with the system's `ditto`, which keeps
/// the code signature and the bundle's attributes intact, next to the old bundle; then the old one is moved aside,
/// the new one into its place, and the old one deleted. If the second move fails, the old one is put back.
fn replace_bundle(new: &Path, bundle: &Path) -> Result<(), String> {
    let dir = bundle.parent().ok_or("The app has no folder.")?;
    let cannot = |what: String| {
        format!(
            "Could not replace {} ({what}). Download the new version by hand.",
            bundle.display()
        )
    };
    let unpacked = tempfile::Builder::new()
        .prefix(".klefki-update-")
        .tempdir_in(dir)
        .map_err(|e| cannot(e.to_string()))?;
    unpack_zip(new, unpacked.path()).map_err(cannot)?;
    let fresh = unpacked.path().join("Klefki.app");
    if !fresh
        .join("Contents")
        .join("MacOS")
        .join("klefki")
        .is_file()
    {
        return Err(cannot("the download holds no Klefki.app".into()));
    }
    let old = unpacked.path().join("old.app");
    std::fs::rename(bundle, &old).map_err(|e| cannot(e.to_string()))?;
    if let Err(e) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(cannot(e.to_string()));
    }
    // `unpacked`, with the old app in it, is deleted when it goes out of scope.
    let _ = std::fs::remove_file(new);
    Ok(())
}

#[cfg(target_os = "macos")]
fn unpack_zip(zip: &Path, into: &Path) -> Result<(), String> {
    let status = Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(into)
        .status()
        .map_err(|e| e.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("ditto: {status}"))
}

/// Only macOS unpacks app bundles; elsewhere this is never reached, except by tests, which unpack with `unzip`.
#[cfg(not(target_os = "macos"))]
fn unpack_zip(zip: &Path, into: &Path) -> Result<(), String> {
    #[cfg(test)]
    {
        let status = Command::new("unzip")
            .arg("-q")
            .arg(zip)
            .arg("-d")
            .arg(into)
            .status()
            .map_err(|e| e.to_string())?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("unzip: {status}"))
    }
    #[cfg(not(test))]
    {
        let _ = (zip, into);
        Err("app bundles are only unpacked on macOS".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn sha(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    /// GitHub serving some release files.
    #[derive(Default)]
    struct Files {
        files: HashMap<String, Vec<u8>>,
        asked: Mutex<Vec<(String, u64)>>,
    }

    impl Fetch for Files {
        fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
            self.asked.lock().unwrap().push((url.to_owned(), limit));
            self.files.get(url).cloned().ok_or(FetchError::NotFound)
        }
    }

    fn release(name: &str, content: &[u8]) -> (Files, Release) {
        let mut files = Files::default();
        files.files.insert(
            format!("https://github.com/DavySc/Klefki/releases/download/v0.2.0/{name}"),
            content.to_vec(),
        );
        let release = Release {
            version: v("0.2.0"),
            sums: HashMap::from([(name.to_owned(), sha(content))]),
        };
        (files, release)
    }

    #[test]
    fn each_install_is_recognised_from_where_klefki_runs() {
        let win = |exe: &str, uninstaller| {
            InstallKind::detect("windows", Path::new(exe), None, uninstaller)
        };
        assert_eq!(
            win(r"C:\Users\ann\AppData\Local\Klefki\klefki.exe", true),
            InstallKind::WindowsSetup
        );
        assert_eq!(
            win(r"D:\Apps\Klefki\klefki.exe", true),
            InstallKind::WindowsSetup,
            "wherever the user chose"
        );
        assert!(
            matches!(win(r"C:\Program Files\Klefki\klefki.exe", false), InstallKind::Manual(why) if why.contains("MSI"))
        );
        assert!(
            matches!(win(r"D:\tools\klefki\klefki.exe", false), InstallKind::Manual(why) if why.contains("zip"))
        );

        let mac = |exe: &str| InstallKind::detect("macos", Path::new(exe), None, false);
        assert_eq!(
            mac("/Applications/Klefki.app/Contents/MacOS/klefki"),
            InstallKind::MacApp("/Applications/Klefki.app".into())
        );
        assert_eq!(
            mac("/Users/ann/Applications/Klefki.app/Contents/MacOS/klefki"),
            InstallKind::MacApp("/Users/ann/Applications/Klefki.app".into())
        );
        assert!(matches!(
            mac("/usr/local/bin/klefki"),
            InstallKind::Manual(_)
        ));
        assert!(matches!(
            mac("/opt/Contents/MacOS/klefki"),
            InstallKind::Manual(_)
        ));

        // Translocated by Gatekeeper (never moved out of Downloads): a read-only copy, so updated by hand.
        assert!(matches!(
            mac("/private/var/folders/x/T/AppTranslocation/1234/d/Klefki.app/Contents/MacOS/klefki"),
            InstallKind::Manual(why) if why.contains("Applications folder")
        ));

        let image = Path::new("/home/ann/Apps/klefki.AppImage");
        let mount = Path::new("/tmp/.mount_x");
        let linux =
            |exe: &str, appimage| InstallKind::detect("linux", Path::new(exe), appimage, false);
        assert_eq!(
            linux("/tmp/.mount_x/usr/bin/klefki", Some((image, mount))),
            InstallKind::AppImage(image.into())
        );
        assert!(matches!(
            linux("/usr/bin/klefki", None),
            InstallKind::Manual(why) if why.contains(".deb")
        ));
        assert!(matches!(
            linux("/usr/bin/klefki", Some((Path::new("relative"), mount))),
            InstallKind::Manual(_)
        ));
    }

    /// Another AppImage (an editor, a terminal) passes `APPIMAGE` and `APPDIR` on to what is started from it: they
    /// name that program, not Klefki, which must never replace it.
    #[test]
    fn an_appimage_klefki_was_started_from_is_not_taken_for_klefki() {
        let editor = Path::new("/home/ann/Apps/Editor.AppImage");
        let linux = |exe: &str, appdir: &str| {
            InstallKind::detect(
                "linux",
                Path::new(exe),
                Some((editor, Path::new(appdir))),
                false,
            )
        };
        assert!(matches!(
            linux("/usr/bin/klefki", "/tmp/.mount_editor"),
            InstallKind::Manual(why) if why.contains(".deb")
        ));
        // Klefki's own mount only: a mount whose name starts the same is another one.
        assert!(matches!(
            linux("/tmp/.mount_editorX/usr/bin/klefki", "/tmp/.mount_editor"),
            InstallKind::Manual(_)
        ));
        // Without APPDIR there is no telling whose APPIMAGE it is.
        assert!(matches!(
            InstallKind::detect(
                "linux",
                Path::new("/usr/bin/klefki"),
                Some((editor, Path::new(""))),
                false
            ),
            InstallKind::Manual(_)
        ));
    }

    #[test]
    fn each_install_updates_from_its_own_package() {
        let v2 = v("0.2.0");
        assert_eq!(
            InstallKind::WindowsSetup.package(v2).as_deref(),
            Some("klefki-0.2.0-windows-x64-setup.exe")
        );
        assert_eq!(
            InstallKind::MacApp("/A/Klefki.app".into())
                .package(v2)
                .as_deref(),
            Some("klefki-0.2.0-macos-universal.app.zip")
        );
        assert_eq!(
            InstallKind::AppImage("/a".into()).package(v2).as_deref(),
            Some("klefki-0.2.0-linux-x86_64.AppImage")
        );
        assert_eq!(InstallKind::Manual("x".into()).package(v2), None);
    }

    #[test]
    fn a_download_that_matches_the_signed_checksum_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("klefki.AppImage");
        let kind = InstallKind::AppImage(image);
        let (files, release) = release("klefki-0.2.0-linux-x86_64.AppImage", b"new klefki");
        let staged = download(&files, &release, &kind, &dir.path().join("updates")).unwrap();
        assert_eq!(std::fs::read(&staged.file).unwrap(), b"new klefki");
        assert_eq!(staged.version, v("0.2.0"));
        let asked = files.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert!(
            asked[0]
                .0
                .starts_with("https://github.com/DavySc/Klefki/releases/download/v0.2.0/")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&staged.file)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "only its owner may read the download");
        }
    }

    #[test]
    fn a_download_that_does_not_match_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let kind = InstallKind::AppImage(dir.path().join("klefki.AppImage"));
        let (mut files, release) = release("klefki-0.2.0-linux-x86_64.AppImage", b"new klefki");
        for content in files.files.values_mut() {
            *content = b"something else".to_vec();
        }
        let updates = dir.path().join("updates");
        let err = download(&files, &release, &kind, &updates).unwrap_err();
        assert!(err.contains("did not match"), "{err}");
        assert!(!updates.join("klefki-0.2.0-linux-x86_64.AppImage").exists());
    }

    #[test]
    fn a_release_without_this_package_or_a_failed_download_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let updates = dir.path().join("updates");
        let (files, release) = release("klefki-0.2.0-linux-amd64.deb", b"deb");
        let kind = InstallKind::AppImage(dir.path().join("k.AppImage"));
        let err = download(&files, &release, &kind, &updates).unwrap_err();
        assert!(
            err.contains("no klefki-0.2.0-linux-x86_64.AppImage"),
            "{err}"
        );
        assert!(
            files.asked.lock().unwrap().is_empty(),
            "not asked for a file it has no checksum for"
        );

        // Listed, but GitHub has no such file: nothing is left behind.
        let release = Release {
            version: v("0.2.0"),
            sums: HashMap::from([("klefki-0.2.0-linux-x86_64.AppImage".into(), [0; 32])]),
        };
        let err = download(&Files::default(), &release, &kind, &updates).unwrap_err();
        assert!(
            err.contains("no klefki-0.2.0-linux-x86_64.AppImage to download"),
            "{err}"
        );
        assert!(!updates.join("klefki-0.2.0-linux-x86_64.AppImage").exists());

        let manual = InstallKind::Manual("Use apt.".into());
        assert_eq!(
            download(&files, &release, &manual, &updates),
            Err("Use apt.".into())
        );
    }

    #[test]
    fn nothing_is_downloaded_for_a_place_klefki_cannot_change() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            can_replace(&InstallKind::AppImage(dir.path().join("k.AppImage"))),
            Ok(())
        );
        assert_eq!(
            can_replace(&InstallKind::MacApp(dir.path().join("Klefki.app"))),
            Ok(())
        );
        assert_eq!(can_replace(&InstallKind::WindowsSetup), Ok(()));
        assert_eq!(
            can_replace(&InstallKind::Manual("Use apt.".into())),
            Err("Use apt.".into())
        );
        // A folder that cannot take a new file, like /Applications for a standard user or a disk image.
        let gone = dir.path().join("not-there");
        assert!(matches!(
            can_replace(&InstallKind::MacApp(gone.join("Klefki.app"))),
            Err(why) if why.contains("cannot replace itself") && why.contains("Applications folder")
        ));
        assert!(matches!(
            can_replace(&InstallKind::AppImage(gone.join("k.AppImage"))),
            Err(why) if why.contains("cannot replace itself")
        ));
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(left.is_empty(), "the check leaves nothing behind");
    }

    #[test]
    fn a_new_download_clears_earlier_packages_but_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let updates = dir.path().join("updates");
        std::fs::create_dir(&updates).unwrap();
        std::fs::write(updates.join("klefki-0.1.5-linux-x86_64.AppImage"), "old").unwrap();
        std::fs::write(
            updates.join("klefki-0.2.0-linux-x86_64.AppImage"),
            "partial",
        )
        .unwrap();
        note_failure(&updates, "kept");
        let kind = InstallKind::AppImage(dir.path().join("k.AppImage"));
        let (files, release) = release("klefki-0.2.0-linux-x86_64.AppImage", b"new klefki");
        download(&files, &release, &kind, &updates).unwrap();
        let mut left: Vec<_> = std::fs::read_dir(&updates)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["install-failed.txt", "klefki-0.2.0-linux-x86_64.AppImage"]
        );
    }

    #[test]
    fn a_failure_at_close_is_told_once() {
        let dir = tempfile::tempdir().unwrap();
        let updates = dir.path().join("updates");
        assert_eq!(take_failure(&updates), None);
        note_failure(
            &updates,
            "Could not replace /Applications/Klefki.app (denied).",
        );
        note_failure(
            &updates,
            "Could not replace /Applications/Klefki.app (denied again).",
        );
        assert_eq!(
            take_failure(&updates).as_deref(),
            Some("Could not replace /Applications/Klefki.app (denied again).")
        );
        assert_eq!(take_failure(&updates), None, "once");
    }

    #[cfg(unix)]
    #[test]
    fn an_appimage_is_replaced_in_place_and_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("Klefki.AppImage");
        std::fs::write(&image, "old").unwrap();
        let file = dir.path().join("download");
        std::fs::write(&file, "new").unwrap();
        let staged = Staged {
            version: v("0.2.0"),
            kind: InstallKind::AppImage(image.clone()),
            file: file.clone(),
        };
        apply(&staged, false).unwrap();
        assert_eq!(std::fs::read_to_string(&image).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&image).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!file.exists(), "the download is cleaned up");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file left next to it");
    }

    #[cfg(unix)]
    #[test]
    fn an_appimage_in_a_folder_klefki_cannot_change_stays_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("ro");
        std::fs::create_dir(&folder).unwrap();
        let image = folder.join("Klefki.AppImage");
        std::fs::write(&image, "old").unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        let file = dir.path().join("download");
        std::fs::write(&file, "new").unwrap();
        let staged = Staged {
            version: v("0.2.0"),
            kind: InstallKind::AppImage(image.clone()),
            file,
        };
        let result = apply(&staged, false);
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root may write anyway; then it simply worked.
        if let Err(why) = result {
            assert!(why.contains("Download the new version by hand"), "{why}");
            assert_eq!(std::fs::read_to_string(&image).unwrap(), "old");
        }
    }

    /// The app bundle swap, with a zip as the release workflow makes it (`ditto -c -k --keepParent`).
    #[cfg(unix)]
    #[test]
    fn an_app_bundle_is_replaced_by_the_one_in_the_zip() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let bundle = apps.join("Klefki.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/klefki"), "old").unwrap();
        std::fs::write(bundle.join("Contents/stale-file"), "old").unwrap();

        let source = dir.path().join("src");
        std::fs::create_dir_all(source.join("Klefki.app/Contents/MacOS")).unwrap();
        std::fs::write(source.join("Klefki.app/Contents/MacOS/klefki"), "new").unwrap();
        let zip = dir.path().join("klefki.app.zip");
        let zipped = Command::new("zip")
            .arg("-qr")
            .arg(&zip)
            .arg("Klefki.app")
            .current_dir(&source)
            .status();
        assert!(
            zipped.is_ok_and(|s| s.success()),
            "zip makes the test's download"
        );
        let staged = Staged {
            version: v("0.2.0"),
            kind: InstallKind::MacApp(bundle.clone()),
            file: zip.clone(),
        };
        apply(&staged, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/MacOS/klefki")).unwrap(),
            "new"
        );
        assert!(
            !bundle.join("Contents/stale-file").exists(),
            "the old bundle is gone, not merged"
        );
        let left: Vec<_> = std::fs::read_dir(&apps)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["Klefki.app"], "nothing left next to it");
        assert!(!zip.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_zip_without_klefki_leaves_the_app_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Klefki.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/klefki"), "old").unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("other.txt"), "x").unwrap();
        let zip = dir.path().join("bad.zip");
        let zipped = Command::new("zip")
            .arg("-q")
            .arg(&zip)
            .arg("other.txt")
            .current_dir(&source)
            .status();
        assert!(
            zipped.is_ok_and(|s| s.success()),
            "zip makes the test's download"
        );
        let staged = Staged {
            version: v("0.2.0"),
            kind: InstallKind::MacApp(bundle.clone()),
            file: zip,
        };
        let err = apply(&staged, false).unwrap_err();
        assert!(err.contains("holds no Klefki.app"), "{err}");
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/MacOS/klefki")).unwrap(),
            "old"
        );
    }
}
