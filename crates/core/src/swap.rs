//! Changes made by removing a server and adding it again, so that one signing in with OAuth can be edited, moved or
//! copied (decision 007). Claude Code has no command that changes a server, and klefki writes no server with `oauth`
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
use std::sync::Arc;
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
            end_leftovers: false,
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
        Some(p) => format!("in {}", sanitize(crate::words::folder_name(p))),
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
    /// What a copy changes from the original, after `edited`: its address, and how it signs in. A new client
    /// brings its own client secret instead of Klefki's copy of the original's.
    pub adjust: crate::adjust::Adjust,
}

impl Swap {
    /// The definition Claude Code is to add, from `old`, the one there now.
    fn new_entry(&self, old: &Value) -> Result<Value, String> {
        let edited = match &self.edited {
            Some(edited) => entry::unmasked(edited, old)?,
            None => old.clone(),
        };
        self.adjust.apply(&edited, &self.from.name)
    }

    /// The client secret handed to Claude Code with the add: the new client's, when the copy has one.
    fn new_client_secret(&self) -> Option<Option<&Secret>> {
        self.adjust.client.as_ref().map(|c| c.secret.as_ref())
    }

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
    /// Every backup of `.claude.json` made on the way, whether the change finished or not, for the activity log.
    pub backups: &'a std::cell::RefCell<Vec<PathBuf>>,
}

impl Ctx<'_> {
    fn recovery_dir(&self) -> PathBuf {
        recovery_dir(self.backup_dir)
    }

    fn kept(&self, backup: PathBuf) {
        self.backups.borrow_mut().push(backup);
    }

    fn kept_all(&self, backups: impl IntoIterator<Item = PathBuf>) {
        self.backups.borrow_mut().extend(backups);
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

/// The settings Claude Code's `add-json` writes; it drops any other (`alwaysAllow`, `cwd`, unknown keys). Checked
/// against 2.1.302 and 2.1.303. If a later version keeps more, only the preview is wrong: what Claude Code left out
/// is read back from the file after each command.
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
    let folder = sanitize(crate::words::folder_name(project));
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
    /// Good to know: settings Claude Code left out, a secret copy klefki could not move.
    pub notes: Vec<String>,
}

