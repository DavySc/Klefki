//! The Status tab's "Copy debug report": what Klefki knows about the server in one text, for a bug report or for
//! whoever is helping: versions, status, what keeps it from starting, the definition with every value that may be a
//! secret replaced, what the server said and how Claude Code's own starts went. Only copied; nothing is sent.

use super::detail::diagnosis_details;
use super::export::{COPIED_SECS, announce};
use super::last_starts::starts_text;
use super::list::preflight_shown;
use super::{DetailTab, ShellApp};
use crate::explain::Explanation;
use crate::theme;
use egui::{RichText, Ui};
use klefki_core::api::{Command, Diagnosis, Exported, LastStarts, ServerId, ServerRow, Status};

pub(super) const HEADING: &str = "Debug report";
pub(super) const ABOUT: &str = "Everything on this tab and the server's definition as one text, for a bug report or someone who is helping you.";
pub(super) const COPY: &str = "Copy debug report";
const COPY_HOVER: &str = "Copy this server's status, definition and logs as plain text. Environment and header values \
     and anything that looks like a secret are replaced or hidden, but check it before you share it.";

/// A debug report asked for: waiting for the definition, then copied.
pub(super) struct DebugSeen {
    id: ServerId,
    /// Still to be copied when the definition arrives.
    copy: bool,
    /// The text to copy the next time the section is drawn.
    ready: Option<String>,
    /// When it was copied, in egui time.
    copied: Option<f64>,
}

/// What a report is made of. The engine already sanitised and redacted all of it.
pub(super) struct Facts<'a> {
    pub klefki: &'a str,
    pub claude_code: Option<&'a str>,
    pub system: &'a str,
    pub row: &'a ServerRow,
    pub status: Status,
    pub detail: Option<&'a str>,
    /// The status is what Claude Code's cache said, not an answer of this session's own checks.
    pub last_known: bool,
    pub explanation: Option<&'a Explanation>,
    pub diagnosis: Option<&'a Diagnosis>,
    pub starts: Option<&'a LastStarts>,
    pub definition: &'a Result<Exported, String>,
}

/// The report as plain text, without markup or control characters.
pub(super) fn report(f: &Facts<'_>) -> String {
    use std::fmt::Write;
    let row = f.row;
    let mut out = String::from("Klefki debug report\n");
    let _ = writeln!(
        out,
        "Klefki {}, Claude Code {}, {}",
        f.klefki,
        f.claude_code.unwrap_or("version unknown"),
        f.system
    );
    let place = match &row.project {
        Some(project) => format!("{}, {project}", row.scope.label()),
        None => row.scope.label().to_owned(),
    };
    let _ = writeln!(out, "\nserver: {} ({place})", row.name);
    let _ = writeln!(out, "transport: {}", row.transport.label());
    let known = if f.last_known { " (last known)" } else { "" };
    let _ = match f.detail {
        Some(detail) => writeln!(out, "status: {}{known}: {detail}", f.status.label()),
        None => writeln!(out, "status: {}{known}", f.status.label()),
    };
    if let Some(explanation) = f.explanation {
        out.push('\n');
        out.push_str(&explanation.text());
    }
    let preflight = preflight_shown(row, f.status);
    if !preflight.is_empty() {
        out.push_str("\nbefore it can start:\n");
        for found in preflight {
            let _ = writeln!(out, "  {}\n  {}", found.problem, found.fix);
        }
    }
    match f.definition {
        Ok(found) => {
            out.push_str(
                "\ndefinition (values that may be secrets are replaced by ${VAR} references):\n",
            );
            out.push_str(&found.text);
            out.push('\n');
            for variable in &found.variables {
                let _ = write!(
                    out,
                    "  ${{{}}}: {}",
                    variable.name,
                    variable.replaces.join(", ")
                );
                if let Some(default) = &variable.default {
                    let _ = write!(out, " (a setting, kept as the default: {default})");
                }
                out.push('\n');
            }
            for note in &found.notes {
                let _ = writeln!(out, "  {note}");
            }
        }
        Err(why) => {
            let _ = writeln!(out, "\ndefinition: not shown, {why}");
        }
    }
    if let Some(d) = f.diagnosis {
        out.push_str("\ndiagnosis:\n");
        diagnosis_details(&mut out, row, d);
    }
    if let Some(last) = f.starts {
        out.push_str("\nlast starts by Claude Code:\n");
        if let Some(problem) = &last.problem {
            let _ = writeln!(out, "  {problem}");
        }
        if last.starts.is_empty() && last.problem.is_none() {
            out.push_str("  none in Claude Code's logs\n");
        }
        starts_text(&mut out, row, last);
    }
    out
}

