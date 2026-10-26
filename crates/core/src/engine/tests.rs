//! The engine's tests, run against `fake-claude` in a temporary configuration; helpers they share.
use super::*;
use crate::api::{Scope, ServerRow};
use crate::cli::testutil::{fake_claude, write_scenario};
use crate::config::inventory;
use crate::model::Target;
use std::ffi::OsStr;
use std::time::Instant;

/// Long enough for a slow machine: starting a process took 1-3 s while a whole run of these tests started them at once.
const WAIT: Duration = Duration::from_secs(60);

struct Env {
    dir: tempfile::TempDir,
    log: PathBuf,
    /// Links the engine asked to open in a browser. Tests never open one.
    opened: Arc<Mutex<Vec<String>>>,
    /// What the save dialog answers. Tests never show one.
    chosen: Arc<Mutex<Option<PathBuf>>>,
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
        Env {
            dir,
            log,
            opened: Arc::default(),
            chosen: Arc::default(),
        }
    }

    fn config(&self) -> EngineConfig {
        EngineConfig {
            paths: Some(ConfigPaths {
                // The organisation's folder, empty unless a test writes `org/…`.
                managed_dir: Some(self.dir.path().join("org")),
                ..ConfigPaths::in_dir(self.dir.path().join("cfg"))
            }),
            claude: Some(fake_claude().to_path_buf()),
            home: Some(self.dir.path().join("home")),
            path_var: None,
            vars: std::sync::Arc::new(|_: &str| None),
            cli_env: vec![
                (
                    "FAKE_CLAUDE_SCENARIO".into(),
                    self.dir.path().join("scenario.json").into(),
                ),
                ("FAKE_CLAUDE_LOG".into(), self.log.clone().into()),
            ],
            backup_dir: Some(self.dir.path().join("backups")),
            verify_delay: Duration::from_millis(50),
            max_checks: 3,
            check_timeout: Duration::from_secs(10),
            // A test's commands are not real servers: only the tests of direct checks start them.
            direct_checks: false,
            server_env: Vec::new(),
            background_checks: true,
            watch_interval: Some(Duration::from_millis(40)),
            watch_settle: Duration::from_millis(120),
            // Never the real keychain in tests.
            secrets: Some(Arc::new(crate::secrets::MemoryStore::default())),
            open_url: {
                let opened = Arc::clone(&self.opened);
                Arc::new(move |link: &str| {
                    lock(&opened).push(link.to_owned());
                    Ok(())
                })
            },
            open_dialog: {
                let chosen = Arc::clone(&self.chosen);
                Arc::new(move |_, _| Box::pin(std::future::ready(lock(&chosen).clone())))
            },
            pick_dialog: {
                let chosen = Arc::clone(&self.chosen);
                Arc::new(move |_| Box::pin(std::future::ready(lock(&chosen).clone())))
            },
            sign_in_timeout: Duration::from_secs(20),
            changer: None,
            desktop: crate::desktop::DesktopRoots::default(),
            claude_logs: None,
            releases: Arc::new(crate::update::Offline),
            release_key: None,
            install: crate::install::InstallKind::Manual("Updated by hand in tests.".into()),
            updates_dir: Some(self.dir.path().join("updates")),
            // The release before the test release (0.2.0).
            version: crate::update::Version::parse("0.1.0").unwrap(),
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
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad log line {l:?}: {e}")))
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

    /// The folder of every `mcp list` the fake CLI was asked to run, in order.
    fn lists(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|l| l["event"] == "start" && l["argv"] == serde_json::json!(["mcp", "list"]))
            .map(|l| l["cwd"].as_str().unwrap_or_default().to_owned())
            .collect()
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
        Event::Status {
            id, status, detail, ..
        } if id.name == name => Some((*status, detail.clone())),
        _ => None,
    })
}

fn final_status(events: &[Event], name: &str) -> Option<Status> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Status { id, status, .. } if id.name == name && *status != Status::Checking => {
                Some(*status)
            }
            _ => None,
        })
        .next_back()
}

const CONNECTED: &[u8] = include_bytes!("../../../../fixtures/mcp-get/connected-stdio.txt");
const FAILED: &[u8] = include_bytes!("../../../../fixtures/mcp-get/failed-stdio.txt");
const PENDING: &[u8] = include_bytes!("../../../../fixtures/mcp-get/pending-approval.txt");

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

