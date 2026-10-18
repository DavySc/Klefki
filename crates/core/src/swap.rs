//! Changes made by removing a server and adding it again, so that one signing in with OAuth can be edited, moved or
//! copied (decision 006). Claude Code has no command that changes a server, and klefki writes no server with `oauth`
//! settings itself: such a server is removed with `claude mcp remove` and added with `claude mcp add-json`. Claude's
//! remove also forgets the client secret it keeps for the server, so the add hands it back, from klefki's own copy.
//!
//! Each change runs as a small transaction, and nothing is lost on the way:
//! 1. a private copy of `.claude.json` is kept, and the definition about to be removed is written to a recovery file;
//! 2. the old server is removed, and the new one added; both are then looked for in the file;
//! 3. when the add fails, the old one is added back, with the same client secret;
//! 4. the recovery file is deleted only once the old or the new definition is in the file. When even putting it back
//!    fails, the file stays, the snapshot lists it, and the user can put it back later ([`put_back`]).
//!
//! At every moment the old definition is in the configuration or in a recovery file, and its client secret, when klefki
//! keeps one, in the system's secret store under the server's connection.
use crate::api::Scope;
use crate::cli::runner::{self, RunOutput, RunSpec};
use crate::config::docstore::{self, Docstore, Op};
use crate::config::{entry, fsx, inventory};
use crate::model::{ConnectionId, ServerName};
use crate::sanitize::sanitize;
use crate::secrets::{Secret, SecretStore};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs Claude Code's own commands. The client secret, when given, travels only in `MCP_CLIENT_SECRET`.
pub(crate) trait Claude: Send + Sync {
    fn run(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        secret: Option<&Secret>,
    ) -> io::Result<RunOutput>;
}

/// The installed `claude`, started with no shell, a timeout and capped output.
pub(crate) struct Program {
    pub path: PathBuf,
    pub env: Vec<(String, std::ffi::OsString)>,
    pub timeout: Duration,
}

/// Output kept per stream; Claude's answers to these commands are a line or two.
const MAX_OUTPUT: usize = 64 * 1024;

impl Claude for Program {
    fn run(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        secret: Option<&Secret>,
    ) -> io::Result<RunOutput> {
        let mut env: Vec<(&str, &OsStr)> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_os_str()))
            .collect();
        if let Some(secret) = secret {
            env.push(("MCP_CLIENT_SECRET", OsStr::new(secret.expose())));
        }
        runner::run(&RunSpec {
            program: &self.path,
            args,
            cwd,
            env: &env,
            timeout: self.timeout,
            max_output: MAX_OUTPUT,
        })
    }
}

/// A server where `.claude.json` has it: `project` `None` for the user, else the project as the file spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spot {
    pub project: Option<String>,
    pub name: String,
}

impl Spot {
    pub(crate) fn scope(&self) -> Scope {
        if self.project.is_some() {
            Scope::Local
        } else {
            Scope::User
        }
    }

    /// Klefki keeps the client secret under this.
    pub(crate) fn connection(&self) -> ConnectionId {
        ConnectionId::new(self.scope(), self.project.as_deref(), &self.name)
    }

    /// `github for the user`, `github in app`.
    pub(crate) fn words(&self) -> String {
        format!(
            "{} {}",
            sanitize(&self.name),
            place(self.project.as_deref())
        )
    }

    fn same_place(&self, other: &Spot) -> bool {
        match (&self.project, &other.project) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                inventory::normalise_project(a) == inventory::normalise_project(b)
            }
            _ => false,
        }
    }
}

/// `for the user`, `in app`.
pub(crate) fn place(project: Option<&str>) -> String {
    match project {
        None => "for the user".to_owned(),
        Some(p) => format!("in {}", sanitize(&crate::ops::folder_name(p))),
    }
}

/// One server changed by removing and adding: edited in place, moved, or copied.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Swap {
    pub from: Spot,
    /// The old server is removed before the new one is added (an edit or a move); a copy leaves it.
    pub remove: bool,
    pub to: Spot,
    /// The new definition as edited, where `<hidden>` keeps the old value. `None` keeps the old definition.
    pub edited: Option<Value>,
}

impl Swap {
    fn verb(&self) -> &'static str {
        match (
            self.remove,
            self.from.same_place(&self.to) && self.from.name == self.to.name,
        ) {
            (true, true) => "saved",
            (true, false) => "moved",
            (false, _) => "copied",
        }
    }
}

