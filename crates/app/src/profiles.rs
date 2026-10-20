//! The Profiles page (decision 012): named sets of servers, used together in a project. A profile is kept in
//! Klefki's own folder; using one is an ordinary change, reviewed before anything is saved.
//! Asked of the engine each time the page is opened, and let go when it is left, like the Activity log. A profile
//! being made or edited is kept by the shell when the page is left ([`ProfilesPage::draft`]).
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Change, ConnectionId, Profile, Profiles, Scope, ServerRow, Snapshot};
use std::collections::HashMap;

/// A profile's servers named on its card; the rest are counted.
const NAMED: usize = 5;
/// Above this many projects, the project list has a box to type in.
const FILTER_FROM: usize = 10;
/// Why a profile cannot be made, changed or deleted while the file is not read in full.
const READ_ONLY: &str = "Not until the profiles file can be read in full (see above).";

#[derive(Default)]
pub struct ProfilesPage {
    /// `None` until the engine answers.
    profiles: Option<Profiles>,
    /// What the top bar's filter box holds on this page: it filters the profiles, and the servers of the one edited.
    pub filter: String,
    /// The profile being made or edited.
    editing: Option<Draft>,
    /// The profile whose Delete is waiting for a yes.
    confirm: Option<String>,
    /// Where each profile is to be used, and whether alone, by name: kept while the page is shown.
    choices: HashMap<String, Choice>,
    /// Why the engine refused the last save or delete.
    refused: Option<String>,
    /// Typed into the project list, to find a project among many.
    project_filter: String,
    /// A save was asked for: once the engine answers with the profiles, the editor's work is done.
    saving: bool,
}

/// A profile being made or edited.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    /// The profile's name before the edit; `None` for a new profile.
    replaces: Option<String>,
    name: String,
    /// In the order they were ticked; a profile's own order is kept.
    members: Vec<ConnectionId>,
    /// The name field takes the keyboard on the next frame.
    focus: bool,
    /// Save was pressed with this wrong: said next to the name.
    problem: Option<&'static str>,
}

#[derive(Default, Clone)]
struct Choice {
    /// `None`: not chosen yet. `Some(None)`: every project.
    place: Option<Option<String>>,
    only: bool,
    /// Use profile… was pressed with no place chosen.
    asked: bool,
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

enum Edited {
    Save(Asked),
    Cancel,
}

enum Card {
    Edit,
    Asked(Asked),
}

impl ProfilesPage {
    /// The page, with a profile being made or edited when the page was last left.
    pub fn with_draft(draft: Option<Draft>) -> Self {
        Self {
            editing: draft.map(|d| Draft { focus: true, ..d }),
            ..Self::default()
        }
    }

    /// The profile being made or edited, to keep while the page is not shown.
    pub fn draft(&mut self) -> Option<Draft> {
        self.editing.take()
    }

    /// The profiles as the engine has them. After a save, the profile being made or edited is done; the first
    /// answer on opening the page leaves a draft carried over from before.
    pub fn set(&mut self, profiles: Profiles) {
        self.profiles = Some(profiles);
        if std::mem::take(&mut self.saving) {
            self.editing = None;
        }
        self.confirm = None;
        self.refused = None;
    }

    pub fn refused(&mut self, why: String) {
        self.saving = false;
        self.refused = Some(why);
    }