/// Runs `swap`. On failure says what happened and where everything is; the configuration then holds the old server
/// as it was (or, when even that could not be done, a recovery file holds it).
pub(crate) fn run(ctx: &Ctx, swap: &Swap) -> Result<Done, String> {
    // Each place as the file spells it, which is how the list names a server and Klefki files its secret.
    let respelled = |spot: &Spot| Spot {
        project: spot
            .project
            .as_deref()
            .map(|project| spelled_in(ctx.claude_json, project)),
        name: spot.name.clone(),
    };
    let swap = &Swap {
        from: respelled(&swap.from),
        to: respelled(&swap.to),
        ..swap.clone()
    };
    let verb = swap.verb();
    let shown = swap.from.words();
    let old = read_entry(ctx.claude_json, &swap.from)?.ok_or_else(|| {
        format!("{shown} is no longer in the configuration. Refresh and try again.")
    })?;
    let new = swap
        .new_entry(&old)
        .map_err(|why| format!("{shown} was not {verb}: {why}."))?;
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
    // Checked again here: a change in another Klefki may have kept a definition there since the review.
    if !replaces_itself && let Err(why) = free_of_kept(ctx.backup_dir, &swap.to) {
        return Err(format!("{shown} was not {verb}: {why}"));
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
    // A copy that signs in as another client takes that client's secret instead.
    let secret = if let Some(given) = swap.new_client_secret() {
        given.cloned()
    } else if signs_in(&old) || signs_in(&new) {
        let store = ctx.store.ok_or_else(|| {
            format!("{shown} was not {verb}: Klefki can't use your system's secret store.")
        })?;
        store
            .get(&swap.from.connection())
            .map_err(|e| format!("{shown} was not {verb}: {e}"))?
    } else {
        None
    };
    ctx.kept(
        ctx.docstore()
            .keep_copy()
            .map_err(|e| format!("{shown} was not {verb}: {e}"))?,
    );
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
    let mut done = Done::default();
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
                    "{shown} was not {verb}: {why}\nIt was put back, but Claude Code left out {}; Klefki keeps the whole definition, listed on the Connections page until you forget it.",
                    left_out.join(", ")
                )),
                Err(again) => {
                    // Claude Code's remove took the client secret of a server sharing this sign-in too; putting this
                    // one back hands it over again.
                    let also = if back.is_some() {
                        let others = read_root(ctx.claude_json)
                            .map(|root| sharing(&root, &swap.from, &old))
                            .unwrap_or_default();
                        if others.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "\n{} {} shares its sign-in, so it has no client secret in Claude Code until then.",
                                sanitize(&swap.from.name),
                                others.join(" and ")
                            )
                        }
                    } else {
                        String::new()
                    };
                    Err(format!(
                        "{shown} was not {verb}: {why}\nPutting it back failed too: {again}\nKlefki kept its definition{}, so you can put it back from the Connections page once Claude Code works again.{also}",
                        if back.is_some() {
                            " and its client secret"
                        } else {
                            ""
                        }
                    ))
                }
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
                        // Better none than a stale one, here or where it was: a server added there later would
                        // claim it. Claude Code has the secret.
                        let _ = store.delete(&to);
                        if swap.remove {
                            let _ = store.delete(&from);
                        }
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
    let new = match swap.new_entry(&old) {
        Ok(new) => new,
        // Said now, before anything runs: the copy's address or client does not fit.
        Err(why) if !swap.adjust.is_empty() => {
            return Err(format!(
                "{} cannot be copied that way: {why}.",
                sanitize(&swap.from.name)
            ));
        }
        Err(_) => swap.edited.clone().unwrap_or_else(|| old.clone()),
    };
    if !swap.adjust.is_empty() {
        let checked = if signs_in(&new) {
            entry::validate_new(&new)
        } else {
            entry::validate(&new)
        };
        checked.map_err(|why| {
            format!(
                "The copy of {} is not usable: {why}.",
                sanitize(&swap.from.name)
            )
        })?;
    }
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
        let sharing = sharing(root, &swap.from, &old);
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
            said.warnings.insert(
                0,
                format!(
                    "{name} {} shares this sign-in, so it is signed out too.",
                    sharing.join(" and ")
                ),
            );
        }
        // The costliest consequence, said first.
        said.warnings.insert(
            0,
            format!(
                "Claude Code removes {name} and adds it again, so you will need to sign in to it again afterwards."
            ),
        );
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
    if signs_in(&new) && !has_copy && swap.adjust.client.is_none() {
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
    // As edited (`<hidden>` stays hidden), with the copy's changes.
    let edited = swap.edited.clone().unwrap_or_else(|| old.clone());
    let new = as_claude_writes(
        &swap
            .adjust
            .apply(&edited, &swap.from.name)
            .unwrap_or(edited),
    );
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
    ctx.kept(
        ctx.docstore()
            .keep_copy()
            .map_err(|e| format!("{shown} was not put back: {e}"))?,
    );
    let left_out = add(ctx, &spot, &record.entry, secret.as_ref())
        .map_err(|why| format!("{shown} was not put back: {why}"))?;
    let mut done = Done::default();
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

/// Why no server may go to `spot` yet, if a kept definition waits to be put back there: Klefki's copy of its client
/// secret is filed under that place, and a server arriving there would replace or delete it (a copy, a move, an add
/// with a secret), or claim it later (its Auth tab, a move away). Put back would also find the place taken.
pub(crate) fn free_of_kept(backup_dir: &Path, spot: &Spot) -> Result<(), String> {
    let waiting = Recovery::files(backup_dir).into_iter().any(|r| {
        r.name == spot.name
            && spot.same_place(&Spot {
                project: r.project.clone(),
                name: r.name.clone(),
            })
    });
    if waiting {
        return Err(format!(
            "Klefki still keeps a definition of {} from an earlier change. Put it back or forget it on the Connections page first, or choose another name or place.",
            spot.words()
        ));
    }
    Ok(())
}

/// Deletes a recovery file the user no longer needs.
pub(crate) fn forget(backup_dir: &Path, claude_json: &Path, id: &str) -> Result<(), String> {
    let kept = Recovery::load(&recovery_dir(backup_dir), id)?;
    if let Some(why) = forget_problem(claude_json, &kept) {
        return Err(why);
    }
    kept.discard();
    Ok(())
}

/// Why `kept` must not be forgotten now, if so: a server elsewhere shares its sign-in (same name, address and
/// headers), and Claude Code's remove of the kept one took that server's client secret too. Only putting it back
/// hands the secret to Claude Code again; forgetting it would leave the other server without one. Not once the kept
/// server is back where it was: that add handed the secret over already.
pub(crate) fn forget_problem(claude_json: &Path, kept: &Recovery) -> Option<String> {
    if !signs_in(&kept.entry) {
        return None;
    }
    let root = match read_root(claude_json) {
        Ok(root) => root,
        Err(e) => {
            return Some(format!(
                "Klefki cannot read the configuration to check what shares its sign-in: {e}."
            ));
        }
    };
    let spot = Spot {
        project: kept.project.clone(),
        name: kept.name.clone(),
    };
    if entry_in(&root, &spot).is_some() {
        return None;
    }
    let others = sharing(&root, &spot, &kept.entry);
    (!others.is_empty()).then(|| {
        format!(
            "{} {} shares this definition's sign-in, so removing this one may also have removed that server's client secret in Claude Code. Put this one back instead: that gives both their client secret again.",
            sanitize(&kept.name),
            others.join(" and ")
        )
    })
}

pub(crate) fn signs_in(entry: &Value) -> bool {
    entry.get("oauth").is_some()
}

/// The definition at `spot`, read fresh from the file; `None` when there is none.
pub(crate) fn read_entry(claude_json: &Path, spot: &Spot) -> Result<Option<Value>, String> {
    Ok(entry_in(&read_root(claude_json)?, spot))
}

fn read_root(claude_json: &Path) -> Result<Value, String> {
    let text = fsx::read_limited(claude_json, inventory::MAX_CLAUDE_JSON)
        .map_err(|e| sanitize(&e.to_string()))?
        .unwrap_or_default();
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| {
        format!(
            "the configuration is not valid JSON ({})",
            sanitize(&e.to_string())
        )
    })
}

/// `project` as the configuration's key for it is spelled; as given when the file has none (or cannot be read).
pub(crate) fn spelled_in(claude_json: &Path, project: &str) -> String {
    let wanted = inventory::normalise_project(project);
    read_root(claude_json)
        .ok()
        .and_then(|root| {
            root.get("projects")?
                .as_object()?
                .keys()
                .find(|key| inventory::normalise_project(key) == wanted)
                .cloned()
        })
        .unwrap_or_else(|| project.to_owned())
}

