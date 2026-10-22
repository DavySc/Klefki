//! Live status checks: the queue, the workers that take jobs from it, and how `claude mcp get` and
//! `claude mcp list` answers are read.
use super::diagnose::run_diagnose;
use super::{
    Diagnosis, Event, MAX_CHECK_OUTPUT, MAX_DETAIL_CHARS, ProgramStamp, Shared, claude_program,
    lock,
};
use crate::api::{LONGER_CHECK, Scope, ServerId, ServerRow, Status, TimeLimit, Timing};
use crate::cli::parse;
use crate::cli::runner::{self, RunOutput, RunSpec};
use crate::config::inventory::{self, FileKey};
use crate::config::timeout;
use crate::diagnose::hints::{HintKind, hints_for};
use crate::model::ServerName;
use crate::sanitize::{clip, sanitize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum JobKind {
    Check,
    Diagnose,
}

#[derive(Clone)]
pub(super) struct Job {
    pub(super) id: ServerId,
    /// Already validated; it is passed to the CLI as its own argument.
    pub(super) name: ServerName,
    pub(super) cwd: PathBuf,
    pub(super) kind: JobKind,
    /// Tell the UI the check is running (only for rows that have nothing better to show meanwhile).
    pub(super) announce: bool,
    /// Nobody asked for it: it runs only while no other background job does.
    pub(super) background: bool,
    /// Asked for on its own ([`Queue::push_now`]): ahead of every other job, after earlier such asks.
    pub(super) now: bool,
    /// A `claude mcp list` had no clear answer for it: it is checked on its own, not through another list.
    pub(super) alone: bool,
    /// The user asked to give the server [`LONGER_CHECK`] to connect ([`super::Command::CheckLonger`]).
    pub(super) longer: bool,
    /// A user server run in this project, as shown, instead of the home folder ([`super::Command::DiagnoseIn`]): its
    /// answer says where it ran and leaves the server's status alone.
    pub(super) folder: Option<String>,
}

#[derive(Default)]
pub(super) struct Queue {
    pub(super) jobs: VecDeque<Job>,
    /// Background jobs running now; at most [`BACKGROUND_CHECKS`].
    pub(super) background_running: usize,
    /// The servers on screen, from [`super::Command::Visible`]: their background jobs go first.
    pub(super) visible: std::collections::HashSet<crate::model::ConnectionId>,
    /// Checks that said they are running and have not answered yet: the app forgets "Checking…" on every
    /// snapshot, so each reload says it again for those that will still answer.
    pub(super) announced: Vec<Running>,
    /// Tells the entries of [`Queue::announced`] apart.
    pub(super) tokens: u64,
    pub(super) shutdown: bool,
}

impl Queue {
    /// The next job a worker may take: the first that is not background, or the first background one when there is
    /// room for it.
    /// Queues checks, leaving out servers that already have one queued (a server switched off and on again).
    pub(super) fn add(&mut self, jobs: Vec<Job>) {
        let mut queued: HashSet<_> = self
            .jobs
            .iter()
            .map(|q| (q.kind, q.id.connection.clone()))
            .collect();
        for job in jobs {
            if queued.insert((job.kind, job.id.connection.clone())) {
                self.jobs.push_back(job);
            }
        }
        self.order();
    }

    /// Puts a job the user asked for in front, behind those asked for the same way before it, so that they run in the
    /// order asked. A check of the same server still waiting is dropped: it would run the server again, and its later
    /// answer would replace this one.
    /// A run in a project ([`Job::folder`]) leaves those alone: its answer is not the server's status.
    pub(super) fn push_now(&mut self, job: Job) {
        let connection = &job.id.connection;
        if job.folder.is_none() {
            self.jobs
                .retain(|q| !(q.kind == JobKind::Check && q.id.connection == *connection));
        }
        let at = self.jobs.iter().take_while(|q| q.now).count();
        self.jobs.insert(at, Job { now: true, ..job });
    }

    /// Drops the background checks still waiting of the servers in `edited`: they may not have said they were
    /// checking, and a new check replaces each.
    pub(super) fn drop_background_checks(&mut self, edited: &HashSet<crate::model::ConnectionId>) {
        self.jobs.retain(|q| {
            !(q.background && q.kind == JobKind::Check && edited.contains(&q.id.connection))
        });
    }

    /// Takes the other background checks waiting to run in `cwd`, to be answered by one `claude mcp list`. None when
    /// there are too few for a list to pay off: it starts every server of the folder, those not asked about too.
    fn take_mates(&mut self, cwd: &Path) -> Vec<Job> {
        let is_mate =
            |j: &Job| j.background && j.kind == JobKind::Check && !j.alone && j.cwd == cwd;
        if self.jobs.iter().filter(|j| is_mate(j)).count() + 1 < MIN_LIST_BATCH {
            return Vec::new();
        }
        let (mates, rest): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|j| is_mate(j));
        self.jobs = rest.into();
        mates
    }

    fn take(&mut self) -> Option<Job> {
        let room = self.background_running < BACKGROUND_CHECKS;
        let at = self.jobs.iter().position(|j| room || !j.background)?;
        let job = self.jobs.remove(at)?;
        if job.background {
            self.background_running += 1;
        }
        Some(job)
    }

    /// Jobs the user asked for first, then background ones on screen; within each, those with nothing to show yet
    /// first, otherwise in the order they came.
    pub(super) fn order(&mut self) {
        let visible = &self.visible;
        let rank = |j: &Job| {
            let waiting = u8::from(!j.announce);
            if j.background {
                2 + u8::from(!visible.contains(&j.id.connection)) * 2 + waiting
            } else {
                waiting
            }
        };
        self.jobs.make_contiguous().sort_by_key(rank);
    }
}