    /// Draws the page. `live` is false for sample data, which has no folder to keep profiles in.
    pub fn show(&mut self, ui: &mut Ui, snapshot: &Snapshot, live: bool) -> Option<Asked> {
        let pal = theme::palette(ui.ctx());
        let mut asked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Profiles").heading());
                ui.add_space(2.0);
                muted(
                    ui,
                    "Named sets of servers to turn on together in a project. You see every change before it is \
                     saved.",
                );
                ui.add_space(10.0);
                if !live {
                    muted(ui, "Not available for sample data.");
                    return;
                }
                let Some(profiles) = self.profiles.as_ref() else {
                    muted(ui, "Loading…");
                    return;
                };
                // Said once, up front: Klefki changes nothing in a file it could not read in full.
                let editable = profiles.problem.is_none();
                if let Some(problem) = &profiles.problem {
                    warn(
                        ui,
                        &format!(
                            "{problem} No profile can be made, changed or deleted until the file is fixed; they \
                             can still be used."
                        ),
                    );
                    ui.add_space(8.0);
                }
                if let Some(draft) = &mut self.editing {
                    match editor(ui, draft, snapshot, &self.filter, self.refused.as_deref()) {
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
                let new = |replaces: Option<&Profile>| Draft {
                    replaces: replaces.map(|p| p.name.clone()),
                    name: replaces.map(|p| p.name.clone()).unwrap_or_default(),
                    members: replaces.map(|p| p.members.clone()).unwrap_or_default(),
                    focus: true,
                    problem: None,
                };
                if profiles.profiles.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(icon::STACK).size(32.0).color(pal.text_muted));
                        ui.add_space(8.0);
                        ui.label("No profiles yet");
                        ui.add_space(4.0);
                        muted(
                            ui,
                            "A Writing profile, say, with your notes and docs servers: use it in a project, and \
                             those servers are the ones on there.",
                        );
                        ui.add_space(10.0);
                        if ui
                            .add_enabled(
                                editable,
                                egui::Button::new(format!("{} New profile", icon::PLUS)),
                            )
                            .on_disabled_hover_text(READ_ONLY)
                            .clicked()
                        {
                            self.editing = Some(new(None));
                            self.refused = None;
                        }
                    });
                    return;
                }
                if ui
                    .add_enabled(
                        editable,
                        egui::Button::new(format!("{} New profile", icon::PLUS)),
                    )
                    .on_hover_text("Pick the servers that belong together and give them a name.")
                    .on_disabled_hover_text(READ_ONLY)
                    .clicked()
                {
                    self.editing = Some(new(None));
                    self.refused = None;
                }
                ui.add_space(8.0);
                let query = self.filter.trim().to_lowercase();
                let shown: Vec<&Profile> = profiles
                    .profiles
                    .iter()
                    .filter(|p| query.is_empty() || matches(p, snapshot, &query))
                    .collect();
                if shown.is_empty() {
                    muted(ui, &format!("No profiles match \"{}\".", self.filter.trim()));
                }
                for profile in shown {
                    let choice = self.choices.entry(profile.name.clone()).or_default();
                    let out = card(
                        ui,
                        profile,
                        snapshot,
                        choice,
                        &mut self.confirm,
                        &mut self.project_filter,
                        editable,
                    );
                    match out {
                        Some(Card::Edit) => {
                            self.editing = Some(new(Some(profile)));
                            self.refused = None;
                        }
                        Some(Card::Asked(a)) => asked = Some(a),
                        None => {}
                    }
                    ui.add_space(8.0);
                }
            });
        if asked.is_some() {
            self.refused = None;
        }
        if matches!(asked, Some(Asked::Save { .. })) {
            self.saving = true;
        }
        asked
    }
}

