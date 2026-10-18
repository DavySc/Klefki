//! The desktop-style shell: left nav, top bar, icon toolbar, grid and detail panel.
//! Pure UI over an immutable `Snapshot`; no IO.
use crate::command::{self, CommandPalette, Entry, shortcut};
use crate::explain::Explanation;
use crate::theme::{self, Palette, ThemeChoice, Tone};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Approval, Change, Command, ConnectionId, Core, Definition, Diagnosis, Event, Hint, PlanView,
    Scope, Secret, ServerId, ServerName, ServerRow, Snapshot, Status, Target, TransportKind,
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

    fn glyph(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(p, ..)| *p == self)
            .map_or("", |(_, g, _)| g)
    }

    /// Pages that are not built yet say what they will do, so the menu is a map rather than a dead end.
    fn ready(self) -> bool {
        matches!(self, Page::Connections | Page::Settings)
    }

    fn promise(self) -> &'static str {
        match self {
            Page::Connections => "",
            Page::ScopeDrift => {
                "Coming soon: servers whose definitions differ between scopes, side by side."
            }
            Page::Secrets => {
                "Coming soon: the secrets Klefki keeps in your system keychain, and any that nothing uses any more."
            }
            Page::Settings => {
                "Coming soon: where to find the Claude CLI, and how often to check servers."
            }
            Page::ActivityLog => "Coming soon: every change Klefki made, with its backup.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    General,
    Auth,
    HeadersEnv,
    Status,
}

impl DetailTab {
    // No JSON tab: the definition is edited in labelled fields, and as JSON only from there, when they fall short.
    const ALL: [(DetailTab, &'static str); 4] = [
        // Short names keep the tabs on one line in a panel of a third of the window.
        (DetailTab::General, "General"),
        (DetailTab::Status, "Status"),
        (DetailTab::HeadersEnv, "Env"),
        (DetailTab::Auth, "Auth"),
    ];
}

/// The definition being edited, for one server: in the form, or as JSON.
struct Editor {
    id: ServerId,
    state: EditorState,
}

enum EditorState {
    /// Asked the engine for the definition.
    Loading,
    Failed(String),
    Ready {
        /// As the engine sent it, to tell whether anything was edited.
        original: String,
        /// The draft, as JSON: what the JSON editor edits and what is sent for review.
        text: String,
        /// Project servers are shown but not edited (they live in the repository).
        editable: bool,
        /// The same draft as the form shows it, or why the form cannot. Boxed, like `synced_form`: a definition is
        /// large next to the other states.
        form: Box<Result<Definition, String>>,
        /// What stops the form's draft from being saved.
        problem: Option<String>,
        /// `text` and `form` as they were when last brought in line, to tell which one was edited since.
        synced_text: String,
        synced_form: Option<Box<Definition>>,
        /// Edited as JSON rather than in the form: asked for, or a definition the form cannot show. It stays until
        /// the user goes back, so the form does not replace the JSON under the cursor once it becomes readable.
        as_json: bool,
    },
}

impl EditorState {
    fn ready(text: String, editable: bool) -> Self {
        let mut state = Self::Ready {
            original: text.clone(),
            text,
            editable,
            form: Box::new(Err(String::new())),
            problem: None,
            synced_text: String::new(),
            synced_form: None,
            as_json: false,
        };
        state.sync();
        if let Self::Ready { form, as_json, .. } = &mut state {
            *as_json = form.is_err();
        }
        state
    }

