//! The engine thread: it takes the UI's commands one at a time and reads the configuration again when it
//! changes, queueing the checks that calls for.
use super::changes::{Plans, apply, prepare};
use super::checks::{Folders, Job, JobKind, Queue, build_job, cached_cli_version, worker};
use super::client_secrets::{SecretOp, SecretsAsk, secret, secret_store, secrets};
use super::{
    Command, Emitter, EngineConfig, Event, Input, Shared, State, UpdateDownload, Updating,
    backup_dir, lock, sign_in,
};
use crate::api::{Scope, ServerId, ServerRow, Snapshot, Status};
use crate::cli::locate;
use crate::config::entry;
use crate::config::inventory::{self, FileKeys};
use crate::sanitize::sanitize;
use crate::swap;
use crate::watch::{self, Watch};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};

pub(super) fn engine_main(
    config: EngineConfig,
    inputs: Receiver<Input>,
    input_tx: Sender<Input>,
    emitter: Emitter,
    updating: Arc<Mutex<Updating>>,
) {
    let workers = config.max_checks.max(1);
    let shared = Arc::new(Shared {
        config,
        queue: Mutex::new(Queue::default()),
        wake_workers: Condvar::new(),
        changes: Mutex::default(),
        claude: RwLock::new(None),
        version: Mutex::new(None),
        journals: Arc::default(),
        secret_store: Mutex::new(None),
        list_works: AtomicBool::new(true),
        secrets_listed: Mutex::new(None),
        emitter,
        input: input_tx.clone(),
        updating,
    });
    for _ in 0..workers {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || worker(&shared));
    }

    let watch = shared.config.watch_interval.map(|interval| {
        Watch::start(interval, shared.config.watch_settle, move || {
            let _ = input_tx.send(Input::FilesChanged);
        })
    });
    let mut state = State {
        snapshot: Arc::new(Snapshot::default()),
        keys: FileKeys::new(),
        watch,
        sign_in: None,
    };
    let mut plans = Plans::default();
    refresh(&shared, &mut state, false);
    // scripts/perf-baseline.py times start-up to here, when the UI has the configuration to show: this file appears.
    if let Some(mark) = std::env::var_os("KLEFKI_STARTUP_MARK") {
        let _ = std::fs::write(mark, "first snapshot sent\n");
    }
    while let Ok(input) = inputs.recv() {
        let command = match input {
            Input::Ui(command) => command,
            Input::FilesChanged => {
                reload_after_change(&shared, &mut state);
                continue;
            }
            Input::Closed => break,
        };
        match command {
            Command::Prepare(changes) => prepare(&shared, &mut plans, &changes),
            Command::Apply(id) => apply(&shared, &mut plans, &mut state, id),
            Command::Discard(id) => {
                plans.stored.remove(&id);
            }
            Command::Refresh { live_all } => refresh(&shared, &mut state, live_all),
            Command::Visible(rows) => {
                let mut queue = lock(&shared.queue);
                queue.visible = rows.into_iter().collect();
                queue.order();
            }
            Command::Check(id) => enqueue_now(&shared, &state, &id, JobKind::Check),
            Command::Diagnose(id) => enqueue_now(&shared, &state, &id, JobKind::Diagnose),
            Command::Inspect(id) => inspect(&shared, &state, id),
            Command::Export(ids) => {
                let sources = crate::export::sources(
                    shared.config.paths.as_ref(),
                    &state.snapshot,
                    &state.keys,
                    &ids,
                );
                shared
                    .emitter
                    .emit(Event::Exported(crate::export::export(sources)));
            }
            Command::ChooseExportFile { name, shown } => {
                let home = shared.config.home.clone();
                // Only the UI's thread may open a dialog (on macOS, AppKit is main-thread only), so without one
                // opened there, a path is asked for instead.
                match shown.and_then(|s| s.take()) {
                    None => {
                        let choice = crate::export_file::not_shown(&name, home.as_deref());
                        shared.emitter.emit(Event::ExportFileChosen(choice));
                    }
                    // The dialog waits for the user, so its answer is waited for on a thread of its own: the engine
                    // keeps going.
                    Some((opened, answer)) => {
                        let emitter = shared.emitter.clone();
                        std::thread::spawn(move || {
                            let picked = pollster::block_on(answer);
                            let choice = crate::export_file::answered(
                                opened,
                                picked,
                                &name,
                                home.as_deref(),
                            );
                            emitter.emit(Event::ExportFileChosen(choice));
                        });
                    }
                }
            }
            Command::SaveExport {
                path,
                text,
                replace,
            } => {
                let result =
                    crate::export_file::save(shared.config.paths.as_ref(), &path, &text, replace);
                let shown = crate::sanitize::shown(&path.to_string_lossy());
                shared.emitter.emit(Event::ExportSaved {
                    path,
                    shown,
                    result,
                });
            }
            Command::ChooseImportFile { shown } => match shown.and_then(|s| s.take()) {
                // Only the UI's thread may open a dialog, so without one opened there, a path is asked for.
                None => shared
                    .emitter
                    .emit(Event::ImportFile(crate::import_file::ImportFile::NoDialog)),
                // Waited for, and the file read, on a thread of its own: the engine keeps going.
                Some((opened, answer)) => {
                    let emitter = shared.emitter.clone();
                    std::thread::spawn(move || {
                        let picked = pollster::block_on(answer);
                        let file = match crate::import_file::answered(opened, picked) {
                            Ok(path) => crate::import_file::read(&path),
                            Err(none) => none,
                        };
                        emitter.emit(Event::ImportFile(file));
                    });
                }
            },
            // Read on a thread of its own too: a path on a network share that stopped answering would hold up
            // every other command.
            Command::ReadImportFile(path) => {
                let emitter = shared.emitter.clone();
                std::thread::spawn(move || {
                    emitter.emit(Event::ImportFile(crate::import_file::read(&path)));
                });
            }
            Command::SecretStatus(id) => secret(&shared, &state.snapshot, id, SecretOp::Status),
            Command::StoreSecret { id, secret: value } => {
                secret(&shared, &state.snapshot, id, SecretOp::Store(value));
            }
            Command::ForgetSecret(id) => secret(&shared, &state.snapshot, id, SecretOp::Forget),
            Command::SignIn(id) => sign_in::start(&shared, &mut state, id),
            Command::SignInPaste { id, address } => sign_in::paste(&state, &id, address),
            Command::OpenSignInPage(id) => sign_in::open_again(&shared, &state, &id),
            Command::KeepWaitingForSignIn(id) => sign_in::keep_waiting(&state, &id),
            Command::CancelSignIn(id) => sign_in::cancel(&state, &id),
            Command::CheckCallbackPort(port) => shared.emitter.emit(Event::CallbackPort {
                port,
                warning: crate::form::callback_port_warning(port),
            }),
            Command::Activity => shared
                .emitter
                .emit(Event::Activity(match backup_dir(&shared) {
                    Ok(dir) => crate::activity::load(&dir),
                    Err(problem) => crate::activity::Activity {
                        problem: Some(problem),
                        ..Default::default()
                    },
                })),
            Command::Profiles => shared
                .emitter
                .emit(Event::Profiles(match backup_dir(&shared) {
                    Ok(dir) => crate::profiles::load(&dir),
                    Err(problem) => crate::profiles::Profiles {
                        problem: Some(problem),
                        ..Default::default()
                    },
                })),
            Command::SaveProfile {
                name,
                members,
                replaces,
            } => {
                let saved = backup_dir(&shared).and_then(|dir| {
                    let saved = crate::profiles::save(&dir, &name, &members, replaces.as_deref())?;
                    // A renamed profile keeps what its uses switched, and a record left under its name is not its own.
                    // The profile itself is saved either way; a record not kept is said.
                    let claimed = crate::profile_uses::claim_name(
                        &dir,
                        replaces.as_deref(),
                        saved_name(&saved, &name),
                    );
                    Ok((saved, claimed))
                });
                match saved {
                    Ok((profiles, claimed)) => {
                        shared.emitter.emit(Event::Profiles(profiles));
                        match claimed {
                            Ok(false) => {}
                            Ok(true) => profiles_changed(&shared, &mut state),
                            Err(why) => shared.emitter.emit(Event::ProfileRefused(format!(
                                "The profile is saved, but what its uses switched could not be kept with it: {why}"
                            ))),
                        }
                    }
                    Err(why) => shared.emitter.emit(Event::ProfileRefused(why)),
                }
            }
            // Delete only: what its uses switched stays as it is, and is forgotten.
            Command::DeleteProfile(name) => {
                let left = backup_dir(&shared).and_then(|dir| {
                    let left = crate::profiles::delete(&dir, &name)?;
                    Ok((left, crate::profile_uses::forget(&dir, &name)))
                });
                match left {
                    Ok((profiles, forgot)) => {
                        shared.emitter.emit(Event::Profiles(profiles));
                        match forgot {
                            Ok(false) => {}
                            Ok(true) => profiles_changed(&shared, &mut state),
                            // A profile saved under this name later does not take the record over (claim_name).
                            Err(why) => shared.emitter.emit(Event::ProfileRefused(format!(
                                "The profile is deleted, but what its uses switched is still remembered: {why}"
                            ))),
                        }
                    }
                    Err(why) => shared.emitter.emit(Event::ProfileRefused(why)),
                }
            }
            Command::Desktop => shared
                .emitter
                .emit(Event::Desktop(crate::desktop::load(&shared.config.desktop))),
            Command::Secrets => secrets(&shared, &state, SecretsAsk::List),
            Command::SecretsAfterFileChange => secrets(&shared, &state, SecretsAsk::Reuse),
            Command::ForgetUnusedSecret(connection) => {
                secrets(&shared, &state, SecretsAsk::Forget(connection));
            }
            // A slow or unreachable network must not hold up other commands.
            Command::CheckForUpdates => {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    // Before the answer, so the app knows not to download again what it could not install.
                    if let Some(why) = shared
                        .config
                        .updates_dir
                        .as_deref()
                        .and_then(crate::install::take_failure)
                    {
                        shared
                            .emitter
                            .emit(Event::UpdateDownload(UpdateDownload::Failed(format!(
                                "The update could not be installed when Klefki closed: {why}"
                            ))));
                    }
                    let (found, release) = crate::update::check_release(
                        shared.config.releases.as_ref(),
                        shared.config.version,
                        shared.config.release_key.as_deref(),
                    );
                    if let Some(release) = release {
                        lock(&shared.updating).release = Some(release);
                    }
                    shared.emitter.emit(Event::Update(found));
                });
            }
            Command::DownloadUpdate => {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    if let Some(done) = download_update(&shared) {
                        shared.emitter.emit(Event::UpdateDownload(done));
                    }
                });
            }
            Command::OpenReleasePage(version) => {
                let _ = (shared.config.open_url)(&crate::update::release_page(version));
            }
        }
    }
    // The UI is gone: a sign-in still running is stopped, so no `claude` keeps listening after Klefki closes.
    sign_in::stop(&mut state);
    // The UI is gone: let the workers finish what they are doing and stop.
    lock(&shared.queue).shutdown = true;
    shared.wake_workers.notify_all();
}

