//! The engine thread: it takes the UI's commands one at a time and reads the configuration again when it
//! changes, queueing the checks that calls for.
use super::changes::{Plans, apply, prepare};
use super::checks::{Folders, Job, JobKind, Queue, build_job, worker};
use super::client_secrets::{SecretOp, SecretsAsk, secret, secrets};
use super::reload::{profiles_changed, refresh, reload_after_change};
use super::{
    Command, Emitter, EngineConfig, Event, Input, Shared, State, UpdateDownload, Updating,
    backup_dir, lock, sign_in,
};
use crate::api::{Scope, ServerId, Snapshot};
use crate::config::entry;
use crate::config::inventory::{self, FileKeys};
use crate::sanitize::sanitize;
use crate::watch::Watch;
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
                plans.forget(id);
            }
            Command::Refresh { live_all } => refresh(&shared, &mut state, live_all),
            Command::Visible(rows) => {
                let mut queue = lock(&shared.queue);
                queue.visible = rows.into_iter().collect();
                queue.order();
            }
            Command::Check(id) => enqueue_now(&shared, &state, &id, JobKind::Check, false),
            Command::CheckLonger(id) => enqueue_now(&shared, &state, &id, JobKind::Check, true),
            Command::Diagnose(id) => enqueue_now(&shared, &state, &id, JobKind::Diagnose, false),
            Command::Inspect(id) => inspect(&shared, &state, id),
            Command::LastStarts(id) => last_starts(&shared, &state, id),
            Command::InspectorCommand(id) => inspector_command(&shared, &state, id),
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

/// What Claude Code's logs say about the server's last starts. Reads only names and, of at most a few files, both
/// ends (decision 013).
fn last_starts(shared: &Shared, state: &State, id: ServerId) {
    let found = inventory::locate(&state.snapshot, &state.keys, &id);
    let row = state.snapshot.servers.iter().find(|r| r.is(&id));
    let starts = match (row, &shared.config.claude_logs) {
        (None, _) => crate::api::LastStarts {
            problem: Some("The server is no longer in the configuration.".to_owned()),
            ..Default::default()
        },
        (Some(_), None) => crate::api::LastStarts {
            problem: Some("Klefki could not tell where Claude Code keeps its logs.".to_owned()),
            ..Default::default()
        },
        (Some(row), Some(root)) => {
            // The name and project as the files spell them, not as shown.
            let (name, project) = match (
                found,
                crate::model::ConnectionId::parse(row.connection.as_str()),
            ) {
                (Some((_, file)), _) => (file.name.clone(), file.project.clone()),
                (None, Some((_, project, name))) => (name, project),
                (None, None) => (row.name.clone(), row.project.clone()),
            };
            let within = matches!(row.scope, Scope::Local | Scope::Project)
                .then_some(project.as_deref())
                .flatten();
            let secrets = match (&shared.config.paths, found) {
                (Some(paths), Some((_, file))) => inventory::server_entry(
                    paths,
                    file.project.as_deref(),
                    &file.name,
                    row.scope == Scope::Project,
                )
                .map(|entry| entry::values(&entry))
                .unwrap_or_default(),
                _ => Vec::new(),
            };
            let mut starts = crate::last_start::read(&crate::last_start::Ask {
                root,
                name: &name,
                within,
                elsewhere: &row.shadowed_in,
                home: shared.config.home.as_deref(),
                secrets: &secrets,
                now_ms: crate::activity::now_ms(),
                offset: &crate::activity::local_offset_secs,
            });
            let logged_as = crate::last_start::folder_name(&name);
            let mut others: Vec<String> = Vec::new();
            for other in &state.snapshot.servers {
                let other_name = state
                    .keys
                    .get(&other.connection)
                    .map(|k| k.name.clone())
                    .or_else(|| {
                        crate::model::ConnectionId::parse(other.connection.as_str()).map(|p| p.2)
                    })
                    .unwrap_or_else(|| other.name.clone());
                if other_name != name
                    && crate::last_start::folder_name(&other_name) == logged_as
                    && !others.contains(&other.name)
                {
                    others.push(other.name.clone());
                }
            }
            starts.shares_log_with = others;
            // A user server hidden in every known project still starts in other folders.
            if (row.shadowed && !row.scope.is_everywhere()) || row.not_loaded.is_some() {
                starts.problem = Some(
                    "Claude Code loads another server of this name in its place, so its logs are about that one."
                        .to_owned(),
                );
            }
            starts
        }
    };
    shared.emitter.emit(Event::LastStarts { id, starts });
}

/// The line that opens the server in the MCP Inspector. Only reads the definition (decision 014).
fn inspector_command(shared: &Shared, state: &State, id: ServerId) {
    let project = inventory::locate(&state.snapshot, &state.keys, &id)
        .filter(|(row, _)| matches!(row.scope, Scope::Local | Scope::Project))
        .and_then(|(_, file)| file.project.clone());
    let source = crate::export::sources(
        shared.config.paths.as_ref(),
        &state.snapshot,
        &state.keys,
        std::slice::from_ref(&id),
    )
    .pop();
    let line = match source {
        Some(source) => source
            .definition
            .and_then(|definition| {
                crate::inspector::command(&definition, project.as_deref(), cfg!(windows))
            })
            .map_err(|why| crate::sanitize::shown(&why)),
        None => Err("the server is no longer in the configuration".to_owned()),
    };
    shared.emitter.emit(Event::InspectorCommand { id, line });
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

/// A check or diagnosis the user asked for: ahead of the background checks.
fn enqueue_now(shared: &Shared, state: &State, id: &ServerId, kind: JobKind, longer: bool) {
    let Some((row, file)) = inventory::locate(&state.snapshot, &state.keys, id) else {
        return;
    };
    if let Some(job) = build_job(shared, &mut Folders::default(), row, Some(file), kind, true) {
        lock(&shared.queue).push_now(Job { longer, ..job });
        shared.wake_workers.notify_one();
    }
}
