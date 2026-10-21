//! The notices above the server list that ask for a change: old switch-off lists to migrate, and servers a change
//! took out that Klefki kept.

use super::{ShellApp, folder_name, some_names};
use crate::theme;
use egui::{Align, Frame, Layout, Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Change, LegacyDisabled};
use std::sync::Arc;

impl ShellApp {
    /// Projects with the switch-off list of older Claude Code versions: what is on because Claude Code no longer
    /// reads it, and a button that moves it into the list it does read. Read-only until the user reviews the plan.
    pub(super) fn legacy_notice(&mut self, ui: &mut Ui) {
        if self.snapshot.legacy_disabled.is_empty() {
            return;
        }
        // Built once per snapshot, not every frame.
        let notice = match &self.legacy_cache {
            Some((for_snapshot, built)) if Arc::ptr_eq(for_snapshot, &self.snapshot) => {
                built.clone()
            }
            _ => {
                let built = Arc::new(LegacyNotice::new(&self.snapshot.legacy_disabled));
                self.legacy_cache = Some((self.snapshot.clone(), built.clone()));
                built
            }
        };
        let pal = theme::palette(ui.ctx());
        let can_migrate = self.core.is_some() && self.can_prepare() && notice.migratable;
        let mut migrate = false;
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(notice.title.as_str()).color(pal.warn));
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Older Claude Code versions kept switched-off servers in mcpServersDisabled. Claude Code no longer reads that list, so servers on it can be on again. Migrating moves them to the list it reads now.",
                        )
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
                for (line, path) in &notice.lines {
                    ui.add(egui::Label::new(line.as_str()).wrap())
                        .on_hover_text(path.as_str());
                }
                if let Some(more) = &notice.more {
                    ui.label(RichText::new(more.as_str()).color(pal.text_muted));
                }
                migrate = ui
                    .add_enabled(can_migrate, egui::Button::new("Migrate…"))
                    .on_hover_text("Review the change before anything is written")
                    .on_disabled_hover_text(if self.core.is_none() {
                        "Not connected to the configuration"
                    } else if !notice.migratable {
                        "Only lists of server names can be migrated"
                    } else {
                        "Wait for the open change to finish"
                    })
                    .clicked();
            });
        ui.add_space(6.0);
        if migrate {
            self.migrate_legacy();
        }
    }

    /// Servers a change took out and could not put back (Claude Code's command failed twice): Klefki kept their
    /// definitions, and offers to add each back where it was, or to forget it.
    pub(super) fn kept_notice(&mut self, ui: &mut Ui) {
        if self.snapshot.kept.is_empty() {
            return;
        }
        let pal = theme::palette(ui.ctx());
        let can = self.core.is_some() && self.can_prepare();
        let mut chosen = None;
        let out = self.snapshot.kept.iter().filter(|k| !k.back).count();
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (title, body) = match out {
                    0 => (
                        "Klefki keeps settings Claude Code left out".to_owned(),
                        "The server is back, but Claude Code did not keep all of its settings. Klefki keeps the whole definition until you forget it.",
                    ),
                    1 => (
                        "A server is out of the configuration".to_owned(),
                        "A change took it out and Claude Code could not put it back. Klefki kept its definition, with its client secret, so you can add it back where it was.",
                    ),
                    n => (
                        format!("{n} servers are out of the configuration"),
                        "A change took them out and Claude Code could not put them back. Klefki kept each definition, with its client secret, so you can add it back where it was.",
                    ),
                };
                ui.label(RichText::new(title).color(pal.warn));
                ui.add(egui::Label::new(RichText::new(body).color(pal.text_muted)).wrap());
                let wait = "Wait for the open change to finish";
                for kept in &self.snapshot.kept {
                    ui.horizontal(|ui| {
                        // The server first, then what can be done with it.
                        let mut what = format!("{} {}", kept.name, kept.place);
                        if kept.back {
                            what.push_str(" is back without some settings");
                        } else if kept.signs_in {
                            what.push_str(" (signs in with OAuth)");
                        }
                        ui.add(egui::Label::new(what).truncate());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(can, egui::Button::new("Forget\u{2026}"))
                                .on_hover_text("Delete Klefki's copy; the review says what is lost")
                                .on_disabled_hover_text(wait)
                                .clicked()
                            {
                                chosen = Some(Change::ForgetKept {
                                    kept: kept.id.clone(),
                                });
                            }
                            // Back already: only its fuller definition is left to forget.
                            if !kept.back
                                && ui
                                .add_enabled(can, egui::Button::new("Put back"))
                                .on_hover_text("Add it back where it was, after a review")
                                .on_disabled_hover_text(wait)
                                .clicked()
                            {
                                chosen = Some(Change::PutBack {
                                    kept: kept.id.clone(),
                                });
                            }
                        });
                    });
                }
            });
        ui.add_space(6.0);
        if let Some(change) = chosen {
            self.prepare(vec![change]);
        }
    }

    /// Asks for one plan that moves every old switch-off list that can be moved.
    pub(super) fn migrate_legacy(&mut self) {
        let changes: Vec<Change> = self
            .snapshot
            .legacy_disabled
            .iter()
            .filter(|l| !l.malformed)
            .map(|l| Change::MigrateLegacyDisabled {
                project: l.project.clone(),
            })
            .collect();
        if self.core.is_some() && !changes.is_empty() {
            self.prepare(changes);
        }
    }
}

/// What the notice about old switch-off lists says, for one snapshot.
pub(super) struct LegacyNotice {
    title: String,
    /// One line per project shown, with its full path for the hover.
    lines: Vec<(String, String)>,
    /// "… and N more" when there are more projects than lines.
    more: Option<String>,
    /// At least one list can be migrated (holds only names).
    migratable: bool,
}

impl LegacyNotice {
    fn new(lists: &[LegacyDisabled]) -> Self {
        const SHOWN: usize = 4;
        let title = if lists.len() == 1 {
            "Old switch-off list in 1 project".to_owned()
        } else {
            format!("Old switch-off lists in {} projects", lists.len())
        };
        let lines = lists
            .iter()
            .take(SHOWN)
            .map(|l| {
                let folder = folder_name(&l.shown);
                let line = if l.malformed {
                    format!(
                        "{folder}: holds something other than server names; edit it in .claude.json by hand"
                    )
                } else if l.names.is_empty() {
                    format!("{folder}: an empty list")
                } else {
                    let state = match l.turns_off.as_slice() {
                        [] => "already off".to_owned(),
                        [one] => format!("{one} is on"),
                        many => format!("{} are on", some_names(many)),
                    };
                    format!("{folder}: {} ({state})", some_names(&l.names))
                };
                (line, l.shown.clone())
            })
            .collect();
        Self {
            title: format!("{} {title}", icon::CLOCK_COUNTER_CLOCKWISE),
            lines,
            more: (lists.len() > SHOWN).then(|| format!("… and {} more", lists.len() - SHOWN)),
            migratable: lists.iter().any(|l| !l.malformed),
        }
    }
}
