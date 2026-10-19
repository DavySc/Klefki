//! Import from a file: the system's open dialog, and the file the user picked read as untrusted text, size-limited
//! and never a link or a credential file. What it holds is read as a paste is ([`Pasted`](crate::api::Pasted)), and
//! nothing in it is written until the user has reviewed the plan it makes.
use crate::config::fsx;
use crate::export_file::{Answer, Owner, ShownDialog, UNSEEN};
use crate::paste::MAX_PASTE_BYTES;
use crate::sanitize::shown;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// What came of asking for a file to import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportFile {
    /// The file's text, to read as a paste is read. `shown` is its name, sanitised.
    Read { shown: String, text: String },
    /// It could not be read; `why` is a sentence to show.
    Failed { shown: String, why: String },
    /// The dialog closed without a file.
    Cancelled,
    /// No open dialog could be shown (on Linux: no desktop portal and no zenity): ask for a path instead.
    NoDialog,
}

/// Shows the system's open dialog, over `owner` when there is one. It returns once the dialog is open; the answer
/// comes later.
pub(crate) type PickDialog = Arc<dyn Fn(Option<&dyn Owner>) -> Answer + Send + Sync>;

impl ShownDialog {
    /// Opens the open dialog through `pick`, on the calling thread.
    pub(crate) fn pick(pick: &PickDialog, owner: Option<&dyn Owner>) -> Self {
        let opened = Instant::now();
        Self::new(opened, pick(owner))
    }
}

/// What the answer `picked`, to a dialog opened at `opened`, means: the file to read, or why there is none.
pub(crate) fn answered(opened: Instant, picked: Option<PathBuf>) -> Result<PathBuf, ImportFile> {
    match picked {
        Some(path) => Ok(path),
        None if opened.elapsed() >= UNSEEN => Err(ImportFile::Cancelled),
        None => Err(ImportFile::NoDialog),
    }
}

/// Reads the file at `path` for an import.
pub(crate) fn read(path: &Path) -> ImportFile {
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
    let shown = shown(&name);
    let failed = |why: String| ImportFile::Failed {
        shown: shown.clone(),
        why,
    };
    if fsx::is_denied(&name) {
        return failed(
            "That is Claude Code's sign-in store, which Klefki never reads. Choose an export, or a file of server definitions."
                .to_owned(),
        );
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return failed(format!(
            "{shown} is a link, which Klefki does not follow. Choose the file it leads to."
        ));
    }
    if path.is_dir() {
        return failed(format!("{shown} is a folder. Choose a file."));
    }
    let max = MAX_PASTE_BYTES as u64;
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > max => {
            return failed(format!(
                "{shown} is larger than {} KiB, more than an export of 100 servers takes. Choose an export, or a file of server definitions.",
                max / 1024
            ));
        }
        _ => {}
    }
    match fsx::read_limited_bytes(path, max) {
        Ok(Some(bytes)) => match decode(bytes) {
            Some(text) => ImportFile::Read {
                shown: shown.clone(),
                text,
            },
            None => failed(format!(
                "{shown} is not text that Klefki can read (UTF-8 or UTF-16). Choose an export, or a file of server definitions."
            )),
        },
        // The whole path: a typo in a folder's name shows there.
        Ok(None) => failed(format!(
            "There is no file at {}.",
            crate::sanitize::shown(&path.to_string_lossy())
        )),
        Err(e) => failed(format!(
            "Could not read {shown}: {}.",
            crate::sanitize::shown(&e.to_string())
        )),
    }
}

