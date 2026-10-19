//! Test helpers: build `fake-claude` once and write scenario files. Test-only.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The built `fake-claude` binary. The first call runs `cargo build -p fake-claude` and reads the artifact path
/// from cargo's JSON output, so `cargo test -p klefki-core` works without building the whole workspace first.
pub(crate) fn fake_claude() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo)
            .args(["build", "-p", "fake-claude", "--message-format=json"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run cargo to build fake-claude");
        assert!(out.status.success(), "building fake-claude failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "fake-claude")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported the fake-claude executable")
    })
}

/// Writes `scenario.json` (and any files it refers to) into `dir`; returns the scenario path.
pub(crate) fn write_scenario(dir: &Path, json: &str, files: &[(&str, &[u8])]) -> PathBuf {
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let path = dir.join("scenario.json");
    std::fs::write(&path, json).unwrap();
    path
}

/// Is a process with this id still running? A zombie (ended, not yet reaped by a parent that is gone too) is not.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }
    #[cfg(unix)]
    {
        // An orphan is reaped by the system's first process, which in a container may never do it.
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        let stat = String::from_utf8_lossy(&out.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }
}

/// Waits up to 10 s for the process to end.
pub(crate) fn gone_soon(pid: u32) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Ends a process the test left running on purpose.
pub(crate) fn end_pid(pid: u32) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output();
}

const TREE_FILE: &str = "KLEFKI_TEST_TREE_FILE";
const TREE_DEPTH: &str = "KLEFKI_TEST_TREE_DEPTH";
const TREE_LEADER_ENDS: &str = "KLEFKI_TEST_TREE_LEADER_ENDS";

/// A stand-in for `claude` starting a server (`npx`, then node) that it never ends: this test binary, run as
/// [`tree_helper`], starts `depth` more levels of itself below it, and every level waits forever. Each appends its
/// id to `pids`. Returns the program, its arguments and its environment.
pub(crate) fn process_tree(
    pids: &Path,
    depth: u32,
) -> (PathBuf, Vec<String>, Vec<(String, OsString)>) {
    let program = std::env::current_exe().expect("the test binary");
    let args = [
        "--exact",
        "cli::testutil::tree_helper",
        "--nocapture",
        "--test-threads=1",
    ]
    .map(String::from)
    .to_vec();
    let env = vec![
        (TREE_FILE.to_owned(), pids.as_os_str().to_owned()),
        (TREE_DEPTH.to_owned(), depth.to_string().into()),
    ];
    (program, args, env)
}

/// Like [`process_tree`], but its first process ends by itself once every level below it has started, as `claude`
/// does when its own wait for a server runs out: it ends the server it started, not what that started.
pub(crate) fn process_tree_left_behind(
    pids: &Path,
    depth: u32,
) -> (PathBuf, Vec<String>, Vec<(String, OsString)>) {
    let (program, args, mut env) = process_tree(pids, depth);
    env.push((TREE_LEADER_ENDS.to_owned(), "1".into()));
    (program, args, env)
}

/// The ids [`process_tree`] wrote, once there are `count` of them (waits up to 20 s).
pub(crate) fn tree_pids(pids: &Path, count: usize) -> Vec<u32> {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let found: Vec<u32> = std::fs::read_to_string(pids)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        if found.len() >= count {
            return found;
        }
        assert!(
            Instant::now() < until,
            "only {} of {count} processes started",
            found.len()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Does nothing in a normal test run; see [`process_tree`].
#[test]
fn tree_helper() {
    use std::io::Write;
    let Some(file) = std::env::var_os(TREE_FILE) else {
        return;
    };
    let depth: u32 = std::env::var(TREE_DEPTH).unwrap().parse().unwrap();
    let leader_ends = std::env::var_os(TREE_LEADER_ENDS).is_some();
    // Like a server that outlives a hang-up: closing a terminal must not be what ends it.
    #[cfg(unix)]
    // SAFETY: setting a signal to be ignored has no handler to run and no memory to share.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let _below = (depth > 0).then(|| {
        let (program, args, env) = process_tree(Path::new(&file), depth - 1);
        Command::new(program)
            .args(args)
            .envs(env)
            .env_remove(TREE_LEADER_ENDS)
            .stdin(Stdio::null())
            .spawn()
            .unwrap()
    });
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(format!("{}\n", std::process::id()).as_bytes())
        .unwrap();
    if leader_ends {
        tree_pids(Path::new(&file), depth as usize + 1);
        return;
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