/// What a swap needs around it.
pub(crate) struct Ctx<'a> {
    pub claude_json: &'a Path,
    pub backup_dir: &'a Path,
    pub claude: &'a dyn Claude,
    /// Klefki's copies of client secrets. Needed when a server that signs in with OAuth is involved.
    pub store: Option<&'a dyn SecretStore>,
    /// Where a command for the user's own servers runs.
    pub home: Option<&'a Path>,
    /// Told what is under way before each of Claude Code's commands, which can take seconds.
    pub progress: &'a dyn Fn(String),
}

impl Ctx<'_> {
    fn recovery_dir(&self) -> PathBuf {
        recovery_dir(self.backup_dir)
    }

    fn docstore(&self) -> Docstore {
        let mut store = Docstore::new(self.claude_json.to_owned(), self.backup_dir.to_owned());
        // Each step is looked for in the file right after it; there is no later check to wait for.
        store.verify_delay = Duration::ZERO;
        store
    }
}

/// Recovery files live next to the backups, in klefki's private folder.
pub(crate) fn recovery_dir(backup_dir: &Path) -> PathBuf {
    backup_dir.with_file_name("recovery")
}

/// The settings Claude Code 2.1.302's `add-json` writes; it drops any other (`alwaysAllow`, `cwd`, unknown keys).
const CLAUDE_KEEPS: [&str; 9] = [
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

/// `entry` as Claude Code's `add-json` writes it: without the settings it drops.
pub(crate) fn as_claude_writes(entry: &Value) -> Value {
    match entry.as_object() {
        Some(object) => Value::Object(
            object
                .iter()
                .filter(|(k, _)| CLAUDE_KEEPS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => entry.clone(),
    }
}

/// The settings of `entry` Claude Code's `add-json` drops, cleaned for showing.
fn dropped(entry: &Value) -> Vec<String> {
    entry
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| !CLAUDE_KEEPS.contains(&k.as_str()))
        .map(|(k, _)| sanitize(k))
        .collect()
}

/// Why Claude Code cannot change a server in `project` from where Klefki would run it, if it cannot: the folder is
/// gone, or (on Unix) the key reaches the folder through a link, and Claude Code files a server under the folder
/// itself, so the server would land under another key.
fn folder_problem(project: &str) -> Option<String> {
    let folder = sanitize(&crate::ops::folder_name(project));
    let path = Path::new(project);
    if !path.is_dir() {
        return Some(format!(
            "the folder of {folder} is not there, and Claude Code changes a project's server from inside it."
        ));
    }
    #[cfg(unix)]
    if let Ok(real) = std::fs::canonicalize(path)
        && inventory::normalise_project(&real.to_string_lossy())
            != inventory::normalise_project(project)
    {
        return Some(format!(
            "{folder} is reached through a link ({}), and Claude Code would file the server under the folder it leads to instead.",
            sanitize(&real.to_string_lossy())
        ));
    }
    None
}

/// The first spot of `spots` where Claude Code cannot change a server, in words.
fn folders_problem<'a>(spots: impl IntoIterator<Item = &'a Spot>) -> Option<String> {
    spots
        .into_iter()
        .filter_map(|s| s.project.as_deref())
        .find_map(folder_problem)
}

/// What a swap did, when it did what was asked.
#[derive(Debug, Default)]
pub(crate) struct Done {
    /// Where the copy of `.claude.json` taken first is.
    pub backup: Option<PathBuf>,
    /// Good to know: settings Claude Code left out, a secret copy klefki could not move.
    pub notes: Vec<String>,
}

/// Runs `swap`. On failure says what happened and where everything is; the configuration then holds the old server
/// as it was (or, when even that could not be done, a recovery file holds it).
pub(crate) fn run(ctx: &Ctx, swap: &Swap) -> Result<Done, String> {
    let verb = swap.verb();
    let shown = swap.from.words();
    let old = read_entry(ctx.claude_json, &swap.from)?.ok_or_else(|| {
        format!("{shown} is no longer in the configuration. Refresh and try again.")
    })?;
    let new = match &swap.edited {
        Some(edited) => entry::unmasked(edited, &old)
            .map_err(|why| format!("{shown} was not {verb}: {why}."))?,
        None => old.clone(),
    };
    let checked = if signs_in(&new) {
        entry::validate_new(&new)
    } else {
        entry::validate(&new)
    };
    checked
        .map_err(|why| format!("{shown} was not {verb}: the definition is not usable: {why}."))?;
    ServerName::parse(&swap.to.name).map_err(|e| {
        format!(
            "{shown} was not {verb}: {:?} cannot be used as a server name: {e}.",
            sanitize(&swap.to.name)
        )
    })?;
    let replaces_itself =
        swap.remove && swap.from.same_place(&swap.to) && swap.from.name == swap.to.name;
    if !replaces_itself && read_entry(ctx.claude_json, &swap.to)?.is_some() {
        return Err(format!(
            "{shown} was not {verb}: there is already a server called {}.",
            swap.to.words()
        ));
    }
    // Claude Code runs from the project's folder: checked before anything is removed.
    let removed_by_claude = swap.remove && signs_in(&old);
    let mut by_claude = Vec::new();
    if removed_by_claude {
        by_claude.push(&swap.from);
    }
    if signs_in(&new) {
        by_claude.push(&swap.to);
    }
    if let Some(why) = folders_problem(by_claude) {
        return Err(format!(
            "{shown} was not {verb}: {why}\nNothing was changed."
        ));
    }
    // Klefki's copy of the client secret: handed back to Claude Code with the add, since its remove forgets its own.
    let secret = if signs_in(&old) || signs_in(&new) {
        let store = ctx.store.ok_or_else(|| {
            format!("{shown} was not {verb}: Klefki can't use your system's secret store.")
        })?;
        store
            .get(&swap.from.connection())
            .map_err(|e| format!("{shown} was not {verb}: {e}"))?
    } else {
        None
    };
    let backup = ctx
        .docstore()
        .keep_copy()
        .map_err(|e| format!("{shown} was not {verb}: {e}"))?;
    let recovery = if swap.remove {
        Some(
            Recovery::save(ctx, &swap.from, &old, Some((&swap.to, &new)))
                .map_err(|e| format!("{shown} was not {verb}: Klefki could not keep its definition safe first ({}). Nothing was changed.", sanitize(&e.to_string())))?,
        )
    } else {
        None
    };
    if swap.remove
        && let Err(why) = remove(ctx, &swap.from, &old)
    {
        match read_entry(ctx.claude_json, &swap.from) {
            // Still there: nothing to undo.
            Ok(Some(_)) => {
                if let Some(r) = &recovery {
                    r.discard();
                }
                return Err(format!(
                    "{shown} was not {verb}: {why}\nNothing was changed."
                ));
            }
            // Gone after all (it timed out at the end, say): carry on.
            Ok(None) => {}
            Err(e) => {
                return Err(format!(
                    "{shown} was not {verb}: {why} Klefki cannot read the configuration to see where it stands ({e}); it kept the definition, so you can put it back from the Connections page."
                ));
            }
        }
    }
    let mut done = Done {
        backup: Some(backup),
        notes: Vec::new(),
    };
    let handed = secret.as_ref().filter(|_| signs_in(&new));
    match add(ctx, &swap.to, &new, handed) {
        Ok(left_out) => {
            if !left_out.is_empty() {
                done.notes.push(format!(
                    "Claude Code left out settings it does not use: {}. The copy Klefki kept of the configuration still has them.",
                    left_out.join(", ")
                ));
            }
        }
        Err(why) if !swap.remove => {
            return Err(format!("{shown} was not {verb}: {why}"));
        }
        Err(why) => {
            let back = secret.as_ref().filter(|_| signs_in(&old));
            return match add(ctx, &swap.from, &old, back) {
                Ok(left_out) if left_out.is_empty() => {
                    if let Some(r) = &recovery {
                        r.discard();
                    }
                    Err(format!(
                        "{shown} was not {verb}: {why}\nIt was put back as it was{}.",
                        if signs_in(&old) {
                            "; sign in to it again"
                        } else {
                            ""
                        }
                    ))
                }
                // Back, but without settings Claude Code dropped: the recovery file keeps the whole definition.
                Ok(left_out) => Err(format!(
                    "{shown} was not {verb}: {why}\nIt was put back, but Claude Code left out {}; Klefki kept the whole definition, so you can put that back from the Connections page.",
                    left_out.join(", ")
                )),
                Err(again) => Err(format!(
                    "{shown} was not {verb}: {why}\nPutting it back failed too: {again}\nKlefki kept its definition{}, so you can put it back from the Connections page once Claude Code works again.",
                    if back.is_some() {
                        " and its client secret"
                    } else {
                        ""
                    }
                )),
            };
        }
    }
    // Klefki's copy goes with the server, before the recovery file goes: a crash in between leaves the file, which
    // the next listing sees is done (see [`Recovery::list`]). A copy Klefki holds for the new place must be the one
    // Claude Code was given, or none: one left there from before would claim a secret Claude Code does not have.
    if let Some(store) = ctx.store.filter(|_| signs_in(&new)) {
        let (from, to) = (swap.from.connection(), swap.to.connection());
        if from != to {
            let note = |e: crate::secrets::SecretError| {
                format!(
                    "Claude Code has what it needs, but Klefki could not update its own copy of the client secret: {e} Type it in on the Auth tab of {} to keep one.",
                    swap.to.words()
                )
            };
            match handed {
                Some(secret) => match store.set(&to, secret) {
                    Err(e) => {
                        // Better none than a stale one.
                        let _ = store.delete(&to);
                        done.notes.push(note(e));
                    }
                    Ok(()) if swap.remove => {
                        if let Err(e) = store.delete(&from) {
                            done.notes.push(format!(
                                "Klefki could not delete its copy of the client secret for {}: {e}",
                                swap.from.words()
                            ));
                        }
                    }
                    Ok(()) => {}
                },
                None => {
                    if let Err(e) = store.delete(&to) {
                        done.notes.push(note(e));
                    }
                }
            }
        }
    }
    if let Some(r) = &recovery {
        r.discard();
    }
    Ok(done)
}

/// What a swap means for the user, said in the plan before anything runs.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Said {
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
    /// An edit that changes nothing: there is nothing to remove and add again.
    pub unchanged: bool,
}

