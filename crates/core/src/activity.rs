//! The activity log: every change Klefki saved, with the backups it kept first.
//!
//! One JSON line per change, newest last, in a private file next to the backup folder. It is written after each
//! change and read only when the Activity log page asks ([`load`]), so it costs nothing at start-up. The file is
//! read as untrusted, like any file on disk: size-limited, and every string sanitised.
use crate::config::docstore::{create_private_dir, create_private_file};
use crate::config::fsx;
use crate::sanitize::{clip, sanitize_line};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log is cut back to its newest [`KEEP`] changes once it grows past this.
const MAX_BYTES: u64 = 256 * 1024;
const KEEP: usize = 200;
/// Never read more than this: the log is ours, but anyone can write to the file.
const READ_LIMIT: u64 = 1024 * 1024;
/// Per change, at most this many sentences and backups are shown, each at most `MAX_TEXT` characters.
const MAX_LINES: usize = 20;
const MAX_TEXT: usize = 500;
/// Paths are copied, so they are kept whole up to a length no real path reaches.
const MAX_PATH: usize = 4096;

/// What the Activity log page shows ([`crate::api::Event::Activity`]). Text is sanitised.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Activity {
    /// Newest first.
    pub entries: Vec<ActivityEntry>,
    /// Why the log could not be read in full. What could be read is still listed.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    /// When, in local time: "Today 14:05", "Yesterday 09:12", "13 Oct 18:40" or "13 Oct 2025 18:40".
    pub when: String,
    /// What was done, one sentence per change, as the review said it. Empty for a backup no change in the log
    /// mentions: one kept before the log existed.
    pub summary: Vec<String>,
    /// The change did not finish; this says why, and what it had already done.
    pub failed: Option<String>,
    /// The copies kept before the change, one per file it changed.
    pub backups: Vec<Backup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// The file it is a copy of, as the user knows it: `.claude.json`, or a project's `settings.local.json`.
    pub of: String,
    /// Where the copy is.
    pub path: String,
    /// The copy is still there. Only the newest copies of each file are kept.
    pub kept: bool,
}

/// One line of the log file.
#[derive(Serialize, Deserialize)]
struct Record {
    /// Milliseconds since 1970, UTC.
    at: u64,
    summary: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default)]
    backups: Vec<String>,
}

/// The log, next to the backup folder (like the recovery folder): `.../klefki/activity.jsonl`.
pub(crate) fn log_path(backup_dir: &Path) -> PathBuf {
    backup_dir.with_file_name("activity.jsonl")
}

/// Adds a change to the log. `error` is set when the change stopped part way.
pub(crate) fn record(
    backup_dir: &Path,
    summary: &[String],
    error: Option<&str>,
    backups: &[PathBuf],
) -> io::Result<()> {
    let record = Record {
        at: now_ms(),
        summary: summary.to_vec(),
        error: error.map(str::to_owned),
        backups: backups.iter().map(|b| b.display().to_string()).collect(),
    };
    let mut line = serde_json::to_string(&record).map_err(io::Error::other)?;
    line.push('\n');
    append(&log_path(backup_dir), line.as_bytes())
}

fn append(path: &Path, line: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut file = match create_private_file(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            std::fs::OpenOptions::new().append(true).open(path)?
        }
        Err(e) => return Err(e),
    };
    file.write_all(line)?;
    if file.metadata()?.len() > MAX_BYTES {
        drop(file);
        compact(path)?;
    }
    Ok(())
}

