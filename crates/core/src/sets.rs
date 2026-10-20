//! Server sets (decision 012): named groups of servers the user turns on together, kept in Klefki's own data folder.
//!
//! A set holds servers by [`ConnectionId`], so it outlives a change of what is shown. Using one is an ordinary
//! change ([`crate::engine::Change::UseSet`]): a Prepare → diff → Apply plan over `disabledMcpServers`. The file is
//! read only when asked for, and read as untrusted, like any file on disk: size-limited, and every name sanitised.
use crate::config::fsx;
use crate::model::ConnectionId;
use crate::sanitize::sanitize_line;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// At most this many sets, and this many servers in one.
pub(crate) const MAX_SETS: usize = 100;
pub(crate) const MAX_MEMBERS: usize = 500;
/// A set's name: at most this many characters.
pub(crate) const MAX_NAME: usize = 64;
/// Never read more than this.
const READ_LIMIT: u64 = 512 * 1024;

/// A named group of servers. The name is sanitised; members are ids [`ConnectionId::new`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSet {
    pub name: String,
    pub members: Vec<ConnectionId>,
}

/// What the Sets page shows ([`crate::api::Event::Sets`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sets {
    /// In the order they were made.
    pub sets: Vec<ServerSet>,
    /// Why the file could not be read in full. What could be read is still listed.
    pub problem: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    sets: Vec<Stored>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    name: String,
    members: Vec<String>,
}

/// Where the sets are kept: beside the backups folder, like the activity log.
pub(crate) fn path(backup_dir: &Path) -> PathBuf {
    backup_dir.with_file_name("sets.json")
}

/// Reads the sets. A missing file is no sets.
pub(crate) fn load(backup_dir: &Path) -> Sets {
    let file = path(backup_dir);
    let text = match fsx::read_limited(&file, READ_LIMIT) {
        Ok(None) => return Sets::default(),
        Ok(Some(text)) => text,
        Err(e) => return unreadable(&file, &e.to_string()),
    };
    let stored: File = match serde_json::from_str(&text) {
        Ok(stored) => stored,
        Err(e) => return unreadable(&file, &e.to_string()),
    };
    let mut sets = Sets::default();
    let mut skipped = 0;
    for set in stored.sets.into_iter().take(MAX_SETS) {
        let Ok(name) = set_name(&set.name) else {
            skipped += 1;
            continue;
        };
        if sets.sets.iter().any(|s| same_name(&s.name, &name)) {
            skipped += 1;
            continue;
        }
        let mut members: Vec<ConnectionId> = Vec::new();
        for id in set.members.iter().take(MAX_MEMBERS) {
            match ConnectionId::from_stored(id) {
                Some(id) if !members.contains(&id) => members.push(id),
                Some(_) => {}
                None => skipped += 1,
            }
        }
        sets.sets.push(ServerSet { name, members });
    }
    if skipped > 0 {
        sets.problem = Some(format!(
            "{} has {skipped} entries Klefki does not understand; they are left out.",
            show(&file)
        ));
    }
    sets
}

fn unreadable(file: &Path, why: &str) -> Sets {
    Sets {
        sets: Vec::new(),
        problem: Some(sanitize_line(&format!(
            "Could not read {}: {why}",
            show(file)
        ))),
    }
}

fn show(path: &Path) -> String {
    sanitize_line(&path.to_string_lossy())
}

/// A set's name as typed, checked: trimmed, not empty, at most [`MAX_NAME`] characters, with nothing a screen could
/// hide or reorder.
pub(crate) fn set_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err("Give the set a name.".to_owned());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("A set's name has at most {MAX_NAME} characters."));
    }
    if sanitize_line(name) != name {
        return Err("A set's name cannot hold control or invisible characters.".to_owned());
    }
    Ok(name.to_owned())
}

/// Names are told apart without regard to case: "Work" and "work" would be one set to anyone reading the list.
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Keeps `name` with `members`, in place of `replaces` when given (a rename or a new list), else as a new set.
/// Answers the sets as kept.
pub(crate) fn save(
    backup_dir: &Path,
    name: &str,
    members: &[ConnectionId],
    replaces: Option<&str>,
) -> Result<Sets, String> {
    let name = set_name(name)?;
    if members.is_empty() {
        return Err("A set needs at least one server.".to_owned());
    }
    if members.len() > MAX_MEMBERS {
        return Err(format!("A set holds at most {MAX_MEMBERS} servers."));
    }
    let mut sets = writable(backup_dir)?;
    let at = match replaces {
        Some(old) => Some(
            sets.iter()
                .position(|s| s.name == old)
                .ok_or_else(|| format!("There is no set named {old} any more."))?,
        ),
        None => None,
    };
    if let Some(other) = sets
        .iter()
        .enumerate()
        .find(|(i, s)| Some(*i) != at && same_name(&s.name, &name))
    {
        return Err(format!("There is a set named {} already.", other.1.name));
    }
    if at.is_none() && sets.len() >= MAX_SETS {
        return Err(format!("Klefki keeps at most {MAX_SETS} sets."));
    }
    let mut kept: Vec<ConnectionId> = Vec::new();
    for id in members {
        if !kept.contains(id) {
            kept.push(id.clone());
        }
    }
    let set = ServerSet {
        name,
        members: kept,
    };
    match at {
        Some(i) => sets[i] = set,
        None => sets.push(set),
    }
    write(backup_dir, &sets)?;
    Ok(Sets {
        sets,
        problem: None,
    })
}

