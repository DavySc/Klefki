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

/// Once the log grows past this, it is cut back to the newest changes that fit in half of it (some 400 typical
/// ones), so a cut is not needed again at the next change.
const MAX_BYTES: u64 = 256 * 1024;
/// Never read more than this: the log is ours, but anyone can write to the file. Records are capped when written
/// (see below), so the log never comes near it by itself.
const READ_LIMIT: u64 = 1024 * 1024;
/// Per change, at most this many sentences and backups are kept, each sentence at most `MAX_TEXT` characters and
/// the error at most `MAX_ERROR`: when written, and again when read.
const MAX_LINES: usize = 20;
const MAX_TEXT: usize = 500;
const MAX_ERROR: usize = 2000;
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
    /// The exact time, to the second, with its offset from UTC: "19 Oct 2026, 11:07:23 (UTC+02:00)".
    pub exact: String,
    /// What was done, one sentence per change, as the review said it. Empty for a backup no change in the log
    /// mentions: one kept before the log existed.
    pub summary: Vec<String>,
    /// The change did not finish; this says why, and what it had already done.
    pub failed: Option<String>,
    /// The copies kept before the change, one per file it changed.
    pub backups: Vec<Backup>,
    /// Backups of the change past those listed: a change of many servers can keep more than are shown.
    pub more_backups: usize,
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
    /// Backups past those in `backups`, counted but not listed.
    #[serde(default, skip_serializing_if = "is_zero")]
    more_backups: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
    // Capped here too, so one large change (a bulk delete of a hundred servers) cannot crowd out the rest.
    let record = Record {
        at: now_ms(),
        summary: capped(summary.iter().map(|s| clip(s, MAX_TEXT))),
        error: error.map(|e| clip(e, MAX_ERROR)),
        backups: backups
            .iter()
            .take(listed(backups.len()))
            .map(|b| clip(&b.to_string_lossy(), MAX_PATH))
            .collect(),
        more_backups: backups.len() - listed(backups.len()),
    };
    let mut line = serde_json::to_string(&record).map_err(io::Error::other)?;
    line.push('\n');
    append(&log_path(backup_dir), line.as_bytes())
}

/// How many of `n` lines are listed: all of them up to [`MAX_LINES`], or one fewer, to leave room to say how many
/// more there are. A log that looks complete when it is not is worse than a short one.
fn listed(n: usize) -> usize {
    if n > MAX_LINES { MAX_LINES - 1 } else { n }
}

/// The sentences of a change, ending with "…and N more" past [`MAX_LINES`].
fn capped(lines: impl ExactSizeIterator<Item = String>) -> Vec<String> {
    let n = lines.len();
    let mut out: Vec<String> = lines.take(listed(n)).collect();
    if n > out.len() {
        out.push(format!("…and {} more", n - out.len()));
    }
    out
}

fn append(path: &Path, line: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    // Cut back first when this line would take the log past its size, so the line is never lost with it.
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && meta.len() + line.len() as u64 > MAX_BYTES
        && !compact(path)?
        && unreadable(path)
    {
        set_aside(path)?;
    }
    let mut file = match create_private_file(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            std::fs::OpenOptions::new().append(true).open(path)?
        }
        Err(e) => return Err(e),
    };
    file.write_all(line)
}

/// Keeps the newest whole lines that fit in half of [`MAX_BYTES`], written to a new file that then replaces the
/// log. `false` for a log that cannot be read (too large, not text): it is left as it is, for the caller to set
/// aside.
fn compact(path: &Path) -> io::Result<bool> {
    let Ok(Some(text)) = fsx::read_limited(path, READ_LIMIT) else {
        return Ok(false);
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut size = 0;
    let fit = lines
        .iter()
        .rev()
        .take_while(|l| {
            size += l.len() as u64 + 1;
            size <= MAX_BYTES / 2
        })
        .count();
    // The newest line is always kept, however large.
    let kept = &lines[lines.len() - fit.max(1).min(lines.len())..];
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    for line in kept {
        temp.write_all(line.as_bytes())?;
        temp.write_all(b"\n")?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(true)
}

/// Whether the file at `path` itself cannot be read as a log: not a plain file, larger than the log may be, or not
/// text. Not a read that failed for a moment (another program holding the file): that log is left where it is.
fn unreadable(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.len() > READ_LIMIT => true,
        Ok(_) => std::fs::read(path).is_ok_and(|bytes| std::str::from_utf8(&bytes).is_err()),
        Err(_) => false,
    }
}

/// Moves a log that cannot be read out of the way, as it is, next to it (`activity.unreadable-<ms>-<n>.jsonl`), so
/// a new one can start and nothing in it is lost. Returns where it went.
fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let at = now_ms();
    for n in 0u32.. {
        let aside = path.with_file_name(format!("activity.unreadable-{at:016}-{n}.jsonl"));
        match std::fs::symlink_metadata(&aside) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::rename(path, &aside)?;
                return Ok(aside);
            }
            _ => {}
        }
    }
    unreachable!("some name is free")
}