    /// Brings the form and the JSON in line after either was edited: an edited JSON text is read into the form,
    /// an edited form is written out as JSON. Comparisons only, unless something changed.
    fn sync(&mut self) {
        let Self::Ready {
            text,
            form,
            problem,
            synced_text,
            synced_form,
            ..
        } = self
        else {
            return;
        };
        if *text != *synced_text {
            **form = Definition::from_json(text);
        } else if let Ok(f) = form.as_ref()
            && Some(f) != synced_form.as_deref()
        {
            *text = f.to_json();
        } else {
            return;
        }
        *synced_text = text.clone();
        *synced_form = form.as_ref().as_ref().ok().cloned().map(Box::new);
        *problem = form.as_ref().as_ref().ok().and_then(Definition::problem);
    }
}

/// What happened to the last change, for the banner.
#[derive(Debug, Clone, PartialEq)]
struct Outcome {
    text: String,
    ok: bool,
    /// When it was first shown (egui time, seconds). A success fades after a few seconds; a problem stays.
    shown_at: Option<f64>,
}

/// How long a success message stays.
const SUCCESS_SECONDS: f64 = 4.0;

enum GridRow {
    Group(Group),
    Server(usize),
}

/// A group header: the scope and, for a project, its folder name up front and the rest of the path muted.
struct Group {
    title: String,
    /// The folder the project is in, shown quietly after the title.
    location: Option<String>,
    /// The whole label, as one line: what assistive technology reads and what the tooltip shows.
    full: String,
    count: usize,
}

pub struct ShellApp {
    snapshot: Arc<Snapshot>,
    /// The engine. `None` for demo data and tests that feed the UI directly.
    core: Option<Core>,
    /// Live results from the engine, which take precedence over the snapshot's hints. One slot per row of the
    /// snapshot, by position, so drawing a row looks its status up without building a key.
    live: Vec<Option<Live>>,
    /// Bumped whenever `live` or the snapshot changes, so what is built from them can be kept until then.
    live_gen: u64,
    /// The selected row's explanation, for (row, `live_gen`): built once per status, not every frame.
    explained: Option<(ExplainedFor, Option<Arc<Explanation>>)>,
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
    /// A plan was asked for and the engine has not answered yet.
    preparing: bool,
    /// The definition being viewed or edited, in the form or as JSON.
    editor: Option<Editor>,
    /// Ctrl+K / ⌘K.
    command: CommandPalette,
    /// The Duplicate dialog, while open.
    duplicate: Option<DuplicateForm>,
    /// The Fix casing dialog, while open.
    casing: Option<CasingForm>,
    /// What is known about the shown OAuth server's client secret.
    secret: Option<SecretView>,
    /// A client secret being typed. Wiped from memory when dropped; handed to the engine, never copied.
    secret_input: Option<Secret>,
    /// The outcome of the last change, shown until dismissed.
    outcome: Option<Outcome>,
    pub page: Page,
    pub search: String,
    pub selected: BTreeSet<usize>,
    pub tab: DetailTab,
    pub theme: ThemeChoice,
    pub tone: Tone,
}

impl ShellApp {
    pub fn new(snapshot: Arc<Snapshot>, theme: ThemeChoice) -> Self {
        Self {
            core: None,
            live: vec![None; snapshot.servers.len()],
            grid_cache: None,
            live_gen: 0,
            explained: None,
            diagnoses: HashMap::new(),
            cli_version: None,
            cli_problem: None,
            plan: None,
            applying: false,
            preparing: false,
            editor: None,
            command: CommandPalette::default(),
            duplicate: None,
            casing: None,
            secret: None,
            secret_input: None,
            outcome: None,
            snapshot,
            page: Page::Connections,
            search: String::new(),
            selected: BTreeSet::new(),
            tab: DetailTab::General,
            theme,
            tone: Tone::default(),
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
                // Rows are kept by position; carry selection, live results and diagnoses over by identity: the
                // connection, as the file spells the server, since two servers can show the same name.
                let new_index: HashMap<&ConnectionId, usize> = snapshot
                    .servers
                    .iter()
                    .enumerate()
                    .map(|(i, r)| (&r.connection, i))
                    .collect();
                let moved = |old: usize| {
                    new_index
                        .get(&self.snapshot.servers[old].connection)
                        .copied()
                };
                let selected = self.selected.iter().filter_map(|&i| moved(i)).collect();
                let mut live = vec![None; snapshot.servers.len()];
                for (old, result) in self.live.iter_mut().enumerate() {
                    // A check that was running is cut off by a refresh.
                    if let (Some(new), Some(r)) = (moved(old), result.take())
                        && r.status != Status::Checking
                    {
                        live[new] = Some(r);
                    }
                }
                self.live_gen += 1;
                self.diagnoses
                    .retain(|id, _| new_index.contains_key(&id.connection));
                self.selected = selected;
                self.live = live;
                if self
                    .editor
                    .as_ref()
                    .is_some_and(|e| !new_index.contains_key(&e.id.connection))
                {
                    self.editor = None;
                }
                self.grid_cache = None;
                self.command.invalidate();
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
                self.preparing = false;
                if self.plan.is_some() {
                    // Only one dialog at a time; the one open (perhaps being saved) keeps it.
                    self.send(Command::Discard(plan.id));
                } else {
                    self.plan = Some(plan);
                }
            }
            Event::PlanFailed(why) => {
                self.preparing = false;
                self.outcome = Some(Outcome {
                    text: why,
                    ok: false,
                    shown_at: None,
                })
            }
            Event::Applied {
                plan,
                changed,
                backup,
            } => {
                if changed {
                    // Read again what was saved, for the server still being edited; drop it for any other.
                    let open = self
                        .editor
                        .take()
                        .map(|e| e.id)
                        .filter(|id| self.single_selected().is_some_and(|s| s.is(id)));
                    if let Some(id) = open {
                        self.editor = Some(Editor {
                            id: id.clone(),
                            state: EditorState::Loading,
                        });
                        self.send(Command::Inspect(id));
                    }
                }
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.plan = None;
                    self.applying = false;
                }
                self.outcome = Some(Outcome {
                    // What was saved is read again next time it is shown.
                    text: match (changed, backup) {
                        (false, _) => "Nothing needed to change.".to_owned(),
                        (true, Some(b)) => format!("Saved. The previous file is kept in {b}"),
                        (true, None) => "Saved.".to_owned(),
                    },
                    ok: true,
                    shown_at: None,
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
                    shown_at: None,
                });
            }
            Event::Reapplied { .. } => {
                self.outcome = Some(Outcome {
                    text: "Claude Code overwrote the change right after it was saved, so it was written again. Close running Claude Code sessions if this keeps happening."
                        .to_owned(),
                    ok: false,
                    shown_at: None,
                });
            }
            Event::Entry { id, json, editable } => {
                if let Some(editor) = &mut self.editor
                    && editor.id == id
                    && matches!(editor.state, EditorState::Loading)
                {
                    editor.state = match json {
                        Ok(text) => EditorState::ready(text, editable),
                        Err(why) => EditorState::Failed(why),
                    };
                }
            }
            Event::SecretStatus { id, stored } => {
                if let Some(view) = &mut self.secret
                    && view.id == id
                {
                    view.known = Some(stored);
                    view.busy = None;
                }
            }
            Event::CliVersion(version) => self.cli_version = version,
            Event::CliProblem(problem) => self.cli_problem = Some(problem),
            Event::Status {
                id,
                status,
                detail,
                hint,
            } => {
                if let Some(i) = self.snapshot.servers.iter().position(|r| r.is(&id)) {
                    self.live[i] = Some(Live {
                        status,
                        detail,
                        hint,
                    });
                    self.live_gen += 1;
                }
            }
        }
    }

    /// What to show for row `i`: the engine's latest answer, else what Claude's own cache said.
    fn effective(&self, i: usize) -> (Status, Option<&str>) {
        match self.live.get(i).and_then(Option::as_ref) {
            Some(live) => (live.status, live.detail.as_deref()),
            None => (self.snapshot.servers[i].status, None),
        }
    }

    /// What the engine's latest answer for row `i` most likely means, in plain words.
    fn hint(&self, i: usize) -> Option<&Hint> {
        self.live.get(i)?.as_ref()?.hint.as_ref()
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
                rows.push(GridRow::Group(group_of(s)));
                last = Some(key);
            }
            if let Some(GridRow::Group(g)) = rows
                .iter_mut()
                .rev()
                .find(|r| matches!(r, GridRow::Group(_)))
            {
                g.count += 1;
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
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K))
            && self.plan.is_none()
            && self.duplicate.is_none()
            && self.casing.is_none()
        {
            if self.command.is_open() {
                self.command.close();
            } else {
                self.command.open();
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            ctx.memory_mut(|m| m.request_focus(search_id()));
        }
        let idle = !ctx.egui_wants_keyboard_input()
            && self.plan.is_none()
            && !self.command.is_open()
            && self.duplicate.is_none()
            && self.casing.is_none();
        // Up and down walk the list, like any list.
        if idle && self.page == Page::Connections {
            let step = ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown) as i32
                    - i.key_pressed(egui::Key::ArrowUp) as i32
            });
            if step != 0 {
                self.move_selection(step);
            }
        }
        // Del and Ctrl+E act on the selection, unless a text field has the keyboard (Del edits text there).
        if idle && self.core.is_some() {
            if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.prepare_delete();
            }
            if self.selected.len() == 1
                && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::E))
            {
                self.edit_selected();
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::D)) {
                self.open_duplicate();
            }
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
            // A share of the window, so explanations get room on a wide screen without crowding a narrow one.
            let width = ui.available_width();
            // Named anew when its sizes changed: egui remembers a panel's width by name.
            egui::Panel::right("detail-panel")
                .default_size((width * 0.34).clamp(360.0, 640.0))
                .min_size(280.0)
                .max_size((width * 0.5).max(460.0))
                .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(12)))
                .show(ui, |ui| self.detail(ui));
        }
        self.plan_dialog(&ctx);
        self.duplicate_dialog(&ctx);
        self.casing_dialog(&ctx);
        self.outcome_toast(&ctx);
        let mut command = std::mem::take(&mut self.command);
        let chosen = command.show(&ctx, || self.commands());
        self.command = command;
        if let Some(action) = chosen {
            self.run(&ctx, action);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(14)))
            .show(ui, |ui| match self.page {
                Page::Connections => {
                    self.warnings(ui);
                    self.legacy_notice(ui);
                    self.toolbar(ui);
                    ui.add_space(6.0);
                    self.grid(ui);
                }
                Page::Settings => self.settings(ui),
                other => {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(other.glyph())
                                .size(32.0)
                                .color(pal.text_muted),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new(other.title()).heading());
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(RichText::new(other.promise()).color(pal.text_muted))
                                .wrap(),
                        );
                        ui.add_space(12.0);
                        if ui.button("Back to connections").clicked() {
                            self.page = Page::Connections;
                        }
                    });
                }
            });
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(icon::KEY).size(17.0).color(pal.key));
            ui.label(
                RichText::new("Klefki")
                    .family(theme::bold_family())
                    .size(18.0)
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
                if ui
                    .button(
                        RichText::new(format!("{} {}", icon::COMMAND, shortcut("K")))
                            .color(pal.text_muted),
                    )
                    .on_hover_text("Commands: everything Klefki can do, by name")
                    .clicked()
                {
                    self.command.open();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .id(search_id())
                        .hint_text(format!(
                            "{} Filter  {}",
                            icon::MAGNIFYING_GLASS,
                            shortcut("F")
                        ))
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
            // The menu orients; it should not compete with the list. Inactive items are dim, icons small and
            // all one colour, and the active item is a quiet rounded fill rather than a coloured bar.
            let pill = rect.shrink2(egui::vec2(8.0, 3.0));
            if active {
                ui.painter().rect_filled(pill, 6.0, pal.hover);
            } else if resp.hovered() {
                ui.painter().rect_filled(pill, 6.0, pal.row_alt);
            }
            let color = if active {
                pal.text
            } else if page.ready() {
                pal.text_muted
            } else {
                pal.disabled
            };
            ui.painter().text(
                rect.left_center() + egui::vec2(22.0, 0.0),
                egui::Align2::LEFT_CENTER,
                glyph,
                egui::FontId::proportional(16.0),
                color,
            );
            let label = ui.painter().text(
                rect.left_center() + egui::vec2(48.0, 0.0),
                egui::Align2::LEFT_CENTER,
                title,
                egui::FontId::proportional(15.0),
                color,
            );
            if !page.ready() {
                ui.painter().text(
                    egui::pos2(label.max.x + 8.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "soon",
                    egui::FontId::proportional(12.0),
                    pal.disabled,
                );
            }
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

    /// Projects with the switch-off list of older Claude Code versions: what is on because Claude Code no longer
    /// reads it, and a button that moves it into the list it does read. Read-only until the user reviews the plan.
    fn legacy_notice(&mut self, ui: &mut Ui) {
        let lists = &self.snapshot.legacy_disabled;
        if lists.is_empty() {
            return;
        }
        const SHOWN: usize = 4;
        let pal = theme::palette(ui.ctx());
        let can_migrate =
            self.core.is_some() && self.can_prepare() && lists.iter().any(|l| !l.malformed);
        let mut migrate = false;
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let title = if lists.len() == 1 {
                    "Old switch-off list in 1 project".to_owned()
                } else {
                    format!("Old switch-off lists in {} projects", lists.len())
                };
                ui.label(
                    RichText::new(format!("{} {title}", icon::CLOCK_COUNTER_CLOCKWISE))
                        .color(pal.warn),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Older Claude Code versions kept switched-off servers in mcpServersDisabled. Claude Code no longer reads that list, so servers on it can be on again. Migrating moves them to the list it reads now.",
                        )
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
                for l in lists.iter().take(SHOWN) {
                    let folder = folder_name(&l.shown);
                    let line = if l.malformed {
                        format!("{folder}: holds something other than server names; edit it in .claude.json by hand")
                    } else {
                        let state = match l.turns_off.as_slice() {
                            [] => "already off".to_owned(),
                            [one] => format!("{one} is on"),
                            many => format!("{} are on", some_names(many)),
                        };
                        format!("{folder}: {} ({state})", some_names(&l.names))
                    };
                    ui.add(egui::Label::new(line).wrap())
                        .on_hover_text(l.shown.as_str());
                }
                if lists.len() > SHOWN {
                    ui.label(
                        RichText::new(format!("… and {} more", lists.len() - SHOWN))
                            .color(pal.text_muted),
                    );
                }
                migrate = ui
                    .add_enabled(can_migrate, egui::Button::new("Migrate…"))
                    .on_hover_text("Review the change before anything is written")
                    .on_disabled_hover_text(if self.core.is_none() {
                        "Not connected to the configuration"
                    } else if lists.iter().all(|l| l.malformed) {
                        "Only lists of server names can be migrated"
                    } else {
                        "Finish the open review first"
                    })
                    .clicked();
            });
        ui.add_space(6.0);
        if migrate {
            self.migrate_legacy();
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        // Only what works today: a button that can never be pressed is noise (Add, Move, Re-authorize, Export and
        // Import arrive with M2 and M3).
        const ACTIONS: [(&str, &str); 5] = [
            (icon::PENCIL_SIMPLE, "Edit (Ctrl+E)"),
            (icon::COPY, "Duplicate (Ctrl+D)"),
            (icon::TOGGLE_RIGHT, "Turn on"),
            (icon::TOGGLE_LEFT, "Turn off"),
            (icon::TRASH, "Delete (Del)"),
        ];
        let can_toggle = self.core.is_some() && !self.selected.is_empty() && self.can_prepare();
        let can_edit = self.core.is_some() && self.selected.len() == 1;
        let can_duplicate = self.duplicable().is_some();
        // Wraps onto a second line when the window is narrow: buttons past the edge would be cut off and unreachable.
        ui.horizontal_wrapped(|ui| {
            for (glyph, tip) in ACTIONS {
                let button = egui::Button::new(RichText::new(glyph).size(16.0))
                    .min_size(egui::vec2(44.0, 38.0));
                let (enabled, needs) = match glyph {
                    g if g == icon::TOGGLE_RIGHT || g == icon::TOGGLE_LEFT || g == icon::TRASH => {
                        (can_toggle, "select a server first")
                    }
                    g if g == icon::COPY => (can_duplicate, "select one user or local server"),
                    _ => (can_edit, "select one server"),
                };
                let clicked = ui
                    .add_enabled(enabled, button)
                    .on_hover_text(tip)
                    .on_disabled_hover_text(format!("{tip}: {needs}"))
                    .clicked();
                if clicked {
                    match glyph {
                        g if g == icon::TOGGLE_RIGHT => self.prepare_toggle(true),
                        g if g == icon::TOGGLE_LEFT => self.prepare_toggle(false),
                        g if g == icon::TRASH => self.prepare_delete(),
                        g if g == icon::COPY => self.open_duplicate(),
                        _ => self.edit_selected(),
                    }
                }
            }
            if !self.selected.is_empty() {
                ui.separator();
                ui.label(
                    RichText::new(format!("{} selected", self.selected.len()))
                        .color(pal.text_muted),
                );
            }
        });
    }

    /// Everything the command palette offers right now: actions on the selection first, then the rest.
    fn commands(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let selected: Vec<&ServerRow> = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .collect();
        let names = selected
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let live = self.core.is_some();
        let mut add = |icon: &'static str,
                       title: String,
                       subtitle: String,
                       key: Option<String>,
                       action: command::Action| {
            out.push(Entry {
                icon,
                title,
                subtitle,
                shortcut: key,
                action,
            });
        };
        if live && !selected.is_empty() {
            let what = if selected.len() == 1 {
                names.clone()
            } else {
                format!("{} servers", selected.len())
            };
            add(
                icon::TOGGLE_RIGHT,
                format!("Turn on {what}"),
                String::new(),
                None,
                command::Action::TurnOn,
            );
            add(
                icon::TOGGLE_LEFT,
                format!("Turn off {what}"),
                String::new(),
                None,
                command::Action::TurnOff,
            );
            if selected.len() == 1 {
                let s = selected[0];
                add(
                    icon::PENCIL_SIMPLE,
                    format!("Edit {}", s.name),
                    "Command, address, arguments, environment".into(),
                    Some(shortcut("E")),
                    command::Action::Edit,
                );
                if s.scope != Scope::Project {
                    add(
                        icon::COPY,
                        format!("Duplicate {}", s.name),
                        "To another project or the user".into(),
                        Some(shortcut("D")),
                        command::Action::Duplicate,
                    );
                }
                if s.casing_collision {
                    add(
                        icon::WARNING,
                        format!("Fix casing of {}", s.name),
                        "Give servers that differ only in case one spelling".into(),
                        None,
                        command::Action::FixCasing,
                    );
                }
                add(
                    icon::PULSE,
                    format!("Check {} now", s.name),
                    String::new(),
                    None,
                    command::Action::Check,
                );
                add(
                    icon::STETHOSCOPE,
                    format!("Diagnose {}", s.name),
                    "Explain what goes wrong".into(),
                    None,
                    command::Action::Diagnose,
                );
                if s.scope == Scope::Project && s.approval.is_some_and(|a| a != Approval::Approved)
                {
                    add(
                        icon::SHIELD_CHECK,
                        format!("Review and approve {}", s.name),
                        String::new(),
                        None,
                        command::Action::Approve,
                    );
                }
            }
            add(
                icon::TRASH,
                format!("Delete {what}"),
                String::new(),
                Some("Del".into()),
                command::Action::Delete,
            );
        }
        if live && self.snapshot.legacy_disabled.iter().any(|l| !l.malformed) {
            add(
                icon::CLOCK_COUNTER_CLOCKWISE,
                "Migrate old switch-off lists".into(),
                "Switch off what older Claude Code versions had off".into(),
                None,
                command::Action::MigrateLegacy,
            );
        }
        if live {
            add(
                icon::ARROWS_CLOCKWISE,
                "Refresh".into(),
                "Read the configuration and check every server".into(),
                Some("F5".into()),
                command::Action::Refresh,
            );
        }
        add(
            icon::MAGNIFYING_GLASS,
            "Filter servers".into(),
            String::new(),
            Some(shortcut("F")),
            command::Action::Search,
        );
        for choice in ThemeChoice::ALL {
            add(
                choice.icon(),
                format!("Theme: {}", choice.label()),
                String::new(),
                None,
                command::Action::Theme(choice),
            );
        }
        for tone in Tone::ALL {
            add(
                icon::PALETTE,
                format!("Greys: {}", tone.label()),
                String::new(),
                None,
                command::Action::Tone(tone),
            );
        }
        add(
            icon::GEAR,
            "Settings".into(),
            "Appearance".into(),
            None,
            command::Action::Settings,
        );
        for i in self.visible() {
            let s = &self.snapshot.servers[i];
            add(
                icon::ARROW_RIGHT,
                format!("Go to {}", s.name),
                group_of(s).full,
                None,
                command::Action::Select(i),
            );
        }
        out
    }

    fn run(&mut self, ctx: &egui::Context, action: command::Action) {
        use command::Action as A;
        match action {
            A::Select(i) => {
                self.selected.clear();
                self.selected.insert(i);
                self.page = Page::Connections;
            }
            A::TurnOn => self.prepare_toggle(true),
            A::TurnOff => self.prepare_toggle(false),
            A::Delete => self.prepare_delete(),
            A::Edit => self.edit_selected(),
            A::Check | A::Diagnose | A::Approve => {
                if let Some(s) = self.single_selected() {
                    let id = s.id();
                    match action {
                        A::Check => self.send(Command::Check(id)),
                        A::Diagnose => {
                            self.tab = DetailTab::Status;
                            self.send(Command::Diagnose(id));
                        }
                        _ => {
                            self.outcome = None;
                            self.send(Command::Prepare(vec![Change::Approve { id }]));
                        }
                    }
                }
            }
            A::Refresh => self.refresh(),
            A::Search => ctx.memory_mut(|m| m.request_focus(search_id())),
            A::Theme(choice) => {
                self.theme = choice;
                theme::set_choice(ctx, choice);
            }
            A::Tone(tone) => {
                self.tone = tone;
                theme::set_tone(ctx, tone);
            }
            A::Settings => self.page = Page::Settings,
            A::Duplicate => self.open_duplicate(),
            A::FixCasing => self.open_casing(),
            A::MigrateLegacy => self.migrate_legacy(),
        }
    }

    /// Asks for one plan that moves every old switch-off list that can be moved.
    fn migrate_legacy(&mut self) {
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

    /// The one selected server, if it can be duplicated (project servers cannot yet).
    fn duplicable(&self) -> Option<&ServerRow> {
        self.core.as_ref()?;
        if !self.can_prepare() {
            return None;
        }
        self.single_selected().filter(|s| s.scope != Scope::Project)
    }

    /// Opens the Duplicate dialog for the selected server: named `<name>_copy`, in the place it is now.
    fn open_duplicate(&mut self) {
        let Some(row) = self.duplicable() else {
            return;
        };
        let here = row.project.as_deref();
        let projects: Vec<(String, bool)> = self
            .snapshot
            .projects
            .iter()
            .map(|p| (p.clone(), Some(p.as_str()) == here))
            .collect();
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
        self.duplicate = Some(DuplicateForm {
            source: row.id(),
            about: format!("{} \u{b7} {}", row.transport.label(), row.target),
            name: format!("{}_copy", row.name),
            user: row.scope == Scope::User,
            projects,
            order,
            shared_folders,
            filter: String::new(),
            focus: true,
        });
    }

    /// Opens the Fix casing dialog for the selected server: every server that differs from it only in case.
    fn open_casing(&mut self) {
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

    fn casing_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
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
        } else if cancel || modal.should_close() {
            self.casing = None;
        }
    }

    fn duplicate_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(form) = &mut self.duplicate else {
            return;
        };
        let problem = ServerName::parse(&form.name).err().map(|e| {
            let e = e.to_string();
            let mut c = e.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or(e)
        });
        let taken = form.taken(&self.snapshot.servers);
        let places = form.places();
        let ready = problem.is_none() && taken.is_none() && places.is_some();
        let mut review = ready && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        // The list of projects gets what the window has left once the rest of the dialog is laid out.
        let list_height = (ctx.content_rect().height() - 500.0).clamp(96.0, 320.0);
        let modal = egui::Modal::new(egui::Id::new("duplicate_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(format!("Duplicate {}", form.source.name));
            ui.add(
                egui::Label::new(RichText::new(&form.about).small().color(pal.text_muted))
                    .truncate(),
            );
            ui.add_space(12.0);
            ui.label(RichText::new("Name").color(pal.text_muted));
            let field =
                ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(f32::INFINITY));
            if std::mem::take(&mut form.focus) {
                field.request_focus();
            }
            if let Some(problem) = &problem {
                ui.label(RichText::new(problem).small().color(pal.danger));
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Add it").color(pal.text_muted));
            place_row(ui, &mut form.user, "For the user", "in every project", None);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let ticked = form.projects.iter().filter(|(_, on)| *on).count();
                let mut heading = format!("Projects ({})", form.projects.len());
                if ticked > 0 {
                    heading.push_str(&format!(", {ticked} ticked"));
                }
                ui.label(RichText::new(heading).color(pal.text_muted));
                // A short list is read at a glance; a long one is searched.
                if form.projects.len() > 8 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.filter)
                                .hint_text(format!("{} Filter projects", icon::MAGNIFYING_GLASS))
                                .desired_width(220.0),
                        );
                    });
                }
            });
            // As tall as the whole list would be, up to that room, and no shorter while filtering: the dialog keeps
            // its size and its buttons stay where they are as you type.
            let rows = form.projects.len().max(1) as f32;
            let height = (rows * (PLACE_ROW + PLACE_GAP)).min(list_height);
            egui::ScrollArea::vertical()
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = PLACE_GAP;
                    let needle = form.filter.trim().to_lowercase();
                    let mut shown = 0;
                    for &i in &form.order {
                        let (path, on) = &mut form.projects[i];
                        if !path.to_lowercase().contains(&needle) {
                            continue;
                        }
                        shown += 1;
                        let here = form.source.project.as_deref() == Some(path.as_str());
                        let folder = folder_name(path);
                        let parent = if form.shared_folders.contains(&folder.to_lowercase()) {
                            parent_folder(path)
                        } else {
                            ""
                        };
                        place_row(ui, on, folder, parent, here.then_some("where it is now"))
                            .on_hover_text(path.as_str());
                    }
                    if shown == 0 {
                        ui.label(RichText::new("No project matches.").color(pal.text_muted));
                    }
                });
            ui.add_space(10.0);
            // What Review will lead to, or why it cannot yet.
            if let Some(taken) = &taken {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} already exists {taken}. Choose another name, or untick it there.",
                            form.name
                        ))
                        .color(pal.danger),
                    )
                    .wrap(),
                );
            } else if let Some(places) = &places {
                if problem.is_none() {
                    ui.add(egui::Label::new(format!("Adds {} {places}.", form.name)).wrap());
                }
            } else {
                ui.label(RichText::new("Tick at least one place.").color(pal.text_muted));
            }
            ui.label(
                RichText::new("The copy keeps the original's environment and header values.")
                    .small()
                    .color(pal.text_muted),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let confirm =
                    egui::Button::new(RichText::new("Review").strong().color(pal.on_bright))
                        .fill(pal.primary)
                        .min_size(egui::vec2(96.0, 34.0));
                review |= ui
                    .add_enabled(ready, confirm)
                    .on_hover_text("Enter")
                    .on_disabled_hover_text("Give it a usable name and tick a place")
                    .clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
        });
        if review {
            let targets = form
                .user
                .then_some(Target::User)
                .into_iter()
                .chain(form.ticked().map(|p| Target::Local(p.to_owned())))
                .collect();
            let change = Change::Duplicate {
                id: form.source.clone(),
                name: form.name.clone(),
                targets,
            };
            self.duplicate = None;
            self.prepare(vec![change]);
        } else if cancel || modal.should_close() {
            self.duplicate = None;
        }
    }

    fn single_selected(&self) -> Option<&ServerRow> {
        let mut it = self.selected.iter();
        match (it.next(), it.next()) {
            (Some(&i), None) => self.snapshot.servers.get(i),
            _ => None,
        }
    }

    /// Settings. For now, how Klefki looks: light or dark, and which family of greys.
    fn settings(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(8.0);
        ui.label(RichText::new("Settings").heading());
        ui.add_space(16.0);
        ui.label(RichText::new("Appearance").strong());
        ui.add_space(6.0);
        egui::Grid::new("appearance")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Theme").color(pal.text_muted));
                ui.horizontal(|ui| {
                    for choice in ThemeChoice::ALL {
                        let text = format!("{} {}", choice.icon(), choice.label());
                        if ui.selectable_value(&mut self.theme, choice, text).changed() {
                            theme::set_choice(ui.ctx(), choice);
                        }
                    }
                });
                ui.end_row();
                ui.label(RichText::new("Greys").color(pal.text_muted));
                ui.horizontal(|ui| {
                    for tone in Tone::ALL {
                        if ui
                            .selectable_value(&mut self.tone, tone, tone.label())
                            .changed()
                        {
                            theme::set_tone(ui.ctx(), tone);
                        }
                    }
                });
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Both keep every text at a contrast of at least 4.5:1.")
                .small()
                .color(pal.text_muted),
        );
    }

    /// Moves the selection `step` rows through the list as shown, starting at the top when nothing is selected.
    fn move_selection(&mut self, step: i32) {
        let order = self.visible();
        if order.is_empty() {
            return;
        }
        let at = self
            .selected
            .iter()
            .next()
            .and_then(|i| order.iter().position(|o| o == i));
        let next = match at {
            None if step > 0 => 0,
            None => order.len() - 1,
            Some(p) => (p as i32 + step).clamp(0, order.len() as i32 - 1) as usize,
        };
        self.selected.clear();
        self.selected.insert(order[next]);
    }

    /// Asks the engine what deleting the selected servers would change. Nothing is written yet.
    fn prepare_delete(&mut self) {
        let changes: Vec<Change> = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .map(|row| Change::Delete { id: row.id() })
            .collect();
        if !changes.is_empty() {
            self.outcome = None;
            self.send(Command::Prepare(changes));
        }
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
            self.prepare(changes);
        }
    }

    /// No plan is open or on its way, so a new one may be asked for.
    fn can_prepare(&self) -> bool {
        self.plan.is_none() && !self.preparing
    }

    fn prepare(&mut self, changes: Vec<Change>) {
        if self.can_prepare() {
            self.outcome = None;
            self.preparing = true;
            self.send(Command::Prepare(changes));
        }
    }

    /// The banner for the last change.
    /// The outcome of the last change, as a small card in the bottom-right corner: it does not push the list
    /// around, a success fades after a few seconds, and a problem stays until dismissed.
    fn outcome_toast(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let now = ctx.input(|i| i.time);
        let Some(outcome) = self.outcome.as_mut() else {
            return;
        };
        let shown_at = *outcome.shown_at.get_or_insert(now);
        if outcome.ok {
            let left = SUCCESS_SECONDS - (now - shown_at);
            if left <= 0.0 {
                self.outcome = None;
                return;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        }
        let outcome = outcome.clone();
        let (color, glyph) = if outcome.ok {
            (pal.success, icon::CHECK_CIRCLE)
        } else {
            (pal.danger, icon::WARNING_CIRCLE)
        };
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("outcome"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(pal.panel_raised)
                    .stroke(egui::Stroke::new(1.0, color))
                    .corner_radius(8)
                    .inner_margin(Margin::same(12))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(80),
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(glyph).size(16.0).color(color));
                            ui.add(
                                egui::Label::new(RichText::new(&outcome.text).color(pal.text))
                                    .wrap(),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                dismiss = true;
                            }
                        });
                    });
            });
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
            ui.set_width(540.0);
            // What happens, in words, first; the exact lines follow for those who want them.
            if plan.summary.is_empty() {
                ui.heading("Review the change");
            } else {
                ui.label(RichText::new("Review the change").color(pal.text_muted));
                for line in &plan.summary {
                    ui.add(egui::Label::new(RichText::new(line).heading()).wrap());
                }
            }
            ui.add_space(8.0);
            if plan.nothing_to_do {
                ui.label("The configuration already looks like this. Nothing would be written.");
            }
            for review in &plan.reviews {
                ui.add_space(4.0);
                Frame::new()
                    .stroke(egui::Stroke::new(1.0, pal.warn))
                    .corner_radius(8)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&review.title).strong().color(pal.warn));
                        for line in &review.lines {
                            ui.add(egui::Label::new(RichText::new(line).monospace()).wrap());
                        }
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
            for file in &plan.files {
                if file.lines.is_empty() {
                    continue;
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("Changes in {}", file.path))
                        .small()
                        .color(pal.text_muted),
                );
                Frame::new()
                    .fill(pal.field)
                    .corner_radius(8)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
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
                                        egui::Label::new(
                                            RichText::new(line).monospace().color(color),
                                        )
                                        .wrap(),
                                    );
                                }
                            });
                    });
            }
            for note in &plan.notes {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} {note}", icon::INFO))
                            .small()
                            .color(pal.text_muted),
                    )
                    .wrap(),
                );
            }
            ui.add_space(12.0);
            let label = if !plan.action.is_empty() {
                plan.action.as_str()
            } else if plan.reviews.is_empty() {
                "Apply"
            } else {
                "Approve"
            };
            ui.horizontal(|ui| {
                let fill = if plan.destructive {
                    pal.danger
                } else {
                    pal.primary
                };
                let confirm = egui::Button::new(RichText::new(label).strong().color(pal.on_bright))
                    .fill(fill)
                    .min_size(egui::vec2(96.0, 34.0));
                let enabled = !plan.nothing_to_do && !applying;
                if ui
                    .add_enabled(enabled, confirm)
                    .on_hover_text("Enter")
                    .clicked()
                {
                    apply = true;
                }
                // Once Apply is pressed the engine writes the plan; closing now would hide that.
                if ui
                    .add_enabled(
                        !applying,
                        egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)),
                    )
                    .on_hover_text("Esc")
                    .clicked()
                {
                    cancel = true;
                }
                if applying {
                    // Text, not a spinner: an animation would repaint every frame for a write that takes milliseconds.
                    ui.label(RichText::new("Saving…").color(pal.text_muted));
                }
                // Enter confirms, as in any dialog; not while a field would take it.
                if enabled
                    && !ui.ctx().egui_wants_keyboard_input()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    apply = true;
                }
            });
        });
        if modal.should_close() && !applying {
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
        // Group headers may run across the row's empty cells, but no further than the table itself.
        let right = ui.max_rect().right();
        TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(220.0).at_least(120.0))
            .column(Column::exact(64.0))
            .column(Column::exact(176.0))
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
                    GridRow::Group(group) => {
                        row.col(|ui| group_header(ui, group, right));
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
                        let hint = self.hint(*i);
                        row.col(|ui| status_cell(ui, s, status, detail, hint));
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
        ui.label(RichText::new(&s.name).heading());
        ui.horizontal(|ui| {
            flat_items(ui);
            // Compact pills: smaller text and padding, so the tabs sit on one line and do not shout.
            ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            for (tab, title) in DetailTab::ALL {
                ui.selectable_value(&mut self.tab, tab, RichText::new(title).size(13.5));
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
                if self.core.is_some() {
                    ui.add_space(12.0);
                    self.definition_form(ui, s);
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
                        if explanation.is_none() {
                            ui.label(RichText::new("Detail").color(pal.text_muted));
                            ui.add(egui::Label::new(detail.as_deref().unwrap_or("-")).wrap());
                            ui.end_row();
                        }
                    });
                if let Some(explanation) = &explanation {
                    ui.add_space(10.0);
                    explanation.show(ui, ui.id().with("status"));
                }
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
                        muted(
                            ui,
                            "Signing in again and changing the client arrive with the add wizard; Claude Code keeps the sign-in itself.",
                        );
                    }
                    None => muted(
                        ui,
                        "This server does not sign in with OAuth. Headers and tokens are in the Env tab.",
                    ),
                }
            }
            DetailTab::HeadersEnv => {
                if self.core.is_some() {
                    self.values_form(ui, s);
                } else {
                    ui.label(RichText::new("Not available for sample data.").color(pal.text_muted));
                }
            }
        }
    }

    /// Edit (toolbar, Ctrl+E, palette): the selected server's definition, in the form on the General tab.
    fn edit_selected(&mut self) {
        let Some(s) = self.single_selected().cloned() else {
            return;
        };
        self.tab = DetailTab::General;
        if self.core.is_some() && self.editor_for(&s).is_none() {
            self.open_editor(&s);
        }
    }

    /// Whether `s`'s definition is being edited as JSON instead of in the form.
    fn editing_as_json(&mut self, s: &ServerRow) -> bool {
        matches!(
            self.editor_for(s),
            Some(EditorState::Ready { as_json, form, .. }) if *as_json || form.is_err()
        )
    }

    /// The definition as JSON, in place of the form, for what the form does not show. Saved through the same
    /// review as the form.
    fn json_editor(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        let Some(EditorState::Ready { text, form, .. }) = self.editor_for(s) else {
            return;
        };
        if let Err(why) = form.as_ref() {
            ui.add(egui::Label::new(RichText::new(why.as_str()).color(pal.warn)).wrap());
            ui.add_space(4.0);
        }
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Values shown as <hidden> keep what they are now. Type a new value to change one.",
                )
                .color(pal.text_muted),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(text)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(14),
        );
        ui.add_space(6.0);
        self.save_bar(ui, s);
    }

    /// The OAuth client secret of `s`: whether the system's secret store holds one, and setting, replacing or
    /// forgetting it. Asked for once per server shown.
    fn client_secret(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        if !self.secret.as_ref().is_some_and(|v| s.is(&v.id)) {
            self.secret = Some(SecretView {
                id: s.id(),
                known: None,
                busy: Some("Checking…"),
                confirm_forget: false,
            });
            self.secret_input = None;
            self.send(Command::SecretStatus(s.id()));
        }
        let Some(view) = &mut self.secret else {
            return;
        };
        let mut send = None;
        ui.label(RichText::new("Client secret").color(pal.text_muted));
        if let Some(busy) = view.busy {
            ui.label(RichText::new(busy).color(pal.text_muted));
        } else {
            match &view.known {
                Some(Ok(true)) => {
                    ui.label("Stored in your system's secret store.");
                    ui.horizontal(|ui| {
                        if ui.button("Replace…").clicked() {
                            self.secret_input = Some(Secret::new(String::new()));
                        }
                        if ui.button("Forget…").clicked() {
                            view.confirm_forget = true;
                        }
                    });
                }
                Some(Ok(false)) => {
                    ui.label(RichText::new("Not stored.").color(pal.text_muted));
                    if self.secret_input.is_none() && ui.button("Set client secret…").clicked() {
                        self.secret_input = Some(Secret::new(String::new()));
                    }
                }
                Some(Err(why)) => {
                    ui.add(egui::Label::new(RichText::new(why.as_str()).color(pal.danger)).wrap());
                    if ui.button("Try again").clicked() {
                        send = Some(Command::SecretStatus(s.id()));
                        view.busy = Some("Checking…");
                    }
                }
                None => {}
            }
        }
        if view.confirm_forget && view.busy.is_none() {
            ui.add(
                egui::Label::new(
                    RichText::new("Forget the client secret? This cannot be undone; you would have to type it again.")
                        .color(pal.warn),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                if ui.button("Forget it").clicked() {
                    send = Some(Command::ForgetSecret(s.id()));
                    view.busy = Some("Forgetting…");
                    view.confirm_forget = false;
                }
                if ui.button("Keep").clicked() {
                    view.confirm_forget = false;
                }
            });
        }
        if view.busy.is_none()
            && let Some(input) = &mut self.secret_input
        {
            let (mut save, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(input.edit())
                        .password(true)
                        .hint_text("Client secret")
                        .desired_width(220.0),
                );
                save = ui
                    .add_enabled(!input.is_empty(), egui::Button::new("Save"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if save {
                // Moved, not copied: the field's value goes to the engine and the app keeps nothing.
                if let Some(secret) = self.secret_input.take() {
                    send = Some(Command::StoreSecret { id: s.id(), secret });
                    view.busy = Some("Saving…");
                }
            } else if cancel {
                self.secret_input = None;
            }
        }
        if let Some(command) = send {
            self.send(command);
        }
    }

    /// Asks the engine for `s`'s definition, for the form and the JSON editor.
    fn open_editor(&mut self, s: &ServerRow) {
        self.editor = Some(Editor {
            id: s.id(),
            state: EditorState::Loading,
        });
        self.send(Command::Inspect(s.id()));
    }

    /// The editor's state for `s`, if its definition was asked for.
    fn editor_for(&mut self, s: &ServerRow) -> Option<&mut EditorState> {
        self.editor
            .as_mut()
            .filter(|e| s.is(&e.id))
            .map(|e| &mut e.state)
    }

    /// Why the form is not shown for `s`, or `None` when it can be edited. Draws that reason.
    fn form_unavailable(&mut self, ui: &mut Ui, s: &ServerRow) -> bool {
        let pal = theme::palette(ui.ctx());
        let muted = |ui: &mut Ui, text: &str, color: Color32| {
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        };
        let Some(state) = self.editor_for(s) else {
            // The definition is read only when asked for: `.claude.json` can be tens of megabytes.
            if ui
                .button("Edit definition")
                .on_hover_text("Change the command, address, arguments, environment or headers")
                .clicked()
            {
                self.open_editor(s);
            }
            return true;
        };
        match state {
            EditorState::Loading => muted(ui, "Loading…", pal.text_muted),
            EditorState::Failed(why) => muted(ui, why, pal.danger),
            EditorState::Ready {
                editable: false,
                original,
                ..
            } => {
                muted(
                    ui,
                    "Defined in the repository's .mcp.json. Klefki shows it but does not edit files in a repository.",
                    pal.text_muted,
                );
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut original.as_str())
                        .code_editor()
                        .desired_width(f32::INFINITY),
                );
            }
            EditorState::Ready { form, .. }
                if form
                    .as_ref()
                    .as_ref()
                    .is_ok_and(Definition::signs_in_with_oauth) =>
            {
                muted(
                    ui,
                    "Signs in with OAuth: it is changed through Claude Code's own command, which Klefki does not run yet.",
                    pal.text_muted,
                )
            }
            EditorState::Ready { .. } => return false,
        }
        true
    }

    /// The General tab's form: type, then command and arguments, or address and header command.
    fn definition_form(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        ui.label(RichText::new("Definition").color(pal.text_muted));
        ui.add_space(4.0);
        if self.form_unavailable(ui, s) {
            return;
        }
        if self.editing_as_json(s) {
            self.json_editor(ui, s);
            return;
        }
        let Some(EditorState::Ready { form, .. }) = self.editor_for(s) else {
            return;
        };
        let Ok(f) = form.as_mut() else {
            return;
        };
        let width = ui.available_width();
        egui::Grid::new("definition")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Type").color(pal.text_muted));
                egui::ComboBox::from_id_salt("definition-type")
                    .selected_text(transport_name(f.transport))
                    .show_ui(ui, |ui| {
                        for t in [
                            TransportKind::Stdio,
                            TransportKind::Http,
                            TransportKind::Sse,
                            TransportKind::Ws,
                        ] {
                            ui.selectable_value(&mut f.transport, t, transport_name(t));
                        }
                    });
                ui.end_row();
                let field = |ui: &mut Ui, label: &str, value: &mut String| {
                    let l = ui.label(RichText::new(label).color(pal.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(value)
                            .desired_width((width - 120.0).max(120.0))
                            .font(egui::TextStyle::Monospace),
                    )
                    .labelled_by(l.id);
                    ui.end_row();
                };
                if f.transport == TransportKind::Stdio {
                    field(ui, "Command", &mut f.command);
                } else {
                    field(ui, "Address", &mut f.url);
                    field(ui, "Header command", &mut f.headers_helper);
                }
            });
        // Arguments under their own heading rather than in the grid, which would centre the label beside the list.
        if f.transport == TransportKind::Stdio {
            ui.add_space(6.0);
            ui.label(RichText::new("Arguments").color(pal.text_muted));
            let mut remove = None;
            for (i, arg) in f.args.iter_mut().enumerate() {
                // Right to left: the button takes its room first and the field exactly what is left, so the row
                // never asks for more than the panel has (a resizable panel would grow to fit, frame after frame).
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        if icon_button(ui, icon::X, &format!("Remove argument {}", i + 1)) {
                            remove = Some(i);
                        }
                        ui.add(
                            egui::TextEdit::singleline(arg)
                                .desired_width(ui.available_width())
                                .font(egui::TextStyle::Monospace),
                        );
                    },
                );
            }
            if let Some(i) = remove {
                f.args.remove(i);
            }
            if ui.small_button("Add argument").clicked() {
                f.args.push(String::new());
            }
        }
        ui.add_space(8.0);
        self.save_bar(ui, s);
    }

    /// The Env tab's form: environment variables for a command, headers for an address. Values that are there
    /// stay hidden; a new one is typed without being shown.
    fn values_form(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        if self.form_unavailable(ui, s) {
            return;
        }
        // Environment and headers are part of the definition: while it is edited as JSON, they are there.
        if self.editing_as_json(s) {
            self.json_editor(ui, s);
            return;
        }
        let Some(EditorState::Ready { form, .. }) = self.editor_for(s) else {
            return;
        };
        let Ok(f) = form.as_mut() else {
            return;
        };
        let (pairs, title, noun) = if f.transport == TransportKind::Stdio {
            (&mut f.env, "Environment", "variable")
        } else {
            (&mut f.headers, "Headers", "header")
        };
        ui.label(RichText::new(title).color(pal.text_muted));
        ui.add_space(4.0);
        let mut remove = None;
        for (i, (name, value)) in pairs.iter_mut().enumerate() {
            // Right to left, as for arguments: the buttons first, then name and value share what is left.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                Layout::right_to_left(Align::Center),
                |ui| {
                    if icon_button(ui, icon::X, &format!("Remove {name}")) {
                        remove = Some(i);
                    }
                    let hidden = value == Definition::HIDDEN;
                    if hidden && icon_button(ui, icon::PENCIL_SIMPLE, &format!("Change {name}")) {
                        value.clear();
                    }
                    let half =
                        ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(40.0);
                    if hidden {
                        ui.add_sized(
                            [half, ui.spacing().interact_size.y],
                            egui::Label::new(RichText::new("hidden").color(pal.text_muted)),
                        )
                        .on_hover_text("Kept as it is. Change it to type a new value.");
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(half)
                                .hint_text("Value")
                                .password(true),
                        );
                    }
                    ui.add(
                        egui::TextEdit::singleline(name)
                            .desired_width(ui.available_width())
                            .hint_text("Name")
                            .font(egui::TextStyle::Monospace),
                    );
                },
            );
            if let Some(reference) = Definition::secret_suggestion(name, value) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Looks like a secret.").color(pal.warn));
                    if ui
                        .small_button(format!("Use {reference} instead"))
                        .on_hover_text(
                            "Claude Code fills the variable in from its own environment, so the secret stays out of ~/.claude.json.",
                        )
                        .clicked()
                    {
                        *value = reference;
                    }
                });
            }
        }
        if let Some(i) = remove {
            pairs.remove(i);
        }
        if ui.small_button(format!("Add {noun}")).clicked() {
            pairs.push((String::new(), String::new()));
        }
        ui.add_space(8.0);
        self.save_bar(ui, s);
    }

    /// What stops saving, then Review and save… and Revert, and on the right the way to the JSON and back.
    fn save_bar(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        let can_prepare = self.can_prepare();
        let Some(EditorState::Ready {
            original,
            text,
            problem,
            form,
            as_json,
            ..
        }) = self.editor_for(s)
        else {
            return;
        };
        if let Some(problem) = problem {
            let mut c = problem.chars();
            let shown: String = c
                .next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(shown).color(pal.danger)).wrap());
        }
        let edited = text != original;
        let ready = edited && problem.is_none() && can_prepare;
        let mut review = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Review and save…"))
                .clicked()
            {
                review = Some(text.clone());
            }
            if ui
                .add_enabled(edited, egui::Button::new("Revert"))
                .clicked()
            {
                *text = original.clone();
            }
        });
        // Quiet, on a line of its own under the buttons (beside them it would not fit a narrow panel): the fields
        // are the way to edit, the JSON the way out when they fall short.
        let quiet =
            |text: &str| egui::Button::new(RichText::new(text).color(pal.link)).frame(false);
        if *as_json || form.is_err() {
            if ui
                .add_enabled(form.is_ok(), quiet("Back to the form"))
                .on_disabled_hover_text("The form cannot show the JSON as it is now: see above")
                .clicked()
            {
                *as_json = false;
            }
        } else if ui
            .add(quiet("Edit as JSON"))
            .on_hover_text("Every key of the definition, including those the form does not show")
            .clicked()
        {
            *as_json = true;
        }
        if let Some(json) = review {
            self.prepare(vec![Change::Replace { id: s.id(), json }]);
        }
    }
}