/// The Secrets page: the definitions are read here, the system's store is listed on a thread of its own (it may be
/// slow, or ask the user to unlock it).
/// Downloads the release the last check found, unless it is already downloaded. `None` while another download of it
/// runs: that one answers.
fn download_update(shared: &Shared) -> Option<UpdateDownload> {
    // Nothing is downloaded that cannot be installed.
    if let Err(why) = crate::install::can_replace(&shared.config.install) {
        return Some(UpdateDownload::Manual(why));
    }
    let release = {
        let mut updating = lock(&shared.updating);
        let Some(release) = updating.release.clone() else {
            return Some(UpdateDownload::Failed("No newer release was found.".into()));
        };
        if let Some(staged) = &updating.staged
            && staged.version == release.version
            && staged.file.is_file()
        {
            return Some(UpdateDownload::Ready(staged.version));
        }
        if updating.downloading {
            return None;
        }
        updating.downloading = true;
        release
    };
    let Some(dir) = &shared.config.updates_dir else {
        lock(&shared.updating).downloading = false;
        return Some(UpdateDownload::Failed(
            "Klefki has no folder to download the update to.".into(),
        ));
    };
    let downloaded = crate::install::download(
        shared.config.releases.as_ref(),
        &release,
        &shared.config.install,
        dir,
    );
    let mut updating = lock(&shared.updating);
    updating.downloading = false;
    Some(match downloaded {
        Ok(staged) => {
            let version = staged.version;
            updating.staged = Some(staged);
            UpdateDownload::Ready(version)
        }
        Err(why) => UpdateDownload::Failed(why),
    })
}