/// What the change logged with the backup called `name` (a file name) did, as its summary says; `None` when no
/// change in the log mentions it.
pub(crate) fn summary_of(backup_dir: &Path, name: &str) -> Option<Vec<String>> {
    let text = fsx::read_limited(&log_path(backup_dir), READ_LIMIT).ok()??;
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .find(|r| r.backups.iter().any(|b| file_name(b) == name))
        .map(|r| r.summary)
}

/// Reads the log and the backup folder: every change, and every backup no change mentions, newest first.
pub(crate) fn load(backup_dir: &Path) -> Activity {
    let now = now_ms();
    let mut problem = None;
    let text = match fsx::read_limited(&log_path(backup_dir), READ_LIMIT) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => {
            let why = sanitize_line(&e.to_string());
            let path = log_path(backup_dir);
            let aside = if unreadable(&path) {
                set_aside(&path).ok()
            } else {
                None
            };
            problem = Some(match aside {
                Some(aside) => format!(
                    "The activity log could not be read ({why}), so Klefki moved it to {} and starts a new one.",
                    clip(&sanitize_line(&aside.display().to_string()), MAX_PATH)
                ),
                None => format!("The activity log could not be read: {why}"),
            });
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
        let shown_backups = listed(record.backups.len());
        let more_backups = record
            .more_backups
            .saturating_add(record.backups.len() - shown_backups);
        let backups = record
            .backups
            .iter()
            .take(shown_backups)
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
                exact: String::new(),
                summary: capped(record.summary.iter().map(|s| shown(s))),
                failed: record
                    .error
                    .as_deref()
                    .map(|e| clip(&sanitize_line(e), MAX_ERROR)),
                backups,
                more_backups,
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
                exact: String::new(),
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
                more_backups: 0,
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
                entry.exact = exact(at, local_offset_secs);
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

pub(crate) fn backup_time(name: &str) -> Option<u64> {
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

/// `at` (milliseconds since 1970) in local time as a sentence says it, the same whenever it is read (it is logged):
/// "13 Oct 2026 at 18:40".
pub(crate) fn moment(at_ms: u64) -> String {
    on_day_at(at_ms, local_offset_secs)
}

fn on_day_at(at_ms: u64, offset: impl Fn(i64) -> i64) -> String {
    let secs = (at_ms / 1000) as i64;
    let local = secs + offset(secs);
    let (day, time) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let (y, m, d) = civil(day);
    format!(
        "{d} {} {y} at {:02}:{:02}",
        MONTHS[m as usize - 1],
        time / 3600,
        time % 3600 / 60
    )
}

/// `at` to the second, in local time, with the offset it had then.
fn exact(at_ms: u64, offset: impl Fn(i64) -> i64) -> String {
    let secs = (at_ms / 1000) as i64;
    let off = offset(secs);
    let local = secs + off;
    let (day, time) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let (y, m, d) = civil(day);
    let sign = if off < 0 { '-' } else { '+' };
    format!(
        "{d} {} {y}, {:02}:{:02}:{:02} (UTC{sign}{:02}:{:02})",
        MONTHS[m as usize - 1],
        time / 3600,
        time % 3600 / 60,
        time % 60,
        off.abs() / 3600,
        off.abs() % 3600 / 60
    )
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
    fn the_exact_time_has_seconds_and_its_offset() {
        assert_eq!(exact(NOW, utc), "19 Oct 2026, 10:44:29 (UTC+00:00)");
        assert_eq!(
            exact(NOW, |_| 2 * HOUR),
            "19 Oct 2026, 12:44:29 (UTC+02:00)"
        );
        assert_eq!(
            exact(NOW, |_| -(9 * HOUR + 30 * 60)),
            "19 Oct 2026, 01:14:29 (UTC-09:30)"
        );
    }

    #[test]
    fn a_moment_in_a_sentence_has_its_date_and_minute() {
        // 19 Oct 2026, 09:07:23 UTC.
        let at = 1_792_400_843_000;
        assert_eq!(on_day_at(at, |_| 0), "19 Oct 2026 at 09:07");
        assert_eq!(on_day_at(at, |_| 2 * 3600), "19 Oct 2026 at 11:07");
        assert_eq!(on_day_at(at, |_| -10 * 3600), "18 Oct 2026 at 23:07");
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
        assert!(entry.failed.as_ref().unwrap().chars().count() <= MAX_ERROR + 1);
        assert_eq!(entry.backups.len(), MAX_LINES - 1);
        assert_eq!(entry.more_backups, 50 - (MAX_LINES - 1));
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
        assert!(activity.entries.len() >= 200, "{}", activity.entries.len());
        assert!(activity.entries[0].summary[0].starts_with("change 999 "));
        assert_eq!(activity.problem, None);
    }

    #[test]
    fn big_changes_never_erase_the_log() {
        // Cross-review B2: records were capped only when read, and a log the cut could not read was replaced by an
        // empty one. 205 deletes of 120 servers each left 92 of them, and said nothing.
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let big: Vec<String> = (0..120)
            .map(|i| {
                format!(
                    "Delete server-number-{i:03} from the project /home/someone/work/repository-{i}"
                )
            })
            .collect();
        let long_error = "e".repeat(50_000);
        for i in 0..205 {
            let error = (i % 2 == 0).then_some(long_error.as_str());
            record(&backups, &big, error, &[]).unwrap();
        }
        record(&backups, &summary("Turn off alpha"), None, &[]).unwrap();
        let text = std::fs::read_to_string(log_path(&backups)).unwrap();
        let activity = load(&backups);
        assert_eq!(activity.problem, None);
        assert_eq!(activity.entries.len(), text.lines().count());
        assert!(activity.entries.len() > 20, "{}", activity.entries.len());
        assert_eq!(activity.entries[0].summary, ["Turn off alpha"]);
        let bulk = &activity.entries[1];
        assert_eq!(bulk.summary.len(), MAX_LINES);
        let longest = text.lines().map(str::len).max().unwrap();
        assert!(
            longest < 30_000,
            "a record is capped when written: {longest}"
        );
        // Never more than the size that starts a cut, plus the one change that went past it: far below the read
        // limit.
        assert!(text.len() as u64 <= MAX_BYTES + 30_000, "{}", text.len());
    }

    #[test]
    fn a_change_too_large_to_log_in_full_says_how_much_is_left_out() {
        // Cross-review R1: a bulk delete of 120 servers was logged as 20 deletes, and nothing said there were more.
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let sentences: Vec<String> = (0..30).map(|i| format!("Delete s{i}")).collect();
        let paths: Vec<PathBuf> = (0..25).map(|i| backups.join(format!("b{i}.bak"))).collect();
        record(&backups, &sentences, None, &paths).unwrap();
        let entry = &load(&backups).entries[0];
        assert_eq!(entry.summary.len(), MAX_LINES);
        assert_eq!(entry.summary[..MAX_LINES - 1], sentences[..MAX_LINES - 1]);
        assert_eq!(entry.summary[MAX_LINES - 1], "…and 11 more");
        assert_eq!(entry.backups.len(), MAX_LINES - 1);
        assert_eq!(entry.more_backups, 6);
        // Exactly as many as fit: all listed, nothing said to be left out.
        record(&backups, &sentences[..MAX_LINES], None, &paths[..MAX_LINES]).unwrap();
        let entry = &load(&backups).entries[0];
        assert_eq!(entry.summary, sentences[..MAX_LINES]);
        assert_eq!((entry.backups.len(), entry.more_backups), (MAX_LINES, 0));
    }

    /// The files next to the log that hold an unreadable one, set aside.
    fn set_aside(backups: &Path) -> Vec<PathBuf> {
        let dir = backups.parent().unwrap();
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("activity.unreadable-"))
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn a_log_too_large_to_read_is_set_aside_whole_and_a_new_one_started() {
        // Cross-review of the activity log, R2: a log that cannot be read was appended to, and reported as
        // unreadable, for ever. Nothing in it is lost: it is moved aside as it is.
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let line = format!(
            "{}\n",
            serde_json::json!({"at": NOW, "summary": ["x".repeat(1000)]})
        );
        let old = line.repeat(READ_LIMIT as usize / line.len() + 1);
        std::fs::write(log_path(&backups), &old).unwrap();
        record(&backups, &summary("Turn off alpha"), None, &[]).unwrap();
        let [aside] = &set_aside(&backups)[..] else {
            panic!("set aside once: {:?}", set_aside(&backups));
        };
        assert_eq!(
            std::fs::read_to_string(aside).unwrap(),
            old,
            "nothing that was there is lost"
        );
        let log = load(&backups);
        assert_eq!(log.problem, None, "{log:?}");
        let [entry] = &log.entries[..] else {
            panic!("the new log has the new change: {log:?}");
        };
        assert_eq!(entry.summary, ["Turn off alpha"]);
    }

    #[test]
    fn a_log_that_is_not_text_is_set_aside_when_read_and_says_where() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        let junk = [0xffu8, 0xfe, b'\n', 0x80, b'{'];
        std::fs::write(log_path(&backups), junk).unwrap();
        let log = load(&backups);
        let [aside] = &set_aside(&backups)[..] else {
            panic!("set aside once: {:?}", set_aside(&backups));
        };
        assert_eq!(std::fs::read(aside).unwrap(), junk);
        assert!(!log_path(&backups).exists());
        let problem = log.problem.unwrap();
        assert!(
            problem.starts_with("The activity log could not be read"),
            "{problem}"
        );
        assert!(problem.contains(&aside.display().to_string()), "{problem}");
        // Said once: the next look finds a new, empty log, and the next change starts it.
        assert_eq!(load(&backups), Activity::default());
        record(&backups, &summary("Turn on beta"), None, &[]).unwrap();
        let log = load(&backups);
        assert_eq!(log.problem, None);
        assert_eq!(log.entries.len(), 1);
        // A second unreadable log does not replace the first one set aside.
        std::fs::write(log_path(&backups), junk).unwrap();
        load(&backups);
        assert_eq!(set_aside(&backups).len(), 2);
    }

    #[test]
    fn only_a_log_that_is_itself_unreadable_is_set_aside() {
        // Cross-review U3: a read that fails for a moment (another program holds the file) is not a broken log.
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(&backups, &summary("Turn off alpha"), None, &[]).unwrap();
        let log = log_path(&backups);
        assert!(!unreadable(&log), "a good log");
        std::fs::write(&log, [0xffu8, 0x80]).unwrap();
        assert!(unreadable(&log), "not text");
        std::fs::write(&log, vec![b' '; READ_LIMIT as usize + 1]).unwrap();
        assert!(unreadable(&log), "too large");
        std::fs::remove_file(&log).unwrap();
        assert!(!unreadable(&log), "not there");
        std::fs::create_dir(&log).unwrap();
        assert!(unreadable(&log), "not a file");
    }

    #[cfg(unix)]
    #[test]
    fn a_log_that_cannot_be_opened_for_now_is_reported_and_left_where_it_is() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        record(&backups, &summary("Turn off alpha"), None, &[]).unwrap();
        let log = log_path(&backups);
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&log).is_ok() {
            // Running as root, nothing is refused; `only_a_log_that_is_itself_unreadable_is_set_aside` covers it.
            return;
        }
        let read = load(&backups);
        assert!(set_aside(&backups).is_empty(), "{:?}", set_aside(&backups));
        let problem = read.problem.unwrap();
        assert!(
            problem.starts_with("The activity log could not be read:"),
            "{problem}"
        );
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load(&backups).entries.len(), 1, "all still there");
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
