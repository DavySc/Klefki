//! Changes to the configuration: prepared as a plan the user reviews, then applied, and checked again a moment
//! later in case Claude Code wrote over them.
use super::client_secrets::{secret_store, store_problem};
use super::reload::{kept_list, profiles_changed, reload_after_change};
use super::{
    Change, Event, FileDiff, Input, MAX_DETAIL_CHARS, PlanView, Shared, State, backup_dir,
    claude_program, lock,
};
use crate::api::Scope;
use crate::cli::runner::RunOutput;
use crate::config::docstore::{Docstore, Op, Plan, StoreError};
use crate::config::fsx;
use crate::config::inventory;
use crate::ops;
use crate::sanitize::{clip, sanitize};
use crate::secrets::SecretStore;
use crate::swap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A prepared change: one plan per file, written settings files first and `.claude.json` last, after the servers
/// Claude Code adds, moves and changes itself.
pub(super) struct Stored {
    parts: Vec<(Plan, Docstore)>,
    guards: Vec<ops::Guard>,
    cli: Option<CliSteps>,
    /// Kept definitions to forget, once everything else is done.
    forget: Vec<String>,
    /// What the review said the plan does, for the activity log.
    summary: Vec<String>,
    /// Servers it renames or moves, by their ids before and after: their profiles follow them once it is applied.
    follow: Vec<(crate::model::ConnectionId, crate::model::ConnectionId)>,
    /// Profiles used, remembered once it is applied.
    profiles_used: Vec<ops::UsedProfile>,
    /// A profile stopped, forgotten (and deleted when asked) once it is applied.
    profile_stopped: Option<ops::StoppedProfile>,
    /// Switches another change writes: no profile puts them back once it is applied.
    switched: Vec<(String, String)>,
}

/// What Claude Code does with its own commands: servers it adds, servers it removes and adds again (or copies), and
/// kept definitions it adds back. With what they will change in `.claude.json` (shown, and checked again just before
/// the first command runs) and the store their client secrets are kept in, when any are involved.
struct CliSteps {
    adds: Vec<ops::CliAdd>,
    swaps: Vec<swap::Swap>,
    put_back: Vec<String>,
    preview: (Plan, Docstore),
    claude: Arc<dyn swap::Claude>,
    store: Option<Arc<dyn SecretStore>>,
}

#[derive(Default)]
pub(super) struct Plans {
    next: u64,
    stored: HashMap<u64, Stored>,
}

impl Plans {
    /// Takes the plan `id` out, to apply it or because it was discarded.
    pub(super) fn forget(&mut self, id: u64) -> Option<Stored> {
        self.stored.remove(&id)
    }
}

/// What runs Claude Code's commands that change servers: the CLI found on disk, or the stand-in a test gave.
pub(super) fn changer(shared: &Shared) -> Option<Arc<dyn swap::Claude>> {
    if let Some(changer) = &shared.config.changer {
        return Some(Arc::clone(changer));
    }
    Some(Arc::new(swap::Program {
        path: claude_program(shared)?,
        env: shared.config.cli_env.clone(),
        timeout: shared.config.check_timeout,
    }))
}

/// For each copy given another host, what of the original's it sends there: its header or environment values to
/// the server, its client secret (unless it signs in as another client) to where it signs in. A one-letter slip in
/// the host would hand them to a stranger, and the review shows them only as `<hidden>`.
fn new_hosts(
    changes: &[Change],
    text: &str,
    keys: &crate::config::inventory::FileKeys,
) -> Vec<String> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
    else {
        return Vec::new();
    };
    let host = |entry: &serde_json::Value, pointer: &str| {
        let url = url::Url::parse(entry.pointer(pointer)?.as_str()?).ok()?;
        let host = url.host_str()?.to_owned();
        Some(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host,
        })
    };
    let mut out = Vec::new();
    for change in changes {
        let Change::Duplicate {
            id, name, adjust, ..
        } = change
        else {
            continue;
        };
        if adjust.base.is_none() {
            continue;
        }
        let Some(key) = keys.get(&id.connection) else {
            continue;
        };
        let list = match &key.project {
            None => root.get("mcpServers"),
            Some(p) => root
                .get("projects")
                .and_then(|projects| projects.get(p))
                .and_then(|project| project.get("mcpServers")),
        };
        let Some(entry) = list.and_then(|l| l.get(&key.name)) else {
            continue;
        };
        let Ok(new) = adjust.apply(entry, &key.name) else {
            continue;
        };
        let filled = |k: &str| {
            entry
                .get(k)
                .and_then(serde_json::Value::as_object)
                .is_some_and(|values| !values.is_empty())
        };
        let mut sent: Vec<(&str, String)> = Vec::new();
        if (filled("headers") || filled("env"))
            && host(entry, "/url") != host(&new, "/url")
            && let Some(to) = host(&new, "/url")
        {
            sent.push(("header values", to));
        }
        if entry.get("oauth").is_some() && adjust.client.is_none() {
            let signs_in_at = if entry.pointer("/oauth/authServerMetadataUrl").is_some() {
                "/oauth/authServerMetadataUrl"
            } else {
                "/url"
            };
            if host(entry, signs_in_at) != host(&new, signs_in_at)
                && let Some(to) = host(&new, signs_in_at)
            {
                sent.push(("client secret", to));
            }
        }
        let what = match sent.as_slice() {
            [] => continue,
            [(what, to)] => format!("{what} to {to}"),
            [(a, to_a), (b, to_b)] if to_a == to_b => format!("{a} and {b} to {to_a}"),
            [(a, to_a), (b, to_b), ..] => format!("{a} to {to_a} and its {b} to {to_b}"),
        };
        out.push(format!(
            "{} sends the original's {}. Check the address before you go on.",
            sanitize(name),
            sanitize(&what)
        ));
    }
    out
}