/// A running background job's place in [`Queue::background_running`], given back when dropped, also when the job
/// panics: a slot kept by a panic would stop every later background check.
pub(super) struct BackgroundSlot<'a> {
    pub(super) queue: &'a Mutex<Queue>,
    pub(super) wake: &'a Condvar,
}

impl Drop for BackgroundSlot<'_> {
    fn drop(&mut self) {
        lock(self.queue).background_running -= 1;
        // A worker may be waiting for room to run the next one.
        self.wake.notify_all();
    }
}

pub(super) struct Running {
    pub(super) token: u64,
    pub(super) connection: crate::model::ConnectionId,
    /// The change count its answer is compared with ([`super::Changes::since`]); `None` until it is taken.
    pub(super) started: Option<u64>,
}

/// A running check's entry in [`Queue::announced`], taken out when dropped, also when the job panics.
struct Announced<'a> {
    queue: &'a Mutex<Queue>,
    token: u64,
}

impl<'a> Announced<'a> {
    fn new(queue: &'a Mutex<Queue>, connection: &crate::model::ConnectionId) -> Self {
        let mut q = lock(queue);
        q.tokens += 1;
        let token = q.tokens;
        q.announced.push(Running {
            token,
            connection: connection.clone(),
            started: None,
        });
        Self { queue, token }
    }

    fn started(&self, count: u64) {
        if let Some(r) = lock(self.queue)
            .announced
            .iter_mut()
            .find(|r| r.token == self.token)
        {
            r.started = Some(count);
        }
    }
}

impl Drop for Announced<'_> {
    fn drop(&mut self) {
        lock(self.queue).announced.retain(|r| r.token != self.token);
    }
}

/// Background checks running at once. Each is a `claude` process, a Node program of about 190 MB: three at once took
/// klefki from 123 MB to about 700 MB at startup. One keeps it near 300 MB.
const BACKGROUND_CHECKS: usize = 1;

/// Background checks of one folder that are answered by one `claude mcp list` instead of one `mcp get` each: the
/// list costs one Claude start and checks all servers at once (30 servers: 2.6 s against 28.5 s), but it starts
/// every server of the folder, so for a few separate checks are as quick and touch only those.
const MIN_LIST_BATCH: usize = 4;

/// A list holds a line for every server of the folder, with its command.
const MAX_LIST_OUTPUT: usize = 4 * 1024 * 1024;

