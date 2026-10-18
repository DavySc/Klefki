//! Finding the Claude CLI. Only a real executable is accepted: never a `.cmd`, `.bat` or `.ps1` shim, because
//! those would go through a shell (CLAUDE.md invariant: spawn with no shell).
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocateError {
    NotFound,
    /// Only a script shim was found, for example the npm `claude.cmd`.
    UnsupportedShim(PathBuf),
    NotExecutable(PathBuf),
}

impl std::fmt::Display for LocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "the Claude CLI was not found"),
            Self::UnsupportedShim(p) => write!(
                f,
                "{} is a script shim, which Klefki will not run; install the native Claude Code build",
                p.display()
            ),
            Self::NotExecutable(p) => write!(f, "{} is not an executable file", p.display()),
        }
    }
}

/// Where to look. Passed in, so tests never depend on the real environment.
pub(crate) struct Search<'a> {
    /// A path the user configured. If it is set it must be valid; there is no silent fallback.
    pub explicit: Option<&'a Path>,
    pub home: Option<&'a Path>,
    pub path_var: Option<&'a OsStr>,
}

const EXE: &str = if cfg!(windows) {
    "claude.exe"
} else {
    "claude"
};
const SHIM_EXTENSIONS: &[&str] = &["cmd", "bat", "ps1"];

