//! The desktop-style shell: left nav, top bar, icon toolbar, grid and detail panel.
//! Pure UI over an immutable `Snapshot`; no IO.
use crate::theme::{self, Palette, ThemeChoice};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Approval, Change, Command, Core, Diagnosis, Event, PlanView, Scope, ServerId, ServerRow,
    Snapshot, Status, TransportKind,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Connections,
    ScopeDrift,
    Secrets,
    Settings,
    ActivityLog,
}

impl Page {
    const ALL: [(Page, &'static str, &'static str); 5] = [
        (Page::Connections, icon::PLUGS_CONNECTED, "Connections"),
        (Page::ScopeDrift, icon::GIT_DIFF, "Scope drift"),
        (Page::Secrets, icon::KEY, "Secrets"),
        (Page::Settings, icon::GEAR, "Settings"),
        (Page::ActivityLog, icon::LIST_BULLETS, "Activity log"),
    ];

    pub fn title(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(p, ..)| *p == self)
            .map_or("", |(.., t)| t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    General,
    Auth,
    HeadersEnv,
    RawJson,
    Status,
}

impl DetailTab {
    const ALL: [(DetailTab, &'static str); 5] = [
        (DetailTab::General, "General"),
        (DetailTab::Auth, "Auth"),
        (DetailTab::HeadersEnv, "Headers/Env"),
        (DetailTab::RawJson, "Raw JSON"),
        (DetailTab::Status, "Status"),
    ];
}

/// What happened to the last change, for the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    text: String,
    ok: bool,
}

enum GridRow {
    Group(String),
    Server(usize),
}

pub struct ShellApp {
    snapshot: Arc<Snapshot>,
    /// The engine. `None` for demo data and tests that feed the UI directly.
    core: Option<Core>,
    /// Live results from the engine, which take precedence over the snapshot's hints. One slot per row of the
    /// snapshot, by position, so drawing a row looks its status up without building a key.
    live: Vec<Option<(Status, Option<String>)>>,
    /// The grid's rows for the current snapshot and search, built when either changes rather than every frame.
    grid_cache: Option<(String, Arc<[GridRow]>)>,
    /// The latest explanation per server, from the Diagnose action.
    diagnoses: HashMap<ServerId, Diagnosis>,
    cli_version: Option<String>,
    cli_problem: Option<String>,
    /// A change the engine has prepared and the user has not yet confirmed.
    plan: Option<PlanView>,
    /// Apply was pressed and the engine has not answered yet.
    applying: bool,
    /// The outcome of the last change, shown until dismissed.
    outcome: Option<Outcome>,
    pub page: Page,
    pub search: String,
    pub selected: BTreeSet<usize>,
    pub tab: DetailTab,
    pub theme: ThemeChoice,
}

impl ShellApp {
    pub fn new(snapshot: Arc<Snapshot>, theme: ThemeChoice) -> Self {
        Self {
            core: None,
            live: vec![None; snapshot.servers.len()],
            grid_cache: None,
            diagnoses: HashMap::new(),
            cli_version: None,
            cli_problem: None,
            plan: None,
            applying: false,
            outcome: None,
            snapshot,
            page: Page::Connections,
            search: String::new(),
            selected: BTreeSet::new(),
            tab: DetailTab::General,
            theme,
        }
    }

    /// The app driven by the engine: the snapshot and every status arrive as events.
    pub fn with_core(core: Core, theme: ThemeChoice) -> Self {
        let mut app = Self::new(Arc::new(Snapshot::default()), theme);
        app.core = Some(core);
        app
    }

    fn send(&self, command: Command) {
        if let Some(core) = &self.core {
            core.send(command);
        }
    }

    /// Re-reads the configuration and re-checks every server.
    fn refresh(&self) {
        self.send(Command::Refresh { live_all: true });
    }

    fn poll_core(&mut self) {
        let mut events = Vec::new();
        if let Some(core) = &self.core {
            while let Some(event) = core.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            self.apply_event(event);
        }
    }