impl eframe::App for ShellApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        theme::save_choice(storage, self.theme);
        theme::save_tone(storage, self.tone);
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

fn bar_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.chrome)
        .stroke(egui::Stroke::new(1.0, pal.divider))
        .inner_margin(Margin::symmetric(12, 6))
}

fn search_id() -> egui::Id {
    egui::Id::new("search")
}

/// What the Fix casing dialog holds: the servers that differ only in case, and the spelling they are to get.
struct CasingForm {
    members: Vec<CasingMember>,
    keep: String,
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

/// What the Duplicate dialog holds: the copy's name and where it goes.
struct DuplicateForm {
    source: ServerId,
    /// What is being copied, under the title: `http · https://…`.
    about: String,
    name: String,
    user: bool,
    /// Every project Claude Code knows, in the snapshot's order, and whether the copy goes there.
    projects: Vec<(String, bool)>,
    /// The order the projects are listed in: the one the server is in first, then by folder name.
    order: Vec<usize>,
    /// Folder names (lower case) that more than one project has: only those rows also say where they live. The rest
    /// would all repeat the same few parent folders.
    shared_folders: BTreeSet<String>,
    /// Narrows the list of projects: part of a path, any case.
    filter: String,
    /// Give the name field the keyboard on the first frame.
    focus: bool,
}

impl DuplicateForm {
    /// The projects ticked, in the snapshot's order.
    fn ticked(&self) -> impl Iterator<Item = &str> {
        self.projects
            .iter()
            .filter(|(_, on)| *on)
            .map(|(p, _)| p.as_str())
    }