/// What the plan says about `swap`, on the configuration `root` as it is now, when klefki keeps (`has_copy`) a copy
/// of the client secret or not. Refuses what would take a client secret from another server: Claude Code keeps one
/// sign-in for every server with the same name, type, address and headers, whatever its place, and its `remove`
/// forgets it for all of them.
pub(crate) fn review(root: &Value, swap: &Swap, has_copy: bool) -> Result<Said, String> {
    let mut said = Said::default();
    let Some(old) = entry_in(root, &swap.from) else {
        // Applying reports it is gone.
        return Ok(said);
    };
    let new = match &swap.edited {
        Some(edited) => entry::unmasked(edited, &old).unwrap_or_else(|_| edited.clone()),
        None => old.clone(),
    };
    let name = sanitize(&swap.from.name);
    if swap.remove && swap.from.same_place(&swap.to) && swap.from.name == swap.to.name && new == old
    {
        said.unchanged = true;
        return Ok(said);
    }
    let removes_sign_in = swap.remove && signs_in(&old);
    let mut by_claude = Vec::new();
    if removes_sign_in {
        by_claude.push(&swap.from);
    }
    if signs_in(&new) {
        by_claude.push(&swap.to);
    }
    if let Some(why) = folders_problem(by_claude) {
        let why = why[..1].to_uppercase() + &why[1..];
        return Err(format!("{why} Nothing was changed."));
    }
    if signs_in(&new) {
        let lost = dropped(&new);
        if !lost.is_empty() {
            said.warnings.push(format!(
                "Claude Code keeps only the settings it uses, so {} loses {}.",
                sanitize(&swap.to.name),
                lost.join(", ")
            ));
        }
    }
    if removes_sign_in {
        let sharing: Vec<String> = spots_in(root)
            .into_iter()
            .filter(|(spot, e)| {
                spot.name == swap.from.name
                    && !spot.same_place(&swap.from)
                    && signs_in(e)
                    && sign_in_key(e) == sign_in_key(&old)
            })
            .map(|(spot, _)| place(spot.project.as_deref()))
            .collect();
        let keeps_key = swap.to.name == swap.from.name
            && signs_in(&new)
            && sign_in_key(&new) == sign_in_key(&old);
        if !sharing.is_empty() {
            if !keeps_key || !has_copy {
                return Err(format!(
                    "{name} {} has the same name and address, and Claude Code keeps one sign-in for both: removing this one would take that one's client secret too. Copy it under another name instead, or change both the same way.",
                    sharing.join(" and ")
                ));
            }
            said.warnings.push(format!(
                "{name} {} shares this sign-in, so it is signed out too.",
                sharing.join(" and ")
            ));
        }
        said.warnings.push(format!(
            "Claude Code removes {name} and adds it again, so you will need to sign in to it again afterwards."
        ));
    }
    // The new definition's sign-in already belongs to another server: Claude Code would give both one client secret.
    let new_key = signs_in(&new)
        && (swap.to.name != swap.from.name
            || !signs_in(&old)
            || sign_in_key(&new) != sign_in_key(&old));
    if new_key {
        let taken: Vec<String> = spots_in(root)
            .into_iter()
            .filter(|(spot, e)| {
                spot.name == swap.to.name
                    && !spot.same_place(&swap.to)
                    && !(swap.remove && spot.same_place(&swap.from) && spot.name == swap.from.name)
                    && signs_in(e)
                    && sign_in_key(e) == sign_in_key(&new)
            })
            .map(|(spot, _)| place(spot.project.as_deref()))
            .collect();
        if !taken.is_empty() {
            return Err(format!(
                "{} {} has the same name and address, and Claude Code keeps one sign-in for both, so its client secret would be replaced. Use another name or address.",
                sanitize(&swap.to.name),
                taken.join(" and ")
            ));
        }
    }
    if signs_in(&new) && !has_copy {
        let shared = !swap.remove
            && swap.to.name == swap.from.name
            && signs_in(&old)
            && sign_in_key(&new) == sign_in_key(&old);
        if removes_sign_in {
            said.warnings.push(format!(
                "Klefki has no copy of {name}'s client secret. If it has one, Claude Code forgets it with this change: enter it on the Auth tab first, then review again. A server without a client secret is not affected."
            ));
        } else if !shared {
            said.notes.push(format!(
                "Klefki has no copy of {name}'s client secret, so Claude Code keeps none for {}. If the server needs one, enter it on {name}'s Auth tab first, then review again.",
                sanitize(&swap.to.name)
            ));
        }
    }
    Ok(said)
}

