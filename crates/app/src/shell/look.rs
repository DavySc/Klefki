//! The look of every page, dialog and theme, drawn with wgpu and compared pixel by pixel with the pictures in
//! `look/`. These keep the look the same through a refactor: a test that fails here means the screen changed. When
//! a change is meant, run `UPDATE_SNAPSHOTS=1 cargo test -p klefki look` and review the new pictures in the pull
//! request. A failure leaves `<name>.new.png` and `<name>.diff.png` beside the picture.
//!
//! Linux only, with Mesa's software renderer in CI: each system's graphics stack draws edges a little differently.

use super::*;
use crate::theme::{Tone, set_tone};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use klefki_core::api::{Detached, FileDiff, Review, fixture_snapshot};

const SIZE: egui::Vec2 = egui::vec2(1100.0, 800.0);

/// The app at `size` in a theme, with the fixture configuration, as the engine would send it.
fn app_in(
    size: egui::Vec2,
    choice: ThemeChoice,
    tone: Tone,
) -> (Harness<'static, ShellApp>, Detached) {
    let (core, engine) = klefki_core::api::Core::detached();
    let theme = match choice {
        ThemeChoice::Light => egui::Theme::Light,
        ThemeChoice::Dark | ThemeChoice::System => egui::Theme::Dark,
    };
    let mut h = Harness::builder()
        .with_theme(theme)
        .with_size(size)
        .wgpu_setup(opengl())
        .build_eframe(move |cc| {
            crate::theme::apply(&cc.egui_ctx, choice);
            set_tone(&cc.egui_ctx, tone);
            let mut app = ShellApp::with_core(core, choice);
            app.tone = tone;
            app
        });
    engine
        .events
        .send(Event::Snapshot(fixture_snapshot()))
        .unwrap();
    engine
        .events
        .send(Event::CliVersion(Some("2.1.0".to_owned())))
        .unwrap();
    h.run();
    (h, engine)
}

/// Mesa's OpenGL software renderer, as in CI: Vulkan's (lavapipe), where installed, draws edges a little
/// differently, and would be preferred.
fn opengl() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(new) = &mut setup {
        new.instance_descriptor.backends = egui_wgpu::wgpu::Backends::GL;
    }
    setup
}

fn app() -> (Harness<'static, ShellApp>, Detached) {
    app_in(SIZE, ThemeChoice::Dark, Tone::Cool)
}

/// Settles the frame and compares it with `look/<name>.png`.
fn shot(h: &mut Harness<'static, ShellApp>, name: &str) {
    h.run();
    h.snapshot_options(
        name,
        &SnapshotOptions::new()
            .output_path("src/shell/look")
            .threshold(0.0),
    );
}

/// The fixture row named `name` in `scope`.
fn row(h: &Harness<'static, ShellApp>, name: &str, scope: Scope) -> usize {
    h.state()
        .snapshot
        .servers
        .iter()
        .position(|s| s.name == name && s.scope == scope)
        .unwrap_or_else(|| panic!("no {name} in {scope:?}"))
}

fn select(h: &mut Harness<'static, ShellApp>, name: &str, scope: Scope) {
    let i = row(h, name, scope);
    h.state_mut().selected = BTreeSet::from([i]);
    h.run();
}

#[test]
fn connections_in_every_theme() {
    // Several windows in one test: their results are gathered, so each picture is checked (or written) once.
    let mut all = SnapshotResults::new();
    for (choice, tone, name) in [
        (ThemeChoice::Dark, Tone::Cool, "connections-dark-cool"),
        (ThemeChoice::Light, Tone::Cool, "connections-light-cool"),
        (ThemeChoice::Dark, Tone::Warm, "connections-dark-warm"),
        (ThemeChoice::Light, Tone::Warm, "connections-light-warm"),
    ] {
        let (mut h, _engine) = app_in(SIZE, choice, tone);
        shot(&mut h, name);
        all.extend_harness(&mut h);
    }
}

#[test]
fn connections_in_a_small_window() {
    let (mut h, _engine) = app_in(egui::vec2(800.0, 480.0), ThemeChoice::Dark, Tone::Cool);
    shot(&mut h, "connections-800x480");
}

#[test]
fn connections_filtered_and_with_several_selected() {
    let (mut h, _engine) = app();
    h.state_mut().search = "gate".to_owned();
    shot(&mut h, "connections-filtered");
    h.state_mut().search.clear();
    let (a, b) = (
        row(&h, "azure-devops", Scope::User),
        row(&h, "issue-tracker", Scope::User),
    );
    h.state_mut().selected = BTreeSet::from([a, b]);
    shot(&mut h, "connections-two-selected");
}

#[test]
fn each_tab_of_a_server() {
    let mut all = SnapshotResults::new();
    for (name, scope, label) in [
        ("issue-tracker", Scope::User, "issue-tracker"),
        ("broken", Scope::Project, "broken"),
        ("company-wiki", Scope::Managed, "company-wiki"),
    ] {
        let (mut h, _engine) = app();
        select(&mut h, name, scope);
        for (tab, title) in DetailTab::ALL {
            // The organisation's server shows only these two, as the app does.
            if scope.is_read_only() && !matches!(tab, DetailTab::General | DetailTab::Status) {
                continue;
            }
            h.state_mut().tab = tab;
            let title = title.to_lowercase().replace([' ', '/', '&'], "-");
            shot(&mut h, &format!("detail-{label}-{title}"));
        }
        all.extend_harness(&mut h);
    }
}

