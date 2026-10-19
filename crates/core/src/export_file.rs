//! Export to a file: the block written where the user chose, never over a file unless they said to replace it, and
//! never into Claude Code's own configuration (that changes only through a reviewed plan) or a `.mcp.json` (a
//! project's servers belong to its repository, and Klefki creates nothing in project scope).
use crate::config::ConfigPaths;
use crate::config::fsx::{self, WriteError};
use crate::sanitize::shown;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Why an export was not saved. The text is a sentence to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// A file is there already; ask before replacing it.
    Exists,
    /// Klefki does not write there.
    Refused(String),
    /// Writing failed.
    Failed(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exists => f.write_str("A file with that name is there already."),
            Self::Refused(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

/// What came of asking the user where to save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChoice {
    /// The file the user picked. The system's save dialog has already asked before replacing a file there.
    Chosen(PathBuf),
    /// The user closed the dialog.
    Cancelled,
    /// No save dialog could be shown (on Linux: no desktop portal and no zenity). Ask for a path instead, starting
    /// from `suggested`.
    NoDialog { suggested: PathBuf },
}

/// Shows the system's save dialog, starting with `name` as the file name; the file picked, or `None`.
pub(crate) type ChooseFile = std::sync::Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// A dialog closed sooner than this was never seen: nobody cancels a save dialog this fast, while a system without
/// one answers at once. Neither case says which it was, so the time tells them apart.
const UNSEEN: Duration = Duration::from_millis(150);

/// Asks where to save, through `choose`.
pub(crate) fn choose(choose: &ChooseFile, name: &str, home: Option<&Path>) -> FileChoice {
    let asked = Instant::now();
    match choose(name) {
        Some(path) => FileChoice::Chosen(path),
        None if asked.elapsed() >= UNSEEN => FileChoice::Cancelled,
        None => FileChoice::NoDialog {
            suggested: default_folder(home).join(name),
        },
    }
}

/// Where an export goes when nothing else was picked: the documents folder, or else the home folder.
fn default_folder(home: Option<&Path>) -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.document_dir().map(Path::to_path_buf))
        .or_else(|| home.map(Path::to_path_buf))
        .unwrap_or_default()
}

/// The file name to start from: the server's name for one, `mcp-servers.json` for more.
pub fn file_name(servers: &[&str]) -> String {
    match servers {
        [one] => format!("{one}-mcp.json"),
        _ => "mcp-servers.json".to_owned(),
    }
}

/// Writes the block `text` (and a final line break) to `path`. An existing file is replaced only with `replace`.
pub(crate) fn save(
    paths: Option<&ConfigPaths>,
    path: &Path,
    text: &str,
    replace: bool,
) -> Result<(), SaveError> {
    refused(paths, path).map_or(Ok(()), |why| Err(SaveError::Refused(why.to_owned())))?;
    let folder = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if !folder.is_dir() {
        return Err(SaveError::Failed(format!(
            "The folder {} does not exist.",
            shown(&folder.to_string_lossy())
        )));
    }
    match fsx::write_whole(path, &format!("{text}\n"), replace) {
        Ok(()) => Ok(()),
        Err(WriteError::Exists) => Err(SaveError::Exists),
        Err(WriteError::Refused(why)) => Err(SaveError::Refused(format!(
            "Klefki does not write there: {why}."
        ))),
        Err(WriteError::Io(e)) => Err(SaveError::Failed(format!(
            "Could not write the file: {}.",
            shown(&e.to_string())
        ))),
    }
}

const CLAUDES_OWN: &str = "That is Claude Code's own configuration, which Klefki changes only after showing the change. Save the export somewhere else.";
const PROJECT_FILE: &str = "Klefki does not write .mcp.json files: a project's servers belong to its repository. Save the export under another name and copy it there yourself.";