/// What the review shows for `swap`, on `root` as it is now: what Claude Code will write, without the settings it
/// drops. Values the user did not change stay hidden (`<hidden>`) as edited.
pub(crate) fn preview(root: &Value, swap: &Swap) -> Vec<Op> {
    let Some(old) = entry_in(root, &swap.from) else {
        return Vec::new();
    };
    let new = as_claude_writes(swap.edited.as_ref().unwrap_or(&old));
    if swap.remove && swap.from.same_place(&swap.to) && swap.from.name == swap.to.name {
        return vec![Op::ReplaceServer {
            project: swap.from.project.clone(),
            name: swap.from.name.clone(),
            entry: new,
        }];
    }
    let mut ops = Vec::new();
    if swap.remove {
        ops.push(Op::RemoveServer {
            project: swap.from.project.clone(),
            name: swap.from.name.clone(),
        });
    }
    ops.push(Op::AddServer {
        project: swap.to.project.clone(),
        name: swap.to.name.clone(),
        entry: new,
    });
    ops
}

/// What the review shows for putting `kept` back, and what it warns about.
pub(crate) fn preview_put_back(kept: &Recovery) -> (Op, Option<String>) {
    let lost = if signs_in(&kept.entry) {
        dropped(&kept.entry)
    } else {
        Vec::new()
    };
    let entry = if signs_in(&kept.entry) {
        as_claude_writes(&kept.entry)
    } else {
        kept.entry.clone()
    };
    (
        Op::AddServer {
            project: kept.project.clone(),
            name: kept.name.clone(),
            entry,
        },
        (!lost.is_empty()).then(|| {
            format!(
                "Claude Code keeps only the settings it uses, so {} loses {}; Klefki keeps the whole definition until you forget it.",
                sanitize(&kept.name),
                lost.join(", ")
            )
        }),
    )
}