fn inspect(shared: &Shared, state: &State, id: ServerId) {
    // The row's own scope, and the name and project as the file spells them (not as shown: cleaned, cut).
    let found = inventory::locate(&state.snapshot, &state.keys, &id);
    let in_repository = found.is_some_and(|(row, _)| row.scope == Scope::Project);
    let json = match (&shared.config.paths, found) {
        (None, _) => {
            Err("Could not determine where Claude Code keeps its configuration.".to_owned())
        }
        (Some(_), None) => Err("the server is no longer in the configuration".to_owned()),
        (Some(paths), Some((_, file))) => {
            inventory::server_entry(paths, file.project.as_deref(), &file.name, in_repository)
                .and_then(|entry| {
                    serde_json::to_string_pretty(&entry::masked(&entry)).map_err(|e| e.to_string())
                })
        }
    };
    shared.emitter.emit(Event::Entry {
        id,
        // Text from the files, shown in an editor: no control or bidi characters.
        json: json.map(|j| sanitize(&j)).map_err(|e| sanitize(&e)),
        editable: !in_repository,
    });
}

/// The name `typed` as the profiles now hold it (trimmed, as saved).
fn saved_name<'a>(saved: &'a crate::profiles::Profiles, typed: &'a str) -> &'a str {
    saved
        .profiles
        .iter()
        .find(|p| crate::profiles::same_name(&p.name, typed.trim()))
        .map_or(typed, |p| p.name.as_str())
}

