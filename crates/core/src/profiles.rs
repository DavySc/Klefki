//! Profiles (decision 012): named groups of servers the user turns on together, kept in Klefki's own data folder.
//!
//! A profile holds servers by [`ConnectionId`], so it outlives a change of what is shown. Using one is an ordinary
//! change ([`crate::engine::Change::UseProfile`]): a Prepare → diff → Apply plan over `disabledMcpServers`. The file
//! is read only when asked for, and read as untrusted, like any file on disk: size-limited, and every name sanitised.
//! It is written only when it could be read in full, so a write never loses what a read left out.
use crate::config::fsx;
use crate::model::ConnectionId;
use crate::sanitize::sanitize_line;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// At most this many profiles, and this many servers in one.
pub(crate) const MAX_PROFILES: usize = 100;
pub(crate) const MAX_MEMBERS: usize = 500;
/// A profile's name: at most this many characters.
pub(crate) const MAX_NAME: usize = 64;
/// Never read more than this.
const READ_LIMIT: u64 = 512 * 1024;
/// The file format this Klefki reads and writes. A file of another version is read where it can be, never written.
const VERSION: u32 = 1;

/// A named group of servers. The name is sanitised; members are ids [`ConnectionId::new`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub members: Vec<ConnectionId>,
}

/// What the Profiles page shows ([`crate::api::Event::Profiles`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profiles {
    /// In the order they were made.
    pub profiles: Vec<Profile>,
    /// Why the file could not be read in full, so nothing in it is changed. What could be read is still listed.
    pub problem: Option<String>,
}

type Other = serde_json::Map<String, serde_json::Value>;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    profiles: Vec<Stored>,
    /// Anything else: kept only to notice it, since a write would lose it.
    #[serde(flatten, skip_serializing)]
    other: Other,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    name: String,
    members: Vec<String>,
    #[serde(flatten, skip_serializing)]
    other: Other,
}

/// Where the profiles are kept: beside the backups folder, like the activity log.
pub(crate) fn path(backup_dir: &Path) -> PathBuf {
    backup_dir.with_file_name("profiles.json")
}

/// Reads the profiles. A missing file is no profiles.
pub(crate) fn load(backup_dir: &Path) -> Profiles {
    let file = path(backup_dir);
    let text = match fsx::read_limited(&file, READ_LIMIT) {
        Ok(None) => return Profiles::default(),
        Ok(Some(text)) => text,
        Err(e) => return unreadable(&file, &e.to_string()),
    };
    let stored: File = match serde_json::from_str(&text) {
        Ok(stored) => stored,
        Err(e) => return unreadable(&file, &e.to_string()),
    };
    let mut read = Profiles::default();
    // Entries left out, which a write would lose.
    let mut skipped = stored.other.len();
    let over = stored.profiles.len().saturating_sub(MAX_PROFILES);
    for profile in stored.profiles.into_iter().take(MAX_PROFILES) {
        skipped += profile.other.len();
        let Ok(name) = profile_name(&profile.name) else {
            skipped += 1;
            continue;
        };
        if read.profiles.iter().any(|s| same_name(&s.name, &name)) {
            skipped += 1;
            continue;
        }
        skipped += profile.members.len().saturating_sub(MAX_MEMBERS);
        let mut members: Vec<ConnectionId> = Vec::new();
        for id in profile.members.iter().take(MAX_MEMBERS) {
            match ConnectionId::from_stored(id) {
                Some(id) if !members.contains(&id) => members.push(id),
                Some(_) => {}
                None => skipped += 1,
            }
        }
        read.profiles.push(Profile { name, members });
    }
    let shown = show(&file);
    let mut problems = Vec::new();
    if stored.version != VERSION {
        problems.push(format!(
            "{shown} is in a format this Klefki does not know (version {}), perhaps from a newer Klefki.",
            stored.version
        ));
    }
    if over > 0 {
        problems.push(format!(
            "{shown} has more than {MAX_PROFILES} profiles; {over} more are not shown."
        ));
    }
    if skipped > 0 {
        problems.push(format!(
            "{shown} has {skipped} entries Klefki does not understand; they are left out."
        ));
    }
    if !problems.is_empty() {
        read.problem = Some(problems.join(" "));
    }
    read
}

fn unreadable(file: &Path, why: &str) -> Profiles {
    Profiles {
        profiles: Vec::new(),
        problem: Some(sanitize_line(&format!(
            "Could not read {}: {why}",
            show(file)
        ))),
    }
}

fn show(path: &Path) -> String {
    sanitize_line(&path.to_string_lossy())
}

/// A profile's name as typed, checked: trimmed, not empty, at most [`MAX_NAME`] characters, with nothing a screen
/// could hide or reorder.
pub(crate) fn profile_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err("Give the profile a name.".to_owned());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!(
            "A profile's name has at most {MAX_NAME} characters."
        ));
    }
    if sanitize_line(name) != name {
        return Err("A profile's name cannot hold control or invisible characters.".to_owned());
    }
    Ok(name.to_owned())
}