impl ShellApp {
    /// Asks for the definition; the report is copied when it arrives.
    pub(super) fn copy_debug_report(&mut self, id: ServerId) {
        match &mut self.debug_report {
            Some(seen) if seen.id == id => {
                seen.copy = true;
                seen.ready = None;
            }
            _ => {
                self.debug_report = Some(DebugSeen {
                    id: id.clone(),
                    copy: true,
                    ready: None,
                    copied: None,
                });
            }
        }
        self.send(Command::RedactedDefinition(id));
    }

    pub(super) fn redacted_definition_arrived(
        &mut self,
        id: &ServerId,
        definition: &Result<Exported, String>,
    ) {
        // Not copied after the user went elsewhere: the clipboard is not theirs to have changed then.
        let here =
            self.tab == DetailTab::Status && self.single_selected().is_some_and(|s| s.is(id));
        if !self
            .debug_report
            .as_ref()
            .is_some_and(|seen| seen.id == *id && seen.copy && here)
        {
            return;
        }
        let snapshot = std::sync::Arc::clone(&self.snapshot);
        let Some(i) = snapshot.servers.iter().position(|s| s.is(id)) else {
            return;
        };
        let (status, detail) = self.effective(i);
        let explanation = Explanation::for_status_timed(
            &snapshot.servers[i],
            status,
            detail,
            self.hint(i),
            self.timing(i),
        );
        let text = report(&Facts {
            klefki: env!("CARGO_PKG_VERSION"),
            claude_code: self.shown_cli_version(),
            system: &format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            row: &snapshot.servers[i],
            status,
            detail,
            last_known: self.last_known(i),
            explanation: explanation.as_ref(),
            diagnosis: self.diagnoses.get(id),
            starts: self
                .last_starts
                .get(id)
                .and_then(|seen| seen.starts.as_ref()),
            definition,
        });
        if let Some(seen) = self.debug_report.as_mut() {
            seen.copy = false;
            seen.ready = Some(text);
        }
    }

    /// The last section of the Status tab: what the report holds, the button, and "Copied" after it.
    pub(super) fn debug_report_section(&mut self, ui: &mut Ui, s: &ServerRow) {
        if self.core.is_none() {
            return;
        }
        let pal = theme::palette(ui.ctx());
        ui.add_space(10.0);
        ui.separator();
        ui.label(
            RichText::new(HEADING)
                .family(theme::bold_family())
                .size(16.0),
        );
        ui.add(egui::Label::new(RichText::new(ABOUT).color(pal.text_muted)).wrap());
        ui.add_space(4.0);
        let now = ui.input(|i| i.time);
        if let Some(seen) = self.debug_report.as_mut().filter(|seen| seen.id == s.id())
            && let Some(text) = seen.ready.take()
        {
            ui.ctx().copy_text(text);
            seen.copied = Some(now);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS));
        }
        let copied = self
            .debug_report
            .as_ref()
            .filter(|seen| seen.id == s.id())
            .and_then(|seen| seen.copied)
            .is_some_and(|at| now - at < COPIED_SECS);
        let button = ui
            .button(if copied { "Copied" } else { COPY })
            .on_hover_text(COPY_HOVER);
        announce(ui.ctx(), button.id);
        if button.clicked() {
            self.copy_debug_report(s.id());
        }
    }
}