/// One profile: its name, its servers, where to use it, and its buttons.
fn card(
    ui: &mut Ui,
    profile: &Profile,
    snapshot: &Snapshot,
    choice: &mut Choice,
    confirm: &mut Option<String>,
    project_filter: &mut String,
    editable: bool,
) -> Option<Card> {
    let pal = theme::palette(ui.ctx());
    let mut out = None;
    egui::Frame::new()
        .fill(pal.panel_raised)
        .stroke(egui::Stroke::new(1.0, pal.divider))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let asking = confirm.as_deref() == Some(profile.name.as_str());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&profile.name).heading());
                if asking {
                    return;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            editable,
                            egui::Button::new(format!("{} Delete", icon::TRASH)).small(),
                        )
                        .on_hover_text("Forget this profile. Klefki asks first.")
                        .on_disabled_hover_text(READ_ONLY)
                        .clicked()
                    {
                        *confirm = Some(profile.name.clone());
                    }
                    if ui
                        .add_enabled(
                            editable,
                            egui::Button::new(format!("{} Edit", icon::PENCIL_SIMPLE)).small(),
                        )
                        .on_hover_text("Rename it, or change its servers.")
                        .on_disabled_hover_text(READ_ONLY)
                        .clicked()
                    {
                        out = Some(Card::Edit);
                    }
                });
            });
            ui.add(egui::Label::new(summary(profile, snapshot)).wrap());
            let gone = gone(profile, snapshot);
            if !gone.is_empty() {
                muted(
                    ui,
                    &format!(
                        "{} no longer configured: {}.",
                        gone.len(),
                        gone.join(", ")
                    ),
                );
            }
            if asking {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Delete the profile {}? Its servers stay as they are.",
                            profile.name
                        ))
                        .color(pal.warn),
                    )
                    .wrap(),
                );
                ui.horizontal(|ui| {
                    let delete = egui::Button::new(
                        RichText::new("Delete profile").strong().color(pal.on_bright),
                    )
                    .fill(pal.danger);
                    if ui.add(delete).clicked() {
                        out = Some(Card::Asked(Asked::Delete(profile.name.clone())));
                        *confirm = None;
                    }
                    if ui.button("Cancel").clicked() {
                        *confirm = None;
                    }
                });
                return;
            }
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                let use_in = ui.label("Use in");
                let combo = egui::ComboBox::from_id_salt(("use-in", profile.name.as_str()))
                    .selected_text(match &choice.place {
                        None => "Choose a project".to_owned(),
                        Some(None) => "Every project".to_owned(),
                        Some(Some(p)) => shown_project(p, snapshot),
                    })
                    // One width whatever is chosen, so what follows stays where it was.
                    .width(220.0)
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        if let Some(place) = places(ui, snapshot, choice, project_filter) {
                            choice.place = Some(place);
                            choice.asked = false;
                        }
                    });
                let combo = combo.response.labelled_by(use_in.id);
                if let Some(Some(p)) = &choice.place {
                    combo.on_hover_text(p.as_str());
                }
                theme::checkbox(ui, &mut choice.only, "Only these servers there").on_hover_text(
                    "Every other server in that project is turned off, so these are the only ones Claude Code \
                     loads there.",
                );
                let go = egui::Button::new(
                    RichText::new("Use profile…").strong().color(pal.on_bright),
                )
                .fill(pal.primary);
                if ui
                    .add(go)
                    .on_hover_text("See what changes before anything is saved.")
                    .clicked()
                {
                    match &choice.place {
                        Some(place) => {
                            out = Some(Card::Asked(Asked::Use(Box::new(Change::UseProfile {
                                name: profile.name.clone(),
                                members: profile.members.clone(),
                                project: place.clone(),
                                only: choice.only,
                            }))));
                        }
                        None => choice.asked = true,
                    }
                }
            });
            if choice.asked && choice.place.is_none() {
                warn(ui, "Choose a project to use it in first.");
            }
            if snapshot.projects.is_empty() {
                muted(
                    ui,
                    "Claude Code knows no projects yet. Start it in a folder once, and the folder is listed here.",
                );
            }
        });
    out
}

/// The project list: by folder name, with the folder it is in muted, then Every project. Typed into, when long.
/// Answers what was picked.
fn places(
    ui: &mut Ui,
    snapshot: &Snapshot,
    choice: &Choice,
    filter: &mut String,
) -> Option<Option<String>> {
    let pal = theme::palette(ui.ctx());
    let mut picked = None;
    if snapshot.projects.len() > FILTER_FROM {
        ui.add(
            egui::TextEdit::singleline(filter)
                .hint_text(format!("{} Type to find a project", icon::MAGNIFYING_GLASS))
                .desired_width(f32::INFINITY),
        )
        .request_focus();
    }
    let query = filter.trim().to_lowercase();
    let mut projects: Vec<&String> = snapshot
        .projects
        .iter()
        .filter(|p| query.is_empty() || p.to_lowercase().contains(&query))
        .collect();
    projects.sort_by_cached_key(|p| (folder_name(p).to_lowercase(), p.to_lowercase()));
    for project in projects {
        let mut job = egui::text::LayoutJob::default();
        let font = egui::TextStyle::Body.resolve(ui.style());
        job.append(
            folder_name(project),
            0.0,
            egui::TextFormat::simple(font.clone(), pal.text),
        );
        let parent = parent_of(project);
        if !parent.is_empty() {
            job.append(
                parent,
                10.0,
                egui::TextFormat::simple(
                    egui::TextStyle::Small.resolve(ui.style()),
                    pal.text_muted,
                ),
            );
        }
        let this = Some(project.clone());
        if ui
            .selectable_label(choice.place == Some(this.clone()), job)
            .on_hover_text(project.as_str())
            .clicked()
        {
            picked = Some(this);
        }
    }
    ui.separator();
    if ui
        .selectable_label(choice.place == Some(None), "Every project")
        .on_hover_text("Each project Claude Code knows today.")
        .clicked()
    {
        picked = Some(None);
    }
    picked
}

