//! The desktop-style shell: left nav, top bar, icon toolbar, grid and detail panel.
//! Pure UI over an immutable `Snapshot`; no IO.
use crate::theme::{self, Palette, ThemeChoice};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use klefki_core::api::{Scope, ServerRow, Snapshot, Status};
use std::collections::BTreeSet;
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

enum GridRow {
    Group(String),
    Server(usize),
}

pub struct ShellApp {
    snapshot: Arc<Snapshot>,
    pub page: Page,
    pub search: String,
    pub selected: BTreeSet<usize>,
    pub tab: DetailTab,
    pub theme: ThemeChoice,
}

impl ShellApp {
    pub fn new(snapshot: Arc<Snapshot>, theme: ThemeChoice) -> Self {
        Self {
            snapshot,
            page: Page::Connections,
            search: String::new(),
            selected: BTreeSet::new(),
            tab: DetailTab::General,
            theme,
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
        idx.sort_by(|&a, &b| {
            let (a, b) = (&self.snapshot.servers[a], &self.snapshot.servers[b]);
            (a.scope, &a.project, a.name.to_lowercase()).cmp(&(
                b.scope,
                &b.project,
                b.name.to_lowercase(),
            ))
        });
        idx
    }

    fn grid_rows(&self) -> Vec<GridRow> {
        let mut rows = Vec::new();
        let mut last: Option<(Scope, Option<String>)> = None;
        for i in self.visible() {
            let s = &self.snapshot.servers[i];
            let key = (s.scope, s.project.clone());
            if last.as_ref() != Some(&key) {
                rows.push(GridRow::Group(group_title(s)));
                last = Some(key);
            }
            rows.push(GridRow::Server(i));
        }
        rows
    }

    fn show(&mut self, ui: &mut Ui) {
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
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(14)))
            .show(ui, |ui| match self.page {
                Page::Connections => {
                    self.warnings(ui);
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
                let _ = ui
                    .button(icon::ARROWS_CLOCKWISE)
                    .on_hover_text("Refresh (F5)");
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
                if let Some(version) = self.snapshot.cli_version.as_deref() {
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
        let warnings = &self.snapshot.warnings;
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
        ui.horizontal(|ui| {
            for (glyph, tip) in ACTIONS {
                // Operations arrive with M1; the toolbar is shown disabled so the layout can be judged.
                let button = egui::Button::new(RichText::new(glyph).size(20.0))
                    .min_size(egui::vec2(44.0, 38.0));
                ui.add_enabled(false, button).on_disabled_hover_text(tip);
            }
            ui.separator();
            ui.label(
                RichText::new(format!("{} selected", self.selected.len())).color(pal.text_muted),
            );
        });
    }

    fn grid(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let rows = self.grid_rows();
        let ctrl = ui.input(|i| i.modifiers.command);
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
                        row.col(|ui| name_cell(ui, s));
                        row.col(|ui| {
                            ui.label(RichText::new(s.transport.label()).color(pal.text_muted));
                        });
                        row.col(|ui| status_cell(ui, s));
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
        let s = self.snapshot.servers[i].clone();
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
                    .show(ui, |ui| {
                        let mut kv = |k: &str, v: String| {
                            ui.label(RichText::new(k).color(pal.text_muted));
                            ui.label(v);
                            ui.end_row();
                        };
                        kv("Scope", s.scope.label().to_owned());
                        kv("Project", s.project.clone().unwrap_or_else(|| "-".into()));
                        kv("Type", s.transport.label().to_owned());
                        kv("Target", s.target.clone());
                        kv("Enabled", if s.enabled { "yes" } else { "no" }.to_owned());
                        kv("Shadowed", if s.shadowed { "yes" } else { "no" }.to_owned());
                    });
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

fn name_cell(ui: &mut Ui, s: &ServerRow) {
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
}

fn status_cell(ui: &mut Ui, s: &ServerRow) {
    let pal = theme::palette(ui.ctx());
    if !s.enabled {
        ui.label(RichText::new("Disabled").color(pal.disabled));
        return;
    }
    let color = match s.status {
        Status::Connected => pal.success,
        Status::NeedsAuth => pal.warn,
        Status::Failed => pal.danger,
        Status::Unknown => pal.text_muted,
    };
    ui.label(RichText::new(format!("\u{25CF} {}", s.status.label())).color(color));
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
}