/// Where else in `root` a server shares the sign-in of `entry` at `spot` (same name, address and headers): Claude
/// Code keeps one client secret and one sign-in for them, which removing one takes from both.
fn sharing(root: &Value, spot: &Spot, entry: &Value) -> Vec<String> {
    spots_in(root)
        .into_iter()
        .filter(|(other, e)| {
            other.name == spot.name
                && !other.same_place(spot)
                && signs_in(e)
                && sign_in_key(e) == sign_in_key(entry)
        })
        .map(|(other, _)| place(other.project.as_deref()))
        .collect()
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
    // From the folder as the file spells it: Claude Code keys a project by the folder it runs in, and wrote that key
    // itself, so another spelling of the same folder (`C:/…` for `C:\…`) could file the server under a second entry.
    let folder = spot
        .project
        .as_deref()
        .map(|project| spelled_in(ctx.claude_json, project));
    let cwd = folder.as_deref().map(Path::new).or(ctx.home);
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
        let applied = store.apply(&plan).map_err(|e| e.to_string())?;
        ctx.kept_all(applied.backup);
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
        let applied = store.apply(&plan).map_err(|e| e.to_string())?;
        ctx.kept_all(applied.backup);
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
    /// new definition is in `claude_json` and its old one is not) is deleted instead, with Klefki's copy of its
    /// client secret where it was, once that copy is also where the server went: Klefki stopped between the add and
    /// deleting the file, or a later change put the same server there. Without that copy there, the definition is
    /// still listed: putting it back is then the only way to hand its secret to Claude Code again.
    /// With each, whether its server is back where it was, without the settings Claude Code does not keep: the file
    /// then only holds the rest of the definition, until the user forgets it. `store` opens Klefki's secret store,
    /// only when a change got through: `None` when it cannot, and then nothing is dropped.
    pub(crate) fn list(
        backup_dir: &Path,
        claude_json: &Path,
        store: &dyn Fn() -> Option<Arc<dyn SecretStore>>,
    ) -> Vec<(Self, bool)> {
        let root: Option<Value> = fsx::read_limited(claude_json, inventory::MAX_CLAUDE_JSON)
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok());
        Self::files(backup_dir)
            .into_iter()
            .filter(|r| {
                let done = root.as_ref().is_some_and(|root| r.got_through(root))
                    && r.secret_went_along(claude_json, store);
                if done {
                    r.discard();
                }
                !done
            })
            .map(|r| {
                let back = root.as_ref().is_some_and(|root| r.is_back(root));
                (r, back)
            })
            .collect()
    }

    /// The connections whose client secret this kept definition may still need: where it was, and where it was
    /// going.
    pub(crate) fn connections(&self) -> Vec<ConnectionId> {
        let spot = Spot {
            project: self.project.clone(),
            name: self.name.clone(),
        };
        std::iter::once(spot.connection())
            .chain(self.next.iter().map(|(next, _)| next.connection()))
            .collect()
    }

    fn is_back(&self, root: &Value) -> bool {
        let spot = Spot {
            project: self.project.clone(),
            name: self.name.clone(),
        };
        entry_in(root, &spot).is_some_and(|there| there == as_claude_writes(&self.entry))
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

    /// Whether Klefki's copy of the client secret, if it holds one where the server was, is also where the server
    /// went (the one left behind is then deleted). Unsure counts as no, a store that cannot be opened included.
    fn secret_went_along(
        &self,
        claude_json: &Path,
        store: &dyn Fn() -> Option<Arc<dyn SecretStore>>,
    ) -> bool {
        let Some((to, _)) = &self.next else {
            return true;
        };
        let from = Spot {
            project: self.project.clone(),
            name: self.name.clone(),
        }
        .connection();
        let to = to.connection();
        if from == to {
            return true;
        }
        let Some(store) = store() else {
            return false;
        };
        match (store.get(&from), store.get(&to)) {
            (Ok(None), _) => true,
            // Read again just before: another Klefki may have put a server there since, with this secret.
            (Ok(Some(kept)), Ok(Some(there))) if kept == there => {
                read_root(claude_json).is_ok_and(|root| self.got_through(&root))
                    && store.delete(&from).is_ok()
            }
            _ => false,
        }
    }

    fn discard(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    /// A kept definition of `a` from the project `pb`, whose move to the user's `a` got through: the configuration
    /// has it there and not in pb. Returns the backup folder, the configuration, and where `a` was and went.
    fn got_through(dir: &Path) -> (PathBuf, PathBuf, Spot, Spot) {
        let pb = dir.join("pb").to_string_lossy().into_owned();
        let entry =
            json!({"type": "http", "url": "https://h.example/mcp", "oauth": {"clientId": "c"}});
        let claude_json = dir.join(".claude.json");
        std::fs::write(
            &claude_json,
            json!({"mcpServers": {"a": entry}, "projects": {&pb: {"mcpServers": {}}}}).to_string(),
        )
        .unwrap();
        let backup_dir = dir.join("data").join("backups");
        let recovery = recovery_dir(&backup_dir);
        std::fs::create_dir_all(&recovery).unwrap();
        std::fs::write(
            recovery.join("0000000000000001-0.json"),
            json!({"name": "a", "project": &pb, "entry": entry, "next": {"name": "a", "project": null, "entry": entry}})
                .to_string(),
        )
        .unwrap();
        let from = Spot {
            project: Some(pb),
            name: "a".into(),
        };
        let to = Spot {
            project: None,
            name: "a".into(),
        };
        (backup_dir, claude_json, from, to)
    }

    #[test]
    fn a_kept_definition_whose_change_got_through_stays_listed_while_the_secret_store_cannot_be_opened()
     {
        // The app opens the system's store when first needed, and the listing was given none: it then took the
        // change as done and dropped the definition, leaving Klefki's copy of its client secret under a server
        // that is gone.
        let dir = tempfile::tempdir().unwrap();
        let (backup_dir, claude_json, _, _) = got_through(dir.path());
        let listed = Recovery::list(&backup_dir, &claude_json, &|| None);
        assert_eq!(listed.len(), 1);
        assert_eq!(Recovery::files(&backup_dir).len(), 1);
    }

    #[test]
    fn a_kept_definition_whose_change_got_through_goes_with_its_secret_once_the_store_opens() {
        // The variant: the store opens, the copy is where the server went, and the one left behind is deleted.
        let dir = tempfile::tempdir().unwrap();
        let (backup_dir, claude_json, from, to) = got_through(dir.path());
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let secret = Secret::new("sec-a".to_owned());
        store.set(&from.connection(), &secret).unwrap();
        store.set(&to.connection(), &secret).unwrap();
        let open = || Some(Arc::clone(&store));
        assert!(Recovery::list(&backup_dir, &claude_json, &open).is_empty());
        assert!(Recovery::files(&backup_dir).is_empty());
        assert_eq!(store.get(&from.connection()).unwrap(), None);
        assert_eq!(store.get(&to.connection()).unwrap(), Some(secret));
    }

    // ---- found by cargo-mutants (docs/quality.md): what no test checked before

    use std::cell::RefCell;
    use std::sync::Mutex;

    fn plain(url: &str) -> Value {
        json!({"type": "http", "url": url})
    }

    fn oauth(url: &str) -> Value {
        json!({"type": "http", "url": url, "oauth": {"clientId": "c"}})
    }

    fn user(name: &str) -> Spot {
        Spot {
            project: None,
            name: name.into(),
        }
    }

    fn local(project: &str, name: &str) -> Spot {
        Spot {
            project: Some(project.into()),
            name: name.into(),
        }
    }

    fn swap(from: Spot, to: Spot, remove: bool) -> Swap {
        Swap {
            from,
            remove,
            to,
            edited: None,
            adjust: crate::adjust::Adjust::default(),
        }
    }

    /// What the fake Claude Code's `mcp add-json` does.
    #[derive(Clone)]
    enum Adds {
        /// Writes what it is given, without the settings it does not keep, and succeeds.
        AsClaude,
        /// Writes these settings instead, and succeeds.
        Other(Value),
        /// Writes nothing and ends with this code and error output.
        Fails(i32, &'static str),
    }

    /// Claude Code's `mcp remove` and `mcp add-json` on the configuration file, as far as these tests need them.
    struct FakeClaude {
        claude_json: PathBuf,
        adds: Mutex<Adds>,
        /// Each command's arguments, and whether a client secret came with it.
        runs: Mutex<Vec<(Vec<String>, Option<String>)>>,
    }

    impl Claude for FakeClaude {
        fn run(
            &self,
            args: &[&str],
            cwd: Option<&Path>,
            secret: Option<&Secret>,
        ) -> io::Result<RunOutput> {
            self.runs.lock().unwrap().push((
                args.iter().map(|a| (*a).to_owned()).collect(),
                secret.map(|s| s.expose().to_owned()),
            ));
            let ok = |exit_code, stderr: &str| RunOutput {
                stdout: String::new(),
                stderr: stderr.to_owned(),
                exit_code: Some(exit_code),
                timed_out: false,
                truncated: false,
            };
            let name = args[args.iter().position(|a| *a == "--").unwrap() + 1];
            let mut root = read_root(&self.claude_json).unwrap();
            let servers = if args[3] == "local" {
                let key = cwd.unwrap().to_string_lossy().into_owned();
                &mut root["projects"][key]["mcpServers"]
            } else {
                &mut root["mcpServers"]
            };
            match args[1] {
                "remove" => {
                    servers.as_object_mut().unwrap().remove(name);
                }
                _ => {
                    let given: Value = serde_json::from_str(args.last().unwrap()).unwrap();
                    let written = match self.adds.lock().unwrap().clone() {
                        Adds::AsClaude => as_claude_writes(&given),
                        Adds::Other(other) => other,
                        Adds::Fails(code, why) => return Ok(ok(code, why)),
                    };
                    servers[name] = written;
                }
            }
            std::fs::write(&self.claude_json, root.to_string())?;
            Ok(ok(0, ""))
        }
    }

    /// A configuration in a temporary folder, with a project folder `proj`, a fake Claude Code and a secret store.
    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
        claude_json: PathBuf,
        backup_dir: PathBuf,
        claude: FakeClaude,
        store: Arc<MemoryStore>,
        backups: RefCell<Vec<PathBuf>>,
    }

    impl World {
        /// `config(project)` is the configuration, given the project's folder as the file spells it.
        fn new(config: impl FnOnce(&str) -> Value) -> Self {
            let dir = tempfile::tempdir().unwrap();
            // The folder itself, not a link to it (macOS's temporary folder is one): Claude Code files a project
            // server under the real folder. Not on Windows, where that would be a `\\?\` path.
            let root = if cfg!(windows) {
                dir.path().to_owned()
            } else {
                std::fs::canonicalize(dir.path()).unwrap()
            };
            std::fs::create_dir_all(root.join("proj")).unwrap();
            let claude_json = root.join(".claude.json");
            let world = Self {
                claude: FakeClaude {
                    claude_json: claude_json.clone(),
                    adds: Mutex::new(Adds::AsClaude),
                    runs: Mutex::new(Vec::new()),
                },
                backup_dir: root.join("data").join("backups"),
                claude_json,
                root,
                _dir: dir,
                store: Arc::new(MemoryStore::default()),
                backups: RefCell::new(Vec::new()),
            };
            std::fs::write(&world.claude_json, config(&world.project()).to_string()).unwrap();
            world
        }

        fn project(&self) -> String {
            self.root.join("proj").to_string_lossy().into_owned()
        }

        fn run(&self, swap: &Swap) -> Result<Done, String> {
            let ctx = Ctx {
                claude_json: &self.claude_json,
                backup_dir: &self.backup_dir,
                claude: &self.claude,
                store: Some(&*self.store),
                home: Some(&self.root),
                progress: &|_| {},
                backups: &self.backups,
            };
            run(&ctx, swap)
        }

        fn config(&self) -> Value {
            read_root(&self.claude_json).unwrap()
        }

        /// The commands Claude Code was asked to run: `remove` or `add-json`, and the client secret given.
        fn commands(&self) -> Vec<(String, Option<String>)> {
            self.claude
                .runs
                .lock()
                .unwrap()
                .iter()
                .map(|(args, secret)| (args[1].clone(), secret.clone()))
                .collect()
        }

        /// A kept definition of `name` (`project`), on its way to `next`.
        fn keep(&self, id: &str, body: &Value) {
            let dir = recovery_dir(&self.backup_dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{id}.json")), body.to_string()).unwrap();
        }

        fn listed(&self) -> usize {
            let store: Arc<dyn SecretStore> = self.store.clone();
            Recovery::list(&self.backup_dir, &self.claude_json, &|| {
                Some(Arc::clone(&store))
            })
            .len()
        }
    }

    #[test]
    fn a_change_that_cannot_be_made_says_whether_it_was_a_save_a_move_or_a_copy() {
        let world = World::new(|_| json!({"mcpServers": {"a": plain("https://p.example/mcp")}}));
        let project = world.project();
        let not_usable = json!({"type": "http"});
        for (to, remove, verb) in [
            (user("a"), true, "saved"),
            (user("b"), true, "moved"),
            (local(&project, "a"), true, "moved"),
            (user("b"), false, "copied"),
            (local(&project, "a"), false, "copied"),
        ] {
            let mut change = swap(user("a"), to.clone(), remove);
            change.edited = Some(not_usable.clone());
            let err = world.run(&change).unwrap_err();
            assert!(
                err.starts_with(&format!(
                    "a for the user was not {verb}: the definition is not usable"
                )),
                "{to:?} {remove}: {err}"
            );
        }
        assert!(world.commands().is_empty());
    }

    #[test]
    fn a_server_is_never_copied_or_moved_onto_one_that_is_there() {
        let world = World::new(
            |_| json!({"mcpServers": {"a": plain("https://p.example/mcp"), "b": plain("https://q.example/mcp")}}),
        );
        let before = std::fs::read(&world.claude_json).unwrap();
        // A copy onto itself is not an edit.
        let err = world.run(&swap(user("a"), user("a"), false)).unwrap_err();
        assert_eq!(
            err,
            "a for the user was not copied: there is already a server called a for the user."
        );
        let err = world.run(&swap(user("a"), user("b"), true)).unwrap_err();
        assert_eq!(
            err,
            "a for the user was not moved: there is already a server called b for the user."
        );
        assert_eq!(std::fs::read(&world.claude_json).unwrap(), before);
    }

    #[test]
    fn a_plain_server_is_moved_by_klefki_with_a_backup_for_each_write() {
        let world = World::new(
            |project| json!({"mcpServers": {"a": plain("https://p.example/mcp")}, "projects": {project: {}}}),
        );
        let project = world.project();
        world
            .run(&swap(user("a"), local(&project, "a"), true))
            .unwrap();
        let config = world.config();
        assert_eq!(config["mcpServers"].get("a"), None);
        assert_eq!(
            config["projects"][&project]["mcpServers"]["a"],
            plain("https://p.example/mcp")
        );
        assert!(world.commands().is_empty(), "Claude Code is not needed");
        // The copy kept first, then the backups of the remove and of the add, for the activity log.
        assert_eq!(world.backups.borrow().len(), 3, "{:?}", world.backups);
    }

    #[test]
    fn a_plain_server_leaves_a_project_whose_folder_is_gone() {
        // Klefki writes it itself: only Claude Code's commands need the project's folder.
        let world = World::new(|project| {
            let gone = Path::new(project).with_file_name("gone");
            let gone = gone.to_string_lossy();
            json!({"projects": {gone: {"mcpServers": {"a": plain("https://p.example/mcp")}}}})
        });
        let gone = world.root.join("gone").to_string_lossy().into_owned();
        world
            .run(&swap(local(&gone, "a"), user("a"), true))
            .unwrap();
        assert_eq!(
            world.config()["mcpServers"]["a"],
            plain("https://p.example/mcp")
        );
        // One that signs in goes through Claude Code, which cannot run there.
        let world = World::new(|project| {
            let gone = Path::new(project).with_file_name("gone");
            let gone = gone.to_string_lossy();
            json!({"projects": {gone: {"mcpServers": {"a": oauth("https://o.example/mcp")}}}})
        });
        let gone = world.root.join("gone").to_string_lossy().into_owned();
        let err = world
            .run(&swap(local(&gone, "a"), user("a"), true))
            .unwrap_err();
        assert!(err.contains("the folder of gone is not there"), "{err}");
        assert!(err.ends_with("Nothing was changed."), "{err}");
    }

    #[test]
    fn settings_claude_code_leaves_out_are_noted_only_when_there_are_some() {
        let world = World::new(|_| json!({"mcpServers": {"a": oauth("https://o.example/mcp")}}));
        let project = world.project();
        let done = world
            .run(&swap(user("a"), local(&project, "a"), true))
            .unwrap();
        assert!(done.notes.is_empty(), "{:?}", done.notes);
        assert_eq!(
            world.commands(),
            [("remove".into(), None), ("add-json".into(), None)]
        );

        let mut entry = oauth("https://o.example/mcp");
        entry["alwaysAllow"] = json!(["read"]);
        let world = World::new(|_| json!({"mcpServers": {"a": entry}}));
        let project = world.project();
        let done = world
            .run(&swap(user("a"), local(&project, "a"), true))
            .unwrap();
        assert_eq!(
            done.notes,
            [
                "Claude Code left out settings it does not use: alwaysAllow. The copy Klefki kept of the configuration still has them."
            ]
        );
    }

    #[test]
    fn a_copy_claude_code_does_not_add_leaves_the_original_alone() {
        let world = World::new(|_| json!({"mcpServers": {"a": oauth("https://o.example/mcp")}}));
        *world.claude.adds.lock().unwrap() = Adds::Fails(1, "boom");
        let err = world.run(&swap(user("a"), user("b"), false)).unwrap_err();
        assert_eq!(
            err,
            "a for the user was not copied: Claude Code said: boom."
        );
        // Nothing was removed, so nothing is put back.
        assert_eq!(world.commands(), [("add-json".into(), None)]);
        assert_eq!(
            world.config()["mcpServers"]["a"],
            oauth("https://o.example/mcp")
        );
    }

    #[test]
    fn what_claude_code_wrote_counts_not_what_it_said() {
        let world = World::new(|_| json!({"mcpServers": {"a": oauth("https://o.example/mcp")}}));
        // Said yes, wrote nothing.
        *world.claude.adds.lock().unwrap() = Adds::Fails(0, "");
        let err = world.run(&swap(user("a"), user("b"), false)).unwrap_err();
        assert_eq!(
            err,
            "a for the user was not copied: Claude Code said it added it, but it is not there."
        );
        // Wrote something else.
        *world.claude.adds.lock().unwrap() = Adds::Other(oauth("https://elsewhere.example/mcp"));
        let err = world.run(&swap(user("a"), user("c"), false)).unwrap_err();
        assert_eq!(
            err,
            "a for the user was not copied: Claude Code added it with other settings than Klefki gave it."
        );
    }

    #[test]
    fn a_project_spelled_another_way_still_finds_its_client_secret() {
        // The change names the project with a trailing slash; the file and Klefki's secret store spell it without.
        let world = World::new(
            |project| json!({"projects": {project: {"mcpServers": {"a": oauth("https://o.example/mcp")}}}}),
        );
        let project = world.project();
        let secret = Secret::new("sec-a".to_owned());
        world
            .store
            .set(&local(&project, "a").connection(), &secret)
            .unwrap();
        world
            .run(&swap(local(&format!("{project}/"), "a"), user("a"), true))
            .unwrap();
        assert_eq!(
            world.commands(),
            [
                ("remove".into(), None),
                ("add-json".into(), Some("sec-a".into()))
            ]
        );
        assert_eq!(
            world.store.get(&user("a").connection()).unwrap(),
            Some(secret)
        );
        assert_eq!(
            world.store.get(&local(&project, "a").connection()).unwrap(),
            None
        );
    }

    #[test]
    fn what_claude_code_said_is_kept_short_and_ends_a_sentence() {
        let out = |stdout: &str, stderr: &str| RunOutput {
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            exit_code: Some(1),
            timed_out: false,
            truncated: false,
        };
        assert_eq!(said(&out("out", "  err  ")), "err.");
        assert_eq!(said(&out(" out ", " \n")), "out.");
        assert_eq!(said(&out("", "Not found!")), "Not found!");
        assert_eq!(said(&out("", "")), ".");
        let exactly = "x".repeat(200);
        assert_eq!(said(&out("", &exactly)), format!("{exactly}."));
        let long = "y".repeat(201);
        assert_eq!(said(&out("", &long)), format!("{}…", "y".repeat(200)));
    }

    #[test]
    fn claude_codes_own_settings_count_as_the_same() {
        let stdio = json!({"type": "stdio", "command": "x", "args": ["-y"]});
        assert!(same_settings(&stdio, &stdio));
        let http = oauth("https://o.example/mcp");
        assert!(same_settings(&http, &http));
        let mut more = http.clone();
        more["alwaysAllow"] = json!(["read"]);
        assert!(same_settings(&http, &more), "a setting left out");
        assert!(!same_settings(&more, &http), "a setting added");
        assert!(!same_settings(&oauth("https://other.example/mcp"), &http));
        assert!(same_settings(&json!("x"), &json!("x")));
        assert!(!same_settings(&json!("x"), &json!("y")));
    }

    // -- review

    #[test]
    fn a_copy_that_does_not_fit_is_refused_in_the_review_and_an_edit_waits_for_the_run() {
        use crate::adjust::{Adjust, Rebase};
        let root = json!({"mcpServers": {"a": oauth("https://o.example/mcp")}});
        let mut copy = swap(user("a"), user("b"), false);
        copy.adjust = Adjust {
            base: Some(Rebase {
                from: "https://other.example".into(),
                to: "https://x.example".into(),
            }),
            ..Adjust::default()
        };
        assert_eq!(
            review(&root, &copy, true),
            Err("a cannot be copied that way: the address of a has changed since; review the copy again.".into())
        );
        // An edit whose hidden value is gone, or that is not usable: the run says why.
        let mut edit = swap(user("a"), user("a"), true);
        edit.edited = Some(
            json!({"type": "http", "url": "https://o.example/mcp", "headers": {"X": entry::MASK}}),
        );
        assert!(review(&root, &edit, true).is_ok());
        edit.edited = Some(json!({"type": "http"}));
        assert!(review(&root, &edit, true).is_ok());
    }

    #[test]
    fn only_removing_a_server_that_signs_in_signs_it_out() {
        let root = json!({"mcpServers": {"a": oauth("https://o.example/mcp"), "p": plain("https://p.example/mcp")}});
        let said = review(&root, &swap(user("p"), user("q"), true), true).unwrap();
        assert_eq!(said, Said::default());
        let said = review(&root, &swap(user("a"), user("b"), false), true).unwrap();
        assert_eq!(said, Said::default());
        let said = review(&root, &swap(user("a"), user("b"), true), true).unwrap();
        assert_eq!(
            said.warnings,
            [
                "Claude Code removes a and adds it again, so you will need to sign in to it again afterwards."
            ]
        );
    }

    #[test]
    fn a_new_sign_in_that_another_server_of_that_name_has_is_refused() {
        let world = World::new(|_| json!({}));
        let project = world.project();
        let refused = |to: &str, place: &str| {
            Err(format!(
                "{to} {place} has the same name and address, and Claude Code keeps one sign-in for both, so its client secret would be replaced. Use another name or address."
            ))
        };
        // A copy under another name.
        let root = json!({
            "mcpServers": {"a": oauth("https://o.example/mcp")},
            "projects": {&project: {"mcpServers": {"b": oauth("https://o.example/mcp")}}},
        });
        let copy = swap(user("a"), user("b"), false);
        assert_eq!(review(&root, &copy, true), refused("b", "in proj"));
        // Another address there: nothing shared.
        let root = json!({
            "mcpServers": {"a": oauth("https://o.example/mcp")},
            "projects": {&project: {"mcpServers": {"b": oauth("https://other.example/mcp")}}},
        });
        assert!(review(&root, &copy, true).is_ok());

        // A server that starts signing in, where another of its name signs in at that address.
        let root = json!({
            "mcpServers": {"a": plain("https://o.example/mcp")},
            "projects": {&project: {"mcpServers": {"a": oauth("https://o.example/mcp")}}},
        });
        let mut edit = swap(user("a"), user("a"), true);
        edit.edited = Some(oauth("https://o.example/mcp"));
        assert_eq!(review(&root, &edit, true), refused("a", "in proj"));

        // A new address that another server of its name signs in at.
        let root = json!({
            "mcpServers": {"a": oauth("https://o.example/mcp")},
            "projects": {&project: {"mcpServers": {"a": oauth("https://new.example/mcp")}}},
        });
        edit.edited = Some(oauth("https://new.example/mcp"));
        assert_eq!(review(&root, &edit, true), refused("a", "in proj"));

        // Moved away under another name, where the server of that name signs in at its address.
        let root = json!({
            "mcpServers": {"a": oauth("https://o.example/mcp"), "b": oauth("https://o.example/mcp")},
            "projects": {&project: {"mcpServers": {}}},
        });
        let moved = swap(user("a"), local(&project, "b"), true);
        assert_eq!(review(&root, &moved, true), refused("b", "for the user"));
    }

    #[test]
    fn without_klefkis_copy_of_the_client_secret_the_review_says_what_happens_to_it() {
        let world = World::new(|_| json!({}));
        let project = world.project();
        let root = json!({
            "mcpServers": {"a": oauth("https://o.example/mcp"), "p": plain("https://p.example/mcp")},
            "projects": {&project: {"mcpServers": {}}},
        });
        let note = "Klefki has no copy of a's client secret, so Claude Code keeps none for b. If the server needs one, enter it on a's Auth tab first, then review again.";
        // A copy under another name gets none.
        let copy = swap(user("a"), user("b"), false);
        assert_eq!(review(&root, &copy, false).unwrap().notes, [note]);
        // With Klefki's copy, nothing to say.
        assert_eq!(review(&root, &copy, true).unwrap(), Said::default());
        // A server that does not sign in has no client secret.
        let copy = swap(user("p"), user("q"), false);
        assert_eq!(review(&root, &copy, false).unwrap(), Said::default());
        // The same name and address in another place: Claude Code shares the one it has.
        let copy = swap(user("a"), local(&project, "a"), false);
        assert_eq!(review(&root, &copy, false).unwrap(), Said::default());
        // A move removes it: a warning.
        let moved = swap(user("a"), local(&project, "a"), true);
        let said = review(&root, &moved, false).unwrap();
        assert!(said.notes.is_empty(), "{said:?}");
        assert!(
            said.warnings[1].starts_with(
                "Klefki has no copy of a's client secret. If it has one, Claude Code forgets it"
            ),
            "{said:?}"
        );
    }

    // -- putting a kept definition back

    #[test]
    fn put_back_warns_only_about_settings_claude_code_drops() {
        let kept = |entry: Value| Recovery {
            id: "1-0".into(),
            path: PathBuf::new(),
            name: "a".into(),
            project: None,
            entry,
            next: None,
        };
        let (_, warning) = preview_put_back(&kept(oauth("https://o.example/mcp")));
        assert_eq!(warning, None);
        let mut more = oauth("https://o.example/mcp");
        more["alwaysAllow"] = json!(["read"]);
        let (op, warning) = preview_put_back(&kept(more));
        assert_eq!(
            warning.as_deref(),
            Some(
                "Claude Code keeps only the settings it uses, so a loses alwaysAllow; Klefki keeps the whole definition until you forget it."
            )
        );
        assert_eq!(
            op,
            Op::AddServer {
                project: None,
                name: "a".into(),
                entry: oauth("https://o.example/mcp"),
            }
        );
    }

    #[test]
    fn putting_back_what_is_there_already_only_forgets_the_kept_copy() {
        let world = World::new(|_| json!({"mcpServers": {"a": plain("https://p.example/mcp")}}));
        world.keep(
            "0000000000000001-0",
            &json!({"name": "a", "project": null, "entry": plain("https://p.example/mcp")}),
        );
        let ctx = Ctx {
            claude_json: &world.claude_json,
            backup_dir: &world.backup_dir,
            claude: &world.claude,
            store: Some(&*world.store),
            home: Some(&world.root),
            progress: &|_| {},
            backups: &world.backups,
        };
        put_back(&ctx, "0000000000000001-0").unwrap();
        assert!(Recovery::files(&world.backup_dir).is_empty());
        // Something else there: refused, and kept.
        world.keep(
            "0000000000000002-0",
            &json!({"name": "a", "project": null, "entry": plain("https://other.example/mcp")}),
        );
        let err = put_back(&ctx, "0000000000000002-0").unwrap_err();
        assert!(
            err.contains("there is a server by that name there now"),
            "{err}"
        );
        assert_eq!(Recovery::files(&world.backup_dir).len(), 1);
        assert!(world.commands().is_empty());
    }

    #[test]
    fn a_kept_definition_is_read_up_to_four_times_the_largest_entry() {
        let world = World::new(|_| json!({}));
        let big = |bytes: usize| {
            let mut entry = plain("https://p.example/mcp");
            entry["env"] = json!({"PAD": "x".repeat(bytes)});
            json!({"name": "a", "project": null, "entry": entry})
        };
        // Just under the limit, with the rest of the record around it.
        world.keep("0000000000000001-0", &big(4 * entry::MAX_ENTRY_BYTES - 200));
        let size =
            std::fs::metadata(recovery_dir(&world.backup_dir).join("0000000000000001-0.json"))
                .unwrap()
                .len();
        assert!(size <= MAX_RECOVERY_BYTES, "{size}");
        assert!(size > 3 * entry::MAX_ENTRY_BYTES as u64, "{size}");
        assert!(Recovery::find(&world.backup_dir, "0000000000000001-0").is_ok());
        world.keep("0000000000000002-0", &big(4 * entry::MAX_ENTRY_BYTES));
        assert!(Recovery::find(&world.backup_dir, "0000000000000002-0").is_err());
    }

    #[test]
    fn a_kept_definition_with_a_project_that_is_not_a_local_folder_is_not_read() {
        let world = World::new(|_| json!({}));
        let project = world.project();
        for (id, at, readable) in [
            ("0000000000000001-0", json!(project), true),
            ("0000000000000002-0", json!("relative/folder"), false),
            ("0000000000000003-0", json!(7), false),
        ] {
            world.keep(
                id,
                &json!({"name": "a", "project": at, "entry": plain("https://p.example/mcp")}),
            );
            assert_eq!(
                Recovery::find(&world.backup_dir, id).is_ok(),
                readable,
                "{at}"
            );
        }
    }

    #[test]
    fn definitions_kept_at_once_each_get_their_own_file() {
        // Many in the same millisecond: each takes the next free number.
        let world = World::new(|_| json!({}));
        let ctx = Ctx {
            claude_json: &world.claude_json,
            backup_dir: &world.backup_dir,
            claude: &world.claude,
            store: Some(&*world.store),
            home: Some(&world.root),
            progress: &|_| {},
            backups: &world.backups,
        };
        let ids: std::collections::BTreeSet<String> = (0..40)
            .map(|_| {
                Recovery::save(&ctx, &user("a"), &plain("https://p.example/mcp"), None)
                    .unwrap()
                    .id
            })
            .collect();
        assert_eq!(ids.len(), 40);
        assert_eq!(Recovery::files(&world.backup_dir).len(), 40);
    }

    #[test]
    fn a_rename_got_through_only_once_the_old_name_is_gone() {
        let kept = json!({"name": "a", "project": null, "entry": oauth("https://o.example/mcp"),
            "next": {"name": "b", "project": null, "entry": oauth("https://o.example/mcp")}});
        // Both there: Klefki stopped between the add and the remove.
        let world = World::new(
            |_| json!({"mcpServers": {"a": oauth("https://o.example/mcp"), "b": oauth("https://o.example/mcp")}}),
        );
        world.keep("0000000000000001-0", &kept);
        assert_eq!(world.listed(), 1);
        // Only the new one there: done.
        let world = World::new(|_| json!({"mcpServers": {"b": oauth("https://o.example/mcp")}}));
        world.keep("0000000000000001-0", &kept);
        assert_eq!(world.listed(), 0);
    }

    #[test]
    fn a_kept_definition_whose_change_got_through_without_a_secret_of_its_own_goes() {
        let dir = tempfile::tempdir().unwrap();
        let (backup_dir, claude_json, _, _) = got_through(dir.path());
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let open = || Some(Arc::clone(&store));
        assert!(Recovery::list(&backup_dir, &claude_json, &open).is_empty());
    }

    #[test]
    fn a_kept_definition_stays_while_another_secret_is_where_its_server_went() {
        let dir = tempfile::tempdir().unwrap();
        let (backup_dir, claude_json, from, to) = got_through(dir.path());
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let kept = Secret::new("sec-a".to_owned());
        store.set(&from.connection(), &kept).unwrap();
        store
            .set(&to.connection(), &Secret::new("sec-other".to_owned()))
            .unwrap();
        let open = || Some(Arc::clone(&store));
        assert_eq!(Recovery::list(&backup_dir, &claude_json, &open).len(), 1);
        assert_eq!(store.get(&from.connection()).unwrap(), Some(kept));
    }
}