#[test]
fn each_page() {
    let (mut h, engine) = app();
    for (page, name) in [
        (Page::Profiles, "page-profiles"),
        (Page::Desktop, "page-desktop"),
        (Page::ScopeDrift, "page-scope-drift"),
        (Page::Secrets, "page-secrets"),
        (Page::Settings, "page-settings"),
        (Page::ActivityLog, "page-activity"),
    ] {
        h.state_mut().go(page);
        h.run();
        let answer = match page {
            Page::Profiles => Some(Event::Profiles(klefki_core::api::Profiles {
                profiles: vec![klefki_core::api::Profile {
                    name: "Work".to_owned(),
                    members: fixture_snapshot().servers[1..3]
                        .iter()
                        .map(|s| s.connection.clone())
                        .collect(),
                }],
                problem: None,
            })),
            Page::Desktop => Some(Event::Desktop(crate::desktop::tests::sample())),
            Page::Secrets => Some(Event::Secrets(crate::secrets::tests::sample())),
            Page::ActivityLog => Some(Event::Activity(crate::activity::tests::sample())),
            _ => None,
        };
        if let Some(answer) = answer {
            engine.events.send(answer).unwrap();
        }
        shot(&mut h, name);
    }
}

#[test]
fn the_command_palette() {
    let (mut h, _engine) = app();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    shot(&mut h, "command-palette");
}

#[test]
fn each_dialog() {
    let mut all = SnapshotResults::new();
    let (mut h, _engine) = app();
    h.state_mut().open_add();
    shot(&mut h, "dialog-add");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app();
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().open_duplicate();
    shot(&mut h, "dialog-duplicate");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app();
    select(&mut h, "gateway", Scope::Local);
    h.state_mut().open_move();
    shot(&mut h, "dialog-move");
    all.extend_harness(&mut h);

    let (mut h, engine) = app();
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().open_export();
    shot(&mut h, "dialog-export-reading");
    engine
        .events
        .send(Event::Exported(klefki_core::api::Exported {
            text: [
                "{",
                r#"  "mcpServers": {"#,
                r#"    "azure-devops": {"#,
                r#"      "command": "npx","#,
                r#"      "args": ["-y", "@azure-devops/mcp", "contoso"],"#,
                r#"      "env": { "ADO_TOKEN": "${ADO_TOKEN}" }"#,
                "    }",
                "  }",
                "}",
            ]
            .join("\n"),
            servers: 1,
            left_out: 0,
            variables: vec![klefki_core::api::Variable {
                name: "ADO_TOKEN".to_owned(),
                default: None,
                replaces: vec!["azure-devops: env ADO_TOKEN".to_owned()],
            }],
            notes: Vec::new(),
        }))
        .unwrap();
    shot(&mut h, "dialog-export");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app();
    select(&mut h, "Gateway", Scope::Local);
    h.state_mut().open_casing();
    shot(&mut h, "dialog-casing");
    all.extend_harness(&mut h);
}

#[test]
fn the_edit_form() {
    let (mut h, engine) = app();
    select(&mut h, "azure-devops", Scope::User);
    let i = row(&h, "azure-devops", Scope::User);
    let server = h.state().snapshot.servers[i].clone();
    h.state_mut().open_editor(&server);
    engine
        .events
        .send(Event::Entry {
            id: server.id(),
            json: Ok("{\n  \"command\": \"npx\",\n  \"args\": [\n    \"-y\",\n    \"azure-devops-mcp\"\n  ],\n  \
                      \"env\": {\n    \"ADO_TOKEN\": \"<hidden>\"\n  }\n}"
                .to_owned()),
            editable: true,
        })
        .unwrap();
    shot(&mut h, "edit-form");
    h.get_by_label("Edit as JSON").click();
    shot(&mut h, "edit-json");
}

#[test]
fn the_review_of_a_change() {
    let mut all = SnapshotResults::new();
    for (choice, name) in [
        (ThemeChoice::Dark, "review-dark"),
        (ThemeChoice::Light, "review-light"),
    ] {
        let (mut h, engine) = app_in(SIZE, choice, Tone::Cool);
        engine
            .events
            .send(Event::PlanReady(PlanView {
                id: 1,
                summary: vec!["Turn off gateway in C:/Repos/WebApp.".to_owned()],
                action: "Turn off".to_owned(),
                destructive: false,
                files: vec![FileDiff {
                    path: "~/.claude.json".to_owned(),
                    lines: vec![
                        "  projects[\"C:/Repos/WebApp\"].disabledMcpServers".to_owned(),
                        "- []".to_owned(),
                        "+ [\"gateway\"]".to_owned(),
                    ],
                }],
                reviews: vec![Review {
                    title: "gateway would run:".to_owned(),
                    lines: vec!["URL: https://gw.example.com/mcp".to_owned()],
                }],
                warnings: vec![
                    "A running Claude Code session may need a restart before it notices this."
                        .to_owned(),
                ],
                notes: vec!["A backup is kept first.".to_owned()],
                nothing_to_do: false,
                already: Vec::new(),
                others_on: false,
            }))
            .unwrap();
        shot(&mut h, name);
        all.extend_harness(&mut h);
    }
}