fn config_json(env: &Env) -> serde_json::Value {
    let text = std::fs::read_to_string(env.dir.path().join("cfg").join(".claude.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn write_config(env: &Env, value: &serde_json::Value) {
    std::fs::write(
        env.dir.path().join("cfg").join(".claude.json"),
        serde_json::to_string_pretty(value).unwrap(),
    )
    .unwrap();
}

/// Two projects, a user server with a secret, and a local server in one of them.
fn two_projects(env_dir_hint: &str) -> serde_json::Value {
    serde_json::json!({
        "numStartups": 3,
        "mcpServers": {"gw": {"type": "stdio", "command": "node", "env": {"TOKEN": "s3cret-value"}}},
        "projects": {
            "/work/a": {"mcpServers": {"loc": {"type": "stdio", "command": "x"}}, "disabledMcpServers": []},
            "/work/b": {"disabledMcpServers": ["other"]}
        },
        "tail": env_dir_hint
    })
}

fn quiet_env(claude_json: &serde_json::Value) -> Env {
    Env::new(claude_json, r#"{"rules":[]}"#, &[])
}

fn user_id(name: &str) -> ServerId {
    ServerId::new(Scope::User, None, name)
}

fn local_id(name: &str, project: &str) -> ServerId {
    ServerId::new(Scope::Local, Some(project), name)
}

fn plan_for(core: &Core, changes: Vec<Change>) -> PlanView {
    core.send(Command::Prepare(changes));
    let events = collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::PlanReady(_) | Event::PlanFailed(_)))
    });
    events
        .iter()
        .find_map(|e| match e {
            Event::PlanReady(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a plan, got {events:?}"))
}

fn disabled_list(env: &Env, project: &str) -> serde_json::Value {
    config_json(env)["projects"][project]["disabledMcpServers"].clone()
}

fn activity(core: &Core) -> crate::activity::Activity {
    core.send(Command::Activity);
    let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Activity(_))));
    events
        .into_iter()
        .find_map(|e| match e {
            Event::Activity(a) => Some(a),
            _ => None,
        })
        .unwrap()
}

/// Events that arrive within `window`, without waiting for any in particular.
fn events_for(core: &Core, window: Duration) -> Vec<Event> {
    let until = Instant::now() + window;
    let mut events = Vec::new();
    while Instant::now() < until {
        while let Some(e) = core.try_recv() {
            events.push(e);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    events
}

fn with_store(env: &Env, store: Arc<crate::secrets::MemoryStore>) -> Core {
    let mut config = env.config();
    config.secrets = Some(store);
    start_with(config, || {})
}

fn secrets_report(core: &Core) -> crate::secret_uses::Secrets {
    core.send(Command::Secrets);
    collect(core, |e| e.iter().any(|x| matches!(x, Event::Secrets(_))))
        .into_iter()
        .find_map(|e| match e {
            Event::Secrets(s) => Some(s),
            _ => None,
        })
        .unwrap()
}

/// Every file's text under `dir`, to look for a value that must not be there.
fn walk(dir: &Path) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push_str(&walk(&path));
        } else {
            out.push_str(&String::from_utf8_lossy(
                &std::fs::read(&path).unwrap_or_default(),
            ));
        }
    }
    out
}

const MCP_JSON: &str = r#"{"mcpServers":{"ps":{"type":"stdio","command":"node","args":["server.js","--port","9"],"env":{"NODE_OPTIONS":"--require ./hook.js"}}}}"#;

/// A repository with a `.mcp.json`, known to Claude as a project. Returns the environment, the project key as
/// the inventory spells it, and the repository path.
fn repo_env(claude_entry: serde_json::Value) -> (Env, String, PathBuf) {
    let env = quiet_env(&serde_json::json!({}));
    let repo = env.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join(".mcp.json"), MCP_JSON).unwrap();
    let key = repo.to_string_lossy().replace('\\', "/");
    write_config(
        &env,
        &serde_json::json!({"numStartups": 1, "projects": {key.clone(): claude_entry}}),
    );
    // The key as written: ids are made from the file's spellings, whatever normalising would make of them.
    (env, key, repo)
}

fn project_id(key: &str, name: &str) -> ServerId {
    ServerId::new(Scope::Project, Some(key), name)
}

fn approve(key: &str) -> Vec<Change> {
    vec![Change::Approve {
        id: project_id(key, "ps"),
    }]
}

fn apply_and_wait(core: &Core, plan: &PlanView) -> Vec<Event> {
    core.send(Command::Apply(plan.id));
    collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    })
}

/// The verify that runs shortly after a write (50 ms in tests) reports no failure.
fn verify_passes(core: &Core) {
    std::thread::sleep(Duration::from_millis(400));
    let mut late = Vec::new();
    while let Some(e) = core.try_recv() {
        late.push(e);
    }
    assert!(
        !late.iter().any(|e| matches!(e, Event::ApplyFailed { .. })),
        "{late:#?}"
    );
}

