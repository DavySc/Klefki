//! The engine: one background thread owns the state, a small worker pool runs live status checks, and the UI
//! only sees events. The UI never touches files, processes or the network.
use crate::api::{Scope, ServerId, ServerRow, Snapshot, Status};
use crate::cli::locate;
use crate::cli::parse;
use crate::cli::runner::{self, RunSpec};
use crate::config::ConfigPaths;
use crate::config::inventory;
use crate::model::ServerName;
use crate::sanitize::{clip, sanitize};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Reload the configuration. With `live_all`, also re-check every checkable server, not just the ones whose
    /// status Claude does not already know.
    Refresh { live_all: bool },
    /// Check one server now. This is the only way a project (`.mcp.json`) server is checked, because checking
    /// it runs the repository's command.
    Check(ServerId),
}

#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Arc<Snapshot>),
    CliVersion(Option<String>),
    /// The CLI could not be found or used; live checks are unavailable.
    CliProblem(String),
    Status {
        id: ServerId,
        status: Status,
        detail: Option<String>,
    },
}

/// The UI's handle to the engine.
pub struct Core {
    tx: Sender<Command>,
    rx: Receiver<Event>,
}

impl Core {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// The next event, if one is ready. Never blocks.
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// A `Core` that is not connected to an engine, for testing code that talks to one: the test plays the
    /// engine's part through [`Detached`].
    #[doc(hidden)]
    pub fn detached() -> (Core, Detached) {
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        (Core { tx, rx }, Detached { commands, events })
    }
}

/// The engine's side of a [`Core::detached`] pair.
#[doc(hidden)]
pub struct Detached {
    pub commands: Receiver<Command>,
    pub events: Sender<Event>,
}

/// Everything the engine needs. Passed in, so tests never depend on the real environment.
pub(crate) struct EngineConfig {
    /// Where Claude keeps its files; `None` when the home folder cannot be found.
    pub paths: Option<ConfigPaths>,
    /// A user-configured path to the Claude CLI.
    pub claude: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub path_var: Option<OsString>,
    /// Extra environment for the CLI. Used by tests to point `fake-claude` at a scenario.
    pub cli_env: Vec<(String, OsString)>,
    pub max_checks: usize,
    pub check_timeout: Duration,
}

const MAX_CHECK_OUTPUT: usize = 64 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DETAIL_CHARS: usize = 200;

pub(crate) fn start_with(config: EngineConfig, wake: impl Fn() + Send + Sync + 'static) -> Core {
    let (tx, cmd_rx) = mpsc::channel();
    let (ev_tx, rx) = mpsc::channel();
    let emitter = Emitter {
        tx: ev_tx,
        wake: Arc::new(wake),
    };
    std::thread::spawn(move || engine_main(config, cmd_rx, emitter));
    Core { tx, rx }
}

#[derive(Clone)]
struct Emitter {
    tx: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Emitter {
    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.wake)();
    }

    fn status(&self, id: ServerId, status: Status, detail: Option<String>) {
        self.emit(Event::Status { id, status, detail });
    }
}

struct Job {
    generation: u64,
    id: ServerId,
    /// Already validated; it is passed to the CLI as its own argument.
    name: ServerName,
    cwd: PathBuf,
    /// Tell the UI the check is running (only for rows that have nothing better to show meanwhile).
    announce: bool,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    shutdown: bool,
}

struct Shared {
    config: EngineConfig,
    queue: Mutex<Queue>,
    wake_workers: Condvar,
    generation: AtomicU64,
    claude: RwLock<Option<PathBuf>>,
    emitter: Emitter,
}

