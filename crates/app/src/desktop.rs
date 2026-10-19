//! The Claude Desktop page: the servers in Claude Desktop's own configuration, read-only.
//! Asked of the engine each time the page is opened, and let go when it is left, so it costs nothing at start-up
//! and holds no memory while not shown. Klefki never changes Claude Desktop's servers: there is no CLI to do it
//! through, so the page says where to change them instead.
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Desktop, DesktopServer, Scope, Snapshot, TransportKind};
use std::sync::Arc;

/// How long "Copied" shows after the path is copied, in seconds.
const COPIED_SECONDS: f64 = 2.0;

#[derive(Default)]
pub struct DesktopPage {
    /// `None` until the engine answers.
    pub desktop: Option<Desktop>,
    /// What the top bar's filter box holds on this page: it filters these servers, not Claude Code's.
    pub filter: String,
    /// Per server, where Claude Code has one of the same name, for the snapshot it was worked out for.
    in_code: Option<(Arc<Snapshot>, Vec<String>)>,
    /// When the path was last copied.
    copied: Option<f64>,
}

impl DesktopPage {
    pub fn set(&mut self, desktop: Desktop) {
        self.desktop = Some(desktop);
        self.in_code = None;
    }

    /// Draws the page. `snapshot` is Claude Code's servers, to say which Claude Desktop servers it has too.
    pub fn show(&mut self, ui: &mut Ui, snapshot: &Arc<Snapshot>) {
        let now = ui.input(|i| i.time);
        if self.copied.is_some_and(|at| now - at >= COPIED_SECONDS) {
            self.copied = None;
        }
        // The whole page scrolls: in a small window the header would leave little room for a list of its own.
        egui::ScrollArea::vertical()
            .id_salt("desktop")
            .auto_shrink([false, false])
            .show(ui, |ui| self.contents(ui, snapshot, now));
        if let Some(at) = self.copied {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(
                    (COPIED_SECONDS - (now - at)).max(0.0),
                ));
        }
    }

    fn contents(&mut self, ui: &mut Ui, snapshot: &Arc<Snapshot>, now: f64) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Claude Desktop").heading());
            ui.label(
                RichText::new(format!("{} Read-only", icon::LOCK_SIMPLE))
                    .small()
                    .color(pal.text_muted),
            )
            .on_hover_text(
                "Klefki shows these servers but never changes Claude Desktop's configuration.",
            );
        });
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Claude Desktop's own servers, separate from Claude Code's. To change them, use Claude \
                     Desktop: Settings, Developer, Edit Config, then restart it.",
                )
                .color(pal.text_muted),
            )
            .wrap(),
        );
        ui.add_space(12.0);
        let Some(desktop) = &self.desktop else {
            // No spinner: reading takes a moment, and a spinner would keep the window repainting.
            ui.label(RichText::new("Loading…").color(pal.text_muted));
            return;
        };
        let Some(file) = &desktop.file else {
            not_found(ui, desktop);
            return;
        };
        // What the file is, with a button that copies where it is; the path itself shows on hover, so it does not
        // take over the page (as on the Activity log).
        ui.horizontal_wrapped(|ui| {
            let place = if desktop.packaged {
                "From claude_desktop_config.json in the Microsoft Store install's folder"
            } else {
                "From claude_desktop_config.json in Claude Desktop's settings folder"
            };
            ui.label(RichText::new(place).color(pal.text_muted))
                .on_hover_text(file.as_str());
            if ui
                .small_button(format!("{} Copy path", icon::COPY))
                .on_hover_text(file.as_str())
                .clicked()
            {
                ui.ctx().copy_text(file.clone());
                self.copied = Some(now);
            }
        });
        // What was copied, in the open: hover text is out of reach of the keyboard.
        if self.copied.is_some() {
            ui.add(
                egui::Label::new(
                    RichText::new(format!("Copied {file}"))
                        .small()
                        .color(pal.success),
                )
                .wrap(),
            );
        }
        for path in &desktop.unused {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Also found {path}. Claude Desktop from the Microsoft Store reads its own copy instead."
                    ))
                    .small()
                    .color(pal.text_muted),
                )
                .wrap(),
            );
        }
        if let Some(problem) = &desktop.problem {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{} Could not read this file: {problem}",
                        icon::WARNING
                    ))
                    .color(pal.warn),
                )
                .wrap(),
            );
            return;
        }
        if !desktop.repeated.is_empty() {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{} Defined more than once, so only the last definition counts: {}",
                        icon::WARNING,
                        desktop.repeated.join(", ")
                    ))
                    .color(pal.warn),
                )
                .wrap(),
            );
        }
        ui.add_space(8.0);
        if desktop.servers.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(icon::DESKTOP)
                        .size(32.0)
                        .color(pal.text_muted),
                );
                ui.add_space(8.0);
                ui.label("No servers in Claude Desktop");
            });
            return;
        }
        if !matches!(&self.in_code, Some((s, _)) if Arc::ptr_eq(s, snapshot)) {
            let places = desktop
                .servers
                .iter()
                .map(|s| in_claude_code(snapshot, &s.name))
                .collect();
            self.in_code = Some((Arc::clone(snapshot), places));
        }
        let in_code = self.in_code.as_ref().map_or(&[][..], |(_, p)| p.as_slice());
        let query = self.filter.trim().to_lowercase();
        let mut shown = 0;
        for (i, server) in desktop.servers.iter().enumerate() {
            if !query.is_empty() && !matches(server, &query) {
                continue;
            }
            if shown > 0 {
                ui.separator();
            }
            shown += 1;
            row(ui, server, in_code.get(i).map_or("", String::as_str));
        }
        if shown == 0 {
            ui.label(
                RichText::new(format!(
                    "No Claude Desktop servers match \"{}\".",
                    self.filter.trim()
                ))
                .color(pal.text_muted),
            );
        }
    }
}