/// What Claude Code keeps a server's sign-in under, besides its name: its type, address and headers.
fn sign_in_key(entry: &Value) -> (Option<&Value>, Option<&Value>, Value) {
    (
        entry.get("type"),
        entry.get("url"),
        entry.get("headers").cloned().unwrap_or_else(|| json!({})),
    )
}

/// Every user and local server in `root`, with where it is.
fn spots_in(root: &Value) -> Vec<(Spot, Value)> {
    let mut out = Vec::new();
    let servers = |list: Option<&Value>| -> Vec<(String, Value)> {
        list.and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(n, e)| (n.clone(), e.clone()))
            .collect()
    };
    for (name, e) in servers(root.get("mcpServers")) {
        out.push((
            Spot {
                project: None,
                name,
            },
            e,
        ));
    }
    for (project, p) in root
        .get("projects")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for (name, e) in servers(p.get("mcpServers")) {
            out.push((
                Spot {
                    project: Some(project.clone()),
                    name,
                },
                e,
            ));
        }
    }
    out
}

/// The definition at `spot` in `root`.
fn entry_in(root: &Value, spot: &Spot) -> Option<Value> {
    match &spot.project {
        None => root
            .get("mcpServers")
            .and_then(|s| s.get(&spot.name))
            .cloned(),
        Some(project) => {
            let wanted = inventory::normalise_project(project);
            root.get("projects")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(key, _)| inventory::normalise_project(key) == wanted)
                .find_map(|(_, p)| p.get("mcpServers").and_then(|s| s.get(&spot.name)).cloned())
        }
    }
}

