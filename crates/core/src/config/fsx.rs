//! The single file-access layer. Reads are size-limited and refuse symlinks and secret stores.
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const DENIED_FILE_NAMES: &[&str] = &[".credentials.json"];

/// Compared the way Windows and macOS open files: any case, without a `:stream` suffix or trailing dots and spaces.
fn is_denied(name: &str) -> bool {
    let name = name.split(':').next().unwrap_or(name);
    let name = name.trim_end_matches(['.', ' ']).to_lowercase();
    DENIED_FILE_NAMES.contains(&name.as_str())
}

/// Reads a UTF-8 text file of at most `max_bytes`. `Ok(None)` when the file does not exist.
pub(crate) fn read_limited(path: &Path, max_bytes: u64) -> io::Result<Option<String>> {
    Ok(read_limited_bom(path, max_bytes)?.map(|(text, _)| text))
}

/// Like [`read_limited`], and also says whether the file started with a byte-order mark, so a writer can keep it.
pub(crate) fn read_limited_bom(path: &Path, max_bytes: u64) -> io::Result<Option<(String, bool)>> {
    if path
        .file_name()
        .is_some_and(|n| is_denied(&n.to_string_lossy()))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reading credential files is not allowed",
        ));
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Err(io::Error::other("refusing to follow a symlink"));
    }
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    if meta.len() > max_bytes {
        return Err(io::Error::other(format!(
            "file is larger than the {max_bytes} byte limit"
        )));
    }
    // Sized from the metadata (plus one byte, to notice a file that grew meanwhile), so a large file is read
    // into one allocation rather than a series of growing ones.
    let mut buf = Vec::with_capacity(meta.len() as usize + 1);
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(io::Error::other(
            "file grew past the size limit while reading",
        ));
    }
    let text = String::from_utf8(buf).map_err(|_| io::Error::other("file is not valid UTF-8"))?;
    Ok(Some(match text.strip_prefix('\u{feff}') {
        Some(rest) => (rest.to_owned(), true),
        None => (text, false),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_limited(&dir.path().join("nope.json"), 10).unwrap(),
            None
        );
    }

    #[test]
    fn reads_within_limit_and_strips_bom() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        std::fs::write(&f, "\u{feff}{}").unwrap();
        assert_eq!(read_limited(&f, 10).unwrap().as_deref(), Some("{}"));
    }

    #[test]
    fn rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big.json");
        std::fs::write(&f, "x".repeat(11)).unwrap();
        assert!(read_limited(&f, 10).is_err());
    }

    #[test]
    fn rejects_invalid_utf8_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("bin.json");
        std::fs::write(&f, [0xff, 0xfe, 0xfd]).unwrap();
        assert!(read_limited(&f, 10).is_err());
        assert!(read_limited(dir.path(), 10).is_err());
    }

    #[test]
    fn never_reads_the_credentials_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join(".credentials.json");
        std::fs::write(&f, "{}").unwrap();
        let err = read_limited(&f, 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // Also when it does not exist: the refusal must not depend on presence.
        let missing = dir.path().join("sub").join(".credentials.json");
        assert_eq!(
            read_limited(&missing, 10).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn other_spellings_of_the_credentials_file_are_refused_too() {
        // Windows and macOS ignore case; Windows also drops trailing dots and spaces and opens `name::$DATA`.
        let dir = tempfile::tempdir().unwrap();
        for name in [
            ".Credentials.JSON",
            ".credentials.json.",
            ".credentials.json ",
            ".credentials.json::$DATA",
        ] {
            assert_eq!(
                read_limited(&dir.path().join(name), 10).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied,
                "{name:?}"
            );
        }
        // A file that merely starts the same way is fine.
        let other = dir.path().join(".credentials.json.bak");
        std::fs::write(&other, "{}").unwrap();
        assert!(read_limited(&other, 10).is_ok());
    }
}