fn not_found(ui: &mut Ui, desktop: &Desktop) {
    let pal = theme::palette(ui.ctx());
    ui.add_space(16.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon::DESKTOP).size(32.0).color(pal.text_muted));
        ui.add_space(8.0);
        ui.label("Claude Desktop's configuration was not found");
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Claude Desktop writes it once a server is added there (Settings, Developer, Edit Config).",
                )
                .color(pal.text_muted),
            )
            .wrap(),
        );
    });
    if !desktop.looked_in.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new("Klefki looked in").color(pal.text_muted));
        for path in &desktop.looked_in {
            ui.add(egui::Label::new(RichText::new(path.as_str()).monospace().small()).wrap());
        }
    }
}

/// Whether `server` has `query` (lower case) in its name, command or URL, or a setting's name.
fn matches(server: &DesktopServer, query: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(query);
    has(&server.name)
        || has(&server.target)
        || server.env.iter().chain(&server.headers).any(|n| has(n))
}

/// Where Claude Code has a server named `name`, as a short phrase (`user, local in 2 projects`); empty if nowhere.
fn in_claude_code(snapshot: &Snapshot, name: &str) -> String {
    let count = |scope: Scope| {
        snapshot
            .servers
            .iter()
            .filter(|r| r.scope == scope && r.name == name)
            .count()
    };
    let mut places = Vec::new();
    if count(Scope::User) > 0 {
        places.push("user".to_owned());
    }
    for scope in [Scope::Local, Scope::Project] {
        match count(scope) {
            0 => {}
            1 => places.push(format!("{} in 1 project", scope.label().to_lowercase())),
            n => places.push(format!("{} in {n} projects", scope.label().to_lowercase())),
        }
    }
    places.join(", ")
}

