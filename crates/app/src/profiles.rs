//! The Profiles page (decision 012): named sets of servers, used together in a project. A profile is kept in
//! Klefki's own folder; using one is an ordinary change, reviewed before anything is saved.
//! Asked of the engine each time the page is opened, and let go when it is left, like the Activity log. A profile
//! being made or edited is kept by the shell when the page is left ([`ProfilesPage::draft`]).
use crate::shell::folder_name;
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::words::{self, parent_folder};
use klefki_core::api::{Change, ConnectionId, Profile, Profiles, Scope, ServerRow, Snapshot};
use std::collections::HashMap;

/// A profile's servers named on its card; the rest are counted.
const NAMED: usize = 5;
/// Above this many projects, the project list has a box to type in.
const FILTER_FROM: usize = 10;
/// Why a profile cannot be made, changed or deleted while the file is not read in full.
const READ_ONLY: &str = "Not until the profiles file can be read in full (see above).";

#[derive(Default)]
pub(crate) struct ProfilesPage {
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
    /// The projects (as shown) where each profile's servers are the only ones on, by name. Worked out once for the
    /// profiles and the snapshot shown, not every frame.
    alone: Option<HashMap<String, Vec<String>>>,
    /// The projects (as shown) where each profile is in use: stopping it there would put back what it switched.
    /// Worked out with [`Self::alone`].
    in_use: Option<HashMap<String, Vec<(String, String)>>>,
    /// The profile whose "Use in" takes the keyboard when the cards are next drawn, asked for from the palette.
    /// Dropped once they are drawn, so focus never moves later on its own.
    reveal: Option<String>,
    /// New profile was asked for from the palette: the editor opens once the profiles are read, if they can be
    /// changed.
    new_asked: bool,
}

/// Using a profile would write nothing: what is already so, and what would still keep a server from loading.
#[derive(Clone)]
struct Unchanged {
    already: Vec<String>,
    warnings: Vec<String>,
    /// Other servers are on there: "Only these servers there" would turn them off.
    others_on: bool,
    /// Not drawn yet: brought into view when it is.
    fresh: bool,
}

/// A profile being made or edited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Draft {
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

#[derive(Clone)]
struct Choice {
    /// `None`: not chosen yet. `Some(None)`: every project.
    place: Option<Option<String>>,
    /// "Only these servers there". Ticked at first (the owner's choice, 2026-10-20): a profile's servers are usually on
    /// already, so using it otherwise mostly changes nothing.
    only: bool,
    /// Use profile… was pressed with no place chosen.
    asked: bool,
    /// The last Use… would have changed nothing: said on the card until where or how it is used changes.
    unchanged: Option<Unchanged>,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            place: None,
            only: true,
            asked: false,
            unchanged: None,
        }
    }
}