/// Names are told apart without regard to case: "Work" and "work" would be one profile to anyone reading the list.
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Keeps `name` with `members`, in place of `replaces` when given (a rename or a new list), else as a new profile.
/// Answers the profiles as kept.
pub(crate) fn save(
    backup_dir: &Path,
    name: &str,
    members: &[ConnectionId],
    replaces: Option<&str>,
) -> Result<Profiles, String> {
    let name = profile_name(name)?;
    if members.is_empty() {
        return Err("A profile needs at least one server.".to_owned());
    }
    if members.len() > MAX_MEMBERS {
        return Err(format!("A profile holds at most {MAX_MEMBERS} servers."));
    }
    let mut profiles = writable(backup_dir)?;
    let at = match replaces {
        Some(old) => Some(
            profiles
                .iter()
                .position(|s| s.name == old)
                .ok_or_else(|| format!("There is no profile named {old} any more."))?,
        ),
        None => None,
    };
    if let Some(other) = profiles
        .iter()
        .enumerate()
        .find(|(i, s)| Some(*i) != at && same_name(&s.name, &name))
    {
        return Err(format!(
            "There is a profile named {} already.",
            other.1.name
        ));
    }
    if at.is_none() && profiles.len() >= MAX_PROFILES {
        return Err(format!("Klefki keeps at most {MAX_PROFILES} profiles."));
    }
    let mut kept: Vec<ConnectionId> = Vec::new();
    for id in members {
        if !kept.contains(id) {
            kept.push(id.clone());
        }
    }
    let profile = Profile {
        name,
        members: kept,
    };
    match at {
        Some(i) => profiles[i] = profile,
        None => profiles.push(profile),
    }
    write(backup_dir, &profiles)?;
    Ok(Profiles {
        profiles,
        problem: None,
    })
}

/// Forgets the profile `name`. Answers the profiles as kept.
pub(crate) fn delete(backup_dir: &Path, name: &str) -> Result<Profiles, String> {
    let mut profiles = writable(backup_dir)?;
    let before = profiles.len();
    profiles.retain(|s| s.name != name);
    if profiles.len() == before {
        return Err(format!("There is no profile named {name} any more."));
    }
    write(backup_dir, &profiles)?;
    Ok(Profiles {
        profiles,
        problem: None,
    })
}

/// The names of the profiles that hold `id`. Empty when there are none, or the file cannot be read.
pub(crate) fn holding(backup_dir: &Path, id: &ConnectionId) -> Vec<String> {
    load(backup_dir)
        .profiles
        .into_iter()
        .filter(|p| p.members.contains(id))
        .map(|p| p.name)
        .collect()
}

/// A server was renamed or moved: each profile that held it, by its `old` id, holds it by its `new` one, so it stays
/// in its profiles. Answers the profiles changed; nothing when no profile held it.
pub(crate) fn follow(
    backup_dir: &Path,
    moved: &[(ConnectionId, ConnectionId)],
) -> Result<Vec<String>, String> {
    let read = load(backup_dir);
    let touches = |p: &Profile| moved.iter().any(|(old, _)| p.members.contains(old));
    if !read.profiles.iter().any(touches) {
        return Ok(Vec::new());
    }
    let mut profiles = writable(backup_dir)?;
    let mut changed = Vec::new();
    for profile in &mut profiles {
        let mut members: Vec<ConnectionId> = Vec::new();
        for id in &profile.members {
            let now = moved
                .iter()
                .find(|(old, _)| old == id)
                .map_or(id, |(_, new)| new);
            if !members.contains(now) {
                members.push(now.clone());
            }
        }
        if members != profile.members {
            profile.members = members;
            changed.push(profile.name.clone());
        }
    }
    write(backup_dir, &profiles)?;
    Ok(changed)
}

/// The profiles to change: never written over when part of the file could not be read, which would lose that part.
fn writable(backup_dir: &Path) -> Result<Vec<Profile>, String> {
    let read = load(backup_dir);
    match read.problem {
        Some(problem) => Err(format!(
            "{problem} Klefki changes no profile until the file can be read in full."
        )),
        None => Ok(read.profiles),
    }
}

