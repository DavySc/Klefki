//! Reading the configuration again: a refresh, or after a change, when only what the change switched on or edited is
//! checked again.
use super::checks::{Folders, Job, JobKind, build_job, cached_cli_version};
use super::client_secrets::secret_store;
use super::{Event, Shared, State, lock};
use crate::api::{Scope, ServerRow, Snapshot, Status};
use crate::cli::locate;
use crate::config::inventory::{self, FileKeys};
use crate::config::preflight::Probe;
use crate::sanitize::sanitize;
use crate::swap;
use crate::watch;
use std::collections::HashSet;
use std::sync::Arc;

/// Klefki's own profile files changed, not Claude Code's: the snapshot names the profiles and their uses anew, without
/// reading the configuration again.
pub(super) fn profiles_changed(shared: &Shared, state: &mut State) {
    let mut snapshot = (*state.snapshot).clone();
    if let Some(dir) = shared.config.backup_dir.as_deref() {
        snapshot.profiles = crate::profiles::load(dir).profiles;
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
                let vars = &*shared.config.vars;
                // Claude Code fills `${NAME}` in from the settings files' `env` too. A project's own settings count
                // only where it is trusted, and only for its servers: left out here.
                let settings = crate::config::server_env::settings_env(paths, None);
                let probe = Probe::new(shared.config.path_var.as_deref(), vars, cfg!(windows))
                    .with_settings(settings.into_iter().map(|(name, _)| name));
                let next = inventory::load_probed(paths, Some(&probe));
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
            .map(|dir| crate::profiles::load(dir).profiles)
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

pub(super) fn refresh(shared: &Shared, state: &mut State, live_all: bool) {
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
        queue.drop_background_checks(&checking);
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