/// What the page's buttons asked the shell to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Asked {
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
    pub(crate) fn with_draft(draft: Option<Draft>) -> Self {
        Self {
            editing: draft.map(|d| Draft { focus: true, ..d }),
            ..Self::default()
        }
    }

    /// The profile being made or edited, to keep while the page is not shown.
    pub(crate) fn draft(&mut self) -> Option<Draft> {
        self.editing.take()
    }

    /// The profiles as the engine has them. After a save, the profile being made or edited is done; the first
    /// answer on opening the page leaves a draft carried over from before.
    pub(crate) fn set(&mut self, profiles: Profiles) {
        self.profiles = Some(profiles);
        self.alone = None;
        self.in_use = None;
        if std::mem::take(&mut self.saving) {
            self.editing = None;
        }
        self.confirm = None;
        self.refused = None;
    }

    /// Using `profile` would change nothing: said on its card, rather than in a review with nothing to save.
    /// `already` says what is already so; `warnings`, what still keeps a server from loading there; `others_on`, that
    /// other servers there are on, which "Only these servers there" would turn off.
    pub(crate) fn unchanged(
        &mut self,
        profile: &str,
        already: Vec<String>,
        warnings: Vec<String>,
        others_on: bool,
    ) {
        self.choices
            .entry(profile.to_owned())
            .or_default()
            .unchanged = Some(Unchanged {
            already,
            warnings,
            others_on,
            fresh: true,
        });
    }

    /// A new snapshot is shown: where each profile is on alone is worked out again, and what a Use found already so is
    /// let go.
    pub(crate) fn snapshot_changed(&mut self) {
        self.alone = None;
        self.in_use = None;
        // What a Use found already so was true of the files then: another change may have made it false since.
        for choice in self.choices.values_mut() {
            choice.unchanged = None;
        }
    }

    /// Starts a new profile once the profiles are read, unless one is being made or edited: that is not thrown away.
    /// Not while the file cannot be read in full, as the page's own New profile.
    pub(crate) fn start_new(&mut self) {
        if self.editing.is_none() {
            self.new_asked = true;
        }
    }

    /// Brings the profile `name` into view, with where to use it taking the keyboard. A filter that would hide it is
    /// cleared. Not while a profile is being made or edited, which fills the page: that is said instead.
    pub(crate) fn reveal(&mut self, name: String) {
        if self.editing.is_some() {
            self.refused = Some(format!(
                "Finish or cancel the profile you are editing first, then use {name}."
            ));
            return;
        }
        self.filter.clear();
        self.reveal = Some(name);
    }

    pub(crate) fn refused(&mut self, why: String) {
        self.saving = false;
        self.refused = Some(why);
    }

    /// Draws the page. `live` is false for sample data, which has no folder to keep profiles in.
    pub(crate) fn show(&mut self, ui: &mut Ui, snapshot: &Snapshot, live: bool) -> Option<Asked> {
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
                if std::mem::take(&mut self.new_asked) {
                    if editable {
                        self.editing = Some(Draft {
                            replaces: None,
                            name: String::new(),
                            members: Vec::new(),
                            focus: true,
                            problem: None,
                        });
                        self.refused = None;
                    } else {
                        self.refused = Some(
                            "No profile can be made until the profiles file can be read in full (see above)."
                                .to_owned(),
                        );
                    }
                }
                if let Some(problem) = &profiles.problem {
                    // "Still usable" only when something could be read to use.
                    let usable = if profiles.profiles.is_empty() {
                        ""
                    } else {
                        "; those read can still be used"
                    };
                    warn(
                        ui,
                        &format!(
                            "{problem} No profile can be made, changed or deleted until the file is fixed{usable}."
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
                let alone = self.alone.get_or_insert_with(|| {
                    profiles
                        .profiles
                        .iter()
                        .map(|p| {
                            let places = klefki_core::api::profile_alone_in(&p.members, snapshot);
                            let shown = places.iter().map(|place| shown_project(place, snapshot)).collect();
                            (p.name.clone(), shown)
                        })
                        .collect()
                });
                let in_use = self.in_use.get_or_insert_with(|| {
                    profiles
                        .profiles
                        .iter()
                        .map(|p| {
                            let places = klefki_core::api::profile_in_use(&p.name, snapshot)
                                .into_iter()
                                .map(|place| {
                                    let shown = shown_project(&place, snapshot);
                                    (place, shown)
                                })
                                .collect();
                            (p.name.clone(), places)
                        })
                        .collect()
                });
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
                    // Only when something is said, so drawing the cards copies nothing otherwise.
                    let before = choice
                        .unchanged
                        .is_some()
                        .then(|| (choice.place.clone(), choice.only));
                    let reveal = self.reveal.as_deref() == Some(profile.name.as_str());
                    if reveal {
                        self.reveal = None;
                    }
                    let out = card(
                        ui,
                        profile,
                        snapshot,
                        Context {
                            alone: alone.get(&profile.name).map_or(&[], Vec::as_slice),
                            in_use: in_use.get(&profile.name).map_or(&[], Vec::as_slice),
                            reveal,
                            editable,
                        },
                        choice,
                        &mut self.confirm,
                        &mut self.project_filter,
                    );
                    // What was said is about the place and way it was used: gone once either changes, or the card
                    // asks for something else.
                    if let Some((place, only)) = before
                        && (choice.place != place || choice.only != only || out.is_some())
                    {
                        choice.unchanged = None;
                    }
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
                // Asked for a profile that is not there (any more): not waited for.
                self.reveal = None;
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

/// What a card shows beyond the profile itself.
struct Context<'a> {
    /// The projects (as shown) where its servers are the only ones on.
    alone: &'a [String],
    /// The projects (as shown) where it is in use: stopping it there puts back what it switched.
    /// Each as `.claude.json` spells it, and as shown.
    in_use: &'a [(String, String)],
    /// Bring it into view, with where to use it taking the keyboard.
    reveal: bool,
    editable: bool,
}

/// One profile: its name, its servers, where to use it, and its buttons.
fn card(
    ui: &mut Ui,
    profile: &Profile,
    snapshot: &Snapshot,
    cx: Context,
    choice: &mut Choice,
    confirm: &mut Option<String>,
    project_filter: &mut String,
) -> Option<Card> {
    let editable = cx.editable;
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
            // Where the eye already is, under its servers: true of the files now, so it is not muted. A project where it
            // is both in use and on alone is said once.
            let in_use: Vec<String> = cx.in_use.iter().map(|(_, shown)| shown.clone()).collect();
            let (both, use_only): (Vec<String>, Vec<String>) =
                in_use.iter().cloned().partition(|p| cx.alone.contains(p));
            let alone_only: Vec<String> = cx
                .alone
                .iter()
                .filter(|p| !in_use.contains(p))
                .cloned()
                .collect();
            for (glyph, colour, text) in [
                (
                    icon::CHECK_CIRCLE,
                    pal.success,
                    (!both.is_empty()).then(|| {
                        format!("In use in {}, where only these servers are on.", named_few(&both))
                    }),
                ),
                (
                    icon::STACK,
                    pal.primary,
                    (!use_only.is_empty()).then(|| format!("In use in {}.", named_few(&use_only))),
                ),
                (
                    icon::CHECK_CIRCLE,
                    pal.success,
                    (!alone_only.is_empty())
                        .then(|| format!("Only these servers are on in {}.", named_few(&alone_only))),
                ),
            ] {
                if let Some(text) = text {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(glyph).color(colour));
                        ui.add(egui::Label::new(text).wrap());
                    });
                }
            }
            if !cx.in_use.is_empty() && !asking {
                let stop = |project: Option<String>| {
                    Card::Asked(Asked::Use(Box::new(Change::StopProfile {
                        name: profile.name.clone(),
                        project,
                        delete: false,
                    })))
                };
                const STOP_TIP: &str = "Put back what this profile switched there, unless it was switched again \
                                        since. You see every change before it is saved.";
                match cx.in_use {
                    [(_, one)] => {
                        if ui
                            .add(egui::Button::new(format!("Stop using in {one}…")).small())
                            .on_hover_text(STOP_TIP)
                            .clicked()
                        {
                            out = Some(stop(None));
                        }
                    }
                    // Where to stop it: one project, or all of them.
                    many => {
                        ui.menu_button("Stop using in…", |ui| {
                            for (place, shown) in many {
                                if ui.button(shown).on_hover_text(place.as_str()).clicked() {
                                    out = Some(stop(Some(place.clone())));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if ui
                                .button(format!("All {} projects", many.len()))
                                .clicked()
                            {
                                out = Some(stop(None));
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text(STOP_TIP);
                    }
                }
            }
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
                // In use: deleting it stops using it first, as leaving a profile puts back what it switched. Delete
                // only is there for keeping things as they are now.
                let text = if cx.in_use.is_empty() {
                    format!("Delete the profile {}? Its servers stay as they are.", profile.name)
                } else {
                    format!(
                        "Delete the profile {}? It is in use in {}. Stop using it to put back what it switched there, \
                         or delete only and keep things as they are now.",
                        profile.name,
                        named_few(&in_use)
                    )
                };
                ui.add(egui::Label::new(RichText::new(text).color(pal.warn)).wrap());
                ui.horizontal_wrapped(|ui| {
                    if !cx.in_use.is_empty() {
                        let stop = egui::Button::new(
                            RichText::new("Stop using and delete…").strong().color(pal.on_bright),
                        )
                        .fill(pal.primary);
                        if ui
                            .add(stop)
                            .on_hover_text("See what is put back before anything is saved.")
                            .clicked()
                        {
                            out = Some(Card::Asked(Asked::Use(Box::new(Change::StopProfile {
                                name: profile.name.clone(),
                                project: None,
                                delete: true,
                            }))));
                            *confirm = None;
                        }
                    }
                    let delete = egui::Button::new(
                        RichText::new(if cx.in_use.is_empty() {
                            "Delete profile"
                        } else {
                            "Delete only"
                        })
                        .strong()
                        .color(pal.on_bright),
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
                if cx.reveal {
                    combo.request_focus();
                    combo.scroll_to_me(Some(egui::Align::Center));
                }
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
            if let Some(u) = &mut choice.unchanged {
                // Said already, under its servers, when it is used alone where it is on alone.
                let covered = choice.only
                    && match &choice.place {
                        Some(Some(p)) => cx.alone.contains(&shown_project(p, snapshot)),
                        Some(None) => {
                            !snapshot.projects.is_empty() && cx.alone.len() == snapshot.projects.len()
                        }
                        None => false,
                    };
                said_unchanged(ui, u, covered);
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
        let parent = parent_folder(project);
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
            settle(draft);
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
                settle(draft);
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
                settle(draft);
            }
        }
    }
    if snapshot.servers.is_empty() {
        muted(ui, "No servers are configured yet.");
    }
    if save {
        let name = draft.name.trim();
        draft.problem = problem_of(draft);
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

/// What keeps the draft from being saved, first things first.
fn problem_of(draft: &Draft) -> Option<&'static str> {
    if draft.name.trim().is_empty() {
        Some("Give the profile a name.")
    } else if draft.members.is_empty() {
        Some("Tick at least one server.")
    } else {
        None
    }
}

/// What Save said goes as soon as it no longer holds, not on the next Save; nothing new is said before Save.
fn settle(draft: &mut Draft) {
    if draft.problem.is_some() && problem_of(draft) != draft.problem {
        draft.problem = None;
    }
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
    format!(
        "{}: {}",
        words::count(rows.len(), "server"),
        words::and_more(&named, rows.len())
    )
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
        format!("{name} ({})", folder_name(parent_folder(path)))
    } else {
        name.to_owned()
    }
}

/// A few projects by name, and how many more: "q, r and 2 more".
fn named_few(names: &[String]) -> String {
    const FEW: usize = 3;
    words::and_list_upto(names, FEW, FEW)
}

/// Next to the button that asked: nothing to save, what is already so (unless `covered`, said under its servers
/// already), and how to turn the other servers off when some are on. Not a warning: nothing went wrong. Read out by a
/// screen reader without moving the focus (WCAG 2.2, 4.1.3 Status Messages), and brought into view once.
fn said_unchanged(ui: &mut Ui, unchanged: &mut Unchanged, covered: bool) {
    let pal = theme::palette(ui.ctx());
    ui.add_space(6.0);
    let live = |ui: &Ui, id: egui::Id| {
        ui.ctx()
            .accesskit_node_builder(id, |node| node.set_live(egui::accesskit::Live::Polite));
    };
    let head = ui.add(
        egui::Label::new(
            RichText::new(format!("{} Nothing to change", icon::CHECK_CIRCLE))
                .strong()
                .color(pal.text),
        )
        .wrap(),
    );
    live(ui, head.id);
    if !covered {
        for line in &unchanged.already {
            let said = ui.add(egui::Label::new(line.as_str()).wrap());
            live(ui, said.id);
        }
    }
    if unchanged.others_on {
        muted(
            ui,
            "To turn the other servers there off, tick Only these servers there and use the profile again.",
        );
    }
    for warning in &unchanged.warnings {
        warn(ui, warning);
    }
    // The whole of it, not only its heading, which can sit at the bottom edge with the rest below (cross-review of
    // #60).
    if std::mem::take(&mut unchanged.fresh) {
        let block = egui::Rect::from_min_max(
            head.rect.min,
            egui::pos2(head.rect.max.x, ui.cursor().top()),
        );
        ui.scroll_to_rect(block, None);
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

#[cfg(test)]
mod tests {
    use super::named_few;

    #[test]
    fn a_few_projects_are_named_and_the_rest_counted() {
        let names = |n: usize| (0..n).map(|i| format!("p{i}")).collect::<Vec<_>>();
        assert_eq!(named_few(&names(1)), "p0");
        assert_eq!(named_few(&names(2)), "p0 and p1");
        assert_eq!(named_few(&names(3)), "p0, p1 and p2");
        assert_eq!(named_few(&names(4)), "p0, p1, p2 and 1 more");
        assert_eq!(named_few(&names(9)), "p0, p1, p2 and 6 more");
    }
}
