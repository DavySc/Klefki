//! `profiles` and `use` (decision 012): the profiles Klefki keeps, and using one in a project. `use` goes through the
//! same plan as the app: the change is shown first, with a backup kept before it is saved.
use crate::args::Place;
use klefki_core::api::{self, Change, Command, Event, PlanView, Profiles, Snapshot};
use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long the engine may take to make the plan, and to save it.
const PREPARE: Duration = Duration::from_secs(30);
const APPLY: Duration = Duration::from_secs(60);

/// What `use` ended with.
pub enum Used {
    Saved,
    /// The files already look like this.
    Nothing,
    /// Not confirmed: nothing was saved.
    NotSaved,
    /// The plan could not be made or saved.
    Failed(String),
}

/// The profile named `typed`: its own name, case aside. Otherwise a sentence that suggests the closest name.
pub fn find<'a>(profiles: &'a Profiles, typed: &str) -> Result<&'a api::Profile, String> {
    if let Some(p) = profiles
        .profiles
        .iter()
        .find(|p| p.name.to_lowercase() == typed.to_lowercase())
    {
        return Ok(p);
    }
    let names = profiles.profiles.iter().map(|p| p.name.as_str());
    Err(match crate::suggest::closest(typed, names) {
        Some(meant) => format!("no profile is named {typed:?}; did you mean {meant:?}?"),
        None if profiles.profiles.is_empty() => {
            format!(
                "no profile is named {typed:?}: there are none yet; make one on Klefki's Profiles page"
            )
        }
        None => format!("no profile is named {typed:?}; `klefki-cli profiles` lists them"),
    })
}

/// The project `place` names, as the configuration spells it. A folder is matched as given, then with its links
/// resolved (a temporary folder on macOS is reached through one). A folder inside a git repository that is not a
/// project itself is the repository's: Claude Code takes the repository's root as the project there.
pub fn project(snapshot: &Snapshot, place: &Place) -> Result<Option<String>, String> {
    let folder = match place {
        Place::Every => return Ok(None),
        Place::Project(path) => path.clone(),
        Place::Here => std::env::current_dir().map_err(|e| {
            format!("the current folder cannot be read ({e}); name one with --project")
        })?,
    };
    if let Some(found) = known(snapshot, &folder) {
        return Ok(Some(found));
    }
    if let Some(root) = git_root(&folder).filter(|root| *root != folder)
        && let Some(found) = known(snapshot, &root)
    {
        return Ok(Some(found));
    }
    let given = folder.to_string_lossy().replace('\\', "/");
    Err(format!(
        "Claude Code does not know {given} as a project: start Claude Code there once, or name one with --project \
         (`klefki-cli list` shows the projects)"
    ))
}

/// The known project that is `folder`, as the configuration spells it.
fn known(snapshot: &Snapshot, folder: &std::path::Path) -> Option<String> {
    let given = folder.to_string_lossy().replace('\\', "/");
    if let Some(p) = snapshot
        .projects
        .iter()
        .find(|p| api::same_project(p, &given))
    {
        return Some(p.clone());
    }
    let real = |path: &std::path::Path| {
        std::fs::canonicalize(path)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    };
    let real_given = real(folder)?;
    snapshot
        .projects
        .iter()
        .find(|p| {
            api::same_project(p, &real_given)
                || real(std::path::Path::new(p)).is_some_and(|r| api::same_project(&r, &real_given))
        })
        .cloned()
}

/// The root of the git repository `folder` is in: the nearest folder, itself or above, that holds `.git` (a folder,
/// or a file in a worktree).
fn git_root(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    folder
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(std::path::Path::to_path_buf)
}

/// Uses `profile` in `project` (every project when `None`): shows the plan on stdout, asks at a terminal unless
/// `yes`, and saves it.
pub fn use_profile(profile: &api::Profile, project: Option<String>, only: bool, yes: bool) -> Used {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    // The engine the app uses, without background checks or watching: only what is asked for here runs.
    let core = api::start_headless(None, move || {
        let _ = wake_tx.send(());
    });
    core.send(Command::Prepare(vec![Change::UseProfile {
        name: profile.name.clone(),
        members: profile.members.clone(),
        project,
        only,
    }]));
    let deadline = Instant::now() + PREPARE;
    let plan = loop {
        match next(&core, &wake_rx, deadline) {
            Some(Event::PlanReady(plan)) => break plan,
            Some(Event::PlanFailed(why)) => return Used::Failed(why),
            Some(_) => {}
            None => return Used::Failed("Klefki's engine did not answer".to_owned()),
        }
    };
    print!("{}", shown(&plan));
    if plan.nothing_to_do {
        println!("Nothing to change: the configuration already looks like this.");
        return Used::Nothing;
    }
    // The question names what is decided: after a long list of changes, the summary is out of view.
    if !yes && !confirmed(&plan.summary.join("; ")) {
        return Used::NotSaved;
    }
    core.send(Command::Apply(plan.id));
    let deadline = Instant::now() + APPLY;
    loop {
        match next(&core, &wake_rx, deadline) {
            Some(Event::Applied {
                plan: id,
                backup,
                notes,
                ..
            }) if id == plan.id => {
                for note in notes {
                    eprintln!("note: {note}");
                }
                match backup {
                    Some(backup) => {
                        eprintln!("Saved. The files as they were are kept in {backup}.")
                    }
                    None => eprintln!("Saved."),
                }
                return Used::Saved;
            }
            Some(Event::ApplyFailed { plan: id, error }) if id == plan.id => {
                return Used::Failed(format!("nothing was saved: {error}"));
            }
            Some(_) => {}
            None => {
                return Used::Failed(
                    "Klefki's engine did not say whether the change was saved; look at the Activity log".to_owned(),
                );
            }
        }
    }
}

/// The plan as the app's review shows it: what it does, what to know, then each file's changes.
fn shown(plan: &PlanView) -> String {
    let mut out = String::new();
    for line in &plan.summary {
        out.push_str(line);
        out.push('\n');
    }
    if !plan.warnings.is_empty() || !plan.notes.is_empty() {
        out.push('\n');
    }
    for warning in &plan.warnings {
        out.push_str(&format!("warning: {warning}\n"));
    }
    for note in &plan.notes {
        out.push_str(&format!("note: {note}\n"));
    }
    for file in &plan.files {
        out.push('\n');
        out.push_str(&file.path);
        out.push('\n');
        for line in &file.lines {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Asks on the terminal whether to do `what`; no when nobody is there to answer, and says how to save without asking.
fn confirmed(what: &str) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!(
            "Nothing was saved: no one at a terminal to ask. Add --yes to save without asking."
        );
        return false;
    }
    eprint!("\n{what}? [y/N] ");
    let mut answer = String::new();
    let yes = std::io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes");
    if !yes {
        eprintln!("Nothing was saved.");
    }
    yes
}

/// The engine's next event; `None` once it is gone or `deadline` passes.
fn next(core: &api::Core, wake: &mpsc::Receiver<()>, deadline: Instant) -> Option<Event> {
    loop {
        if let Some(event) = core.try_recv() {
            return Some(event);
        }
        // A wake may come before its event is readable, and one wake may cover several events: always look again.
        if wake
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err()
        {
            return core.try_recv();
        }
    }
}