/// Klefki's own profile files changed, not Claude Code's: the snapshot names the profiles and their uses anew, without
/// reading the configuration again.
pub(super) fn profiles_changed(shared: &Shared, state: &mut State) {
    let mut snapshot = (*state.snapshot).clone();
    if let Some(dir) = shared.config.backup_dir.as_deref() {
        snapshot.profiles = crate::profiles::load(dir)
            .profiles
            .into_iter()
            .map(|p| p.name)
            .collect();
        snapshot.profile_uses = crate::profile_uses::load(dir);
    }
    state.snapshot = Arc::new(snapshot);
    shared
        .emitter
        .emit(Event::Snapshot(Arc::clone(&state.snapshot)));
}

impl State {
    /// Reads the configuration again and returns the snapshot it replaces, and the servers still there whose
    /// definition changed. A check of a changed or removed server that is running now will not answer. The
    /// watcher's baseline is taken just before reading, so a change made meanwhile is noticed afterwards instead of
    /// being lost.
    pub(super) fn reload(
        &mut self,
        shared: &Shared,
    ) -> (Arc<Snapshot>, HashSet<crate::model::ConnectionId>) {
        let next = match &shared.config.paths {
            Some(paths) => {
                if let Some(watch) = &self.watch {
                    watch.rebase(watch::watched_files(paths, &self.snapshot));
                }
                let next = inventory::load(paths);
                if let Some(watch) = &self.watch {
                    // Projects that just appeared start being watched from now.
                    watch.extend(watch::watched_files(paths, &next.snapshot));
                }
                next
            }
            None => inventory::Inventory {
                snapshot: Snapshot {
                    warnings: vec![
                        "Could not determine where Claude Code keeps its configuration.".to_owned(),
                    ],
                    ..Snapshot::default()
                },
                keys: FileKeys::new(),
            },
        };
        let before_keys = std::mem::replace(&mut self.keys, next.keys);
        let mut edited = HashSet::new();
        {
            let mut changes = lock(&shared.changes);
            for (connection, old) in &before_keys {
                match self.keys.get(connection) {
                    Some(new) if new.definition == old.definition => {}
                    Some(_) => {
                        changes.note(connection);
                        edited.insert(connection.clone());
                    }
                    None => changes.note(connection),
                }
            }
        }
        let mut snapshot = next.snapshot;
        snapshot.kept = kept_list(shared);
        snapshot.profiles = shared
            .config
            .backup_dir
            .as_deref()
            .map(|dir| {
                crate::profiles::load(dir)
                    .profiles
                    .into_iter()
                    .map(|p| p.name)
                    .collect()
            })
            .unwrap_or_default();
        snapshot.profile_uses = shared
            .config
            .backup_dir
            .as_deref()
            .map(crate::profile_uses::load)
            .unwrap_or_default();
        let before = std::mem::replace(&mut self.snapshot, Arc::new(snapshot));
        shared
            .emitter
            .emit(Event::Snapshot(Arc::clone(&self.snapshot)));
        let running: HashSet<_> = {
            let mut queue = lock(&shared.queue);
            // A waiting check of a server that is gone would start a process for nothing.
            queue
                .jobs
                .retain(|j| self.keys.contains_key(&j.id.connection));
            let changes = lock(&shared.changes);
            // Not those whose server changed since they started: their answer will not come.
            queue
                .announced
                .iter()
                .filter(|r| {
                    !edited.contains(&r.connection)
                        && r.started.is_none_or(|n| !changes.since(&r.connection, n))
                })
                .map(|r| r.connection.clone())
                .collect()
        };
        // The snapshot made the app forget which checks are running.
        for row in self
            .snapshot
            .servers
            .iter()
            .filter(|r| running.contains(&r.connection))
        {
            shared.emitter.status(row.id(), Status::Checking, None);
        }
        (before, edited)
    }
}

