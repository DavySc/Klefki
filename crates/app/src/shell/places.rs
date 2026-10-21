//! Where a server goes, as ticked in the Add, Import, Duplicate and Move dialogs: for the user, and in any of the
//! projects Claude Code knows.

use super::{folder_name, list_in_words};
use crate::theme::{self, Palette};
use egui::{Align, Layout, RichText, Sense, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::words::parent_folder;
use klefki_core::api::{Scope, ServerRow, Target, same_project};
use std::collections::{BTreeSet, HashMap};

/// Where a new server goes, as ticked in a dialog: for the user, and in any of the projects Claude Code knows.
pub(super) struct Places {
    pub(super) user: bool,
    /// Every project Claude Code knows, in the snapshot's order, and whether the server goes there.
    pub(super) projects: Vec<(String, bool)>,
    /// The order the projects are listed in: `here` first, then by folder name.
    order: Vec<usize>,
    /// The project the dialog starts from (the selected server's), if any.
    here: Option<String>,
    /// Folder names (lower case) that more than one project has: only those rows also say where they live. The rest
    /// would all repeat the same few parent folders.
    shared_folders: BTreeSet<String>,
    /// Narrows the list of projects: part of a path, any case.
    pub(super) filter: String,
    /// Choose one place, like radio buttons, rather than tick any number: the Move dialog.
    pub(super) one: bool,
    /// Where the server is now, which cannot be chosen: the Move dialog.
    pub(super) now: Option<Target>,
}

impl Places {
    /// Nothing ticked yet.
    pub(super) fn new(projects: &[String], here: Option<&str>) -> Self {
        let projects: Vec<(String, bool)> = projects.iter().map(|p| (p.clone(), false)).collect();
        let mut order: Vec<usize> = (0..projects.len()).collect();
        order.sort_by_cached_key(|&i| {
            let path = &projects[i].0;
            (
                Some(path.as_str()) != here,
                folder_name(path).to_lowercase(),
            )
        });
        let mut seen = BTreeSet::new();
        let shared_folders = projects
            .iter()
            .map(|(p, _)| folder_name(p).to_lowercase())
            .filter(|f| !seen.insert(f.clone()))
            .collect();
        Self {
            user: false,
            projects,
            order,
            here: here.map(str::to_owned),
            shared_folders,
            filter: String::new(),
            one: false,
            now: None,
        }
    }

    /// Ticks the project the dialog starts from. False when there is none.
    pub(super) fn tick_here(&mut self) -> bool {
        let mut ticked = false;
        for (path, on) in &mut self.projects {
            if Some(path.as_str()) == self.here.as_deref() {
                *on = true;
                ticked = true;
            }
        }
        ticked
    }

    /// The projects ticked, in the snapshot's order.
    pub(super) fn ticked(&self) -> impl Iterator<Item = &str> {
        self.projects
            .iter()
            .filter(|(_, on)| *on)
            .map(|(p, _)| p.as_str())
    }

    pub(super) fn targets(&self) -> Vec<Target> {
        self.user
            .then_some(Target::User)
            .into_iter()
            .chain(self.ticked().map(|p| Target::Local(p.to_owned())))
            .collect()
    }

    /// Where it goes, in words: `for the user and in a and b`. None when nothing is ticked.
    pub(super) fn in_words(&self) -> Option<String> {
        let folders: Vec<&str> = self.ticked().map(folder_name).collect();
        places_in_words(self.user, &folders)
    }

    /// The one place chosen, as a move says it: `to the user`, `to app`. None when nothing is chosen.
    pub(super) fn one_in_words(&self) -> Option<String> {
        if self.user {
            return Some("to the user".to_owned());
        }
        self.ticked()
            .next()
            .map(|p| format!("to {}", folder_name(p)))
    }

    /// The ticked places that already have a server called `name`, in words. Only the lists a new server is written
    /// to count: the engine refuses those, while a name in a project's `.mcp.json` is only hidden by it, which the
    /// review warns about.
    pub(super) fn taken(&self, name: &str, servers: &[ServerRow]) -> Option<String> {
        Taken::new(self, servers.iter().filter(|s| s.name == name)).of(name)
    }

    /// The tick boxes: the user, then the projects in a list of at most `list_height`, searchable when long. `tag`
    /// marks the project the dialog starts from.
    /// Says whether a tick changed.
    pub(super) fn ui(&mut self, ui: &mut Ui, list_height: f32, tag: Option<&str>) -> bool {
        let pal = theme::palette(ui.ctx());
        const NOW: &str = "where it is now";
        let mark = if self.one { Mark::Radio } else { Mark::Tick };
        let user_now = self.now == Some(Target::User);
        let mut changed = place_mark(
            ui,
            &mut self.user,
            "For the user",
            "in every project",
            user_now.then_some(NOW),
            mark,
            !user_now,
        )
        .changed();
        if changed && self.one && self.user {
            for (_, on) in &mut self.projects {
                *on = false;
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let ticked = self.projects.iter().filter(|(_, on)| *on).count();
            // Local scope, as the list's groups say it; not the shared .mcp.json, which Klefki never writes.
            let mut heading = format!("In a project, just for you ({})", self.projects.len());
            if ticked > 0 && !self.one {
                heading.push_str(&format!(", {ticked} ticked"));
            }
            ui.label(RichText::new(heading).color(pal.text_muted));
            // A short list is read at a glance; a long one is searched.
            if self.projects.len() > 8 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text(format!("{} Filter projects", icon::MAGNIFYING_GLASS))
                            .desired_width(220.0),
                    );
                });
            }
        });
        // As tall as the whole list would be, up to that room, and no shorter while filtering: the dialog keeps
        // its size and its buttons stay where they are as you type.
        let rows = self.projects.len().max(1) as f32;
        let height = (rows * (PLACE_ROW + PLACE_GAP)).min(list_height);
        egui::ScrollArea::vertical()
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = PLACE_GAP;
                let needle = self.filter.trim().to_lowercase();
                let mut shown = 0;
                let mut picked = None;
                for &i in &self.order {
                    let (path, on) = &mut self.projects[i];
                    if !path.to_lowercase().contains(&needle) {
                        continue;
                    }
                    shown += 1;
                    let here = self.here.as_deref() == Some(path.as_str());
                    let folder = folder_name(path);
                    let parent = if self.shared_folders.contains(&folder.to_lowercase()) {
                        parent_folder(path)
                    } else {
                        ""
                    };
                    let now = matches!(&self.now, Some(Target::Local(p)) if same_project(p, path));
                    let tag = if now { Some(NOW) } else { tag.filter(|_| here) };
                    let row = place_mark(ui, on, folder, parent, tag, mark, !now)
                        .on_hover_text(path.as_str());
                    if row.changed() {
                        changed = true;
                        picked = Some(i);
                    }
                }
                // One place at a time: choosing a project un-chooses the rest.
                if let Some(picked) = picked.filter(|_| self.one) {
                    self.user = false;
                    for (i, (_, on)) in self.projects.iter_mut().enumerate() {
                        *on = i == picked;
                    }
                }
                if shown == 0 {
                    ui.label(RichText::new("No project matches.").color(pal.text_muted));
                }
            });
        changed
    }
}

