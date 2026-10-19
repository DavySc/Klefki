//! Notices when the files the inventory is built from change on disk, from Claude Code or anyone else, so the
//! list stays current without a manual refresh.
//!
//! It polls file metadata rather than subscribing to the operating system's change notifications: the set is small
//! (`.claude.json`, two caches, and per project its `.mcp.json` and `.claude/settings.local.json`), a `stat` of each
//! once a second costs microseconds, it behaves the same on every platform and file system (network drives too),
//! and it needs no new dependency. A change is reported once the files have stopped changing for a moment, so a
//! burst of saves (Claude saves several times per command) causes one reload.
use crate::api::Snapshot;
use crate::config::{ConfigPaths, inventory};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// What identifies one version of a file without reading it. Writers that replace a file (Claude, klefki) give it a
/// new identity even when size and time happen to match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

/// The stamp of `path`, or `None` when it does not exist (or cannot be looked at).
pub(crate) fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        inode: std::os::unix::fs::MetadataExt::ino(&meta),
    })
}

/// The files a snapshot was built from.
pub(crate) fn watched_files(paths: &ConfigPaths, snapshot: &Snapshot) -> Vec<PathBuf> {
    let mut files = vec![
        paths.claude_json.clone(),
        paths.claude_dir.join("mcp-health-cache.json"),
        paths.claude_dir.join("mcp-needs-auth-cache.json"),
    ];
    for project in &snapshot.projects {
        // Like the inventory: a path that is not local and absolute is never touched, not even looked at.
        if inventory::is_safe_project_path(project) {
            let dir = Path::new(project);
            files.push(dir.join(".mcp.json"));
            files.push(dir.join(".claude").join("settings.local.json"));
        }
    }
    files
}

/// The files and their stamps as of the last reload.
#[derive(Default)]
struct Baseline {
    files: Vec<(PathBuf, Option<Stamp>)>,
    /// Bumped by every [`Watch::rebase`], so a change already reported is not reported again.
    generation: u64,
}

pub(crate) struct Watch {
    baseline: Arc<Mutex<Baseline>>,
    stop: Arc<AtomicBool>,
}

