//! What each use of a profile switched (decision 012, "Stop using"), so that stopping or deleting the profile can
//! put those switches back.
//!
//! A use records each `disabledMcpServers` switch it flipped, by project and name as the file spells them, with the
//! state it set: the state before is the other one. It also shares a switch it found already set its way by another
//! profile, so stopping one of the two leaves it as the other wants it. Nothing here is ever believed over Claude
//! Code's files: a switch is put back only while it is still as the profile left it.
//!
//! Klefki's own data, kept in `profile-uses.json` beside `profiles.json` and written the same way (owner-approved,
//! 2026-10-20): read as untrusted (size-limited, every string checked), written whole and owner only, and only when
//! it could be read in full.
use crate::config::fsx;
use crate::sanitize::sanitize_line;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// At most this many switches per profile: every project Claude Code knows, times its servers, fits.
pub(crate) const MAX_SWITCHES: usize = 20_000;
/// A project path or server name longer than this is not one Klefki wrote.
const MAX_TEXT: usize = 4096;
/// Never read more than this.
const READ_LIMIT: u64 = 4 * 1024 * 1024;
const VERSION: u32 = 1;

/// One switch a profile set: `name` in `project`, turned off (`off`) or on. The state before was the other one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    /// As [`crate::api::Snapshot::projects`] spells it.
    pub project: String,
    /// As `disabledMcpServers` spells it.
    pub name: String,
    pub off: bool,
}

/// What one profile switched, over all its uses not stopped yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileUse {
    pub profile: String,
    pub switches: Vec<Switch>,
}

/// Every profile's uses, read with each snapshot ([`crate::api::Snapshot::profile_uses`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileUses {
    pub uses: Vec<ProfileUse>,
    /// Why the file could not be read in full, so nothing in it is changed. What could be read is still listed.
    pub problem: Option<String>,
}

impl ProfileUses {
    /// The switches `profile` set, if it has any.
    pub fn of(&self, profile: &str) -> &[Switch] {
        self.uses
            .iter()
            .find(|u| u.profile == profile)
            .map_or(&[], |u| u.switches.as_slice())
    }

    /// The other profiles that set `name` in `project` to `off`.
    pub(crate) fn others_setting<'a>(
        &'a self,
        profile: &'a str,
        project: &'a str,
        name: &'a str,
        off: bool,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.uses
            .iter()
            .filter(move |u| u.profile != profile)
            .filter(move |u| {
                u.switches
                    .iter()
                    .any(|s| s.project == project && s.name == name && s.off == off)
            })
            .map(|u| u.profile.as_str())
    }
}

type Other = serde_json::Map<String, serde_json::Value>;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    uses: Vec<StoredUse>,
    #[serde(flatten, skip_serializing)]
    other: Other,
}

#[derive(Serialize, Deserialize)]
struct StoredUse {
    profile: String,
    switches: Vec<StoredSwitch>,
    #[serde(flatten, skip_serializing)]
    other: Other,
}

#[derive(Serialize, Deserialize)]
struct StoredSwitch {
    project: String,
    name: String,
    off: bool,
    #[serde(flatten, skip_serializing)]
    other: Other,
}

/// Beside `profiles.json`.
pub(crate) fn path(backup_dir: &Path) -> PathBuf {
    backup_dir.with_file_name("profile-uses.json")
}

