//! Runs the Claude CLI with no shell, a timeout and a cap on how much output is kept.
use super::locate::{self, LocateError};
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
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn()?;
    let stdout = Capture::start(child.stdout.take(), spec.max_output);
    let stderr = Capture::start(child.stderr.take(), spec.max_output);

    let deadline = Instant::now() + spec.timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(POLL);
    };

    let (stdout, out_cut) = stdout.finish();
    let (stderr, err_cut) = stderr.finish();
    Ok(RunOutput {
        stdout: text(&stdout, out_cut),
        stderr: text(&stderr, err_cut),
        exit_code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
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

    fn finish(self) -> (Vec<u8>, bool) {
        let _ = self.done.recv_timeout(PIPE_GRACE);
        let guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        (guard.0.clone(), guard.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testutil::{fake_claude, pid_alive, write_scenario};
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
    fn flooding_output_is_capped_without_blocking_the_child() {
        let f = fixture(r#"{"rules":[{"args":[],"flood_bytes":5000000}]}"#, &[]);
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
}
