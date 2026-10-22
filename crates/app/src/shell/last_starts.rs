//! The Status tab's "Last starts by Claude Code": what Claude Code's own logs say about the last times it started the
//! server (decision 013), the newest in each folder. The engine read, sanitised and redacted it.

use crate::explain::Explanation;
use crate::theme;
use egui::{RichText, Ui};
use klefki_core::api::{LastStart, LastStarts, ServerRow, StartOutcome, Status, words};

pub(super) const HEADING: &str = "Last starts by Claude Code";
/// Said under the heading, not only in a hover: it decides how a start in the home folder reads.
pub(super) const ABOUT: &str = "Newest first, one per folder. Klefki's own checks are listed too (in ~ for user servers, unless diagnosed in \
     a project).";
/// Claude Code removes its logs after 30 days unless `cleanupPeriodDays` says otherwise.
const NONE: &str =
    "Claude Code has no log of starting it (it keeps logs for 30 days unless set otherwise)";
pub(super) const COPY: &str = "Copy last starts";
const COPY_HOVER: &str =
    "Copy these starts as plain text. Known secrets are hidden; check before sharing.";

/// "connected in 62 ms", "failed (CONNECTION_CLOSED)", as a start's line says it.
fn outcome_words(outcome: &StartOutcome) -> String {
    match outcome {
        StartOutcome::Connected { after: Some(after) } => {
            format!("connected in {}", words::duration(*after))
        }
        StartOutcome::Connected { after: None } => "connected".to_owned(),
        StartOutcome::Failed {
            code: Some(code), ..
        } => format!("failed ({code})"),
        StartOutcome::Failed { code: None, .. } => "failed".to_owned(),
        StartOutcome::Unknown => "no result in the log".to_owned(),
    }
}

/// One start in a line: "Today 07:32 in ~/proj: failed (CONNECTION_CLOSED)".
fn headline(start: &LastStart) -> String {
    format!(
        "{} in {}: {}",
        start.when,
        start.folder,
        outcome_words(&start.outcome)
    )
}

/// Claude Code's message for a failed start, with how long it took.
fn failure(outcome: &StartOutcome) -> Option<String> {
    let StartOutcome::Failed { said, after, .. } = outcome else {
        return None;
    };
    Some(match after {
        Some(after) => format!(
            "Claude Code said: {said} (after {})",
            words::duration(*after)
        ),
        None => format!("Claude Code said: {said}"),
    })
}

/// Which start opens first: the newest that failed, or else the newest.
fn opened(starts: &[LastStart]) -> usize {
    starts
        .iter()
        .position(|s| matches!(s.outcome, StartOutcome::Failed { .. }))
        .unwrap_or(0)
}

/// Plain text to paste into a support request. Already sanitised and redacted by the engine.
pub(super) fn report(row: &ServerRow, last: &LastStarts) -> String {
    let mut out = format!("Last starts of {} by Claude Code\n", row.name);
    starts_text(&mut out, row, last);
    out
}

/// Each start as plain text: when, where, how it went, the likely causes, what the server printed and the log file.
pub(super) fn starts_text(out: &mut String, row: &ServerRow, last: &LastStarts) {
    use std::fmt::Write;
    for start in &last.starts {
        let _ = writeln!(out, "\n{}", headline(start));
        if !start.exact.is_empty() {
            let _ = writeln!(out, "  at {}", start.exact);
        }
        if let Some(failure) = failure(&start.outcome) {
            let _ = writeln!(out, "  {failure}");
        }
        for hint in &start.hints {
            out.push('\n');
            out.push_str(
                &Explanation::for_hint(row, Status::Failed, hint, said(&start.outcome)).text(),
            );
        }
        if !start.server_said.is_empty() {
            out.push_str("  the server said:\n");
            for line in &start.server_said {
                let _ = writeln!(out, "    {line}");
            }
        }
        let _ = writeln!(out, "  log: {}", start.file);
    }
}

fn said(outcome: &StartOutcome) -> Option<&str> {
    match outcome {
        StartOutcome::Failed { said, .. } => Some(said),
        _ => None,
    }
}

