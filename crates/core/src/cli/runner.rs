//! Runs the Claude CLI with no shell, a timeout that ends everything it started, and a cap on how much output is
//! kept.
use super::locate::{self, LocateError};
use super::tree;
use crate::sanitize::sanitize;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub(crate) struct RunSpec<'a> {
    pub program: &'a Path,
    /// Fixed strings built by the caller from validated values; never joined into a command line.
    pub args: &'a [&'a str],
    pub cwd: Option<&'a Path>,
    /// Extra environment, for example `MCP_CLIENT_SECRET`.
    pub env: &'a [(&'a str, &'a OsStr)],
    pub timeout: Duration,
    /// Per stream. More output is read and discarded so the child never blocks on a full pipe.
    pub max_output: usize,
    /// Also end what the program leaves running: when it ends by itself, and when Klefki itself ends while it runs
    /// (it returns, is interrupted or its terminal closes; on Windows however it ends). For runs that start servers,
    /// such as `mcp get`: Claude ends the server it started, not what that server started. Other runs leave what
    /// they started alone, as Claude may start something on purpose that outlives it.
    pub end_leftovers: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

/// How long to wait for a pipe to close after the child is gone. A grandchild that inherited it may keep
/// it open; whatever was read by then is used.
const PIPE_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

pub(crate) fn run(spec: &RunSpec<'_>) -> io::Result<RunOutput> {
    locate::check(spec.program).map_err(|e| {
        let kind = match e {
            LocateError::NotFound => io::ErrorKind::NotFound,
            _ => io::ErrorKind::InvalidInput,
        };
        io::Error::new(kind, e.to_string())
    })?;

    // No shell: the program is started directly and every argument is its own argv entry.
    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        // A secret in our own environment must not leak to a child that was not given it on purpose.
        .env_remove("MCP_CLIENT_SECRET");
    for (key, value) in spec.env {
        cmd.env(key, value);
    }
    if let Some(cwd) = spec.cwd {
        cmd.current_dir(cwd);
    }

    let (mut child, tree) = tree::spawn(&mut cmd, spec.end_leftovers)?;
    let stdout = Capture::start(child.stdout.take(), spec.max_output);
    let stderr = Capture::start(child.stderr.take(), spec.max_output);

    let deadline = Instant::now() + spec.timeout;
    let mut timed_out = false;
    let status = loop {
        if tree::has_ended(&mut child)? {
            if spec.end_leftovers {
                // Not waited for yet, so the tree still has its id.
                tree.kill();
            }
            break Some(child.wait()?);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            // Not waited for yet, so the tree still has its id.
            tree.kill();
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(POLL);
    };

    // One grace period for both pipes, not one each.
    let grace = Instant::now() + PIPE_GRACE;
    let (stdout, out_cut) = stdout.finish(grace);
    let (stderr, err_cut) = stderr.finish(grace);
    Ok(RunOutput {
        stdout: text(&stdout, out_cut),
        stderr: text(&stderr, err_cut),
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        truncated: out_cut || err_cut,
    })
}

fn text(bytes: &[u8], truncated: bool) -> String {
    let mut out = sanitize(&String::from_utf8_lossy(bytes));
    if truncated {
        out.push('…');
    }
    out
}

/// Reads one pipe on its own thread, keeping at most `max` bytes but reading everything, so the child never
/// blocks on a full pipe.
struct Capture {
    shared: Arc<Mutex<(Vec<u8>, bool)>>,
    done: mpsc::Receiver<()>,
}

impl Capture {
    fn start<R: Read + Send + 'static>(stream: Option<R>, max: usize) -> Self {
        let shared = Arc::new(Mutex::new((Vec::new(), false)));
        let (tx, done) = mpsc::channel();
        if let Some(mut stream) = stream {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
                    let room = max.saturating_sub(guard.0.len());
                    let keep = n.min(room);
                    guard.0.extend_from_slice(&buf[..keep]);
                    if keep < n {
                        guard.1 = true;
                    }
                }
                let _ = tx.send(());
            });
        } else {
            let _ = tx.send(());
        }
        Self { shared, done }
    }

    /// What was read, once the pipe closes or `until` passes.
    fn finish(self, until: Instant) -> (Vec<u8>, bool) {
        let _ = self
            .done
            .recv_timeout(until.saturating_duration_since(Instant::now()));
        let guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        (guard.0.clone(), guard.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testutil::{
        end_pid, fake_claude, gone_soon, pid_alive, process_tree, process_tree_left_behind,
        tree_pids, write_scenario,
    };
    use sha2::{Digest, Sha256};
    use std::time::Instant;

    const SECS: Duration = Duration::from_secs(20);

    struct Fixture {
        dir: tempfile::TempDir,
        scenario: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    fn fixture(json: &str, files: &[(&str, &[u8])]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let scenario = write_scenario(dir.path(), json, files);
        let log = dir.path().join("log.jsonl");
        Fixture { dir, scenario, log }
    }

    impl Fixture {
        fn run(
            &self,
            args: &[&str],
            extra_env: &[(&str, &OsStr)],
            timeout: Duration,
            max: usize,
        ) -> io::Result<RunOutput> {
            let mut env: Vec<(&str, &OsStr)> = vec![
                ("FAKE_CLAUDE_SCENARIO", self.scenario.as_os_str()),
                ("FAKE_CLAUDE_LOG", self.log.as_os_str()),
            ];
            env.extend_from_slice(extra_env);
            run(&RunSpec {
                program: fake_claude(),
                args,
                cwd: None,
                env: &env,
                timeout,
                max_output: max,
                end_leftovers: false,
            })
        }

        fn log_lines(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    #[test]
    fn captures_output_and_exit_code() {
        let f = fixture(
            r#"{"rules":[{"args":["mcp","get","a"],"stdout_file":"out.txt","stderr_file":"err.txt","exit_code":0},
                        {"args":["mcp","get","b"],"stderr_file":"err.txt","exit_code":1}]}"#,
            &[("out.txt", b"hello\nworld\n"), ("err.txt", b"oops\n")],
        );
        let ok = f.run(&["mcp", "get", "a"], &[], SECS, 1000).unwrap();
        assert_eq!(
            (ok.stdout.as_str(), ok.stderr.as_str(), ok.exit_code),
            ("hello\nworld\n", "oops\n", Some(0))
        );
        assert!(!ok.timed_out && !ok.truncated);
        let bad = f.run(&["mcp", "get", "b"], &[], SECS, 1000).unwrap();
        assert_eq!((bad.stdout.as_str(), bad.exit_code), ("", Some(1)));
    }

    #[test]
    fn output_is_sanitised() {
        let hostile = include_bytes!("../../../../fixtures/mcp-get/hostile.txt");
        let f = fixture(
            r#"{"rules":[{"args":[],"stdout_file":"h.txt"}]}"#,
            &[("h.txt", hostile)],
        );
        let out = f.run(&["x"], &[], SECS, 100_000).unwrap();
        assert!(
            !out.stdout
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "{:?}",
            out.stdout
        );
        assert!(!out.stdout.contains('\u{202e}') && !out.stdout.contains('\u{1b}'));
        assert!(out.stdout.contains("Status: ✔ Connected"));
    }

    #[test]
    fn a_timeout_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let json = format!(
            r#"{{"rules":[{{"args":[],"hang":true,"write_pid_to":{}}}]}}"#,
            serde_json::to_string(&pid_file.display().to_string()).unwrap()
        );
        let f = fixture(&json, &[]);
        // The first use builds fake-claude with cargo, which can take seconds on a cold CI runner (11 s seen on
        // Windows); that must not count against the timeout being measured.
        fake_claude();
        let started = Instant::now();
        let out = f
            .run(
                &["mcp", "get", "slow"],
                &[],
                Duration::from_millis(700),
                1000,
            )
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !pid_alive(pid),
            "the child {pid} is still running after the timeout"
        );
    }

    #[test]
    fn a_timeout_ends_everything_the_child_started() {
        // Regression: only `claude` was killed, so a server it started (npx, then node) stayed behind.
        for depth in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let (program, args, env) = process_tree(&pids, depth);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let env: Vec<(&str, &OsStr)> = env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_os_str()))
                .collect();
            let timeout = Duration::from_secs(4);
            let started = Instant::now();
            let out = run(&RunSpec {
                program: &program,
                args: &args,
                cwd: None,
                env: &env,
                timeout,
                max_output: 1000,
                end_leftovers: false,
            })
            .unwrap();
            assert!(out.timed_out);
            // Whatever held the output pipes is gone too, so nothing waits out the grace period.
            assert!(
                started.elapsed() < timeout + PIPE_GRACE,
                "took {:?}",
                started.elapsed()
            );
            for pid in tree_pids(&pids, depth as usize + 1) {
                assert!(gone_soon(pid), "process {pid} (depth {depth}) still runs");
            }
        }
    }

    /// Runs `program` as a check would (or, with `end_leftovers` false, as any other run).
    fn run_tree(
        program: &Path,
        args: &[String],
        env: &[(String, std::ffi::OsString)],
        end_leftovers: bool,
        timeout: Duration,
    ) -> RunOutput {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let env: Vec<(&str, &OsStr)> = env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_os_str()))
            .collect();
        run(&RunSpec {
            program,
            args: &args,
            cwd: None,
            env: &env,
            timeout,
            max_output: 1000,
            end_leftovers,
        })
        .unwrap()
    }

    #[test]
    fn a_check_ends_what_claude_left_running_when_it_ends_by_itself() {
        // Regression: when Claude's own wait for a server ran out first, Claude ended the server but not what the
        // server started (npx, then node), and those stayed after the check.
        for depth in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let (program, args, env) = process_tree_left_behind(&pids, depth);
            let out = run_tree(&program, &args, &env, true, SECS);
            assert!(!out.timed_out, "{out:?}");
            assert_eq!(out.exit_code, Some(0), "{out:?}");
            for pid in tree_pids(&pids, depth as usize + 1) {
                assert!(gone_soon(pid), "process {pid} (depth {depth}) still runs");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_check_that_is_stopped_for_a_while_is_not_taken_for_ended() {
        // Found in review: on macOS `waitid(WEXITED)` also reports a stopped process, which ended the check's group
        // while Claude still ran and then waited for it without a timeout.
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let json = format!(
            r#"{{"rules":[{{"args":[],"delay_ms":1500,"stdout_file":"out.txt","write_pid_to":{}}}]}}"#,
            serde_json::to_string(&pid_file.display().to_string()).unwrap()
        );
        let f = fixture(&json, &[("out.txt", b"answered\n")]);
        let pauser = std::thread::spawn(move || {
            let until = Instant::now() + SECS;
            let pid = loop {
                if let Some(pid) = std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                {
                    break pid;
                }
                assert!(Instant::now() < until, "the check never started");
                std::thread::sleep(Duration::from_millis(10));
            };
            let signal = |name: &str| {
                Command::new("kill")
                    .args([name, &pid.to_string()])
                    .status()
                    .unwrap()
            };
            signal("-STOP");
            std::thread::sleep(Duration::from_millis(500));
            signal("-CONT");
        });
        let env: Vec<(&str, &OsStr)> = vec![
            ("FAKE_CLAUDE_SCENARIO", f.scenario.as_os_str()),
            ("FAKE_CLAUDE_LOG", f.log.as_os_str()),
        ];
        let out = run(&RunSpec {
            program: fake_claude(),
            args: &["mcp", "get", "x"],
            cwd: None,
            env: &env,
            timeout: SECS,
            max_output: 1000,
            end_leftovers: true,
        })
        .unwrap();
        pauser.join().unwrap();
        assert_eq!(
            (out.exit_code, out.stdout.as_str()),
            (Some(0), "answered\n"),
            "{out:?}"
        );
    }

    #[test]
    fn other_runs_leave_what_claude_started_alone() {
        // Claude may start something on purpose that outlives it, such as an update.
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let (program, args, env) = process_tree_left_behind(&pids, 1);
        let out = run_tree(&program, &args, &env, false, SECS);
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        std::thread::sleep(Duration::from_millis(300));
        // The run's own process ended; the one it started is still there.
        let alive: Vec<u32> = tree_pids(&pids, 2)
            .into_iter()
            .filter(|&p| pid_alive(p))
            .collect();
        for &pid in &alive {
            end_pid(pid);
        }
        assert_eq!(alive.len(), 1, "{alive:?}");
    }

    const END_DURING_CHECK: &str = "KLEFKI_TEST_END_DURING_CHECK";

    #[test]
    fn klefki_ending_during_a_check_ends_the_check_and_what_it_started() {
        // Regression: closing Klefki, or klefki-cli returning or being interrupted, while a check ran left `claude`
        // and its server running: they lead a process group (or job) of their own, which nothing ended.
        let mut ways = vec!["exit"];
        if cfg!(unix) {
            ways.extend(["SIGINT", "SIGTERM", "SIGHUP"]);
        }
        for way in ways {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cli::runner::tests::ends_during_a_check",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(END_DURING_CHECK, way)
                .env("KLEFKI_TEST_END_PIDS", &pids)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .unwrap();
            let found = tree_pids(&pids, 2);
            let left: Vec<u32> = found.into_iter().filter(|&p| !gone_soon(p)).collect();
            for &pid in &left {
                end_pid(pid);
            }
            assert!(left.is_empty(), "{way} ({status}): {left:?} still ran");
        }
    }

    /// Does nothing in a normal test run. Run by `klefki_ending_during_a_check_ends_the_check_and_what_it_started`,
    /// it starts a check of a process tree and, once the tree runs, ends itself the way it is told to.
    #[test]
    fn ends_during_a_check() {
        let Ok(way) = std::env::var(END_DURING_CHECK) else {
            return;
        };
        let pids = std::path::PathBuf::from(std::env::var_os("KLEFKI_TEST_END_PIDS").unwrap());
        let (program, args, env) = process_tree(&pids, 1);
        std::thread::spawn(move || run_tree(&program, &args, &env, true, SECS * 3));
        tree_pids(&pids, 2);
        #[cfg(unix)]
        {
            let signal = match way.as_str() {
                "SIGINT" => Some(libc::SIGINT),
                "SIGTERM" => Some(libc::SIGTERM),
                "SIGHUP" => Some(libc::SIGHUP),
                _ => None,
            };
            if let Some(signal) = signal {
                // SAFETY: a plain system call that sends a signal to this process.
                unsafe {
                    libc::raise(signal);
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        }
        let _ = way;
        std::process::exit(0);
    }

    #[test]
    fn pipes_left_open_after_the_child_ends_are_waited_for_once() {
        // Found reviewing main: each stream waited out the grace period in turn, so a process the child left
        // behind holding both pipes delayed the answer by twice the grace period.
        let program = std::env::current_exe().expect("the test binary");
        let started = Instant::now();
        let out = run(&RunSpec {
            program: &program,
            args: &[
                "--exact",
                "cli::runner::tests::pipe_holder",
                "--nocapture",
                "--test-threads=1",
            ],
            cwd: None,
            env: &[(PIPE_HOLDER, OsStr::new("parent"))],
            timeout: SECS,
            max_output: 100_000,
            end_leftovers: false,
        })
        .unwrap();
        assert!(!out.timed_out);
        assert!(
            out.stdout.contains("said before ending") && out.stderr.contains("and on stderr"),
            "what was read is kept: {out:?}"
        );
        assert!(
            started.elapsed() < 2 * PIPE_GRACE,
            "took {:?}",
            started.elapsed()
        );
    }

    const PIPE_HOLDER: &str = "KLEFKI_TEST_PIPE_HOLDER";

    /// Does nothing in a normal test run. Run by `pipes_left_open_after_the_child_ends_are_waited_for_once` as
    /// "parent", it starts itself again as "child", which keeps the same output pipes open for a while, and ends.
    #[test]
    fn pipe_holder() {
        match std::env::var(PIPE_HOLDER).as_deref() {
            Ok("parent") => {
                let child = Command::new(std::env::current_exe().unwrap())
                    .args(std::env::args_os().skip(1))
                    .env(PIPE_HOLDER, "child")
                    .stdin(Stdio::null())
                    .spawn()
                    .unwrap();
                // Never waited for: ending before it, with the pipes still open, is the point.
                std::mem::forget(child);
                println!("said before ending");
                eprintln!("and on stderr");
            }
            Ok("child") => std::thread::sleep(3 * PIPE_GRACE),
            _ => {}
        }
    }

    #[test]
    fn flooding_output_is_capped_without_blocking_the_child() {
        let f = fixture(r#"{"rules":[{"args":[],"flood_bytes":5000000}]}"#, &[]);
        // Build fake-claude before the clock starts (see `a_timeout_kills_the_child`).
        fake_claude();
        let started = Instant::now();
        let out = f.run(&["x"], &[], SECS, 10_000).unwrap();
        assert!(out.truncated);
        assert!(
            out.stdout.chars().count() <= 10_001,
            "kept {} chars",
            out.stdout.chars().count()
        );
        assert_eq!(
            out.exit_code,
            Some(0),
            "the child must be allowed to finish writing"
        );
        assert!(!out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn script_shims_are_refused_before_anything_is_spawned() {
        let f = fixture(r#"{"rules":[{"args":[]}]}"#, &[]);
        for name in ["claude.cmd", "claude.bat", "claude.ps1"] {
            let shim = f.dir.path().join(name);
            std::fs::write(&shim, "echo pwned").unwrap();
            let err = run(&RunSpec {
                program: &shim,
                args: &["mcp", "get", "x"],
                cwd: None,
                env: &[("FAKE_CLAUDE_LOG", f.log.as_os_str())],
                timeout: SECS,
                max_output: 1000,
                end_leftovers: false,
            })
            .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        assert!(f.log_lines().is_empty());
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_hang() {
        let t = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "nope.exe" } else { "nope" };
        let err = run(&RunSpec {
            program: &t.path().join(name),
            args: &[],
            cwd: None,
            env: &[],
            timeout: SECS,
            max_output: 10,
            end_leftovers: false,
        })
        .unwrap_err();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_secret_travels_only_in_the_environment() {
        let f = fixture(r#"{"rules":[{"args":[]}]}"#, &[]);
        let secret = "Top-Secret-Value-123";
        f.run(
            &["mcp", "add-json", "name", "{}"],
            &[("MCP_CLIENT_SECRET", OsStr::new(secret))],
            SECS,
            1000,
        )
        .unwrap();
        let log = f.log_lines();
        let start = log.iter().find(|l| l["event"] == "start").unwrap();
        assert_eq!(start["secret_set"], true);
        assert_eq!(
            start["secret_sha256"],
            format!("{:x}", Sha256::digest(secret.as_bytes()))
        );
        assert!(
            !start["argv"].to_string().contains(secret),
            "secret leaked into argv"
        );
        assert!(
            !std::fs::read_to_string(&f.log).unwrap().contains(secret),
            "secret leaked into the log"
        );
    }

    #[test]
    fn without_a_secret_none_is_passed() {
        let f = fixture(r#"{"rules":[{"args":[]}]}"#, &[]);
        f.run(&["mcp", "get", "x"], &[], SECS, 1000).unwrap();
        let start = f
            .log_lines()
            .into_iter()
            .find(|l| l["event"] == "start")
            .unwrap();
        assert_eq!(start["secret_set"], false);
    }

    #[test]
    fn colour_is_switched_off_and_the_working_directory_is_honoured() {
        let f = fixture(r#"{"rules":[{"args":[]}]}"#, &[]);
        let cwd = tempfile::tempdir().unwrap();
        let env: Vec<(&str, &OsStr)> = vec![
            ("FAKE_CLAUDE_SCENARIO", f.scenario.as_os_str()),
            ("FAKE_CLAUDE_LOG", f.log.as_os_str()),
        ];
        run(&RunSpec {
            program: fake_claude(),
            args: &["mcp", "get", "x"],
            cwd: Some(cwd.path()),
            env: &env,
            timeout: SECS,
            max_output: 1000,
            end_leftovers: false,
        })
        .unwrap();
        let start = f
            .log_lines()
            .into_iter()
            .find(|l| l["event"] == "start")
            .unwrap();
        assert_eq!(start["no_color_set"], true);
        let logged = std::path::PathBuf::from(start["cwd"].as_str().unwrap());
        assert_eq!(logged.file_name(), cwd.path().file_name());
    }

    #[test]
    fn an_unmatched_command_is_a_result_not_an_error() {
        let f = fixture(r#"{"rules":[{"args":["mcp","get","known"]}]}"#, &[]);
        let out = f.run(&["mcp", "get", "unknown"], &[], SECS, 1000).unwrap();
        assert_eq!(out.exit_code, Some(2));
        assert!(out.stderr.contains("no rule matches"));
    }

    #[test]
    fn a_wildcard_argument_matches_any_single_value_but_not_a_missing_one() {
        let f = fixture(
            r#"{"rules":[{"args":["mcp","get","*","extra"],"exit_code":0}]}"#,
            &[],
        );
        assert_eq!(
            f.run(&["mcp", "get", "anything-at-all", "extra"], &[], SECS, 100)
                .unwrap()
                .exit_code,
            Some(0)
        );
        // Too few arguments: the wildcard stands for exactly one.
        assert_eq!(
            f.run(&["mcp", "get"], &[], SECS, 100).unwrap().exit_code,
            Some(2)
        );
        // The wrong fixed argument after the wildcard.
        assert_eq!(
            f.run(&["mcp", "get", "x", "other"], &[], SECS, 100)
                .unwrap()
                .exit_code,
            Some(2)
        );
    }

    #[test]
    fn the_debug_file_the_client_would_write_is_written_where_asked() {
        let f = fixture(
            r#"{"rules":[{"args":["--debug","mcp","--debug-file","*","mcp","get","s"],
                          "debug_file_from":"debug.txt","stdout_file":"out.txt"}]}"#,
            &[
                ("debug.txt", b"line one\nline two\n"),
                ("out.txt", b"Status: ok\n"),
            ],
        );
        let target = f.dir.path().join("written-by-the-cli.log");
        let target_str = target.to_string_lossy().into_owned();
        let out = f
            .run(
                &[
                    "--debug",
                    "mcp",
                    "--debug-file",
                    &target_str,
                    "mcp",
                    "get",
                    "s",
                ],
                &[],
                SECS,
                1000,
            )
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, "Status: ok\n");
        assert_eq!(std::fs::read(&target).unwrap(), b"line one\nline two\n");
    }

    #[test]
    fn existing_exact_rules_still_work_next_to_wildcards() {
        let f = fixture(
            r#"{"rules":[{"args":["mcp","get","a"],"exit_code":3},
                        {"args":["mcp","get","*"],"exit_code":4}]}"#,
            &[],
        );
        assert_eq!(
            f.run(&["mcp", "get", "a"], &[], SECS, 10)
                .unwrap()
                .exit_code,
            Some(3)
        );
        assert_eq!(
            f.run(&["mcp", "get", "b"], &[], SECS, 10)
                .unwrap()
                .exit_code,
            Some(4)
        );
    }

    #[test]
    fn concurrent_stub_runs_never_interleave_log_lines() {
        // Regression: the stub wrote each log line with several `write` calls, so parallel runs tore lines apart.
        let dir = tempfile::tempdir().unwrap();
        let scenario = write_scenario(dir.path(), r#"{"rules":[{"args":["mcp","get"]}]}"#, &[]);
        let log = dir.path().join("log.jsonl");
        let long = "x".repeat(2000);
        let mut children = Vec::new();
        for round in 0..8 {
            for i in 0..16 {
                children.push(
                    Command::new(fake_claude())
                        .args(["mcp", "get", &format!("s{round}-{i}-{long}")])
                        .env("FAKE_CLAUDE_SCENARIO", &scenario)
                        .env("FAKE_CLAUDE_LOG", &log)
                        .current_dir(dir.path())
                        .stdout(Stdio::null())
                        .spawn()
                        .unwrap(),
                );
            }
        }
        for mut c in children {
            c.wait().unwrap();
        }
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 8 * 16 * 2, "one start and one end per run");
        for l in lines {
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("torn log line: {e}"));
        }
    }
}