    fn apply_event(&mut self, event: Event) {
        match event {
            Event::Snapshot(snapshot) => {
                // Rows are kept by position; carry selection, live results and diagnoses over by identity.
                let new_index: HashMap<(Scope, Option<&str>, &str), usize> = snapshot
                    .servers
                    .iter()
                    .enumerate()
                    .map(|(i, r)| ((r.scope, r.project.as_deref(), r.name.as_str()), i))
                    .collect();
                let moved = |old: usize| {
                    let r = &self.snapshot.servers[old];
                    new_index
                        .get(&(r.scope, r.project.as_deref(), r.name.as_str()))
                        .copied()
                };
                let selected = self.selected.iter().filter_map(|&i| moved(i)).collect();
                let mut live = vec![None; snapshot.servers.len()];
                for (old, result) in self.live.iter_mut().enumerate() {
                    // A check that was running is cut off by a refresh.
                    if let (Some(new), Some(r)) = (moved(old), result.take())
                        && r.0 != Status::Checking
                    {
                        live[new] = Some(r);
                    }
                }
                self.diagnoses.retain(|id, _| {
                    new_index.contains_key(&(id.scope, id.project.as_deref(), id.name.as_str()))
                });
                self.selected = selected;
                self.live = live;
                self.grid_cache = None;
                if snapshot.cli_version.is_some() {
                    self.cli_version = snapshot.cli_version.clone();
                }
                self.snapshot = snapshot;
            }
            Event::Diagnosis { id, diagnosis } => {
                if self.snapshot.servers.iter().any(|r| r.is(&id)) {
                    self.diagnoses.insert(id, diagnosis);
                }
            }
            Event::PlanReady(plan) => {
                self.applying = false;
                self.plan = Some(plan);
            }
            Event::PlanFailed(why) => {
                self.outcome = Some(Outcome {
                    text: why,
                    ok: false,
                })
            }
            Event::Applied {
                plan,
                changed,
                backup,
            } => {
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.plan = None;
                    self.applying = false;
                }
                self.outcome = Some(Outcome {
                    text: match (changed, backup) {
                        (false, _) => "Nothing needed to change.".to_owned(),
                        (true, Some(b)) => format!("Saved. The previous file is kept in {b}"),
                        (true, None) => "Saved.".to_owned(),
                    },
                    ok: true,
                });
            }
            Event::ApplyFailed { plan, error } => {
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.plan = None;
                    self.applying = false;
                }
                self.outcome = Some(Outcome {
                    text: error,
                    ok: false,
                });
            }
            Event::Reapplied { .. } => {
                self.outcome = Some(Outcome {
                    text: "Claude Code overwrote the change right after it was saved, so it was written again. Close running Claude Code sessions if this keeps happening."
                        .to_owned(),
                    ok: false,
                });
            }
            Event::CliVersion(version) => self.cli_version = version,
            Event::CliProblem(problem) => self.cli_problem = Some(problem),
            Event::Status { id, status, detail } => {
                if let Some(i) = self.snapshot.servers.iter().position(|r| r.is(&id)) {
                    self.live[i] = Some((status, detail));
                }
            }
        }
    }

    /// What to show for row `i`: the engine's latest answer, else what Claude's own cache said.
    fn effective(&self, i: usize) -> (Status, Option<&str>) {
        match self.live.get(i).and_then(Option::as_ref) {
            Some((status, detail)) => (*status, detail.as_deref()),
            None => (self.snapshot.servers[i].status, None),
        }
    }

    /// Indices into the snapshot matching the search, ordered by scope, project and name.
    pub fn visible(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        let mut idx: Vec<usize> = (0..self.snapshot.servers.len())
            .filter(|&i| {
                let s = &self.snapshot.servers[i];
                q.is_empty()
                    || s.name.to_lowercase().contains(&q)
                    || s.target.to_lowercase().contains(&q)
                    || s.project
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(&q))
            })
            .collect();
        // Each name is lower-cased once, not once per comparison.
        idx.sort_by_cached_key(|&i| {
            let s = &self.snapshot.servers[i];
            (s.scope, s.project.as_deref(), s.name.to_lowercase())
        });
        idx
    }

    fn grid_rows(&self) -> Vec<GridRow> {
        let mut rows = Vec::new();
        let mut last: Option<(Scope, Option<&str>)> = None;
        for i in self.visible() {
            let s = &self.snapshot.servers[i];
            let key = (s.scope, s.project.as_deref());
            if last != Some(key) {
                rows.push(GridRow::Group(group_title(s)));
                last = Some(key);
            }
            rows.push(GridRow::Server(i));
        }
        rows
    }

    /// The grid's rows, rebuilt only when the snapshot or the search changed.
    fn cached_grid_rows(&mut self) -> Arc<[GridRow]> {
        match &self.grid_cache {
            Some((search, rows)) if *search == self.search => Arc::clone(rows),
            _ => {
                let rows: Arc<[GridRow]> = self.grid_rows().into();
                self.grid_cache = Some((self.search.clone(), Arc::clone(&rows)));
                rows
            }
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        self.poll_core();
        if ui.input(|i| i.key_pressed(egui::Key::F5)) {
            self.refresh();
        }
        let pal = theme::palette(ui.ctx());
        egui::Panel::top("top_bar")
            .frame(bar_frame(pal))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(220.0)
            .frame(
                Frame::new()
                    .fill(pal.chrome)
                    .inner_margin(Margin::symmetric(0, 10)),
            )
            .show(ui, |ui| self.nav(ui));
        if self.page == Page::Connections {
            egui::Panel::right("detail")
                .default_size(340.0)
                .min_size(260.0)
                .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(12)))
                .show(ui, |ui| self.detail(ui));
        }
        let ctx = ui.ctx().clone();
        self.plan_dialog(&ctx);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(14)))
            .show(ui, |ui| match self.page {
                Page::Connections => {
                    self.warnings(ui);
                    self.outcome_banner(ui);
                    self.toolbar(ui);
                    ui.add_space(6.0);
                    self.grid(ui);
                }
                other => {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(other.title()).heading());
                        ui.label(
                            RichText::new("Not built yet (see docs/PLAN.md).")
                                .color(pal.text_muted),
                        );
                    });
                }
            });
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(icon::KEY).size(22.0).color(pal.key));
            ui.label(
                RichText::new("klefki")
                    .family(theme::bold_family())
                    .size(20.0)
                    .color(pal.primary),
            );
            ui.label(RichText::new(self.page.title()).color(pal.text_muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(icon::ARROWS_CLOCKWISE)
                    .on_hover_text("Refresh and re-check all (F5)")
                    .clicked()
                {
                    self.refresh();
                }
                let mut changed = false;
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(format!("{} {}", self.theme.icon(), self.theme.label()))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        flat_items(ui);
                        for choice in ThemeChoice::ALL {
                            let text = format!("{} {}", choice.icon(), choice.label());
                            changed |= ui.selectable_value(&mut self.theme, choice, text).changed();
                        }
                    })
                    .response
                    .on_hover_text("Theme");
                if changed {
                    theme::set_choice(ui.ctx(), self.theme);
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(format!("{} Search", icon::MAGNIFYING_GLASS))
                        .desired_width(220.0),
                );
                if let Some(version) = self
                    .cli_version
                    .as_deref()
                    .or(self.snapshot.cli_version.as_deref())
                {
                    ui.label(RichText::new(format!("Claude CLI {version}")).color(pal.text_muted));
                }
            });
        });
    }

    fn nav(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        for (page, glyph, title) in Page::ALL {
            let active = self.page == page;
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, title)
            });
            if active {
                ui.painter().rect_filled(rect, 0.0, pal.panel);
                let bar = egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height()));
                ui.painter().rect_filled(bar, 0.0, pal.primary);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, pal.row_alt);
            }
            let color = if active { pal.text } else { pal.text_muted };
            let glyph_color = if page == Page::Secrets {
                pal.key
            } else if active {
                pal.primary
            } else {
                color
            };
            ui.painter().text(
                rect.left_center() + egui::vec2(18.0, 0.0),
                egui::Align2::LEFT_CENTER,
                glyph,
                egui::FontId::proportional(20.0),
                glyph_color,
            );
            ui.painter().text(
                rect.left_center() + egui::vec2(52.0, 0.0),
                egui::Align2::LEFT_CENTER,
                title,
                egui::FontId::proportional(16.0),
                color,
            );
            if resp.clicked() {
                self.page = page;
            }
        }
    }

    /// Problems found while reading Claude's configuration, or a hint when nothing is configured.
    fn warnings(&self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        const SHOWN: usize = 4;
        let warnings: Vec<&str> = self
            .snapshot
            .warnings
            .iter()
            .map(String::as_str)
            .chain(self.cli_problem.as_deref())
            .collect();
        if warnings.is_empty() {
            return;
        }
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for w in warnings.iter().take(SHOWN) {
                    ui.label(RichText::new(format!("{} {w}", icon::WARNING)).color(pal.warn));
                }
                if warnings.len() > SHOWN {
                    ui.label(
                        RichText::new(format!("… and {} more", warnings.len() - SHOWN))
                            .color(pal.text_muted),
                    );
                }
            });
        ui.add_space(6.0);
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        const ACTIONS: [(&str, &str); 11] = [
            (icon::PLUS, "Add (Ctrl+N)"),
            (icon::PENCIL_SIMPLE, "Edit (Ctrl+E)"),
            (icon::COPY, "Duplicate (Ctrl+D)"),
            (icon::SWAP, "Move / copy"),
            (icon::TOGGLE_RIGHT, "Enable"),
            (icon::TOGGLE_LEFT, "Disable"),
            (icon::TRASH, "Delete (Del)"),
            (icon::SIGN_IN, "Re-authorize"),
            (icon::GIT_DIFF, "Refresh scopes"),
            (icon::EXPORT, "Export"),
            (icon::DOWNLOAD_SIMPLE, "Import"),
        ];
        let can_toggle = self.core.is_some() && !self.selected.is_empty() && self.plan.is_none();
        ui.horizontal(|ui| {
            for (glyph, tip) in ACTIONS {
                let toggle = match glyph {
                    g if g == icon::TOGGLE_RIGHT => Some(true),
                    g if g == icon::TOGGLE_LEFT => Some(false),
                    _ => None,
                };
                // The other operations arrive later in M1; they are shown disabled so the layout can be judged.
                let button = egui::Button::new(RichText::new(glyph).size(20.0))
                    .min_size(egui::vec2(44.0, 38.0));
                match toggle {
                    Some(enabled) => {
                        if ui
                            .add_enabled(can_toggle, button)
                            .on_hover_text(tip)
                            .on_disabled_hover_text(format!("{tip}: select a server first"))
                            .clicked()
                        {
                            self.prepare_toggle(enabled);
                        }
                    }
                    None => {
                        ui.add_enabled(false, button).on_disabled_hover_text(tip);
                    }
                }
            }
            ui.separator();
            ui.label(
                RichText::new(format!("{} selected", self.selected.len())).color(pal.text_muted),
            );
        });
    }

    /// Asks the engine what switching the selected servers on or off would change. Nothing is written yet.
    fn prepare_toggle(&mut self, enabled: bool) {
        let changes: Vec<Change> = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .map(|row| Change::SetEnabled {
                id: row.id(),
                enabled,
            })
            .collect();
        if !changes.is_empty() {
            self.outcome = None;
            self.send(Command::Prepare(changes));
        }
    }

    /// The banner for the last change.
    fn outcome_banner(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let Some(outcome) = self.outcome.clone() else {
            return;
        };
        let color = if outcome.ok { pal.success } else { pal.danger };
        let mut dismiss = false;
        Frame::new()
            .stroke(egui::Stroke::new(1.0, color))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&outcome.text).color(color));
                    if ui.button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
        ui.add_space(6.0);
        if dismiss {
            self.outcome = None;
        }
    }

    /// The confirmation step: shows exactly what would change, then applies or drops it.
    fn plan_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let applying = self.applying;
        let mut apply = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("plan_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Review the change");
            ui.add_space(6.0);
            if plan.nothing_to_do {
                ui.label("The configuration already looks like this. Nothing would be written.");
            } else {
                ui.label("These lines in your Claude Code configuration will change:");
            }
            for review in &plan.reviews {
                ui.add_space(4.0);
                Frame::new()
                    .stroke(egui::Stroke::new(1.0, pal.warn))
                    .corner_radius(6)
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&review.title).strong().color(pal.warn));
                        for line in &review.lines {
                            ui.add(egui::Label::new(line).wrap());
                        }
                    });
            }
            for file in &plan.files {
                if file.lines.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(RichText::new(&file.path).color(pal.text_muted));
                Frame::new()
                    .fill(pal.field)
                    .corner_radius(6)
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .max_height(260.0)
                            .show(ui, |ui| {
                                for line in &file.lines {
                                    let color = if line.starts_with("+ ") {
                                        pal.success
                                    } else if line.starts_with("- ") {
                                        pal.danger
                                    } else {
                                        pal.text_muted
                                    };
                                    ui.add(
                                        egui::Label::new(RichText::new(line).color(color)).wrap(),
                                    );
                                }
                            });
                    });
            }
            for warning in &plan.warnings {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} {warning}", icon::WARNING)).color(pal.warn),
                    )
                    .wrap(),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !plan.nothing_to_do && !applying,
                        egui::Button::new(if plan.reviews.is_empty() {
                            "Apply"
                        } else {
                            "Approve"
                        }),
                    )
                    .clicked()
                {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if apply {
            self.applying = true;
            self.send(Command::Apply(plan.id));
            // The dialog stays until the engine answers, so a second click cannot apply twice.
        } else if cancel {
            self.send(Command::Discard(plan.id));
            self.plan = None;
            self.applying = false;
        }
    }

    fn grid(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let rows = self.cached_grid_rows();
        let ctrl = ui.input(|i| i.modifiers.command);
        let project_count = self.snapshot.project_count;
        let mut clicked = None;
        TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(240.0).at_least(120.0))
            .column(Column::exact(84.0))
            .column(Column::exact(150.0))
            .column(Column::remainder().clip(true))
            .header(32.0, |mut h| {
                for t in ["Name", "Type", "Status", "Target"] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).strong().color(pal.text_muted));
                    });
                }
            })
            .body(|body| {
                body.rows(36.0, rows.len(), |mut row| match &rows[row.index()] {
                    GridRow::Group(title) => {
                        row.col(|ui| {
                            // Not clipped to the column: the title may run across the empty cells of the group row.
                            ui.add(
                                egui::Label::new(RichText::new(title).strong().color(pal.primary))
                                    .extend(),
                            );
                        });
                        for _ in 0..3 {
                            row.col(|_| {});
                        }
                    }
                    GridRow::Server(i) => {
                        let s = &self.snapshot.servers[*i];
                        row.set_selected(self.selected.contains(i));
                        row.col(|ui| name_cell(ui, s, project_count));
                        row.col(|ui| {
                            ui.label(RichText::new(s.transport.label()).color(pal.text_muted));
                        });
                        let (status, detail) = self.effective(*i);
                        row.col(|ui| status_cell(ui, s, status, detail));
                        row.col(|ui| {
                            ui.label(RichText::new(&s.target).monospace().color(pal.text_muted));
                        });
                        if row.response().clicked() {
                            clicked = Some(*i);
                        }
                    }
                });
            });
        if let Some(i) = clicked {
            if ctrl {
                if !self.selected.remove(&i) {
                    self.selected.insert(i);
                }
            } else {
                self.selected.clear();
                self.selected.insert(i);
            }
        }
    }

    fn detail(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        // Long values must wrap inside the panel; without a cap a grid cell grows to fit its text, and the whole
        // panel grows with it.
        let col_width = (ui.available_width() - 110.0).max(100.0);
        let Some(&i) = self
            .selected
            .iter()
            .next()
            .filter(|_| self.selected.len() == 1)
        else {
            let msg = if self.selected.is_empty() {
                "Select a connection"
            } else {
                "Multiple connections selected"
            };
            ui.label(RichText::new(msg).color(pal.text_muted));
            return;
        };
        // A second handle to the snapshot (a reference count), so the row can be read while `self` is borrowed
        // mutably below, without copying its strings every frame.
        let snapshot = Arc::clone(&self.snapshot);
        let s = &snapshot.servers[i];
        let (status, detail) = {
            let (status, detail) = self.effective(i);
            (status, detail.map(str::to_owned))
        };
        ui.label(RichText::new(&s.name).heading());
        ui.horizontal(|ui| {
            flat_items(ui);
            for (tab, title) in DetailTab::ALL {
                ui.selectable_value(&mut self.tab, tab, title);
            }
        });
        ui.separator();
        match self.tab {
            DetailTab::General => {
                egui::Grid::new("general")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .max_col_width(col_width)
                    .show(ui, |ui| {
                        let mut kv = |k: &str, v: String| {
                            ui.label(RichText::new(k).color(pal.text_muted));
                            ui.add(egui::Label::new(v).wrap());
                            ui.end_row();
                        };
                        kv("Scope", s.scope.label().to_owned());
                        kv("Project", s.project.clone().unwrap_or_else(|| "-".into()));
                        kv("Type", s.transport.label().to_owned());
                        kv("Target", s.target.clone());
                        let enabled = if s.scope == Scope::User && s.enabled && s.disabled_in > 0 {
                            format!(
                                "disabled in {} of {} projects",
                                s.disabled_in, self.snapshot.project_count
                            )
                        } else if s.enabled {
                            "yes".to_owned()
                        } else {
                            "no".to_owned()
                        };
                        kv("Enabled", enabled);
                        if let Some(approval) = s.approval {
                            kv("Approval", approval.label().to_owned());
                        }
                        kv("Shadowed", if s.shadowed { "yes" } else { "no" }.to_owned());
                    });
                if s.scope == Scope::Project
                    && s.approval.is_some_and(|a| a != Approval::Approved)
                    && self.core.is_some()
                {
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(
                            self.plan.is_none(),
                            egui::Button::new("Review and approve…"),
                        )
                        .on_hover_text(
                            "See everything this server would run, then let Claude Code run it",
                        )
                        .clicked()
                    {
                        self.outcome = None;
                        self.send(Command::Prepare(vec![Change::Approve { id: s.id() }]));
                    }
                }
            }
            DetailTab::Status => {
                egui::Grid::new("status")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .max_col_width(col_width)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Status").color(pal.text_muted));
                        ui.label(status.label());
                        ui.end_row();
                        ui.label(RichText::new("Detail").color(pal.text_muted));
                        ui.add(egui::Label::new(detail.as_deref().unwrap_or("-")).wrap());
                        ui.end_row();
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
                let can_check = self.core.is_some() && status != Status::Checking;
                ui.horizontal(|ui| {
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
                if let Some(d) = self.diagnoses.get(&s.id()) {
                    diagnosis_panel(ui, s, d);
                }
            }
            _ => {
                ui.label(RichText::new("Available from M1.").color(pal.text_muted));
            }
        }
    }
}

impl eframe::App for ShellApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        theme::save_choice(storage, self.theme);
    }
}