/// The section, under the status and its explanation, once the engine has read the logs: that takes milliseconds,
/// so nothing is drawn meanwhile rather than a line that would flash.
pub(super) fn panel(ui: &mut Ui, row: &ServerRow, last: &LastStarts) {
    let pal = theme::palette(ui.ctx());
    let muted = |ui: &mut Ui, text: &str| {
        ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
    };
    ui.add_space(10.0);
    ui.separator();
    ui.label(
        RichText::new(HEADING)
            .family(theme::bold_family())
            .size(16.0),
    );
    muted(ui, ABOUT);
    if let Some(problem) = &last.problem {
        muted(ui, problem);
    }
    if !last.shares_log_with.is_empty() {
        muted(
            ui,
            &format!(
                "Claude Code logs {} in the same place, so a start below may be {}.",
                words::and_list(&last.shares_log_with),
                if last.shares_log_with.len() == 1 {
                    "its"
                } else {
                    "theirs"
                }
            ),
        );
    }
    if last.starts.is_empty() {
        if last.problem.is_none() {
            let place = last
                .looked_in
                .as_deref()
                .map_or(String::new(), |dir| format!(" ({dir})"));
            muted(ui, &format!("{NONE}{place}."));
        }
        return;
    }
    let open = opened(&last.starts);
    for (i, start) in last.starts.iter().enumerate() {
        let color = match start.outcome {
            StartOutcome::Failed { .. } => pal.danger,
            _ => pal.text,
        };
        // A header that wraps: the folder can be long, and egui's own collapsing header would widen the panel.
        let id = ui.make_persistent_id(("last start", i));
        let line = headline(start);
        let (toggle, header, _) = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id,
            i == open,
        )
        .show_header(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(&line).color(color))
                    .wrap()
                    .sense(egui::Sense::click()),
            )
        })
        .body(|ui| {
            if let Some(failure) = failure(&start.outcome) {
                ui.add(egui::Label::new(failure).wrap());
            }
            for (j, hint) in start.hints.iter().enumerate() {
                ui.add_space(4.0);
                Explanation::for_hint(row, Status::Failed, hint, said(&start.outcome))
                    .show(ui, ui.id().with(("start cause", i, j)));
            }
            if !start.server_said.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("The server said").family(theme::bold_family()));
                for line in &start.server_said {
                    ui.add(egui::Label::new(RichText::new(line).monospace()).wrap());
                }
            }
            if start.partly {
                muted(
                    ui,
                    "The log is large, so only its start and its end were read.",
                );
            }
            ui.add(
                egui::Label::new(RichText::new(&start.file).small().color(pal.text_muted)).wrap(),
            );
        });
        // The arrow is the button a screen reader and the keyboard reach: it carries the line as its name.
        toggle.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::CollapsingHeader, true, &line)
        });
        let header = header.inner;
        // The body stored the state just now, so it is there to toggle.
        if header.clicked()
            && let Some(mut state) = egui::collapsing_header::CollapsingState::load(ui.ctx(), id)
        {
            state.toggle(ui);
            state.store(ui.ctx());
        }
        if !start.exact.is_empty() {
            header.on_hover_text(&start.exact);
        }
    }
    ui.add_space(4.0);
    // The button says "Copied" for a few seconds, as Export's does, and a screen reader hears it.
    let copied_id = egui::Id::new(("last starts copied", row.id()));
    let now = ui.input(|i| i.time);
    let copied = ui
        .data(|d| d.get_temp::<f64>(copied_id))
        .is_some_and(|at| now - at < super::export::COPIED_SECS);
    let button = ui
        .button(if copied { "Copied" } else { COPY })
        .on_hover_text(COPY_HOVER);
    super::export::announce(ui.ctx(), button.id);
    if button.clicked() {
        ui.ctx().copy_text(report(row, last));
        ui.data_mut(|d| d.insert_temp(copied_id, now));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(
                super::export::COPIED_SECS,
            ));
    }
}
