//! The Server sets page (decision 012): named groups of servers, used together in a project. A set is kept in
//! Klefki's own folder; using one is an ordinary change, reviewed before anything is saved.
//! Asked of the engine each time the page is opened, and let go when it is left, like the Activity log.
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Change, ConnectionId, Scope, ServerSet, Sets, Snapshot};
use std::collections::HashMap;

#[derive(Default)]
pub struct SetsPage {
    /// `None` until the engine answers.
    sets: Option<Sets>,
    /// What the top bar's filter box holds on this page: it filters the sets, and the servers of the one edited.
    pub filter: String,
    /// The set being made or edited.
    editing: Option<Editing>,
    /// The set whose Delete is waiting for a yes.
    confirm: Option<String>,
    /// Where each set is to be used, and whether alone, by name: kept while the page is shown.
    choices: HashMap<String, Choice>,
    /// Why the engine refused the last save or delete.
    refused: Option<String>,
}

struct Editing {
    /// The set's name before the edit; `None` for a new set.
    replaces: Option<String>,
    name: String,
    /// In the order they were ticked; a set's own order is kept.
    members: Vec<ConnectionId>,
    /// The name field takes the keyboard on the next frame.
    focus: bool,
}

#[derive(Default, Clone)]
struct Choice {
    /// `None`: not chosen yet. `Some(None)`: every project.
    place: Option<Option<String>>,
    only: bool,
}

/// What the page's buttons asked the shell to do.
#[derive(Debug, PartialEq)]
pub enum Asked {
    Save {
        name: String,
        members: Vec<ConnectionId>,
        replaces: Option<String>,
    },
    Delete(String),
    /// Prepare this change for review. Boxed: a change is large next to the other asks.
    Use(Box<Change>),
}

impl SetsPage {
    /// The sets as the engine has them: what was being made or deleted is done.
    pub fn set(&mut self, sets: Sets) {
        self.sets = Some(sets);
        self.editing = None;
        self.confirm = None;
        self.refused = None;
    }

    pub fn refused(&mut self, why: String) {
        self.refused = Some(why);
    }

    /// Draws the page. `live` is false for sample data, which has no folder to keep sets in.
    pub fn show(&mut self, ui: &mut Ui, snapshot: &Snapshot, live: bool) -> Option<Asked> {
        let pal = theme::palette(ui.ctx());
        let mut asked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Server sets").heading());
                ui.add_space(4.0);
                muted(
                    ui,
                    "Groups of servers you turn on together in a project, such as the ones for writing or for one \
                     client. Using a set changes only which servers are on; you see every change before it is \
                     saved.",
                );
                ui.add_space(12.0);
                if !live {
                    muted(ui, "Not available for sample data.");
                    return;
                }
                let Some(sets) = self.sets.as_ref() else {
                    muted(ui, "Loading…");
                    return;
                };
                if let Some(problem) = &sets.problem {
                    warn(ui, problem);
                    ui.add_space(8.0);
                }
                if let Some(editing) = &mut self.editing {
                    match editor(ui, editing, snapshot, &self.filter, self.refused.as_deref()) {
                        Some(Edited::Save(a)) => asked = Some(a),
                        Some(Edited::Cancel) => {
                            self.editing = None;
                            self.refused = None;
                        }
                        None => {}
                    }
                    return;
                }
                if let Some(why) = &self.refused {
                    warn(ui, why);
                    ui.add_space(8.0);
                }
                if ui
                    .button(format!("{} New set", icon::PLUS))
                    .on_hover_text("Pick the servers that belong together and give them a name.")
                    .clicked()
                {
                    self.editing = Some(Editing {
                        replaces: None,
                        name: String::new(),
                        members: Vec::new(),
                        focus: true,
                    });
                    self.refused = None;
                }
                ui.add_space(8.0);
                if sets.sets.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(icon::STACK).size(32.0).color(pal.text_muted));
                        ui.add_space(8.0);
                        ui.label("No sets yet");
                        ui.add_space(4.0);
                        muted(
                            ui,
                            "Make one to switch a project to just the servers it needs in one step.",
                        );
                    });
                    return;
                }
                let query = self.filter.trim().to_lowercase();
                let shown: Vec<&ServerSet> = sets
                    .sets
                    .iter()
                    .filter(|s| query.is_empty() || matches(s, snapshot, &query))
                    .collect();
                if shown.is_empty() {
                    muted(ui, &format!("No sets match \"{}\".", self.filter.trim()));
                }
                for (i, set) in shown.into_iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    let choice = self.choices.entry(set.name.clone()).or_default();
                    match card(ui, set, snapshot, choice, &mut self.confirm) {
                        Some(Card::Edit) => {
                            self.editing = Some(Editing {
                                replaces: Some(set.name.clone()),
                                name: set.name.clone(),
                                members: set.members.clone(),
                                focus: true,
                            });
                            self.refused = None;
                        }
                        Some(Card::Asked(a)) => asked = Some(a),
                        None => {}
                    }
                }
            });
        if asked.is_some() {
            self.refused = None;
        }
        asked
    }
}