/// Selectable items (tabs, list options) are unframed until hovered or selected. egui lays an unframed
/// item out with the stroke width subtracted from its padding, then adds the stroke when the frame
/// appears, so any stroke makes the item grow on hover. Without strokes they only change colour.
fn flat_items(ui: &mut Ui) {
    let w = &mut ui.visuals_mut().widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.bg_stroke = egui::Stroke::NONE;
    }
}

/// Plain text a user can paste into a support request. The engine already sanitised and redacted it.
fn diagnosis_report(row: &ServerRow, d: &Diagnosis) -> String {
    use std::fmt::Write;
    let mut out = String::from("klefki diagnostic report\n");
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
            let _ = writeln!(out, "  - {}: {}", hint.title, hint.advice);
        }
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
        for hint in &d.hints {
            ui.label(
                RichText::new(&hint.title)
                    .family(theme::bold_family())
                    .color(pal.primary),
            );
            ui.label(RichText::new(&hint.advice).color(pal.text_muted));
            ui.add_space(4.0);
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

fn bar_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.panel_raised)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .inner_margin(Margin::symmetric(12, 6))
}

fn group_title(s: &ServerRow) -> String {
    match (&s.scope, &s.project) {
        (Scope::User, _) => "User".to_owned(),
        (scope, Some(p)) => format!("{} · {p}", scope.label()),
        (scope, None) => scope.label().to_owned(),
    }
}