/// Which ticked places already have a server of a given name, for many names at once.
pub(super) struct Taken<'a> {
    user: bool,
    /// The ticked projects, in order.
    ticked: Vec<&'a str>,
    /// By name: whether the user has one, and the ticked projects (in `ticked`) that have one of their own.
    names: HashMap<&'a str, (bool, Vec<usize>)>,
}

impl<'a> Taken<'a> {
    pub(super) fn new(places: &'a Places, servers: impl Iterator<Item = &'a ServerRow>) -> Self {
        let ticked: Vec<&str> = places.ticked().collect();
        let mut at: HashMap<_, Vec<usize>> = HashMap::new();
        for (i, project) in ticked.iter().enumerate() {
            at.entry(klefki_core::api::project_key(project))
                .or_default()
                .push(i);
        }
        let mut names: HashMap<&str, (bool, Vec<usize>)> = HashMap::new();
        for s in servers {
            match (s.scope, s.project.as_deref()) {
                (Scope::User, None) => names.entry(s.name.as_str()).or_default().0 = true,
                (Scope::Local, Some(project)) => {
                    if let Some(found) = at.get(&klefki_core::api::project_key(project)) {
                        names.entry(s.name.as_str()).or_default().1.extend(found);
                    }
                }
                _ => {}
            }
        }
        Self {
            user: places.user,
            ticked,
            names,
        }
    }

    /// The ticked places that already have a server called `name`, in words.
    pub(super) fn of(&self, name: &str) -> Option<String> {
        let (user, found) = self.names.get(name)?;
        let mut found = found.clone();
        found.sort_unstable();
        found.dedup();
        let folders: Vec<&str> = found.iter().map(|&i| folder_name(self.ticked[i])).collect();
        places_in_words(self.user && *user, &folders)
    }
}

/// `for the user`, `in a and b`, `for the user and in 5 projects`; None for nowhere.
pub(super) fn places_in_words(user: bool, folders: &[&str]) -> Option<String> {
    let projects =
        (!folders.is_empty()).then(|| format!("in {}", list_in_words(folders, "project")));
    match (user, projects) {
        (false, None) => None,
        (true, None) => Some("for the user".to_owned()),
        (true, Some(p)) => Some(format!("for the user and {p}")),
        (false, Some(p)) => Some(p),
    }
}