/// The definitions Klefki kept because a change could not put them back, as the snapshot lists them.
pub(super) fn kept_list(shared: &Shared) -> Vec<crate::api::Kept> {
    let (Some(dir), Some(paths)) = (&shared.config.backup_dir, &shared.config.paths) else {
        return Vec::new();
    };
    // The system's store is opened only when a listing needs it: opening it can be slow, or ask the user.
    swap::Recovery::list(dir, &paths.claude_json, &|| secret_store(shared).ok())
        .into_iter()
        .map(|(r, back)| crate::api::Kept {
            id: r.id,
            name: sanitize(&r.name),
            place: swap::place(r.project.as_deref()),
            signs_in: swap::signs_in(&r.entry),
            back,
        })
        .collect()
}

fn refresh(shared: &Shared, state: &mut State, live_all: bool) {
    let (_, edited) = state.reload(shared);
    // Checks the user asked for stay: a server that is off, or a project's, would not be queued again.
    lock(&shared.queue).jobs.retain(|j| !j.background);
    let snapshot = &state.snapshot;

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
            show_edited(shared, snapshot, &edited, &HashSet::new(), &HashSet::new());
            return;
        }
    };
    *shared.claude.write().unwrap_or_else(|e| e.into_inner()) = Some(program.clone());
    shared
        .emitter
        .emit(Event::CliVersion(cached_cli_version(shared, &program)));

    let mut jobs = Vec::new();
    let mut tried = HashSet::new();
    let mut folders = Folders::default();
    let keys = &state.keys;
    for row in &snapshot.servers {
        let edit = edited.contains(&row.connection);
        // Project servers run the repository's own command: only on an explicit request. A plugin's servers are not
        // checked: their names are not ones Klefki passes to `claude` (decision 008).
        let wanted = matches!(row.scope, Scope::User | Scope::Local)
            && row.enabled
            && row.not_loaded.is_none()
            && (live_all || (shared.config.background_checks && (edit || needs_check(row))));
        if wanted && edit {
            tried.insert(row.connection.clone());
        }
        if wanted
            && let Some(mut job) = build_job(
                shared,
                &mut folders,
                row,
                keys.get(&row.connection),
                JobKind::Check,
                edit || row.status == Status::Unknown,
            )
        {
            job.background = !live_all;
            jobs.push(job);
        }
    }
    let checking = queue_checks(shared, jobs, &edited);
    show_edited(shared, snapshot, &edited, &tried, &checking);
}

