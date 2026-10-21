//! The outcome message: what a change did, or what went wrong, beside the list or over it, with its next steps.

use super::{Page, ShellApp};
use crate::theme::{self, Palette};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Change, ServerId};

/// What happened to the last change, for the banner.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Outcome {
    pub(super) text: String,
    pub(super) ok: bool,
    /// When it was first shown (egui time, seconds). A success fades after a few seconds; a problem stays.
    pub(super) shown_at: Option<f64>,
    /// A server just added that signs in: the card offers to sign in to it now, and stays until used or dismissed.
    pub(super) sign_in: Option<ServerId>,
    /// It worked, with something to take in (settings Claude Code left out, say): drawn as a warning, and it stays.
    pub(super) warn: bool,
    /// What undoes it (a move back), offered on the card, which then stays until used or dismissed.
    pub(super) undo: Option<Change>,
    /// The change is in the Activity log, with its backup: the card offers to open it.
    pub(super) logged: bool,
    /// No file came of asking for one to import: the card offers to type its path instead.
    pub(super) type_path: bool,
}

/// What was chosen on the outcome message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum OutcomeChoice {
    SignIn,
    Undo,
    OpenLog,
    TypePath,
    Dismiss,
}

/// How long a success message stays.
pub(super) const SUCCESS_SECONDS: f64 = 4.0;

/// The outcome message: its text's widest, its narrowest beside the detail panel, and its distance from the
/// window's edges.
const TOAST_WIDTH: f32 = 420.0;
const TOAST_NARROWEST: f32 = 280.0;
const TOAST_MARGIN: f32 = 16.0;

/// The outcome message's color and glyph: a problem, a warning or a success.
fn outcome_look(pal: &Palette, outcome: &Outcome) -> (Color32, &'static str) {
    if outcome.type_path {
        (pal.primary, icon::INFO)
    } else if outcome.ok && outcome.warn {
        (pal.warn, icon::WARNING)
    } else if outcome.ok {
        (pal.success, icon::CHECK_CIRCLE)
    } else {
        (pal.danger, icon::WARNING_CIRCLE)
    }
}

/// The outcome message's card, edged in its color.
fn outcome_frame(pal: &Palette, color: Color32) -> Frame {
    Frame::new()
        .fill(pal.panel_raised)
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(8)
        .inner_margin(Margin::same(12))
}