/// Forgets the set `name`. Answers the sets as kept.
pub(crate) fn delete(backup_dir: &Path, name: &str) -> Result<Sets, String> {
    let mut sets = writable(backup_dir)?;
    let before = sets.len();
    sets.retain(|s| s.name != name);
    if sets.len() == before {
        return Err(format!("There is no set named {name} any more."));
    }
    write(backup_dir, &sets)?;
    Ok(Sets {
        sets,
        problem: None,
    })
}

/// The sets to change: never written over when part of the file could not be read, which would lose that part.
fn writable(backup_dir: &Path) -> Result<Vec<ServerSet>, String> {
    let read = load(backup_dir);
    match read.problem {
        Some(problem) => Err(format!(
            "{problem} Klefki changes no set until the file can be read in full."
        )),
        None => Ok(read.sets),
    }
}

fn write(backup_dir: &Path, sets: &[ServerSet]) -> Result<(), String> {
    let file = path(backup_dir);
    let stored = File {
        version: 1,
        sets: sets
            .iter()
            .map(|s| Stored {
                name: s.name.clone(),
                members: s.members.iter().map(|m| m.as_str().to_owned()).collect(),
            })
            .collect(),
    };
    let text = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        crate::config::docstore::create_private_dir(dir)
            .map_err(|e| format!("Could not create {}: {e}", show(dir)))?;
    }
    fsx::write_whole(&file, &text, true).map_err(|e| {
        let why = match e {
            fsx::WriteError::Exists => "it is in the way".to_owned(),
            fsx::WriteError::Refused(why) => why.to_owned(),
            fsx::WriteError::Io(e) => e.to_string(),
        };
        format!("Could not save {}: {why}", show(&file))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Scope;

    fn id(name: &str) -> ConnectionId {
        ConnectionId::new(Scope::User, None, name)
    }

    #[test]
    fn a_set_is_kept_read_back_renamed_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        assert_eq!(load(&backups), Sets::default());
        let local = ConnectionId::new(Scope::Local, Some("/p"), "db");
        save(
            &backups,
            " Work ",
            &[id("gh"), local.clone(), id("gh")],
            None,
        )
        .unwrap();
        let read = load(&backups);
        assert_eq!(read.problem, None);
        assert_eq!(
            read.sets,
            [ServerSet {
                name: "Work".into(),
                members: vec![id("gh"), local.clone()],
            }]
        );
        // A rename keeps its place; another name in another case is the same name.
        save(&backups, "Home", &[id("music")], None).unwrap();
        let err = save(&backups, "home", &[id("x")], Some("Work")).unwrap_err();
        assert!(err.contains("named Home already"), "{err}");
        let sets = save(&backups, "Office", &[id("gh")], Some("Work")).unwrap();
        assert_eq!(
            sets.sets
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Office", "Home"]
        );
        let sets = delete(&backups, "Office").unwrap();
        assert_eq!(sets.sets.len(), 1);
        assert_eq!(load(&backups).sets, sets.sets);
        assert!(delete(&backups, "Office").is_err());
    }

    #[test]
    fn names_and_members_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        for (name, says) in [
            ("  ", "Give the set a name"),
            (&"x".repeat(MAX_NAME + 1)[..], "at most 64"),
            ("a\u{202e}b", "invisible"),
            ("a\nb", "invisible"),
        ] {
            let err = save(&backups, name, &[id("x")], None).unwrap_err();
            assert!(err.contains(says), "{name:?}: {err}");
        }
        assert!(save(&backups, &"x".repeat(MAX_NAME), &[id("x")], None).is_ok());
        assert!(
            save(&backups, "empty", &[], None)
                .unwrap_err()
                .contains("at least one")
        );
    }

    #[test]
    fn a_file_written_by_anyone_is_read_with_care_and_never_written_over_in_part() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let file = path(&backups);
        std::fs::write(
            &file,
            serde_json::json!({"version": 1, "sets": [
                {"name": "Good", "members": [id("a").as_str(), "not an id", id("a").as_str()]},
                {"name": "\u{1b}[31mBad", "members": []},
                {"name": "good", "members": []}
            ]})
            .to_string(),
        )
        .unwrap();
        let read = load(&backups);
        assert_eq!(read.sets.len(), 1);
        assert_eq!(read.sets[0].members, [id("a")]);
        assert!(
            read.problem.as_deref().unwrap().contains("3 entries"),
            "{read:?}"
        );
        // What could not be read would be lost by a write: nothing is changed.
        let err = save(&backups, "New", &[id("b")], None).unwrap_err();
        assert!(err.contains("changes no set"), "{err}");
        std::fs::write(&file, "{").unwrap();
        assert!(
            load(&backups)
                .problem
                .unwrap()
                .starts_with("Could not read")
        );
        std::fs::write(&file, vec![b' '; READ_LIMIT as usize + 1]).unwrap();
        assert!(load(&backups).problem.unwrap().contains("limit"));
    }
}