/// After the configuration changed (by klefki, or on disk from outside): show the new state, and check only servers
/// the change switched on or edited. A full refresh would look for the CLI again, ask its version, and check servers
/// the change did not touch, each one a process start that the change did not call for.
pub(super) fn reload_after_change(shared: &Shared, state: &mut State) {
    if shared.config.paths.is_none() {
        return;
    }
    let (before, edited) = state.reload(shared);
    let snapshot = &state.snapshot;
    // The same server in both snapshots is the same connection, as the file spells it, not the shown name.
    let was_on: HashSet<_> = before
        .servers
        .iter()
        .filter(|old| old.enabled)
        .map(|old| &old.connection)
        .collect();
    let mut jobs = Vec::new();
    let mut tried = HashSet::new();
    let mut folders = Folders::default();
    for row in &snapshot.servers {
        let edit = edited.contains(&row.connection);
        let wanted = shared.config.background_checks
            && matches!(row.scope, Scope::User | Scope::Local)
            && row.enabled
            && (edit || (needs_check(row) && !was_on.contains(&row.connection)));
        if !wanted {
            continue;
        }
        if edit {
            tried.insert(row.connection.clone());
        }
        if let Some(job) = build_job(
            shared,
            &mut folders,
            row,
            state.keys.get(&row.connection),
            JobKind::Check,
            edit || row.status == Status::Unknown,
        ) {
            jobs.push(Job {
                background: true,
                ..job
            });
        }
    }
    let checking = queue_checks(shared, jobs, &edited);
    show_edited(shared, snapshot, &edited, &tried, &checking);
}

/// Queues background checks and returns the edited servers among them. A background check of an edited server
/// still waiting is replaced: it may not have said it was checking.
fn queue_checks(
    shared: &Shared,
    jobs: Vec<Job>,
    edited: &HashSet<crate::model::ConnectionId>,
) -> HashSet<crate::model::ConnectionId> {
    let checking: HashSet<_> = jobs
        .iter()
        .map(|j| j.id.connection.clone())
        .filter(|c| edited.contains(c))
        .collect();
    if !jobs.is_empty() {
        let mut queue = lock(&shared.queue);
        queue.jobs.retain(|q| {
            !(q.background && q.kind == JobKind::Check && checking.contains(&q.id.connection))
        });
        queue.add(jobs);
        drop(queue);
        shared.wake_workers.notify_all();
    }
    checking
}

/// The status the app shows for an edited server is about its old definition. Each says it is being checked again
/// (`checking`), or no longer claims a verdict. Those `tried` but not checking already said why they cannot be.
fn show_edited(
    shared: &Shared,
    snapshot: &Snapshot,
    edited: &HashSet<crate::model::ConnectionId>,
    tried: &HashSet<crate::model::ConnectionId>,
    checking: &HashSet<crate::model::ConnectionId>,
) {
    for row in snapshot.servers.iter().filter(|r| {
        edited.contains(&r.connection)
            && (checking.contains(&r.connection) || !tried.contains(&r.connection))
    }) {
        if checking.contains(&row.connection) {
            shared.emitter.status(row.id(), Status::Checking, None);
        } else if row.status_cached || row.status == Status::Unknown {
            shared.emitter.status(
                row.id(),
                Status::Unknown,
                Some("changed since its last check".to_owned()),
            );
        } else {
            // What the files say (waiting for approval), not a verdict of a check.
            shared.emitter.status(row.id(), row.status, None);
        }
    }
}

/// A row whose status klefki has not established: unknown, or only last known from Claude's cache (which can be any
/// age). A fresh needs-auth entry from Claude is not checked again until the user asks.
fn needs_check(row: &ServerRow) -> bool {
    row.status == Status::Unknown || row.status_cached
}

/// A check or diagnosis the user asked for: ahead of the background checks.
fn enqueue_now(shared: &Shared, state: &State, id: &ServerId, kind: JobKind) {
    let Some((row, file)) = inventory::locate(&state.snapshot, &state.keys, id) else {
        return;
    };
    if let Some(job) = build_job(shared, &mut Folders::default(), row, Some(file), kind, true) {
        lock(&shared.queue).push_now(job);
        shared.wake_workers.notify_one();
    }
}