/// Height of a place in the Duplicate dialog, and the space between two.
const PLACE_ROW: f32 = 32.0;

const PLACE_GAP: f32 = 2.0;

/// One place in the Duplicate dialog: a tick box, a name and where it is, as wide as the list so the whole row can be
/// clicked. Drawn rather than laid out, so a long path is cut to fit instead of widening the dialog.
pub(super) fn place_row(
    ui: &mut Ui,
    on: &mut bool,
    title: &str,
    detail: &str,
    tag: Option<&str>,
) -> egui::Response {
    place_mark(ui, on, title, detail, tag, Mark::Tick, true)
}

/// How a place is chosen: a tick box among many, or a radio button for one.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mark {
    Tick,
    Radio,
}

/// A tick box, as the places a dialog lists have: filled when ticked, an outline when not.
fn paint_tick(painter: &egui::Painter, tick: egui::Rect, ticked: bool, pal: &Palette) {
    if ticked {
        painter.rect_filled(tick, 4.0, pal.primary);
        painter.text(
            tick.center(),
            egui::Align2::CENTER_CENTER,
            icon::CHECK,
            egui::FontId::proportional(13.0),
            pal.on_bright,
        );
    } else {
        painter.rect_stroke(
            tick,
            4.0,
            egui::Stroke::new(1.5, pal.text_muted),
            egui::StrokeKind::Inside,
        );
    }
}

/// A tick box on its own, beside a field: `label` is what it is read out as.
pub(super) fn tick_box(ui: &mut Ui, on: &mut bool, label: &str) -> egui::Response {
    let pal = theme::palette(ui.ctx());
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let ticked = *on;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, ticked, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        if response.hovered() || response.has_focus() {
            painter.rect_filled(rect, 6.0, pal.hover);
        }
        let tick = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0));
        paint_tick(&painter, tick, ticked, pal);
    }
    response.on_hover_text(label)
}

/// [`place_row`] drawn as `mark`; a row that is not `enabled` cannot be chosen and is drawn faded.
pub(super) fn place_mark(
    ui: &mut Ui,
    on: &mut bool,
    title: &str,
    detail: &str,
    tag: Option<&str>,
    mark: Mark,
    enabled: bool,
) -> egui::Response {
    let pal = theme::palette(ui.ctx());
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), PLACE_ROW), sense);
    if response.clicked() {
        // A radio button stays chosen when clicked again.
        let next = mark == Mark::Tick && *on;
        if *on != !next {
            *on = !next;
            response.mark_changed();
        }
    }
    let ticked = *on;
    response.widget_info(|| {
        let label = if detail.is_empty() {
            title.to_owned()
        } else {
            format!("{title}, {detail}")
        };
        let kind = match mark {
            Mark::Tick => egui::WidgetType::Checkbox,
            Mark::Radio => egui::WidgetType::RadioButton,
        };
        egui::WidgetInfo::selected(kind, enabled, ticked, label)
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    if enabled && (response.hovered() || response.has_focus()) {
        painter.rect_filled(rect, 6.0, pal.hover);
    }
    let tick = egui::Rect::from_center_size(
        rect.left_center() + egui::vec2(18.0, 0.0),
        egui::vec2(18.0, 18.0),
    );
    let text = if enabled { pal.text } else { pal.text_muted };
    if mark == Mark::Radio {
        let stroke = if ticked { pal.primary } else { pal.text_muted };
        // Where it is now cannot be chosen: faded, ring and all.
        let stroke = if enabled {
            stroke
        } else {
            stroke.gamma_multiply(0.4)
        };
        painter.circle_stroke(tick.center(), 8.25, egui::Stroke::new(1.5, stroke));
        if ticked {
            painter.circle_filled(tick.center(), 4.5, pal.primary);
        }
    } else {
        paint_tick(&painter, tick, ticked, pal);
    }
    let name = painter.text(
        egui::pos2(tick.right() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(15.0),
        text,
    );
    let small = egui::FontId::proportional(13.0);
    let mut right = rect.right() - 10.0;
    if let Some(tag) = tag {
        let t = painter.text(
            egui::pos2(right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            tag,
            small.clone(),
            pal.primary,
        );
        right = t.left() - 12.0;
    }
    let x = name.right() + 10.0;
    if !detail.is_empty() && right > x {
        let galley = ui.fonts_mut(|f| {
            let mut job =
                egui::text::LayoutJob::simple_singleline(detail.to_owned(), small, pal.text_muted);
            job.wrap = egui::text::TextWrapping::truncate_at_width(right - x);
            f.layout_job(job)
        });
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text_muted,
        );
    }
    response
}