/// Adds a definition that a recovery file kept, where it was, with klefki's copy of its client secret.
pub(crate) fn put_back(ctx: &Ctx, id: &str) -> Result<Done, String> {
    let record = Recovery::load(&ctx.recovery_dir(), id)?;
    let spot = Spot {
        project: record.project.clone(),
        name: record.name.clone(),
    };
    let shown = spot.words();
    if let Some(there) = read_entry(ctx.claude_json, &spot)? {
        if there == record.entry {
            record.discard();
            return Ok(Done::default());
        }
        return Err(format!(
            "{shown} was not put back: there is a server by that name there now. Rename or delete it first."
        ));
    }
    if signs_in(&record.entry)
        && let Some(why) = folders_problem([&spot])
    {
        return Err(format!("{shown} was not put back: {why}"));
    }
    let secret = if signs_in(&record.entry) {
        let store = ctx
            .store
            .ok_or("Klefki can't use your system's secret store.".to_owned())?;
        store
            .get(&spot.connection())
            .map_err(|e| format!("{shown} was not put back: {e}"))?
    } else {
        None
    };
    let backup = ctx
        .docstore()
        .keep_copy()
        .map_err(|e| format!("{shown} was not put back: {e}"))?;
    let left_out = add(ctx, &spot, &record.entry, secret.as_ref())
        .map_err(|why| format!("{shown} was not put back: {why}"))?;
    let mut done = Done {
        backup: Some(backup),
        notes: Vec::new(),
    };
    if left_out.is_empty() {
        record.discard();
    } else {
        done.notes.push(format!(
            "Claude Code left out settings it does not use: {}. Klefki keeps the whole definition until you forget it.",
            left_out.join(", ")
        ));
    }
    Ok(done)
}

/// Deletes a recovery file the user no longer needs.
pub(crate) fn forget(backup_dir: &Path, id: &str) -> Result<(), String> {
    Recovery::load(&recovery_dir(backup_dir), id)?.discard();
    Ok(())
}

pub(crate) fn signs_in(entry: &Value) -> bool {
    entry.get("oauth").is_some()
}

/// The definition at `spot`, read fresh from the file; `None` when there is none.
pub(crate) fn read_entry(claude_json: &Path, spot: &Spot) -> Result<Option<Value>, String> {
    let text = fsx::read_limited(claude_json, inventory::MAX_CLAUDE_JSON)
        .map_err(|e| sanitize(&e.to_string()))?
        .unwrap_or_default();
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| {
        format!(
            "the configuration is not valid JSON ({})",
            sanitize(&e.to_string())
        )
    })?;
    Ok(entry_in(&root, spot))
}

/// What Claude Code said, briefly, when it said anything.
fn said(out: &RunOutput) -> String {
    let text = if out.stderr.trim().is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    let text = sanitize(text.trim());
    let mut cut: String = text.chars().take(200).collect();
    if cut.len() < text.len() {
        cut.push('…');
    }
    // What follows is another sentence.
    if !cut.ends_with(['.', '!', '?', '…']) {
        cut.push('.');
    }
    cut
}

/// Runs one of Claude's commands for `spot`: `-s user` from the home folder, `-s local` from inside the project,
/// as Claude Code finds a local server by the folder it runs in. The name comes after `--`, so it is never an option.
fn command(
    ctx: &Ctx,
    spot: &Spot,
    verb: &str,
    flag: Option<&str>,
    json: Option<&str>,
    secret: Option<&Secret>,
) -> Result<RunOutput, String> {
    if let Some(why) = folders_problem([spot]) {
        return Err(why);
    }
    (ctx.progress)(match verb {
        "remove" => format!("Claude Code is removing {}\u{2026}", spot.words()),
        _ => format!("Claude Code is adding {}\u{2026}", spot.words()),
    });
    let mut args = vec![
        "mcp",
        verb,
        "-s",
        if spot.project.is_some() {
            "local"
        } else {
            "user"
        },
    ];
    args.extend(flag);
    args.extend(["--", spot.name.as_str()]);
    args.extend(json);
    let cwd = spot.project.as_deref().map(Path::new).or(ctx.home);
    ctx.claude.run(&args, cwd, secret).map_err(|e| {
        format!(
            "Claude Code's command could not be started ({}).",
            sanitize(&e.to_string())
        )
    })
}