/// Why `path` is not a place for an export, if it is not.
fn refused(paths: Option<&ConfigPaths>, path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy();
    // Compared as Windows and macOS open files: any case, without trailing dots and spaces.
    let name = name.trim_end_matches(['.', ' ']).to_lowercase();
    if name == ".mcp.json" {
        return Some(PROJECT_FILE);
    }
    if name == ".claude.json" || name.starts_with(".claude.json.") {
        return Some(CLAUDES_OWN);
    }
    let folder = path.parent().filter(|d| !d.as_os_str().is_empty())?;
    let folder = folder.canonicalize().ok()?;
    // A project's `.claude` folder holds its settings; Claude Code's own folder holds the rest.
    let in_claude_folder = folder.ancestors().any(|dir| {
        dir.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(".claude"))
    });
    let in_config = paths.is_some_and(|p| {
        p.claude_dir
            .canonicalize()
            .is_ok_and(|dir| folder.starts_with(dir))
    });
    (in_claude_folder || in_config).then_some(CLAUDES_OWN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const BLOCK: &str = "{\n  \"mcpServers\": {}\n}";

    fn paths(dir: &Path) -> ConfigPaths {
        let claude_dir = dir.join("config");
        std::fs::create_dir_all(&claude_dir).unwrap();
        ConfigPaths {
            claude_json: claude_dir.join(".claude.json"),
            claude_dir,
        }
    }

    /// The names of the files in `dir`, sorted: nothing else (no temporary file) is left behind.
    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn saves_the_block_with_a_final_line_break() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        assert_eq!(save(None, &file, BLOCK, false), Ok(()));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{BLOCK}\n")
        );
        assert_eq!(files(dir.path()), ["servers.json"]);
    }

    #[test]
    fn an_existing_file_is_kept_unless_replacing_was_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        std::fs::write(&file, "mine").unwrap();
        assert_eq!(save(None, &file, BLOCK, false), Err(SaveError::Exists));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
        assert_eq!(
            files(dir.path()),
            ["servers.json"],
            "no temporary file left"
        );

        assert_eq!(save(None, &file, BLOCK, true), Ok(()));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{BLOCK}\n")
        );
        assert_eq!(files(dir.path()), ["servers.json"]);
    }

    #[test]
    fn claude_codes_own_files_and_project_files_are_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let project = dir.path().join("repo");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(paths.claude_dir.join("deeper")).unwrap();
        let cases = [
            (project.join(".mcp.json"), PROJECT_FILE),
            (project.join(".MCP.json"), PROJECT_FILE),
            (project.join(".mcp.json. "), PROJECT_FILE),
            (dir.path().join(".claude.json"), CLAUDES_OWN),
            (dir.path().join(".Claude.json.backup"), CLAUDES_OWN),
            (
                project.join(".claude").join("settings.local.json"),
                CLAUDES_OWN,
            ),
            (project.join(".claude").join("servers.json"), CLAUDES_OWN),
            (paths.claude_dir.join("servers.json"), CLAUDES_OWN),
            (paths.claude_dir.join("deeper").join("x.json"), CLAUDES_OWN),
        ];
        for (file, why) in cases {
            for replace in [false, true] {
                assert_eq!(
                    save(Some(&paths), &file, BLOCK, replace),
                    Err(SaveError::Refused(why.to_owned())),
                    "{file:?}"
                );
            }
            assert!(!file.exists(), "{file:?} was written");
        }
        // Beside them, under another name, is fine.
        assert_eq!(
            save(Some(&paths), &project.join("servers.json"), BLOCK, false),
            Ok(())
        );
    }

    #[test]
    fn credential_files_folders_and_missing_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            save(None, &dir.path().join(".credentials.json"), BLOCK, true),
            Err(SaveError::Refused(_))
        ));
        std::fs::create_dir(dir.path().join("taken")).unwrap();
        assert!(matches!(
            save(None, &dir.path().join("taken"), BLOCK, true),
            Err(SaveError::Refused(why)) if why.contains("folder")
        ));
        match save(None, &dir.path().join("gone").join("x.json"), BLOCK, false) {
            Err(SaveError::Failed(why)) => assert!(why.contains("does not exist"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(files(dir.path()), ["taken"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_neither_followed_nor_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "kept").unwrap();
        let link = dir.path().join("servers.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        for replace in [false, true] {
            assert!(matches!(
                save(None, &link, BLOCK, replace),
                Err(SaveError::Refused(why)) if why.contains("link")
            ));
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_dialog_that_closes_at_once_was_not_shown_and_a_path_is_suggested() {
        let home = tempfile::tempdir().unwrap();
        let quick: ChooseFile = Arc::new(|_| None);
        match choose(&quick, "mcp-servers.json", Some(home.path())) {
            FileChoice::NoDialog { suggested } => {
                assert!(suggested.is_absolute(), "{suggested:?}");
                assert!(suggested.ends_with("mcp-servers.json"), "{suggested:?}");
            }
            other => panic!("{other:?}"),
        }
        let slow: ChooseFile = Arc::new(|_| {
            std::thread::sleep(UNSEEN + Duration::from_millis(50));
            None
        });
        assert_eq!(choose(&slow, "x.json", None), FileChoice::Cancelled);
        let picked: ChooseFile = Arc::new(|name| Some(PathBuf::from("/picked").join(name)));
        assert_eq!(
            choose(&picked, "x.json", None),
            FileChoice::Chosen(PathBuf::from("/picked/x.json"))
        );
    }

    #[test]
    fn the_file_name_says_what_it_holds() {
        assert_eq!(file_name(&["github"]), "github-mcp.json");
        assert_eq!(file_name(&["a", "b"]), "mcp-servers.json");
        assert_eq!(file_name(&[]), "mcp-servers.json");
    }
}