/// What a list is run with, besides the usual environment.
/// - No claude.ai connectors: a list also checks those over the network (21 of them took 19-32 s where the servers in
///   the files took 7 s), and they are not servers of these files.
/// - A connection timeout of 3 s (Claude's own is 30 s) and 10 servers connected at once (Claude's is 3): a list
///   prints only when the last server is done, so one that hangs would hold back the answers of all the others for
///   30 s. Now that is about 8 s (the 3 s probe Claude starts with, the timeout and its own start); 6 hanging servers
///   took 13.5 s with 3 at once, 8.4 s with 10. A server that needs longer than 3 s is not given a failure for it:
///   see `parse::parse_list`, and it is checked with `get`.
const LIST_ENV: [(&str, &str); 3] = [
    ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
    ("MCP_TIMEOUT", "3000"),
    ("MCP_SERVER_CONNECTION_BATCH_SIZE", "10"),
];

pub(super) fn list_env<'a>(mut base: Vec<(&'a str, &'a OsStr)>) -> Vec<(&'a str, &'a OsStr)> {
    base.extend(LIST_ENV.iter().map(|&(k, v)| (k, OsStr::new(v))));
    base
}

/// Turns a row into a job, or reports why it cannot be run. A name that is not safe never reaches a spawn. `file` is
/// how the file spells the row: the name passed to `claude mcp get` and the project it runs in are those, never the
/// cleaned text shown (which could name another server, or another folder).
pub(super) fn build_job(
    shared: &Shared,
    folders: &mut Folders,
    row: &ServerRow,
    file: Option<&FileKey>,
    kind: JobKind,
    announce: bool,
) -> Option<Job> {
    let refuse = |why: String| {
        match kind {
            // A row showing a last-known status keeps it: "not checked" would hide what little is known.
            JobKind::Check if !announce => {}
            JobKind::Check => shared.emitter.status(
                row.id(),
                Status::Unknown,
                Some(format!("not checked: {why}")),
            ),
            JobKind::Diagnose => shared.emitter.emit(Event::Diagnosis {
                id: row.id(),
                diagnosis: Diagnosis {
                    status: Status::Unknown,
                    detail: None,
                    hints: Vec::new(),
                    server_said: Vec::new(),
                    client_log: Vec::new(),
                    note: Some(format!("not diagnosed: {why}")),
                    folder: None,
                },
            }),
        }
        None
    };
    let (name, cwd) = match where_to_run(shared, folders, row, file) {
        Ok(found) => found,
        Err(why) => return refuse(why),
    };
    Some(Job {
        id: row.id(),
        name,
        cwd,
        kind,
        announce,
        background: false,
        now: false,
        alone: false,
        longer: false,
        folder: None,
    })
}

/// The name to pass to `claude` for a row and the folder to run it in, from how the file spells them, or why it
/// cannot run. Claude finds a local server through its working directory, so it runs in the project's folder.
pub(super) fn where_to_run(
    shared: &Shared,
    folders: &mut Folders,
    row: &ServerRow,
    file: Option<&FileKey>,
) -> Result<(ServerName, PathBuf), String> {
    let Some(file) = file else {
        return Err("the server is no longer in the configuration".to_owned());
    };
    // `claude mcp get` would answer for the server Claude Code loads in its place (decision 009).
    if let Some(why) = &row.not_loaded {
        return Err(format!("Claude Code does not load it. {why}"));
    }
    let name = ServerName::parse(&file.name)
        .map_err(|e| format!("the server name is not safe to pass to claude ({e})"))?;
    let cwd = match (row.scope, &file.project) {
        (Scope::User, _) => match &shared.config.home {
            Some(home) => home.clone(),
            None => return Err("no home folder".to_owned()),
        },
        (_, Some(project)) => {
            if !folders.usable(project) {
                return Err("the project folder was not found".to_owned());
            }
            PathBuf::from(project)
        }
        (_, None) => return Err("the project folder is unknown".to_owned()),
    };
    Ok((name, cwd))
}

/// Whether each project folder can be run in, looked up once per pass over the rows: a project has many servers.
#[derive(Default)]
pub(super) struct Folders(HashMap<String, bool>);

impl Folders {
    pub(super) fn usable(&mut self, project: &str) -> bool {
        if let Some(&usable) = self.0.get(project) {
            return usable;
        }
        let usable = inventory::is_safe_project_path(project) && Path::new(project).is_dir();
        self.0.insert(project.to_owned(), usable);
        usable
    }
}