fn name_cell(ui: &mut Ui, s: &ServerRow, project_count: usize) {
    let pal = theme::palette(ui.ctx());
    let color = if s.enabled { pal.text } else { pal.disabled };
    ui.add(egui::Label::new(RichText::new(&s.name).color(color)).truncate());
    for (show, text, fill) in [
        (s.shadowed, "shadowed", pal.warn),
        (s.casing_collision, "casing", pal.warn),
        (
            s.transport == klefki_core::api::TransportKind::Invalid,
            "invalid",
            pal.danger,
        ),
    ] {
        if show {
            badge(ui, text, fill);
        }
    }
    // The toggle is per project: a user server can be off in some and on in others.
    if s.scope == Scope::User && s.enabled && s.disabled_in > 0 {
        badge(
            ui,
            &format!("off in {}/{}", s.disabled_in, project_count),
            pal.warn,
        );
    }
    if s.approval == Some(Approval::Rejected) {
        badge(ui, "rejected", pal.danger);
    }
}

fn status_cell(ui: &mut Ui, s: &ServerRow, status: Status, detail: Option<&str>) {
    let pal = theme::palette(ui.ctx());
    if !s.enabled {
        ui.label(RichText::new("Disabled").color(pal.disabled));
        return;
    }
    if s.approval == Some(Approval::Rejected) {
        ui.label(
            RichText::new(format!("\u{25CF} {}", Approval::Rejected.label())).color(pal.danger),
        );
        return;
    }
    let color = match status {
        Status::Connected => pal.success,
        Status::Degraded | Status::NeedsAuth | Status::PendingApproval => pal.warn,
        Status::Failed => pal.danger,
        Status::Checking => pal.primary,
        Status::NotConfigured | Status::Unknown => pal.text_muted,
    };
    let response = ui.label(RichText::new(format!("\u{25CF} {}", status.label())).color(color));
    if let Some(detail) = detail {
        response.on_hover_text(detail);
    }
}