/// Keeps the newest [`KEEP`] lines, written whole to a new file that then replaces the log.
fn compact(path: &Path) -> io::Result<()> {
    let text = fsx::read_limited(path, READ_LIMIT)
        .ok()
        .flatten()
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let kept = &lines[lines.len().saturating_sub(KEEP)..];
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    for line in kept {
        temp.write_all(line.as_bytes())?;
        temp.write_all(b"\n")?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Reads the log and the backup folder: every change, and every backup no change mentions, newest first.
pub(crate) fn load(backup_dir: &Path) -> Activity {
    let now = now_ms();
    let mut problem = None;
    let text = match fsx::read_limited(&log_path(backup_dir), READ_LIMIT) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => {
            problem = Some(format!(
                "The activity log could not be read: {}",
                sanitize_line(&e.to_string())
            ));
            String::new()
        }
    };
    let on_disk = backups_on_disk(backup_dir);
    let names: HashSet<&str> = on_disk.iter().map(|(name, _)| name.as_str()).collect();
    let mut mentioned = HashSet::new();
    let mut unreadable = 0usize;
    let mut entries: Vec<((u64, usize), ActivityEntry)> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<Record>(line) else {
            unreadable += 1;
            continue;
        };
        let backups = record
            .backups
            .iter()
            .take(MAX_LINES)
            .map(|path| {
                let name = file_name(path);
                mentioned.insert(name.to_owned());
                Backup {
                    of: backup_of(name),
                    path: clip(&sanitize_line(path), MAX_PATH),
                    kept: names.contains(name),
                }
            })
            .collect();
        let seq = entries.len() + 1;
        entries.push((
            (record.at, seq),
            ActivityEntry {
                when: String::new(),
                summary: record
                    .summary
                    .iter()
                    .take(MAX_LINES)
                    .map(|s| shown(s))
                    .collect(),
                failed: record.error.as_deref().map(shown),
                backups,
            },
        ));
    }
    for (name, at) in on_disk {
        if mentioned.contains(&name) {
            continue;
        }
        entries.push((
            (at, 0),
            ActivityEntry {
                when: String::new(),
                summary: Vec::new(),
                failed: None,
                backups: vec![Backup {
                    of: backup_of(&name),
                    path: clip(
                        &sanitize_line(&backup_dir.join(&name).display().to_string()),
                        MAX_PATH,
                    ),
                    kept: true,
                }],
            },
        ));
    }
    if unreadable > 0 && problem.is_none() {
        problem = Some(format!(
            "{unreadable} line{} of the activity log could not be read and {} left out.",
            if unreadable == 1 { "" } else { "s" },
            if unreadable == 1 { "is" } else { "are" },
        ));
    }
    // Changes made in the same millisecond: the one logged last first.
    entries.sort_by_key(|(key, _)| std::cmp::Reverse(*key));
    Activity {
        entries: entries
            .into_iter()
            .map(|((at, _), mut entry)| {
                entry.when = when(at, now, local_offset_secs);
                entry
            })
            .collect(),
        problem,
    }
}

/// The backups in the folder, by file name, with when they were made (from the name: `<file>.<ms>-<n>.bak`).
fn backups_on_disk(backup_dir: &Path) -> Vec<(String, u64)> {
    let Ok(entries) = std::fs::read_dir(backup_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let at = backup_time(&name)?;
            Some((name, at))
        })
        .collect()
}

fn backup_time(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".bak")?;
    let (rest, n) = stem.rsplit_once('-')?;
    n.parse::<u32>().ok()?;
    let (_, ms) = rest.rsplit_once('.')?;
    (ms.len() == 16).then_some(())?;
    ms.parse().ok()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Which file a backup is a copy of, from its name (see `Docstore::backup_label`).
fn backup_of(name: &str) -> String {
    if name.starts_with("claude.json.") {
        ".claude.json".to_owned()
    } else if name.starts_with("settings.local-") {
        "a project's settings.local.json".to_owned()
    } else {
        "a configuration file".to_owned()
    }
}

fn shown(s: &str) -> String {
    clip(&sanitize_line(s), MAX_TEXT)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `at` as the user reads it, in local time, next to `now`. `offset` gives the local offset from UTC in seconds at
/// a moment (seconds since 1970), so a change from before a daylight-saving switch shows the hour it had then.
fn when(at_ms: u64, now_ms: u64, offset: impl Fn(i64) -> i64) -> String {
    let local = |ms: u64| {
        let secs = (ms / 1000) as i64;
        secs + offset(secs)
    };
    let (at, now) = (local(at_ms), local(now_ms));
    let (day, time) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    let today = now.div_euclid(86_400);
    let clock = format!("{:02}:{:02}", time / 3600, time % 3600 / 60);
    let (y, m, d) = civil(day);
    if day == today {
        format!("Today {clock}")
    } else if day + 1 == today {
        format!("Yesterday {clock}")
    } else if y == civil(today).0 {
        format!("{d} {} {clock}", MONTHS[m as usize - 1])
    } else {
        format!("{d} {} {y} {clock}", MONTHS[m as usize - 1])
    }
}

/// Year, month (1-12) and day of a day number counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The day number of a date, the inverse of [`civil`].
#[cfg(any(windows, test))]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(unix)]
fn local_offset_secs(at: i64) -> i64 {
    let time = at as libc::time_t;
    // SAFETY: `localtime_r` writes only into `tm`, which outlives the call; both pointers are valid.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    if ok { tm.tm_gmtoff as i64 } else { 0 }
}

#[cfg(windows)]
fn local_offset_secs(at: i64) -> i64 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
    let (day, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    let (y, m, d) = civil(day);
    let utc = SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDayOfWeek: 0,
        wDay: d as u16,
        wHour: (secs / 3600) as u16,
        wMinute: (secs % 3600 / 60) as u16,
        wSecond: (secs % 60) as u16,
        wMilliseconds: 0,
    };
    // SAFETY: both pointers are to live `SYSTEMTIME`s; a null zone means the current time zone.
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return 0;
    }
    let local_secs = days_from_civil(
        i64::from(local.wYear),
        u32::from(local.wMonth),
        u32::from(local.wDay),
    ) * 86_400
        + i64::from(local.wHour) * 3600
        + i64::from(local.wMinute) * 60
        + i64::from(local.wSecond);
    local_secs - at
}