impl Watch {
    /// Starts polling every `interval`. `changed` is called once the files differ from the baseline and have not
    /// changed again for `settle`; then nothing more is reported until the next [`rebase`](Self::rebase).
    pub(crate) fn start(
        interval: Duration,
        settle: Duration,
        changed: impl Fn() + Send + 'static,
    ) -> Self {
        let baseline = Arc::new(Mutex::new(Baseline::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (b, s) = (Arc::clone(&baseline), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("klefki-watch".into())
            .stack_size(64 * 1024)
            .spawn(move || poll(&b, &s, interval, settle, &changed))
            .expect("start the file watcher");
        Self { baseline, stop }
    }

    /// Takes `files` as they are now as the new baseline. Call it right before reading them, so a change made while
    /// they are read is noticed afterwards rather than lost.
    pub(crate) fn rebase(&self, files: Vec<PathBuf>) {
        let stamped = files
            .into_iter()
            .map(|f| {
                let s = stamp(&f);
                (f, s)
            })
            .collect();
        let mut b = self.baseline.lock().unwrap_or_else(|e| e.into_inner());
        b.files = stamped;
        b.generation += 1;
    }
}

impl Watch {
    /// Also watches those of `files` not watched yet, as they are now. Files already watched keep their baseline.
    pub(crate) fn extend(&self, files: Vec<PathBuf>) {
        let mut b = self.baseline.lock().unwrap_or_else(|e| e.into_inner());
        let mut known: HashSet<PathBuf> = b.files.iter().map(|(f, _)| f.clone()).collect();
        for f in files {
            if known.insert(f.clone()) {
                let s = stamp(&f);
                b.files.push((f, s));
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn poll(
    baseline: &Mutex<Baseline>,
    stop: &AtomicBool,
    interval: Duration,
    settle: Duration,
    changed: &dyn Fn(),
) {
    // The stamps seen when a difference was first noticed, and since when they have held still.
    let mut pending: Option<(u64, Vec<Option<Stamp>>, Instant)> = None;
    let mut reported: Option<u64> = None;
    loop {
        std::thread::sleep(interval);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let (generation, files) = {
            let b = baseline.lock().unwrap_or_else(|e| e.into_inner());
            (b.generation, b.files.clone())
        };
        if reported == Some(generation) {
            continue;
        }
        let now: Vec<Option<Stamp>> = files.iter().map(|(f, _)| stamp(f)).collect();
        if files.iter().zip(&now).all(|((_, then), now)| then == now) {
            pending = None;
            continue;
        }
        match &pending {
            Some((g, seen, since)) if *g == generation && *seen == now => {
                if since.elapsed() >= settle {
                    pending = None;
                    reported = Some(generation);
                    changed();
                }
            }
            // A new difference, or still changing: wait for it to hold still.
            _ => pending = Some((generation, now, Instant::now())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const TICK: Duration = Duration::from_millis(20);
    const SETTLE: Duration = Duration::from_millis(120);

    fn watching(files: &[PathBuf]) -> (Watch, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let w = Watch::start(TICK, SETTLE, move || {
            let _ = tx.send(());
        });
        w.rebase(files.to_vec());
        (w, rx)
    }

    #[test]
    fn a_change_is_reported_once_it_holds_still() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        std::fs::write(&f, "1").unwrap();
        let (_w, rx) = watching(std::slice::from_ref(&f));
        assert!(rx.recv_timeout(SETTLE * 3).is_err(), "nothing changed yet");
        std::fs::write(&f, "22").unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("reported");
        assert!(
            rx.recv_timeout(SETTLE * 3).is_err(),
            "not reported again until rebased"
        );
    }

    #[test]
    fn a_burst_of_writes_is_one_report() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        std::fs::write(&f, "0").unwrap();
        let (w, rx) = watching(std::slice::from_ref(&f));
        for i in 0..8 {
            std::fs::write(&f, "x".repeat(i + 2)).unwrap();
            std::thread::sleep(TICK * 2);
        }
        rx.recv_timeout(Duration::from_secs(5)).expect("reported");
        assert!(rx.recv_timeout(SETTLE * 3).is_err());
        // After a rebase the next change is reported again.
        w.rebase(vec![f.clone()]);
        std::fs::write(&f, "after").unwrap();
        rx.recv_timeout(Duration::from_secs(5))
            .expect("reported again");
    }

    #[test]
    fn appearing_and_disappearing_files_count_as_changes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join(".mcp.json");
        let (w, rx) = watching(std::slice::from_ref(&f));
        std::fs::write(&f, "{}").unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("created");
        w.rebase(vec![f.clone()]);
        std::fs::remove_file(&f).unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("removed");
    }

    #[test]
    fn a_rebase_after_our_own_write_is_not_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        std::fs::write(&f, "1").unwrap();
        let (w, rx) = watching(std::slice::from_ref(&f));
        std::fs::write(&f, "our own write").unwrap();
        w.rebase(vec![f.clone()]);
        assert!(rx.recv_timeout(SETTLE * 4).is_err());
    }

    #[test]
    fn a_replaced_file_of_the_same_size_and_time_is_still_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        std::fs::write(&f, "aaaa").unwrap();
        let time = std::fs::metadata(&f).unwrap().modified().unwrap();
        let (_w, rx) = watching(std::slice::from_ref(&f));
        let tmp = dir.path().join("t");
        std::fs::write(&tmp, "bbbb").unwrap();
        filetime::set_file_mtime(&tmp, filetime::FileTime::from_system_time(time)).unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        if cfg!(unix) {
            rx.recv_timeout(Duration::from_secs(5))
                .expect("a new file, by inode");
        }
    }

    #[test]
    fn unsafe_project_paths_are_not_watched() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().to_string_lossy().into_owned();
        let paths = ConfigPaths::in_dir(dir.path());
        let snapshot = Snapshot {
            projects: vec![
                local.clone(),
                "//server/share/x".into(),
                "relative/path".into(),
            ],
            ..Snapshot::default()
        };
        let files = watched_files(&paths, &snapshot);
        assert!(files.contains(&dir.path().join(".mcp.json")));
        assert!(!files.iter().any(|f| f.to_string_lossy().contains("server")));
        assert!(
            !files
                .iter()
                .any(|f| f.to_string_lossy().contains("relative"))
        );
        assert_eq!(files.len(), 3 + 2);
    }

    #[test]
    fn many_new_files_are_watched_at_once() {
        // Found reviewing performance: each new file was looked for among all the files added before it, so the
        // first load with 300 projects (603 files) spent 15 to 20 ms here before the list was sent. The bound is
        // loose, for debug builds on slow machines; the quadratic version took far longer.
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..10_000)
            .map(|i| dir.path().join(format!("p{i}")).join(".mcp.json"))
            .collect();
        let w = Watch::start(Duration::from_secs(3600), SETTLE, || {});
        w.rebase(files[..1].to_vec());
        let started = Instant::now();
        w.extend(files.clone());
        w.extend(files.clone());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        let b = w.baseline.lock().unwrap();
        assert_eq!(b.files.len(), files.len(), "each file once");
        assert!(
            b.files.iter().zip(&files).all(|((f, _), g)| f == g),
            "in order"
        );
    }

    #[test]
    fn a_file_named_twice_is_watched_once() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.json"), dir.path().join("b.json"));
        let w = Watch::start(Duration::from_secs(3600), SETTLE, || {});
        w.rebase(vec![a.clone()]);
        w.extend(vec![b.clone(), a.clone(), b.clone()]);
        let files: Vec<PathBuf> = w
            .baseline
            .lock()
            .unwrap()
            .files
            .iter()
            .map(|(f, _)| f.clone())
            .collect();
        assert_eq!(files, [a, b]);
    }

    #[test]
    fn dropping_the_watch_stops_the_thread() {
        let (tx, rx) = mpsc::channel::<()>();
        let w = Watch::start(TICK, SETTLE, move || {
            let _ = tx.send(());
        });
        drop(w);
        // The thread owns the only sender; once it ends, the channel disconnects.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.recv_timeout(TICK) {
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => assert!(Instant::now() < deadline, "watch thread still running"),
            }
        }
    }
}
