//! The detail panel: the selected server's name, tabs and what each tab shows, its status and diagnosis among them.

use super::editor::EditorState;
use super::list::{
    ABOUT_CONNECTORS, CONNECTOR_READ_ONLY, NOT_LOADED, PLUGIN_STATUS, in_projects_text,
    managed_read_only, not_checked, plugin_off_everywhere, plugin_on_text, plugin_read_only,
    projects_paths, relative_job, replaced_by_organisation, shadowed_paths, shadowed_text,
};
use super::{DetailTab, ShellApp, flat_items};
use crate::command::shortcut;
use crate::explain::Explanation;
use crate::sign_in;
use crate::theme;
use egui::{Align, Frame, Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Approval, Change, Command, Diagnosis, Scope, ServerRow, Status, TransportKind,
};
use std::sync::Arc;

/// Plain text a user can paste into a support request. The engine already sanitised and redacted it.
pub(super) fn diagnosis_report(row: &ServerRow, d: &Diagnosis) -> String {
    use std::fmt::Write;
    let mut out = String::from("Klefki diagnostic report\n");
    let place = match &row.project {
        Some(project) => format!("{}, {project}", row.scope.label()),
        None => row.scope.label().to_owned(),
    };
    let _ = writeln!(out, "server: {} ({place})", row.name);
    let _ = writeln!(out, "transport: {}", row.transport.label());
    let _ = match &d.detail {
        Some(detail) => writeln!(out, "status: {}: {detail}", d.status.label()),
        None => writeln!(out, "status: {}", d.status.label()),
    };
    if let Some(note) = &d.note {
        let _ = writeln!(out, "note: {note}");
    }
    if !d.hints.is_empty() {
        out.push_str("possible causes:\n");
        for hint in &d.hints {
            out.push('\n');
            out.push_str(&Explanation::for_hint(row, d.status, hint, d.detail.as_deref()).text());
        }
        out.push('\n');
    }
    if !d.server_said.is_empty() {
        out.push_str("the server said:\n");
        for line in &d.server_said {
            let _ = writeln!(out, "  {line}");
        }
    }
    if !d.client_log.is_empty() {
        out.push_str("client log:\n");
        for line in &d.client_log {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}

/// What the client's debug output said, in the order a person needs it: the likely cause, what the server
/// printed itself, and the raw log behind a collapsed header.
fn diagnosis_panel(ui: &mut Ui, row: &ServerRow, d: &Diagnosis) {
    let pal = theme::palette(ui.ctx());
    ui.add_space(10.0);
    ui.separator();
    if let Some(note) = &d.note {
        ui.label(RichText::new(note).color(pal.warn));
    }
    if !d.hints.is_empty() {
        ui.label(
            RichText::new("What probably went wrong")
                .family(theme::bold_family())
                .size(16.0),
        );
        for (i, hint) in d.hints.iter().enumerate() {
            Explanation::for_hint(row, d.status, hint, d.detail.as_deref())
                .show(ui, ui.id().with(("cause", i)));
            ui.add_space(6.0);
        }
    }
    if !d.server_said.is_empty() {
        ui.label(
            RichText::new("The server said")
                .family(theme::bold_family())
                .size(16.0),
        );
        for line in &d.server_said {
            ui.label(RichText::new(line).monospace());
        }
        ui.add_space(4.0);
    }
    if !d.client_log.is_empty() {
        egui::CollapsingHeader::new("Client log")
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::both().max_height(220.0).show(ui, |ui| {
                    for line in &d.client_log {
                        ui.add(egui::Label::new(RichText::new(line).monospace().small()).extend());
                    }
                });
            });
    }
    ui.add_space(6.0);
    if ui
        .button("Copy report")
        .on_hover_text("Copy this explanation as plain text. Secrets are already removed.")
        .clicked()
    {
        ui.ctx().copy_text(diagnosis_report(row, d));
    }
}

/// Room kept free at the right of the detail panel's content: its scroll bar floats over the content and, at its
/// widest when the pointer is on it, takes the clicks there.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a few pixels; `as` saturates for a float"
)]
pub(super) fn scroll_gutter(style: &egui::Style) -> i8 {
    let bar = &style.spacing.scroll;
    (bar.bar_width + bar.bar_outer_margin).ceil() as i8
}

/// Scrolls the scroll area `ui` is in to a widget in it that has just gained keyboard focus. egui does not, as GTK
/// and WinUI do, so Tab could take focus below the window. Focus gained anywhere else is left to its own place.
pub(super) fn bring_focus_into_view(ui: &Ui) {
    if let Some(r) = ui
        .memory(|m| m.focused())
        .and_then(|id| ui.ctx().read_response(id))
        && r.gained_focus()
        && ui.min_rect().contains_rect(r.rect)
    {
        ui.scroll_to_rect(r.rect, None);
    }
}