fn engine_main(config: EngineConfig, commands: Receiver<Command>, emitter: Emitter) {
    let workers = config.max_checks.max(1);
    let shared = Arc::new(Shared {
        config,
        queue: Mutex::new(Queue::default()),
        wake_workers: Condvar::new(),
        generation: AtomicU64::new(0),
        claude: RwLock::new(None),
        emitter,
    });
    for _ in 0..workers {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || worker(&shared));
    }

    let mut snapshot = Arc::new(Snapshot::default());
    refresh(&shared, &mut snapshot, false);
    while let Ok(command) = commands.recv() {
        match command {
            Command::Refresh { live_all } => refresh(&shared, &mut snapshot, live_all),
            Command::Check(id) => check_now(&shared, &snapshot, &id),
        }
    }
    // The UI is gone: let the workers finish what they are doing and stop.
    lock(&shared.queue).shutdown = true;
    shared.wake_workers.notify_all();
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn refresh(shared: &Shared, snapshot: &mut Arc<Snapshot>, live_all: bool) {
    let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
    lock(&shared.queue).jobs.clear();

    *snapshot = match &shared.config.paths {
        Some(paths) => Arc::new(inventory::load(paths)),
        None => Arc::new(Snapshot {
            warnings: vec![
                "Could not determine where Claude Code keeps its configuration.".to_owned(),
            ],
            ..Snapshot::default()
        }),
    };
    shared.emitter.emit(Event::Snapshot(Arc::clone(snapshot)));

    let cli = locate::locate(&locate::Search {
        explicit: shared.config.claude.as_deref(),
        home: shared.config.home.as_deref(),
        path_var: shared.config.path_var.as_deref(),
    });
    let program = match cli {
        Ok(p) => p,
        Err(e) => {
            *shared.claude.write().unwrap_or_else(|e| e.into_inner()) = None;
            shared.emitter.emit(Event::CliProblem(format!(
                "Live status is unavailable: {e}."
            )));
            return;
        }
    };
    *shared.claude.write().unwrap_or_else(|e| e.into_inner()) = Some(program.clone());
    shared
        .emitter
        .emit(Event::CliVersion(cli_version(shared, &program)));

    let mut jobs = Vec::new();
    for row in &snapshot.servers {
        // Project servers run the repository's own command: only on an explicit request.
        let wanted = row.scope != Scope::Project
            && row.enabled
            && (live_all || row.status == Status::Unknown);
        if wanted
            && let Some(job) = build_job(shared, row, generation, row.status == Status::Unknown)
        {
            jobs.push(job);
        }
    }
    // Servers with nothing to show yet go first.
    jobs.sort_by_key(|j| !j.announce);
    lock(&shared.queue).jobs.extend(jobs);
    shared.wake_workers.notify_all();
}

fn check_now(shared: &Shared, snapshot: &Snapshot, id: &ServerId) {
    let Some(row) = snapshot.servers.iter().find(|r| r.id() == *id) else {
        return;
    };
    let generation = shared.generation.load(Ordering::SeqCst);
    if let Some(job) = build_job(shared, row, generation, true) {
        lock(&shared.queue).jobs.push_front(job);
        shared.wake_workers.notify_one();
    }
}

/// Turns a row into a job, or reports why it cannot be checked. A name that is not safe never reaches a spawn.
fn build_job(shared: &Shared, row: &ServerRow, generation: u64, announce: bool) -> Option<Job> {
    let refuse = |why: String| {
        shared.emitter.status(
            row.id(),
            Status::Unknown,
            Some(format!("not checked: {why}")),
        );
        None
    };
    let name = match ServerName::parse(&row.name) {
        Ok(n) => n,
        Err(e) => {
            return refuse(format!(
                "the server name is not safe to pass to claude ({e})"
            ));
        }
    };
    let cwd = match (row.scope, &row.project) {
        (Scope::User, _) => match &shared.config.home {
            Some(home) => home.clone(),
            None => return refuse("no home folder".to_owned()),
        },
        (_, Some(project)) => {
            if !inventory::is_safe_project_path(project) || !Path::new(project).is_dir() {
                return refuse("the project folder was not found".to_owned());
            }
            PathBuf::from(project)
        }
        (_, None) => return refuse("the project folder is unknown".to_owned()),
    };
    Some(Job {
        generation,
        id: row.id(),
        name,
        cwd,
        announce,
    })
}

