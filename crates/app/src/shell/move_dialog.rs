//! The Move dialog: a server to one other place.

use super::places::Places;
use super::{GOES_ALONG, ShellApp, closed_on_purpose};
use crate::theme;
use egui::RichText;
use egui_phosphor::regular as icon;
use klefki_core::api::{Change, Scope, ServerId, Target};

/// What the Move dialog holds: the one place the server goes.
pub(super) struct MoveForm {
    source: ServerId,
    /// What is being moved, under the title: `http · https://…`.
    about: String,
    places: Places,
    /// It signs in with OAuth, so it is signed out by the move.
    signs_in: bool,
}

impl ShellApp {
    /// Opens the Move dialog for the selected server, with nowhere chosen yet: where it is now cannot be chosen.
    pub(super) fn open_move(&mut self) {
        let Some(row) = self.duplicable() else {
            return;
        };
        let mut places = Places::new(&self.snapshot.projects, row.project.as_deref());
        places.one = true;
        places.now = Some(match (&row.project, row.scope) {
            (Some(p), Scope::Local) => Target::Local(p.clone()),
            _ => Target::User,
        });
        self.moving = Some(MoveForm {
            source: row.id(),
            about: format!("{} \u{b7} {}", row.transport.label(), row.target),
            places,
            signs_in: row.oauth.is_some(),
        });
    }

    pub(super) fn move_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let draft = self.drafted();
        let Some(form) = &mut self.moving else {
            return;
        };
        let draft_goes = draft.as_ref() == Some(&form.source);
        let name = form.source.name.clone();
        let taken = form.places.taken(&name, &self.snapshot.servers);
        let ready = taken.is_none() && form.places.targets().len() == 1;
        let mut review = ready && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        let list_height = (ctx.content_rect().height() - 420.0).clamp(96.0, 320.0);
        let modal = egui::Modal::new(egui::Id::new("move_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(format!("Move {name}"));
            ui.add(
                egui::Label::new(RichText::new(&form.about).small().color(pal.text_muted))
                    .truncate(),
            );
            ui.add_space(12.0);
            // What it costs, where the place is chosen.
            if form.signs_in {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} It signs in with OAuth: Claude Code removes it and adds it again, so you will need to sign in to it again afterwards.",
                            icon::WARNING
                        ))
                        .color(pal.warn),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
            }
            ui.label(RichText::new("Move it to").color(pal.text_muted));
            form.places.ui(ui, list_height, None);
            ui.add_space(10.0);
            // What Review will lead to, or why it cannot yet.
            if let Some(taken) = &taken {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{name} already exists {taken}. Rename one of them first, or choose another place."
                        ))
                        .color(pal.danger),
                    )
                    .wrap(),
                );
            } else if let Some(places) = form.places.one_in_words() {
                ui.add(egui::Label::new(format!("Moves {name} {places}.")).wrap());
            } else {
                ui.label(RichText::new("Choose where it goes.").color(pal.text_muted));
            }
            if !form.signs_in {
                ui.add(
                    egui::Label::new(
                        RichText::new("It keeps its settings, environment and header values.")
                            .small()
                            .color(pal.text_muted),
                    )
                    .wrap(),
                );
            }
            if draft_goes {
                ui.label(RichText::new(GOES_ALONG).small().color(pal.text_muted));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let confirm =
                    egui::Button::new(RichText::new("Review").strong().color(pal.on_bright))
                        .fill(pal.primary)
                        .min_size(egui::vec2(96.0, 34.0));
                review |= ui
                    .add_enabled(ready, confirm)
                    .on_hover_text("Enter")
                    .on_disabled_hover_text("Choose where it goes")
                    .clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
        });
        if review {
            let to = form.places.targets().remove(0);
            let change = Change::Move {
                id: form.source.clone(),
                to,
            };
            let signs_in = form.signs_in;
            let from = form.source.clone();
            self.moving = None;
            self.prepare(vec![change]);
            self.adding_signs_in = signs_in;
            self.moved_from = Some((from, signs_in));
        } else if cancel || closed_on_purpose(&modal) {
            self.moving = None;
        }
    }
}