impl ShellApp {
    pub(super) fn detail(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let gutter = Margin {
            right: scroll_gutter(ui.style()),
            ..Margin::ZERO
        };
        let Some(&i) = self
            .selected
            .iter()
            .next()
            .filter(|_| self.selected.len() == 1)
        else {
            Frame::new().inner_margin(gutter).show(ui, |ui| {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    if self.selected.is_empty() {
                        ui.label(
                            RichText::new(icon::PLUGS_CONNECTED)
                                .size(28.0)
                                .color(pal.text_muted),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new("Select a connection").strong());
                    } else {
                        ui.label(
                            RichText::new(format!("{} connections selected", self.selected.len()))
                                .strong(),
                        );
                        ui.label(
                            RichText::new("Turn them on or off, or delete them, together.")
                                .color(pal.text_muted),
                        );
                    }
                    ui.add_space(12.0);
                    for (keys, what) in [
                        ("↑ ↓".to_owned(), "move through the list"),
                        (shortcut("K"), "all commands"),
                        (shortcut("F"), "filter"),
                        ("F5".to_owned(), "check every server again"),
                    ] {
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 240.0).max(0.0) / 2.0);
                            ui.label(RichText::new(keys).monospace().color(pal.text));
                            ui.label(RichText::new(what).color(pal.text_muted));
                        });
                    }
                });
            });
            return;
        };
        // A second handle to the snapshot (a reference count), so the row can be read while `self` is borrowed
        // mutably below, without copying its strings every frame.
        let snapshot = Arc::clone(&self.snapshot);
        let s = &snapshot.servers[i];
        if let Some(editor) = &mut self.editor
            && s.is(&editor.id)
        {
            editor.state.sync();
        }
        let (status, detail) = {
            let (status, detail) = self.effective(i);
            (status, detail.map(str::to_owned))
        };
        let explanation = match &self.explained {
            Some((key, built)) if *key == (i, self.live_gen) => built.clone(),
            _ => {
                let built = Explanation::for_status(s, status, detail.as_deref(), self.hint(i))
                    .map(Arc::new);
                self.explained = Some(((i, self.live_gen), built.clone()));
                built
            }
        };
        // The name and the tabs stay in view while what is under them scrolls.
        Frame::new().inner_margin(gutter).show(ui, |ui| {
            ui.label(RichText::new(&s.name).heading());
            ui.horizontal(|ui| {
                flat_items(ui);
                // Compact pills: smaller text and padding, so the tabs sit on one line and do not shout.
                ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
                ui.spacing_mut().item_spacing.x = 4.0;
                for (tab, title) in DetailTab::ALL {
                    // A plugin's or the organisation's server has no definition here to show values or sign-in
                    // settings from.
                    if s.scope.is_read_only()
                        && !matches!(tab, DetailTab::General | DetailTab::Status)
                    {
                        continue;
                    }
                    ui.selectable_value(&mut self.tab, tab, RichText::new(title).size(13.5));
                }
            });
            ui.separator();
        });
        // A diagnosis or an open form is often taller than the window. Each server and tab keeps a place of its
        // own, by identity: rows are kept by position, which another server added or removed above changes. The
        // save bar's room is left below, as tall as it was last frame.
        self.save_bar_wanted = false;
        egui::ScrollArea::vertical()
            .id_salt(("detail", &s.connection, self.tab))
            .auto_shrink([false, false])
            .max_height((ui.available_height() - self.save_bar_height).max(0.0))
            .content_margin(gutter)
            .show(ui, |ui| {
                self.detail_tab(ui, i, s, status, detail.as_deref(), explanation.as_deref());
                bring_focus_into_view(ui);
            });
        // Under the form rather than at its end, so it, and why saving is blocked, stay in view; drawn after the
        // form, so Tab reaches it last. A change in its height is laid out again before the frame is shown.
        let height = if self.save_bar_wanted {
            let bar = Frame::new().inner_margin(gutter).show(ui, |ui| {
                if !self.outcome_fits_beside(ui.ctx()) {
                    self.outcome_strip(ui);
                }
                ui.separator();
                self.save_bar(ui, s);
            });
            self.save_bar_shown = true;
            bar.response.rect.height() + ui.spacing().item_spacing.y
        } else {
            0.0
        };
        if (height - self.save_bar_height).abs() > 0.5 {
            self.save_bar_height = height;
            ui.ctx()
                .request_discard("the detail panel's save bar changed height");
        }
        // A definition asked for with Edit is brought into view until a pass laid out with its save bar is shown.
        if !ui.ctx().will_discard()
            && let Some(e) = self.editor.as_mut().filter(|e| s.is(&e.id))
            && !matches!(e.state, EditorState::Loading)
        {
            e.reveal = false;
        }
    }

    /// The detail panel's open tab for the row `i`, `s`, under the name and tabs.
    fn detail_tab(
        &mut self,
        ui: &mut Ui,
        i: usize,
        s: &ServerRow,
        status: Status,
        detail: Option<&str>,
        explanation: Option<&Explanation>,
    ) {
        let pal = theme::palette(ui.ctx());
        // Long values must wrap inside the panel; without a cap a grid cell grows to fit its text, and the whole
        // panel grows with it.
        let col_width = (ui.available_width() - 110.0).max(100.0);
        let tab = match self.tab {
            DetailTab::HeadersEnv | DetailTab::Auth if s.scope.is_read_only() => DetailTab::General,
            tab => tab,
        };
        match tab {
            DetailTab::General => {
                // First: Claude Code does not run it at all.
                if let Some(why) = &s.not_loaded {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Claude Code does not load this server. {why}"))
                                .color(pal.warn),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                }
                // Then, before anyone looks for a way to edit it.
                if let Some(file) = &s.managed {
                    ui.add(
                        egui::Label::new(
                            RichText::new(managed_read_only(file)).color(pal.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                }
                if let Some(p) = &s.plugin {
                    ui.add(
                        egui::Label::new(RichText::new(plugin_read_only(p)).color(pal.text_muted))
                            .wrap(),
                    );
                    ui.add_space(8.0);
                }
                // A local or project server that runs where a user server of its name is hidden.
                let replaces = s.scope != Scope::User
                    && !s.shadowed
                    && s.project.as_ref().is_some_and(|p| {
                        self.snapshot.servers.iter().any(|u| {
                            u.scope == Scope::User && u.name == s.name && u.shadowed_in.contains(p)
                        })
                    });
                egui::Grid::new("general")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .max_col_width(col_width)
                    .show(ui, |ui| {
                        let mut kv = |k: &str, v: String| {
                            ui.label(RichText::new(k).color(pal.text_muted));
                            let value = ui.add(egui::Label::new(v).wrap());
                            ui.end_row();
                            value
                        };
                        kv("Scope", s.scope.label().to_owned());
                        match &s.plugin {
                            Some(p) => {
                                kv("Plugin", format!("{}, from {}", p.plugin, p.marketplace));
                                if !p.off_in.is_empty() {
                                    kv("Plugin on", plugin_on_text(p, self.snapshot.project_count))
                                        .on_hover_text(format!("Off in:\n{}", p.off_in.join("\n")))
                                } else {
                                    kv("Plugin on", "in every project".to_owned())
                                }
                            }
                            None => match &s.managed {
                                Some(file) => kv("Set in", file.clone()),
                                None => {
                                    kv("Project", s.project.clone().unwrap_or_else(|| "-".into()))
                                }
                            },
                        };
                        kv("Type", s.transport.label().to_owned());
                        let target = kv("Target", s.target.clone());
                        // As Claude Code runs it; what the plugin writes is a hover away.
                        if let Some(written) =
                            s.plugin.as_ref().and_then(|p| p.written_target.as_deref())
                        {
                            target.on_hover_text(format!("The plugin writes: {written}"));
                        }
                        let enabled = if s.scope.is_everywhere() && s.enabled && s.disabled_in > 0 {
                            if s.disabled_projects.is_empty() {
                                format!(
                                    "disabled in {} of {} projects",
                                    s.disabled_in, self.snapshot.project_count
                                )
                            } else {
                                format!(
                                    "disabled {}",
                                    in_projects_text(
                                        &s.disabled_projects,
                                        self.snapshot.project_count,
                                        false
                                    )
                                )
                            }
                        } else if plugin_off_everywhere(s, self.snapshot.project_count) {
                            "yes, but its plugin is off in every project".to_owned()
                        } else if s.enabled {
                            "yes".to_owned()
                        } else {
                            "no".to_owned()
                        };
                        let enabled = kv("Enabled", enabled);
                        if !s.disabled_projects.is_empty() {
                            enabled.on_hover_text(projects_paths(&s.disabled_projects));
                        }
                        if let Some(approval) = s.approval {
                            kv("Approval", approval.label().to_owned());
                        }
                        // The organisation's servers win over every other of their name: never hidden, and what
                        // they hide is said where they are.
                        if s.scope == Scope::Managed {
                            let replaced = replaced_by_organisation(&self.snapshot.servers, s);
                            if !replaced.is_empty() {
                                kv("Replaces", replaced.join("; "));
                            }
                        } else {
                            let shadowed = kv(
                                if s.scope == Scope::Plugin {
                                    "Left out"
                                } else {
                                    "Shadowed"
                                },
                                shadowed_text(s, self.snapshot.project_count),
                            );
                            if !s.shadowed_in.is_empty() {
                                shadowed.on_hover_text(shadowed_paths(s));
                            }
                        }
                        if replaces {
                            kv("Replaces", format!("the user server {}", s.name));
                        }
                        // Everywhere, the reason leads the details already.
                        if let Some(b) = s.blocked.as_ref().filter(|_| s.not_loaded.is_none()) {
                            let count = self.snapshot.project_count;
                            kv(
                                if b.policy {
                                    "Blocked"
                                } else {
                                    "Connectors off"
                                },
                                format!(
                                    "{}. {}",
                                    in_projects_text(&b.projects, count, false),
                                    b.why
                                ),
                            )
                            .on_hover_text(projects_paths(&b.projects));
                        }
                        // What Klefki cannot check: said, as a note.
                        if let Some(why) = &s.may_not_load {
                            kv("May not load", why.clone());
                        }
                    });
                if s.scope == Scope::ClaudeAi {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(RichText::new(CONNECTOR_READ_ONLY).color(pal.text_muted))
                            .wrap(),
                    );
                    egui::CollapsingHeader::new("About claude.ai connectors")
                        .id_salt("about-connectors")
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(ABOUT_CONNECTORS).color(pal.text_muted),
                                )
                                .wrap(),
                            );
                        });
                }
                if s.casing_collision && self.core.is_some() {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Another server's name differs from this one only in upper and lower case.",
                            )
                            .color(pal.warn),
                        )
                        .wrap(),
                    );
                    if ui
                        .add_enabled(self.can_prepare(), egui::Button::new("Fix casing…"))
                        .on_hover_text("Give them all the same spelling")
                        .clicked()
                    {
                        self.open_casing();
                    }
                }
                if s.scope == Scope::User && !s.relative_paths.is_empty() {
                    ui.add_space(8.0);
                    // What to do first, with the paths as values; why under it, quieter.
                    ui.add(egui::Label::new(relative_job(ui, &s.relative_paths, pal.warn)).wrap());
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Claude Code starts user servers in each session's folder. Klefki checks them \
                                 from your home folder, so the check can pass here.",
                            )
                            .color(pal.text_muted),
                        )
                        .wrap(),
                    );
                }
                if s.scope == Scope::Project
                    && s.approval.is_some_and(|a| a != Approval::Approved)
                    && self.core.is_some()
                {
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(self.can_prepare(), egui::Button::new("Review and approve…"))
                        .on_hover_text(
                            "See everything this server would run, then let Claude Code run it",
                        )
                        .clicked()
                    {
                        self.prepare(vec![Change::Approve { id: s.id() }]);
                    }
                }
                // Where the user lands first: the way out of "Needs auth" is here too, not only on the Status tab.
                if status == Status::NeedsAuth && self.core.is_some() && sign_in::can_sign_in(s) {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new("This server needs you to sign in.").color(pal.warn),
                        )
                        .wrap(),
                    );
                    let button =
                        egui::Button::new(RichText::new("Sign in").strong().color(pal.on_bright))
                            .fill(pal.primary);
                    if ui
                        .add_enabled(self.sign_in.is_none(), button)
                        .on_hover_text("Sign in through Claude Code, in your browser")
                        .clicked()
                    {
                        self.start_sign_in(s);
                    }
                }
                if !s.scope.is_read_only() && self.core.is_some() {
                    ui.add_space(12.0);
                    let form = ui.scope(|ui| self.definition_form(ui, s)).response;
                    self.reveal_definition(s, &form);
                }
            }
            DetailTab::Status => {
                egui::Grid::new("status")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .max_col_width(col_width)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Status").color(pal.text_muted));
                        if s.not_loaded.is_some() {
                            ui.label(NOT_LOADED);
                        } else if s.scope.is_read_only() {
                            ui.label(PLUGIN_STATUS);
                        } else if self.last_known(i) {
                            ui.label(format!("{} (last known)", status.label()));
                        } else {
                            ui.label(status.label());
                        }
                        ui.end_row();
                        if explanation.is_none() {
                            ui.label(RichText::new("Detail").color(pal.text_muted));
                            ui.add(egui::Label::new(detail.unwrap_or("-")).wrap());
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                if s.scope == Scope::Project && s.transport == TransportKind::Stdio {
                    ui.label(
                        RichText::new(
                            "Checking runs this server's command from its repository, so it only happens when you ask.",
                        )
                        .color(pal.warn),
                    );
                }
                if let Some(why) = not_checked(s) {
                    ui.add(egui::Label::new(RichText::new(why).color(pal.text_muted)).wrap());
                    return;
                }
                let can_check = self.core.is_some() && status != Status::Checking;
                ui.horizontal_wrapped(|ui| {
                if self.core.is_some() && sign_in::offers_sign_in(s, status) {
                    // The way out of "Needs auth", so it leads; otherwise it is one action among others.
                    let button = if status == Status::NeedsAuth {
                        egui::Button::new(RichText::new("Sign in").strong().color(pal.on_bright)).fill(pal.primary)
                    } else {
                        egui::Button::new("Sign in")
                    };
                    if ui
                        .add_enabled(self.sign_in.is_none(), button)
                        .on_hover_text("Sign in through Claude Code, in your browser")
                        .clicked()
                    {
                        self.start_sign_in(s);
                    }
                }
                if ui
                    .add_enabled(can_check, egui::Button::new("Check now"))
                    .on_hover_text("Check the status again")
                    .clicked()
                {
                    self.send(Command::Check(s.id()));
                }
                if ui
                    .add_enabled(can_check, egui::Button::new("Diagnose"))
                    .on_hover_text(
                        "Run the check again with the client's own debug output and explain what went wrong",
                    )
                    .clicked()
                {
                    self.send(Command::Diagnose(s.id()));
                }
                });
                // Under the actions, so they are in view without scrolling, even at the smallest window.
                if let Some(explanation) = &explanation {
                    ui.add_space(10.0);
                    explanation.show(ui, ui.id().with("status"));
                }
                if let Some(d) = self.diagnoses.get(&s.id()) {
                    let shown = ui.scope(|ui| diagnosis_panel(ui, s, d)).response;
                    if self.reveal_diagnosis.take_if(|id| s.is(id)).is_some() {
                        shown.scroll_to_me(Some(Align::TOP));
                    }
                }
            }
            DetailTab::Auth => {
                let muted = |ui: &mut Ui, text: &str| {
                    ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
                };
                match &s.oauth {
                    Some(oauth) => {
                        egui::Grid::new("oauth")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .max_col_width(col_width)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Signs in").color(pal.text_muted));
                                ui.label("with OAuth");
                                ui.end_row();
                                ui.label(RichText::new("Client id").color(pal.text_muted));
                                match &oauth.client_id {
                                    Some(id) => ui.add(
                                        egui::Label::new(RichText::new(id).monospace()).wrap(),
                                    ),
                                    None => ui.label(
                                        RichText::new("none: the client registers itself")
                                            .color(pal.text_muted),
                                    ),
                                };
                                ui.end_row();
                                ui.label(RichText::new("Scopes").color(pal.text_muted));
                                if oauth.scopes.is_empty() {
                                    ui.label(RichText::new("none listed").color(pal.text_muted));
                                } else {
                                    ui.add(egui::Label::new(oauth.scopes.join(", ")).wrap());
                                }
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        if self.core.is_some() {
                            self.client_secret(ui, s);
                            ui.add_space(8.0);
                        }
                    }
                    None if sign_in::can_sign_in(s) => muted(
                        ui,
                        "Claude Code signs in to this server for you when the server asks for it.",
                    ),
                    None => muted(
                        ui,
                        "This server does not sign in with OAuth. Headers and tokens are in the Env tab.",
                    ),
                }
                if self.core.is_some() && sign_in::can_sign_in(s) {
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(self.sign_in.is_none(), egui::Button::new("Sign in"))
                        .on_hover_text("Sign in through Claude Code, in your browser")
                        .clicked()
                    {
                        self.start_sign_in(s);
                    }
                    muted(ui, "Klefki starts the sign-in and never sees it.");
                }
            }
            DetailTab::HeadersEnv => {
                if self.core.is_some() {
                    let form = ui.scope(|ui| self.values_form(ui, s)).response;
                    self.reveal_definition(s, &form);
                } else {
                    ui.label(RichText::new("Not available for sample data.").color(pal.text_muted));
                }
            }
        }
    }
}