/// Removes `spot`, whose definition is `old`. One that signs in with OAuth is removed by Claude Code; any other by
/// klefki, which keeps it byte for byte otherwise.
fn remove(ctx: &Ctx, spot: &Spot, old: &Value) -> Result<(), String> {
    if !signs_in(old) {
        let store = ctx.docstore();
        let plan = store
            .prepare(vec![Op::RemoveServer {
                project: spot.project.clone(),
                name: spot.name.clone(),
            }])
            .map_err(|e| e.to_string())?;
        store.apply(&plan).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let out = command(ctx, spot, "remove", None, None, None)?;
    if out.timed_out {
        return Err("Claude Code did not finish removing it in time.".to_owned());
    }
    if out.exit_code != Some(0) {
        return Err(format!("Claude Code said: {}", said(&out)));
    }
    if read_entry(ctx.claude_json, spot)?.is_some() {
        return Err("Claude Code said it removed it, but it is still there.".to_owned());
    }
    Ok(())
}

/// Adds `entry` at `spot`: one that signs in with OAuth through Claude Code (with its client secret), any other
/// through klefki. Then looks for it. Returns the settings Claude Code left out, which it does not know.
fn add(
    ctx: &Ctx,
    spot: &Spot,
    entry: &Value,
    secret: Option<&Secret>,
) -> Result<Vec<String>, String> {
    if !signs_in(entry) {
        let store = ctx.docstore();
        let plan = store
            .prepare(vec![Op::AddServer {
                project: spot.project.clone(),
                name: spot.name.clone(),
                entry: entry.clone(),
            }])
            .map_err(|e| e.to_string())?;
        store.apply(&plan).map_err(|e| e.to_string())?;
        return Ok(Vec::new());
    }
    let json = entry.to_string();
    let out = command(
        ctx,
        spot,
        "add-json",
        secret.map(|_| "--client-secret"),
        Some(&json),
        secret,
    )?;
    let failed = if out.timed_out {
        Some("Claude Code did not finish adding it in time.".to_owned())
    } else if out.exit_code != Some(0) {
        Some(format!("Claude Code said: {}", said(&out)))
    } else {
        None
    };
    match (read_entry(ctx.claude_json, spot)?, failed) {
        // It may have got there before the command failed or was stopped: what counts is the file.
        (Some(there), _) if same_settings(&there, entry) => Ok(left_out(&there, entry)),
        (Some(_), Some(why)) | (None, Some(why)) => Err(why),
        (Some(_), None) => {
            Err("Claude Code added it with other settings than Klefki gave it.".to_owned())
        }
        (None, None) => Err("Claude Code said it added it, but it is not there.".to_owned()),
    }
}

/// Whether what Claude Code wrote is what klefki gave it, apart from settings Claude Code dropped.
fn same_settings(there: &Value, given: &Value) -> bool {
    let (Some(there), Some(given)) = (there.as_object(), given.as_object()) else {
        return there == given;
    };
    there.iter().all(|(k, v)| given.get(k) == Some(v))
        && given
            .keys()
            .filter(|k| !there.contains_key(*k))
            // Claude Code writes no `type` for a stdio server.
            .all(|k| k != "type" || given.get(k) != Some(&json!("stdio")))
}

/// The keys of `given` Claude Code did not write.
fn left_out(there: &Value, given: &Value) -> Vec<String> {
    given
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| there.get(k.as_str()).is_none())
        .map(|(k, _)| sanitize(k))
        .collect()
}

/// A definition kept in klefki's private folder while its server is removed and added again.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recovery {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub project: Option<String>,
    pub entry: Value,
    /// Where the server was going, and as what: when that is in the file, the change got through (Klefki stopped
    /// before it could delete this file) and there is nothing to put back.
    pub next: Option<(Spot, Value)>,
}

/// A recovery file larger than this is not read.
const MAX_RECOVERY_BYTES: u64 = 4 * entry::MAX_ENTRY_BYTES as u64;
/// More recovery files than this are not listed.
const MAX_RECOVERIES: usize = 50;

