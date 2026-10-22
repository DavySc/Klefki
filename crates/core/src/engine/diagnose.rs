//! Diagnose: one server run with Claude Code's debug output, and what it says explained.
use super::checks::{Job, check_env, interpret, run_timeout, time_limit, timing};
use super::claude_program;
use super::{Diagnosis, MAX_CHECK_OUTPUT, Shared, hint};
use crate::api::Scope;
use crate::api::{Status, Timing};
use crate::cli::runner::{self, RunSpec};
use crate::config::fsx;
use crate::config::inventory;
use crate::diagnose::redact::scrubbed;
use crate::diagnose::report;
use crate::sanitize::sanitize;

const MAX_DEBUG_FILE: u64 = 1024 * 1024;

/// Runs the check again with the client's own debug output and turns that into a safe, readable diagnosis.
/// The debug file lives in a private temporary folder that is removed when this function returns. Comes with a timing
/// when the server ran out of time.
pub(super) fn run_diagnose(shared: &Shared, job: &Job) -> (Diagnosis, Option<Timing>) {
    let nothing = |note: String| Diagnosis {
        status: Status::Unknown,
        detail: None,
        hints: Vec::new(),
        server_said: Vec::new(),
        client_log: Vec::new(),
        note: Some(note),
    };
    let Some(program) = claude_program(shared) else {
        return (nothing("the claude CLI is not available".to_owned()), None);
    };
    let dir = match tempfile::Builder::new()
        .prefix("klefki-diagnose-")
        .tempdir()
    {
        Ok(dir) => dir,
        Err(e) => {
            let note = format!(
                "could not create a private temporary folder: {}",
                sanitize(&e.to_string())
            );
            return (nothing(note), None);
        }
    };
    let file = dir.path().join("debug.log");
    let Some(file_arg) = file.to_str() else {
        return (
            nothing("the temporary path is not valid text".to_owned()),
            None,
        );
    };
    let limit = time_limit(shared, job);
    let timeout = run_timeout(shared, &limit, false);
    let started = std::time::Instant::now();
    let result = runner::run(&RunSpec {
        program: &program,
        args: &[
            "--debug",
            "mcp",
            "--debug-file",
            file_arg,
            "mcp",
            "get",
            job.name.as_str(),
        ],
        cwd: Some(&job.cwd),
        env: &check_env(shared),
        timeout,
        max_output: MAX_CHECK_OUTPUT,
        end_leftovers: true,
    });
    let hidden = hidden_values_of(shared, &job.id);
    let stderr = result
        .as_ref()
        .ok()
        .map(|o| scrubbed(&o.stderr, &hidden))
        .unwrap_or_default();
    let took = started.elapsed();
    let timed_out = matches!(&result, Ok(o) if o.timed_out);
    let (status, detail) = interpret(result, timeout);
    let detail = detail.map(|d| scrubbed(&d, &hidden));
    let timing = timing(false, status, detail.as_deref(), timed_out, took, limit);

    let (debug_text, read_note) = match fsx::read_limited(&file, MAX_DEBUG_FILE) {
        Ok(Some(text)) => (text, None),
        Ok(None) => (
            String::new(),
            Some("the client wrote no debug output".to_owned()),
        ),
        Err(e) => (
            String::new(),
            Some(format!(
                "could not read the debug output: {}",
                sanitize(&e.to_string())
            )),
        ),
    };
    let debug_text = scrubbed(&debug_text, &hidden);
    let note = if timed_out { detail.clone() } else { read_note };
    let said = format!("{}\n{}", detail.as_deref().unwrap_or(""), stderr);
    let report = report::build(&debug_text, job.name.as_str(), &said);
    let diagnosis = Diagnosis {
        status,
        detail,
        hints: report
            .hints
            .iter()
            .map(|&kind| hint(kind, &format!("{said}\n{debug_text}")))
            .collect(),
        server_said: report.server_said,
        client_log: report.client_log,
        note,
    };
    (diagnosis, timing)
}

/// What the server's definition holds that must not show in text about it: its environment and header values, as
/// written and filled in, and the arguments and address parts an export replaces. Nothing when it cannot be read.
pub(super) fn hidden_values_of(shared: &Shared, id: &crate::api::ServerId) -> Vec<String> {
    let (Some(paths), Some((scope, project, name))) = (
        &shared.config.paths,
        crate::model::ConnectionId::parse(id.connection.as_str()),
    ) else {
        return Vec::new();
    };
    // The project and name as the file spells them, which the connection holds.
    inventory::server_entry(paths, project.as_deref(), &name, scope == Scope::Project)
        .map(|entry| crate::config::entry::hidden_values(&entry, &*shared.config.vars))
        .unwrap_or_default()
}