enum Edited {
    Save(Asked),
    Cancel,
}

enum Card {
    Edit,
    Asked(Asked),
}

/// One set: its servers, where to use it, and its buttons.
fn card(
    ui: &mut Ui,
    set: &ServerSet,
    snapshot: &Snapshot,
    choice: &mut Choice,
    confirm: &mut Option<String>,
) -> Option<Card> {
    let pal = theme::palette(ui.ctx());
    let mut out = None;
    ui.add_space(6.0);
    let asking = confirm.as_deref() == Some(set.name.as_str());
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&set.name).strong().size(15.0));
        if !asking {
            if ui
                .small_button(format!("{} Edit", icon::PENCIL_SIMPLE))
                .on_hover_text("Rename it, or change its servers.")
                .clicked()
            {
                out = Some(Card::Edit);
            }
            if ui
                .small_button(format!("{} Delete", icon::TRASH))
                .on_hover_text("Forget this set. Klefki asks first.")
                .clicked()
            {
                *confirm = Some(set.name.clone());
            }
        }
    });
    let names: Vec<String> = set
        .members
        .iter()
        .map(|m| member_name(m, snapshot))
        .collect();
    ui.add(egui::Label::new(names.join(", ")).wrap());
    if asking {
        ui.indent(("confirm", set.name.as_str()), |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Delete the set {}? Its servers stay as they are.",
                        set.name
                    ))
                    .color(pal.warn),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                let delete =
                    egui::Button::new(RichText::new("Delete set").strong().color(pal.on_bright))
                        .fill(pal.danger);
                if ui.add(delete).clicked() {
                    out = Some(Card::Asked(Asked::Delete(set.name.clone())));
                    *confirm = None;
                }
                if ui.button("Cancel").clicked() {
                    *confirm = None;
                }
            });
        });
        return out;
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        let use_in = ui.label("Use in");
        let chosen = match &choice.place {
            None => "Choose a project".to_owned(),
            Some(None) => "Every project".to_owned(),
            Some(Some(p)) => folder_name(p).to_owned(),
        };
        let combo = egui::ComboBox::from_id_salt(("use-in", set.name.as_str()))
            .selected_text(chosen)
            // One width whatever is chosen, so the buttons after it stay where they were.
            .width(200.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(choice.place == Some(None), "Every project")
                    .on_hover_text("Each project Claude Code knows today.")
                    .clicked()
                {
                    choice.place = Some(None);
                }
                for project in &snapshot.projects {
                    let this = Some(Some(project.clone()));
                    if ui
                        .selectable_label(choice.place == this, project.as_str())
                        .clicked()
                    {
                        choice.place = this;
                    }
                }
            });
        let combo = combo.response.labelled_by(use_in.id);
        if let Some(Some(p)) = &choice.place {
            combo.on_hover_text(p.as_str());
        }
        ui.checkbox(&mut choice.only, "Turn off every other server there")
            .on_hover_text(format!(
                "So that only the servers of {} load there.",
                set.name
            ));
        let ready = choice.place.is_some();
        let review = ui
            .add_enabled(ready, egui::Button::new("Review…"))
            .on_hover_text("See what changes before anything is saved.")
            .on_disabled_hover_text("Choose where to use it first.");
        if review.clicked()
            && let Some(place) = &choice.place
        {
            out = Some(Card::Asked(Asked::Use(Box::new(Change::UseSet {
                name: set.name.clone(),
                members: set.members.clone(),
                project: place.clone(),
                only: choice.only,
            }))));
        }
    });
    if snapshot.projects.is_empty() {
        muted(
            ui,
            "Claude Code knows no projects yet. Start it in a folder once, and the folder is listed here.",
        );
    }
    out
}

