//! The Status tab's "Diagnose in a project": a user server run from a project's folder, as a session started there
//! runs it, for when it works from the home folder, where Klefki checks it, and not in that project. The engine
//! refuses a project where Claude Code would not start it; the server's status stays the home folder's.

use super::ShellApp;
use crate::profiles::{project_choices, shown_project};
use crate::shell::folder_name;
use crate::theme;
use egui::{RichText, Ui};
use klefki_core::api::{Command, Scope, ServerId, ServerRow, Snapshot, TransportKind, words};

pub(super) const HEADING: &str = "Diagnose in a project";
const ABOUT: &str = "Claude Code starts this server in each session's folder; Klefki checks it from your home \
     folder. Run it from a project's folder to see what a session there sees.";
pub(super) const DIAGNOSE: &str = "Diagnose there";
const NONE: &str = "Claude Code knows no project where it starts this server.";

/// The project chosen for the server shown, what is typed to find one, and the diagnosis still running.
#[derive(Default)]
pub(super) struct DiagnoseIn {
    chosen: Option<(ServerId, String)>,
    filter: String,
    running: Option<(ServerId, String)>,
}

/// Whether `s` is started in each session's folder: a user server that runs a command.
pub(super) fn offers(s: &ServerRow) -> bool {
    s.scope == Scope::User && s.transport == TransportKind::Stdio
}

/// The projects where Claude Code starts `s`, by folder name, and how many it leaves out: off there, another server
/// of its name in its place, or held back by policy.
fn places<'a>(s: &ServerRow, snapshot: &'a Snapshot) -> (Vec<&'a String>, usize) {
    let held = |p: &String| s.blocked.as_ref().is_some_and(|b| b.projects.contains(p));
    let (mut on, off): (Vec<&String>, Vec<&String>) = snapshot
        .projects
        .iter()
        .partition(|p| !s.disabled_projects.contains(p) && !s.shadowed_in.contains(p) && !held(p));
    on.sort_by_cached_key(|p| (folder_name(p).to_lowercase(), p.to_lowercase()));
    (on, off.len())
}

impl ShellApp {
    /// A diagnosis arrived: the one asked for here is no longer running.
    pub(super) fn diagnosed_in(&mut self, id: &ServerId) {
        self.diagnose_in.running.take_if(|(r, _)| r == id);
    }

    /// Under the status and its explanation: what it is for, the project to run it in and the button.
    pub(super) fn diagnose_in_section(&mut self, ui: &mut Ui, s: &ServerRow, can_check: bool) {
        // Not before Claude Code was started in a folder: there is nothing to choose.
        if self.core.is_none() || !offers(s) || self.snapshot.projects.is_empty() {
            return;
        }
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
        let snapshot = self.snapshot.clone();
        let (places, left_out) = places(s, &snapshot);
        let id = s.id();
        let running = self
            .diagnose_in
            .running
            .as_ref()
            .filter(|(r, _)| *r == id)
            .map(|(_, p)| p.clone());
        if places.is_empty() {
            muted(ui, NONE);
        } else {
            // The one chosen for this server while it is still listed, or else the first.
            let chosen = self
                .diagnose_in
                .chosen
                .as_ref()
                .filter(|(c, p)| *c == id && places.contains(&p))
                .map_or_else(|| places[0].clone(), |(_, p)| p.clone());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Project");
                let mut picked = None;
                let combo = egui::ComboBox::from_id_salt(("diagnose-in", s.connection.as_str()))
                    .selected_text(shown_project(&chosen, &snapshot))
                    // One width whatever is chosen, so the button stays where it was.
                    .width(220.0)
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        picked = project_choices(ui, &places, Some(&chosen), &mut self.diagnose_in.filter);
                    });
                combo.response.labelled_by(label.id).on_hover_text(chosen.as_str());
                if let Some(p) = picked {
                    self.diagnose_in.chosen = Some((id.clone(), p));
                }
                let chosen = self
                    .diagnose_in
                    .chosen
                    .as_ref()
                    .filter(|(c, _)| *c == id)
                    .map_or(chosen, |(_, p)| p.clone());
                if ui
                    .add_enabled(can_check && running.is_none(), egui::Button::new(DIAGNOSE))
                    .on_hover_text(format!(
                        "Start it with Claude Code from {chosen}, as a session there would, and explain what went \
                         wrong. Its status here stays the home folder's."
                    ))
                    .clicked()
                {
                    self.diagnose_in.running = Some((id.clone(), chosen.clone()));
                    self.send(Command::DiagnoseIn {
                        id: id.clone(),
                        project: chosen,
                    });
                }
            });
        }
        if let Some(project) = &running {
            let line = ui.add(
                egui::Label::new(
                    RichText::new(format!("Diagnosing in {project}…")).color(pal.text_muted),
                )
                .wrap(),
            );
            ui.ctx().accesskit_node_builder(line.id, |node| {
                node.set_live(egui::accesskit::Live::Polite)
            });
        }
        if left_out > 0 {
            muted(
                ui,
                &format!(
                    "Not listed: {} where it is off or another server called {} loads.",
                    words::count(left_out, "project"),
                    s.name
                ),
            );
        }
    }
}
