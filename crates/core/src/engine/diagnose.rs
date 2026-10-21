//! Diagnose: one server run with Claude Code's debug output, and what it says explained.
use super::checks::{Job, check_env, interpret};
use super::claude_program;
use super::{Diagnosis, MAX_CHECK_OUTPUT, Shared, hint};
use crate::api::Status;
use crate::cli::runner::{self, RunSpec};
use crate::config::fsx;
use crate::diagnose::report;
use crate::sanitize::sanitize;

const MAX_DEBUG_FILE: u64 = 1024 * 1024;

/// Runs the check again with the client's own debug output and turns that into a safe, readable diagnosis.
/// The debug file lives in a private temporary folder that is removed when this function returns.
pub(super) fn run_diagnose(shared: &Shared, job: &Job) -> Diagnosis {
    let nothing = |note: String| Diagnosis {
        status: Status::Unknown,
        detail: None,
        hints: Vec::new(),
        server_said: Vec::new(),
        client_log: Vec::new(),
        note: Some(note),
    };
    let Some(program) = claude_program(shared) else {
        return nothing("the claude CLI is not available".to_owned());
    };
    let dir = match tempfile::Builder::new()
        .prefix("klefki-diagnose-")
        .tempdir()
    {
        Ok(dir) => dir,
        Err(e) => {
            return nothing(format!(
                "could not create a private temporary folder: {}",
                sanitize(&e.to_string())
            ));
        }
    };
    let file = dir.path().join("debug.log");
    let Some(file_arg) = file.to_str() else {
        return nothing("the temporary path is not valid text".to_owned());
    };
    let timeout = shared.config.check_timeout;
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
    let stderr = result
        .as_ref()
        .ok()
        .map(|o| o.stderr.clone())
        .unwrap_or_default();
    let timed_out = matches!(&result, Ok(o) if o.timed_out);
    let (status, detail) = interpret(result, timeout);

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
    let note = if timed_out { detail.clone() } else { read_note };
    let said = format!("{}\n{}", detail.as_deref().unwrap_or(""), stderr);
    let report = report::build(&debug_text, job.name.as_str(), &said);
    Diagnosis {
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
    }
}