/// Making or editing a set: its name, and a box for each server.
fn editor(
    ui: &mut Ui,
    editing: &mut Editing,
    snapshot: &Snapshot,
    filter: &str,
    refused: Option<&str>,
) -> Option<Edited> {
    let pal = theme::palette(ui.ctx());
    let mut asked = None;
    ui.label(
        RichText::new(if editing.replaces.is_some() {
            "Edit set"
        } else {
            "New set"
        })
        .strong(),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let label = ui.label("Set name");
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut editing.name)
                    .hint_text("such as Writing")
                    .char_limit(64)
                    .desired_width(240.0),
            )
            .labelled_by(label.id);
        // Typed into straight away: a new set starts with its name.
        if std::mem::take(&mut editing.focus) {
            field.request_focus();
        }
    });
    ui.add_space(8.0);
    ui.label(format!("Servers ({} ticked)", editing.members.len()));
    let query = filter.trim().to_lowercase();
    // As the Connections page groups them: by place, then by name.
    let mut rows: Vec<&klefki_core::api::ServerRow> = snapshot.servers.iter().collect();
    rows.sort_by_cached_key(|r| {
        (
            r.scope != Scope::User,
            place_of(r.scope, r.project.as_deref()),
            r.name.to_lowercase(),
        )
    });
    for row in rows {
        let shown = query.is_empty()
            || row.name.to_lowercase().contains(&query)
            || editing.members.contains(&row.connection);
        if !shown {
            continue;
        }
        let mut ticked = editing.members.contains(&row.connection);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut ticked, row.name.as_str()).changed() {
                toggle(&mut editing.members, &row.connection, ticked);
            }
            ui.label(
                RichText::new(place_of(row.scope, row.project.as_deref())).color(pal.text_muted),
            );
        });
    }
    // Members no longer configured: kept unless unticked, for they may come back.
    let gone: Vec<ConnectionId> = editing
        .members
        .iter()
        .filter(|m| !snapshot.servers.iter().any(|r| &r.connection == *m))
        .cloned()
        .collect();
    for id in gone {
        let mut ticked = true;
        if ui
            .checkbox(&mut ticked, member_name(&id, snapshot))
            .changed()
        {
            toggle(&mut editing.members, &id, ticked);
        }
    }
    if snapshot.servers.is_empty() {
        muted(ui, "No servers are configured yet.");
    }
    ui.add_space(8.0);
    if let Some(why) = refused {
        warn(ui, why);
    }
    let name = editing.name.trim();
    let why_not = if name.is_empty() {
        Some("Give the set a name.")
    } else if editing.members.is_empty() {
        Some("Tick at least one server.")
    } else {
        None
    };
    ui.horizontal(|ui| {
        let save = ui
            .add_enabled(why_not.is_none(), egui::Button::new("Save set"))
            .on_disabled_hover_text(why_not.unwrap_or_default());
        if save.clicked() {
            asked = Some(Edited::Save(Asked::Save {
                name: name.to_owned(),
                members: editing.members.clone(),
                replaces: editing.replaces.clone(),
            }));
        }
        if ui.button("Cancel").clicked() {
            asked = Some(Edited::Cancel);
        }
    });
    asked
}

fn toggle(members: &mut Vec<ConnectionId>, id: &ConnectionId, on: bool) {
    if on {
        if !members.contains(id) {
            members.push(id.clone());
        }
    } else {
        members.retain(|m| m != id);
    }
}

/// A member as listed: its name, with its project's folder for a local or project server, or why it is not found.
fn member_name(id: &ConnectionId, snapshot: &Snapshot) -> String {
    match snapshot.servers.iter().find(|r| &r.connection == id) {
        Some(row) => match (row.scope, row.project.as_deref()) {
            (Scope::Local | Scope::Project, Some(p)) => {
                format!("{} ({})", row.name, folder_name(p))
            }
            _ => row.name.clone(),
        },
        None => {
            format!("{} (no longer configured)", id.shown_name())
        }
    }
}

fn place_of(scope: Scope, project: Option<&str>) -> String {
    match project {
        Some(p) => format!("{} · {}", scope.label(), folder_name(p)),
        None => scope.label().to_owned(),
    }
}

/// Whether `set` has `query` (lower case) in its name or a server's.
fn matches(set: &ServerSet, snapshot: &Snapshot, query: &str) -> bool {
    set.name.to_lowercase().contains(query)
        || set
            .members
            .iter()
            .any(|m| member_name(m, snapshot).to_lowercase().contains(query))
}

fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[cut + 1..],
        _ => trimmed,
    }
}

fn muted(ui: &mut Ui, text: &str) {
    let pal = theme::palette(ui.ctx());
    ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
}

fn warn(ui: &mut Ui, text: &str) {
    let pal = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(RichText::new(format!("{} {text}", icon::WARNING)).color(pal.warn)).wrap(),
    );
}
