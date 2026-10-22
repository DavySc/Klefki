//! A profile card's "Copy start command" (on/off plan S5): the line that starts Claude Code in one project with only
//! the profile's servers, without saving anything. The engine reads every settings file as that session would, and
//! the card says plainly what would still differ from the profile.

use crate::shell::{COPIED_SECS, announce};
use crate::theme;
use egui::{Frame, Margin, RichText, Ui};
use klefki_core::api::StartPlan;

pub(super) const COPY: &str = "Copy start command";

/// The start command asked for last, for one profile in one project.
pub(crate) struct Seen {
    pub profile: String,
    pub project: String,
    /// `None` until the engine answers.
    plan: Option<Result<StartPlan, String>>,
    /// Copy the line when it arrives.
    copy: bool,
    /// When it was copied, in egui time.
    copied: Option<f64>,
    /// The files changed since it was made: asked for again, without copying.
    stale: bool,
    /// Brought into view when the answer is first drawn: in a small window it is below the buttons.
    reveal: bool,
}

impl Seen {
    pub(super) fn asked(profile: &str, project: &str) -> Self {
        Self {
            profile: profile.to_owned(),
            project: project.to_owned(),
            plan: None,
            copy: true,
            copied: None,
            stale: false,
            reveal: true,
        }
    }

    pub(super) fn is_for(&self, profile: &str, project: &str) -> bool {
        self.profile == profile && self.project == project
    }

    /// The engine's answer, if it is about this one.
    pub(super) fn arrived(
        &mut self,
        profile: &str,
        project: &str,
        plan: Result<StartPlan, String>,
    ) {
        if self.is_for(profile, project) {
            self.plan = Some(plan);
        }
    }

    /// The files changed: what the plan says may no longer be so.
    pub(super) fn snapshot_changed(&mut self) {
        self.stale = self.plan.is_some();
    }

    /// Whether to ask again now, once, for the files as they are.
    pub(super) fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// Whether the button says "Copied" now.
    pub(super) fn copied(&self, now: f64) -> bool {
        self.copied.is_some_and(|at| now - at < COPIED_SECS)
    }
}

/// The button, in the card's row of buttons. `true` when pressed.
pub(super) fn button(ui: &mut Ui, seen: Option<&Seen>, shown_project: &str) -> bool {
    let now = ui.input(|i| i.time);
    let copied = seen.is_some_and(|s| s.copied(now));
    let button = ui
        .button(if copied { "Copied" } else { COPY })
        .on_hover_text(format!(
            "Copy a line that starts Claude Code in {shown_project} with only these servers. Nothing is saved, and \
             other sessions keep their own servers."
        ));
    announce(ui.ctx(), button.id);
    button.clicked()
}

/// The line and what it does, under the card's buttons, once it was asked for. Copies the line the frame it is first
/// drawn.
pub(super) fn panel(ui: &mut Ui, seen: &mut Seen) {
    let pal = theme::palette(ui.ctx());
    let Some(plan) = &seen.plan else {
        ui.add_space(4.0);
        muted(ui, "Reading the settings files…");
        return;
    };
    ui.add_space(6.0);
    let reveal = std::mem::take(&mut seen.reveal);
    let shown = ui.scope(|ui| match plan {
        Ok(plan) => {
            if std::mem::take(&mut seen.copy) {
                ui.ctx().copy_text(plan.line.clone());
                seen.copied = Some(ui.input(|i| i.time));
                // The button above was drawn before this: it says "Copied" in the next frame.
                ui.ctx().request_repaint();
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS));
            }
            Frame::new()
                .fill(pal.field)
                .inner_margin(Margin::same(8))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&plan.line).monospace()).wrap());
                });
            for item in &plan.to_do {
                ui.add_space(2.0);
                ui.add(egui::Label::new(format!("• {item}")).wrap());
            }
            ui.add_space(2.0);
            if plan.exact() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(egui_phosphor::regular::CHECK_CIRCLE).color(pal.success));
                    ui.add(
                        egui::Label::new(format!(
                            "Claude Code loads only these servers there: {}.",
                            klefki_core::api::words::and_list(&plan.loads)
                        ))
                        .wrap(),
                    );
                });
            } else {
                // What differs from the profile comes first, in the warning colour: the line alone does not say it.
                let warn = |ui: &mut Ui, text: &str| {
                    ui.add(egui::Label::new(RichText::new(text).color(pal.warn)).wrap());
                };
                if !plan.missing.is_empty() {
                    warn(ui, "Not loaded, though in the profile:");
                    for m in &plan.missing {
                        ui.add(egui::Label::new(format!("• {m}")).wrap());
                    }
                }
                if !plan.extra.is_empty() {
                    warn(ui, "Loaded too, though not in the profile:");
                    for e in &plan.extra {
                        ui.add(egui::Label::new(format!("• {e}")).wrap());
                    }
                }
                if !plan.loads.is_empty() {
                    muted(
                        ui,
                        &format!(
                            "Loaded from the profile: {}.",
                            klefki_core::api::words::and_list(&plan.loads)
                        ),
                    );
                }
            }
            muted(
                ui,
                "Nothing is saved. Sessions already open, and sessions started without this line, keep their own \
                 servers.",
            );
        }
        Err(why) => {
            seen.copy = false;
            ui.add(
                egui::Label::new(RichText::new(format!("There is no start command: {why}")).color(pal.warn))
                    .wrap(),
            );
        }
    });
    if reveal {
        shown.response.scroll_to_me(None);
    }
}

fn muted(ui: &mut Ui, text: &str) {
    let pal = theme::palette(ui.ctx());
    ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
}