/// For each server moved or copied into a project whose entry switches that name off: it arrives switched off there.
fn stays_off(changes: &[Change], text: &str) -> Vec<String> {
    let arrivals: Vec<(&str, &str)> = changes
        .iter()
        .flat_map(|change| match change {
            Change::Move {
                id,
                to: crate::model::Target::Local(project),
            } => vec![(id.name.as_str(), project.as_str())],
            Change::Duplicate { name, targets, .. } => targets
                .iter()
                .filter_map(|t| match t {
                    crate::model::Target::Local(project) => Some((name.as_str(), project.as_str())),
                    crate::model::Target::User => None,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    if arrivals.is_empty() {
        return Vec::new();
    }
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
    else {
        return Vec::new();
    };
    let projects = root.get("projects").and_then(serde_json::Value::as_object);
    arrivals
        .into_iter()
        .filter(|(name, project)| {
            let wanted = inventory::normalise_project(project);
            projects.into_iter().flatten().any(|(key, entry)| {
                inventory::normalise_project(key) == wanted
                    && entry
                        .get("disabledMcpServers")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|off| off.iter().any(|n| n.as_str() == Some(name)))
            })
        })
        .map(|(name, project)| {
            let (name, folder) = (sanitize(name), sanitize(crate::words::folder_name(project)));
            format!("{name} is switched off in {folder}, so it stays off there. Switch it on there to use it.")
        })
        .collect()
}

/// Adds a change to the activity log. A log that cannot be written leaves the change as it is; what went wrong is
/// returned, to be said with the result.
fn log_activity(
    shared: &Shared,
    summary: &[String],
    error: Option<&str>,
    backups: &[PathBuf],
) -> Option<String> {
    let dir = shared.config.backup_dir.as_ref()?;
    crate::activity::record(dir, summary, error, backups)
        .err()
        .map(|e| format!("The change is saved, but it could not be added to the activity log: {e}"))
}

pub(super) fn prepare(shared: &Shared, plans: &mut Plans, changes: &[Change]) {
    let made = (|| {
        let paths = shared.config.paths.as_ref().ok_or_else(|| {
            "Could not determine where Claude Code keeps its configuration.".to_owned()
        })?;
        // Always against the files as they are now, not the snapshot the user last saw. `.claude.json` is read once
        // for both the inventory and the plan.
        let read = fsx::read_limited_bom(&paths.claude_json, inventory::MAX_CLAUDE_JSON);
        let fresh = inventory::load_text(
            paths,
            match &read {
                Ok(text) => Ok(text.as_ref().map(|(t, _)| t.as_str())),
                Err(e) => Err(e.to_string()),
            },
        );
        let mut fresh = fresh;
        fresh.snapshot.kept = kept_list(shared);
        if let Some(dir) = shared.config.backup_dir.as_deref() {
            fresh.snapshot.profile_uses = crate::profile_uses::load(dir);
        }
        let mut resolved = match changes {
            [Change::Restore { backup }] => {
                let now = match &read {
                    Ok(Some((text, _))) => text.as_str(),
                    Ok(None) => "",
                    Err(e) => {
                        return Err(format!(
                            "The configuration could not be read, so nothing is restored: {e}"
                        ));
                    }
                };
                crate::restore::resolve(&backup_dir(shared)?, backup, now)?
            }
            _ => ops::resolve(changes, &fresh.snapshot, &fresh.keys)?,
        };
        // Switches a change other than a profile's writes: the user's choice from now on, which no profile puts back.
        if resolved.profiles_used.is_empty() && resolved.profile_stopped.is_none() {
            let mut switched = Vec::new();
            for op in &resolved.claude {
                match op {
                    Op::SetDisabled { project, name, .. } => {
                        switched.push((project.clone(), name.clone()));
                    }
                    Op::MigrateLegacyDisabled { project } => {
                        for legacy in fresh
                            .snapshot
                            .legacy_disabled
                            .iter()
                            .filter(|l| l.project == *project)
                        {
                            switched.extend(
                                legacy
                                    .turns_off
                                    .iter()
                                    .map(|n| (project.clone(), n.clone())),
                            );
                        }
                    }
                    _ => {}
                }
            }
            resolved
                .notes
                .extend(ops::no_longer_put_back(&switched, &fresh.snapshot));
            resolved.switched = switched;
        }
        // A kept definition whose removal also took another server's client secret is put back, not forgotten.
        for id in &resolved.forget {
            let kept = swap::Recovery::find(&backup_dir(shared)?, id)?;
            if let Some(why) = swap::forget_problem(&paths.claude_json, &kept) {
                return Err(why);
            }
        }
        // A kept definition's place stays free until it is put back or forgotten: its client secret is filed there.
        let arrivals = resolved.arrivals();
        if !arrivals.is_empty() {
            let dir = backup_dir(shared)?;
            for spot in &arrivals {
                swap::free_of_kept(&dir, spot)?;
            }
        }
        if let Ok(Some((text, _))) = &read {
            resolved.warnings.extend(stays_off(changes, text));
            // A mistyped host would get the original's credentials: said before anything else.
            resolved
                .warnings
                .splice(0..0, new_hosts(changes, text, &fresh.keys));
        }
        // What a swap does to other servers' sign-ins depends on what else is in the file.
        let root: serde_json::Value = if resolved.swaps.is_empty() {
            serde_json::Value::Null
        } else {
            read.as_ref()
                .ok()
                .and_then(|t| t.as_ref())
                .and_then(|(t, _)| serde_json::from_str(t).ok())
                .unwrap_or_default()
        };
        let mut parts = Vec::new();
        for (project, ops) in std::mem::take(&mut resolved.settings) {
            let file = Path::new(&project)
                .join(".claude")
                .join("settings.local.json");
            let store = Docstore::project_settings(file, backup_dir(shared)?);
            let plan = store.prepare(ops).map_err(|e| e.to_string())?;
            parts.push((plan, store));
        }
        if !resolved.claude.is_empty() {
            let mut store = Docstore::new(paths.claude_json.clone(), backup_dir(shared)?);
            store.verify_delay = shared.config.verify_delay;
            let claude_ops = std::mem::take(&mut resolved.claude);
            let plan = match read {
                Ok(Some((text, bom))) => store.prepare_text(text, bom, claude_ops),
                _ => store.prepare(claude_ops),
            }
            .map_err(|e| e.to_string())?;
            parts.push((plan, store));
        }
        for (_, store) in &mut parts {
            store.verify_delay = shared.config.verify_delay;
        }
        let cli = if resolved.cli.is_empty()
            && resolved.swaps.is_empty()
            && resolved.put_back.is_empty()
        {
            None
        } else {
            let claude = changer(shared).ok_or(
                "Servers that sign in with OAuth are added and changed with Claude Code's commands (claude mcp add-json and remove), and Klefki cannot find Claude Code.",
            )?;
            // What putting a kept definition back adds, shown like any add.
            for id in &resolved.put_back {
                let kept = swap::Recovery::find(&backup_dir(shared)?, id)?;
                let (op, warning) = swap::preview_put_back(&kept);
                resolved.preview.push(op);
                resolved.warnings.extend(warning);
            }
            // A server a restore puts back gets the client secret Klefki kept for it.
            if resolved.cli.iter().any(|a| a.stored) {
                let store = secret_store(shared).map_err(store_problem)?;
                let mut without = Vec::new();
                for add in resolved.cli.iter_mut().filter(|a| a.stored) {
                    let scope = if add.project.is_some() {
                        Scope::Local
                    } else {
                        Scope::User
                    };
                    let id =
                        crate::model::ConnectionId::new(scope, add.project.as_deref(), &add.name);
                    add.secret = store.get(&id).map_err(store_problem)?;
                    if add.secret.is_none() {
                        without.push(sanitize(&add.name));
                    }
                }
                if !without.is_empty() {
                    resolved.notes.push(format!(
                        "Klefki has no copy of the client secret of {}, so Claude Code keeps none. If a server needs one, enter it on its Auth tab once it is back.",
                        crate::words::and_list(&without)
                    ));
                }
            }
            let involves_secrets = !resolved.swaps.is_empty() || !resolved.put_back.is_empty();
            // Asked now, so a store that is missing or locked stops the change before anything runs.
            let store = if involves_secrets {
                let store = secret_store(shared).map_err(store_problem)?;
                let mut kept = Vec::new();
                // A swap's warnings (signing in again first) lead: they cost the most.
                let mut first = Vec::new();
                for s in std::mem::take(&mut resolved.swaps) {
                    let copy = store.get(&s.from.connection()).map_err(store_problem)?;
                    let review = swap::review(&root, &s, copy.is_some())?;
                    if review.unchanged {
                        continue;
                    }
                    first.extend(review.warnings);
                    resolved.notes.extend(review.notes);
                    // What the review shows: what Claude Code will write.
                    resolved.preview.extend(swap::preview(&root, &s));
                    kept.push(s);
                }
                resolved.warnings.splice(0..0, first);
                // A copy's new callback port, like an added server's (below): said once, however many places.
                let mut ports: Vec<(&str, u16)> = Vec::new();
                for s in &kept {
                    if let Some(Some(port)) = s.adjust.callback_port
                        && !ports.contains(&(s.to.name.as_str(), port))
                    {
                        ports.push((s.to.name.as_str(), port));
                        if let Some(warning) = crate::form::callback_port_warning(port) {
                            resolved.warnings.push(format!("{}: {warning}", s.to.name));
                        }
                    }
                }
                resolved.swaps = kept;
                Some(store)
            } else if resolved.cli.iter().any(|a| a.secret.is_some()) {
                let next = |e| {
                    let mut said = store_problem(e);
                    said = said.replacen(
                        " and press Review again.",
                        " and press Review again, or leave the client secret empty if the server does not need one.",
                        1,
                    );
                    said
                };
                let store = secret_store(shared).map_err(next)?;
                let first = &resolved.cli[0];
                let id = crate::model::ConnectionId::new(
                    if first.project.is_some() {
                        Scope::Local
                    } else {
                        Scope::User
                    },
                    first.project.as_deref(),
                    &first.name,
                );
                store.get(&id).map_err(next)?;
                Some(store)
            } else {
                None
            };
            // A fixed callback port another program holds makes the sign-in fail later, so it is said now. Each
            // server's port is checked once, even when it is added to several places.
            let mut checked = Vec::new();
            for add in &resolved.cli {
                let port = serde_json::from_str::<serde_json::Value>(&add.json)
                    .ok()
                    .and_then(|v| v.pointer("/oauth/callbackPort")?.as_u64())
                    .and_then(|p| u16::try_from(p).ok());
                if let Some(port) = port
                    && !checked.contains(&(add.name.as_str(), port))
                {
                    checked.push((add.name.as_str(), port));
                    if let Some(warning) = crate::form::callback_port_warning(port) {
                        resolved.warnings.push(format!("{}: {warning}", add.name));
                    }
                }
            }
            let docstore = Docstore::new(paths.claude_json.clone(), backup_dir(shared)?);
            let preview = docstore
                .preview(std::mem::take(&mut resolved.preview))
                .map_err(|e| e.to_string())?;
            let nothing = resolved.cli.is_empty()
                && resolved.swaps.is_empty()
                && resolved.put_back.is_empty();
            (!nothing).then(|| CliSteps {
                adds: std::mem::take(&mut resolved.cli),
                swaps: std::mem::take(&mut resolved.swaps),
                put_back: std::mem::take(&mut resolved.put_back),
                preview: (preview, docstore),
                claude,
                store,
            })
        };
        Ok::<_, String>((parts, cli, resolved))
    })();
    match made {
        Ok((parts, cli, resolved)) => {
            let ops::Resolved {
                guards,
                warnings,
                notes,
                sessions,
                summary,
                action,
                destructive,
                reviews,
                forget,
                already,
                others_on,
                profiles_used,
                profile_stopped,
                switched,
                ..
            } = resolved;
            let mut notes = notes;
            let follow = followed(changes);
            if let Ok(dir) = backup_dir(shared) {
                for (old, _) in &follow {
                    let held = crate::profiles::holding(&dir, old);
                    if !held.is_empty() {
                        notes.push(format!(
                            "{} is in the {} {}, which {} it where it goes.",
                            old.shown_name(),
                            if held.len() == 1 {
                                "profile"
                            } else {
                                "profiles"
                            },
                            crate::words::and_list(&held),
                            if held.len() == 1 { "keeps" } else { "keep" }
                        ));
                    }
                }
            }
            plans.next += 1;
            let id = plans.next;
            // Stopping a profile forgets what it switched, even where nothing is left to put back.
            let nothing_to_do = cli.is_none()
                && forget.is_empty()
                && profile_stopped.is_none()
                && parts.iter().all(|(plan, _)| plan.is_empty());
            let view = PlanView {
                id,
                summary: summary.clone(),
                action,
                destructive,
                notes: if nothing_to_do { Vec::new() } else { notes },
                sessions: sessions.filter(|_| !nothing_to_do),
                files: cli
                    .iter()
                    .map(|c| &c.preview)
                    .chain(&parts)
                    .flat_map(|(plan, _)| plan.changes())
                    .filter(|c| !c.lines.is_empty())
                    .map(|c| FileDiff {
                        path: c.path.clone(),
                        lines: c.lines.clone(),
                    })
                    .collect(),
                // Nothing to approve when nothing would be written.
                reviews: if nothing_to_do { Vec::new() } else { reviews },
                warnings,
                nothing_to_do,
                already: if nothing_to_do { already } else { Vec::new() },
                others_on: nothing_to_do && others_on,
            };
            plans.stored.insert(
                id,
                Stored {
                    parts,
                    guards,
                    cli,
                    forget,
                    summary,
                    follow,
                    profiles_used,
                    profile_stopped,
                    switched,
                },
            );
            shared.emitter.emit(Event::PlanReady(view));
        }
        Err(e) => shared.emitter.emit(Event::PlanFailed(sanitize(&e))),
    }
}

/// The servers `changes` rename or move, by their ids before and after, as the files will spell them.
fn followed(changes: &[Change]) -> Vec<(crate::model::ConnectionId, crate::model::ConnectionId)> {
    use crate::model::{ConnectionId, Target};
    changes
        .iter()
        .filter_map(|change| {
            let (id, scope, project, name) = match change {
                Change::Rename { id, name } => {
                    let (scope, project, _) = ConnectionId::parse(id.connection.as_str())?;
                    (id, scope, project, name.clone())
                }
                Change::Move { id, to } => {
                    let (_, _, name) = ConnectionId::parse(id.connection.as_str())?;
                    match to {
                        Target::User => (id, Scope::User, None, name),
                        Target::Local(p) => (id, Scope::Local, Some(p.clone()), name),
                    }
                }
                _ => return None,
            };
            Some((
                id.connection.clone(),
                ConnectionId::new(scope, project.as_deref(), &name),
            ))
        })
        .collect()
}

pub(super) fn apply(shared: &Shared, plans: &mut Plans, state: &mut State, id: u64) {
    let fail = |error: String| {
        shared.emitter.emit(Event::ApplyFailed { plan: id, error });
    };
    let Some(Stored {
        parts,
        guards,
        cli,
        forget,
        summary,
        follow,
        profiles_used,
        profile_stopped,
        switched,
    }) = plans.forget(id)
    else {
        return fail("This change was already applied or discarded. Prepare it again.".to_owned());
    };
    // What the user reviewed must still be what is there.
    if let Some(changed) = guards.iter().find(|g| !g.still_holds()) {
        return fail(format!(
            "{} changed in {} after you looked at it. Nothing was saved; review it again.",
            sanitize(&changed.name),
            sanitize(&changed.project)
        ));
    }
    let mut added = Vec::new();
    let mut backups = Vec::new();
    let mut notes = Vec::new();
    if let Some(steps) = &cli {
        let done = settle(shared)
            .and_then(|()| add_with_claude(shared, state, steps, &mut added))
            .and_then(|()| {
                change_with_claude(
                    shared,
                    state,
                    id,
                    steps,
                    &mut added,
                    &mut backups,
                    &mut notes,
                )
            });
        if let Err(e) = done {
            if !added.is_empty() || !backups.is_empty() {
                log_activity(shared, &summary, Some(&e), &backups);
            }
            return fail(e);
        }
    }
    let mut written = Vec::new();
    // Every file this change starts on, with its sequence number there; each gets checked afterwards, written or not.
    let mut started = Vec::new();
    for (plan, store) in &parts {
        // Numbered before the write, so an earlier change's check that takes the lock after it already knows.
        let lsn = lock(&shared.journals)
            .entry(store.path.clone())
            .or_default()
            .start();
        started.push((store.clone(), lsn));
        match store.apply(plan) {
            Ok(applied) => {
                if applied.wrote {
                    lock(&shared.journals)
                        .entry(store.path.clone())
                        .or_default()
                        .pending
                        .push((lsn, plan.clone()));
                    written.push(plan.clone());
                }
                backups.extend(applied.backup);
            }
            Err(e) => {
                let mut message = e.to_string();
                if !added.is_empty() {
                    message.push_str(&format!(" Claude Code already added {}.", added.join(", ")));
                }
                if !written.is_empty() {
                    let done: Vec<String> = written
                        .iter()
                        .flat_map(|p| p.changes().iter().map(|c| c.path.clone()))
                        .collect();
                    message.push_str(&format!(
                        " Already saved: {}. The rest was not changed.",
                        done.join(", ")
                    ));
                    reload_after_change(shared, state);
                }
                if !added.is_empty() || !written.is_empty() {
                    log_activity(shared, &summary, Some(&message), &backups);
                }
                // This change now numbers last on the file, so the earlier ones' checks stand down for it.
                check_later(shared, id, started);
                return fail(message);
            }
        }
    }
    let mut forgot = false;
    if !forget.is_empty() {
        let dir = match backup_dir(shared) {
            Ok(dir) => dir,
            Err(e) => return fail(e),
        };
        for kept in &forget {
            let claude_json = shared
                .config
                .paths
                .as_ref()
                .map(|p| p.claude_json.as_path());
            let forgot_it = match claude_json {
                Some(claude_json) => swap::forget(&dir, claude_json, kept),
                None => {
                    Err("Could not determine where Claude Code keeps its configuration.".to_owned())
                }
            };
            if let Err(e) = forgot_it {
                if !added.is_empty() || !written.is_empty() || forgot {
                    log_activity(shared, &summary, Some(&e), &backups);
                }
                state.reload(shared);
                return fail(e);
            }
            forgot = true;
        }
    }
    let changed = !written.is_empty() || !added.is_empty() || forgot;
    let mut uses_changed = false;
    // Klefki's own records of profile uses, once Claude Code's files are saved; read again with the snapshot below.
    if changed && let Ok(dir) = backup_dir(shared) {
        if !follow.is_empty() {
            match crate::profile_uses::follow(&dir, &follow) {
                Ok(moved) => uses_changed |= moved,
                Err(why) => notes.push(format!(
                    "What its profiles switched still names it as it was: {why}"
                )),
            }
        }
        match crate::profile_uses::forget_switches(&dir, &switched) {
            Ok(forgot) => uses_changed |= forgot,
            Err(why) => notes.push(format!(
                "A profile may still put back what this switched: {why}"
            )),
        }
        for used in &profiles_used {
            match crate::profile_uses::record(&dir, &used.profile, &used.flipped, &used.already) {
                Ok(recorded) => uses_changed |= recorded,
                Err(why) => notes.push(format!(
                    "Klefki could not remember what this switched, so stopping {} will not put it back: {why}",
                    sanitize(&used.profile)
                )),
            }
        }
    }
    if let Some(stopped) = &profile_stopped {
        let done = backup_dir(shared).and_then(|dir| {
            crate::profile_uses::stop(
                &dir,
                &stopped.profile,
                stopped.project.as_deref(),
                &stopped.handed,
            )?;
            // Forgotten already, so the snapshot is read again whether or not the delete goes through.
            uses_changed = true;
            if stopped.delete {
                shared.emitter.emit(Event::Profiles(crate::profiles::delete(
                    &dir,
                    &stopped.profile,
                )?));
            }
            Ok(())
        });
        if let Err(why) = done {
            notes.push(sanitize(&why));
        }
    }
    if !written.is_empty() || forgot {
        reload_after_change(shared, state);
    } else if uses_changed {
        profiles_changed(shared, state);
    }
    if changed && let Some(problem) = log_activity(shared, &summary, None, &backups) {
        notes.push(problem);
    }
    // A renamed or moved server stays in its profiles (decision 012).
    if changed
        && !follow.is_empty()
        && let Err(why) = backup_dir(shared).and_then(|dir| crate::profiles::follow(&dir, &follow))
    {
        notes.push(format!("Its profiles still name it as it was: {why}"));
    }
    check_later(shared, id, started);
    shared.emitter.emit(Event::Applied {
        plan: id,
        changed,
        backup: (!backups.is_empty()).then(|| {
            sanitize(
                &backups
                    .iter()
                    .map(|b| b.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }),
        notes: notes.iter().map(|n| sanitize(n)).collect(),
    });
}

/// Makes sure klefki's earlier changes to `.claude.json` whose check is still due are in place now, and retires
/// them, before Claude Code's own commands change the file. Replayed later, they would land on what those commands
/// wrote: an earlier edit of a server would undo a later one made through Claude Code. Their checks, still waiting,
/// stand down.
fn settle(shared: &Shared) -> Result<(), String> {
    let Some(paths) = &shared.config.paths else {
        return Ok(());
    };
    let log: Vec<Plan> = {
        let mut journals = lock(&shared.journals);
        let journal = journals.entry(paths.claude_json.clone()).or_default();
        journal.start();
        std::mem::take(&mut journal.pending)
            .into_iter()
            .map(|(_, plan)| plan)
            .collect()
    };
    if log.is_empty() {
        return Ok(());
    }
    let mut store = Docstore::new(paths.claude_json.clone(), backup_dir(shared)?);
    store.verify_delay = Duration::ZERO;
    store
        .verify(&log, &|| false)
        .map(|_| ())
        .map_err(|e| format!("Nothing was changed: {e}"))
}

/// Has Claude Code remove and add again (or copy) each server of `steps.swaps`, then add back each kept definition
/// of `steps.put_back`, one after the other, each as a transaction that leaves nothing lost (see `swap.rs`). A
/// failure says what is already done.
fn change_with_claude(
    shared: &Shared,
    state: &mut State,
    plan: u64,
    steps: &CliSteps,
    done: &mut Vec<String>,
    backups: &mut Vec<PathBuf>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    if steps.swaps.is_empty() && steps.put_back.is_empty() {
        return Ok(());
    }
    let paths = shared
        .config
        .paths
        .as_ref()
        .ok_or("Could not determine where Claude Code keeps its configuration.")?;
    let backup_dir = backup_dir(shared)?;
    let made = std::cell::RefCell::new(Vec::new());
    let ctx = swap::Ctx {
        backups: &made,
        claude_json: &paths.claude_json,
        backup_dir: &backup_dir,
        claude: steps.claude.as_ref(),
        store: steps.store.as_deref(),
        home: shared.config.home.as_deref(),
        progress: &|step| {
            shared.emitter.emit(Event::Applying {
                plan,
                step: sanitize(&step),
            })
        },
    };
    let earlier = |done: &[String]| {
        if done.is_empty() {
            String::new()
        } else {
            format!(" Already done: {}.", done.join(", "))
        }
    };
    let ran = steps
        .swaps
        .iter()
        .map(|s| (swap::run(&ctx, s), s.to.words()))
        .chain(
            steps
                .put_back
                .iter()
                .map(|id| (swap::put_back(&ctx, id), "the kept definition".to_owned())),
        );
    for (result, what) in ran {
        // Made whether the step finished or not: a failed one is logged with them.
        backups.append(&mut made.borrow_mut());
        match result {
            Ok(d) => {
                notes.extend(d.notes);
                done.push(what);
            }
            Err(e) => {
                // The list shows where things are now, a kept definition included.
                reload_after_change(shared, state);
                return Err(format!("{e}{}", earlier(done)));
            }
        }
    }
    reload_after_change(shared, state);
    Ok(())
}

/// Has Claude Code add each server of `steps` with `claude mcp add-json`, one after the other, and keeps each client
/// secret in Klefki's store under the new server's connection. `added` collects what was added, in words, so a
/// failure part way can say what is already done. The secret reaches the command only in `MCP_CLIENT_SECRET`.
fn add_with_claude(
    shared: &Shared,
    state: &mut State,
    steps: &CliSteps,
    added: &mut Vec<String>,
) -> Result<(), String> {
    let (preview, docstore) = &steps.preview;
    // What the user reviewed must still fit the file: the name still free in every place.
    docstore.still_applies(preview).map_err(|e| e.to_string())?;
    let done = |added: &[String]| {
        if added.is_empty() {
            String::new()
        } else {
            format!(" Already added: {}.", added.join(", "))
        }
    };
    for add in &steps.adds {
        let name = sanitize(&add.name);
        let place = match &add.project {
            None => "for the user".to_owned(),
            Some(p) => format!("in {}", sanitize(crate::words::folder_name(p))),
        };
        // Checked again here: a change in another Klefki may have kept a definition there since the review.
        let spot = swap::Spot {
            project: add.project.clone(),
            name: add.name.clone(),
        };
        if let Some(dir) = &shared.config.backup_dir
            && let Err(why) = swap::free_of_kept(dir, &spot)
        {
            return Err(format!(
                "{name} was not added {place}: {why}{}",
                done(added)
            ));
        }
        let mut args = vec![
            "mcp",
            "add-json",
            "-s",
            if add.project.is_some() {
                "local"
            } else {
                "user"
            },
        ];
        if add.secret.is_some() {
            args.push("--client-secret");
        }
        // The name is checked, but may start with `-`: after `--` it is never read as an option.
        args.extend(["--", add.name.as_str(), add.json.as_str()]);
        // From the folder as the file spells it, so Claude Code files the server under that project's own entry.
        let folder = add.project.as_deref().map(|project| {
            shared.config.paths.as_ref().map_or_else(
                || project.to_owned(),
                |paths| swap::spelled_in(&paths.claude_json, project),
            )
        });
        let cwd = match &folder {
            Some(folder) => Some(Path::new(folder)),
            None => shared.config.home.as_deref(),
        };
        // The secret reaches the command only in `MCP_CLIENT_SECRET`.
        let out = steps.claude.run(&args, cwd, add.secret.as_ref());
        let said = |out: &RunOutput| {
            let text = if out.stderr.trim().is_empty() {
                &out.stdout
            } else {
                &out.stderr
            };
            clip(text.trim(), MAX_DETAIL_CHARS)
        };
        match out {
            Err(e) => {
                return Err(format!(
                    "{name} was not added {place}: Claude Code's command could not be started ({}).{}",
                    sanitize(&e.to_string()),
                    done(added)
                ));
            }
            Ok(out) if out.timed_out => {
                reload_after_change(shared, state);
                return Err(format!(
                    "Claude Code did not finish adding {name} {place} in {} s, so it may or may not be there; look in the list.{}",
                    shared.config.check_timeout.as_secs(),
                    done(added)
                ));
            }
            Ok(out) if out.exit_code != Some(0) => {
                return Err(format!(
                    "{name} was not added {place}. Claude Code said: {}{}",
                    said(&out),
                    done(added)
                ));
            }
            Ok(_) => {}
        }
        reload_after_change(shared, state);
        let row = state.snapshot.servers.iter().find(|r| {
            state
                .keys
                .get(&r.connection)
                .is_some_and(|k| k.name == add.name)
                && match &add.project {
                    None => r.scope == Scope::User,
                    Some(p) => {
                        r.scope == Scope::Local
                            && r.project.as_deref().map(inventory::normalise_project)
                                == Some(inventory::normalise_project(p))
                    }
                }
        });
        let Some(row) = row else {
            return Err(format!(
                "Claude Code said it added {name} {place}, but Klefki cannot find it there.{}",
                done(added)
            ));
        };
        let earlier = done(added);
        added.push(format!("{name} {place}"));
        if let (Some(secret), Some(store)) = (&add.secret, &steps.store)
            && let Err(e) = store.set(&row.connection, secret)
        {
            return Err(format!(
                "{name} was added {place}, and Claude Code keeps its client secret, but Klefki could not keep its own copy: {e} Type it in on the server's Auth tab to try again.{earlier}"
            ));
        }
    }
    Ok(())
}

/// The changes klefki wrote to one file and has not yet checked, oldest first: a small redo log. Claude Code may
/// write without our lock from a copy it read earlier; replaying the log in order puts every overwritten change
/// back, and two changes to the same thing end the way the later one says.
#[derive(Default)]
pub(super) struct Journal {
    /// The sequence number of the latest change started on the file.
    latest: u64,
    pending: Vec<(u64, Plan)>,
}

impl Journal {
    fn start(&mut self) -> u64 {
        self.latest += 1;
        self.latest
    }
}

/// Shortly after a change, off the engine thread, checks each file it started on against that file's whole log.
/// Only the latest change on a file checks it (older ones stand down, the newest log covers theirs), the way a
/// database groups commits; once checked, the logged changes are retired.
fn check_later(shared: &Shared, id: u64, started: Vec<(Docstore, u64)>) {
    let journals = Arc::clone(&shared.journals);
    let logs: Vec<_> = {
        let j = lock(&journals);
        started
            .into_iter()
            .filter_map(|(store, lsn)| {
                let pending = &j.get(&store.path)?.pending;
                let last = pending.last()?.0;
                let log: Vec<Plan> = pending.iter().map(|(_, p)| p.clone()).collect();
                Some((store, lsn, last, log))
            })
            .collect()
    };
    if logs.is_empty() {
        return;
    }
    let emitter = shared.emitter.clone();
    let input = shared.input.clone();
    std::thread::spawn(move || {
        let mut reapplied = false;
        let mut failed = Vec::new();
        for (store, lsn, last, log) in &logs {
            let superseded = || lock(&journals).get(&store.path).map(|j| j.latest) != Some(*lsn);
            let result = store.verify(log, &superseded);
            // Retired whatever the outcome: a log that cannot be put back is reported now, not retried forever.
            if let Some(j) = lock(&journals).get_mut(&store.path)
                && j.latest == *lsn
            {
                j.pending.retain(|(n, _)| n > last);
            }
            match result {
                Ok(again) => reapplied |= again,
                // The other files are still checked: each has its own log to retire.
                Err(e) => failed.push((store.path.clone(), e)),
            }
        }
        if reapplied {
            // The engine thread reads the files again, as for any change on disk.
            let _ = input.send(Input::FilesChanged);
        }
        if !failed.is_empty() {
            emitter.emit(Event::ApplyFailed {
                plan: id,
                error: after_save_failures(&failed),
            });
        } else if reapplied {
            emitter.emit(Event::Reapplied { plan: id });
        }
    });
}

/// Why the check after a save failed, once per reason, naming the files. The change itself was saved.
fn after_save_failures(failed: &[(PathBuf, StoreError)]) -> String {
    let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
    for (path, e) in failed {
        let reason = match e {
            StoreError::Locked | StoreError::Changed => {
                "Claude Code was busy writing its configuration. Look \
                again in a moment, and make the change again if it is gone."
                    .to_owned()
            }
            other => other.to_string(),
        };
        let file = sanitize(&path.display().to_string());
        match reasons.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, files)) => files.push(file),
            None => reasons.push((reason, vec![file])),
        }
    }
    reasons
        .iter()
        .map(|(reason, files)| {
            format!(
                "After saving, the check of {} failed: {reason}",
                files.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}