fn row(ui: &mut Ui, server: &DesktopServer, in_code: &str) {
    let pal = theme::palette(ui.ctx());
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&server.name).strong());
        let kind = if server.transport == TransportKind::Invalid {
            RichText::new(format!("{} invalid", icon::WARNING)).color(pal.warn)
        } else {
            RichText::new(server.transport.label()).color(pal.text_muted)
        };
        ui.label(kind.small());
        if !in_code.is_empty() {
            ui.label(
                RichText::new(format!("Also in Claude Code: {in_code}"))
                    .small()
                    .color(pal.text_muted),
            )
            .on_hover_text("A Claude Code server has the same name. Its definition may differ.");
        }
    });
    ui.add(egui::Label::new(RichText::new(&server.target).monospace()).wrap());
    for (what, names) in [("Env", &server.env), ("Headers", &server.headers)] {
        if !names.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{what}: {} (values not shown)", names.join(", ")))
                        .small()
                        .color(pal.text_muted),
                )
                .wrap(),
            );
        }
    }
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use crate::shell::{Page, ShellApp};
    use crate::theme::ThemeChoice;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use klefki_core::api::{
        Command, Core, Desktop, DesktopServer, Detached, Event, Scope, Snapshot, TransportKind,
    };
    use std::sync::Arc;

    fn harness(
        snapshot: Arc<Snapshot>,
        size: egui::Vec2,
    ) -> (Harness<'static, ShellApp>, Detached) {
        let (core, engine) = Core::detached();
        let h = Harness::builder().with_size(size).build_eframe(move |cc| {
            crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
            ShellApp::with_core(core, ThemeChoice::Dark)
        });
        engine.events.send(Event::Snapshot(snapshot)).unwrap();
        (h, engine)
    }

    fn big() -> egui::Vec2 {
        egui::vec2(1100.0, 900.0)
    }

    fn asked(engine: &Detached) -> usize {
        std::iter::from_fn(|| engine.commands.try_recv().ok())
            .filter(|c| *c == Command::Desktop)
            .count()
    }

    fn open(h: &mut Harness<'static, ShellApp>) {
        h.get_by_label("Claude Desktop").click();
        h.run();
    }

    fn server(name: &str, target: &str) -> DesktopServer {
        DesktopServer {
            name: name.to_owned(),
            transport: TransportKind::Stdio,
            target: target.to_owned(),
            env: Vec::new(),
            headers: Vec::new(),
        }
    }

    fn sample() -> Desktop {
        let mut files = server(
            "filesystem",
            "npx -y @modelcontextprotocol/server-filesystem C:/Users/me",
        );
        files.env = vec!["GITHUB_TOKEN".to_owned(), "LOG_LEVEL".to_owned()];
        Desktop {
            file: Some("C:/Users/me/AppData/Roaming/Claude/claude_desktop_config.json".to_owned()),
            servers: vec![
                files,
                DesktopServer {
                    transport: TransportKind::Invalid,
                    ..server("broken", "missing command")
                },
            ],
            ..Desktop::default()
        }
    }

    #[test]
    fn the_file_is_read_only_once_the_page_is_opened_and_let_go_when_left() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), big());
        h.run();
        assert_eq!(asked(&engine), 0, "nothing is read at start-up");
        open(&mut h);
        assert_eq!(h.state().page, Page::Desktop);
        assert_eq!(asked(&engine), 1);
        h.get_by_label("Loading…");
        h.run();
        assert_eq!(asked(&engine), 0, "drawing again asks nothing more");
        engine.events.send(Event::Desktop(sample())).unwrap();
        h.run();
        h.get_by_label("Connections").click();
        h.run();
        assert!(
            h.state().desktop.is_none(),
            "not held once the page is left"
        );
        // An answer that arrives after the page is left is not kept either.
        engine.events.send(Event::Desktop(sample())).unwrap();
        h.run();
        assert!(h.state().desktop.is_none());
        open(&mut h);
        assert_eq!(asked(&engine), 1, "read again when opened again");
        // F5 on this page reads the file again too.
        h.key_press(egui::Key::F5);
        h.run();
        assert_eq!(asked(&engine), 1);
    }

    #[test]
    fn servers_show_with_their_command_and_setting_names_only() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), big());
        h.run();
        open(&mut h);
        engine.events.send(Event::Desktop(sample())).unwrap();
        h.run();
        h.get_by_label("From claude_desktop_config.json in Claude Desktop's settings folder");
        // The path shows on hover and is copied by its button; it does not take over the page.
        assert!(h.query_by_label_contains("AppData").is_none());
        h.get_by_label("filesystem");
        h.get_by_label("npx -y @modelcontextprotocol/server-filesystem C:/Users/me");
        h.get_by_label("Env: GITHUB_TOKEN, LOG_LEVEL (values not shown)");
        h.get_by_label_contains("invalid");
        h.get_by_label("missing command");
        h.get_by_label_contains("Read-only");
        // Says where to change them, and offers nothing that would change them.
        h.get_by_label_contains("Settings, Developer, Edit Config");
        for verb in ["Edit", "Delete", "Turn on", "Turn off", "Save"] {
            assert!(h.query_by_label(verb).is_none(), "{verb}");
        }
    }

    #[test]
    fn the_path_can_be_copied() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), big());
        h.run();
        open(&mut h);
        engine.events.send(Event::Desktop(sample())).unwrap();
        h.run();
        h.get_by_label_contains("Copy path").click();
        h.step();
        let copied: Vec<_> = h
            .output()
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            copied,
            ["C:/Users/me/AppData/Roaming/Claude/claude_desktop_config.json"]
        );
        h.run();
        h.get_by_label("Copied C:/Users/me/AppData/Roaming/Claude/claude_desktop_config.json");
        for _ in 0..30 {
            h.step();
        }
        h.run();
        assert!(
            h.query_by_label_contains("Copied").is_none(),
            "the note goes away"
        );
    }

    #[test]
    fn a_server_claude_code_also_has_says_where() {
        let snapshot = klefki_core::api::fixture_snapshot();
        let user = snapshot
            .servers
            .iter()
            .find(|r| r.scope == Scope::User)
            .unwrap()
            .name
            .clone();
        let (mut h, engine) = harness(snapshot, big());
        h.run();
        open(&mut h);
        engine
            .events
            .send(Event::Desktop(Desktop {
                servers: vec![server(&user, "a"), server("only-in-desktop", "b")],
                ..sample()
            }))
            .unwrap();
        h.run();
        assert_eq!(
            h.get_all_by_label_contains("Also in Claude Code:").count(),
            1,
            "only the one Claude Code has too"
        );
        h.get_by_label_contains("Also in Claude Code: user");
    }

    #[test]
    fn the_top_bar_filter_filters_these_servers() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), big());
        h.run();
        open(&mut h);
        engine.events.send(Event::Desktop(sample())).unwrap();
        h.run();
        h.state_mut().desktop.as_mut().unwrap().filter = "github_token".to_owned();
        h.run();
        h.get_by_label("filesystem");
        assert!(h.query_by_label("broken").is_none());
        h.state_mut().desktop.as_mut().unwrap().filter = "zzz".to_owned();
        h.run();
        h.get_by_label("No Claude Desktop servers match \"zzz\".");
        // The Connections list's own filter is untouched.
        assert_eq!(h.state().search, "");
    }

    #[test]
    fn not_found_says_where_it_looked() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), big());
        h.run();
        open(&mut h);
        engine
            .events
            .send(Event::Desktop(Desktop {
                looked_in: vec!["/home/me/.config/Claude/claude_desktop_config.json".to_owned()],
                ..Desktop::default()
            }))
            .unwrap();
        h.run();
        h.get_by_label("Claude Desktop's configuration was not found");
        h.get_by_label("/home/me/.config/Claude/claude_desktop_config.json");
    }

    #[test]
    fn a_file_that_cannot_be_read_says_why_and_an_unused_copy_is_named() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), big());
        h.run();
        open(&mut h);
        engine
            .events
            .send(Event::Desktop(Desktop {
                problem: Some("invalid JSON (expected value at line 1 column 3)".to_owned()),
                servers: Vec::new(),
                unused: vec!["C:/old/claude_desktop_config.json".to_owned()],
                ..sample()
            }))
            .unwrap();
        h.run();
        h.get_by_label_contains("Could not read this file: invalid JSON");
        h.get_by_label_contains("Also found C:/old/claude_desktop_config.json");
        assert!(h.query_by_label("No servers in Claude Desktop").is_none());
    }

    #[test]
    fn the_page_fits_the_smallest_window() {
        let (mut h, engine) = harness(Arc::new(Snapshot::default()), egui::vec2(800.0, 480.0));
        h.run();
        open(&mut h);
        let mut desktop = sample();
        desktop.servers[0].target = format!("node {}", "C:/a/very/long/path/".repeat(20));
        engine.events.send(Event::Desktop(desktop)).unwrap();
        h.run();
        let right = h.ctx.content_rect().right();
        for node in h.get_all_by_label_contains("") {
            let rect = node.rect();
            assert!(
                rect.right() <= right + 1.0,
                "{:?} runs past the window: {rect:?}",
                node.accesskit_node().label()
            );
        }
    }
}