/// The text of a file: UTF-8, with or without a byte-order mark, or UTF-16 with one (what Windows PowerShell's `>`
/// writes). `None` when it is neither.
fn decode(bytes: Vec<u8>) -> Option<String> {
    let utf16 = |bytes: &[u8], word: fn([u8; 2]) -> u16| {
        let (pairs, rest) = bytes.as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        let words: Vec<u16> = pairs.iter().map(|&pair| word(pair)).collect();
        String::from_utf16(&words).ok()
    };
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => {
            let mut text = String::from_utf8(bytes).ok()?;
            if text.starts_with('\u{feff}') {
                text.drain(..'\u{feff}'.len_utf8());
            }
            Some(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const BLOCK: &str = "{\"mcpServers\": {\"a\": {\"command\": \"node\"}}}";

    fn read_text(path: &Path) -> Result<String, String> {
        match read(path) {
            ImportFile::Read { text, .. } => Ok(text),
            ImportFile::Failed { why, .. } => Err(why),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_export_is_read_as_text_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mcp-servers.json");
        std::fs::write(&file, BLOCK).unwrap();
        assert_eq!(
            read(&file),
            ImportFile::Read {
                shown: "mcp-servers.json".into(),
                text: BLOCK.into()
            }
        );
    }

    #[test]
    fn utf8_with_a_mark_and_utf16_from_powershell_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend(BLOCK.as_bytes());
        std::fs::write(&file, &bom).unwrap();
        assert_eq!(read_text(&file).as_deref(), Ok(BLOCK));
        for (mark, word) in [
            ([0xFF, 0xFE], u16::to_le_bytes as fn(u16) -> [u8; 2]),
            ([0xFE, 0xFF], u16::to_be_bytes),
        ] {
            let mut bytes = mark.to_vec();
            for w in format!("{BLOCK}\r\n").encode_utf16() {
                bytes.extend(word(w));
            }
            std::fs::write(&file, &bytes).unwrap();
            assert_eq!(read_text(&file), Ok(format!("{BLOCK}\r\n")), "{mark:?}");
        }
        // Nearby: half a UTF-16 character, or bytes that are no text, are refused, saying what it reads.
        for bytes in [vec![0xFF, 0xFE, 0x41], vec![0x00, 0x9F, 0x92, 0x96, 0xC3]] {
            std::fs::write(&file, &bytes).unwrap();
            let why = read_text(&file).unwrap_err();
            assert!(why.contains("UTF-8 or UTF-16"), "{why}");
        }
    }

    #[test]
    fn a_file_too_large_a_folder_or_a_missing_file_is_refused_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.json");
        std::fs::write(&big, "x".repeat(MAX_PASTE_BYTES + 1)).unwrap();
        let why = read_text(&big).unwrap_err();
        assert!(why.starts_with("big.json is larger than 256 KiB"), "{why}");
        // Exactly the limit is read.
        std::fs::write(&big, "x".repeat(MAX_PASTE_BYTES)).unwrap();
        assert!(read_text(&big).is_ok());
        let why = read_text(dir.path()).unwrap_err();
        assert!(why.ends_with("is a folder. Choose a file."), "{why}");
        let gone = dir.path().join("gone.json");
        let why = read_text(&gone).unwrap_err();
        assert_eq!(why, format!("There is no file at {}.", gone.display()));
    }

    #[test]
    fn claude_codes_sign_in_store_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            ".credentials.json",
            ".Credentials.JSON",
            ".credentials.json. ",
        ] {
            let file = dir.path().join(name);
            std::fs::write(&file, BLOCK).unwrap();
            let why = read_text(&file).unwrap_err();
            assert!(why.contains("never reads"), "{name}: {why}");
            assert!(!why.contains("mcpServers"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join(".credentials.json");
        std::fs::write(&real, BLOCK).unwrap();
        let link = dir.path().join("servers.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let why = read_text(&link).unwrap_err();
        assert!(why.contains("is a link"), "{why}");
    }

    #[test]
    fn a_name_that_could_disguise_text_is_shown_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a\u{202e}nosj.b");
        std::fs::write(&file, BLOCK).unwrap();
        match read(&file) {
            ImportFile::Read { shown, .. } => assert!(!shown.contains('\u{202e}'), "{shown:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_dialog_that_closes_at_once_was_not_shown() {
        assert_eq!(answered(Instant::now(), None), Err(ImportFile::NoDialog));
        let long_ago = Instant::now() - (UNSEEN + Duration::from_millis(50));
        assert_eq!(answered(long_ago, None), Err(ImportFile::Cancelled));
        assert_eq!(
            answered(Instant::now(), Some(PathBuf::from("/x.json"))),
            Ok(PathBuf::from("/x.json"))
        );
    }
}