    /// Where the copy goes, in words: `for the user and in a and b`. None when nothing is ticked.
    fn places(&self) -> Option<String> {
        let folders: Vec<&str> = self.ticked().map(folder_name).collect();
        places_in_words(self.user, &folders)
    }

    /// The ticked places that already have a server by the copy's name, in words. Only the lists a copy is written
    /// to count: the engine refuses those, while a name in a project's `.mcp.json` is only hidden by the copy, which
    /// the review warns about.
    fn taken(&self, servers: &[ServerRow]) -> Option<String> {
        let has = |scope: Scope, project: Option<&str>| {
            servers
                .iter()
                .any(|s| s.name == self.name && s.scope == scope && s.project.as_deref() == project)
        };
        let user = self.user && has(Scope::User, None);
        let folders: Vec<&str> = self
            .ticked()
            .filter(|p| has(Scope::Local, Some(p)))
            .map(folder_name)
            .collect();
        places_in_words(user, &folders)
    }
}

/// `for the user`, `in a and b`, `for the user and in 5 projects`; None for nowhere.
fn places_in_words(user: bool, folders: &[&str]) -> Option<String> {
    let projects = match folders {
        [] => None,
        [one] => Some(format!("in {one}")),
        [rest @ .., last] if folders.len() <= 3 => {
            Some(format!("in {} and {last}", rest.join(", ")))
        }
        _ => Some(format!("in {} projects", folders.len())),
    };
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
fn place_row(
    ui: &mut Ui,
    on: &mut bool,
    title: &str,
    detail: &str,
    tag: Option<&str>,
) -> egui::Response {
    let pal = theme::palette(ui.ctx());
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), PLACE_ROW), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let ticked = *on;
    response.widget_info(|| {
        let label = if detail.is_empty() {
            title.to_owned()
        } else {
            format!("{title}, {detail}")
        };
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, ticked, label)
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    if response.hovered() || response.has_focus() {
        painter.rect_filled(rect, 6.0, pal.hover);
    }
    let tick = egui::Rect::from_center_size(
        rect.left_center() + egui::vec2(18.0, 0.0),
        egui::vec2(18.0, 18.0),
    );
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
    let name = painter.text(
        egui::pos2(tick.right() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(15.0),
        pal.text,
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

/// Where a project folder lives: `C:/work/app` -> `C:/work`. Empty for a root.
fn parent_folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[..cut],
        _ => "",
    }
}

/// Names joined with commas; past the first few, only counted, so a long list stays readable.
fn some_names(names: &[String]) -> String {
    const NAMED: usize = 6;
    if names.len() <= NAMED + 1 {
        return names.join(", ");
    }
    format!(
        "{} and {} more",
        names[..NAMED].join(", "),
        names.len() - NAMED
    )
}

/// The last part of a project path: `C:/work/app` -> `app`.
fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[cut + 1..],
        _ => trimmed,
    }
}