fn worker(shared: &Shared) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared
                    .wake_workers
                    .wait(queue)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let current = || shared.generation.load(Ordering::SeqCst) == job.generation;
        if !current() {
            continue;
        }
        if job.announce {
            shared
                .emitter
                .status(job.id.clone(), Status::Checking, None);
        }
        let (status, detail) = execute(shared, &job);
        // A refresh may have replaced the world while this ran; its answer would be about a stale row.
        if current() {
            shared.emitter.status(job.id.clone(), status, detail);
        }
    }
}

fn cli_env(shared: &Shared) -> Vec<(&str, &OsStr)> {
    shared
        .config
        .cli_env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect()
}

fn cli_version(shared: &Shared, program: &Path) -> Option<String> {
    let out = runner::run(&RunSpec {
        program,
        args: &["--version"],
        cwd: shared.config.home.as_deref(),
        env: &cli_env(shared),
        timeout: VERSION_TIMEOUT,
        max_output: 1024,
    })
    .ok()?;
    let token = out.stdout.split_whitespace().next()?;
    (out.exit_code == Some(0) && token.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| clip(token, 32))
}

fn execute(shared: &Shared, job: &Job) -> (Status, Option<String>) {
    let program = shared
        .claude
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(program) = program else {
        return (
            Status::Unknown,
            Some("the claude CLI is not available".to_owned()),
        );
    };
    let timeout = shared.config.check_timeout;
    let result = runner::run(&RunSpec {
        program: &program,
        args: &["mcp", "get", job.name.as_str()],
        cwd: Some(&job.cwd),
        env: &cli_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
    });
    match result {
        Err(e) => (
            Status::Unknown,
            Some(clip(
                &format!("could not run claude: {}", sanitize(&e.to_string())),
                MAX_DETAIL_CHARS,
            )),
        ),
        Ok(out) if out.timed_out => (
            Status::Unknown,
            Some(format!(
                "check timed out after {:.1}s",
                timeout.as_secs_f32()
            )),
        ),
        Ok(out) if out.exit_code != Some(0) => {
            let line = out
                .stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| clip(l, MAX_DETAIL_CHARS))
                .unwrap_or_else(|| format!("claude exited with code {:?}", out.exit_code));
            (Status::Unknown, Some(line))
        }
        Ok(out) => {
            let checked = parse::parse_get(&out.stdout);
            if checked.status == Status::Unknown && checked.detail.is_none() {
                (
                    Status::Unknown,
                    Some("unrecognised output from claude".to_owned()),
                )
            } else {
                (checked.status, checked.detail)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testutil::{fake_claude, write_scenario};
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(15);

    struct Env {
        dir: tempfile::TempDir,
        log: PathBuf,
    }

    impl Env {
        fn new(claude_json: &serde_json::Value, scenario: &str, files: &[(&str, &[u8])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("cfg")).unwrap();
            std::fs::create_dir_all(dir.path().join("home")).unwrap();
            std::fs::write(
                dir.path().join("cfg").join(".claude.json"),
                claude_json.to_string(),
            )
            .unwrap();
            write_scenario(dir.path(), scenario, files);
            let log = dir.path().join("log.jsonl");
            Env { dir, log }
        }

        fn config(&self) -> EngineConfig {
            EngineConfig {
                paths: Some(ConfigPaths::in_dir(self.dir.path().join("cfg"))),
                claude: Some(fake_claude().to_path_buf()),
                home: Some(self.dir.path().join("home")),
                path_var: None,
                cli_env: vec![
                    (
                        "FAKE_CLAUDE_SCENARIO".into(),
                        self.dir.path().join("scenario.json").into(),
                    ),
                    ("FAKE_CLAUDE_LOG".into(), self.log.clone().into()),
                ],
                max_checks: 3,
                check_timeout: Duration::from_secs(10),
            }
        }

        fn start(&self) -> Core {
            start_with(self.config(), || {})
        }

        /// Every `mcp get <name>` the fake CLI was asked to run, with its start and end times.
        fn gets(&self) -> Vec<(String, u64, u64)> {
            let lines: Vec<serde_json::Value> = std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let mut out = Vec::new();
            for start in lines.iter().filter(|l| l["event"] == "start") {
                let argv = start["argv"].as_array().unwrap();
                if argv.len() == 3 && argv[0] == "mcp" && argv[1] == "get" {
                    let end = lines
                        .iter()
                        .find(|l| l["event"] == "end" && l["argv"] == start["argv"])
                        .map(|l| l["ms"].as_u64().unwrap())
                        .unwrap_or(u64::MAX);
                    out.push((
                        argv[2].as_str().unwrap().to_owned(),
                        start["ms"].as_u64().unwrap(),
                        end,
                    ));
                }
            }
            out
        }
    }

    /// Collects events until `done` says so, or panics after `WAIT`.
    fn collect(core: &Core, mut done: impl FnMut(&[Event]) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + WAIT;
        let mut events = Vec::new();
        loop {
            while let Some(e) = core.try_recv() {
                events.push(e);
            }
            if done(&events) {
                return events;
            }
            assert!(Instant::now() < deadline, "timed out; got {events:#?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn status_of(events: &[Event], name: &str) -> Option<(Status, Option<String>)> {
        events.iter().rev().find_map(|e| match e {
            Event::Status { id, status, detail } if id.name == name => {
                Some((*status, detail.clone()))
            }
            _ => None,
        })
    }

    fn final_status(events: &[Event], name: &str) -> Option<Status> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Status { id, status, .. }
                    if id.name == name && *status != Status::Checking =>
                {
                    Some(*status)
                }
                _ => None,
            })
            .next_back()
    }

    const CONNECTED: &[u8] = include_bytes!("../../../fixtures/mcp-get/connected-stdio.txt");
    const FAILED: &[u8] = include_bytes!("../../../fixtures/mcp-get/failed-stdio.txt");
    const PENDING: &[u8] = include_bytes!("../../../fixtures/mcp-get/pending-approval.txt");

    fn http(url: &str) -> serde_json::Value {
        serde_json::json!({"type": "http", "url": url})
    }

    #[test]
    fn startup_loads_the_snapshot_finds_the_cli_and_checks_unknown_servers() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "azure-devops": {"type": "stdio", "command": "npx"},
                "broken": {"type": "stdio", "command": "nope"}}}),
            r#"{"rules":[
                {"args":["--version"],"stdout_file":"version.txt"},
                {"args":["mcp","get","azure-devops"],"stdout_file":"connected.txt"},
                {"args":["mcp","get","broken"],"stdout_file":"failed.txt"}]}"#,
            &[
                ("version.txt", b"2.1.302 (Claude Code)\n"),
                ("connected.txt", CONNECTED),
                ("failed.txt", FAILED),
            ],
        );
        let core = env.start();
        let events = collect(&core, |e| {
            final_status(e, "azure-devops").is_some()
                && final_status(e, "broken").is_some()
                && e.iter().any(|x| matches!(x, Event::CliVersion(Some(_))))
        });
        assert!(
            matches!(&events[0], Event::Snapshot(s) if s.servers.len() == 2),
            "the snapshot comes first: {:?}",
            events[0]
        );
        assert_eq!(
            final_status(&events, "azure-devops"),
            Some(Status::Connected)
        );
        let (st, detail) = status_of(&events, "broken").unwrap();
        assert_eq!(st, Status::Failed);
        assert_eq!(
            detail.as_deref(),
            Some("CONNECTION_CLOSED: Connection closed")
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CliVersion(Some(v)) if v == "2.1.302"))
        );
        // Rows with no cached status show "checking" while they wait.
        assert_eq!(
            events.iter().find_map(|e| match e {
                Event::Status { id, status, .. } if id.name == "azure-devops" => Some(*status),
                _ => None,
            }),
            Some(Status::Checking)
        );
    }

    #[test]
    fn servers_claude_already_knows_are_not_rechecked_until_a_full_refresh() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "known": http("https://a.example.com"),
                "fresh": {"type": "stdio", "command": "x"}}}),
            r#"{"rules":[{"args":["mcp","get","known"],"stdout_file":"c.txt"},
                        {"args":["mcp","get","fresh"],"stdout_file":"c.txt"},
                        {"args":["--version"]}]}"#,
            &[("c.txt", CONNECTED)],
        );
        std::fs::write(
            env.dir.path().join("cfg").join("mcp-health-cache.json"),
            r#"{"version":1,"servers":{"known":{"status":"healthy"}}}"#,
        )
        .unwrap();
        let core = env.start();
        collect(&core, |e| final_status(e, "fresh").is_some());
        let names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(
            names,
            vec!["fresh"],
            "only the unknown server is checked at startup"
        );

        core.send(Command::Refresh { live_all: true });
        collect(&core, |_| env.gets().iter().any(|g| g.0 == "known"));
        let mut names: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        names.sort();
        assert!(names.contains(&"known".to_owned()));
    }

    #[test]
    fn project_servers_are_only_checked_on_request_and_run_in_their_folder() {
        let env = Env::new(
            &serde_json::json!({}),
            r#"{"rules":[{"args":["--version"]},
            {"args":["mcp","get","docs"],"stdout_file":"p.txt"}]}"#,
            &[("p.txt", PENDING)],
        );
        let project = env.dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"node"}}}"#,
        )
        .unwrap();
        std::fs::write(
            env.dir.path().join("cfg").join(".claude.json"),
            serde_json::json!({"projects": {project.to_string_lossy(): {}}}).to_string(),
        )
        .unwrap();
        let core = env.start();
        let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
        std::thread::sleep(Duration::from_millis(600));
        assert!(env.gets().is_empty(), "nothing runs until the user asks");
        let Event::Snapshot(snap) = &events[0] else {
            panic!()
        };
        let row = snap
            .servers
            .iter()
            .find(|r| r.scope == Scope::Project)
            .unwrap();

        core.send(Command::Check(row.id()));
        let events = collect(&core, |e| final_status(e, "docs").is_some());
        assert_eq!(final_status(&events, "docs"), Some(Status::PendingApproval));
        let log = std::fs::read_to_string(&env.log).unwrap();
        let start: serde_json::Value = log
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|l| l["event"] == "start" && l["argv"][2] == "docs")
            .unwrap();
        assert_eq!(
            std::path::Path::new(start["cwd"].as_str().unwrap()).file_name(),
            project.file_name()
        );
    }

    #[test]
    fn a_name_that_is_not_safe_never_reaches_the_cli() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "a&b": {"command": "x"}, "--help": {"command": "x"}, "ok": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get","ok"],"stdout_file":"c.txt"}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        let events = collect(&core, |e| {
            ["a&b", "--help", "ok"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        assert_eq!(final_status(&events, "ok"), Some(Status::Connected));
        for bad in ["a&b", "--help"] {
            let (status, detail) = status_of(&events, bad).unwrap();
            assert_eq!(status, Status::Unknown, "{bad}");
            assert!(detail.unwrap().contains("not checked"), "{bad}");
        }
        let asked: Vec<String> = env.gets().into_iter().map(|g| g.0).collect();
        assert_eq!(
            asked,
            vec!["ok"],
            "only the valid name was ever passed to the CLI"
        );
    }

    #[test]
    fn at_most_three_checks_run_at_once() {
        let servers: serde_json::Map<String, serde_json::Value> = (0..9)
            .map(|i| (format!("s{i}"), serde_json::json!({"command": "x"})))
            .collect();
        let env = Env::new(
            &serde_json::json!({ "mcpServers": servers }),
            r#"{"rules":[{"args":["--version"]},{"args":["mcp","get"],"stdout_file":"c.txt","delay_ms":400}]}"#,
            &[("c.txt", CONNECTED)],
        );
        let core = env.start();
        collect(&core, |e| {
            (0..9).all(|i| final_status(e, &format!("s{i}")).is_some())
        });
        let gets = env.gets();
        assert_eq!(gets.len(), 9);
        let max_overlap = gets
            .iter()
            .map(|(_, start, _)| {
                gets.iter()
                    .filter(|(_, s, e)| s <= start && start < e)
                    .count()
            })
            .max()
            .unwrap();
        assert!(max_overlap <= 3, "{max_overlap} checks overlapped");
        assert!(
            max_overlap >= 2,
            "the pool should actually run in parallel, saw {max_overlap}"
        );
    }

    #[test]
    fn cli_failures_become_unknown_with_a_reason() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {
                "missing": {"command": "x"}, "slow": {"command": "x"}, "garbage": {"command": "x"}}}),
            r#"{"rules":[{"args":["--version"]},
                {"args":["mcp","get","missing"],"stderr_file":"nf.txt","exit_code":1},
                {"args":["mcp","get","slow"],"hang":true},
                {"args":["mcp","get","garbage"],"stdout_file":"g.txt"}]}"#,
            &[
                ("nf.txt", b"No MCP server named \"missing\"."),
                ("g.txt", b"something completely different\n"),
            ],
        );
        let mut config = env.config();
        config.check_timeout = Duration::from_millis(800);
        let core = start_with(config, || {});
        let events = collect(&core, |e| {
            ["missing", "slow", "garbage"]
                .iter()
                .all(|n| final_status(e, n).is_some())
        });
        for (name, needle) in [
            ("missing", "No MCP server named"),
            ("slow", "timed out"),
            ("garbage", "unrecognised"),
        ] {
            let (status, detail) = status_of(&events, name).unwrap();
            assert_eq!(status, Status::Unknown, "{name}");
            let detail = detail.unwrap_or_default();
            assert!(detail.contains(needle), "{name}: {detail:?}");
        }
    }

    #[test]
    fn a_missing_cli_is_reported_and_nothing_crashes() {
        let env = Env::new(
            &serde_json::json!({"mcpServers": {"a": {"command": "x"}}}),
            r#"{"rules":[]}"#,
            &[],
        );
        let mut config = env.config();
        config.claude = None;
        config.path_var = Some(env.dir.path().join("empty").into());
        let core = start_with(config, || {});
        let events = collect(&core, |e| {
            e.iter().any(|x| matches!(x, Event::CliProblem(_)))
        });
        assert!(matches!(&events[0], Event::Snapshot(s) if s.servers.len() == 1));
        assert!(
            status_of(&events, "a").is_none(),
            "no check can run without the CLI"
        );
    }

    #[test]
    fn a_detached_core_carries_commands_and_events_both_ways() {
        let (core, engine) = Core::detached();
        core.send(Command::Refresh { live_all: true });
        assert!(matches!(
            engine.commands.try_recv(),
            Ok(Command::Refresh { live_all: true })
        ));
        assert!(core.try_recv().is_none());
        engine
            .events
            .send(Event::CliVersion(Some("1.2.3".to_owned())))
            .unwrap();
        assert!(matches!(core.try_recv(), Some(Event::CliVersion(Some(v))) if v == "1.2.3"));
    }

    #[test]
    fn the_ui_is_woken_when_events_arrive() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let env = Env::new(
            &serde_json::json!({}),
            r#"{"rules":[{"args":["--version"]}]}"#,
            &[],
        );
        let woken = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&woken);
        let core = start_with(env.config(), move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        collect(&core, |e| !e.is_empty());
        assert!(woken.load(Ordering::SeqCst) >= 1);
    }
}