fn badge(ui: &mut Ui, text: &str, fill: Color32) {
    let pal = theme::palette(ui.ctx());
    Frame::new()
        .fill(fill)
        .corner_radius(4)
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(pal.on_bright));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use klefki_core::api::fixture_snapshot;

    /// Mirrors the app: the theme is applied once in the creation callback, before the first frame.
    fn harness_with(snapshot: Snapshot) -> Harness<'static, ShellApp> {
        harness_themed(snapshot, ThemeChoice::Dark)
    }

    fn harness_themed(snapshot: Snapshot, choice: ThemeChoice) -> Harness<'static, ShellApp> {
        // The harness sets its own theme once the app is built, so tell it the one we want.
        let harness_theme = match choice {
            ThemeChoice::Light => egui::Theme::Light,
            ThemeChoice::Dark | ThemeChoice::System => egui::Theme::Dark,
        };
        Harness::builder()
            .with_theme(harness_theme)
            // Tall enough for every fixture row: the grid only renders rows that are in view.
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(|cc| {
                crate::theme::apply(&cc.egui_ctx, choice);
                ShellApp::new(Arc::new(snapshot), choice)
            })
    }

    fn harness() -> Harness<'static, ShellApp> {
        harness_with((*fixture_snapshot()).clone())
    }

    #[test]
    fn grid_lists_fixture_servers_grouped_by_scope() {
        let mut h = harness();
        h.run();
        h.get_by_label("User");
        h.get_by_label("issue-tracker");
        h.get_by_label("Local · C:/Repos/Gateway");
        h.get_by_label("Project · C:/Repos/WebApp");
    }

    #[test]
    fn search_filters_the_grid() {
        let mut app = ShellApp::new(fixture_snapshot(), ThemeChoice::Dark);
        let all = app.visible().len();
        app.search = "it.example".into();
        let hits = app.visible();
        assert_eq!(hits.len(), 1);
        assert!(hits.len() < all);
        app.search = "NO-SUCH-SERVER".into();
        assert!(app.visible().is_empty());
    }

    #[test]
    fn navigation_switches_pages() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Secrets").click();
        h.run();
        assert_eq!(h.state().page, Page::Secrets);
        h.get_by_label_contains("Connections").click();
        h.run();
        assert_eq!(h.state().page, Page::Connections);
    }

    #[test]
    fn clicking_a_row_selects_it_and_shows_detail() {
        let mut h = harness();
        h.run();
        h.get_by_label("issue-tracker").click();
        h.run();
        assert_eq!(h.state().selected.len(), 1);
        // Once in the grid and once in the detail panel.
        assert_eq!(h.get_all_by_label("https://it.example.com/mcp").count(), 2);
    }

    #[test]
    fn config_warnings_are_visible() {
        let mut h = harness_with(Snapshot {
            warnings: vec!["Could not read the config".to_owned()],
            ..Snapshot::default()
        });
        h.run();
        h.get_by_label_contains("Could not read the config");
    }

    #[test]
    fn many_warnings_are_summarised() {
        let mut h = harness_with(Snapshot {
            warnings: (0..10).map(|i| format!("problem number {i}")).collect(),
            ..Snapshot::default()
        });
        h.run();
        h.get_by_label_contains("problem number 3");
        h.get_by_label_contains("and 6 more");
        assert!(h.query_by_label_contains("problem number 9").is_none());
    }

    #[test]
    fn unknown_cli_version_is_not_shown_as_a_claim() {
        let mut h = harness_with(Snapshot::default());
        h.run();
        assert!(h.query_by_label_contains("Claude CLI").is_none());
    }

    #[test]
    fn nav_and_toolbar_targets_are_large() {
        let mut h = harness();
        h.run();
        let nav = h.get_by_label_contains("Secrets").rect();
        assert!(nav.height() >= 40.0, "nav item {nav:?}");
        let add = h.get_by_label(icon::PLUS).rect();
        assert!(
            add.width() >= 40.0 && add.height() >= 34.0,
            "toolbar button {add:?}"
        );
    }

    #[test]
    fn grid_renders_any_snapshot_not_just_the_fixture() {
        // Loading from a real config is tested in klefki-core; the UI only renders what it is given.
        let row = |name: &str| ServerRow {
            name: name.to_owned(),
            scope: Scope::User,
            project: None,
            transport: klefki_core::api::TransportKind::Http,
            target: format!("https://{name}.example.com/mcp"),
            status: Status::Connected,
            enabled: true,
            disabled_in: 0,
            approval: None,
            shadowed: false,
            casing_collision: false,
        };
        let mut h = harness_with(Snapshot {
            servers: vec![row("REMOTE_1"), row("REMOTE_2")],
            ..Snapshot::default()
        });
        h.run();
        h.get_by_label("REMOTE_1");
        h.get_by_label("REMOTE_2");
    }

    #[test]
    fn sanitised_fixture_text_reaches_the_ui() {
        let mut h = harness();
        h.run();
        // The fixture target contains ANSI escapes; the UI must only see the clean text.
        h.get_by_label("missing command");
    }

    #[test]
    fn theme_dropdown_offers_all_three_and_switches_the_palette() {
        use ThemeChoice::{Dark, Light, System};
        let shown = |c: ThemeChoice| format!("{} {}", c.icon(), c.label());
        let mut h = harness();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::DARK.panel);

        // Closed, the dropdown shows the current choice only.
        h.get_by_value(&shown(Dark));
        assert!(h.query_by_label_contains("Light").is_none());
        assert!(h.query_by_label_contains("System").is_none());

        // Open, all three are offered.
        h.get_by_value(&shown(Dark)).click();
        h.run();
        h.get_by_label_contains("System");
        h.get_by_label_contains("Dark");
        h.get_by_label_contains("Light").click();
        h.run();
        assert_eq!(h.state().theme, Light);
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);

        h.get_by_value(&shown(Light)).click();
        h.run();
        h.get_by_label_contains("System").click();
        h.run();
        assert_eq!(h.state().theme, System);

        h.get_by_value(&shown(System)).click();
        h.run();
        h.get_by_label_contains("Dark").click();
        h.run();
        assert_eq!(h.state().theme, Dark);
        assert_eq!(theme::palette(&h.ctx).panel, theme::DARK.panel);
    }

    #[test]
    fn light_theme_renders_the_whole_shell() {
        let mut h = harness_themed((*fixture_snapshot()).clone(), ThemeChoice::Light);
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
        h.get_by_label("issue-tracker").click();
        h.run();
        h.get_by_label_contains("Secrets");
        assert_eq!(h.get_all_by_label("https://it.example.com/mcp").count(), 2);
    }

    #[test]
    fn the_theme_choice_is_saved() {
        use eframe::App as _;
        let mut storage = crate::theme::tests::MemoryStorage::default();
        let mut app = ShellApp::new(fixture_snapshot(), ThemeChoice::Light);
        app.save(&mut storage);
        assert_eq!(storage.0.get("theme").map(String::as_str), Some("light"));
    }

    #[test]
    fn detail_tabs_do_not_change_size_on_hover() {
        let mut h = harness();
        h.run();
        h.get_by_label("issue-tracker").click();
        h.run();
        for tab in ["Auth", "Headers/Env", "General"] {
            let before = h.get_by_label(tab).rect();
            h.get_by_label(tab).hover();
            h.run();
            let after = h.get_by_label(tab).rect();
            assert_eq!(before, after, "tab {tab} moved or resized on hover");
        }
    }

    #[test]
    fn dropdown_options_do_not_change_size_on_hover() {
        let shown = format!("{} {}", ThemeChoice::Dark.icon(), ThemeChoice::Dark.label());
        let mut h = harness();
        h.run();
        h.get_by_value(&shown).click();
        h.run();
        for option in ["System", "Light"] {
            let before = h.get_by_label_contains(option).rect();
            h.get_by_label_contains(option).hover();
            h.run();
            let after = h.get_by_label_contains(option).rect();
            assert_eq!(before, after, "option {option} moved or resized on hover");
        }
    }

    // ---- live status: the UI only reacts to events from the engine and sends commands back ----

    fn live_row(name: &str, scope: Scope, project: Option<&str>) -> ServerRow {
        ServerRow {
            name: name.to_owned(),
            scope,
            project: project.map(str::to_owned),
            transport: klefki_core::api::TransportKind::Stdio,
            target: "npx something".to_owned(),
            status: Status::Unknown,
            enabled: true,
            disabled_in: 0,
            approval: None,
            shadowed: false,
            casing_collision: false,
        }
    }

    fn snapshot_of(rows: Vec<ServerRow>) -> klefki_core::api::Event {
        klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows,
            ..Snapshot::default()
        }))
    }

    fn live_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (core, engine) = klefki_core::api::Core::detached();
        let h = Harness::builder()
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(move |cc| {
                crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
                ShellApp::with_core(core, ThemeChoice::Dark)
            });
        (h, engine)
    }

    fn status_event(
        row: &ServerRow,
        status: Status,
        detail: Option<&str>,
    ) -> klefki_core::api::Event {
        klefki_core::api::Event::Status {
            id: row.id(),
            status,
            detail: detail.map(str::to_owned),
        }
    }

    #[test]
    fn a_snapshot_event_fills_the_grid_and_status_events_update_it_live() {
        let (mut h, engine) = live_harness();
        h.run();
        let row = live_row("azure-devops", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
        h.get_by_label("azure-devops");
        h.get_by_label_contains("Not checked");

        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Checking");
        assert!(h.query_by_label_contains("Not checked").is_none());

        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Connected");
        assert!(h.query_by_label_contains("Checking").is_none());
    }

    #[test]
    fn every_status_has_its_own_readable_label() {
        let (mut h, engine) = live_harness();
        let statuses = [
            Status::Connected,
            Status::Degraded,
            Status::NeedsAuth,
            Status::Failed,
            Status::PendingApproval,
            Status::NotConfigured,
            Status::Checking,
            Status::Unknown,
        ];
        let labels: std::collections::BTreeSet<_> = statuses.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels.len(),
            statuses.len(),
            "every status reads differently"
        );
        let rows: Vec<ServerRow> = (0..statuses.len())
            .map(|i| live_row(&format!("srv{i}"), Scope::User, None))
            .collect();
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        for (row, status) in rows.iter().zip(statuses) {
            engine.events.send(status_event(row, status, None)).unwrap();
        }
        h.run();
        for status in statuses {
            h.get_by_label(&format!("\u{25CF} {}", status.label()));
        }
    }

    #[test]
    fn a_new_snapshot_clears_checks_that_were_cut_off() {
        let (mut h, engine) = live_harness();
        let row = live_row("azure-devops", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Checking");
        // A refresh replaced the world; the running check's answer will be dropped by the engine.
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
        assert!(h.query_by_label_contains("Checking").is_none());
        h.get_by_label_contains("Not checked");
    }

    #[test]
    fn results_for_rows_that_are_gone_do_not_linger_or_crash() {
        let (mut h, engine) = live_harness();
        let gone = live_row("gone", Scope::User, None);
        let stays = live_row("stays", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![gone.clone(), stays.clone()]))
            .unwrap();
        engine
            .events
            .send(status_event(&gone, Status::Failed, Some("x")))
            .unwrap();
        engine
            .events
            .send(snapshot_of(vec![stays.clone()]))
            .unwrap();
        engine
            .events
            .send(status_event(&gone, Status::Failed, Some("late")))
            .unwrap();
        h.run();
        assert!(h.query_by_label("gone").is_none());
        h.get_by_label("stays");
    }

    #[test]
    fn the_cli_version_and_problems_are_shown() {
        let (mut h, engine) = live_harness();
        h.run();
        assert!(h.query_by_label_contains("Claude CLI").is_none());
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.302".to_owned(),
            )))
            .unwrap();
        h.run();
        h.get_by_label_contains("Claude CLI 2.1.302");
        engine
            .events
            .send(klefki_core::api::Event::CliProblem(
                "Live status is unavailable: nope.".to_owned(),
            ))
            .unwrap();
        h.run();
        h.get_by_label_contains("Live status is unavailable");
    }

    #[test]
    fn the_status_tab_shows_the_detail_and_check_now_asks_the_engine() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(
                &row,
                Status::Failed,
                Some("CONNECTION_CLOSED: Connection closed"),
            ))
            .unwrap();
        h.run();
        h.get_by_label("docs").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("CONNECTION_CLOSED: Connection closed");
        // A project server's check runs the repository's own command, and the UI says so.
        h.get_by_label_contains("runs this server's command");
        h.get_by_label_contains("Check now").click();
        h.run();
        let sent = engine.commands.try_recv().expect("a command was sent");
        assert_eq!(sent, klefki_core::api::Command::Check(row.id()));
        assert!(engine.commands.try_recv().is_err(), "exactly one command");
    }

    #[test]
    fn check_now_does_not_warn_about_commands_for_remote_servers() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.transport = klefki_core::api::TransportKind::Http;
        engine.events.send(snapshot_of(vec![row])).unwrap();
        h.run();
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        assert!(
            h.query_by_label_contains("runs this server's command")
                .is_none()
        );
        h.get_by_label_contains("Check now");
    }

    #[test]
    fn the_refresh_button_and_f5_ask_for_a_full_recheck() {
        let (mut h, engine) = live_harness();
        h.run();
        h.get_by_label(icon::ARROWS_CLOCKWISE).click();
        h.run();
        assert_eq!(
            engine.commands.try_recv().unwrap(),
            klefki_core::api::Command::Refresh { live_all: true }
        );
        h.key_press(egui::Key::F5);
        h.run();
        assert_eq!(
            engine.commands.try_recv().unwrap(),
            klefki_core::api::Command::Refresh { live_all: true }
        );
        assert!(engine.commands.try_recv().is_err());
    }

    #[test]
    fn disabled_rows_still_say_disabled_whatever_the_engine_reports() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("off", Scope::User, None);
        row.enabled = false;
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Disabled");
        assert!(h.query_by_label_contains("Connected").is_none());
    }

    // ---- the toggle is per project; project servers have an approval ----

    fn snapshot_with_projects(count: usize, rows: Vec<ServerRow>) -> klefki_core::api::Event {
        klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows,
            project_count: count,
            ..Snapshot::default()
        }))
    }

    #[test]
    fn a_user_server_off_in_some_projects_says_how_many() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.disabled_in = 3;
        engine
            .events
            .send(snapshot_with_projects(17, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label("gateway");
        h.get_by_label("off in 3/17");
        assert!(
            h.query_by_label_contains("Disabled").is_none(),
            "still on in 14 projects"
        );
    }

    #[test]
    fn a_user_server_off_everywhere_reads_disabled_without_the_partial_badge() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.disabled_in = 17;
        row.enabled = false;
        engine
            .events
            .send(snapshot_with_projects(17, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label_contains("Disabled");
        assert!(h.query_by_label_contains("off in").is_none());
    }

    #[test]
    fn a_rejected_project_server_says_rejected_and_pending_is_a_status() {
        let (mut h, engine) = live_harness();
        let mut rejected = live_row("no-thanks", Scope::Project, Some("C:/Repos/WebApp"));
        rejected.approval = Some(klefki_core::api::Approval::Rejected);
        let mut pending = live_row("waiting", Scope::Project, Some("C:/Repos/WebApp"));
        pending.approval = Some(klefki_core::api::Approval::Pending);
        pending.status = Status::PendingApproval;
        engine
            .events
            .send(snapshot_with_projects(1, vec![rejected, pending]))
            .unwrap();
        h.run();
        h.get_by_label("\u{25CF} Rejected");
        h.get_by_label("\u{25CF} Pending approval");
    }

    #[test]
    fn the_detail_panel_explains_the_toggle_and_the_approval() {
        let (mut h, engine) = live_harness();
        let mut user = live_row("gateway", Scope::User, None);
        user.disabled_in = 3;
        let mut project = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
        project.approval = Some(klefki_core::api::Approval::Pending);
        engine
            .events
            .send(snapshot_with_projects(17, vec![user, project]))
            .unwrap();
        h.run();
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("disabled in 3 of 17 projects");
        h.get_by_label("docs").click();
        h.run();
        h.get_by_label("Pending approval");
        assert!(h.query_by_label_contains("of 17 projects").is_none());
    }

    // ---- Diagnose: the explanation the engine builds is shown, and asked for, from the Status tab ----

    fn diagnosis(
        status: Status,
        hints: &[(&str, &str)],
        said: &[&str],
        log: &[&str],
        note: Option<&str>,
    ) -> Diagnosis {
        Diagnosis {
            status,
            detail: Some("CONNECTION_CLOSED: Connection closed".to_owned()),
            hints: hints
                .iter()
                .map(|(t, a)| klefki_core::api::Hint {
                    title: (*t).to_owned(),
                    advice: (*a).to_owned(),
                })
                .collect(),
            server_said: said.iter().map(|l| (*l).to_owned()).collect(),
            client_log: log.iter().map(|l| (*l).to_owned()).collect(),
            note: note.map(str::to_owned),
        }
    }

    fn open_status_tab(h: &mut Harness<'static, ShellApp>, row_name: &str) {
        use egui::accesskit::Role;
        h.get_by_label(row_name).click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
    }

    #[test]
    fn a_diagnosis_is_shown_in_the_status_tab() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(
                    Status::Failed,
                    &[("The command was not found", "Claude could not start the server because the program is not on the PATH it uses.")],
                    &["'xyz' is not recognized as an internal or external command,", "operable program or batch file."],
                    &["[ERROR] MCP server \"bad-stdio\" Connection failed (CONNECTION_CLOSED)"],
                    Some("the client wrote no debug output"),
                ),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        h.get_by_label("The command was not found");
        h.get_by_label_contains("not on the PATH it uses");
        h.get_by_label("The server said");
        h.get_by_label("'xyz' is not recognized as an internal or external command,");
        h.get_by_label("operable program or batch file.");
        h.get_by_label_contains("the client wrote no debug output");
    }

    #[test]
    fn the_client_log_is_collapsed_until_opened() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(
                    Status::Failed,
                    &[],
                    &[],
                    &["LOGLINE-ONE", "LOGLINE-TWO"],
                    None,
                ),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        assert!(
            h.query_by_label_contains("LOGLINE-ONE").is_none(),
            "collapsed by default"
        );
        h.get_by_label_contains("Client log").click();
        h.run();
        h.get_by_label_contains("LOGLINE-ONE");
        h.get_by_label_contains("LOGLINE-TWO");
    }

    #[test]
    fn diagnose_asks_the_engine_and_waits_while_a_check_runs() {
        use egui_kittest::kittest::NodeT;
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        h.get_by_label("Diagnose").click();
        h.run();
        assert_eq!(
            engine.commands.try_recv().unwrap(),
            klefki_core::api::Command::Diagnose(row.id())
        );
        assert!(engine.commands.try_recv().is_err(), "exactly one command");
        // While a check or diagnosis is running, asking again would only queue a duplicate.
        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        assert!(h.get_by_label("Diagnose").accesskit_node().is_disabled());
        assert!(h.get_by_label("Check now").accesskit_node().is_disabled());
    }

    #[test]
    fn a_diagnosis_for_a_row_that_is_gone_is_dropped() {
        let (mut h, engine) = live_harness();
        let gone = live_row("gone", Scope::User, None);
        let stays = live_row("stays", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![stays.clone()]))
            .unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: gone.id(),
                diagnosis: diagnosis(Status::Failed, &[], &["NEVER-SHOWN"], &[], None),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, "stays");
        assert!(h.query_by_label_contains("NEVER-SHOWN").is_none());
        assert!(h.query_by_label("The server said").is_none());
    }

    #[test]
    fn a_new_snapshot_forgets_diagnoses_of_rows_that_disappeared() {
        let (mut h, engine) = live_harness();
        let a = live_row("a", Scope::User, None);
        let b = live_row("b", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: b.id(),
                diagnosis: diagnosis(Status::Failed, &[], &["B-SAID-THIS"], &[], None),
            })
            .unwrap();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        open_status_tab(&mut h, "b");
        assert!(
            h.query_by_label_contains("B-SAID-THIS").is_none(),
            "an old answer is not shown for a new row"
        );
    }

    #[test]
    fn the_report_to_copy_is_complete_plain_and_free_of_markup() {
        let mut row = live_row("bad-stdio", Scope::Project, Some("C:/Repos/WebApp"));
        row.transport = klefki_core::api::TransportKind::Stdio;
        let d = diagnosis(
            Status::Failed,
            &[(
                "The command was not found",
                "Install it or use its full path.",
            )],
            &["xyz is not recognized"],
            &["[ERROR] MCP server \"bad-stdio\" Connection failed"],
            Some("a note"),
        );
        let text = diagnosis_report(&row, &d);
        for needle in [
            "bad-stdio",
            "Project",
            "C:/Repos/WebApp",
            "Failed",
            "CONNECTION_CLOSED: Connection closed",
            "The command was not found",
            "Install it or use its full path.",
            "xyz is not recognized",
            "Connection failed",
            "a note",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
        assert!(!text.contains('\u{1b}') && !text.contains("\r"), "{text:?}");
        assert!(text.ends_with('\n'));
    }

    // ---- long text must wrap inside the detail panel, never resize it ----

    /// Where the detail panel's contents start. If the panel grows, this moves left.
    fn detail_left_edge(h: &Harness<'static, ShellApp>, label: &str) -> f32 {
        h.get_by_label(label).rect().min.x
    }

    #[test]
    fn a_very_long_status_detail_wraps_instead_of_widening_the_panel() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::Failed, Some("short")))
            .unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        let before = detail_left_edge(&h, "Check now");

        let long =
            "ECONNREFUSED: Unable to connect. Is the computer able to access the url? ".repeat(8);
        engine
            .events
            .send(status_event(&row, Status::Failed, Some(&long)))
            .unwrap();
        h.run();
        let after = detail_left_edge(&h, "Check now");
        assert_eq!(
            before, after,
            "the detail panel changed width because of a long detail"
        );
    }

    #[test]
    fn a_very_long_target_wraps_instead_of_widening_the_panel() {
        let short = {
            let (mut h, engine) = live_harness();
            engine
                .events
                .send(snapshot_of(vec![live_row("srv", Scope::User, None)]))
                .unwrap();
            h.run();
            h.get_by_label("srv").click();
            h.run();
            detail_left_edge(&h, "Scope")
        };
        let long = {
            let (mut h, engine) = live_harness();
            let mut row = live_row("srv", Scope::User, None);
            row.target = format!(
                "https://example.com/{}",
                "very-long-path-segment/".repeat(20)
            );
            engine.events.send(snapshot_of(vec![row])).unwrap();
            h.run();
            h.get_by_label("srv").click();
            h.run();
            detail_left_edge(&h, "Scope")
        };
        assert_eq!(short, long, "a long target made the detail panel wider");
    }

    // ---- switching servers on and off

    fn plan_view(id: u64, lines: &[&str], warnings: &[&str], nothing_to_do: bool) -> PlanView {
        PlanView {
            id,
            files: vec![klefki_core::api::FileDiff {
                path: ".claude.json".to_owned(),
                lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            }],
            warnings: warnings.iter().map(|w| (*w).to_owned()).collect(),
            nothing_to_do,
            reviews: Vec::new(),
        }
    }

    fn two_rows() -> (ServerRow, ServerRow) {
        (
            live_row("alpha", Scope::User, None),
            live_row("beta", Scope::Local, Some("C:/Repos/WebApp")),
        )
    }

    fn drain(engine: &klefki_core::api::Detached) -> Vec<Command> {
        let mut out = Vec::new();
        while let Ok(c) = engine.commands.try_recv() {
            out.push(c);
        }
        out
    }

    #[test]
    fn enable_and_disable_need_a_selection() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        assert!(drain(&engine).is_empty(), "nothing selected, nothing asked");
    }

    #[test]
    fn disable_asks_the_engine_for_a_plan_for_each_selected_server() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: a.id(),
                enabled: false
            }])]
        );
    }

    #[test]
    fn enable_asks_for_the_opposite() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_RIGHT).click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: a.id(),
                enabled: true
            }])]
        );
    }

    fn with_plan(
        lines: &[&str],
        warnings: &[&str],
        nothing: bool,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        engine
            .events
            .send(Event::PlanReady(plan_view(7, lines, warnings, nothing)))
            .unwrap();
        h.run();
        (h, engine)
    }

    #[test]
    fn the_plan_dialog_shows_exactly_what_will_change_and_nothing_is_sent_yet() {
        let (_h, engine) = {
            let (h, engine) = with_plan(
                &["  projects[\"C:/x\"].disabledMcpServers", "+ alpha"],
                &["A running Claude Code session may need a restart before it notices this."],
                false,
            );
            h.get_by_label_contains("Review the change");
            h.get_by_label_contains("disabledMcpServers");
            h.get_by_label("+ alpha");
            h.get_by_label_contains("may need a restart");
            (h, engine)
        };
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn apply_sends_the_plan_once_even_if_pressed_twice() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Apply").click();
        h.run();
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
    }

    #[test]
    fn cancel_discards_the_plan_and_closes_the_dialog() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Cancel").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Discard(7)]);
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    #[test]
    fn escape_cancels_like_the_cancel_button() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(drain(&engine), vec![Command::Discard(7)]);
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    #[test]
    fn a_plan_that_changes_nothing_cannot_be_applied() {
        let (mut h, engine) = with_plan(&[], &[], true);
        h.get_by_label_contains("Nothing would be written");
        h.get_by_label("Apply").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn a_saved_change_closes_the_dialog_and_says_where_the_backup_is() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: Some("/data/backups/claude.json.1.bak".to_owned()),
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
        h.get_by_label_contains(
            "Saved. The previous file is kept in /data/backups/claude.json.1.bak",
        );
    }

    #[test]
    fn a_failed_change_closes_the_dialog_and_shows_the_reason() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 7,
                error: "Claude Code is writing its configuration right now.".to_owned(),
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
        h.get_by_label_contains("Claude Code is writing its configuration");
        h.get_by_label("Dismiss").click();
        h.run();
        assert!(
            h.query_by_label_contains("writing its configuration")
                .is_none()
        );
    }

    #[test]
    fn a_plan_that_could_not_be_made_shows_the_reason() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(Event::PlanFailed(
                "ghost is no longer in the configuration.".to_owned(),
            ))
            .unwrap();
        h.run();
        h.get_by_label_contains("ghost is no longer");
    }

    #[test]
    fn a_change_claude_overwrote_is_reported_after_the_dialog_is_gone() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: Some("b".to_owned()),
            })
            .unwrap();
        engine.events.send(Event::Reapplied { plan: 7 }).unwrap();
        h.run();
        h.get_by_label_contains("overwrote the change right after it was saved");
    }

    #[test]
    fn the_toggle_buttons_do_not_start_a_second_plan_while_one_is_open() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(8, &["+ alpha"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        // A click outside the dialog closes it (and drops the plan); it never starts a second plan.
        let sent = drain(&engine);
        assert!(
            sent.iter().all(|c| !matches!(c, Command::Prepare(_))),
            "a second plan cannot start: {sent:?}"
        );
    }

    // ---- approving a project server

    fn project_row(name: &str, approval: Option<klefki_core::api::Approval>) -> ServerRow {
        let mut row = live_row(name, Scope::Project, Some("C:/Repos/WebApp"));
        row.approval = approval;
        row
    }

    fn open_general(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
    }

    #[test]
    fn a_pending_or_rejected_project_server_offers_review_and_approve() {
        use klefki_core::api::Approval::{Pending, Rejected};
        for approval in [Pending, Rejected] {
            let (mut h, engine) = live_harness();
            let row = project_row("docs", Some(approval));
            engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
            h.run();
            open_general(&mut h, "docs");
            h.get_by_label_contains("Review and approve").click();
            h.run();
            assert_eq!(
                drain(&engine),
                vec![Command::Prepare(vec![Change::Approve { id: row.id() }])]
            );
        }
    }

    #[test]
    fn approved_servers_and_other_scopes_have_no_approve_button() {
        let (mut h, engine) = live_harness();
        let approved = project_row("done", Some(klefki_core::api::Approval::Approved));
        let user = live_row("mine", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![approved, user]))
            .unwrap();
        h.run();
        for name in ["done", "mine"] {
            open_general(&mut h, name);
            assert!(
                h.query_by_label_contains("Review and approve").is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_approval_dialog_shows_what_will_run_before_anything_is_saved() {
        let (mut h, engine) = live_harness();
        let row = project_row("docs", Some(klefki_core::api::Approval::Pending));
        engine.events.send(snapshot_of(vec![row])).unwrap();
        let mut plan = plan_view(
            3,
            &["  enabledMcpjsonServers", "+ docs"],
            &["Only approve it if you trust the repository."],
            false,
        );
        plan.reviews = vec![klefki_core::api::Review {
            title: "docs would be allowed to run:".to_owned(),
            lines: vec![
                "Command: node".to_owned(),
                "Argument 1: server.js".to_owned(),
                "Environment: NODE_OPTIONS=--require ./hook.js".to_owned(),
            ],
        }];
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label_contains("docs would be allowed to run");
        h.get_by_label("Command: node");
        h.get_by_label("Argument 1: server.js");
        h.get_by_label_contains("NODE_OPTIONS=--require ./hook.js");
        h.get_by_label_contains("trust the repository");
        assert!(drain(&engine).is_empty());
        // The button says what it does, and sends the plan.
        h.get_by_label("Approve").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(3)]);
    }

    #[test]
    fn a_saved_approval_without_a_backup_just_says_saved() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
            })
            .unwrap();
        h.run();
        h.get_by_label("Saved.");
    }

    #[test]
    fn live_results_and_selection_follow_their_rows_when_a_snapshot_reorders_them() {
        let (mut h, engine) = live_harness();
        let a = live_row("alpha", Scope::User, None);
        let b = live_row("beta", Scope::User, None);
        let c = live_row("gamma", Scope::Local, Some("C:/Repos/WebApp"));
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        engine
            .events
            .send(status_event(&b, Status::Failed, Some("beta broke")))
            .unwrap();
        h.run();
        h.get_by_label("beta").click();
        h.run();
        // Same rows, another order, one more in front.
        engine
            .events
            .send(snapshot_of(vec![c, b.clone(), a.clone()]))
            .unwrap();
        h.run();
        assert_eq!(h.state().selected.len(), 1);
        let selected = *h.state().selected.iter().next().unwrap();
        assert_eq!(h.state().snapshot.servers[selected].name, "beta");
        assert_eq!(h.state().effective(selected).0, Status::Failed);
        let alpha = h
            .state()
            .snapshot
            .servers
            .iter()
            .position(|r| r.name == "alpha")
            .unwrap();
        assert_eq!(h.state().effective(alpha).0, Status::Unknown);
        h.get_by_label_contains(Status::Failed.label());
    }

    #[test]
    fn the_grid_follows_a_new_snapshot_and_a_new_search() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(snapshot_of(vec![live_row("alpha", Scope::User, None)]))
            .unwrap();
        h.run();
        h.get_by_label("alpha");
        engine
            .events
            .send(snapshot_of(vec![
                live_row("alpha", Scope::User, None),
                live_row("omega", Scope::User, None),
            ]))
            .unwrap();
        h.run();
        h.get_by_label("omega");
        h.state_mut().search = "alp".to_owned();
        h.run();
        assert!(h.query_by_label("omega").is_none());
        h.state_mut().search.clear();
        h.run();
        h.get_by_label("omega");
    }
}
