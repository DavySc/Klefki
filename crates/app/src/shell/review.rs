//! The review dialog: what a change will do, in words first, then each file's diff, before it is applied.

use super::outcome::Outcome;
use super::{ShellApp, closed_on_purpose, settle};
use crate::theme;
use egui::{Frame, Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Command, PlanView};
use std::collections::HashMap;
use std::sync::Arc;

/// Where each line of a plan's diffs starts, laid out once per plan, file and width.
#[derive(Default)]
pub(super) struct DiffTops {
    plan: u64,
    /// Per file: the width laid out at, and the top of each line, with the end of the last one after them.
    pub(super) files: HashMap<usize, (f32, Vec<f32>)>,
}

impl DiffTops {
    fn of(
        &mut self,
        ui: &Ui,
        plan: u64,
        file: usize,
        lines: &[String],
        height: impl Fn(&Ui, &str) -> f32,
    ) -> &[f32] {
        if self.plan != plan {
            self.plan = plan;
            self.files.clear();
        }
        let width = ui.available_width();
        let gap = ui.spacing().item_spacing.y;
        let (laid_at, tops) = self.files.entry(file).or_insert((f32::NAN, Vec::new()));
        if *laid_at != width || tops.len() != lines.len() + 1 {
            *laid_at = width;
            tops.clear();
            tops.push(0.0);
            let mut y = 0.0;
            for line in lines {
                y += height(ui, line) + gap;
                tops.push(y);
            }
        }
        tops
    }
}