impl Recovery {
    fn save(
        ctx: &Ctx,
        spot: &Spot,
        entry: &Value,
        next: Option<(&Spot, &Value)>,
    ) -> io::Result<Self> {
        let dir = ctx.recovery_dir();
        docstore::create_private_dir(&dir)?;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut body = json!({
            "name": spot.name,
            "project": spot.project,
            "entry": entry,
        });
        if let Some((to, new)) = next {
            body["next"] = json!({"name": to.name, "project": to.project, "entry": new});
        }
        let body = body.to_string();
        for n in 0u32.. {
            let id = format!("{ms:016}-{n}");
            let path = dir.join(format!("{id}.json"));
            match docstore::create_private_file(&path) {
                Ok(mut file) => {
                    file.write_all(body.as_bytes())?;
                    file.sync_all()?;
                    return Ok(Self {
                        id,
                        path,
                        name: spot.name.clone(),
                        project: spot.project.clone(),
                        entry: entry.clone(),
                        next: next.map(|(to, new)| (to.clone(), new.clone())),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        unreachable!("some file name is free")
    }

    /// The kept definition `id`, from the recovery folder next to `backup_dir`.
    pub(crate) fn find(backup_dir: &Path, id: &str) -> Result<Self, String> {
        Self::load(&recovery_dir(backup_dir), id)
    }

    fn load(dir: &Path, id: &str) -> Result<Self, String> {
        let gone = || "That kept definition is gone. Refresh and look again.".to_owned();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit() || c == '-') {
            return Err(gone());
        }
        let path = dir.join(format!("{id}.json"));
        let text = fsx::read_limited(&path, MAX_RECOVERY_BYTES)
            .map_err(|e| sanitize(&e.to_string()))?
            .ok_or_else(gone)?;
        let value: Value = serde_json::from_str(&text).map_err(|_| gone())?;
        let (spot, entry) = Self::spot_in(&value).ok_or_else(gone)?;
        // An unreadable `next` only loses the check that the change got through.
        let next = value.get("next").and_then(Self::spot_in);
        Ok(Self {
            id: id.to_owned(),
            path,
            name: spot.name,
            project: spot.project,
            entry,
            next,
        })
    }

    /// The `name`, `project` and `entry` of a record, checked like any untrusted input.
    fn spot_in(value: &Value) -> Option<(Spot, Value)> {
        let name = value.get("name").and_then(Value::as_str)?;
        ServerName::parse(name).ok()?;
        let project = match value.get("project") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) if inventory::is_safe_project_path(p) => Some(p.clone()),
            Some(_) => return None,
        };
        let entry = value.get("entry").filter(|e| e.is_object())?;
        Some((
            Spot {
                project,
                name: name.to_owned(),
            },
            entry.clone(),
        ))
    }

    /// Every kept definition, oldest first, as far as it can be read. One whose change got through after all (its
    /// new definition is in `claude_json` and its old one is not) is deleted instead: Klefki stopped between the add
    /// and deleting the file.
    pub(crate) fn list(backup_dir: &Path, claude_json: &Path) -> Vec<Self> {
        let root: Option<Value> = fsx::read_limited(claude_json, inventory::MAX_CLAUDE_JSON)
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok());
        Self::files(backup_dir)
            .into_iter()
            .filter(|r| {
                let done = root.as_ref().is_some_and(|root| r.got_through(root));
                if done {
                    r.discard();
                }
                !done
            })
            .collect()
    }

    /// Every recovery file, oldest first, as far as it can be read.
    pub(crate) fn files(backup_dir: &Path) -> Vec<Self> {
        let dir = recovery_dir(backup_dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".json")
                    .map(str::to_owned)
            })
            .collect();
        ids.sort();
        ids.into_iter()
            .take(MAX_RECOVERIES)
            .filter_map(|id| Self::load(&dir, &id).ok())
            .collect()
    }

    /// The change this record was kept for is in `root`: the new definition where it was going, and the old one
    /// gone from where it was (or replaced there by the new one).
    fn got_through(&self, root: &Value) -> bool {
        let Some((to, new)) = &self.next else {
            return false;
        };
        let from = Spot {
            project: self.project.clone(),
            name: self.name.clone(),
        };
        let arrived = entry_in(root, to).is_some_and(|there| same_settings(&there, new));
        let in_place = from.same_place(to) && from.name == to.name;
        arrived && (in_place || entry_in(root, &from).is_none())
    }

    fn discard(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
