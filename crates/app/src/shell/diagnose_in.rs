//! The Status tab's "Diagnose in a project": a user server run from a project's folder, as a session started there
//! runs it, for when it works from the home folder, where Klefki checks it, and not in that project. The engine
//! refuses a project where Claude Code would not start it; the server's status stays the home folder's.

use super::ShellApp;
use crate::profiles::{project_choices, shown_project};
use crate::shell::folder_name;
use crate::theme;
use egui::{RichText, Ui};
use klefki_core::api::{
    Command, Diagnosis, LastStarts, Scope, ServerId, ServerRow, Snapshot, StartOutcome, Status,
    TransportKind, words,
};

pub(super) const HEADING: &str = "Diagnose in a project";
const ABOUT: &str = "Run it from a project's folder, as a session there does.";
const ABOUT_HOVER: &str = "Claude Code starts a user server in the folder each session starts in, and Klefki checks \
     it from your home folder. A relative path, a .env file and the like are looked for in that folder, so it can work \
     from one and not the other. Its status here stays the home folder's.";
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

/// Whether nothing ran: the engine refused, and says why in the note.
pub(super) fn refused(d: &Diagnosis) -> bool {
    d.status == Status::Unknown
        && d.note
            .as_deref()
            .is_some_and(|n| n.starts_with("not diagnosed"))
}

/// The projects to offer for `s`, by folder name; how many are left out because Claude Code does not start it there
/// (off, another server of its name, policy), and how many because the folder is gone or is a network folder.
fn places<'a>(s: &ServerRow, snapshot: &'a Snapshot) -> (Vec<&'a String>, usize, usize) {
    let held = |p: &String| s.blocked.as_ref().is_some_and(|b| b.projects.contains(p));
    let (mut offered, mut not_started, mut unusable) = (Vec::new(), 0, 0);
    for p in &snapshot.projects {
        if s.disabled_projects.contains(p) || s.shadowed_in.contains(p) || held(p) {
            not_started += 1;
        } else if snapshot.unusable_projects.contains(p) {
            unusable += 1;
        } else {
            offered.push(p);
        }
    }
    offered.sort_by_cached_key(|p| (folder_name(p).to_lowercase(), p.to_lowercase()));
    (offered, not_started, unusable)
}

/// The project to choose first: where Claude Code's newest failed start was, or else its newest start, when it is
/// offered; or else the first.
fn likeliest(places: &[&String], last: Option<&LastStarts>) -> String {
    let offered = |start: &&klefki_core::api::LastStart| {
        start
            .project
            .as_ref()
            .filter(|p| places.contains(p))
            .cloned()
    };
    let starts = last.map(|l| l.starts.as_slice()).unwrap_or_default();
    starts
        .iter()
        .filter(|s| matches!(s.outcome, StartOutcome::Failed { .. }))
        .find_map(|s| offered(&s))
        .or_else(|| starts.iter().find_map(|s| offered(&s)))
        .unwrap_or_else(|| places[0].clone())
}

/// "Not listed: 1 project where it is off or another server called x loads; 2 projects whose folder is gone or is
/// a network folder."
fn left_out(name: &str, not_started: usize, unusable: usize) -> Option<String> {
    let mut parts = Vec::new();
    if not_started > 0 {
        parts.push(format!(
            "{} where it is off or another server called {name} loads",
            words::count(not_started, "project")
        ));
    }
    if unusable > 0 {
        parts.push(format!(
            "{} whose folder is gone or is a network folder",
            words::count(unusable, "project")
        ));
    }
    (!parts.is_empty()).then(|| format!("Not listed: {}.", parts.join("; ")))
}

impl ShellApp {
    /// A diagnosis arrived: one run in a project ends the wait for it (one from the home folder can come first).
    pub(super) fn diagnosed_in(&mut self, id: &ServerId, d: &Diagnosis) {
        if d.folder.is_some() {
            self.diagnose_in.running.take_if(|(r, _)| r == id);
        }
    }

    /// Under the status and its explanation: what it is for, the project to run it in, the button and, once it ran,
    /// how it went, leading to the diagnosis below.
    pub(super) fn diagnose_in_section(&mut self, ui: &mut Ui, s: &ServerRow, can_check: bool) {
        // Not before Claude Code was started in a folder: there is nothing to choose.
        if self.core.is_none() || !offers(s) || self.snapshot.projects.is_empty() {
            return;
        }
        let pal = theme::palette(ui.ctx());
        let muted = |ui: &mut Ui, text: &str| {
            ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap())
        };
        ui.add_space(10.0);
        ui.separator();
        ui.label(
            RichText::new(HEADING)
                .family(theme::bold_family())
                .size(16.0),
        );
        muted(ui, ABOUT).on_hover_text(ABOUT_HOVER);
        let snapshot = self.snapshot.clone();
        let (places, not_started, unusable) = places(s, &snapshot);
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
            // The one chosen for this server while it is still offered, or else the likeliest.
            let chosen = self
                .diagnose_in
                .chosen
                .as_ref()
                .filter(|(c, p)| *c == id && places.contains(&p))
                .map(|(_, p)| p.clone())
                .unwrap_or_else(|| {
                    let last = self.last_starts.get(&id).and_then(|l| l.starts.as_ref());
                    likeliest(&places, last)
                });
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
                        picked = project_choices(
                            ui,
                            &places,
                            Some(&chosen),
                            &mut self.diagnose_in.filter,
                        );
                    });
                combo
                    .response
                    .labelled_by(label.id)
                    .on_hover_text(chosen.as_str());
                // Kept only once picked: until then the likeliest may change as the last starts arrive.
                if let Some(p) = &picked {
                    self.diagnose_in.chosen = Some((id.clone(), p.clone()));
                }
                let chosen = picked.unwrap_or(chosen);
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
                // How it went, by the button; the diagnosis itself is below the last starts.
                if running.is_none()
                    && let Some(d) = self.diagnoses.get(&id)
                    && let Some(folder) = &d.folder
                {
                    let outcome = if refused(d) {
                        "Not diagnosed: see below".to_owned()
                    } else {
                        format!("{} in {}: see below", d.status.label(), folder_name(folder))
                    };
                    if ui.link(outcome).clicked() {
                        self.reveal_diagnosis = Some(id.clone());
                    }
                }
            });
        }
        if let Some(project) = &running {
            let line = muted(ui, &format!("Diagnosing in {project}…"));
            ui.ctx().accesskit_node_builder(line.id, |node| {
                node.set_live(egui::accesskit::Live::Polite);
            });
        }
        if let Some(left_out) = left_out(&s.name, not_started, unusable) {
            muted(ui, &left_out);
        }
    }
}