#[cfg(not(any(unix, windows)))]
fn local_offset_secs(_at: i64) -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3600;
    /// 2026-10-19 10:44:29 UTC.
    const NOW: u64 = 1_792_406_669_000;

    fn utc(_: i64) -> i64 {
        0
    }

    fn summary(s: &str) -> Vec<String> {
        vec![s.to_owned()]
    }

    /// A backup file in the folder, named as the docstore names them, made at `ms`.
    fn backup(dir: &Path, label: &str, ms: u64) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{label}.{ms:016}-0.bak"));
        std::fs::write(&path, "{}").unwrap();
        path
    }

    #[test]
    fn times_read_as_today_yesterday_or_a_date() {
        assert_eq!(when(NOW, NOW, utc), "Today 10:44");
        assert_eq!(when(NOW - 10 * 3_600_000, NOW, utc), "Today 00:44");
        assert_eq!(when(NOW - 11 * 3_600_000, NOW, utc), "Yesterday 23:44");
        assert_eq!(when(NOW - 3 * 86_400_000, NOW, utc), "16 Oct 10:44");
        assert_eq!(when(NOW - 365 * 86_400_000, NOW, utc), "19 Oct 2025 10:44");
        assert_eq!(when(0, NOW, utc), "1 Jan 1970 00:00");
    }

    #[test]
    fn times_are_local_with_the_offset_they_had_then() {
        // Two hours east of UTC: 10:44 UTC is 12:44 there.
        assert_eq!(when(NOW, NOW, |_| 2 * HOUR), "Today 12:44");
        // A day boundary in local time, not in UTC: 23:30 UTC is already tomorrow at +2.
        let late = NOW + (12 * 3600 + 46 * 60) * 1000;
        assert_eq!(when(NOW, late, |_| 2 * HOUR), "Yesterday 12:44");
        // Each moment gets its own offset (daylight saving): summer at +2, winter at +1.
        let winter = NOW + 40 * 86_400_000;
        let offset = |at: i64| {
            if at < (winter / 1000) as i64 {
                2 * HOUR
            } else {
                HOUR
            }
        };
        assert_eq!(when(NOW, winter, offset), "19 Oct 12:44");
        assert_eq!(when(winter, winter, offset), "Today 11:44");
    }

    #[test]
    fn dates_convert_both_ways() {
        for day in [-800_000, -1, 0, 1, 59, 60, 11_016, 20_745, 2_932_896] {
            let (y, m, d) = civil(day);
            assert_eq!(days_from_civil(y, m, d), day, "{y}-{m}-{d}");
        }
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_745), (2026, 10, 19));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn the_local_offset_is_a_whole_zone() {
        let offset = local_offset_secs((NOW / 1000) as i64);
        assert!(offset.abs() <= 14 * HOUR, "{offset}");
        assert_eq!(offset % 900, 0, "{offset}");
    }

    #[test]
    fn a_change_is_listed_with_its_backup_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("data").join("backups");
        let first = backup(&backups, "claude.json", now_ms() - 2000);
        record(
            &backups,
            &summary("Turn off alpha"),
            None,
            std::slice::from_ref(&first),
        )
        .unwrap();
        let second = backup(&backups, "claude.json", now_ms() - 1000);
        record(
            &backups,
            &["Delete beta".to_owned(), "Delete gamma".to_owned()],
            None,
            std::slice::from_ref(&second),
        )
        .unwrap();

        let activity = load(&backups);
        assert_eq!(activity.problem, None);
        let [newest, oldest] = &activity.entries[..] else {
            panic!("two entries: {activity:?}");
        };
        assert_eq!(newest.summary, ["Delete beta", "Delete gamma"]);
        assert_eq!(oldest.summary, ["Turn off alpha"]);
        assert!(newest.when.starts_with("Today "), "{}", newest.when);
        assert_eq!(newest.failed, None);
        assert_eq!(
            oldest.backups,
            [Backup {
                of: ".claude.json".to_owned(),
                path: first.display().to_string(),
                kept: true,
            }]
        );
    }

    #[test]
    fn a_backup_rotated_away_is_marked_gone() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let path = backup(&backups, "claude.json", now_ms());
        record(
            &backups,
            &summary("Turn off alpha"),
            None,
            std::slice::from_ref(&path),
        )
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        let activity = load(&backups);
        assert_eq!(activity.entries.len(), 1);
        assert!(!activity.entries[0].backups[0].kept);
    }

    #[test]
    fn backups_no_change_mentions_are_listed_on_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mentioned = backup(&backups, "claude.json", now_ms() - 5000);
        record(&backups, &summary("Turn off alpha"), None, &[mentioned]).unwrap();
        // Kept before the log existed, and one of a project's settings.
        backup(&backups, "claude.json", now_ms() - 86_400_000);
        backup(&backups, "settings.local-0123456789ab", now_ms() - 1000);
        // Not backups: left out.
        std::fs::write(backups.join("notes.txt"), "x").unwrap();
        std::fs::write(backups.join("claude.json.12-0.bak"), "x").unwrap();

        let activity = load(&backups);
        let shape: Vec<(usize, &str)> = activity
            .entries
            .iter()
            .map(|e| (e.summary.len(), e.backups[0].of.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                // Logged after its backup was made, so it is newer than both the others.
                (1, ".claude.json"),
                (0, "a project's settings.local.json"),
                (0, ".claude.json"),
            ]
        );
        assert!(activity.entries[2].when.starts_with("Yesterday "));
    }

    #[test]
    fn a_change_that_stopped_part_way_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(
            &backups,
            &summary("Move alpha to user"),
            Some("Claude Code already added alpha to user."),
            &[],
        )
        .unwrap();
        let entry = &load(&backups).entries[0];
        assert_eq!(
            entry.failed.as_deref(),
            Some("Claude Code already added alpha to user.")
        );
        assert!(entry.backups.is_empty());
    }

    #[test]
    fn nothing_logged_and_no_folder_is_an_empty_log() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("backups")), Activity::default());
    }

    #[test]
    fn the_log_is_read_as_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let long = "x".repeat(10_000);
        let lines = [
            "not json".to_owned(),
            serde_json::json!({
                "at": NOW,
                "summary": ["Turn off \u{1b}[31mred\u{202e}evil\nnext"],
                "error": long,
                "backups": vec!["C:\\x\\claude.json.0000000000000001-0.bak"; 50],
            })
            .to_string(),
            "{\"at\": \"soon\"}".to_owned(),
        ];
        std::fs::write(log_path(&backups), lines.join("\n")).unwrap();
        let activity = load(&backups);
        assert_eq!(activity.entries.len(), 1);
        let entry = &activity.entries[0];
        assert_eq!(entry.summary, ["Turn off redevil next"]);
        assert!(entry.failed.as_ref().unwrap().chars().count() <= MAX_TEXT + 1);
        assert_eq!(entry.backups.len(), MAX_LINES);
        assert_eq!(entry.backups[0].of, ".claude.json");
        assert_eq!(
            activity.problem.as_deref(),
            Some("2 lines of the activity log could not be read and are left out.")
        );
    }

    #[test]
    fn an_oversized_log_is_refused_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        std::fs::write(log_path(&backups), vec![b' '; READ_LIMIT as usize + 1]).unwrap();
        let activity = load(&backups);
        assert!(activity.entries.is_empty());
        assert!(
            activity
                .problem
                .as_deref()
                .unwrap()
                .starts_with("The activity log could not be read"),
            "{activity:?}"
        );
    }

    #[test]
    fn the_log_stays_small_keeping_the_newest_changes() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let filler = "y".repeat(400);
        for i in 0..1000 {
            record(&backups, &[format!("change {i} {filler}")], None, &[]).unwrap();
            assert!(std::fs::metadata(log_path(&backups)).unwrap().len() <= MAX_BYTES + 1024);
        }
        let activity = load(&backups);
        assert!(activity.entries.len() >= KEEP, "{}", activity.entries.len());
        assert!(activity.entries[0].summary[0].starts_with("change 999 "));
        assert_eq!(activity.problem, None);
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("data").join("backups");
        record(&backups, &summary("Turn off alpha"), None, &[]).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&log_path(&backups)), 0o600);
        assert_eq!(mode(&dir.path().join("data")), 0o700);
        // Still private once cut back.
        compact(&log_path(&backups)).unwrap();
        assert_eq!(mode(&log_path(&backups)), 0o600);
    }
}