impl Env {
    /// How many times the fake CLI was started with these leading arguments.
    fn runs(&self, leading: &[&str]) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|l| l["event"] == "start")
            .filter(|l| {
                let argv = l["argv"].as_array().unwrap();
                argv.len() >= leading.len()
                    && leading.iter().zip(argv).all(|(want, got)| got == want)
            })
            .count()
    }
}

fn snapshots(events: &[Event]) -> Vec<&Snapshot> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Snapshot(s) => Some(&**s),
            _ => None,
        })
        .collect()
}

fn entry_of(core: &Core, id: ServerId) -> (Result<String, String>, bool) {
    core.send(Command::Inspect(id.clone()));
    let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Entry { .. })));
    events
        .into_iter()
        .find_map(|e| match e {
            Event::Entry {
                id: got,
                json,
                editable,
            } if got == id => Some((json, editable)),
            _ => None,
        })
        .unwrap()
}

fn plan_failure(core: &Core, changes: Vec<Change>) -> String {
    core.send(Command::Prepare(changes));
    let events = collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::PlanFailed(_) | Event::PlanReady(_)))
    });
    events
        .into_iter()
        .find_map(|e| match e {
            Event::PlanFailed(m) => Some(m),
            _ => None,
        })
        .expect("the plan was refused")
}

fn duplicate(id: ServerId, name: &str, targets: Vec<Target>) -> Change {
    Change::Duplicate {
        id,
        name: name.to_owned(),
        targets,
        adjust: Default::default(),
    }
}

fn add(name: &str, json: &str, targets: Vec<Target>) -> Change {
    Change::Add {
        name: name.to_owned(),
        json: json.to_owned(),
        targets,
        secret: None,
    }
}

fn migrate(project: &str) -> Change {
    Change::MigrateLegacyDisabled {
        project: project.to_owned(),
    }
}

/// /work/a has an old switch-off list naming the user server gw, its local loc and a name already off.
fn legacy_config() -> serde_json::Value {
    serde_json::json!({
        "numStartups": 3,
        "mcpServers": {"gw": {"type": "stdio", "command": "node", "env": {"TOKEN": "s3cret-value"}}},
        "projects": {
            "/work/a": {
                "mcpServers": {"loc": {"type": "stdio", "command": "x"}},
                "mcpServersDisabled": ["gw", "loc", "old"],
                "disabledMcpServers": ["old"],
                "allowedTools": []
            },
            "/work/b": {"mcpServersDisabled": ["gw"]},
            "/work/c": {"mcpServersDisabled": [1]}
        }
    })
}

/// Three user servers, on everywhere, in two projects.
fn designs_config() -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "gw": {"command": "gw"},
            "memory": {"command": "m"},
            "db": {"command": "d"}
        },
        "projects": {"/work/designs": {"allowedTools": []}, "/work/notes": {}}
    })
}

fn use_profile(name: &str, members: &[&str], project: Option<&str>, only: bool) -> Change {
    Change::UseProfile {
        name: name.to_owned(),
        members: members.iter().map(|m| user_id(m).connection).collect(),
        project: project.map(str::to_owned),
        only,
    }
}

fn stop_profile(name: &str, project: Option<&str>, delete: bool) -> Change {
    Change::StopProfile {
        name: name.to_owned(),
        project: project.map(str::to_owned),
        delete,
    }
}

fn rename(id: ServerId, name: &str) -> Change {
    Change::Rename {
        id,
        name: name.to_owned(),
    }
}

const OAUTH_DEF: &str = r#"{"type": "http", "url": "https://sso.example.com/mcp", "headers": {"X-Team": "${TEAM}"}, "oauth": {"clientId": "client-7", "callbackPort": 8123}}"#;
const CLIENT_SECRET: &str = "Client-S3cret-Value";

fn oauth_add(name: &str, targets: Vec<Target>, secret: Option<&str>) -> Change {
    Change::Add {
        name: name.to_owned(),
        json: OAUTH_DEF.to_owned(),
        targets,
        secret: secret.map(|s| crate::secrets::Secret::new(s.to_owned())),
    }
}

/// Claude Code's `mcp add-json` takes a moment and then says it added the server; the test plays Claude's part
/// and writes the entry while it runs, as the fake CLI cannot.
const ADD_JSON_OK: &str = r#"{"rules":[
    {"args":["mcp","add-json"],"delay_ms":1500,"stdout_file":"added.txt"}]}"#;

fn oauth_add_env(scenario: &str) -> (Env, String, PathBuf) {
    let env = Env::new(
        &serde_json::json!({}),
        scenario,
        &[
            ("added.txt", b"Added http MCP server sso2 to user config\n"),
            (
                "exists.txt",
                b"MCP server sso2 already exists in user config\n",
            ),
        ],
    );
    let repo = env.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let key = repo.to_string_lossy().replace('\\', "/");
    write_config(
        &env,
        &serde_json::json!({"numStartups": 1, "projects": {key.clone(): {"mcpServers": {}}}}),
    );
    (env, key, repo)
}

