//! The top bar: the search, Refresh, the theme, the command palette, Claude Code's version and a found update,
//! and getting that update.

use super::{ShellApp, flat_items, name_for_screen_readers};
use crate::command::shortcut;
use crate::theme::{self, Palette, ThemeOption};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Command, UpdateDownload};

pub(super) fn bar_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.chrome)
        .stroke(egui::Stroke::new(1.0, pal.divider))
        .inner_margin(Margin::symmetric(12, 6))
}

pub(super) fn search_id() -> egui::Id {
    egui::Id::new("search")
}

impl ShellApp {
    pub(super) fn top_bar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(icon::KEY).size(17.0).color(pal.key));
            ui.label(
                RichText::new("Klefki")
                    .family(theme::bold_family())
                    .size(18.0)
                    .color(pal.primary),
            );
            ui.label(RichText::new(self.page.title()).color(pal.text_muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let refresh = ui
                    .button(icon::ARROWS_CLOCKWISE)
                    .on_hover_text("Refresh and re-check all (F5)");
                name_for_screen_readers(&refresh, "Refresh and re-check all");
                if refresh.clicked() {
                    self.refresh();
                }
                if let Some(version) = self.updates.available() {
                    self.update_button(ui, version, pal.primary);
                } else if let Some(version) = self.updates.updated
                    && ui
                        .button(
                            RichText::new(format!(
                                "{} Updated to {version} · What's new",
                                icon::CHECK_CIRCLE
                            ))
                            .color(pal.primary),
                        )
                        .on_hover_text("Opens the release's page on GitHub.")
                        .clicked()
                {
                    self.updates.updated = None;
                    self.send(Command::OpenReleasePage(version));
                }
                let current = ThemeOption::new(self.theme, self.tone);
                let mut picked = None;
                let theme_menu = egui::ComboBox::from_id_salt("theme")
                    .selected_text(format!("{} {}", current.icon(), current.label()))
                    .width(140.0)
                    // All six and the separator fit without a scroll bar (the default is 200 px).
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        flat_items(ui);
                        let focus = pal.focus;
                        for (i, option) in ThemeOption::ALL.into_iter().enumerate() {
                            if i > 0 && option.tone != ThemeOption::ALL[i - 1].tone {
                                ui.separator();
                            }
                            let mut text = format!("{} {}", option.icon(), option.label());
                            if option == current {
                                // The fill alone is too faint to tell the current entry (WCAG 1.4.11).
                                text = format!("{text}  {}", icon::CHECK);
                            }
                            let response = ui.selectable_label(option == current, text);
                            if response.has_focus() {
                                // Focus must not look like the current entry's fill (WCAG 2.4.7).
                                ui.painter().rect_stroke(
                                    response.rect,
                                    ui.visuals().widgets.active.corner_radius,
                                    egui::Stroke::new(1.5, focus),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if response.clicked() {
                                picked = Some(option);
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Theme");
                // Named, and its value in words: what it shows starts with an icon.
                theme_menu
                    .ctx
                    .accesskit_node_builder(theme_menu.id, |node| {
                        node.set_label("Theme");
                        node.set_value(current.label());
                    });
                if let Some(option) = picked {
                    self.theme = option.choice;
                    self.tone = option.tone;
                    theme::set_choice(ui.ctx(), option.choice);
                    theme::set_tone(ui.ctx(), option.tone);
                }
                let commands = ui
                    .button(
                        RichText::new(format!("{} {}", icon::COMMAND, shortcut("K")))
                            .color(pal.text_muted),
                    )
                    .on_hover_text("Commands: everything Klefki can do, by name");
                // The shortcut it shows is part of its name (WCAG 2.5.3, label in name).
                name_for_screen_readers(&commands, &format!("Commands ({})", shortcut("K")));
                if commands.clicked() {
                    self.command.open();
                }
                // On the Activity log it filters the changes, and says so: a box that filtered the hidden list of
                // servers would find nothing there.
                // The shortcut is left out there, so the longer words fit the box.
                let (query, hint, name) = if let Some(page) = &mut self.activity {
                    (
                        &mut page.filter,
                        format!("{} Filter changes", icon::MAGNIFYING_GLASS),
                        "Filter changes",
                    )
                } else if let Some(page) = &mut self.secrets {
                    (
                        &mut page.filter,
                        format!("{} Filter secrets", icon::MAGNIFYING_GLASS),
                        "Filter secrets",
                    )
                } else if let Some(page) = &mut self.desktop {
                    (
                        &mut page.filter,
                        format!("{} Filter servers", icon::MAGNIFYING_GLASS),
                        "Filter servers",
                    )
                } else if let Some(page) = &mut self.profiles {
                    (
                        &mut page.filter,
                        format!("{} Filter profiles", icon::MAGNIFYING_GLASS),
                        "Filter profiles",
                    )
                } else {
                    (
                        &mut self.search,
                        format!("{} Filter  {}", icon::MAGNIFYING_GLASS, shortcut("F")),
                        "Filter servers",
                    )
                };
                // A hint in the box is no name: it is gone once something is typed.
                let filter = ui.add(
                    egui::TextEdit::singleline(query)
                        .id(search_id())
                        .hint_text(hint)
                        .desired_width(220.0),
                );
                name_for_screen_readers(&filter, name);
                if let Some(version) = self.shown_cli_version() {
                    // Only where it fits: in a narrow window it would be drawn over the title. Where the whole
                    // of it does not, the number alone, named in full on hover.
                    let full = format!("Claude CLI {version}");
                    let fitting = [full.as_str(), version].into_iter().find_map(|text| {
                        let galley =
                            egui::WidgetText::from(RichText::new(text).color(pal.text_muted))
                                .into_galley(
                                    ui,
                                    Some(egui::TextWrapMode::Extend),
                                    f32::INFINITY,
                                    egui::TextStyle::Body,
                                );
                        (galley.size().x <= ui.available_width()).then_some(galley)
                    });
                    if let Some(galley) = fitting {
                        ui.label(galley)
                            .on_hover_text(format!("Claude Code {version}"));
                    }
                }
            });
        });
    }

    /// Starts the automatic check for updates once the window has been open a moment, and wakes up for it.
    pub(super) fn look_for_updates(&mut self, ctx: &egui::Context) {
        if !self.checks_for_updates || self.core.is_none() {
            return;
        }
        let uptime = ctx.input(|i| i.time);
        if self.updates.due(uptime, crate::updates::now()) {
            self.send(Command::CheckForUpdates);
        } else if self.updates.auto && uptime < crate::updates::START_DELAY {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                crate::updates::START_DELAY - uptime,
            ));
        }
    }

    /// The top bar's way to the update found: download it, then restart into it. Installs updated by hand open the
    /// release's page instead.
    fn update_button(&mut self, ui: &mut Ui, version: klefki_core::api::Version, color: Color32) {
        let label =
            |text: String| RichText::new(format!("{} {text}", icon::ARROW_CIRCLE_UP)).color(color);
        if let Some(why) = &self.updated_by_hand {
            let hover = format!(
                "Klefki {version} is out. Opens its page on GitHub, with what is new and the downloads. {why}"
            );
            if ui
                .button(label(format!("Update: {version}")))
                .on_hover_text(hover)
                .clicked()
            {
                self.send(Command::OpenReleasePage(version));
            }
        } else if self.updates.downloading {
            ui.add_enabled(
                false,
                egui::Button::new(label(format!("Downloading {version}…"))),
            );
        } else if self.updates.ready() == Some(version) {
            let busy = self.change_open();
            let response = ui
                .add_enabled(!busy, egui::Button::new(label("Restart to update".into())))
                .on_hover_text(format!(
                    "Installs Klefki {version} and starts it again. Otherwise it installs when Klefki closes."
                ))
                .on_disabled_hover_text(format!(
                    "Finish the open change first. Klefki {version} also installs when Klefki closes."
                ));
            if response.clicked() {
                self.restart_to_update(ui.ctx());
            }
        } else {
            let hover = match self.updates.download_failed() {
                Some(why) => format!("{why} Click to try again."),
                None => {
                    format!("Klefki {version} is out. Downloads it; you choose when to restart.")
                }
            };
            if ui
                .button(label(format!("Update: {version}")))
                .on_hover_text(hover)
                .clicked()
            {
                self.download_update();
            }
        }
    }

    pub(super) fn download_update(&mut self) {
        self.updates.downloading();
        self.send(Command::DownloadUpdate);
    }

    /// Installs the downloaded update and closes Klefki, which the new version replaces.
    pub(super) fn restart_to_update(&mut self, ctx: &egui::Context) {
        let Some(core) = &self.core else {
            return;
        };
        match core.install_update(true) {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(why) => self.updates.downloaded(UpdateDownload::Failed(why)),
        }
    }

    /// A change is being made or reviewed: restarting now would lose it.
    pub(super) fn change_open(&self) -> bool {
        self.plan.is_some()
            || self.applying
            || self.preparing
            || self.editor.is_some()
            || self.add.is_some()
            || self.duplicate.is_some()
            || self.moving.is_some()
            || self.sign_in.is_some()
            || self.secret.is_some()
            || self.secret_input.is_some()
            || self.casing.is_some()
            || self.forgetting.is_some()
            || self.exporting.is_some()
            || self.import_waiting
    }

    pub(super) fn check_for_updates_now(&mut self) {
        self.updates.check_now();
        self.send(Command::CheckForUpdates);
    }

    /// Claude Code's version, when it is known. Not while Claude Code cannot be found: the version would be of a
    /// program that is gone.
    pub(super) fn shown_cli_version(&self) -> Option<&str> {
        self.cli_version
            .as_deref()
            .or(self.snapshot.cli_version.as_deref())
            .filter(|_| self.cli_problem.is_none())
    }
}