impl ShellApp {
    /// The outcome of the last change, as a small card in the bottom-right corner: it does not push the list
    /// around, a success fades after a few seconds, and a problem stays until dismissed.
    pub(super) fn outcome_toast(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let now = ctx.input(|i| i.time);
        let Some(outcome) = self.outcome.as_mut() else {
            return;
        };
        let shown_at = *outcome.shown_at.get_or_insert(now);
        // A card that offers something to do stays until it is used or dismissed: time limits on a choice exclude
        // whoever reads slowly.
        if outcome.ok && !outcome.warn && outcome.sign_in.is_none() && outcome.undo.is_none() {
            let left = SUCCESS_SECONDS - (now - shown_at);
            if left <= 0.0 {
                self.outcome = None;
                return;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        }
        let outcome = outcome.clone();
        let (color, glyph) = outcome_look(pal, &outcome);
        let mut chosen = self.outcome_chosen.take();
        if !self.outcome_inline {
            // In the window's bottom-right corner, but never over the save bar, whose buttons may be wanted while
            // it shows: beside the detail panel, over the list, where there is room (otherwise the detail panel
            // has it, above the bar).
            let screen = ctx.content_rect();
            let mut offset = egui::vec2(-TOAST_MARGIN, -TOAST_MARGIN);
            let mut width = TOAST_WIDTH;
            if self.save_bar_shown
                && let Some(room) = self.list_room
            {
                offset.x = room.right() - screen.right() - TOAST_MARGIN;
                width = self.outcome_width_beside(ctx).min(TOAST_WIDTH);
            }
            egui::Area::new(egui::Id::new("outcome"))
                .anchor(egui::Align2::RIGHT_BOTTOM, offset)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    outcome_frame(pal, color)
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: Color32::from_black_alpha(80),
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(width);
                            if let Some(choice) =
                                self.outcome_card(ui, &outcome, glyph, color, false)
                            {
                                chosen = Some(choice);
                            }
                        });
                });
        }
        match chosen {
            Some(OutcomeChoice::SignIn) => {
                if let Some(row) = outcome
                    .sign_in
                    .and_then(|id| self.snapshot.servers.iter().find(|r| r.is(&id)).cloned())
                {
                    self.outcome = None;
                    self.start_sign_in(&row);
                }
            }
            Some(OutcomeChoice::Undo) => {
                if let Some(change) = outcome.undo {
                    self.outcome = None;
                    self.prepare(vec![change]);
                }
            }
            Some(OutcomeChoice::OpenLog) => {
                self.outcome = None;
                self.go(Page::ActivityLog);
            }
            Some(OutcomeChoice::TypePath) => {
                self.outcome = None;
                if let Some(form) = self.open_import_form() {
                    form.typing = true;
                    form.focus = true;
                }
            }
            Some(OutcomeChoice::Dismiss) => self.outcome = None,
            None => {}
        }
    }

    /// How wide the outcome message's text can be beside the detail panel, over the list.
    fn outcome_width_beside(&self, ctx: &egui::Context) -> f32 {
        let frame = outcome_frame(theme::palette(ctx), Color32::TRANSPARENT);
        self.list_room.map_or(0.0, |room| {
            room.width() - 2.0 * TOAST_MARGIN - frame.total_margin().sum().x
        })
    }

    /// The outcome message fits beside the detail panel, so its save bar stays clear.
    pub(super) fn outcome_fits_beside(&self, ctx: &egui::Context) -> bool {
        self.outcome_width_beside(ctx) >= TOAST_NARROWEST
    }

    /// The outcome message as a strip above the save bar, when it does not fit beside the detail panel: in the
    /// layout, so it covers nothing and the form above still scrolls.
    pub(super) fn outcome_strip(&mut self, ui: &mut Ui) {
        let Some(outcome) = self.outcome.clone() else {
            return;
        };
        let pal = theme::palette(ui.ctx());
        let (color, glyph) = outcome_look(pal, &outcome);
        outcome_frame(pal, color).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.outcome_chosen = self.outcome_card(ui, &outcome, glyph, color, true);
        });
        self.outcome_inline = true;
    }

    /// The outcome message's text and buttons, and which of them was used.
    /// In a strip (`strip`), a long text scrolls past two and a half lines, the half line showing there is more,
    /// and the buttons wrap, so it leaves the form room and does not widen the panel.
    fn outcome_card(
        &self,
        ui: &mut Ui,
        outcome: &Outcome,
        glyph: &str,
        color: Color32,
        strip: bool,
    ) -> Option<OutcomeChoice> {
        let pal = theme::palette(ui.ctx());
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).size(16.0).color(color));
            let text = egui::Label::new(RichText::new(&outcome.text).color(pal.text)).wrap();
            if strip {
                // Wrapped lines follow each other with no spacing between them.
                let line = ui.text_style_height(&egui::TextStyle::Body);
                egui::ScrollArea::vertical()
                    .id_salt("outcome_text")
                    .max_height(2.5 * line)
                    .min_scrolled_height(2.5 * line)
                    .show(ui, |ui| ui.add(text));
            } else {
                ui.add(text);
            }
        });
        ui.with_layout(
            Layout::right_to_left(Align::Min).with_main_wrap(strip),
            |ui| {
                if ui.small_button("Dismiss").clicked() {
                    chosen = Some(OutcomeChoice::Dismiss);
                }
                // The backup's place stays on the Activity log after this card is gone.
                if outcome.type_path && ui.small_button("Type a path instead").clicked() {
                    chosen = Some(OutcomeChoice::TypePath);
                }
                if outcome.logged
                    && self.page != Page::ActivityLog
                    && ui.small_button("Open activity log").clicked()
                {
                    chosen = Some(OutcomeChoice::OpenLog);
                }
                if outcome.undo.is_some()
                    && ui
                        .add_enabled(self.can_prepare(), egui::Button::new("Undo"))
                        .on_hover_text("Move it back, after a review")
                        .clicked()
                {
                    chosen = Some(OutcomeChoice::Undo);
                }
                if outcome.sign_in.is_some() && self.sign_in.is_none() {
                    let button =
                        egui::Button::new(RichText::new("Sign in").strong().color(pal.on_bright))
                            .fill(pal.primary);
                    if ui.add(button).clicked() {
                        chosen = Some(OutcomeChoice::SignIn);
                    }
                }
            },
        );
        chosen
    }
}