impl Env {
    /// The `start` lines of every `mcp add-json` the fake CLI ran.
    fn add_json_runs(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|l| l["event"] == "start" && l["argv"][1] == "add-json")
            .collect()
    }

    /// Waits until `mcp add-json` is running, then writes `entry` the way Claude Code would: in the user's list
    /// or the project's.
    fn claude_writes_when_asked(&self, project: Option<&str>, name: &str, entry: &str) {
        let deadline = Instant::now() + WAIT;
        while self.add_json_runs().is_empty() {
            assert!(Instant::now() < deadline, "add-json never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut config = config_json(self);
        let entry: serde_json::Value = serde_json::from_str(entry).unwrap();
        match project {
            None => config["mcpServers"][name] = entry,
            Some(p) => config["projects"][p]["mcpServers"][name] = entry,
        }
        write_config(self, &config);
    }
}

fn sha256(text: &str) -> String {
    use sha2::Digest;
    crate::hex::lower(&sha2::Sha256::digest(text.as_bytes()))
}

const SSO_ENTRY: &str = r#"{"type": "http", "url": "https://sso.example.com/mcp", "oauth": {"clientId": "client-7", "callbackPort": 8123}}"#;

/// sso, for the user and signing in with OAuth, with Klefki's copy of its client secret; and an empty project.
fn oauth_move_env(scenario: &str) -> (Env, Arc<crate::secrets::MemoryStore>, String, PathBuf) {
    let env = Env::new(
        &serde_json::json!({}),
        scenario,
        &[
            ("removed.txt", b"Removed MCP server sso from user config\n"),
            ("added.txt", b"Added http MCP server sso to local config\n"),
        ],
    );
    let repo = real(&env.dir.path().join("repo"));
    let key = repo.to_string_lossy().replace('\\', "/");
    let entry: serde_json::Value = serde_json::from_str(SSO_ENTRY).unwrap();
    write_config(
        &env,
        &serde_json::json!({"numStartups": 1, "mcpServers": {"sso": entry}, "projects": {key.clone(): {}}, "tail": 1}),
    );
    let store = Arc::new(crate::secrets::MemoryStore::default());
    store
        .set(
            &crate::model::ConnectionId::new(Scope::User, None, "sso"),
            &crate::secrets::Secret::new(CLIENT_SECRET.to_owned()),
        )
        .unwrap();
    (env, store, key, repo)
}

impl Env {
    /// The `start` lines of every command the fake CLI ran, as (argv, cwd, secret hash).
    fn changes_run(&self) -> Vec<(Vec<String>, String, Option<String>)> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|l| l["event"] == "start" && l["argv"][0] == "mcp" && l["argv"][1] != "get")
            .map(|l| {
                (
                    l["argv"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|a| a.as_str().unwrap().to_owned())
                        .collect(),
                    l["cwd"].as_str().unwrap_or_default().to_owned(),
                    l["secret_sha256"].as_str().map(str::to_owned),
                )
            })
            .collect()
    }

    /// Waits until the `n`th change command (`mcp remove` or `mcp add-json`) has started.
    fn wait_for_run(&self, n: usize) {
        let deadline = Instant::now() + WAIT;
        while self.changes_run().len() < n {
            assert!(
                Instant::now() < deadline,
                "run {n} never started: {:?}",
                self.changes_run()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Makes the folder `path` and spells it as Claude Code keys a project: the folder itself, not a link to it
/// (macOS's temporary folder is one).
fn real(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    return std::fs::canonicalize(path).unwrap();
    #[cfg(not(unix))]
    return path.to_path_buf();
}

fn applied(core: &Core, plan: &PlanView) -> Vec<Event> {
    core.send(Command::Apply(plan.id));
    let events = collect(core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. })),
        "{events:#?}"
    );
    events
}

/// The `.claude.json` backup of the newest change in the activity log, as the page lists it.
fn newest_backup(core: &Core) -> String {
    let log = activity(core);
    log.entries[0].backups[0].path.clone()
}

mod approvals;
mod changes;
mod checks;
mod client_secrets;
mod debug_report;
mod diagnose;
mod diagnose_in;
mod direct;
mod edits;
mod export;
mod golden;
mod inspector;
mod last_starts;
mod oauth;
mod one_project;
mod profiles;
mod reload;
mod renames;
mod restore;
mod sign_in;
mod spelling;
mod switch_profile;
mod timing;
mod undo;
mod updates;
