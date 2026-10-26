//! The Settings page: appearance, Claude Code, and Klefki's updates.

use super::ShellApp;
use crate::theme::{self, ThemeChoice, Tone};
use egui::{RichText, Ui};
use klefki_core::api::{Command, UpdateCheck};

/// Where [`ShellApp::claude_only`] is kept.
const CLAUDE_ONLY_KEY: &str = "checks.claude_only";

/// Whether every server is to be checked by Claude Code, as last saved. Off unless it was turned on.
pub(crate) fn load_claude_only(storage: Option<&dyn eframe::Storage>) -> bool {
    storage
        .and_then(|s| s.get_string(CLAUDE_ONLY_KEY))
        .as_deref()
        == Some("on")
}

pub(super) fn save_claude_only(storage: &mut dyn eframe::Storage, on: bool) {
    storage.set_string(CLAUDE_ONLY_KEY, if on { "on" } else { "off" }.to_owned());
}

impl ShellApp {
    /// Settings. For now, how Klefki looks: light or dark, and which family of greys.
    pub(super) fn settings(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(8.0);
        ui.label(RichText::new("Settings").heading());
        ui.add_space(16.0);
        ui.label(RichText::new("Appearance").strong());
        ui.add_space(6.0);
        egui::Grid::new("appearance")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Theme").color(pal.text_muted));
                ui.horizontal(|ui| {
                    for choice in ThemeChoice::ALL {
                        let text = format!("{} {}", choice.icon(), choice.label());
                        if ui.selectable_value(&mut self.theme, choice, text).changed() {
                            theme::set_choice(ui.ctx(), choice);
                        }
                    }
                });
                ui.end_row();
                ui.label(RichText::new("Greys").color(pal.text_muted));
                ui.horizontal(|ui| {
                    for tone in Tone::ALL {
                        if ui
                            .selectable_value(&mut self.tone, tone, tone.label())
                            .changed()
                        {
                            theme::set_tone(ui.ctx(), tone);
                        }
                    }
                });
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Both keep every text at a contrast of at least 4.5:1.")
                .small()
                .color(pal.text_muted),
        );
        // The top bar names the version only where it fits; here it always has room.
        ui.add_space(24.0);
        ui.label(RichText::new("Programs").strong());
        ui.add_space(6.0);
        egui::Grid::new("programs")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Claude Code").color(pal.text_muted));
                match (self.shown_cli_version(), &self.cli_problem) {
                    (Some(version), _) => ui.label(version),
                    // Beside it, so it is in view even when what follows is not.
                    (None, Some(_)) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Not found").color(pal.warn));
                            ui.add_space(12.0);
                            if ui.button("Look again").on_hover_text("F5").clicked() {
                                self.refresh();
                            }
                        })
                        .response
                    }
                    (None, None) => ui.label(RichText::new("Not known yet").color(pal.text_muted)),
                };
                ui.end_row();
            });
        // Not found: what to do about it, and a way to look again once it is done; the engine's reason under it,
        // for when it is there but cannot be run.
        if self.shown_cli_version().is_none()
            && let Some(problem) = self.cli_problem.clone()
        {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    "Klefki looks for claude in ~/.local/bin and on your PATH. Install Claude Code or add it \
                     to your PATH, then look again.",
                )
                .wrap(),
            );
            ui.add(egui::Label::new(RichText::new(problem).small().color(pal.text_muted)).wrap());
        }
        self.check_settings(ui);
        self.update_settings(ui);
    }

    /// Who checks the servers: Klefki first, or Claude Code only (decision 015).
    fn check_settings(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(14.0);
        if theme::checkbox(
            ui,
            &mut self.claude_only,
            "Check servers with Claude Code only",
        )
        .changed()
        {
            self.send(Command::CheckWithClaudeOnly(self.claude_only));
        }
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Otherwise Klefki starts a server on your computer the way Claude Code would and asks it for \
                     its tools, which is quicker and uses less memory. Claude Code checks every server that does not \
                     connect that way, and every server that is reached by its address.",
                )
                .color(pal.text_muted),
            )
            .wrap(),
        );
    }

    fn update_settings(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(24.0);
        ui.label(RichText::new("Updates").strong());
        ui.add_space(6.0);
        if !self.checks_for_updates {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Klefki {}. This build does not check for updates: it was not built by Klefki's release \
                         workflow.",
                        klefki_core::api::Version::current()
                    ))
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            return;
        }
        egui::Grid::new("updates")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Klefki").color(pal.text_muted));
                ui.horizontal(|ui| {
                    ui.label(klefki_core::api::Version::current().to_string());
                    ui.add_space(12.0);
                    if self.updates.checking {
                        ui.spinner();
                        ui.label(RichText::new("Checking…").color(pal.text_muted));
                        return;
                    }
                    if let Some(version) = self.updates.available() {
                        if self.updated_by_hand.is_some() {
                            ui.label(
                                RichText::new(format!("{version} is available")).color(pal.primary),
                            );
                        } else if self.updates.downloading {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!("Downloading {version}…"))
                                    .color(pal.text_muted),
                            );
                        } else if self.updates.ready() == Some(version) {
                            ui.label(
                                RichText::new(format!(
                                    "{version} is ready: it installs when Klefki closes"
                                ))
                                .color(pal.primary),
                            );
                            let busy = self.change_open();
                            if ui
                                .add_enabled(!busy, egui::Button::new("Restart now"))
                                .on_disabled_hover_text("Finish the open change first.")
                                .clicked()
                            {
                                self.restart_to_update(ui.ctx());
                            }
                        } else {
                            ui.label(
                                RichText::new(format!("{version} is available")).color(pal.primary),
                            );
                            if ui.button("Download").clicked() {
                                self.download_update();
                            }
                        }
                        if ui.button("What's new").clicked() {
                            self.send(Command::OpenReleasePage(version));
                        }
                        // Checking again would only find the same release.
                        return;
                    } else if let Some(UpdateCheck::NoRelease) = &self.updates.found {
                        ui.label(
                            RichText::new("No signed release is published yet")
                                .color(pal.text_muted),
                        );
                    } else if let Some(text) = self.updates.up_to_date(crate::updates::now()) {
                        ui.label(RichText::new(text).color(pal.text_muted));
                    }
                    if ui.button("Check now").clicked() {
                        self.check_for_updates_now();
                    }
                });
                ui.end_row();
            });
        if let Some(UpdateCheck::Failed(why)) = &self.updates.found
            && !self.updates.checking
        {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(why.as_str()).color(pal.warn)).wrap());
        }
        if self.updates.available().is_some() {
            let note = match (&self.updated_by_hand, self.updates.download_failed()) {
                (Some(why), _) => Some((why.clone(), pal.text_muted)),
                (None, Some(why)) => Some((why.to_owned(), pal.warn)),
                (None, None) => None,
            };
            if let Some((text, color)) = note {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
            }
        }
        ui.add_space(10.0);
        let automatic = if self.updated_by_hand.is_some() {
            "Check for updates automatically"
        } else {
            "Update Klefki automatically"
        };
        theme::checkbox(ui, &mut self.updates.auto, automatic);
        ui.add(
            egui::Label::new(
                RichText::new(
                    if self.updated_by_hand.is_some() {
                        "Once a day, Klefki asks GitHub whether a newer version is out. GitHub sees your IP address \
                         and Klefki's version, as with any download; nothing about your servers or settings is sent. \
                         A release is offered only if it carries Klefki's signature."
                    } else {
                        "Once a day, Klefki asks GitHub whether a newer version is out, and downloads it; it installs \
                         when Klefki closes, or when you restart. GitHub sees your IP address and Klefki's version, \
                         as with any download; nothing about your servers or settings is sent. A release is used only \
                         if it carries Klefki's signature, and each file only if it matches the signed checksum."
                    },
                )
                .small()
                .color(pal.text_muted),
            )
            .wrap(),
        );
    }
}
