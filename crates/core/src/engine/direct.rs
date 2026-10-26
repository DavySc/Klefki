//! The first half of a check of a user or local stdio server (decision 015): Klefki starts the server itself, the way
//! Claude Code would, and says hello as Claude Code does (`mcp/stdio.rs`). Only "it connected" is taken from it.
//! Every other answer, and every server Klefki does not start itself (on Windows a `.cmd` or `.bat`), is checked by
//! Claude Code next, so a failure shown is always Claude Code's own word.
use super::checks::{Job, cached_cli_version, time_limit};
use super::{Shared, claude_program};
use crate::api::{LONGER_CHECK, Scope};
use crate::config::{inventory, server_env};
use crate::mcp::stdio::{self, Verdict};
use std::sync::atomic::Ordering;

/// What the direct check leaves to do.
pub(super) enum First {
    /// It connected: that is the answer.
    Connected,
    /// Claude Code checks it. `timed_out`: the server did not answer within Claude Code's time limit, said as a
    /// sentence (sanitised).
    AskClaude { timed_out: Option<String> },
}

/// Starts the job's server itself when it may, and says what is left to do.
pub(super) fn first(shared: &Shared, job: &Job) -> First {
    let ask = First::AskClaude { timed_out: None };
    if !shared.direct_checks.load(Ordering::Relaxed) || job.longer {
        return ask;
    }
    let Some(outcome) = start(shared, job) else {
        return ask;
    };
    match outcome.verdict {
        Verdict::Connected => First::Connected,
        Verdict::TimedOut => First::AskClaude {
            timed_out: Some(outcome.reason),
        },
        Verdict::Failed | Verdict::ToolsFailed => ask,
    }
}

/// The direct check's outcome, or `None` when Klefki does not start this server itself: it is not a user or local
/// stdio server, its definition changed since the job was made, or the program needs a shell or was not found.
fn start(shared: &Shared, job: &Job) -> Option<stdio::Outcome> {
    let paths = shared.config.paths.as_ref()?;
    // A local server's project is the folder it runs in, as the file spells it (`checks::where_to_run`).
    let project = match job.id.scope {
        Scope::User => None,
        Scope::Local => Some(job.cwd.to_str()?),
        _ => return None,
    };
    let (entry, trusted) = inventory::launch_entry(paths, project, job.name.as_str()).ok()?;
    if inventory::fingerprint(&entry) != job.definition {
        return None;
    }
    let settings = server_env::settings_env(paths, trusted.then_some(job.cwd.as_path()));
    let version = claude_program(shared).and_then(|program| cached_cli_version(shared, &program));
    let session_id = server_env::new_session_id();
    let launch = server_env::launch(&server_env::Inputs {
        entry: &entry,
        folder: &job.cwd,
        klefki_env: &shared.config.server_env,
        settings: &settings,
        claude_version: version.as_deref(),
        session_id: &session_id,
        os: server_env::Os::HERE,
    })
    .ok()?;
    // Never longer than a check given more time waits (`checks::wait_for`): the limit can come from a repository.
    let limit = time_limit(shared, job).limit.min(LONGER_CHECK);
    Some(stdio::check(
        &launch,
        limit,
        version.as_deref().unwrap_or("unknown"),
    ))
}