/// Reads the uses. A missing file is none.
pub(crate) fn load(backup_dir: &Path) -> ProfileUses {
    let file = path(backup_dir);
    let text = match fsx::read_limited(&file, READ_LIMIT) {
        Ok(None) => return ProfileUses::default(),
        Ok(Some(text)) => text,
        Err(e) => return unreadable(&file, &e.to_string()),
    };
    let stored: File = match serde_json::from_str(&text) {
        Ok(stored) => stored,
        Err(e) => return unreadable(&file, &e.to_string()),
    };
    let mut read = ProfileUses::default();
    let mut skipped = stored.other.len();
    let over = stored
        .uses
        .len()
        .saturating_sub(crate::profiles::MAX_PROFILES);
    for used in stored.uses.into_iter().take(crate::profiles::MAX_PROFILES) {
        skipped += used.other.len();
        let Ok(profile) = crate::profiles::profile_name(&used.profile) else {
            skipped += 1;
            continue;
        };
        if read.uses.iter().any(|u| u.profile == profile) {
            skipped += 1;
            continue;
        }
        skipped += used.switches.len().saturating_sub(MAX_SWITCHES);
        let mut switches: Vec<Switch> = Vec::new();
        // Read with every snapshot: each switch is looked up once, not against all before it.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for s in used.switches.into_iter().take(MAX_SWITCHES) {
            skipped += s.other.len();
            if !plain(&s.project) || !plain(&s.name) {
                skipped += 1;
                continue;
            }
            if !seen.insert((s.project.clone(), s.name.clone())) {
                skipped += 1;
                continue;
            }
            switches.push(Switch {
                project: s.project,
                name: s.name,
                off: s.off,
            });
        }
        read.uses.push(ProfileUse { profile, switches });
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
            "{shown} has more than {} entries; {over} more are not read.",
            crate::profiles::MAX_PROFILES
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

/// Not empty, not too long, and nothing a screen hides: a project path or a server name, as Klefki writes them.
fn plain(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT && sanitize_line(text) == text
}

fn unreadable(file: &Path, why: &str) -> ProfileUses {
    ProfileUses {
        uses: Vec::new(),
        problem: Some(sanitize_line(&format!(
            "Could not read {}: {why}",
            show(file)
        ))),
    }
}

fn show(path: &Path) -> String {
    sanitize_line(&path.to_string_lossy())
}

/// A use of `profile` was saved: it flipped each of `flipped`, and found each of `shared` already set its way by
/// another profile. A switch it sets back to what an earlier use of it found is no longer its doing.
pub(crate) fn record(
    backup_dir: &Path,
    profile: &str,
    flipped: &[Switch],
    shared: &[Switch],
) -> Result<bool, String> {
    let read = load(backup_dir);
    let shares = |s: &&Switch| {
        read.others_setting(profile, &s.project, &s.name, s.off)
            .next()
            .is_some()
    };
    if flipped.is_empty() && !shared.iter().any(|s| shares(&s)) {
        return Ok(false);
    }
    let mut uses = writable(backup_dir)?;
    let at = match uses.uses.iter().position(|u| u.profile == profile) {
        Some(at) => at,
        None => {
            uses.uses.push(ProfileUse {
                profile: profile.to_owned(),
                switches: Vec::new(),
            });
            uses.uses.len() - 1
        }
    };
    // In order, with a hole where a switch is forgotten; each found by its place, not by a search.
    let mut mine: Vec<Option<Switch>> = std::mem::take(&mut uses.uses[at].switches)
        .into_iter()
        .map(Some)
        .collect();
    let mut at_key: HashMap<(String, String), usize> = mine
        .iter()
        .enumerate()
        .filter_map(|(i, k)| k.as_ref().map(|k| ((k.project.clone(), k.name.clone()), i)))
        .collect();
    for s in flipped {
        let key = (s.project.clone(), s.name.clone());
        match at_key.get(&key).copied() {
            // Set its way again after something else changed it: the state before is still the one it found first.
            Some(i) if mine[i].as_ref().is_some_and(|k| k.off == s.off) => {}
            // Back to what its first use found: nothing of its doing is left there.
            Some(i) => {
                mine[i] = None;
                at_key.remove(&key);
            }
            None => {
                at_key.insert(key, mine.len());
                mine.push(Some(s.clone()));
            }
        }
    }
    for s in shared {
        let key = (s.project.clone(), s.name.clone());
        if shares(&s) && !at_key.contains_key(&key) {
            at_key.insert(key, mine.len());
            mine.push(Some(s.clone()));
        }
    }
    let mine: Vec<Switch> = mine.into_iter().flatten().collect();
    uses.uses[at].switches = mine;
    let mine = &uses.uses[at].switches;
    if mine.len() > MAX_SWITCHES {
        return Err(format!(
            "Klefki remembers at most {MAX_SWITCHES} switches per profile."
        ));
    }
    uses.uses.retain(|u| !u.switches.is_empty());
    write(backup_dir, &uses.uses).map(|()| true)
}

/// `profile` is no longer used in `project` (in every project when `None`): its switches there are forgotten, and
/// each of `handed`, another profile's switch that is now back to the state before both, is forgotten too.
pub(crate) fn stop(
    backup_dir: &Path,
    profile: &str,
    project: Option<&str>,
    handed: &[(String, Switch)],
) -> Result<(), String> {
    let mut uses = writable(backup_dir)?;
    for used in &mut uses.uses {
        if used.profile == profile {
            used.switches
                .retain(|s| project.is_some_and(|p| p != s.project));
        }
        used.switches.retain(|s| {
            !handed
                .iter()
                .any(|(owner, h)| *owner == used.profile && h == s)
        });
    }
    uses.uses.retain(|u| !u.switches.is_empty());
    write(backup_dir, &uses.uses)
}

/// A profile was saved as `new`, renamed from `old` when given. A record left under `new` (by a forget that failed,
/// an older Klefki, or `profiles.json` changed by hand) belongs to no profile that was used: it is dropped, so the
/// profile does not take over switches it never made and the file never holds a name twice. Then `old`'s record is
/// `new`'s. Nothing to do when neither has one.
pub(crate) fn claim_name(backup_dir: &Path, old: Option<&str>, new: &str) -> Result<bool, String> {
    let renamed = old.filter(|old| *old != new);
    let read = load(backup_dir);
    let leftover = old != Some(new)
        && read.uses.iter().any(|u| {
            crate::profiles::same_name(&u.profile, new) && Some(u.profile.as_str()) != old
        });
    let moves = renamed.is_some_and(|old| !read.of(old).is_empty());
    if !leftover && !moves {
        return Ok(false);
    }
    let mut uses = writable(backup_dir)?;
    uses.uses.retain(|u| {
        !crate::profiles::same_name(&u.profile, new) || Some(u.profile.as_str()) == old
    });
    for used in &mut uses.uses {
        if Some(used.profile.as_str()) == renamed {
            new.clone_into(&mut used.profile);
        }
    }
    write(backup_dir, &uses.uses).map(|()| true)
}

/// A change other than a profile's switched each of `switched` (project, name), through Klefki: that is the user's
/// choice now, so no profile puts it back. Projects are compared in any spelling.
pub(crate) fn forget_switches(
    backup_dir: &Path,
    switched: &[(String, String)],
) -> Result<bool, String> {
    if switched.is_empty() {
        return Ok(false);
    }
    let hit = |s: &Switch| {
        switched
            .iter()
            .any(|(p, n)| *n == s.name && crate::api::same_project(p, &s.project))
    };
    if !load(backup_dir)
        .uses
        .iter()
        .any(|u| u.switches.iter().any(hit))
    {
        return Ok(false);
    }
    let mut uses = writable(backup_dir)?;
    for used in &mut uses.uses {
        used.switches.retain(|s| !hit(s));
    }
    uses.uses.retain(|u| !u.switches.is_empty());
    write(backup_dir, &uses.uses).map(|()| true)
}

/// The profile `profile` is deleted without putting its switches back: they are forgotten. Nothing to do when it
/// was never used.
pub(crate) fn forget(backup_dir: &Path, profile: &str) -> Result<bool, String> {
    if load(backup_dir).of(profile).is_empty() {
        return Ok(false);
    }
    let mut uses = writable(backup_dir)?;
    uses.uses.retain(|u| u.profile != profile);
    write(backup_dir, &uses.uses).map(|()| true)
}

/// Servers renamed through Klefki, by their ids before and after: their switches follow them, as Klefki's rename
/// carries the switch in `disabledMcpServers` over to the new name. A user server's switch is renamed in every
/// project; a local or project server's in its own.
pub(crate) fn follow(
    backup_dir: &Path,
    moved: &[(crate::model::ConnectionId, crate::model::ConnectionId)],
) -> Result<bool, String> {
    use crate::api::Scope;
    let renames: Vec<(Option<String>, String, String)> = moved
        .iter()
        .filter_map(|(old, new)| {
            let (scope, project, old_name) = crate::model::ConnectionId::parse(old.as_str())?;
            let (_, _, new_name) = crate::model::ConnectionId::parse(new.as_str())?;
            let own = match scope {
                Scope::Local | Scope::Project => Some(project?),
                _ => None,
            };
            (old_name != new_name).then_some((own, old_name, new_name))
        })
        .collect();
    let touches = |s: &Switch| {
        renames
            .iter()
            .any(|(own, old, _)| *old == s.name && own.as_ref().is_none_or(|p| *p == s.project))
    };
    if !load(backup_dir)
        .uses
        .iter()
        .any(|u| u.switches.iter().any(touches))
    {
        return Ok(false);
    }
    let mut uses = writable(backup_dir)?;
    for used in &mut uses.uses {
        for s in &mut used.switches {
            if let Some((_, _, new)) = renames.iter().find(|(own, old, _)| {
                *old == s.name && own.as_ref().is_none_or(|p| *p == s.project)
            }) {
                s.name.clone_from(new);
            }
        }
    }
    write(backup_dir, &uses.uses).map(|()| true)
}

fn writable(backup_dir: &Path) -> Result<ProfileUses, String> {
    let read = load(backup_dir);
    match read.problem {
        Some(problem) => Err(format!(
            "{problem} Klefki changes nothing in it until the file can be read in full."
        )),
        None => Ok(read),
    }
}

fn write(backup_dir: &Path, uses: &[ProfileUse]) -> Result<(), String> {
    let file = path(backup_dir);
    let stored = File {
        version: VERSION,
        uses: uses
            .iter()
            .map(|u| StoredUse {
                profile: u.profile.clone(),
                switches: u
                    .switches
                    .iter()
                    .map(|s| StoredSwitch {
                        project: s.project.clone(),
                        name: s.name.clone(),
                        off: s.off,
                        other: Other::new(),
                    })
                    .collect(),
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

    fn sw(project: &str, name: &str, off: bool) -> Switch {
        Switch {
            project: project.into(),
            name: name.into(),
            off,
        }
    }

    #[test]
    fn a_use_is_recorded_merged_and_forgotten_when_set_back() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        assert_eq!(load(&backups), ProfileUses::default());
        record(&backups, "Writing", &[sw("/d", "memory", true)], &[]).unwrap();
        record(&backups, "Writing", &[sw("/d", "db", true)], &[]).unwrap();
        assert_eq!(
            load(&backups).of("Writing"),
            [sw("/d", "memory", true), sw("/d", "db", true)]
        );
        // Used again after an edit of the profile turned db back on: nothing of its doing is left on db.
        record(&backups, "Writing", &[sw("/d", "db", false)], &[]).unwrap();
        assert_eq!(load(&backups).of("Writing"), [sw("/d", "memory", true)]);
        // Shared only with a profile that set it: none did, so nothing is recorded for Research.
        record(&backups, "Research", &[], &[sw("/d", "x", true)]).unwrap();
        assert!(load(&backups).of("Research").is_empty());
        record(&backups, "Research", &[], &[sw("/d", "memory", true)]).unwrap();
        assert_eq!(load(&backups).of("Research"), [sw("/d", "memory", true)]);
        claim_name(&backups, Some("Research"), "Reading").unwrap();
        assert_eq!(load(&backups).of("Reading"), [sw("/d", "memory", true)]);
        forget(&backups, "Reading").unwrap();
        assert!(load(&backups).of("Reading").is_empty());
        assert_eq!(load(&backups).of("Writing"), [sw("/d", "memory", true)]);
    }

    #[test]
    fn stopping_forgets_the_switches_in_one_project_or_all_and_those_handed_back() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(
            &backups,
            "Writing",
            &[sw("/a", "m", true), sw("/b", "m", true)],
            &[],
        )
        .unwrap();
        record(&backups, "Research", &[sw("/a", "db", false)], &[]).unwrap();
        stop(
            &backups,
            "Writing",
            Some("/a"),
            &[("Research".into(), sw("/a", "db", false))],
        )
        .unwrap();
        let read = load(&backups);
        assert_eq!(read.of("Writing"), [sw("/b", "m", true)]);
        assert!(read.of("Research").is_empty());
        stop(&backups, "Writing", None, &[]).unwrap();
        assert_eq!(load(&backups), ProfileUses::default());
    }

    /// A record left under a name (Delete only's forget failed, say) is not taken over by a profile saved under that
    /// name later, and a rename onto it never writes the name twice, which would lock the file (found in review, PR
    /// #63).
    #[test]
    fn a_leftover_record_is_dropped_when_its_name_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(&backups, "B", &[sw("/a", "x", true)], &[]).unwrap();
        record(&backups, "A", &[sw("/a", "y", true)], &[]).unwrap();
        // A renamed onto B, whose record is left over.
        assert!(claim_name(&backups, Some("A"), "B").unwrap());
        let read = load(&backups);
        assert_eq!(read.problem, None);
        assert_eq!(read.of("B"), [sw("/a", "y", true)]);
        assert!(read.of("A").is_empty());
        // Variant: a new profile under a leftover name, in another case, starts with nothing.
        record(&backups, "C", &[sw("/a", "z", true)], &[]).unwrap();
        assert!(claim_name(&backups, None, "c").unwrap());
        assert!(load(&backups).of("C").is_empty());
        // Saved again as it was, or never used: nothing is written.
        assert!(!claim_name(&backups, Some("B"), "B").unwrap());
        assert!(!claim_name(&backups, Some("D"), "E").unwrap());
        assert_eq!(load(&backups).of("B"), [sw("/a", "y", true)]);
    }

    #[test]
    fn a_switch_set_by_another_change_is_forgotten_by_every_profile() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(
            &backups,
            "A",
            &[sw("/a", "x", true), sw("/b", "x", true)],
            &[],
        )
        .unwrap();
        record(&backups, "B", &[], &[sw("/a", "x", true)]).unwrap();
        assert!(forget_switches(&backups, &[("/a/".into(), "x".into())]).unwrap());
        let read = load(&backups);
        assert_eq!(read.of("A"), [sw("/b", "x", true)]);
        assert!(read.of("B").is_empty());
        assert!(!forget_switches(&backups, &[("/a".into(), "y".into())]).unwrap());
    }

    #[test]
    fn a_renamed_server_keeps_its_switches() {
        use crate::api::Scope;
        use crate::model::ConnectionId;
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(
            &backups,
            "W",
            &[
                sw("/a", "m", true),
                sw("/b", "m", true),
                sw("/a", "l", true),
            ],
            &[],
        )
        .unwrap();
        follow(
            &backups,
            &[
                (
                    ConnectionId::new(Scope::User, None, "m"),
                    ConnectionId::new(Scope::User, None, "mem"),
                ),
                (
                    ConnectionId::new(Scope::Local, Some("/b"), "l"),
                    ConnectionId::new(Scope::Local, Some("/b"), "l2"),
                ),
            ],
        )
        .unwrap();
        // The local l of /b is not the l switched in /a.
        assert_eq!(
            load(&backups).of("W"),
            [
                sw("/a", "mem", true),
                sw("/b", "mem", true),
                sw("/a", "l", true)
            ]
        );
    }

    #[test]
    fn a_file_read_in_part_is_never_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let file = path(&backups);
        for (case, text, says) in [
            (
                "a newer version",
                serde_json::json!({"version": 2, "uses": []}).to_string(),
                "version 2",
            ),
            (
                "an unknown field",
                serde_json::json!({"version": 1, "uses": [], "active": "W"}).to_string(),
                "1 entries",
            ),
            (
                "a name with a control character",
                serde_json::json!({"version": 1, "uses": [{"profile": "W", "switches": [
                    {"project": "/a", "name": "m\u{1b}[31m", "off": true},
                    {"project": "/a", "name": "ok", "off": true}
                ]}]})
                .to_string(),
                "1 entries",
            ),
            ("not JSON", "{".to_owned(), "Could not read"),
        ] {
            std::fs::write(&file, &text).unwrap();
            let read = load(&backups);
            assert!(
                read.problem.as_deref().unwrap_or_default().contains(says),
                "{case}: {read:?}"
            );
            for err in [
                record(&backups, "W", &[sw("/a", "x", true)], &[]).unwrap_err(),
                stop(&backups, "W", None, &[]).unwrap_err(),
            ] {
                assert!(err.contains("changes nothing in it"), "{case}: {err}");
            }
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "{case}");
        }
        std::fs::write(&file, vec![b' '; READ_LIMIT as usize + 1]).unwrap();
        assert!(load(&backups).problem.unwrap().contains("limit"));
    }
}