fn group_of(s: &ServerRow) -> Group {
    match (&s.scope, &s.project) {
        (scope, Some(p)) => {
            let trimmed = p.trim_end_matches(['/', '\\']);
            let (parent, folder) = match trimmed.rfind(['/', '\\']) {
                Some(cut) if cut + 1 < trimmed.len() => (&trimmed[..cut], &trimmed[cut + 1..]),
                _ => ("", trimmed),
            };
            Group {
                title: format!("{} · {folder}", scope.label()),
                location: (!parent.is_empty()).then(|| parent.to_owned()),
                full: format!("{} · {p}", scope.label()),
                count: 0,
            }
        }
        (scope, None) => Group {
            title: scope.label().to_owned(),
            location: None,
            full: scope.label().to_owned(),
            count: 0,
        },
    }
}

/// The folder name stands out; where it lives is muted and cut to fit, with the whole path on hover. It is drawn,
/// not laid out, so a long path can run across the row without widening the column, and it reads as one label.
fn group_header(ui: &mut Ui, group: &Group, right: f32) {
    let pal = theme::palette(ui.ctx());
    let width = right;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &group.full));
    // Its own layer above the table, so the row's other cells (and their hover highlight) cannot paint over the
    // text; clipped to the table's width and to the scrolled area.
    let clip = ui.clip_rect();
    let painter = ui
        .ctx()
        .layer_painter(egui::LayerId::new(
            ui.layer_id().order,
            egui::Id::new("group-headers"),
        ))
        .with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(rect.min.x, clip.min.y.max(rect.min.y)),
            egui::pos2(width, clip.max.y.min(rect.max.y)),
        ));
    let font = egui::FontId::new(15.0, theme::bold_family());
    let title = painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        &group.title,
        font,
        pal.primary,
    );
    let mut x = title.max.x + 10.0;
    let count = format!("{}", group.count);
    let small = egui::FontId::proportional(13.0);
    let c = painter.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        count,
        small.clone(),
        pal.text_muted,
    );
    x = c.max.x + 12.0;
    if let Some(location) = &group.location {
        let room = (width - x - 24.0).max(0.0);
        let galley = ui.fonts_mut(|f| {
            let mut job =
                egui::text::LayoutJob::simple_singleline(location.clone(), small, pal.text_muted);
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            f.layout_job(job)
        });
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text_muted,
        );
    }
    response.on_hover_text(&group.full);
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

fn status_cell(
    ui: &mut Ui,
    s: &ServerRow,
    status: Status,
    detail: Option<&str>,
    hint: Option<&Hint>,
) {
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
    match status {
        Status::Failed | Status::NeedsAuth | Status::Degraded => {
            // Not egui's tooltip, which waits half a second for a perfectly still pointer over the words alone:
            // the whole cell answers, moving or not, after a moment that lets a sweep down the column pass by.
            if hovered_for(ui, response.id, ui.max_rect(), EXPLAIN_AFTER) {
                // Built only while shown, not every frame.
                response.show_tooltip_ui(|ui| {
                    if let Some(explanation) = Explanation::for_status(s, status, detail, hint) {
                        ui.set_max_width(560.0);
                        explanation.show(ui, ui.id().with("tooltip"));
                    }
                });
            }
        }
        _ => {
            if let Some(detail) = detail {
                response.on_hover_text(detail);
            }
        }
    }
}

/// Seconds the pointer rests on a failed status before it is explained.
const EXPLAIN_AFTER: f64 = 0.1;

/// Whether the pointer has been over `rect` for `delay` seconds, moving or not, with no button down. Asks for the
/// frame in which that comes true, so it does not wait for the next mouse event.
fn hovered_for(ui: &Ui, id: egui::Id, rect: egui::Rect, delay: f64) -> bool {
    let ctx = ui.ctx();
    let key = id.with("hovered-since");
    let busy = ctx.input(|i| i.pointer.any_down() || i.is_scrolling());
    if busy || !ui.rect_contains_pointer(rect) {
        ctx.data_mut(|d| d.remove::<f64>(key));
        return false;
    }
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or(key, now));
    let left = delay - (now - since);
    if left > 0.0 {
        ctx.request_repaint_after_secs(left as f32);
        return false;
    }
    true
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

/// A small icon button that reads as `label` to assistive technology.
fn icon_button(ui: &mut Ui, glyph: &str, label: &str) -> bool {
    let response = ui.small_button(glyph).on_hover_text(label);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.clicked()
}

fn transport_name(t: TransportKind) -> &'static str {
    match t {
        TransportKind::Stdio => "Command (stdio)",
        TransportKind::Http => "HTTP",
        TransportKind::Sse => "SSE",
        TransportKind::Ws => "WebSocket",
        TransportKind::Invalid => "Unknown",
    }
}

/// What the Auth tab knows about one server's client secret.
struct SecretView {
    id: ServerId,
    /// The store's last answer: stored or not, or why it could not say.
    known: Option<Result<bool, String>>,
    /// Waiting for the store: what to say meanwhile.
    busy: Option<&'static str>,
    /// Forget… was pressed; asking before doing it.
    confirm_forget: bool,
}

/// The row and the `live_gen` an explanation was built for.
type ExplainedFor = (usize, u64);