fn write(backup_dir: &Path, profiles: &[Profile]) -> Result<(), String> {
    let file = path(backup_dir);
    let stored = File {
        version: VERSION,
        profiles: profiles
            .iter()
            .map(|s| Stored {
                name: s.name.clone(),
                members: s.members.iter().map(|m| m.as_str().to_owned()).collect(),
                other: Other::new(),
            })
            .collect(),
        other: Other::new(),
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
    fn a_profile_is_kept_read_back_renamed_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        assert_eq!(load(&backups), Profiles::default());
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
            read.profiles,
            [Profile {
                name: "Work".into(),
                members: vec![id("gh"), local.clone()],
            }]
        );
        // A rename keeps its place; another name in another case is the same name.
        save(&backups, "Home", &[id("music")], None).unwrap();
        let err = save(&backups, "home", &[id("x")], Some("Work")).unwrap_err();
        assert!(err.contains("named Home already"), "{err}");
        let kept = save(&backups, "Office", &[id("gh")], Some("Work")).unwrap();
        assert_eq!(
            kept.profiles
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Office", "Home"]
        );
        let kept = delete(&backups, "Office").unwrap();
        assert_eq!(kept.profiles.len(), 1);
        assert_eq!(load(&backups).profiles, kept.profiles);
        assert!(delete(&backups, "Office").is_err());
    }

    #[test]
    fn names_and_members_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        for (name, says) in [
            ("  ", "Give the profile a name"),
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
            serde_json::json!({"version": 1, "profiles": [
                {"name": "Good", "members": [id("a").as_str(), "not an id", id("a").as_str()]},
                {"name": "\u{1b}[31mBad", "members": []},
                {"name": "good", "members": []}
            ]})
            .to_string(),
        )
        .unwrap();
        let read = load(&backups);
        assert_eq!(read.profiles.len(), 1);
        assert_eq!(read.profiles[0].members, [id("a")]);
        assert!(
            read.problem.as_deref().unwrap().contains("3 entries"),
            "{read:?}"
        );
        // What could not be read would be lost by a write: nothing is changed.
        let err = save(&backups, "New", &[id("b")], None).unwrap_err();
        assert!(err.contains("changes no profile"), "{err}");
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

    /// What a read leaves out is never lost by a write: more profiles or members than shown, a newer format, and
    /// fields Klefki does not know (found in review, PR #57).
    #[test]
    fn a_file_read_in_part_is_never_written_over() {
        let x = || id("x");
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let file = path(&backups);
        let many: Vec<_> = (0..MAX_PROFILES + 5)
            .map(|i| serde_json::json!({"name": format!("s{i}"), "members": [x().as_str()]}))
            .collect();
        let big: Vec<String> = (0..MAX_MEMBERS + 5)
            .map(|i| id(&format!("m{i}")).as_str().to_owned())
            .collect();
        for (case, text, says) in [
            (
                "more profiles than shown",
                serde_json::json!({"version": 1, "profiles": many}).to_string(),
                "5 more are not shown",
            ),
            (
                "more members than shown",
                serde_json::json!({"version": 1, "profiles": [{"name": "Big", "members": big}]})
                    .to_string(),
                "5 entries",
            ),
            (
                "a newer version",
                serde_json::json!({"version": 2, "profiles": [{"name": "A", "members": [x().as_str()]}]})
                    .to_string(),
                "version 2",
            ),
            (
                "an unknown field in a profile",
                serde_json::json!({"version": 1, "profiles": [{"name": "A", "members": [], "colour": "red"}]})
                    .to_string(),
                "1 entries",
            ),
            (
                "an unknown field at the top",
                serde_json::json!({"version": 1, "profiles": [], "active": "A"}).to_string(),
                "1 entries",
            ),
        ] {
            std::fs::write(&file, &text).unwrap();
            let problem = load(&backups).problem.unwrap_or_default();
            assert!(problem.contains(says), "{case}: {problem}");
            for err in [
                save(&backups, "Other", &[x()], None).unwrap_err(),
                delete(&backups, "s0").unwrap_err(),
            ] {
                assert!(err.contains("changes no profile"), "{case}: {err}");
            }
            // Following a rename refuses too where a profile holds the server, and is a no-op where none does.
            assert!(
                follow(&backups, &[(x(), id("y"))]).is_err_and(|e| e.contains("changes no profile"))
                    || holding(&backups, &x()).is_empty(),
                "{case}"
            );
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "{case}");
        }
    }

    #[test]
    fn a_renamed_or_moved_server_stays_in_its_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let local = ConnectionId::new(Scope::Local, Some("/p"), "gh");
        save(&backups, "Work", &[id("gh"), id("db")], None).unwrap();
        save(&backups, "Home", &[id("music")], None).unwrap();
        save(&backups, "Both", &[id("gh"), local.clone()], None).unwrap();
        assert_eq!(holding(&backups, &id("gh")), ["Work", "Both"]);
        // Moving the user gh to the local gh of /p: Both held both, and keeps one.
        let changed = follow(&backups, &[(id("gh"), local.clone())]).unwrap();
        assert_eq!(changed, ["Work", "Both"]);
        let read = load(&backups);
        assert_eq!(read.profiles[0].members, [local.clone(), id("db")]);
        assert_eq!(read.profiles[1].members, [id("music")]);
        assert_eq!(read.profiles[2].members, [local]);
        // A server in no profile changes nothing, not even the file.
        let before = std::fs::read_to_string(path(&backups)).unwrap();
        assert!(
            follow(&backups, &[(id("zzz"), id("y"))])
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read_to_string(path(&backups)).unwrap(), before);
    }
}