/// Making or editing a profile: its name and buttons first, so they stay in reach, then a box for each server,
/// grouped as on the Connections page.
fn editor(
    ui: &mut Ui,
    draft: &mut Draft,
    snapshot: &Snapshot,
    filter: &str,
    refused: Option<&str>,
) -> Option<Edited> {
    let pal = theme::palette(ui.ctx());
    let mut asked = None;
    ui.label(
        RichText::new(if draft.replaces.is_some() {
            "Edit profile"
        } else {
            "New profile"
        })
        .heading(),
    );
    ui.add_space(6.0);
    let mut save = false;
    ui.horizontal_wrapped(|ui| {
        let label = ui.label("Name");
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut draft.name)
                    .hint_text("such as Writing")
                    .char_limit(64)
                    .desired_width(240.0),
            )
            .labelled_by(label.id);
        if std::mem::take(&mut draft.focus) {
            field.request_focus();
        }
        if field.changed() {
            draft.problem = None;
        }
        save |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        save |= ui.button("Save profile").clicked();
        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            asked = Some(Edited::Cancel);
        }
    });
    if let Some(problem) = draft.problem {
        warn(ui, problem);
    } else if let Some(why) = refused {
        warn(ui, why);
    }
    ui.add_space(8.0);
    ui.label(RichText::new(format!("Servers ({} ticked)", draft.members.len())).strong());
    let query = filter.trim().to_lowercase();
    let mut rows: Vec<&ServerRow> = snapshot
        .servers
        .iter()
        .filter(|r| {
            query.is_empty()
                || r.name.to_lowercase().contains(&query)
                || draft.members.contains(&r.connection)
        })
        .collect();
    rows.sort_by_cached_key(|r| (r.scope != Scope::User, place_of(r), r.name.to_lowercase()));
    let mut group = None;
    for row in rows {
        let place = place_of(row);
        if group.as_ref() != Some(&place) {
            ui.add_space(6.0);
            ui.label(RichText::new(&place).color(pal.primary).strong())
                .on_hover_text(row.project.as_deref().unwrap_or(&place));
            group = Some(place);
        }
        let mut ticked = draft.members.contains(&row.connection);
        ui.horizontal(|ui| {
            if theme::checkbox(ui, &mut ticked, row.name.as_str()).changed() {
                toggle(&mut draft.members, &row.connection, ticked);
            }
            if let Some((badge, why)) = badge(row) {
                ui.label(RichText::new(badge).color(pal.text_muted))
                    .on_hover_text(why);
            }
        });
    }
    // Members no longer configured: kept unless unticked, for they may come back.
    let gone: Vec<ConnectionId> = draft
        .members
        .iter()
        .filter(|m| !snapshot.servers.iter().any(|r| &r.connection == *m))
        .cloned()
        .collect();
    if !gone.is_empty() {
        ui.add_space(6.0);
        ui.label(
            RichText::new("No longer configured")
                .color(pal.primary)
                .strong(),
        );
        for id in gone {
            let mut ticked = true;
            if theme::checkbox(ui, &mut ticked, id.shown_name()).changed() {
                toggle(&mut draft.members, &id, ticked);
            }
        }
    }
    if snapshot.servers.is_empty() {
        muted(ui, "No servers are configured yet.");
    }
    if save {
        let name = draft.name.trim();
        draft.problem = if name.is_empty() {
            Some("Give the profile a name.")
        } else if draft.members.is_empty() {
            Some("Tick at least one server.")
        } else {
            None
        };
        if draft.problem.is_none() {
            asked = Some(Edited::Save(Asked::Save {
                name: name.to_owned(),
                members: draft.members.clone(),
                replaces: draft.replaces.clone(),
            }));
        }
    }
    asked
}