/// The engine's latest answer for one row.
#[derive(Clone)]
struct Live {
    status: Status,
    detail: Option<String>,
    hint: Option<Hint>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use klefki_core::api::{Definition, LegacyDisabled, fixture_snapshot};

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
        // (Was the Add button; the toolbar now shows only actions that work, so the check uses Edit.)
        let add = h.get_by_label(icon::PENCIL_SIMPLE).rect();
        assert!(
            add.width() >= 40.0 && add.height() >= 34.0,
            "toolbar button {add:?}"
        );
    }

    #[test]
    fn grid_renders_any_snapshot_not_just_the_fixture() {
        // Loading from a real config is tested in klefki-core; the UI only renders what it is given.
        let row = |name: &str| ServerRow {
            connection: klefki_core::api::ConnectionId::new(Scope::User, None, name),
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
            oauth: None,
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
        // (The Env tab was called "Headers/Env" when this test was written.)
        for tab in ["Auth", "Env", "General"] {
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
            connection: klefki_core::api::ConnectionId::new(scope, project, name),
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
            oauth: None,
        }
    }

    fn snapshot_of(rows: Vec<ServerRow>) -> klefki_core::api::Event {
        klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows,
            ..Snapshot::default()
        }))
    }

    fn live_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        live_harness_from(Harness::builder())
    }

    /// Frames 1/60 s apart, as on a screen, so a delay is seen as it is and not skipped in quarter seconds.
    fn live_harness_at_60fps() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        live_harness_from(Harness::builder().with_step_dt(1.0 / 60.0))
    }

    fn live_harness_from(
        builder: egui_kittest::HarnessBuilder<ShellApp>,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (core, engine) = klefki_core::api::Core::detached();
        let h = builder
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
            hint: None,
        }
    }

    fn unknown_host_event(row: &ServerRow) -> klefki_core::api::Event {
        klefki_core::api::Event::Status {
            id: row.id(),
            status: Status::Failed,
            detail: Some("ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com".to_owned()),
            hint: Some(klefki_core::api::Hint {
                title: "Can't reach the server's host".to_owned(),
                code: Some("ENOTFOUND".to_owned()),
                help: vec![
                    "You are probably offline or not connected to your VPN: check those first."
                        .to_owned(),
                ],
                points_at: Some((
                    klefki_core::api::Focus::Host,
                    "this name could not be looked up".to_owned(),
                )),
            }),
        }
    }

    #[test]
    fn the_status_tab_explains_a_failure_in_plain_words_and_keeps_claudes() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        h.get_by_label("acme-prod").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
        h.get_by_label_contains("not connected to your VPN");
        h.get_by_label_contains("getaddrinfo ENOTFOUND acme-prod.example.com");
    }

    #[test]
    fn the_explanation_follows_each_new_status() {
        // It is built once per status, not every frame; a new status must still replace it.
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        h.get_by_label("acme-prod").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Can't reach").is_none());
        engine
            .events
            .send(status_event(&row, Status::Failed, Some("boom")))
            .unwrap();
        h.run();
        h.get_by_label_contains("could not connect");
        h.get_by_label_contains("Claude Code said: boom");
    }

    #[test]
    fn a_refresh_keeps_the_selection_on_the_same_server_when_two_look_alike() {
        // Review of the fix: the selection was carried over by the shown name. With "ab" and a server whose name
        // hides a right-to-left override (also shown "ab"), a refresh moved it to the other one, and Delete would
        // have deleted the server not selected.
        let (mut h, engine) = live_harness();
        let ab = live_row("ab", Scope::User, None);
        let mut lookalike = live_row("ab", Scope::User, None);
        lookalike.connection = klefki_core::api::ConnectionId::new(Scope::User, None, "a\u{202e}b");
        let rows = vec![ab.clone(), lookalike.clone()];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        engine
            .events
            .send(status_event(&ab, Status::Connected, None))
            .unwrap();
        h.run();
        h.state_mut().selected.insert(0);
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
        assert_eq!(h.state().selected.iter().copied().collect::<Vec<_>>(), [0]);
        assert_eq!(
            h.state().effective(0).0,
            Status::Connected,
            "its status stays with it"
        );
        assert_ne!(h.state().effective(1).0, Status::Connected);
        // And the change asked for is about the selected one.
        h.key_press(egui::Key::Delete);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: ab.id() }])]
        );
        assert_ne!(ab.id(), lookalike.id());
    }

    #[test]
    fn the_explanation_survives_a_refresh_of_the_configuration() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        let other = live_row("other", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        // A new snapshot, with the row at a different position, carries live results over by identity.
        engine
            .events
            .send(snapshot_of(vec![other, row.clone()]))
            .unwrap();
        h.run();
        h.get_by_label("acme-prod").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
    }

    #[test]
    fn hovering_a_failed_status_explains_it() {
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        h.get_by_label_contains(Status::Failed.label()).hover();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
        h.get_by_label_contains("getaddrinfo ENOTFOUND acme-prod.example.com");
    }

    #[test]
    fn a_failed_status_explains_itself_at_once_even_while_the_mouse_moves() {
        let (mut h, engine) = live_harness_at_60fps();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        let at = h
            .get_by_label_contains(Status::Failed.label())
            .rect()
            .center();
        // A hand on a mouse is rarely still: drift a pixel a frame, for a fifth of a second.
        for dx in 0..12 {
            h.hover_at(at + egui::vec2(dx as f32, 0.0));
            h.step();
        }
        h.get_by_label_contains("Can't reach the server's host");
    }

    #[test]
    fn the_whole_status_cell_explains_not_only_its_text() {
        let (mut h, engine) = live_harness_at_60fps();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        let text = h.get_by_label_contains(Status::Failed.label()).rect();
        h.hover_at(egui::pos2(text.right() + 60.0, text.center().y));
        for _ in 0..12 {
            h.step();
        }
        h.get_by_label_contains("Can't reach the server's host");
        // And it goes when the pointer leaves the cell.
        h.hover_at(egui::pos2(text.right() + 60.0, text.center().y + 300.0));
        for _ in 0..12 {
            h.step();
        }
        assert!(
            h.query_by_label_contains("Can't reach the server's host")
                .is_none()
        );
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
                    code: None,
                    help: vec![(*a).to_owned()],
                    points_at: None,
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
            summary: Vec::new(),
            action: String::new(),
            destructive: false,
            notes: Vec::new(),
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

    #[test]
    fn escape_does_not_close_the_dialog_while_the_change_is_being_saved() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Apply").click();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        // The engine writes the plan whatever comes after Apply; saying it was dropped would be a lie.
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
        h.get_by_label_contains("Review the change");
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    #[test]
    fn cancel_does_nothing_while_the_change_is_being_saved() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Apply").click();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
        h.get_by_label_contains("Review the change");
    }

    #[test]
    fn a_double_click_on_disable_asks_for_one_plan() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        // The engine has not answered the first click yet.
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        let prepares = drain(&engine)
            .into_iter()
            .filter(|c| matches!(c, Command::Prepare(_)))
            .count();
        assert_eq!(prepares, 1);
    }

    #[test]
    fn the_toggle_works_again_once_a_plan_could_not_be_made() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        engine
            .events
            .send(Event::PlanFailed("alpha is gone.".to_owned()))
            .unwrap();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        let prepares = drain(&engine)
            .into_iter()
            .filter(|c| matches!(c, Command::Prepare(_)))
            .count();
        assert_eq!(prepares, 2);
    }

    #[test]
    fn a_plan_arriving_while_another_is_open_is_discarded_not_shown() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Apply").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(9, &["+ zeta"], &[], false)))
            .unwrap();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(7), Command::Discard(9)]);
        assert!(h.query_by_label("+ zeta").is_none());
        // Still saving plan 7: its Apply stays off and its answer closes the dialog.
        h.get_by_label("+ alpha");
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
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
    fn a_double_click_on_review_and_approve_asks_for_one_plan() {
        let (mut h, engine) = live_harness();
        let row = project_row("docs", Some(klefki_core::api::Approval::Pending));
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
        open_general(&mut h, "docs");
        h.get_by_label_contains("Review and approve").click();
        h.run();
        h.get_by_label_contains("Review and approve").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Approve { id: row.id() }])]
        );
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

    // ---- delete and edit

    #[test]
    fn delete_asks_the_engine_for_a_plan_for_each_selected_server() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        h.get_by_label(icon::TRASH).click();
        h.run();
        assert!(drain(&engine).is_empty(), "nothing selected, nothing asked");
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TRASH).click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: a.id() }])]
        );
    }

    #[test]
    fn the_delete_key_deletes_the_selection_but_not_while_typing() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.key_press(egui::Key::Delete);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: a.id() }])]
        );
        // With the search field focused, Del edits the text.
        h.get_by_role(egui::accesskit::Role::TextInput).click();
        h.run();
        h.key_press(egui::Key::Delete);
        h.run();
        assert!(drain(&engine).is_empty());
    }

    fn press_edit(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        h.get_by_label(icon::PENCIL_SIMPLE).click();
        h.run();
    }

    #[test]
    fn edit_opens_the_form_and_asks_for_the_definition() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Status")
            .click();
        h.run();
        h.get_by_label(icon::PENCIL_SIMPLE).click();
        h.run();
        assert_eq!(h.state().tab, DetailTab::General);
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
        h.get_by_label_contains("Loading");
        // Asked once, not every frame.
        h.run();
        assert!(drain(&engine).is_empty());
        engine
            .events
            .send(entry_event(&a, FORM_ORIGINAL, true))
            .unwrap();
        h.run();
        // Labelled fields, not the JSON: that is a fallback, reached from the form.
        assert!(
            h.query_all_by_value("node").next().is_some(),
            "the command field"
        );
        assert!(
            h.query_all_by_value("a.js").next().is_some(),
            "the argument field"
        );
        assert!(h.query_by_value(FORM_ORIGINAL).is_none());
        assert!(h.query_by_label("JSON").is_none(), "no JSON tab");
        h.get_by_label("Edit as JSON");
    }

    #[test]
    fn every_way_to_edit_opens_the_form() {
        type Way = fn(&mut Harness<'static, ShellApp>);
        let ways: [(&str, Way); 2] = [
            ("Ctrl+E", |h| {
                h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
                h.run();
            }),
            ("the palette", |h| {
                ctrl_k(h);
                type_into_palette(h, "edit alpha");
                h.key_press(egui::Key::Enter);
                h.run();
            }),
        ];
        for (how, way) in ways {
            let (mut h, engine) = live_harness();
            let (a, _) = two_rows();
            engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
            h.run();
            h.get_by_label("alpha").click();
            h.run();
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Status")
                .click();
            h.run();
            way(&mut h);
            assert_eq!(h.state().tab, DetailTab::General, "{how}");
            assert_eq!(drain(&engine), vec![Command::Inspect(a.id())], "{how}");
        }
    }

    #[test]
    fn json_is_reached_from_the_form_and_leads_back_to_it() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label("Edit as JSON").click();
        h.run();
        h.get_by_value(&json_text(&h));
        h.get_by_label_contains("keep what they are now");
        assert!(
            h.query_by_value("a.js").is_none(),
            "the form is not shown next to the JSON"
        );
        set_json_text(&mut h, |t| t.replace("\"node\"", "\"deno\""));
        h.run();
        h.get_by_label("Back to the form").click();
        h.run();
        assert!(h.query_all_by_value("deno").next().is_some());
        assert!(h.query_by_value(&json_text(&h)).is_none());
        // JSON the form cannot read keeps you in the JSON, saying why.
        h.get_by_label("Edit as JSON").click();
        h.run();
        set_json_text(&mut h, |_| "{ \"command\": ".into());
        h.run();
        h.get_by_label_contains("not valid");
        assert!(
            h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Revert").click();
        h.run();
        assert!(
            !h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        // The Env tab follows: the definition is being edited as JSON.
        h.get_by_label("Env").click();
        h.run();
        h.get_by_value(&json_text(&h));
        assert!(h.query_by_label("hidden").is_none());
    }

    #[test]
    fn a_definition_the_form_cannot_show_opens_as_json() {
        let odd = "{\n  \"command\": \"x\",\n  \"args\": \"a b\"\n}";
        let (mut h, engine, a) = form_harness(odd, true);
        h.get_by_label_contains("The form cannot show this definition");
        h.get_by_value(odd);
        assert!(
            h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        let fixed = "{\n  \"command\": \"x\",\n  \"args\": [\"a\", \"b\"]\n}";
        set_json_text(&mut h, |_| fixed.into());
        h.run();
        // Still the JSON being typed in, not the form popping up under the cursor.
        h.get_by_value(fixed);
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: fixed.into()
            }])]
        );
    }

    fn entry_event(row: &ServerRow, json: &str, editable: bool) -> Event {
        Event::Entry {
            id: row.id(),
            json: Ok(json.to_owned()),
            editable,
        }
    }

    #[test]
    fn an_edited_definition_is_sent_for_review_and_revert_undoes_it() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "alpha");
        drain(&engine);
        let original =
            "{\n  \"command\": \"node\",\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}";
        engine.events.send(entry_event(&a, original, true)).unwrap();
        h.run();
        h.get_by_label("Edit as JSON").click();
        h.run();
        h.get_by_label_contains("keep what they are now");
        // Unchanged: nothing to review.
        h.get_by_label("Review and save…").click();
        h.run();
        assert!(drain(&engine).is_empty());
        let edited = original.replace("node", "node22");
        if let Some(Editor {
            state: EditorState::Ready { text, .. },
            ..
        }) = &mut h.state_mut().editor
        {
            *text = edited.clone();
        }
        h.run();
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: edited
            }])]
        );
        h.get_by_label("Revert").click();
        h.run();
        match &h.state().editor {
            Some(Editor {
                state: EditorState::Ready { text, .. },
                ..
            }) => assert_eq!(text, original),
            _ => panic!("editor gone"),
        }
    }

    // ---- the edit form

    const FORM_ORIGINAL: &str = "{\n  \"cwd\": \"/w\",\n  \"command\": \"node\",\n  \"args\": [\n    \"a.js\"\n  ],\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}";

    /// alpha selected, its General tab's Edit pressed, and its definition arrived.
    fn form_harness(
        json: &str,
        editable: bool,
    ) -> (
        Harness<'static, ShellApp>,
        klefki_core::api::Detached,
        ServerRow,
    ) {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label("Edit definition").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
        engine.events.send(entry_event(&a, json, editable)).unwrap();
        h.run();
        (h, engine, a)
    }

    fn form<'a>(h: &'a mut Harness<'static, ShellApp>) -> &'a mut Definition {
        match &mut h.state_mut().editor {
            Some(Editor {
                state: EditorState::Ready { form, .. },
                ..
            }) => form
                .as_mut()
                .as_mut()
                .expect("the form reads the definition"),
            _ => panic!("no form"),
        }
    }

    fn set_json_text(h: &mut Harness<'static, ShellApp>, edit: impl FnOnce(&str) -> String) {
        match &mut h.state_mut().editor {
            Some(Editor {
                state: EditorState::Ready { text, .. },
                ..
            }) => *text = edit(text),
            _ => panic!("no editor"),
        }
    }

    fn json_text(h: &Harness<'static, ShellApp>) -> String {
        match &h.state().editor {
            Some(Editor {
                state: EditorState::Ready { text, .. },
                ..
            }) => text.clone(),
            _ => panic!("no editor"),
        }
    }

    #[test]
    fn the_auth_tab_shows_the_oauth_client_and_scopes() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut signed = live_row("acme-prod", Scope::User, None);
        signed.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: Some("klefki-client-1".into()),
            scopes: vec!["read".into(), "write".into()],
        });
        let mut dynamic = live_row("dyn", Scope::User, None);
        dynamic.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: None,
            scopes: Vec::new(),
        });
        let plain = live_row("plain", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![signed, dynamic, plain]))
            .unwrap();
        h.run();
        let open = |h: &mut Harness<'static, ShellApp>, name: &str| {
            h.get_by_label(name).click();
            h.run();
            h.get_by_role_and_label(Role::Button, "Auth").click();
            h.run();
        };
        open(&mut h, "acme-prod");
        h.get_by_label("klefki-client-1");
        h.get_by_label("read, write");
        open(&mut h, "dyn");
        h.get_by_label_contains("registers itself");
        open(&mut h, "plain");
        h.get_by_label_contains("does not sign in with OAuth");
    }

    // ---- the client secret, in the system's secret store

    fn oauth_harness() -> (
        Harness<'static, ShellApp>,
        klefki_core::api::Detached,
        ServerRow,
    ) {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut sso = live_row("sso", Scope::User, None);
        sso.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: Some("klefki-1".into()),
            scopes: Vec::new(),
        });
        engine
            .events
            .send(snapshot_of(vec![
                sso.clone(),
                live_row("plain", Scope::User, None),
            ]))
            .unwrap();
        h.run();
        h.get_by_label("sso").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Auth").click();
        h.run();
        (h, engine, sso)
    }

    fn secret_event(row: &ServerRow, stored: Result<bool, String>) -> Event {
        Event::SecretStatus {
            id: row.id(),
            stored,
        }
    }

    #[test]
    fn the_auth_tab_asks_once_whether_a_client_secret_is_stored() {
        let (mut h, engine, sso) = oauth_harness();
        assert_eq!(drain(&engine), vec![Command::SecretStatus(sso.id())]);
        h.get_by_label_contains("Checking");
        h.run();
        assert!(drain(&engine).is_empty(), "asked once, not every frame");
        engine.events.send(secret_event(&sso, Ok(true))).unwrap();
        h.run();
        h.get_by_label_contains("Stored in your system's secret store");
        // A server that does not sign in with OAuth has no client secret to ask about.
        h.get_by_label("plain").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn a_client_secret_is_typed_hidden_and_handed_over_once() {
        let (mut h, engine, sso) = oauth_harness();
        engine.events.send(secret_event(&sso, Ok(false))).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label_contains("Not stored");
        h.get_by_label("Set client secret…").click();
        h.run();
        h.state_mut()
            .secret_input
            .as_mut()
            .unwrap()
            .edit()
            .push_str("s3cret-value");
        h.run();
        assert!(
            h.query_by_value("s3cret-value").is_none(),
            "not shown, not even to assistive technology"
        );
        h.get_by_label("Save").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::StoreSecret {
                id: sso.id(),
                secret: klefki_core::api::Secret::new("s3cret-value".into()),
            }]
        );
        assert!(h.state().secret_input.is_none(), "the app keeps no copy");
        h.get_by_label_contains("Saving");
    }

    #[test]
    fn forgetting_a_client_secret_asks_first() {
        let (mut h, engine, sso) = oauth_harness();
        engine.events.send(secret_event(&sso, Ok(true))).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label("Forget…").click();
        h.run();
        assert!(drain(&engine).is_empty(), "not yet");
        h.get_by_label_contains("cannot be undone");
        h.get_by_label("Forget it").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::ForgetSecret(sso.id())]);
    }

    #[test]
    fn a_secret_store_problem_is_shown_and_can_be_retried() {
        let (mut h, engine, sso) = oauth_harness();
        drain(&engine);
        engine
            .events
            .send(secret_event(&sso, Err("The system's secret store is not available (no Secret Service). Klefki keeps no plaintext copy of secrets anywhere else.".into())))
            .unwrap();
        h.run();
        h.get_by_label_contains("no plaintext copy");
        h.get_by_label("Try again").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::SecretStatus(sso.id())]);
    }

    #[test]
    fn the_form_edits_the_same_definition_as_the_json_tab() {
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        assert_eq!(form(&mut h).command, "node");
        // Nothing edited yet: nothing to review.
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
        form(&mut h).command = "node22".into();
        form(&mut h).args.push("--x".into());
        h.run();
        let text = json_text(&h);
        assert_eq!(
            text,
            "{\n  \"cwd\": \"/w\",\n  \"command\": \"node22\",\n  \"args\": [\n    \"a.js\",\n    \"--x\"\n  ],\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}",
            "the key the form does not show and the hidden value stay as they were"
        );
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: text
            }])]
        );
        // The JSON shows the same draft.
        h.get_by_label("Edit as JSON").click();
        h.run();
        h.get_by_value(&json_text(&h));
    }

    #[test]
    fn editing_the_json_updates_the_form() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        if let Some(Editor {
            state: EditorState::Ready { text, .. },
            ..
        }) = &mut h.state_mut().editor
        {
            *text = text.replace("\"node\"", "\"deno\"");
        }
        h.run();
        assert_eq!(form(&mut h).command, "deno");
        // JSON the form cannot read: the form says so instead of guessing.
        if let Some(Editor {
            state: EditorState::Ready { text, .. },
            ..
        }) = &mut h.state_mut().editor
        {
            *text = "{ \"command\": ".into();
        }
        h.run();
        h.get_by_label_contains("not valid");
        // Revert brings the form back.
        h.get_by_label("Revert").click();
        h.run();
        assert_eq!(form(&mut h).command, "node");
    }

    #[test]
    fn values_stay_hidden_until_changed_and_new_ones_can_be_added() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label("Env").click();
        h.run();
        h.get_by_label("hidden");
        h.get_by_label("Change TOKEN").click();
        h.run();
        assert_eq!(form(&mut h).env, [("TOKEN".to_owned(), String::new())]);
        h.get_by_label("Add variable").click();
        h.run();
        assert_eq!(form(&mut h).env.len(), 2);
        form(&mut h).env[1] = ("MODE".into(), "dev".into());
        form(&mut h).env[0].1 = "n3w".into();
        h.run();
        assert!(
            json_text(&h).contains("\"MODE\": \"dev\""),
            "{}",
            json_text(&h)
        );
        // Typed values are not shown in the clear, to the eye or to assistive technology.
        assert!(h.query_by_value("n3w").is_none());
        assert!(h.query_by_value("dev").is_none());
        assert!(
            h.query_all_by_value("MODE").next().is_some(),
            "names are shown"
        );
        h.get_by_label("Remove MODE").click();
        h.run();
        assert!(!json_text(&h).contains("MODE"));
    }

    #[test]
    fn a_typed_secret_is_met_with_a_reference_to_use_instead() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label("Env").click();
        h.run();
        // The value that is kept is never offered a hint: the app does not have it.
        assert!(h.query_by_label_contains("Looks like a secret").is_none());
        h.get_by_label("Add variable").click();
        h.run();
        form(&mut h).env[1] = ("MODE".into(), "dev".into());
        h.run();
        assert!(h.query_by_label_contains("Looks like a secret").is_none());
        form(&mut h).env[1] = (
            "GITHUB_TOKEN".into(),
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
        );
        h.run();
        h.get_by_label_contains("Looks like a secret");
        assert!(
            h.query_by_value("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
                .is_none()
        );
        h.get_by_label("Use ${GITHUB_TOKEN} instead").click();
        h.run();
        assert_eq!(form(&mut h).env[1].1, "${GITHUB_TOKEN}");
        assert!(
            json_text(&h).contains("\"GITHUB_TOKEN\": \"${GITHUB_TOKEN}\""),
            "{}",
            json_text(&h)
        );
        assert!(h.query_by_label_contains("Looks like a secret").is_none());
    }

    #[test]
    fn the_form_does_not_widen_the_detail_panel() {
        // Seen in the real window: rows that asked for the panel's whole width plus a button made the resizable
        // panel grow a little every frame, until it reached its largest size.
        let long = "{\n  \"command\": \"npx\",\n  \"args\": [\n    \"-y\",\n    \"@modelcontextprotocol/server-github-with-a-long-name\"\n  ],\n  \"env\": {\n    \"GITHUB_PERSONAL_ACCESS_TOKEN\": \"<hidden>\"\n  }\n}";
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        let before = detail_left_edge(&h, "General");
        h.get_by_label("Edit definition").click();
        h.run();
        engine.events.send(entry_event(&a, long, true)).unwrap();
        h.run();
        assert_eq!(detail_left_edge(&h, "General"), before, "General");
        h.get_by_label("Env").click();
        h.run();
        assert_eq!(detail_left_edge(&h, "General"), before, "Env");
    }

    #[test]
    fn a_problem_in_the_form_is_shown_and_blocks_review() {
        let (mut h, engine, _a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "  ".into();
        h.run();
        h.get_by_label_contains("needs a command");
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Review and save…").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn the_form_asks_for_one_plan_at_a_time() {
        let (mut h, engine, _a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        h.get_by_label("Review and save…").click();
        h.run();
        // The first is on its way: the button waits for its answer.
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(drain(&engine).len(), 1);
    }

    #[test]
    fn project_and_oauth_servers_are_shown_but_not_edited_in_the_form() {
        let (h, _engine, _a) = form_harness("{\n  \"command\": \"x\"\n}", false);
        h.get_by_label_contains("does not edit files in a repository");
        assert!(h.query_by_label("Review and save…").is_none());
        let (h, _engine, _a) = form_harness(
            "{\n  \"type\": \"http\",\n  \"url\": \"https://h.example/mcp\",\n  \"oauth\": {\n    \"clientId\": \"c\"\n  }\n}",
            true,
        );
        h.get_by_label_contains("OAuth");
        assert!(h.query_by_label("Review and save…").is_none());
    }

    #[test]
    fn a_project_server_is_shown_read_only() {
        let (mut h, engine) = live_harness();
        let p = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
        engine.events.send(snapshot_of(vec![p.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "docs");
        engine
            .events
            .send(entry_event(&p, "{\n  \"command\": \"x\"\n}", false))
            .unwrap();
        h.run();
        h.get_by_label_contains("does not edit files in a repository");
        assert!(h.query_by_label("Review and save…").is_none());
        h.get_by_value("{\n  \"command\": \"x\"\n}");
    }

    #[test]
    fn a_definition_that_cannot_be_read_says_why() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "alpha");
        engine
            .events
            .send(Event::Entry {
                id: a.id(),
                json: Err("the server is no longer in the configuration".into()),
                editable: true,
            })
            .unwrap();
        h.run();
        h.get_by_label_contains("no longer in the configuration");
    }

    #[test]
    fn after_a_save_the_definition_is_read_again() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "alpha");
        engine.events.send(entry_event(&a, "{}", true)).unwrap();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::Applied {
                plan: 1,
                changed: true,
                backup: None,
            })
            .unwrap();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
    }

    #[test]
    fn every_toolbar_button_stays_reachable_next_to_the_detail_panel() {
        // Found while testing Delete: with a row selected the detail panel took the toolbar's right half, and the
        // buttons past its edge were cut off and could not be clicked.
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        let detail_left = h.get_by_label("General").rect().min.x;
        // (Add and Import were in this list; they are no longer shown until they work.)
        for glyph in [
            icon::PENCIL_SIMPLE,
            icon::COPY,
            icon::TOGGLE_RIGHT,
            icon::TOGGLE_LEFT,
            icon::TRASH,
        ] {
            let r = h.get_by_label(glyph).rect();
            assert!(
                r.max.x <= detail_left,
                "{glyph:?} at {r:?}, detail panel from x={detail_left}"
            );
        }
    }

    // ---- duplicate

    /// alpha (user), beta (local in WebApp), gamma (project in WebApp); Claude knows two projects.
    fn duplicate_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        let mut c = live_row("gamma", Scope::Project, Some("C:/Repos/WebApp"));
        c.approval = Some(Approval::Approved);
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, b, c],
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    fn duplicate_requested(engine: &klefki_core::api::Detached) -> Vec<Change> {
        drain(engine)
            .into_iter()
            .filter_map(|c| match c {
                Command::Prepare(changes) => Some(changes),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn duplicate_suggests_a_name_and_the_same_place() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("beta").click();
        h.run();
        h.get_by_label(icon::COPY).click();
        h.run();
        h.get_by_label_contains("Duplicate beta");
        assert_eq!(h.state().duplicate.as_ref().unwrap().name, "beta_copy");
        h.get_by_label("Review").click();
        h.run();
        assert!(h.state().duplicate.is_none());
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: h.state().snapshot.servers[1].id(),
                name: "beta_copy".into(),
                targets: vec![Target::Local("C:/Repos/WebApp".into())],
            }]
        );
    }

    #[test]
    fn duplicate_goes_wherever_it_is_ticked_under_the_name_typed() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        // User is ticked for a user server; add both projects, untick the user.
        h.get_by_label_contains("For the user").click();
        h.get_by_label("WebApp").click();
        h.get_by_label("notes").click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().name = "alpha-2".into();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: h.state().snapshot.servers[0].id(),
                name: "alpha-2".into(),
                targets: vec![
                    Target::Local("C:/Repos/WebApp".into()),
                    Target::Local("/home/me/notes".into()),
                ],
            }]
        );
    }

    #[test]
    fn duplicate_needs_a_usable_name_and_a_place() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::COPY).click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().name = "has space".into();
        h.run();
        h.get_by_label_contains("not allowed");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.state_mut().duplicate.as_mut().unwrap().name = "fine".into();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
        h.get_by_label_contains("For the user").click();
        h.run();
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.key_press(egui::Key::Enter);
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().duplicate.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn duplicate_waits_while_another_plan_is_on_its_way() {
        // One plan at a time: a Disable not yet answered keeps Duplicate from asking for a second.
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        assert!(h.get_by_label(icon::COPY).accesskit_node().is_disabled());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_none());
        assert_eq!(drain(&engine).len(), 1, "only the Disable");
    }

    #[test]
    fn a_project_server_cannot_be_duplicated_yet() {
        let (mut h, _engine) = duplicate_harness();
        h.get_by_label("gamma").click();
        h.run();
        assert!(h.get_by_label(icon::COPY).accesskit_node().is_disabled());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_none());
    }

    // ---- the old switch-off list

    fn legacy(
        project: &str,
        names: &[&str],
        turns_off: &[&str],
        malformed: bool,
    ) -> LegacyDisabled {
        LegacyDisabled {
            project: project.to_owned(),
            shown: project.to_owned(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            turns_off: turns_off.iter().map(|n| (*n).to_owned()).collect(),
            malformed,
        }
    }

    fn legacy_harness(
        lists: Vec<LegacyDisabled>,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![live_row("gw", Scope::User, None)],
                projects: lists.iter().map(|l| l.project.clone()).collect(),
                legacy_disabled: lists,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    #[test]
    fn no_old_list_shows_no_notice() {
        let (h, _engine) = legacy_harness(vec![]);
        assert!(h.query_by_label_contains("old switch-off list").is_none());
        assert!(h.query_by_label("Migrate…").is_none());
    }

    #[test]
    fn the_old_list_is_shown_with_what_is_on_because_of_it() {
        let (h, _engine) = legacy_harness(vec![
            legacy("C:/Repos/WebApp", &["gw", "old"], &["gw"], false),
            legacy("/home/x/api", &["a"], &[], false),
        ]);
        h.get_by_label_contains("Old switch-off lists in 2 projects");
        h.get_by_label_contains("Claude Code no longer reads");
        h.get_by_label_contains("WebApp: gw, old (gw is on)");
        h.get_by_label_contains("api: a (already off)");
    }

    #[test]
    fn migrating_asks_for_one_plan_covering_every_list_that_can_move() {
        let (mut h, engine) = legacy_harness(vec![
            legacy("C:/Repos/WebApp", &["gw", "x"], &["gw", "x"], false),
            legacy("/bad", &["a"], &["a"], true),
            legacy("/home/x/api", &["a"], &[], false),
        ]);
        h.get_by_label_contains("bad: holds something other than server names");
        h.get_by_label("Migrate…").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert_eq!(
            changes,
            vec![
                Change::MigrateLegacyDisabled {
                    project: "C:/Repos/WebApp".into()
                },
                Change::MigrateLegacyDisabled {
                    project: "/home/x/api".into()
                },
            ]
        );
    }

    #[test]
    fn only_malformed_lists_cannot_be_migrated() {
        let (h, _engine) = legacy_harness(vec![legacy("/bad", &["a"], &["a"], true)]);
        assert!(h.get_by_label("Migrate…").accesskit_node().is_disabled());
    }

    #[test]
    fn the_palette_offers_the_migration() {
        let (mut h, engine) = legacy_harness(vec![legacy("/p", &["gw"], &["gw"], false)]);
        let titles: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
        assert!(
            titles.iter().any(|t| t == "Migrate old switch-off lists"),
            "{titles:?}"
        );
        h.state_mut()
            .run(&egui::Context::default(), command::Action::MigrateLegacy);
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::MigrateLegacyDisabled {
                project: "/p".into()
            }]
        );
        h.run();
    }

    // ---- fix casing

    /// Gateway (user) and gateway (local in WebApp) differ only in case; `other` is fine.
    fn casing_harness(
        extra: Vec<ServerRow>,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let mut user = live_row("Gateway", Scope::User, None);
        let mut local = live_row("gateway", Scope::Local, Some("C:/Repos/WebApp"));
        user.casing_collision = true;
        local.casing_collision = true;
        let mut rows = vec![user, local, live_row("other", Scope::User, None)];
        rows.extend(extra);
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: rows,
                projects: vec!["C:/Repos/WebApp".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    fn renames(engine: &klefki_core::api::Detached) -> Vec<(ServerId, String)> {
        duplicate_requested(engine)
            .into_iter()
            .map(|c| match c {
                Change::Rename { id, name } => (id, name),
                other => panic!("not a rename: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn only_a_server_with_a_casing_collision_offers_the_fix() {
        let (mut h, _engine) = casing_harness(vec![]);
        h.get_by_label("other").click();
        h.run();
        assert!(h.query_by_label("Fix casing…").is_none());
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…");
    }

    #[test]
    fn the_fix_renames_the_others_to_the_spelling_chosen() {
        let (mut h, engine) = casing_harness(vec![]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        // Both spellings are offered; the selected server's own is the starting point.
        h.get_by_label("Keep gateway");
        h.get_by_label("Keep Gateway");
        assert_eq!(h.state().casing.as_ref().unwrap().keep, "gateway");
        h.get_by_label("Review").click();
        h.run();
        assert!(h.state().casing.is_none());
        let user = h.state().snapshot.servers[0].id();
        assert_eq!(renames(&engine), vec![(user, "gateway".to_owned())]);
    }

    #[test]
    fn choosing_the_other_spelling_renames_the_other_server() {
        let (mut h, engine) = casing_harness(vec![]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        h.get_by_label("Keep Gateway").click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let local = h.state().snapshot.servers[1].id();
        assert_eq!(renames(&engine), vec![(local, "Gateway".to_owned())]);
    }

    #[test]
    fn a_project_server_cannot_be_renamed_so_its_spelling_is_the_one_kept() {
        let mut project = live_row("GATEWAY", Scope::Project, Some("C:/Repos/WebApp"));
        project.casing_collision = true;
        project.approval = Some(Approval::Approved);
        let (mut h, engine) = casing_harness(vec![project]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        assert_eq!(h.state().casing.as_ref().unwrap().keep, "GATEWAY");
        assert!(
            h.get_by_label("Keep gateway")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Review").click();
        h.run();
        let ids: Vec<ServerId> = renames(&engine).into_iter().map(|(id, _)| id).collect();
        let rows = &h.state().snapshot.servers;
        assert_eq!(ids, vec![rows[0].id(), rows[1].id()]);
    }

    #[test]
    fn two_project_servers_that_differ_cannot_be_fixed_from_here() {
        let mut one = live_row("GATEWAY", Scope::Project, Some("C:/Repos/WebApp"));
        let mut two = live_row("Gateway", Scope::Project, Some("C:/Repos/Other"));
        one.casing_collision = true;
        two.casing_collision = true;
        let (mut h, engine) = casing_harness(vec![one, two]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        h.get_by_label_contains(".mcp.json");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().casing.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn cancelling_the_fix_asks_for_nothing() {
        let (mut h, engine) = casing_harness(vec![]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.state().casing.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    /// alpha (user) and beta (local in WebApp), and the forty-odd projects of a long-time Claude Code user.
    fn duplicate_harness_with_many_projects()
    -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        let projects = (0..40)
            .map(|i| format!("C:/Users/me/source/Repos/project-{i:02}"))
            .chain(["C:/Repos/WebApp".into(), "/home/me/notes".into()])
            .collect();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, b],
                projects,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    fn open_duplicate_of(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_some());
    }

    #[test]
    fn duplicate_fits_in_the_window_however_many_projects_claude_knows() {
        let (mut h, _engine) = duplicate_harness_with_many_projects();
        open_duplicate_of(&mut h, "alpha");
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 900.0));
        for label in ["Review", "Cancel"] {
            let r = h.get_by_label(label).rect();
            assert!(
                window.contains_rect(r),
                "{label} at {r:?}, window {window:?}"
            );
        }
        let title = h.get_by_label_contains("Duplicate alpha").rect();
        assert!(window.contains_rect(title), "title at {title:?}");
    }

    #[test]
    fn duplicate_finds_a_project_by_part_of_its_path_and_keeps_what_was_ticked() {
        let (mut h, engine) = duplicate_harness_with_many_projects();
        open_duplicate_of(&mut h, "alpha");
        h.state_mut().duplicate.as_mut().unwrap().filter = "HOME/me".into();
        h.run();
        assert!(h.query_by_label("project-00").is_none());
        h.get_by_label("notes").click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().filter = "web".into();
        h.run();
        assert!(h.query_by_label("notes").is_none());
        h.get_by_label("WebApp").click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().filter.clear();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: h.state().snapshot.servers[0].id(),
                name: "alpha_copy".into(),
                targets: vec![
                    Target::User,
                    Target::Local("C:/Repos/WebApp".into()),
                    Target::Local("/home/me/notes".into()),
                ],
            }]
        );
    }

    #[test]
    fn duplicate_says_where_a_project_lives_only_when_its_folder_name_is_not_enough() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a],
                projects: vec![
                    "C:/work/app".into(),
                    "D:/play/App/".into(),
                    "C:/Repos/WebApp".into(),
                ],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        open_duplicate_of(&mut h, "alpha");
        h.get_by_label("app, C:/work");
        h.get_by_label("App, D:/play");
        h.get_by_label("WebApp");
    }

    #[test]
    fn duplicate_lists_the_project_the_server_is_in_first() {
        let (mut h, _engine) = duplicate_harness_with_many_projects();
        open_duplicate_of(&mut h, "beta");
        let here = h.get_by_label("WebApp").rect();
        let other = h.get_by_label("project-00").rect();
        assert!(
            here.top() < other.top(),
            "WebApp {here:?}, project-00 {other:?}"
        );
    }

    #[test]
    fn duplicate_says_in_words_where_the_copy_will_go() {
        let (mut h, _engine) = duplicate_harness();
        open_duplicate_of(&mut h, "alpha");
        h.get_by_label("Adds alpha_copy for the user.");
        h.get_by_label("WebApp").click();
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("Adds alpha_copy for the user and in WebApp and notes.");
        h.get_by_label_contains("For the user").click();
        h.run();
        h.get_by_label("Adds alpha_copy in WebApp and notes.");
        h.get_by_label("WebApp").click();
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("Tick at least one place.");
    }

    #[test]
    fn duplicate_says_where_the_name_is_already_taken_before_review() {
        let (mut h, _engine) = duplicate_harness();
        open_duplicate_of(&mut h, "alpha");
        // beta is a local server in WebApp; the user has none by that name.
        h.state_mut().duplicate.as_mut().unwrap().name = "beta".into();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
        h.get_by_label("WebApp").click();
        h.run();
        h.get_by_label_contains("beta already exists in WebApp");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        // The user's own alpha.
        h.state_mut().duplicate.as_mut().unwrap().name = "alpha".into();
        h.run();
        h.get_by_label_contains("alpha already exists for the user");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        // gamma comes from WebApp's .mcp.json, not from a place a copy is written to: allowed,
        // and the review warns that the local copy would hide it.
        h.state_mut().duplicate.as_mut().unwrap().name = "gamma".into();
        h.get_by_label_contains("For the user").click();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    }

    #[test]
    fn the_palette_offers_duplicate() {
        let (mut h, _engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "dup");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().duplicate.as_ref().unwrap().name, "alpha_copy");
    }

    // ---- keyboard-first: command palette and arrow keys

    fn ctrl_k(h: &mut Harness<'static, ShellApp>) {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
    }

    fn type_into_palette(h: &mut Harness<'static, ShellApp>, text: &str) {
        *h.state_mut().command.query_mut() = text.to_owned();
        h.run();
    }

    #[test]
    fn the_palette_runs_a_command_by_name() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine.events.send(snapshot_of(vec![a, b])).unwrap();
        h.run();
        ctrl_k(&mut h);
        assert!(h.state().command.is_open());
        h.get_by_label_contains("Go to alpha");
        type_into_palette(&mut h, "refr");
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(!h.state().command.is_open());
        assert_eq!(drain(&engine), vec![Command::Refresh { live_all: true }]);
    }

    #[test]
    fn the_palette_goes_to_a_server_and_acts_on_the_selection() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine.events.send(snapshot_of(vec![a, b.clone()])).unwrap();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go beta");
        h.key_press(egui::Key::Enter);
        h.run();
        let selected: Vec<_> = h
            .state()
            .selected
            .iter()
            .map(|&i| h.state().snapshot.servers[i].name.clone())
            .collect();
        assert_eq!(selected, ["beta"]);
        ctrl_k(&mut h);
        h.get_by_label_contains("Turn off beta");
        type_into_palette(&mut h, "off");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: b.id(),
                enabled: false
            }])]
        );
    }

    #[test]
    fn the_open_palette_follows_a_new_snapshot() {
        // The palette keeps its entries while open; a snapshot arriving meanwhile must still show.
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go");
        h.get_by_label_contains("Go to alpha");
        assert!(h.query_by_label_contains("Go to beta").is_none());
        engine.events.send(snapshot_of(vec![a, b])).unwrap();
        h.run();
        h.get_by_label_contains("Go to beta");
        // And one without the server takes it away again, query unchanged.
        engine
            .events
            .send(snapshot_of(vec![live_row("gamma", Scope::User, None)]))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Go to alpha").is_none());
        h.get_by_label_contains("Go to gamma");
        // Reopening starts from the current entries too.
        h.key_press(egui::Key::Escape);
        h.run();
        ctrl_k(&mut h);
        h.get_by_label_contains("Go to gamma");
    }

    #[test]
    fn arrows_pick_the_entry_and_escape_closes_without_running_anything() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine.events.send(snapshot_of(vec![a, b])).unwrap();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go to");
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        // Second "Go to" entry: beta (user servers sort before local ones; alpha is first).
        let name = &h.state().snapshot.servers[*h.state().selected.iter().next().unwrap()].name;
        assert_eq!(name, "beta");
        ctrl_k(&mut h);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().command.is_open());
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn up_and_down_walk_the_list() {
        let (mut h, _engine) = live_harness();
        let rows = vec![
            live_row("one", Scope::User, None),
            live_row("two", Scope::User, None),
            live_row("three", Scope::Local, Some("C:/Repos/WebApp")),
        ];
        let engine = _engine;
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
        let name = |h: &Harness<'static, ShellApp>| {
            h.state()
                .selected
                .iter()
                .next()
                .map(|&i| h.state().snapshot.servers[i].name.clone())
        };
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("one"));
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("three"));
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("three"), "stays at the end");
        h.key_press(egui::Key::ArrowUp);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("two"));
    }

    #[test]
    fn arrows_do_not_move_the_list_while_typing_in_the_filter() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(snapshot_of(vec![live_row("one", Scope::User, None)]))
            .unwrap();
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).click();
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(h.state().selected.is_empty());
    }

    // ---- the review dialog, the result, the empty places

    #[test]
    fn the_review_leads_with_words_and_its_button_says_what_it_does() {
        let (mut h, engine) = live_harness();
        let mut plan = plan_view(4, &["+ alpha"], &[], false);
        plan.summary = vec!["Turn off alpha in 3 projects".into()];
        plan.action = "Turn off".into();
        plan.notes =
            vec!["A running Claude Code session may need a restart before it notices this.".into()];
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label("Turn off alpha in 3 projects");
        h.get_by_label_contains("may need a restart");
        assert!(h.query_by_label("Apply").is_none());
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(4)]);
        // Pressed again while saving: nothing more is sent.
        h.get_by_label("Turn off").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn a_success_fades_and_a_problem_stays() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(Event::Applied {
                plan: 1,
                changed: true,
                backup: None,
            })
            .unwrap();
        h.run();
        h.get_by_label("Saved.");
        for _ in 0..((SUCCESS_SECONDS + 1.0) * 60.0) as usize {
            h.step();
        }
        assert!(
            h.query_by_label("Saved.").is_none(),
            "gone after a few seconds"
        );
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 2,
                error: "Claude Code is writing its configuration right now.".into(),
            })
            .unwrap();
        h.run();
        for _ in 0..((SUCCESS_SECONDS + 1.0) * 60.0) as usize {
            h.step();
        }
        h.get_by_label_contains("writing its configuration");
    }

    #[test]
    fn a_page_that_is_not_built_says_what_it_will_do_and_leads_back() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Secrets").click();
        h.run();
        h.get_by_label_contains("Coming soon");
        h.get_by_label("Back to connections").click();
        h.run();
        assert_eq!(h.state().page, Page::Connections);
    }

    #[test]
    fn with_nothing_selected_the_detail_panel_teaches_the_keys() {
        let (mut h, _engine) = live_harness();
        h.run();
        h.get_by_label("Select a connection");
        h.get_by_label_contains("all commands");
        h.get_by_label_contains("move through the list");
    }

    #[test]
    fn the_toolbar_shows_only_what_works() {
        let (mut h, _engine) = live_harness();
        h.run();
        // Duplicate (icon::COPY) was in this list until it was built; it is shown now.
        h.get_by_label(icon::COPY);
        for gone in [
            icon::PLUS,
            icon::SWAP,
            icon::SIGN_IN,
            icon::EXPORT,
            icon::DOWNLOAD_SIMPLE,
        ] {
            assert!(h.query_by_label(gone).is_none(), "{gone:?}");
        }
        assert!(
            h.query_by_label_contains("selected").is_none(),
            "no count without a selection"
        );
    }

    // ---- settings: appearance

    #[test]
    fn settings_switch_the_greys_and_remember_them() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Settings").click();
        h.run();
        assert_eq!(h.state().page, Page::Settings);
        h.get_by_label("Warm gray").click();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_DARK.panel);
        h.get_by_label_contains("Light").click();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_LIGHT.panel);
        let mut storage = theme::tests::MemoryStorage::default();
        eframe::App::save(h.state_mut(), &mut storage);
        assert_eq!(theme::load_tone(Some(&storage)), Tone::Warm);
        assert_eq!(theme::load_choice(Some(&storage)), ThemeChoice::Light);
        h.get_by_label("Cool (Catppuccin)").click();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
    }

    #[test]
    fn the_palette_changes_the_greys_too() {
        let (mut h, _engine) = live_harness();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "greys warm");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().tone, Tone::Warm);
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_DARK.panel);
    }
}