pub(crate) fn locate(search: &Search<'_>) -> Result<PathBuf, LocateError> {
    if let Some(explicit) = search.explicit {
        check(explicit)?;
        return Ok(explicit.to_path_buf());
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = search.home {
        dirs.push(home.join(".local").join("bin"));
    }
    if let Some(path_var) = search.path_var {
        dirs.extend(std::env::split_paths(path_var).filter(|d| is_search_dir(d)));
    }
    for dir in &dirs {
        let candidate = dir.join(EXE);
        if check(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
    // Nothing usable: say so precisely when the only thing around is a script shim.
    for dir in &dirs {
        for ext in SHIM_EXTENSIONS {
            let shim = dir.join(format!("claude.{ext}"));
            if shim.is_file() {
                return Err(LocateError::UnsupportedShim(shim));
            }
        }
    }
    Err(LocateError::NotFound)
}

/// A relative or empty PATH entry means "the current directory" on Windows; never search there.
fn is_search_dir(dir: &Path) -> bool {
    dir.is_absolute()
}

pub(crate) fn check(path: &Path) -> Result<(), LocateError> {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    if ext.as_deref().is_some_and(|e| SHIM_EXTENSIONS.contains(&e)) {
        return Err(LocateError::UnsupportedShim(path.to_path_buf()));
    }
    let meta = std::fs::metadata(path).map_err(|_| LocateError::NotFound)?;
    if !meta.is_file() {
        return Err(LocateError::NotExecutable(path.to_path_buf()));
    }
    #[cfg(windows)]
    if ext.as_deref() != Some("exe") {
        return Err(LocateError::NotExecutable(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(LocateError::NotExecutable(path.to_path_buf()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };

    fn touch_exe(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"stub").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn path_of(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn the_native_install_location_wins_over_path() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let native = touch_exe(&home.join(".local").join("bin"), EXE);
        let other = t.path().join("other");
        touch_exe(&other, EXE);
        let got = locate(&Search {
            explicit: None,
            home: Some(&home),
            path_var: Some(&path_of(&[&other])),
        });
        assert_eq!(got, Ok(native));
    }

    #[test]
    fn falls_back_to_path_and_skips_junk() {
        let t = tempfile::tempdir().unwrap();
        let missing = t.path().join("missing");
        let dir_named_like_it = t.path().join("a");
        std::fs::create_dir_all(dir_named_like_it.join(EXE)).unwrap();
        let good = t.path().join("b");
        let want = touch_exe(&good, EXE);
        let got = locate(&Search {
            explicit: None,
            home: None,
            path_var: Some(&path_of(&[&missing, &dir_named_like_it, &good])),
        });
        assert_eq!(got, Ok(want));
    }

    #[test]
    fn nothing_found_is_not_found() {
        let t = tempfile::tempdir().unwrap();
        let got = locate(&Search {
            explicit: None,
            home: Some(t.path()),
            path_var: Some(&path_of(&[t.path()])),
        });
        assert_eq!(got, Err(LocateError::NotFound));
        let got = locate(&Search {
            explicit: None,
            home: None,
            path_var: None,
        });
        assert_eq!(got, Err(LocateError::NotFound));
    }

    #[test]
    fn script_shims_are_refused_everywhere() {
        let t = tempfile::tempdir().unwrap();
        for shim in ["claude.cmd", "claude.bat", "claude.ps1", "CLAUDE.CMD"] {
            // Explicitly configured.
            let p = touch_exe(t.path(), shim);
            let got = locate(&Search {
                explicit: Some(&p),
                home: None,
                path_var: None,
            });
            assert_eq!(
                got,
                Err(LocateError::UnsupportedShim(p.clone())),
                "explicit {shim}"
            );
        }
        // Only a shim on PATH (the npm install): reported as a shim, not as "not found".
        let npm = t.path().join("npm");
        let shim = touch_exe(&npm, "claude.cmd");
        let got = locate(&Search {
            explicit: None,
            home: None,
            path_var: Some(&path_of(&[&npm])),
        });
        assert_eq!(got, Err(LocateError::UnsupportedShim(shim)));
    }

    #[test]
    fn a_real_executable_later_on_path_beats_an_earlier_shim() {
        let t = tempfile::tempdir().unwrap();
        let npm = t.path().join("npm");
        touch_exe(&npm, "claude.cmd");
        let native = t.path().join("native");
        let want = touch_exe(&native, EXE);
        let got = locate(&Search {
            explicit: None,
            home: None,
            path_var: Some(&path_of(&[&npm, &native])),
        });
        assert_eq!(got, Ok(want));
    }

    #[test]
    fn an_explicit_path_must_be_valid_with_no_silent_fallback() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        touch_exe(&home.join(".local").join("bin"), EXE);
        let missing = t.path().join("nope").join(EXE);
        let got = locate(&Search {
            explicit: Some(&missing),
            home: Some(&home),
            path_var: None,
        });
        assert_eq!(got, Err(LocateError::NotFound));
        let got = locate(&Search {
            explicit: Some(t.path()),
            home: Some(&home),
            path_var: None,
        });
        assert_eq!(got, Err(LocateError::NotExecutable(t.path().to_path_buf())));
        let good = touch_exe(&t.path().join("custom"), EXE);
        let got = locate(&Search {
            explicit: Some(&good),
            home: None,
            path_var: None,
        });
        assert_eq!(got, Ok(good));
    }

    #[test]
    fn relative_and_empty_path_entries_are_not_searched() {
        assert!(!is_search_dir(Path::new("")));
        assert!(!is_search_dir(Path::new(".")));
        assert!(!is_search_dir(Path::new("bin")));
        assert!(!is_search_dir(Path::new("..")));
        assert!(is_search_dir(&std::env::temp_dir()));
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_only_exe_files_are_accepted() {
        let t = tempfile::tempdir().unwrap();
        let p = touch_exe(t.path(), "claude");
        let got = locate(&Search {
            explicit: Some(&p),
            home: None,
            path_var: None,
        });
        assert_eq!(got, Err(LocateError::NotExecutable(p)));
        let p = touch_exe(t.path(), "claude.com");
        let got = locate(&Search {
            explicit: Some(&p),
            home: None,
            path_var: None,
        });
        assert!(matches!(
            got,
            Err(LocateError::NotExecutable(_) | LocateError::UnsupportedShim(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn on_unix_the_file_must_be_executable() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let p = touch_exe(t.path(), EXE);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let got = locate(&Search {
            explicit: Some(&p),
            home: None,
            path_var: None,
        });
        assert_eq!(got, Err(LocateError::NotExecutable(p)));
    }
}
