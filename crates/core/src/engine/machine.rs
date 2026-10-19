//! The model-based test of change operations that PLAN.md asks for ("Verification"). Random sequences of edits,
//! moves, copies, renames, deletes, switches and put-backs run through the engine, as the app sends them, against a
//! stand-in for Claude Code ([`Sim`]) that fails, times out, never starts or is overtaken by an outside write at any
//! of its steps. After every step the files, Klefki's secret store and Claude Code's own secret store are read back
//! and checked:
//! - no server is lost: each is where it was, or where the change put it, or kept in a recovery file, and then its
//!   client secret is still in Klefki's store under its connection;
//! - a change that was reported as done did exactly what was asked, and one that was refused changed nothing;
//! - every server that signs in with OAuth and has a client secret in Klefki's store has the same one in Claude Code's,
//!   also when Klefki's store fails to take or delete a copy part way;
//! - putting back or forgetting a kept definition works when nothing fails (a refusal would be "safe" and useless);
//! - a secret never appears in a command's arguments, and travels only with `--client-secret`;
//! - the configuration is valid JSON, its other keys and their order are kept, and nothing is written in project
//!   scope (no `.mcp.json`, no settings file).
//!
//! The model is the world as read back after each step (`World`); what a step may change is predicted from the
//! step and the outcome the engine reported. proptest shrinks a failing sequence and keeps it in
//! `proptest-regressions/engine/machine.txt`, which runs first every time.
use super::*;
use crate::model::Target;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

/// What the stand-in does with its next command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Does what it is asked.
    Works,
    /// Says why not and changes nothing.
    FailsBefore,
    /// Changes the file, then fails anyway.
    FailsAfter,
    /// Is stopped before it changed anything.
    TimesOutBefore,
    /// Is stopped after it changed the file.
    TimesOutAfter,
    /// Cannot be started.
    NeverStarts,
    /// Another program writes the file first; then it works.
    Overtaken,
}

/// What Klefki's secret store does with the copies a change moves: it reads always.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreFault {
    Works,
    /// Taking a copy fails.
    SetFails,
    /// Deleting a copy fails (after the new one was taken).
    DeleteFails,
}

/// What another program does while Klefki is at work.
type Meanwhile = Box<dyn FnOnce() + Send>;

/// Klefki's secret store, failing as told.
#[derive(Default)]
struct Flaky {
    inner: crate::secrets::MemoryStore,
    fault: Mutex<Option<StoreFault>>,
    /// Runs once, on the next read of that connection: another program at work in between.
    on_get: Mutex<Option<(ConnectionId, Meanwhile)>>,
}

impl crate::secrets::SecretStore for Flaky {
    fn get(&self, id: &ConnectionId) -> Result<Option<Secret>, crate::secrets::SecretError> {
        let meanwhile = {
            let mut on_get = lock(&self.on_get);
            match on_get.take() {
                Some((at, f)) if at == *id => Some(f),
                other => {
                    *on_get = other;
                    None
                }
            }
        };
        if let Some(meanwhile) = meanwhile {
            meanwhile();
        }
        self.inner.get(id)
    }

    fn set(&self, id: &ConnectionId, secret: &Secret) -> Result<(), crate::secrets::SecretError> {
        if *lock(&self.fault) == Some(StoreFault::SetFails) {
            return Err(crate::secrets::SecretError::Failed("injected".into()));
        }
        self.inner.set(id, secret)
    }

    fn delete(&self, id: &ConnectionId) -> Result<bool, crate::secrets::SecretError> {
        if *lock(&self.fault) == Some(StoreFault::DeleteFails) {
            return Err(crate::secrets::SecretError::Failed("injected".into()));
        }
        self.inner.delete(id)
    }

    fn list(&self) -> Result<Vec<ConnectionId>, crate::secrets::SecretError> {
        self.inner.list()
    }
}

/// The stand-in for Claude Code's `mcp add-json` and `mcp remove`: it changes `.claude.json` as Claude Code does
/// (under its lock, keeping only the settings it knows, a local server in the project it runs in) and keeps its own
/// client secrets as Claude Code does, one per name, type, address and headers, whatever the place.
struct Sim {
    config: PathBuf,
    faults: Mutex<VecDeque<Fault>>,
    /// Claude Code's own store: sign-in key to client secret.
    creds: Mutex<BTreeMap<String, String>>,
    /// Every command: its arguments and the secret it was handed.
    calls: Mutex<Vec<(Vec<String>, Option<String>)>>,
}

const KNOWN: [&str; 9] = [
    "type",
    "url",
    "headers",
    "headersHelper",
    "oauth",
    "command",
    "args",
    "env",
    "timeout",
];

fn sign_in_key(name: &str, entry: &Value) -> String {
    let headers = entry.get("headers").cloned().unwrap_or_else(|| json!({}));
    format!(
        "{name}|{}",
        json!({"type": entry.get("type"), "url": entry.get("url"), "headers": headers})
    )
}