/// What keeps a server from loading, in a word or two, with the whole reason; as the Connections page says it.
fn badge(row: &ServerRow) -> Option<(&'static str, String)> {
    if let Some(why) = &row.not_loaded {
        return Some(("not loaded", why.clone()));
    }
    if let Some(b) = &row.blocked {
        return Some((
            if b.policy {
                "blocked"
            } else {
                "connectors off"
            },
            format!("In {}: {}", b.projects.join(", "), b.why),
        ));
    }
    if row.shadowed {
        return Some((
            "replaced",
            "Another server of the same name loads in its place.".to_owned(),
        ));
    }
    if !row.shadowed_in.is_empty() {
        return Some((
            "replaced in some projects",
            format!(
                "Another server of the same name loads in its place in {}.",
                row.shadowed_in.join(", ")
            ),
        ));
    }
    if row.scope == Scope::Project && row.approval != Some(klefki_core::api::Approval::Approved) {
        return Some((
            "not approved",
            "Claude Code runs a project server only once it is approved.".to_owned(),
        ));
    }
    None
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

/// The servers of `profile` that are configured, by name, the first few, with where each is when its name is not
/// enough to tell it apart.
fn summary(profile: &Profile, snapshot: &Snapshot) -> String {
    let rows: Vec<&ServerRow> = profile
        .members
        .iter()
        .filter_map(|m| snapshot.servers.iter().find(|r| &r.connection == m))
        .collect();
    if rows.is_empty() {
        return "None of its servers is configured.".to_owned();
    }
    let named: Vec<String> = rows
        .iter()
        .take(NAMED)
        .map(|r| {
            let repeated = snapshot.servers.iter().filter(|o| o.name == r.name).count() > 1;
            if repeated {
                format!("{} ({})", r.name, place_of(r))
            } else {
                r.name.clone()
            }
        })
        .collect();
    let servers = if rows.len() == 1 { "server" } else { "servers" };
    match rows.len() - named.len() {
        0 => format!("{} {servers}: {}", rows.len(), named.join(", ")),
        more => format!(
            "{} {servers}: {} and {more} more",
            rows.len(),
            named.join(", ")
        ),
    }
}

/// The names of the servers of `profile` that are no longer configured.
fn gone(profile: &Profile, snapshot: &Snapshot) -> Vec<String> {
    profile
        .members
        .iter()
        .filter(|m| !snapshot.servers.iter().any(|r| &r.connection == *m))
        .map(ConnectionId::shown_name)
        .collect()
}

/// Where a server is, as the Connections page groups it.
fn place_of(row: &ServerRow) -> String {
    if let Some(p) = &row.plugin {
        return format!("{} · {}", row.scope.label(), p.plugin);
    }
    match &row.project {
        Some(p) => format!("{} · {}", row.scope.label(), folder_name(p)),
        None => row.scope.label().to_owned(),
    }
}

/// Whether `profile` has `query` (lower case) in its name or a server's.
fn matches(profile: &Profile, snapshot: &Snapshot, query: &str) -> bool {
    profile.name.to_lowercase().contains(query)
        || profile.members.iter().any(|m| {
            snapshot
                .servers
                .iter()
                .find(|r| &r.connection == m)
                .map_or_else(|| m.shown_name(), |r| r.name.clone())
                .to_lowercase()
                .contains(query)
        })
}

fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[cut + 1..],
        _ => trimmed,
    }
}

/// A project by its folder's name, with the folder that holds it when another project's folder has that name too.
fn shown_project(path: &str, snapshot: &Snapshot) -> String {
    let name = folder_name(path);
    let shared = snapshot
        .projects
        .iter()
        .filter(|p| folder_name(p) == name)
        .count()
        > 1;
    if shared {
        format!("{name} ({})", folder_name(parent_of(path)))
    } else {
        name.to_owned()
    }
}

/// The folder a project's folder is in; empty for a project at the top.
fn parent_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[..cut],
        _ => "",
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