pub(super) fn worker(shared: &Shared) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(job) = queue.take() {
                    break job;
                }
                queue = shared
                    .wake_workers
                    .wait(queue)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        // Built only for a background job: dropping one gives a slot back.
        let _slot = job.background.then(|| BackgroundSlot {
            queue: &shared.queue,
            wake: &shared.wake_workers,
        });
        run(shared, &job);
    }
}

/// Says a job is checking, when it should, and notes the change count its answer is compared with. Taken after the
/// announcement: an edit noted before this is one the check will see.
fn begin<'a>(shared: &'a Shared, job: &Job) -> (Option<Announced<'a>>, u64) {
    let announced = job
        .announce
        .then(|| Announced::new(&shared.queue, &job.id.connection));
    if job.announce {
        let detail = job.longer.then(|| {
            format!(
                "giving it up to {} to connect",
                crate::words::duration(LONGER_CHECK)
            )
        });
        shared
            .emitter
            .status(job.id.clone(), Status::Checking, detail);
    }
    let started = lock(&shared.changes).count;
    if let Some(a) = &announced {
        a.started(started);
    }
    (announced, started)
}

fn run(shared: &Shared, job: &Job) {
    let (_announced, started) = begin(shared, job);
    // An answer about a server edited or removed while this ran is about a definition that is gone.
    let current = || !lock(&shared.changes).since(&job.id.connection, started);
    match job.kind {
        JobKind::Check => {
            let listed = if job.background {
                check_listed(shared, job)
            } else {
                ListedCheck::Not
            };
            let (status, detail, timing) = match listed {
                ListedCheck::Answer((status, detail)) => (status, detail, None),
                ListedCheck::Not => execute(shared, job),
                // Its `get` goes in the queue behind those of the servers that were only slow.
                ListedCheck::Later => return,
            };
            if current() {
                shared
                    .emitter
                    .status_timed(job.id.clone(), status, detail, timing);
            }
        }
        JobKind::Diagnose => {
            let (diagnosis, timing) = run_diagnose(shared, job);
            if current() {
                // The status is the home folder's; a run in a project only answers the user who asked.
                if job.folder.is_none() {
                    shared.emitter.status_timed(
                        job.id.clone(),
                        diagnosis.status,
                        diagnosis.detail.clone(),
                        timing,
                    );
                }
                shared.emitter.emit(Event::Diagnosis {
                    id: job.id.clone(),
                    diagnosis,
                });
            } else {
                // The user asked: say why there is no diagnosis, instead of none ever coming.
                shared.emitter.emit(Event::Diagnosis {
                    id: job.id.clone(),
                    diagnosis: Diagnosis {
                        status: Status::Unknown,
                        detail: None,
                        hints: Vec::new(),
                        server_said: Vec::new(),
                        client_log: Vec::new(),
                        note: Some(
                            "not diagnosed: the server changed while it ran; diagnose it again"
                                .to_owned(),
                        ),
                        folder: job.folder.clone(),
                    },
                });
            }
        }
    }
}

pub(super) fn cli_env(shared: &Shared) -> Vec<(&str, &OsStr)> {
    shared
        .config
        .cli_env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect()
}

/// The environment of a run that checks a server. A check ends whatever Claude leaves running
/// (`RunSpec::end_leftovers`), so it must not start an update of Claude that would be ended with it.
pub(super) fn check_env(shared: &Shared) -> Vec<(&str, &OsStr)> {
    let mut env = cli_env(shared);
    env.push(("DISABLE_AUTOUPDATER", OsStr::new("1")));
    env
}

pub(super) fn cached_cli_version(shared: &Shared, program: &Path) -> Option<String> {
    let stamp = ProgramStamp::of(program);
    if let (Some(stamp), Some((known, version))) = (&stamp, &*lock(&shared.version))
        && stamp == known
    {
        return version.clone();
    }
    let version = cli_version(shared, program);
    // Only an answer is kept: a failed ask (a timeout, a virus scan on first start) is tried again next time.
    if let (Some(stamp), Some(_)) = (stamp, &version) {
        *lock(&shared.version) = Some((stamp, version.clone()));
    }
    version
}

