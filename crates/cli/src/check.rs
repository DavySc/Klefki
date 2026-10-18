//! `status`: asks Claude Code about each server through core's engine, the same checks the app runs.
use crate::render::Checked;
use klefki_core::api::{self, Command, ConnectionId, Event, ServerId, Snapshot, Status};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Answers {
    pub snapshot: Arc<Snapshot>,
    pub cli_version: Option<String>,
    /// Final answers, by server. A server asked about but missing here did not answer in time.
    pub checked: HashMap<ConnectionId, Checked>,
}

/// The engine runs at most this many checks at once, each up to 30 s (`api::start_headless`); a check that has not
/// answered after its share of that and some slack is reported as not answering rather than waited on forever.
const CHECKS_AT_ONCE: u32 = 3;
const PER_CHECK: Duration = Duration::from_secs(35);

/// Starts the engine, waits for its snapshot, asks for every server `pick` chooses, and collects the answers.
/// `progress` is told how many of how many have answered. `Err` when the Claude CLI cannot be used.
pub fn run(
    claude: Option<PathBuf>,
    pick: impl FnOnce(&Snapshot) -> Vec<ServerId>,
    mut progress: impl FnMut(usize, usize),
) -> Result<Answers, String> {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let core = api::start_headless(claude, move || {
        let _ = wake_tx.send(());
    });
    let mut snapshot = None;
    // The engine reads the configuration, then looks for the CLI: one of these two comes after the snapshot.
    let cli_version = loop {
        let Some(event) = next(&core, &wake_rx, None) else {
            return Err("Klefki's engine stopped unexpectedly".to_owned());
        };
        match event {
            Event::Snapshot(s) => snapshot = Some(s),
            Event::CliVersion(v) if snapshot.is_some() => break v,
            Event::CliProblem(problem) => return Err(problem),
            _ => {}
        }
    };
    let snapshot = snapshot.unwrap_or_default();
    let wanted = pick(&snapshot);
    let total = wanted.len();
    for id in &wanted {
        core.send(Command::Check(id.clone()));
    }
    let mut waiting: HashSet<ConnectionId> =
        wanted.iter().map(|id| id.connection.clone()).collect();
    let mut checked = HashMap::new();
    let rounds = (total as u32).div_ceil(CHECKS_AT_ONCE) + 1;
    let deadline = Instant::now() + PER_CHECK * rounds;
    progress(0, total);
    while !waiting.is_empty() {
        let Some(event) = next(&core, &wake_rx, Some(deadline)) else {
            break;
        };
        if let Event::Status {
            id,
            status,
            detail,
            hint,
        } = event
            && status != Status::Checking
            && waiting.remove(&id.connection)
        {
            checked.insert(
                id.connection,
                Checked {
                    status,
                    detail,
                    hint,
                },
            );
            progress(checked.len(), total);
        }
    }
    Ok(Answers {
        snapshot,
        cli_version,
        checked,
    })
}

/// The engine's next event; blocks until one comes, the engine is gone, or `deadline` passes.
fn next(core: &api::Core, wake: &mpsc::Receiver<()>, deadline: Option<Instant>) -> Option<Event> {
    loop {
        if let Some(event) = core.try_recv() {
            return Some(event);
        }
        let woke = match deadline {
            None => wake.recv().map_err(|_| ()),
            Some(at) => wake
                .recv_timeout(at.saturating_duration_since(Instant::now()))
                .map_err(|_| ()),
        };
        // A wake may come before its event is readable, and one wake may cover several events: always look again.
        if woke.is_err() {
            return core.try_recv();
        }
    }
}