impl Sim {
    fn with_lock<T>(&self, f: impl FnOnce(&mut Value) -> T) -> T {
        let mut lock = self.config.clone().into_os_string();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        let started = Instant::now();
        while std::fs::create_dir(&lock).is_err() {
            assert!(started.elapsed() < Duration::from_secs(10), "lock held");
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut root: Value =
            serde_json::from_str(&std::fs::read_to_string(&self.config).unwrap()).unwrap();
        let out = f(&mut root);
        let tmp = self.config.with_extension("sim-tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&root).unwrap()).unwrap();
        std::fs::rename(&tmp, &self.config).unwrap();
        std::fs::remove_dir(&lock).unwrap();
        out
    }

    /// Another program changes values klefki does not touch.
    fn outside_write(&self) {
        self.with_lock(|root| {
            let n = root["numStartups"].as_u64().unwrap_or(0);
            root["numStartups"] = json!(n + 1);
        });
    }

    /// Claude Code's own work, or its refusal.
    fn work(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        secret: Option<&Secret>,
    ) -> Result<(), String> {
        let (verb, rest) = match args {
            ["mcp", verb, "-s", scope, rest @ ..] => (*verb, (*scope, rest)),
            _ => return Err(format!("unknown command {args:?}")),
        };
        let (scope, rest) = rest;
        let (client_secret, rest) = match rest {
            ["--client-secret", rest @ ..] => (true, rest),
            rest => (false, rest),
        };
        let project = match scope {
            "user" => None,
            "local" => Some(
                cwd.expect("local runs in its project")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
            _ => return Err("bad scope".into()),
        };
        match (verb, rest) {
            ("add-json", ["--", name, json]) => {
                let given: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
                let kept: serde_json::Map<String, Value> = given
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(k, _)| KNOWN.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let entry = Value::Object(kept);
                self.with_lock(|root| {
                    let servers = servers(root, project.as_deref());
                    if servers.get(*name).is_some() {
                        return Err(format!(
                            "MCP server {name} already exists in {scope} config"
                        ));
                    }
                    servers[*name] = entry.clone();
                    Ok(())
                })?;
                if client_secret && let Some(secret) = secret {
                    lock(&self.creds).insert(sign_in_key(name, &entry), secret.expose().to_owned());
                }
                Ok(())
            }
            ("remove", ["--", name]) => {
                let old = self.with_lock(|root| {
                    servers(root, project.as_deref())
                        .as_object_mut()
                        .unwrap()
                        .shift_remove(*name)
                        .ok_or_else(|| format!("No MCP server found with name: {name}"))
                })?;
                lock(&self.creds).remove(&sign_in_key(name, &old));
                Ok(())
            }
            _ => Err(format!("unknown command {args:?}")),
        }
    }
}

/// The server list of `project` (`None`: the user's), made where missing, as Claude Code does.
fn servers<'a>(root: &'a mut Value, project: Option<&str>) -> &'a mut Value {
    let owner = match project {
        None => root,
        Some(p) => {
            if root["projects"].get(p).is_none() {
                root["projects"][p] = json!({});
            }
            &mut root["projects"][p]
        }
    };
    if owner.get("mcpServers").is_none() {
        owner["mcpServers"] = json!({});
    }
    &mut owner["mcpServers"]
}

impl swap::Claude for Sim {
    fn run(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        secret: Option<&Secret>,
    ) -> io::Result<RunOutput> {
        lock(&self.calls).push((
            args.iter().map(|a| (*a).to_owned()).collect(),
            secret.map(|s| s.expose().to_owned()),
        ));
        let fault = lock(&self.faults).pop_front().unwrap_or(Fault::Works);
        if fault == Fault::NeverStarts {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such program"));
        }
        if fault == Fault::Overtaken {
            self.outside_write();
        }
        let worked = match fault {
            Fault::FailsBefore | Fault::TimesOutBefore => Ok(()),
            _ => self.work(args, cwd, secret),
        };
        let timed_out = matches!(fault, Fault::TimesOutBefore | Fault::TimesOutAfter);
        let (exit_code, stderr) = match (&worked, fault) {
            _ if timed_out => (None, String::new()),
            (_, Fault::FailsBefore | Fault::FailsAfter) => {
                (Some(1), "something went wrong".to_owned())
            }
            (Ok(()), _) => (Some(0), String::new()),
            (Err(why), _) => (Some(1), why.clone()),
        };
        Ok(RunOutput {
            stdout: String::new(),
            stderr,
            exit_code,
            timed_out,
            truncated: false,
        })
    }
}

/// One change, as the app could ask for it. Servers are picked by their position in the list as it is then.
#[derive(Debug, Clone)]
enum Step {
    Edit {
        pick: usize,
        oauth: bool,
        url: u8,
        client: u8,
    },
    Move {
        pick: usize,
        to: usize,
    },
    Copy {
        pick: usize,
        to: usize,
        rename: bool,
    },
    Rename {
        pick: usize,
    },
    Delete {
        pick: usize,
    },
    Toggle {
        pick: usize,
        on: bool,
    },
    PutBack {
        pick: usize,
    },
    /// A copy of a server into the place a kept definition came from, under its name: random steps rarely aim there.
    CopyToKept {
        pick: usize,
        kept: usize,
    },
    Forget {
        pick: usize,
    },
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        6 => Just(Fault::Works),
        1 => Just(Fault::FailsBefore),
        1 => Just(Fault::FailsAfter),
        1 => Just(Fault::TimesOutBefore),
        1 => Just(Fault::TimesOutAfter),
        1 => Just(Fault::NeverStarts),
        1 => Just(Fault::Overtaken),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    let pick = 0usize..8;
    prop_oneof![
        4 => (pick.clone(), any::<bool>(), 0u8..3, 0u8..2)
            .prop_map(|(pick, oauth, url, client)| Step::Edit { pick, oauth, url, client }),
        3 => (pick.clone(), 0usize..3).prop_map(|(pick, to)| Step::Move { pick, to }),
        3 => (pick.clone(), 0usize..3, any::<bool>())
            .prop_map(|(pick, to, rename)| Step::Copy { pick, to, rename }),
        1 => pick.clone().prop_map(|pick| Step::Rename { pick }),
        1 => pick.clone().prop_map(|pick| Step::Delete { pick }),
        1 => (pick.clone(), any::<bool>()).prop_map(|(pick, on)| Step::Toggle { pick, on }),
        2 => pick.clone().prop_map(|pick| Step::PutBack { pick }),
        1 => (pick.clone(), 0usize..3).prop_map(|(pick, kept)| Step::CopyToKept { pick, kept }),
        1 => pick.prop_map(|pick| Step::Forget { pick }),
    ]
}

fn store_fault() -> impl Strategy<Value = StoreFault> {
    prop_oneof![
        6 => Just(StoreFault::Works),
        1 => Just(StoreFault::SetFails),
        1 => Just(StoreFault::DeleteFails),
    ]
}

/// A step with what the stand-in does on its next few commands, and what Klefki's store does.
fn case() -> impl Strategy<Value = (Step, Vec<Fault>, StoreFault)> {
    (
        step(),
        proptest::collection::vec(fault(), 0..4),
        store_fault(),
    )
}

/// Everything a change may touch, read back from disk and the two secret stores.
#[derive(Debug, Clone, PartialEq)]
struct World {
    /// By (project, name); `None` for the user.
    servers: BTreeMap<(Option<String>, String), Value>,
    /// Recovery files: by id, the place, name and definition kept.
    kept: BTreeMap<String, (Option<String>, String, Value)>,
    /// Klefki's store: by connection.
    klefki: BTreeMap<String, String>,
    creds: BTreeMap<String, String>,
    /// The configuration's other contents: everything but the server lists and switch-off lists.
    rest: Value,
}

struct Rig {
    dir: tempfile::TempDir,
    projects: Vec<String>,
    sim: Arc<Sim>,
    store: Arc<Flaky>,
    core: Core,
}

/// The configuration a run starts from: a user server and a local one that sign in with OAuth, with client secrets
/// in both stores, one without a secret, and plain servers; plus keys klefki must leave alone.
fn start() -> Rig {
    start_checking_after(Duration::from_millis(5))
}

/// [`start`], with klefki's own check of each write `delay` after it.
fn start_checking_after(delay: Duration) -> Rig {
    start_spelled(delay, |key| key)
}

/// Like [`start_checking_after`], with each project's key in the file spelled by `spell`.
fn start_spelled(delay: Duration, spell: fn(String) -> String) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    let projects: Vec<String> = ["pa", "pb"]
        .iter()
        .map(|p| {
            let path = dir.path().join(p);
            std::fs::create_dir_all(&path).unwrap();
            // As Claude Code keys a project: the folder itself, not a link to it (macOS's temp folder is one).
            #[cfg(unix)]
            let path = std::fs::canonicalize(&path).unwrap();
            spell(path.to_str().unwrap().to_owned())
        })
        .collect();
    let (pa, pb) = (&projects[0], &projects[1]);
    let root = json!({
        "numStartups": 1,
        "zzUnknown": {"keep": [1, 2.50, "x"]},
        "mcpServers": {
            "a": definition(true, 0, 0),
            "p": definition(false, 0, 0),
        },
        "projects": {
            pa: {"zzKeep": true, "mcpServers": {"b": definition(true, 1, 0), "q": definition(false, 1, 0)}, "disabledMcpServers": []},
            pb: {"zzKeep": "yes", "mcpServers": {"c": definition(true, 2, 1)}, "disabledMcpServers": ["a"]}
        },
        "tail": 0
    });
    let config = cfg.join(".claude.json");
    std::fs::write(&config, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    let store = Arc::new(Flaky::default());
    let sim = Arc::new(Sim {
        config,
        faults: Mutex::default(),
        creds: Mutex::default(),
        calls: Mutex::default(),
    });
    // `c` has no client secret in Klefki's store.
    for (project, name) in [(None, "a"), (Some(pa.as_str()), "b")] {
        let secret = format!("sec-{name}");
        let entry = entry_at(&root, project, name).unwrap();
        lock(&sim.creds).insert(sign_in_key(name, &entry), secret.clone());
        let scope = if project.is_some() {
            Scope::Local
        } else {
            Scope::User
        };
        store
            .set(
                &ConnectionId::new(scope, project, name),
                &Secret::new(secret),
            )
            .unwrap();
    }
    let changer: Arc<dyn swap::Claude> = sim.clone();
    let core = start_with(
        EngineConfig {
            paths: Some(ConfigPaths::in_dir(cfg)),
            // No CLI is found, so no server is checked: only the stand-in runs.
            claude: None,
            home: Some(dir.path().join("home")),
            backup_dir: Some(dir.path().join("data").join("backups")),
            verify_delay: delay,
            path_var: None,
            cli_env: Vec::new(),
            max_checks: 1,
            check_timeout: Duration::from_secs(5),
            background_checks: true,
            watch_interval: None,
            watch_settle: Duration::from_millis(50),
            secrets: Some(store.clone()),
            open_url: Arc::new(|_| Ok(())),
            open_dialog: Arc::new(|_, _| Box::pin(std::future::ready(None))),
            pick_dialog: Arc::new(|_| Box::pin(std::future::ready(None))),
            sign_in_timeout: Duration::from_secs(5),
            changer: Some(changer),
            desktop: crate::desktop::DesktopRoots::default(),
        },
        || {},
    );
    Rig {
        dir,
        projects,
        sim,
        store,
        core,
    }
}

use crate::model::ConnectionId;

/// A definition: a remote one that signs in with OAuth, or a command.
fn definition(oauth: bool, url: u8, client: u8) -> Value {
    if oauth {
        json!({"type": "http", "url": format!("https://h{url}.example/mcp"), "oauth": {"clientId": format!("c{client}")}})
    } else {
        json!({"command": "node", "args": [format!("s{url}.js")], "env": {"TOKEN": format!("t{client}")}})
    }
}

fn entry_at(root: &Value, project: Option<&str>, name: &str) -> Option<Value> {
    match project {
        None => root["mcpServers"].get(name).cloned(),
        Some(p) => root["projects"][p]["mcpServers"].get(name).cloned(),
    }
}

impl Rig {
    fn config(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.sim.config).unwrap())
            .expect("the configuration stays valid JSON")
    }

    fn world(&self) -> World {
        let root = self.config();
        let mut servers = BTreeMap::new();
        for (name, e) in root["mcpServers"].as_object().into_iter().flatten() {
            servers.insert((None, name.clone()), e.clone());
        }
        for (p, project) in root["projects"].as_object().unwrap() {
            for (name, e) in project["mcpServers"].as_object().into_iter().flatten() {
                servers.insert((Some(p.clone()), name.clone()), e.clone());
            }
        }
        let mut rest = root.clone();
        rest.as_object_mut().unwrap().remove("mcpServers");
        rest.as_object_mut().unwrap().remove("numStartups");
        for project in rest["projects"].as_object_mut().unwrap().values_mut() {
            let project = project.as_object_mut().unwrap();
            project.remove("mcpServers");
            project.remove("disabledMcpServers");
        }
        let kept = swap::Recovery::files(&self.dir.path().join("data").join("backups"))
            .into_iter()
            .map(|r| (r.id, (r.project, r.name, r.entry)))
            .collect();
        World {
            servers,
            kept,
            klefki: lock(&self.store.inner.values).clone(),
            creds: lock(&self.sim.creds).clone(),
            rest,
        }
    }

    /// Where each kept definition's server was going, and as what, by id.
    fn going(&self) -> BTreeMap<String, (Key, Value)> {
        swap::Recovery::files(&self.dir.path().join("data").join("backups"))
            .into_iter()
            .filter_map(|r| {
                let (to, new) = r.next?;
                Some((r.id, ((to.project, to.name), new)))
            })
            .collect()
    }

    fn id(&self, (project, name): &(Option<String>, String)) -> ServerId {
        match project {
            None => ServerId::new(Scope::User, None, name),
            Some(p) => ServerId::new(Scope::Local, Some(p), name),
        }
    }

    fn place(&self, to: usize) -> Target {
        match to {
            0 => Target::User,
            n => Target::Local(self.projects[n - 1].clone()),
        }
    }

    /// Prepares and applies `changes`, as the app does, and says how it ended.
    fn run(&self, changes: Vec<Change>) -> Outcome {
        self.core.send(Command::Prepare(changes));
        let plan = loop {
            match self.next() {
                Event::PlanReady(plan) => break plan,
                Event::PlanFailed(why) => return Outcome::Refused(why),
                _ => {}
            }
        };
        if plan.nothing_to_do {
            self.core.send(Command::Discard(plan.id));
            return Outcome::Unchanged;
        }
        self.core.send(Command::Apply(plan.id));
        loop {
            match self.next() {
                Event::Applied {
                    plan: id, changed, ..
                } if id == plan.id => {
                    return if changed {
                        Outcome::Done
                    } else {
                        Outcome::Unchanged
                    };
                }
                Event::ApplyFailed { plan: id, error } if id == plan.id => {
                    return Outcome::Failed(error);
                }
                // The check of an earlier change, a moment after it was written, found it gone.
                Event::ApplyFailed { error, .. } => {
                    panic!("an earlier change's check failed: {error}")
                }
                _ => {}
            }
        }
    }

    fn next(&self) -> Event {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(e) = self.core.try_recv() {
                if std::env::var_os("MACHINE_DEBUG").is_some() {
                    eprintln!("EVENT {e:?}");
                }
                return e;
            }
            assert!(Instant::now() < deadline, "the engine did not answer");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[derive(Debug)]
enum Outcome {
    Done,
    Unchanged,
    Refused(String),
    Failed(String),
}

type Key = (Option<String>, String);

fn connection(key: &Key) -> String {
    let scope = if key.0.is_some() {
        Scope::Local
    } else {
        Scope::User
    };
    crate::secrets::user_for(&ConnectionId::new(scope, key.0.as_deref(), &key.1))
}

fn entry_signs_in(entry: &Value) -> bool {
    entry.get("oauth").is_some()
}

fn same_place(a: &Option<String>, b: &Option<String>) -> bool {
    a == b
}

/// What Klefki's store holds once a server that signs in went from `from` to `to` (moved, or copied) and Claude Code
/// was handed Klefki's copy, if any: the copy at `to` is the one handed or none, as far as the store lets it.
fn copy_secret(
    klefki: &mut BTreeMap<String, String>,
    before: &BTreeMap<String, String>,
    from: &Key,
    to: &Key,
    moved: bool,
    fault: StoreFault,
) {
    let (from, to) = (connection(from), connection(to));
    if from == to {
        return;
    }
    match (before.get(&from), fault) {
        (Some(secret), StoreFault::Works) => {
            klefki.insert(to, secret.clone());
            if moved {
                klefki.remove(&from);
            }
        }
        // Taken, and the old one could not be deleted.
        (Some(secret), StoreFault::DeleteFails) => {
            klefki.insert(to, secret.clone());
        }
        // Not taken; whatever was there is deleted, and so is the one left behind.
        (Some(_), StoreFault::SetFails) => {
            klefki.remove(&to);
            if moved {
                klefki.remove(&from);
            }
        }
        (None, StoreFault::Works | StoreFault::SetFails) => {
            klefki.remove(&to);
        }
        (None, StoreFault::DeleteFails) => {}
    }
}

/// `world` once the listing has dropped the kept definitions whose change got through after all: each one's server
/// is where it was going, as it was going to be, and gone from where it was (decision 007). A later step can bring
/// that about too, such as a copy of the same definition to where a failed move was going. The server is then where
/// the user sent it, and putting it back would bring it back where they moved it from. Only with its client secret,
/// though: when Klefki's copy of it is not also where the server went, the definition stays, as putting it back is
/// the only way to hand the secret to Claude Code again. Once it is there, the copy left where the server was goes,
/// and until it could be deleted the definition stays too, to be tried again at the next listing.
fn settled(
    world: &World,
    going: &BTreeMap<String, (Key, Value)>,
    store_fault: StoreFault,
) -> World {
    let mut world = world.clone();
    let gone: Vec<String> = world
        .kept
        .iter()
        .filter(|(id, (project, name, _))| {
            let Some(((to_project, to_name), new)) = going.get(*id) else {
                return false;
            };
            let arrived = world
                .servers
                .iter()
                .any(|((p, n), e)| same_place(p, to_project) && n == to_name && e == new);
            let from = (project.clone(), name.clone());
            let to = (to_project.clone(), to_name.clone());
            let in_place = same_place(project, to_project) && name == to_name;
            let left = !world
                .servers
                .keys()
                .any(|(p, n)| same_place(p, project) && n == name);
            let secret = match world.klefki.get(&connection(&from)) {
                None => true,
                Some(_) if in_place => true,
                Some(secret) => {
                    world.klefki.get(&connection(&to)) == Some(secret)
                        && store_fault != StoreFault::DeleteFails
                }
            };
            arrived && (in_place || left) && secret
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in gone {
        let (project, name, _) = world.kept.remove(&id).unwrap();
        let ((to_project, to_name), _) = &going[&id];
        let from = connection(&(project, name));
        if from != connection(&(to_project.clone(), to_name.clone())) {
            world.klefki.remove(&from);
        }
    }
    world
}

/// What a step that changed nothing leaves: `before`, or, when it listed the kept definitions again, `before` with
/// the ones dropped that an earlier listing could not (see [`settled`]).
fn unchanged(
    before: &World,
    after: &World,
    going: &BTreeMap<String, (Key, Value)>,
    store_fault: StoreFault,
) -> World {
    if after.kept == before.kept {
        before.clone()
    } else {
        settled(before, going, store_fault)
    }
}

/// Runs `cases` from the starting configuration, checking everything after each.
fn check(cases: &[(Step, Vec<Fault>, StoreFault)]) -> Result<(), TestCaseError> {
    let rig = start();
    // The first snapshot.
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let first = rig.world();
    for (n, (step, faults, store_fault)) in cases.iter().enumerate() {
        let before = rig.world();
        let keys: Vec<Key> = before.servers.keys().cloned().collect();
        let picked = |pick: usize| keys[pick % keys.len()].clone();
        *lock(&rig.sim.faults) = faults.iter().copied().collect();
        *lock(&rig.store.fault) = Some(*store_fault);
        let calls_before = lock(&rig.sim.calls).len();
        // What the step asks, and what the world should look like if it is done.
        let mut expected = before.clone();
        // Where the step puts a server under a name it did not have there.
        let mut arrives: Option<Key> = None;
        let changes = match step {
            Step::Edit {
                pick,
                oauth,
                url,
                client,
            } => {
                let key = picked(*pick);
                let new = definition(*oauth, *url, *client);
                // Values are sent back hidden where they did not change, as the form does.
                let old = &before.servers[&key];
                let shown = if old.get("env") == new.get("env") {
                    entry::masked(&new)
                } else {
                    new.clone()
                };
                expected.servers.insert(key.clone(), new);
                vec![Change::Replace {
                    id: rig.id(&key),
                    json: shown.to_string(),
                }]
            }
            Step::Move { pick, to } => {
                let key = picked(*pick);
                let target = rig.place(*to);
                let project = match &target {
                    Target::User => None,
                    Target::Local(p) => Some(p.clone()),
                };
                let entry = expected.servers.remove(&key).unwrap();
                let moved = (project, key.1.clone());
                arrives = Some(moved.clone());
                expected.servers.insert(moved.clone(), entry);
                // Klefki's copy of a client secret goes with a server that signs in.
                if entry_signs_in(&before.servers[&key]) {
                    copy_secret(
                        &mut expected.klefki,
                        &before.klefki,
                        &key,
                        &moved,
                        true,
                        *store_fault,
                    );
                }
                vec![Change::Move {
                    id: rig.id(&key),
                    to: target,
                }]
            }
            Step::Copy { pick, to, rename } => {
                let key = picked(*pick);
                let target = rig.place(*to);
                let project = match &target {
                    Target::User => None,
                    Target::Local(p) => Some(p.clone()),
                };
                let name = if *rename {
                    format!("{}_copy", key.1)
                } else {
                    key.1.clone()
                };
                let copy = (project, name.clone());
                arrives = Some(copy.clone());
                expected
                    .servers
                    .insert(copy.clone(), before.servers[&key].clone());
                if entry_signs_in(&before.servers[&key]) {
                    copy_secret(
                        &mut expected.klefki,
                        &before.klefki,
                        &key,
                        &copy,
                        false,
                        *store_fault,
                    );
                }
                vec![Change::Duplicate {
                    id: rig.id(&key),
                    name,
                    targets: vec![target],
                }]
            }
            Step::Rename { pick } => {
                let key = picked(*pick);
                let entry = expected.servers.remove(&key).unwrap();
                let name = format!("{}_r", key.1);
                arrives = Some((key.0.clone(), name.clone()));
                expected
                    .servers
                    .insert((key.0.clone(), name.clone()), entry);
                vec![Change::Rename {
                    id: rig.id(&key),
                    name,
                }]
            }
            Step::Delete { pick } => {
                let key = picked(*pick);
                expected.servers.remove(&key);
                vec![Change::Delete { id: rig.id(&key) }]
            }
            Step::Toggle { pick, on } => {
                let key = picked(*pick);
                vec![Change::SetEnabled {
                    id: rig.id(&key),
                    enabled: *on,
                }]
            }
            Step::CopyToKept { pick, kept } => {
                let Some((project, name, _)) =
                    before.kept.values().nth(kept % before.kept.len().max(1))
                else {
                    continue;
                };
                let key = picked(*pick);
                let copy = (project.clone(), name.clone());
                arrives = Some(copy.clone());
                expected
                    .servers
                    .insert(copy.clone(), before.servers[&key].clone());
                if entry_signs_in(&before.servers[&key]) {
                    copy_secret(
                        &mut expected.klefki,
                        &before.klefki,
                        &key,
                        &copy,
                        false,
                        *store_fault,
                    );
                }
                vec![Change::Duplicate {
                    id: rig.id(&key),
                    name: name.clone(),
                    targets: vec![match project {
                        None => Target::User,
                        Some(p) => Target::Local(p.clone()),
                    }],
                }]
            }
            Step::PutBack { pick } | Step::Forget { pick } => {
                let Some((id, (project, name, entry))) =
                    before.kept.iter().nth(pick % before.kept.len().max(1))
                else {
                    continue;
                };
                expected.kept.remove(id);
                if matches!(step, Step::PutBack { .. }) {
                    expected
                        .servers
                        .insert((project.clone(), name.clone()), entry.clone());
                    vec![Change::PutBack { kept: id.clone() }]
                } else {
                    vec![Change::ForgetKept { kept: id.clone() }]
                }
            }
        };
        let going = rig.going();
        let outcome = rig.run(changes);
        let after = rig.world();

        let said = format!("step {n} {step:?} with {faults:?}, {store_fault:?}: {outcome:?}");
        // Putting back a kept definition, or forgetting it, works when nothing fails: a refusal changes nothing and
        // would pass every check below.
        let nothing_fails = faults
            .iter()
            .all(|f| matches!(f, Fault::Works | Fault::Overtaken));
        match step {
            Step::Forget { pick } if !before.kept.is_empty() => {
                // Except one whose removal took the client secret of a server sharing its sign-in: only putting
                // it back hands that secret to Claude Code again (decision 007).
                let (_, (project, name, entry)) =
                    before.kept.iter().nth(pick % before.kept.len()).unwrap();
                let shares = entry_signs_in(entry)
                    && !before
                        .servers
                        .contains_key(&(project.clone(), name.clone()))
                    && before.servers.iter().any(|((p, n), e)| {
                        n == name
                            && !same_place(p, project)
                            && entry_signs_in(e)
                            && sign_in_key(n, e) == sign_in_key(name, entry)
                    });
                if shares {
                    prop_assert!(
                        matches!(&outcome, Outcome::Refused(why) if why.contains("Put this one back instead")),
                        "{}",
                        said
                    );
                } else {
                    prop_assert!(matches!(outcome, Outcome::Done), "{}", said);
                }
            }
            Step::PutBack { pick } if nothing_fails && !before.kept.is_empty() => {
                let (_, (project, name, _)) =
                    before.kept.iter().nth(pick % before.kept.len()).unwrap();
                let free = !before
                    .servers
                    .contains_key(&(project.clone(), name.clone()));
                if free {
                    prop_assert!(matches!(outcome, Outcome::Done), "{}", said);
                }
            }
            _ => {}
        }
        // A kept definition's place stays free until it is put back or forgotten: Klefki's copy of its client secret
        // is filed there, and a server arriving there would replace or delete it. The listing the review makes first
        // may drop some (see [`settled`]).
        if let Some((project, name)) = &arrives {
            let waiting = settled(&before, &going, *store_fault)
                .kept
                .values()
                .any(|(p, n, _)| same_place(p, project) && n == name);
            if waiting {
                prop_assert!(matches!(outcome, Outcome::Refused(_)), "{}", said);
            } else {
                // And only there: a place with nothing kept is not refused for it.
                prop_assert!(
                    !matches!(&outcome, Outcome::Refused(why) if why.contains("Put it back or forget it")),
                    "{}",
                    said
                );
            }
        }

        // The configuration's other keys, their values and their order.
        prop_assert_eq!(&after.rest, &first.rest, "{}", said);
        let config = rig.config();
        let order: Vec<&String> = config.as_object().unwrap().keys().collect();
        prop_assert_eq!(
            order,
            vec!["numStartups", "zzUnknown", "mcpServers", "projects", "tail"],
            "{}",
            said
        );
        // Nothing in project scope.
        for p in &rig.projects {
            prop_assert!(!Path::new(p).join(".mcp.json").exists(), "{}", said);
            prop_assert!(!Path::new(p).join(".claude").exists(), "{}", said);
        }
        // Secrets travel only in the environment, and only with `--client-secret`.
        for (argv, secret) in &lock(&rig.sim.calls)[calls_before..] {
            prop_assert!(
                !argv.iter().any(|a| a.contains("sec-")),
                "a secret in the arguments: {:?}; {}",
                argv,
                said
            );
            prop_assert_eq!(
                argv.iter().any(|a| a == "--client-secret"),
                secret.is_some(),
                "{}",
                said
            );
        }
        match &outcome {
            Outcome::Done => {
                let expected = settled(&expected, &going, *store_fault);
                prop_assert_eq!(&after.servers, &expected.servers, "{}", said);
                prop_assert_eq!(&after.kept, &expected.kept, "{}", said);
                prop_assert_eq!(&after.klefki, &expected.klefki, "{}", said);
            }
            Outcome::Refused(why) => {
                prop_assert!(!why.is_empty() && sanitize(why) == *why, "{}", said);
                let before = &unchanged(&before, &after, &going, *store_fault);
                prop_assert_eq!(&after.servers, &before.servers, "{}", said);
                prop_assert_eq!(&after.kept, &before.kept, "{}", said);
                prop_assert_eq!(&after.klefki, &before.klefki, "{}", said);
                prop_assert_eq!(&after.creds, &before.creds, "{}", said);
            }
            Outcome::Unchanged => {
                let before = &unchanged(&before, &after, &going, *store_fault);
                prop_assert_eq!(&after.servers, &before.servers, "{}", said);
                prop_assert_eq!(&after.kept, &before.kept, "{}", said);
                prop_assert_eq!(&after.klefki, &before.klefki, "{}", said);
                prop_assert_eq!(&after.creds, &before.creds, "{}", said);
            }
            Outcome::Failed(why) => {
                let before = &unchanged(&before, &after, &going, *store_fault);
                // Each server is where it was, or kept, and the failure says so.
                for (key, entry) in &before.servers {
                    match after.servers.get(key) {
                        Some(now) => prop_assert_eq!(now, entry, "{}", said),
                        None => {
                            let kept = after.kept.values().any(|(p, n, e)| {
                                same_place(p, &key.0) && *n == key.1 && e == entry
                            });
                            prop_assert!(kept, "{:?} was lost; {}", key, said);
                            prop_assert!(why.contains("Klefki kept"), "{}", said);
                        }
                    }
                }
                for key in after.servers.keys() {
                    prop_assert!(
                        before.servers.contains_key(key),
                        "{:?} appeared; {}",
                        key,
                        said
                    );
                }
                // A definition is only left in a recovery file when Claude Code failed twice: the add, and putting
                // it back.
                if after.kept.len() > before.kept.len() {
                    let calls = lock(&rig.sim.calls).len() - calls_before;
                    let failed = faults
                        .iter()
                        .take(calls)
                        .filter(|f| !matches!(f, Fault::Works | Fault::Overtaken))
                        .count();
                    prop_assert!(failed >= 2, "kept after {} failure(s); {}", failed, said);
                }
                // Klefki's copies stay where they were.
                prop_assert_eq!(&after.klefki, &before.klefki, "{}", said);
            }
        }
        // Every server that signs in, with a secret in Klefki's store, has the same one in Claude Code's. Except
        // while a kept definition shares its sign-in: Claude Code's remove of that one took the secret of both, and
        // only Claude Code can hand it back, which putting it back does. The failure must have said so.
        let newly_kept = after.kept.len() > before.kept.len();
        for (key, entry) in &after.servers {
            if entry.get("oauth").is_none() {
                continue;
            }
            let shared_with_kept = after.kept.values().any(|(p, n, e)| {
                e.get("oauth").is_some()
                    && !same_place(p, &key.0)
                    && sign_in_key(n, e) == sign_in_key(&key.1, entry)
            });
            if shared_with_kept && !after.creds.contains_key(&sign_in_key(&key.1, entry)) {
                if newly_kept {
                    prop_assert!(
                        matches!(&outcome, Outcome::Failed(why) if why.contains("shares its sign-in")),
                        "{:?} lost its sign-in unsaid; {}",
                        key,
                        said
                    );
                }
                continue;
            }
            if let Some(secret) = after.klefki.get(&connection(key)) {
                prop_assert_eq!(
                    after.creds.get(&sign_in_key(&key.1, entry)),
                    Some(secret),
                    "{:?} lost its client secret in Claude Code; {}",
                    key,
                    said
                );
            }
        }
        // A kept definition's secret is still in Klefki's store.
        for (project, name, _) in after.kept.values() {
            let key = (project.clone(), name.clone());
            if let Some(secret) = before.klefki.get(&connection(&key)) {
                prop_assert_eq!(
                    after.klefki.get(&connection(&key)),
                    Some(secret),
                    "{}",
                    said
                );
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        // `PROPTEST_CASES` runs more, for a longer search.
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(256),
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::SourceParallel("proptest-regressions"),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    fn no_change_ever_loses_a_server_or_its_secret(cases in proptest::collection::vec(case(), 1..12)) {
        check(&cases)?;
    }
}

/// Events for `window`, failing on any late failure or put-back of an earlier change.
fn quiet_for(rig: &Rig, window: Duration) {
    let until = Instant::now() + window;
    while Instant::now() < until {
        while let Some(e) = rig.core.try_recv() {
            assert!(
                !matches!(e, Event::ApplyFailed { .. } | Event::Reapplied { .. }),
                "{e:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_earlier_edits_check_does_not_undo_an_edit_made_through_claude_code() {
    // Found by the model-based test. An edit klefki writes itself, then, before its check, an edit Claude Code makes
    // (the server now signs in with OAuth): the check replayed the first edit over the second.
    let rig = start_checking_after(Duration::from_millis(400));
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let p = (None, "p".to_owned());
    let plain = definition(false, 1, 1);
    let oauth = definition(true, 1, 0);
    for new in [&plain, &oauth] {
        let outcome = rig.run(vec![Change::Replace {
            id: rig.id(&p),
            json: new.to_string(),
        }]);
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    }
    quiet_for(&rig, Duration::from_millis(1200));
    assert_eq!(rig.world().servers[&p], oauth);
}

#[test]
fn an_earlier_deletes_check_does_not_touch_a_server_claude_code_adds_under_that_name() {
    // The variant: delete a server, then add one by that name that Claude Code adds itself.
    let rig = start_checking_after(Duration::from_millis(400));
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let q = (Some(rig.projects[0].clone()), "q".to_owned());
    let outcome = rig.run(vec![Change::Delete { id: rig.id(&q) }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let oauth = definition(true, 2, 0);
    let outcome = rig.run(vec![Change::Add {
        name: "q".to_owned(),
        json: oauth.to_string(),
        targets: vec![Target::Local(rig.projects[0].clone())],
        secret: None,
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    quiet_for(&rig, Duration::from_millis(1200));
    assert_eq!(rig.world().servers[&q], oauth);
}

#[test]
fn a_kept_definition_says_which_server_lost_its_shared_sign_in_and_putting_it_back_restores_it() {
    // Found by the model-based test. Copied under the same name, two servers share one sign-in in Claude Code;
    // removing one for an edit takes the client secret of both. When the add and putting it back both fail, the
    // copy is left without one until it is put back, which is said.
    let rig = start();
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let pa = rig.projects[0].clone();
    let a = (None, "a".to_owned());
    let copy = (Some(pa.clone()), "a".to_owned());
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&a),
        name: "a".into(),
        targets: vec![Target::Local(pa.clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let shared = sign_in_key("a", &rig.world().servers[&copy]);
    *lock(&rig.sim.faults) = [Fault::Works, Fault::FailsBefore, Fault::FailsBefore].into();
    let outcome = rig.run(vec![Change::Replace {
        id: rig.id(&a),
        json: definition(true, 0, 1).to_string(),
    }]);
    let Outcome::Failed(why) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(why.contains("a in pa shares its sign-in"), "{why}");
    assert!(!rig.world().creds.contains_key(&shared));
    // The variant: with Claude Code working again, putting it back restores both.
    let kept = rig.world().kept.into_keys().next().unwrap();
    let outcome = rig.run(vec![Change::PutBack { kept }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let world = rig.world();
    assert_eq!(world.creds.get(&shared).map(String::as_str), Some("sec-a"));
    assert!(world.kept.is_empty());
}

/// `a` copied into pb, then a move of that copy to pa that fails three times, so its definition is kept.
fn a_failed_move_to_pa(rig: &Rig) -> (Key, Key) {
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let (pa, pb) = (rig.projects[0].clone(), rig.projects[1].clone());
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&(None, "a".to_owned())),
        name: "a".into(),
        targets: vec![Target::Local(pb.clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let (from, to) = (
        (Some(pb), "a".to_owned()),
        (Some(pa.clone()), "a".to_owned()),
    );
    *lock(&rig.sim.faults) = [Fault::FailsAfter, Fault::FailsBefore, Fault::FailsBefore].into();
    let outcome = rig.run(vec![Change::Move {
        id: rig.id(&from),
        to: Target::Local(pa),
    }]);
    assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
    assert_eq!(rig.world().kept.len(), 1);
    (from, to)
}

#[test]
fn a_kept_definition_is_not_offered_once_a_copy_puts_it_where_its_move_was_going() {
    // Found by the model-based test (seed 19dc2de0): a copy then puts the same definition where the failed move was
    // going. That is what the move asked, so the kept definition is no longer offered: putting it back would bring
    // the server back where it was moved from.
    let rig = start();
    let (from, to) = a_failed_move_to_pa(&rig);
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&(None, "a".to_owned())),
        name: "a".into(),
        targets: vec![Target::Local(rig.projects[0].clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let world = rig.world();
    assert!(world.kept.is_empty(), "{:?}", world.kept);
    assert_eq!(world.servers.get(&to), Some(&definition(true, 0, 0)));
    assert!(!world.servers.contains_key(&from));
    // With its client secret, in Claude Code and in Klefki's store.
    let secret = Some("sec-a");
    assert_eq!(
        world
            .creds
            .get(&sign_in_key("a", &world.servers[&to]))
            .map(String::as_str),
        secret
    );
    assert_eq!(
        world.klefki.get(&connection(&to)).map(String::as_str),
        secret
    );
    // And none is left where it was, for a server put there later to claim.
    assert!(!world.klefki.contains_key(&connection(&from)));
}

#[test]
fn a_kept_definition_stays_offered_when_a_copy_without_its_secret_puts_it_where_its_move_was_going()
{
    // Found while checking the seed above: the copy came from a server Klefki holds no client secret for, so Claude
    // Code has none for it. Dropping the kept definition would leave its secret only in Klefki's store, under a
    // server that is gone. It stays, and putting it back hands the secret to Claude Code again, for the copy too.
    let rig = start();
    let a = (None, "a".to_owned());
    let (from, to) = a_failed_move_to_pa(&rig);
    lock(&rig.store.inner.values).remove(&connection(&a));
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&a),
        name: "a".into(),
        targets: vec![Target::Local(rig.projects[0].clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let world = rig.world();
    assert_eq!(world.servers.get(&to), Some(&definition(true, 0, 0)));
    let kept = world.kept.into_keys().next().expect("still kept");
    let outcome = rig.run(vec![Change::PutBack { kept }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let world = rig.world();
    assert!(world.kept.is_empty());
    assert_eq!(world.servers.get(&from), Some(&definition(true, 0, 0)));
    assert_eq!(
        world
            .creds
            .get(&sign_in_key("a", &world.servers[&to]))
            .map(String::as_str),
        Some("sec-a")
    );
}

#[test]
fn a_kept_definition_stays_offered_when_a_copy_puts_another_definition_where_its_move_was_going() {
    // The variant: the copy at the move's place is not the kept definition, so it stays, and can be put back.
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    let a = (None, "a".to_owned());
    let outcome = rig.run(vec![Change::Replace {
        id: rig.id(&a),
        json: definition(true, 1, 0).to_string(),
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&a),
        name: "a".into(),
        targets: vec![Target::Local(rig.projects[0].clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let kept = rig.world().kept.into_keys().next().expect("still kept");
    let outcome = rig.run(vec![Change::PutBack { kept }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let world = rig.world();
    assert!(world.kept.is_empty());
    assert_eq!(world.servers.get(&from), Some(&definition(true, 0, 0)));
}

#[test]
fn a_server_that_signs_in_lands_under_the_projects_own_spelling() {
    // Found on Windows CI: Klefki ran Claude Code from the project's folder as Klefki spells it (`C:/…`), not as
    // the file does (`C:\…`), and Claude Code, which keys a project by the folder it runs in, filed the server
    // under a second entry for the same project. Here the variant every system has: a trailing separator.
    let rig = start_spelled(Duration::from_millis(5), |key| {
        format!("{key}{}", std::path::MAIN_SEPARATOR)
    });
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let a = (None, "a".to_owned());
    let pa = rig.projects[0].clone();
    let projects = |rig: &Rig| -> Vec<String> {
        rig.config()["projects"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    let before = projects(&rig);
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&a),
        name: "a".into(),
        targets: vec![Target::Local(pa.clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    assert_eq!(projects(&rig), before);
    assert!(
        rig.config()["projects"][&pa]["mcpServers"]
            .get("a")
            .is_some()
    );
    // And Klefki's copy of its client secret is filed under the server as the list shows it.
    let copy = connection(&(Some(pa.clone()), "a".to_owned()));
    assert_eq!(
        rig.world().klefki.get(&copy).map(String::as_str),
        Some("sec-a")
    );
    // The variant: a move there.
    let b = (Some(pa.clone()), "b".to_owned());
    let outcome = rig.run(vec![Change::Move {
        id: rig.id(&b),
        to: Target::User,
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let outcome = rig.run(vec![Change::Move {
        id: rig.id(&(None, "b".to_owned())),
        to: Target::Local(pa.clone()),
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    assert_eq!(projects(&rig), before);
    assert!(
        rig.config()["projects"][&pa]["mcpServers"]
            .get("b")
            .is_some()
    );
    // And a new one added there.
    let outcome = rig.run(vec![Change::Add {
        name: "n".to_owned(),
        json: definition(true, 3, 0).to_string(),
        targets: vec![Target::Local(pa.clone())],
        secret: None,
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    assert_eq!(projects(&rig), before);
    assert!(
        rig.config()["projects"][&pa]["mcpServers"]
            .get("n")
            .is_some()
    );
}

#[test]
fn saving_a_server_that_signs_in_unchanged_runs_nothing() {
    // Removing and adding it again would only sign it out.
    let rig = start();
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let a = (None, "a".to_owned());
    let outcome = rig.run(vec![Change::Replace {
        id: rig.id(&a),
        json: definition(true, 0, 0).to_string(),
    }]);
    assert!(matches!(outcome, Outcome::Unchanged), "{outcome:?}");
    assert!(lock(&rig.sim.calls).is_empty());
    // The variant: the same definition with its keys in another order.
    let outcome = rig.run(vec![Change::Replace {
        id: rig.id(&a),
        json: r#"{"oauth": {"clientId": "c0"}, "url": "https://h0.example/mcp", "type": "http"}"#
            .to_owned(),
    }]);
    assert!(matches!(outcome, Outcome::Unchanged), "{outcome:?}");
    assert!(lock(&rig.sim.calls).is_empty());
}

#[test]
fn an_earlier_deletes_check_does_not_undo_a_move_back_into_its_place() {
    // Found by the model-based test. Delete p in a project, then move the user's p into that project: the delete's
    // check replayed the move's copy from its source, which was gone, and then the delete, so p was lost.
    let rig = start_checking_after(Duration::from_millis(400));
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let pa = rig.projects[0].clone();
    let p = (None, "p".to_owned());
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&p),
        name: "p".into(),
        targets: vec![Target::Local(pa.clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let in_pa = (Some(pa.clone()), "p".to_owned());
    let outcome = rig.run(vec![Change::Delete { id: rig.id(&in_pa) }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let moved = rig.world().servers[&p].clone();
    let outcome = rig.run(vec![Change::Move {
        id: rig.id(&p),
        to: Target::Local(pa.clone()),
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    quiet_for(&rig, Duration::from_millis(1200));
    let world = rig.world();
    assert_eq!(world.servers.get(&in_pa), Some(&moved));
    assert!(!world.servers.contains_key(&p));
}

#[test]
fn an_earlier_moves_check_does_not_add_back_a_server_renamed_since() {
    // Found by the model-based test: move q to another project, rename it there, then move it on. The move's
    // check added q back where it had gone, and the rename's check then failed on its new name.
    let rig = start_checking_after(Duration::from_millis(400));
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let (pa, pb) = (rig.projects[0].clone(), rig.projects[1].clone());
    let q = (Some(pa.clone()), "q".to_owned());
    let definition = rig.world().servers[&q].clone();
    let in_pb = (Some(pb.clone()), "q".to_owned());
    let renamed = (Some(pb.clone()), "q_r".to_owned());
    let steps = [
        Change::Move {
            id: rig.id(&q),
            to: Target::Local(pb.clone()),
        },
        Change::Rename {
            id: rig.id(&in_pb),
            name: "q_r".into(),
        },
        Change::Move {
            id: rig.id(&renamed),
            to: Target::Local(pa.clone()),
        },
    ];
    for (n, change) in steps.into_iter().enumerate() {
        let outcome = rig.run(vec![change]);
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
        // The move and the rename are checked together, before the last move.
        if n == 1 {
            quiet_for(&rig, Duration::from_millis(600));
        }
    }
    let world = rig.world();
    assert_eq!(
        world.servers.get(&(Some(pa), "q_r".to_owned())),
        Some(&definition)
    );
    assert!(!world.servers.contains_key(&in_pb) && !world.servers.contains_key(&renamed));
}

#[test]
fn an_earlier_deletes_check_does_not_undo_a_copy_back_into_its_place() {
    // The variant: copy it back instead of moving it.
    let rig = start_checking_after(Duration::from_millis(400));
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let pa = rig.projects[0].clone();
    let p = (None, "p".to_owned());
    let in_pa = (Some(pa.clone()), "p".to_owned());
    for change in [
        Change::Duplicate {
            id: rig.id(&p),
            name: "p".into(),
            targets: vec![Target::Local(pa.clone())],
        },
        Change::Delete { id: rig.id(&in_pa) },
    ] {
        let outcome = rig.run(vec![change]);
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    }
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&p),
        name: "p".into(),
        targets: vec![Target::Local(pa.clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    quiet_for(&rig, Duration::from_millis(1200));
    let world = rig.world();
    assert_eq!(world.servers.get(&in_pa), world.servers.get(&p));
    assert!(world.servers.contains_key(&p));
}
#[test]
fn a_secret_klefki_could_not_move_is_not_left_where_the_server_was() {
    // Found by the model-based test. Moving a with Klefki's store refusing the new copy left the old one where a
    // was; the copy brought back there later claimed a secret Claude Code no longer had.
    let cases = [
        (Step::Move { pick: 0, to: 1 }, vec![], StoreFault::SetFails),
        (
            Step::Copy {
                pick: 0,
                to: 1,
                rename: false,
            },
            vec![],
            StoreFault::Works,
        ),
        (
            Step::Move { pick: 7, to: 0 },
            vec![],
            StoreFault::DeleteFails,
        ),
    ];
    check(&cases).unwrap();
    // The variant: the move alone.
    let rig = start();
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let a = (None, "a".to_owned());
    *lock(&rig.store.fault) = Some(StoreFault::SetFails);
    let outcome = rig.run(vec![Change::Move {
        id: rig.id(&a),
        to: Target::Local(rig.projects[0].clone()),
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    assert!(!rig.world().klefki.contains_key(&connection(&a)));
}

#[test]
fn a_kept_definition_whose_left_copy_could_not_be_deleted_goes_at_the_next_listing() {
    // Cross-review of the fix above: when Klefki's copy left where the server was cannot be deleted, the definition
    // stays listed and goes at a later listing, here one a refused step makes. The model expected it to stay.
    use Fault::*;
    let base = vec![
        (
            Step::Copy {
                pick: 5,
                to: 2,
                rename: false,
            },
            vec![],
            StoreFault::Works,
        ),
        (
            Step::Move { pick: 4, to: 1 },
            vec![FailsAfter, FailsBefore, FailsBefore],
            StoreFault::Works,
        ),
        (
            Step::Copy {
                pick: 0,
                to: 1,
                rename: false,
            },
            vec![],
            StoreFault::DeleteFails,
        ),
    ];
    // Refused (taken name, a sign-in server's rename), done, and a toggle.
    let tails = [
        Step::Copy {
            pick: 0,
            to: 1,
            rename: false,
        },
        Step::Rename { pick: 2 },
        Step::Delete { pick: 1 },
        Step::Toggle { pick: 0, on: false },
    ];
    for tail in tails {
        let mut cases = base.clone();
        cases.push((tail.clone(), vec![], StoreFault::Works));
        if let Err(e) = check(&cases) {
            panic!("{tail:?}: {e}");
        }
    }
}

#[test]
fn a_copy_left_behind_is_not_deleted_when_a_server_is_put_back_there_meanwhile() {
    // Cross-review: the listing read the configuration, then the store. If another Klefki put the server back where
    // it was in between, it must not delete the copy that server now uses: it reads the configuration again first.
    let rig = start();
    let (from, to) = a_failed_move_to_pa(&rig);
    let sim = Arc::clone(&rig.sim);
    let project = from.0.clone().unwrap();
    *lock(&rig.store.fault) = Some(StoreFault::Works);
    // The listing reads Klefki's copy where the server went just before it would delete the one left behind.
    let at = ConnectionId::new(Scope::Local, to.0.as_deref(), &to.1);
    *lock(&rig.store.on_get) = Some((
        at,
        Box::new(move || {
            sim.with_lock(|root| {
                root["projects"][&project]["mcpServers"]["a"] = definition(true, 0, 0);
            });
        }),
    ));
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&(None, "a".to_owned())),
        name: "a".into(),
        targets: vec![Target::Local(rig.projects[0].clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    assert!(lock(&rig.store.on_get).is_none(), "the listing read it");
    let world = rig.world();
    assert_eq!(world.servers.get(&from), Some(&definition(true, 0, 0)));
    assert_eq!(
        world.klefki.get(&connection(&from)).map(String::as_str),
        Some("sec-a")
    );
}

/// What a change into the place `a`'s kept definition came from (pb) must leave: refused, with nothing changed, and
/// the kept definition still there with its client secret, so that putting it back hands that secret over again.
fn refused_into_the_kept_place(rig: &Rig, from: &Key, changes: Vec<Change>) {
    let before = rig.world();
    let outcome = rig.run(changes);
    let Outcome::Refused(why) = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(why.contains("Put it back or forget it"), "{why}");
    let after = rig.world();
    assert_eq!(after.servers, before.servers);
    assert_eq!(after.kept, before.kept);
    assert_eq!(after.klefki, before.klefki);
    assert_eq!(after.creds, before.creds);
    let kept = after.kept.into_keys().next().expect("still kept");
    let outcome = rig.run(vec![Change::PutBack { kept }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let world = rig.world();
    assert_eq!(world.servers.get(from), Some(&definition(true, 0, 0)));
    assert_eq!(
        world
            .creds
            .get(&sign_in_key("a", &world.servers[from]))
            .map(String::as_str),
        Some("sec-a")
    );
    assert_eq!(
        world.klefki.get(&connection(from)).map(String::as_str),
        Some("sec-a")
    );
}

#[test]
fn a_copy_into_the_place_a_kept_definition_came_from_is_refused_and_its_secret_kept() {
    // Seen while probing the seed above: a copy into the place a kept definition came from replaced Klefki's copy of
    // its client secret with the copy's own, so putting it back handed Claude Code the wrong one. The model missed it,
    // as its secrets were the same there.
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    let a = (None, "a".to_owned());
    lock(&rig.store.inner.values).insert(connection(&a), "sec-other".to_owned());
    refused_into_the_kept_place(
        &rig,
        &from,
        vec![Change::Duplicate {
            id: rig.id(&a),
            name: "a".into(),
            targets: vec![Target::Local(rig.projects[1].clone())],
        }],
    );
}

#[test]
fn a_copy_without_a_secret_into_the_place_a_kept_definition_came_from_is_refused_and_its_secret_kept()
 {
    // The variant: the copy's source has no Klefki copy of a client secret, and the copy deleted the kept one.
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    let a = (None, "a".to_owned());
    lock(&rig.store.inner.values).remove(&connection(&a));
    refused_into_the_kept_place(
        &rig,
        &from,
        vec![Change::Duplicate {
            id: rig.id(&a),
            name: "a".into(),
            targets: vec![Target::Local(rig.projects[1].clone())],
        }],
    );
}

#[test]
fn a_move_into_the_place_a_kept_definition_came_from_is_refused_and_its_secret_kept() {
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    let a = (None, "a".to_owned());
    lock(&rig.store.inner.values).insert(connection(&a), "sec-other".to_owned());
    refused_into_the_kept_place(
        &rig,
        &from,
        vec![Change::Move {
            id: rig.id(&a),
            to: Target::Local(rig.projects[1].clone()),
        }],
    );
}

#[test]
fn adding_a_server_into_the_place_a_kept_definition_came_from_is_refused_and_its_secret_kept() {
    // A new server that signs in, with a client secret typed in, would take Klefki's store's place for the kept one.
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    refused_into_the_kept_place(
        &rig,
        &from,
        vec![Change::Add {
            name: "a".into(),
            json: definition(true, 2, 0).to_string(),
            targets: vec![Target::Local(rig.projects[1].clone())],
            secret: Some(Secret::new("sec-new".to_owned())),
        }],
    );
}

#[test]
fn a_plain_copy_into_the_place_a_kept_definition_came_from_is_refused() {
    // A server that does not sign in touches no secret, but would stand where the kept one goes back, and the
    // Auth tab and a later move of a server there would then take the kept secret for their own.
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    refused_into_the_kept_place(
        &rig,
        &from,
        vec![Change::Duplicate {
            id: rig.id(&(None, "p".to_owned())),
            name: "a".into(),
            targets: vec![Target::Local(rig.projects[1].clone())],
        }],
    );
}

#[test]
fn a_rename_into_the_place_a_kept_definition_came_from_is_refused() {
    let rig = start();
    let (from, _) = a_failed_move_to_pa(&rig);
    refused_into_the_kept_place(
        &rig,
        &from,
        vec![Change::Rename {
            id: rig.id(&(Some(rig.projects[1].clone()), "c".to_owned())),
            name: "a".into(),
        }],
    );
}

#[test]
fn a_copy_reviewed_before_a_definition_was_kept_in_its_place_is_refused_when_applied() {
    // The review saw the place free; by the time the copy runs, a failed move from there keeps a definition there.
    let rig = start();
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let a = (None, "a".to_owned());
    let pb = rig.projects[1].clone();
    rig.core.send(Command::Prepare(vec![Change::Duplicate {
        id: rig.id(&a),
        name: "a".into(),
        targets: vec![Target::Local(pb.clone())],
    }]));
    let plan = loop {
        match rig.next() {
            Event::PlanReady(plan) => break plan,
            Event::PlanFailed(why) => panic!("{why}"),
            _ => {}
        }
    };
    // `a` copied into pb, then that copy's move to pa fails three times, so its definition is kept.
    let outcome = rig.run(vec![Change::Duplicate {
        id: rig.id(&a),
        name: "a".into(),
        targets: vec![Target::Local(pb.clone())],
    }]);
    assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    let from = (Some(pb), "a".to_owned());
    *lock(&rig.sim.faults) = [Fault::FailsAfter, Fault::FailsBefore, Fault::FailsBefore].into();
    let outcome = rig.run(vec![Change::Move {
        id: rig.id(&from),
        to: Target::Local(rig.projects[0].clone()),
    }]);
    assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
    lock(&rig.store.inner.values).insert(connection(&a), "sec-other".to_owned());
    let before = rig.world();
    assert_eq!(before.kept.len(), 1);
    rig.core.send(Command::Apply(plan.id));
    let error = loop {
        match rig.next() {
            Event::ApplyFailed { plan: id, error } if id == plan.id => break error,
            Event::Applied { plan: id, .. } if id == plan.id => panic!("applied"),
            _ => {}
        }
    };
    assert!(error.contains("Put it back or forget it"), "{error}");
    let after = rig.world();
    assert_eq!(after.servers, before.servers);
    assert_eq!(after.kept, before.kept);
    assert_eq!(
        after.klefki.get(&connection(&from)).map(String::as_str),
        Some("sec-a")
    );
}

#[test]
fn an_earlier_renames_check_does_not_fail_on_a_copy_under_the_old_name() {
    // Found by the model-based test: rename q to q_r, then copy p into its place under the name q. Checked together,
    // the rename found q there again with its new name taken, and the check failed with "q_r already exists there".
    let rig = start_checking_after(Duration::from_millis(400));
    while !matches!(rig.next(), Event::Snapshot(_)) {}
    let pa = rig.projects[0].clone();
    let p = (None, "p".to_owned());
    let q = (Some(pa.clone()), "q".to_owned());
    let renamed = (Some(pa.clone()), "q_r".to_owned());
    let before = rig.world();
    for change in [
        Change::Rename {
            id: rig.id(&q),
            name: "q_r".into(),
        },
        Change::Duplicate {
            id: rig.id(&p),
            name: "q".into(),
            targets: vec![Target::Local(pa.clone())],
        },
    ] {
        let outcome = rig.run(vec![change]);
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
    }
    quiet_for(&rig, Duration::from_millis(1200));
    let world = rig.world();
    assert_eq!(world.servers.get(&renamed), before.servers.get(&q));
    assert_eq!(world.servers.get(&q), before.servers.get(&p));
}

#[test]
fn a_copy_into_the_place_a_failed_edit_kept_is_refused() {
    // Found by the model test once it aimed copies at kept places: an edit of a fails twice, so Klefki keeps it, and
    // a copy of another server under its name into its place went through.
    use Fault::*;
    let cases = [
        (
            Step::Edit {
                pick: 0,
                oauth: true,
                url: 0,
                client: 1,
            },
            vec![Works, TimesOutBefore, FailsBefore],
            StoreFault::Works,
        ),
        (
            Step::CopyToKept { pick: 0, kept: 0 },
            vec![],
            StoreFault::Works,
        ),
    ];
    check(&cases).unwrap();
}

#[test]
fn a_kept_definition_that_took_a_shared_client_secret_is_not_forgotten() {
    // Found by the model-based test (seed e896270e): b copied into pb shares its sign-in with b in pa. An edit of b
    // in pa fails twice, so Klefki keeps it, and Claude Code's remove took the client secret of both. Forgetting it
    // left b in pb without one in Claude Code. Forget is now refused, and putting it back restores both.
    use Fault::*;
    let cases = [
        (
            Step::Copy {
                pick: 2,
                to: 2,
                rename: false,
            },
            vec![],
            StoreFault::Works,
        ),
        (Step::Move { pick: 1, to: 2 }, vec![], StoreFault::Works),
        (
            Step::Edit {
                pick: 1,
                oauth: true,
                url: 1,
                client: 1,
            },
            vec![FailsAfter, FailsBefore, TimesOutBefore],
            StoreFault::Works,
        ),
        (Step::Forget { pick: 0 }, vec![], StoreFault::Works),
        (Step::PutBack { pick: 0 }, vec![], StoreFault::Works),
    ];
    check(&cases).unwrap();
    // The variant: once nothing shares its sign-in (the other b now has another address), it can be forgotten.
    let mut cases = cases[..3].to_vec();
    cases.push((
        Step::Edit {
            pick: 2,
            oauth: true,
            url: 2,
            client: 0,
        },
        vec![],
        StoreFault::Works,
    ));
    cases.push((Step::Forget { pick: 0 }, vec![], StoreFault::Works));
    check(&cases).unwrap();
}
