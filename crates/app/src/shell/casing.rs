//! The Fix casing dialog: servers whose names differ only in case, renamed to one spelling.

use super::{GOES_ALONG, ShellApp, closed_on_purpose, folder_name};
use crate::theme;
use egui::RichText;
use klefki_core::api::{Change, Scope, ServerId};

/// What the Fix casing dialog holds: the servers that differ only in case, and the spelling they are to get.
pub(super) struct CasingForm {
    members: Vec<CasingMember>,
    pub(super) keep: String,
}

struct CasingMember {
    id: ServerId,
    name: String,
    scope: Scope,
    project: Option<String>,
}

impl CasingForm {
    /// Each spelling once, in the order the servers are listed.
    fn spellings(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for m in &self.members {
            if !seen.contains(&m.name) {
                seen.push(m.name.clone());
            }
        }
        seen
    }

    /// The spellings of project servers: they live in a repository's `.mcp.json`, which Klefki does not edit.
    fn fixed(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for m in self.members.iter().filter(|m| m.scope == Scope::Project) {
            if !out.contains(&m.name.as_str()) {
                out.push(&m.name);
            }
        }
        out
    }

    /// Whether every server could get this spelling.
    fn allowed(&self, spelling: &str) -> bool {
        self.fixed().iter().all(|f| *f == spelling)
    }

    /// Whether any spelling can be given to all: not when project servers disagree among themselves.
    fn fixable(&self) -> bool {
        self.fixed().len() <= 1
    }

    /// Renames the servers that Klefki can rename and that do not have the spelling yet.
    fn changes(&self) -> Vec<Change> {
        self.members
            .iter()
            .filter(|m| m.scope != Scope::Project && m.name != self.keep)
            .map(|m| Change::Rename {
                id: m.id.clone(),
                name: self.keep.clone(),
            })
            .collect()
    }
}

impl ShellApp {
    /// Opens the Fix casing dialog for the selected server: every server that differs from it only in case.
    pub(super) fn open_casing(&mut self) {
        if self.core.is_none() || !self.can_prepare() {
            return;
        }
        let Some(row) = self.single_selected().filter(|r| r.casing_collision) else {
            return;
        };
        let lower = row.name.to_lowercase();
        let members: Vec<CasingMember> = self
            .snapshot
            .servers
            .iter()
            .filter(|r| r.casing_collision && r.name.to_lowercase() == lower)
            .map(|r| CasingMember {
                id: r.id(),
                name: r.name.clone(),
                scope: r.scope,
                project: r.project.clone(),
            })
            .collect();
        let mut form = CasingForm {
            members,
            keep: row.name.clone(),
        };
        // A project server cannot change its name, so its spelling is the one to keep.
        if let [only] = form.fixed()[..] {
            form.keep = only.to_owned();
        }
        self.casing = Some(form);
    }

    pub(super) fn casing_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let draft = self.drafted();
        let Some(form) = &mut self.casing else {
            return;
        };
        let ready = form.fixable() && !form.changes().is_empty();
        let mut review = ready && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("casing_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Fix casing");
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "These servers differ only in upper and lower case. Choose the spelling they all get.",
                    )
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            for m in &form.members {
                let place = match (&m.project, m.scope) {
                    (Some(p), _) => format!("{} \u{b7} {}", m.scope.label(), folder_name(p)),
                    (None, _) => m.scope.label().to_owned(),
                };
                ui.add(
                    egui::Label::new(format!("{}  \u{b7}  {place}", m.name))
                        .truncate()
                        .selectable(false),
                );
            }
            ui.add_space(10.0);
            let spellings = form.spellings();
            for spelling in spellings {
                let allowed = form.allowed(&spelling);
                let selected = form.keep == spelling;
                let response = ui
                    .add_enabled(
                        allowed,
                        egui::Button::selectable(selected, format!("Keep {spelling}")),
                    )
                    .on_disabled_hover_text(
                        "A server from a repository's .mcp.json keeps its name; the others follow it",
                    );
                if response.clicked() {
                    form.keep = spelling;
                }
            }
            ui.add_space(8.0);
            if form.fixable() {
                let n = form.changes().len();
                ui.add(
                    egui::Label::new(format!(
                        "Renames {n} {} to {}. A server keeps its definition and stays off where it was off.",
                        if n == 1 { "server" } else { "servers" },
                        form.keep
                    ))
                    .wrap(),
                );
                let renamed = form.changes().into_iter().any(
                    |c| matches!(c, Change::Rename { id, .. } if Some(&id) == draft.as_ref()),
                );
                if renamed {
                    ui.label(RichText::new(GOES_ALONG).small().color(pal.text_muted));
                }
            } else {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Servers from different repositories' .mcp.json files use different spellings, and Klefki does not edit those files. Change them there.",
                        )
                        .color(pal.danger),
                    )
                    .wrap(),
                );
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
                    .on_disabled_hover_text("Nothing to rename with this choice")
                    .clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
        });
        if review {
            let changes = form.changes();
            self.casing = None;
            self.prepare(changes);
        } else if cancel || closed_on_purpose(&modal) {
            self.casing = None;
        }
    }
}