fn cli_version(shared: &Shared, program: &Path) -> Option<String> {
    let out = runner::run(&RunSpec {
        program,
        args: &["--version"],
        cwd: shared.config.home.as_deref(),
        env: &cli_env(shared),
        timeout: VERSION_TIMEOUT,
        max_output: 1024,
        end_leftovers: false,
    })
    .ok()?;
    let token = out.stdout.split_whitespace().next()?;
    (out.exit_code == Some(0) && token.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| clip(token, 32))
}

/// What `check_listed` made of the job it was given.
enum ListedCheck {
    /// The list answered for it.
    Answer((Status, Option<String>)),
    /// No list was run for it: check it with `get` now.
    Not,
    /// The list had no answer for it, and it is queued again, to be checked with `get` in its turn.
    Later,
}

/// Answers the other background checks waiting in the job's folder from one `claude mcp list`, and returns the answer
/// for the job itself when the list had a clear one. A server the list did not answer clearly goes back to the queue
/// to be checked on its own, so what is shown is always what `claude mcp get` says. A list that answered nothing
/// ends listing for the rest of the run, and the job is checked as usual.
fn check_listed(shared: &Shared, job: &Job) -> ListedCheck {
    if job.alone || !shared.list_works.load(Ordering::Relaxed) {
        return ListedCheck::Not;
    }
    let Some(program) = claude_program(shared) else {
        return ListedCheck::Not;
    };
    let mates = lock(&shared.queue).take_mates(&job.cwd);
    if mates.is_empty() {
        return ListedCheck::Not;
    }
    let begun: Vec<_> = mates
        .into_iter()
        .map(|mate| {
            let started = begin(shared, &mate);
            (mate, started)
        })
        .collect();
    let names: Vec<&str> = std::iter::once(job.name.as_str())
        .chain(begun.iter().map(|(mate, _)| mate.name.as_str()))
        .collect();
    let env = list_env(cli_env(shared));
    let result = runner::run(&RunSpec {
        program: &program,
        args: &["mcp", "list"],
        cwd: Some(&job.cwd),
        env: &env,
        timeout: shared.config.check_timeout,
        max_output: MAX_LIST_OUTPUT,
        // It starts the servers, as `mcp get` does: none may outlive it.
        end_leftovers: true,
    });
    let answers = match result {
        Ok(out) if !out.timed_out && out.exit_code == Some(0) => {
            // Cut off in the middle of a line: that line is not read.
            let text = match (out.truncated, out.stdout.rfind('\n')) {
                (true, Some(end)) => &out.stdout[..end],
                (true, None) => "",
                (false, _) => out.stdout.as_str(),
            };
            parse::parse_list(text, &names)
        }
        _ => HashMap::new(),
    };
    if answers.is_empty() {
        shared.list_works.store(false, Ordering::Relaxed);
    }
    // The servers the list did not answer, with whether each never answered the connection.
    let mut again: Vec<(Job, bool)> = Vec::new();
    for (mate, (_announced, started)) in begun {
        match answers.get(mate.name.as_str()) {
            Some(parse::Listed::Settled(checked)) => {
                // Edited or removed while the list ran: the answer is about a definition that is gone.
                if !lock(&shared.changes).since(&mate.id.connection, started) {
                    shared
                        .emitter
                        .status(mate.id.clone(), checked.status, checked.detail.clone());
                }
            }
            other => again.push((
                Job {
                    alone: true,
                    ..mate
                },
                never_connected(other),
            )),
        }
    }
    let own = answers.get(job.name.as_str());
    if let Some(parse::Listed::Settled(checked)) = own {
        let answer = (checked.status, checked.detail.clone());
        return queue_again(shared, again, ListedCheck::Answer(answer));
    }
    // The job has no answer either: it waits for its turn like the others, so that a server that hangs does not
    // hold the rest back with its 30 s. It was taken before them, so it stays ahead of them.
    again.insert(
        0,
        (
            Job {
                alone: true,
                ..job.clone()
            },
            never_connected(own),
        ),
    );
    queue_again(shared, again, ListedCheck::Later)
}