impl ShellApp {
    /// The confirmation step: shows exactly what would change, then applies or drops it.
    pub(super) fn plan_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(plan) = self.plan.clone() else {
            // What the last plan's diffs needed goes with it.
            self.diff_tops = DiffTops::default();
            return;
        };
        let applying = self.applying;
        let from_add = self.add.as_ref().is_some_and(|f| f.reviewing);
        let diff_tops = &mut self.diff_tops;
        let mut apply = false;
        let mut back = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("plan_dialog")).show(ctx, |ui| {
            ui.set_width(540.0);
            // Everything above the buttons scrolls when it is taller than the window (many servers added at once),
            // so the buttons always stay in view.
            let room = (ctx.content_rect().height() - 140.0).max(160.0);
            egui::ScrollArea::vertical()
                .id_salt(("plan_body", plan.id))
                .max_height(room)
                .min_scrolled_height(room)
                .show(ui, |ui| {
                    // What happens, in words, first; the exact lines follow for those who want them.
                    if plan.summary.is_empty() {
                        ui.heading("Review the change");
                    } else {
                        ui.label(RichText::new("Review the change").color(pal.text_muted));
                        for line in &plan.summary {
                            ui.add(egui::Label::new(RichText::new(line).heading()).wrap());
                        }
                    }
                    ui.add_space(8.0);
                    if plan.nothing_to_do {
                        // What is already so, by server and place: "already looks like this" alone does not say which.
                        for line in &plan.already {
                            ui.add(egui::Label::new(line.as_str()).wrap());
                        }
                        ui.label(if plan.already.is_empty() {
                            "The configuration already looks like this. Nothing would be written."
                        } else {
                            "Nothing would be written."
                        });
                    }
                    // Warnings first: what a change costs is read before the details (cross-review of the restore).
                    for warning in &plan.warnings {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} {warning}", icon::WARNING))
                                    .color(pal.warn),
                            )
                            .wrap(),
                        );
                    }
                    for review in &plan.reviews {
                        ui.add_space(4.0);
                        Frame::new()
                            .stroke(egui::Stroke::new(1.0, pal.warn))
                            .corner_radius(8)
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(&review.title).strong().color(pal.warn));
                                for line in &review.lines {
                                    ui.add(
                                        egui::Label::new(RichText::new(line).monospace()).wrap(),
                                    );
                                }
                            });
                    }
                    for (at, file) in plan.files.iter().enumerate() {
                        if file.lines.is_empty() {
                            continue;
                        }
                        ui.add_space(8.0);
                        // The size is in the heading: only the lines in view are laid out, so a screen reader would
                        // otherwise not know there are more, and the box below shows few at a time.
                        let count = file.lines.len();
                        ui.label(
                            RichText::new(format!(
                                "Changes in {} · {count} {}",
                                file.path,
                                if count == 1 { "line" } else { "lines" }
                            ))
                            .small()
                            .color(pal.text_muted),
                        );
                        Frame::new()
                            .fill(pal.field)
                            .corner_radius(8)
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                // Its own scroll position per plan and file: each review opens at the top.
                                // Only the lines in view are laid out: a diff can have thousands.
                                // A bar that is always drawn, beside the lines: the default one is invisible until
                                // pointed at, so a long diff would look no longer than the box.
                                ui.spacing_mut().scroll = egui::style::ScrollStyle {
                                    // In the text's colour, on a track that stands out from the box.
                                    foreground_color: true,
                                    ..egui::style::ScrollStyle::solid()
                                };
                                ui.visuals_mut().extreme_bg_color = pal.border;
                                egui::ScrollArea::vertical()
                                    // By its place in the plan: one file can have two diffs (what Klefki
                                    // writes, and what Claude Code's own command will).
                                    .id_salt(("plan_diff", plan.id, at))
                                    .max_height(220.0)
                                    // As wide as the box, so the bar is at its edge, not after the widest line in view.
                                    .auto_shrink([false, true])
                                    .scroll_bar_visibility(
                                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                                    )
                                    .show_viewport(ui, |ui, viewport| {
                                        let line = |ui: &Ui, line: &str| {
                                            let color = if line.starts_with("+ ") {
                                                pal.success
                                            } else if line.starts_with("- ") {
                                                pal.danger
                                            } else {
                                                pal.text_muted
                                            };
                                            egui::WidgetText::from(
                                                RichText::new(line).monospace().color(color),
                                            )
                                            .into_galley(
                                                ui,
                                                Some(egui::TextWrapMode::Wrap),
                                                ui.available_width(),
                                                egui::FontSelection::Default,
                                            )
                                        };
                                        let tops =
                                            diff_tops.of(ui, plan.id, at, &file.lines, |ui, l| {
                                                line(ui, l).size().y
                                            });
                                        let first = tops
                                            .partition_point(|&top| top <= viewport.min.y)
                                            .saturating_sub(1);
                                        let end = tops
                                            .partition_point(|&top| top < viewport.max.y)
                                            .min(file.lines.len());
                                        let total = tops.last().copied().unwrap_or(0.0);
                                        ui.add_space(tops[first]);
                                        for text in &file.lines[first..end.max(first)] {
                                            ui.add(egui::Label::new(line(ui, text)));
                                        }
                                        ui.add_space(total - tops[end.max(first)]);
                                    });
                            });
                    }
                    for note in &plan.notes {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} {note}", icon::INFO))
                                    .small()
                                    .color(pal.text_muted),
                            )
                            .wrap(),
                        );
                    }
                });
            ui.add_space(12.0);
            let label = if !plan.action.is_empty() {
                plan.action.as_str()
            } else if plan.reviews.is_empty() {
                "Apply"
            } else {
                "Approve"
            };
            ui.horizontal(|ui| {
                let fill = if plan.destructive {
                    pal.danger
                } else {
                    pal.primary
                };
                let confirm = egui::Button::new(RichText::new(label).strong().color(pal.on_bright))
                    .fill(fill)
                    .min_size(egui::vec2(96.0, 34.0));
                let enabled = !plan.nothing_to_do && !applying;
                if ui
                    .add_enabled(enabled, confirm)
                    .on_hover_text("Enter")
                    .clicked()
                {
                    apply = true;
                }
                // Back to the Add dialog, as it was, to change something.
                if from_add
                    && ui
                        .add_enabled(
                            !applying,
                            egui::Button::new(format!("{} Back", icon::ARROW_LEFT))
                                .min_size(egui::vec2(80.0, 34.0)),
                        )
                        .on_hover_text("Change what is added (Esc)")
                        .clicked()
                {
                    back = true;
                }
                // Once Apply is pressed the engine writes the plan; closing now would hide that.
                if ui
                    .add_enabled(
                        !applying,
                        egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)),
                    )
                    .on_hover_text(if from_add { "Add nothing" } else { "Esc" })
                    .clicked()
                {
                    cancel = true;
                }
                if applying {
                    // Text, not a spinner: an animation would repaint every frame. A write takes milliseconds;
                    // Claude Code's commands take seconds each, so each one is named as it starts.
                    match &self.applying_step {
                        None => {
                            ui.label(RichText::new("Saving…").color(pal.text_muted));
                        }
                        Some(step) => {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(step).color(pal.text));
                                ui.label(
                                    RichText::new("Keep Klefki open until this is done.")
                                        .small()
                                        .color(pal.text_muted),
                                );
                            });
                        }
                    }
                }
                // Enter confirms, as in any dialog; not while a field would take it.
                if enabled
                    && !ui.ctx().egui_wants_keyboard_input()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    apply = true;
                }
            });
        });
        settle(ctx, &modal.response);
        if closed_on_purpose(&modal) && !applying {
            // Esc backs out one step: from the review of an Add, to the Add dialog, as it was.
            if from_add {
                back = true;
            } else {
                cancel = true;
            }
        }
        if apply {
            self.applying = true;
            self.send(Command::Apply(plan.id));
            // The dialog stays until the engine answers, so a second click cannot apply twice.
        } else if back || cancel {
            self.send(Command::Discard(plan.id));
            self.plan = None;
            self.applying = false;
            self.adding.clear();
            if back && let Some(form) = &mut self.add {
                form.reviewing = false;
            } else if from_add {
                self.close_add();
            }
        }
    }

    /// The plan asked for is ready: shown for review, said on the profile's card, or discarded while another is open.
    pub(super) fn plan_ready(&mut self, plan: PlanView) {
        self.preparing = false;
        if self.duplicate.as_ref().is_some_and(|f| f.reviewing) {
            self.duplicate = None;
        }
        let saves_draft = std::mem::take(&mut self.draft_asked);
        let profile = self.profile_asked.take();
        // Said on the profile's card while the page is shown; once it is left, the review says it.
        let card = profile
            .filter(|_| plan.nothing_to_do && self.plan.is_none())
            .and_then(|name| Some((name, self.profiles.as_mut()?)));
        if let Some((name, page)) = card {
            page.unchanged(&name, plan.already, plan.warnings, plan.others_on);
            self.send(Command::Discard(plan.id));
        } else if self.plan.is_some() {
            // Only one dialog at a time; the one open (perhaps being saved) keeps it.
            self.send(Command::Discard(plan.id));
        } else {
            self.draft_plan = saves_draft.then_some(plan.id);
            self.plan = Some(Arc::new(plan));
        }
    }

    /// No plan could be made: said in the dialog that asked for it, which comes back as it was, or as the outcome.
    pub(super) fn plan_failed(&mut self, why: String) {
        self.preparing = false;
        self.draft_asked = false;
        self.profile_asked = None;
        self.adding.clear();
        let duplicate = self.duplicate.as_mut().filter(|f| f.reviewing);
        match (self.add.as_mut().filter(|f| f.reviewing), duplicate) {
            // Said in the Add dialog, which comes back as it was, so nothing typed is lost.
            (Some(form), _) => form.review_failed(why),
            // The same for Duplicate.
            (None, Some(form)) => form.review_failed(why),
            (None, None) => self.outcome = Some(Outcome::problem(why)),
        }
    }
}