fn never_connected(listed: Option<&parse::Listed>) -> bool {
    matches!(
        listed,
        Some(parse::Listed::Open {
            never_connected: true
        })
    )
}

/// Queues the servers a list did not answer, those that answered the connection too slowly first and those that never
/// did last (the order within each stays), and returns `then`.
fn queue_again(shared: &Shared, mut again: Vec<(Job, bool)>, then: ListedCheck) -> ListedCheck {
    again.sort_by_key(|(_, never_connected)| *never_connected);
    if !again.is_empty() {
        lock(&shared.queue).add(again.into_iter().map(|(job, _)| job).collect());
    }
    then
}

fn execute(shared: &Shared, job: &Job) -> (Status, Option<String>, Option<Timing>) {
    let Some(program) = claude_program(shared) else {
        return (
            Status::Unknown,
            Some("the claude CLI is not available".to_owned()),
            None,
        );
    };
    let limit = time_limit(shared, job);
    let timeout = run_timeout(shared, &limit, job.longer);
    let name = job.name.as_str();
    let args: &[&str] = if job.longer {
        &["--settings", LONGER_SETTINGS, "mcp", "get", name]
    } else {
        &["mcp", "get", name]
    };
    let started = Instant::now();
    let result = runner::run(&RunSpec {
        program: &program,
        args,
        cwd: Some(&job.cwd),
        env: &check_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
        end_leftovers: true,
    });
    let took = started.elapsed();
    let ran_out = matches!(&result, Ok(out) if out.timed_out);
    let (status, detail) = interpret(result, timeout);
    let timing = timing(job.longer, status, detail.as_deref(), ran_out, took, limit);
    (status, detail, timing)
}

/// The settings a check given more time runs with: passed as `--settings`, which wins over the user's and the
/// project's own `MCP_TIMEOUT` (measured with Claude Code 2.1.308), not over the organisation's.
pub(super) const LONGER_SETTINGS: &str = r#"{"env":{"MCP_TIMEOUT":"120000"}}"#;

/// Claude Code's time limit for connecting in the folder a job runs in.
pub(super) fn time_limit(shared: &Shared, job: &Job) -> TimeLimit {
    timeout::limit(
        shared.config.paths.as_ref(),
        Some(&job.cwd),
        &*shared.config.vars,
    )
}

/// How long to wait for Claude Code's answer: the usual wait, longer by as much as the limit there is above Claude
/// Code's own, so that a server given more time is answered for by Claude Code and not cut off by Klefki.
pub(super) fn run_timeout(shared: &Shared, limit: &TimeLimit, longer: bool) -> Duration {
    wait_for(shared.config.check_timeout, limit.limit, longer)
}

/// [`run_timeout`] for a usual wait of `usual` and a limit of `limit`. Never longer than a check given more time
/// waits: the limit can come from a cloned repository's settings, and an endless wait would hold a worker, and with
/// a few such servers every check.
pub(super) fn wait_for(usual: Duration, limit: Duration, longer: bool) -> Duration {
    let limit = if longer {
        LONGER_CHECK
    } else {
        limit.min(LONGER_CHECK)
    };
    usual + limit.saturating_sub(timeout::DEFAULT)
}

/// The timing to show with an answer: for a check that ran out of time, Claude Code's (`Request timed out`) or
/// Klefki's own, and for every check given more time.
pub(super) fn timing(
    longer: bool,
    status: Status,
    detail: Option<&str>,
    ran_out: bool,
    took: Duration,
    limit: TimeLimit,
) -> Option<Timing> {
    let timed_out = status == Status::Failed
        && detail.is_some_and(|d| hints_for(d).contains(&HintKind::TimedOut));
    (longer || ran_out || timed_out).then(|| Timing {
        took,
        limit,
        given: longer.then_some(LONGER_CHECK),
        timed_out: ran_out || timed_out,
    })
}

/// What a finished `claude mcp get` means: a status, and a reason when there is no verdict.
pub(super) fn interpret(
    result: io::Result<RunOutput>,
    timeout: Duration,
) -> (Status, Option<String>) {
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
