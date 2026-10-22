//! The look of every page, dialog, state and theme, drawn with wgpu and compared pixel by pixel with the pictures in
//! `look/`. These keep the look the same through a refactor: a test that fails here means the screen changed. When
//! a change is meant, run `UPDATE_SNAPSHOTS=1 cargo test -p klefki look` and review the new pictures in the pull
//! request. A failure leaves `<name>.new.png` and `<name>.diff.png` beside the picture.
//!
//! Pages are drawn at the size Klefki opens at, dialogs at the smallest window (800×480), where their layout is most
//! likely to break. Linux only, with Mesa's software renderer as in CI: each system's graphics stack draws edges a
//! little differently.

use super::*;
use crate::theme::{Tone, set_tone};
use egui_kittest::{SnapshotOptions, SnapshotResults};
use klefki_core::api::{FileDiff, Preflight, PreflightKind, Review, UpdateCheck, fixture_snapshot};

/// The size Klefki opens at, about.
const PAGE: egui::Vec2 = egui::vec2(1100.0, 800.0);
/// The smallest window.
const SMALL: egui::Vec2 = egui::vec2(800.0, 480.0);

/// Mesa's OpenGL software renderer, as in CI: Vulkan's (lavapipe), where installed, draws edges a little
/// differently, and would be preferred.
fn opengl() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(new) = &mut setup {
        new.instance_descriptor.backends = egui_wgpu::wgpu::Backends::GL;
    }
    setup
}

/// The app at `size` in a theme, showing `snapshot` as the engine would send it.
fn app_showing(
    snapshot: Arc<Snapshot>,
    size: egui::Vec2,
    choice: ThemeChoice,
    tone: Tone,
) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
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
    engine.events.send(Event::Snapshot(snapshot)).unwrap();
    engine
        .events
        .send(Event::CliVersion(Some("2.1.0".to_owned())))
        .unwrap();
    h.run();
    (h, engine)
}

/// The fixture configuration, dark and cool.
fn app_at(size: egui::Vec2) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    app_showing(fixture_snapshot(), size, ThemeChoice::Dark, Tone::Cool)
}

/// Settles the frame and compares it with `look/<name>.png`.
fn shot(h: &mut Harness<'static, ShellApp>, name: &str) {
    h.run();
    h.snapshot_options(
        name,
        &SnapshotOptions::new()
            .output_path("src/shell/tests/look")
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

/// A plan for the review dialog: turning gateway off, or `destructive`ly deleting it.
fn plan(destructive: bool) -> PlanView {
    let (summary, action, lines) = if destructive {
        (
            "Delete gateway from C:/Repos/WebApp.",
            "Delete",
            vec![
                "  projects[\"C:/Repos/WebApp\"].mcpServers".to_owned(),
                "- \"gateway\": { \"type\": \"http\", \"url\": \"https://gw.example.com/mcp\" }"
                    .to_owned(),
            ],
        )
    } else {
        (
            "Turn off gateway in C:/Repos/WebApp.",
            "Turn off",
            vec![
                "  projects[\"C:/Repos/WebApp\"].disabledMcpServers".to_owned(),
                "- []".to_owned(),
                "+ [\"gateway\"]".to_owned(),
            ],
        )
    };
    PlanView {
        id: 1,
        summary: vec![summary.to_owned()],
        action: action.to_owned(),
        destructive,
        files: vec![FileDiff {
            path: "~/.claude.json".to_owned(),
            lines,
        }],
        reviews: vec![Review {
            title: "gateway would run:".to_owned(),
            lines: vec!["URL: https://gw.example.com/mcp".to_owned()],
        }],
        warnings: Vec::new(),
        notes: vec!["A backup is kept first.".to_owned()],
        sessions: Some(
            "Claude Code sessions already open in WebApp keep their servers until you restart them (or switch \
             them there with /mcp). New sessions get the change."
                .to_owned(),
        ),
        restart: Some("Restart Claude Code sessions already open in WebApp to pick it up.".to_owned()),
        nothing_to_do: false,
        already: Vec::new(),
        others_on: false,
    }
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
        let (mut h, _engine) = app_showing(fixture_snapshot(), PAGE, choice, tone);
        shot(&mut h, name);
        all.extend_harness(&mut h);
    }
}

#[test]
fn connections_in_a_small_window() {
    let (mut h, _engine) = app_at(SMALL);
    shot(&mut h, "connections-800x480");
}

#[test]
fn connections_filtered_and_with_several_selected() {
    let (mut h, _engine) = app_at(PAGE);
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
fn connections_with_every_status() {
    let (mut h, engine) = app_at(PAGE);
    let checking = h.state().snapshot.servers[row(&h, "azure-devops", Scope::User)].clone();
    let failed = h.state().snapshot.servers[row(&h, "issue-tracker", Scope::User)].clone();
    engine
        .events
        .send(status_event(&checking, Status::Checking, None))
        .unwrap();
    engine.events.send(unknown_host_event(&failed)).unwrap();
    shot(&mut h, "connections-checking-and-failed");
    select(&mut h, "issue-tracker", Scope::User);
    h.state_mut().tab = DetailTab::Status;
    shot(&mut h, "detail-failed-status");
}

#[test]
fn connections_empty_and_in_trouble() {
    let mut all = SnapshotResults::new();
    let (mut h, _engine) = app_showing(
        Arc::new(Snapshot::default()),
        PAGE,
        ThemeChoice::Dark,
        Tone::Cool,
    );
    shot(&mut h, "connections-no-servers");
    all.extend_harness(&mut h);

    let (mut h, engine) = app_showing(
        Arc::new(Snapshot {
            warnings: vec![
                "~/.claude.json could not be read: expected `,` or `}` at line 12 column 5. Nothing in it is shown \
                 or changed until it is fixed."
                    .to_owned(),
            ],
            ..Snapshot::default()
        }),
        PAGE,
        ThemeChoice::Dark,
        Tone::Cool,
    );
    engine
        .events
        .send(Event::CliProblem(
            "Claude Code was not found on the PATH Klefki uses.".to_owned(),
        ))
        .unwrap();
    shot(&mut h, "connections-unreadable-and-no-cli");
    all.extend_harness(&mut h);
}

#[test]
fn an_update_in_the_top_bar() {
    let (mut h, engine) = app_at(PAGE);
    h.state_mut().checks_for_updates = true;
    h.state_mut().updated_by_hand = Some("Klefki was installed from the .deb package.".into());
    engine
        .events
        .send(Event::Update(UpdateCheck::Available {
            version: klefki_core::api::Version::parse("999.0.0").unwrap(),
            page: "https://github.com/DavySc/Klefki/releases/tag/v999.0.0".into(),
        }))
        .unwrap();
    shot(&mut h, "update-available");
}

#[test]
fn each_tab_of_a_server() {
    let mut all = SnapshotResults::new();
    for (name, scope) in [
        ("issue-tracker", Scope::User),
        ("broken", Scope::Project),
        ("company-wiki", Scope::Managed),
    ] {
        let (mut h, _engine) = app_at(PAGE);
        select(&mut h, name, scope);
        for (tab, title) in DetailTab::ALL {
            // The organisation's server shows only these two, as the app does.
            if scope.is_read_only() && !matches!(tab, DetailTab::General | DetailTab::Status) {
                continue;
            }
            h.state_mut().tab = tab;
            let title = title.to_lowercase().replace([' ', '/', '&'], "-");
            shot(&mut h, &format!("detail-{name}-{title}"));
        }
        all.extend_harness(&mut h);
    }
}

#[test]
fn what_keeps_a_server_from_starting() {
    // Found before anything runs: a badge in the list, and each finding with what to do in the details.
    let mut snapshot = (*fixture_snapshot()).clone();
    let azure = snapshot
        .servers
        .iter_mut()
        .find(|s| s.name == "azure-devops")
        .unwrap();
    // As a check from Klefki finds it: Claude Code can't start what isn't there.
    azure.status = Status::Failed;
    azure.preflight = vec![
        Preflight {
            kind: PreflightKind::NotFound,
            problem: "Can't find npx on the PATH Klefki was started with.".to_owned(),
            fix: "Install it, or write its full path. Claude Code looks on its own PATH, which a terminal can \
                  set differently."
                .to_owned(),
        },
        Preflight {
            kind: PreflightKind::UnsetVariable,
            problem: "${AZURE_DEVOPS_PAT} isn't set where Klefki was started. Unless it is set where Claude \
                      Code starts, Claude Code passes it on as written."
                .to_owned(),
            fix: "Set it before you start Claude Code and Klefki.".to_owned(),
        },
    ];
    let (mut h, _engine) = app_showing(Arc::new(snapshot), PAGE, ThemeChoice::Dark, Tone::Cool);
    select(&mut h, "azure-devops", Scope::User);
    shot(&mut h, "preflight-findings");
}

/// `azure-devops` timed out after `took` s, or connected given more time.
fn timed(took: u64, given: bool) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    use klefki_core::api::{Hint, LONGER_CHECK, LimitSetIn, TimeLimit, Timing};
    use std::time::Duration;
    let (mut h, engine) = app_at(PAGE);
    let i = row(&h, "azure-devops", Scope::User);
    let id = h.state().snapshot.servers[i].id();
    // As the engine reports them; the hint is Claude Code's "timed out", put in words by the engine.
    let (status, detail, hint) = if given {
        (Status::Connected, None, None)
    } else {
        let hint = Hint {
            title: "The server took too long to answer".to_owned(),
            code: None,
            help: vec![
                "The server may be slow to start, or blocked by a firewall or proxy.".to_owned(),
                "Try again, and check the server's own logs.".to_owned(),
            ],
            points_at: None,
        };
        (
            Status::Failed,
            Some("Request timed out".to_owned()),
            Some(hint),
        )
    };
    engine
        .events
        .send(Event::Status {
            id,
            status,
            detail,
            hint,
            timing: Some(Timing {
                took: Duration::from_secs(took),
                limit: TimeLimit {
                    limit: Duration::from_secs(30),
                    set_in: LimitSetIn::Default,
                    unreadable: false,
                    change_in: Some("~/.claude/settings.json".to_owned()),
                },
                given: given.then_some(LONGER_CHECK),
                timed_out: !given,
            }),
        })
        .unwrap();
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().tab = DetailTab::Status;
    (h, engine)
}

#[test]
fn a_timeout_explained() {
    let (mut h, _engine) = timed(28, false);
    shot(&mut h, "timeout-explained");
}

#[test]
fn a_server_that_needs_more_time() {
    let (mut h, _engine) = timed(41, true);
    shot(&mut h, "timeout-needs-more-time");
}

#[test]
fn a_diagnosis() {
    let (mut h, engine) = app_at(PAGE);
    let i = row(&h, "azure-devops", Scope::User);
    let id = h.state().snapshot.servers[i].id();
    engine
        .events
        .send(Event::Diagnosis {
            id,
            diagnosis: diagnosis(
                Status::Failed,
                &[(
                    "The command was not found",
                    "Claude could not start the server because the program is not on the PATH it uses.",
                )],
                &["'npx' is not recognized as an internal or external command,"],
                &["[ERROR] MCP server \"azure-devops\" Connection failed (CONNECTION_CLOSED)"],
                Some("the client wrote no debug output"),
            ),
        })
        .unwrap();
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().tab = DetailTab::Status;
    shot(&mut h, "detail-diagnosis");
}

#[test]
fn each_page() {
    let (mut h, engine) = app_at(PAGE);
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

/// The profiles page with Work in use, and its dialogs, with the profile tests' configuration.
#[test]
fn profiles_in_use_and_their_dialogs() {
    let Event::Snapshot(snapshot) = snapshot_with_work_in_use_in_notes() else {
        unreachable!()
    };
    let (mut h, engine) = app_showing(snapshot, SMALL, ThemeChoice::Dark, Tone::Cool);
    open_profiles(&mut h, &engine, work_profile());
    shot(&mut h, "profiles-in-use");
    h.get_by_label_contains("Delete").click();
    shot(&mut h, "dialog-delete-profile");
    h.get_by_label("Cancel").click();
    h.run();
    h.get_by_label_contains("New profile").click();
    shot(&mut h, "dialog-new-profile");
}

#[test]
fn the_command_palette() {
    let (mut h, _engine) = app_at(SMALL);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    shot(&mut h, "command-palette");
}

#[test]
fn each_dialog() {
    let mut all = SnapshotResults::new();
    let (mut h, _engine) = app_at(SMALL);
    h.state_mut().open_add();
    shot(&mut h, "dialog-add");
    if let Some(form) = &mut h.state_mut().add {
        form.text = "claude mcp add --transport http notion https://mcp.notion.com/mcp\n\
                     claude mcp add fs -e ROOT=/work -- npx -y @modelcontextprotocol/server-filesystem"
            .to_owned();
    }
    shot(&mut h, "dialog-add-pasted");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app_at(SMALL);
    h.state_mut().open_add();
    if let Some(form) = &mut h.state_mut().add {
        form.mode = AddMode::Fill;
        form.name = "notion".to_owned();
        form.fields.transport = klefki_core::api::TransportKind::Http;
        form.fields.url = "https://mcp.notion.com/mcp".to_owned();
        form.fields.oauth = true;
        form.fields.client_id = "klefki-desktop".to_owned();
        form.fields.callback_port = "8080".to_owned();
    }
    shot(&mut h, "dialog-add-http-oauth");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app_at(SMALL);
    h.state_mut().open_import(ImportFile::Read {
        shown: "team-servers.json".to_owned(),
        text:
            r#"{"mcpServers": {"notion": {"type": "http", "url": "https://mcp.notion.com/mcp"}}}"#
                .to_owned(),
    });
    shot(&mut h, "dialog-import");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app_at(SMALL);
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().open_duplicate();
    shot(&mut h, "dialog-duplicate");
    all.extend_harness(&mut h);

    let (mut h, _engine) = app_at(SMALL);
    select(&mut h, "gateway", Scope::Local);
    h.state_mut().open_move();
    shot(&mut h, "dialog-move");
    all.extend_harness(&mut h);

    let (mut h, engine) = app_at(SMALL);
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

    let (mut h, _engine) = app_at(SMALL);
    select(&mut h, "Gateway", Scope::Local);
    h.state_mut().open_casing();
    shot(&mut h, "dialog-casing");
    all.extend_harness(&mut h);
}

#[test]
fn the_edit_form() {
    let (mut h, engine) = app_at(PAGE);
    select(&mut h, "azure-devops", Scope::User);
    let i = row(&h, "azure-devops", Scope::User);
    let server = h.state().snapshot.servers[i].clone();
    h.state_mut().open_editor(&server);
    engine
        .events
        .send(Event::Entry {
            id: server.id(),
            json: Ok(
                "{\n  \"command\": \"npx\",\n  \"args\": [\n    \"-y\",\n    \"azure-devops-mcp\"\n  ],\n  \
                      \"env\": {\n    \"ADO_TOKEN\": \"<hidden>\"\n  }\n}"
                    .to_owned(),
            ),
            editable: true,
        })
        .unwrap();
    shot(&mut h, "edit-form");
    h.get_by_label("Edit as JSON").click();
    shot(&mut h, "edit-json");
}

/// A change to review, a deletion (its button dangerous), one that changes nothing, then how each ends.
#[test]
fn reviews_and_how_they_end() {
    let mut all = SnapshotResults::new();
    for (choice, theme) in [(ThemeChoice::Dark, "dark"), (ThemeChoice::Light, "light")] {
        let (mut h, engine) = app_showing(fixture_snapshot(), SMALL, choice, Tone::Cool);
        engine.events.send(Event::PlanReady(plan(false))).unwrap();
        shot(&mut h, &format!("review-{theme}"));
        engine
            .events
            .send(Event::Applied {
                plan: 1,
                changed: true,
                backup: Some("~/.klefki/backups/claude.json.1-0.bak".to_owned()),
                notes: Vec::new(),
                undo: None,
            })
            .unwrap();
        shot(&mut h, &format!("outcome-saved-{theme}"));
        all.extend_harness(&mut h);

        let (mut h, engine) = app_showing(fixture_snapshot(), SMALL, choice, Tone::Cool);
        engine.events.send(Event::PlanReady(plan(true))).unwrap();
        shot(&mut h, &format!("review-delete-{theme}"));
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 1,
                error: "~/.claude.json changed since the review: nothing was saved. Review again."
                    .to_owned(),
            })
            .unwrap();
        shot(&mut h, &format!("outcome-failed-{theme}"));
        all.extend_harness(&mut h);
    }

    let (mut h, engine) = app_at(SMALL);
    let mut nothing = plan(false);
    nothing.nothing_to_do = true;
    nothing.files.clear();
    nothing.already = vec!["gateway is already off in C:/Repos/WebApp.".to_owned()];
    engine.events.send(Event::PlanReady(nothing)).unwrap();
    shot(&mut h, "review-nothing-to-do");
    all.extend_harness(&mut h);
}

#[test]
fn signing_in() {
    let mut all = SnapshotResults::new();
    for (choice, theme) in [(ThemeChoice::Dark, "dark"), (ThemeChoice::Light, "light")] {
        let (mut h, engine) = app_showing(fixture_snapshot(), SMALL, choice, Tone::Cool);
        let i = row(&h, "issue-tracker", Scope::User);
        let server = h.state().snapshot.servers[i].clone();
        h.state_mut().start_sign_in(&server);
        shot(&mut h, &format!("sign-in-starting-{theme}"));
        engine
            .events
            .send(sign_in_event(&server, waiting(true)))
            .unwrap();
        shot(&mut h, &format!("sign-in-waiting-{theme}"));
        engine
            .events
            .send(sign_in_event(
                &server,
                failure("The browser did not come back in time.", None, true),
            ))
            .unwrap();
        shot(&mut h, &format!("sign-in-failed-{theme}"));
        all.extend_harness(&mut h);
    }
}

/// `azure-devops` failed in a project the last time, and connected in the home folder before.
#[test]
fn last_starts_from_claude_codes_logs() {
    use klefki_core::api::{Hint, LastStart, LastStarts, StartOutcome};
    use std::time::Duration;
    let (mut h, engine) = app_at(PAGE);
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().tab = DetailTab::Status;
    h.run();
    let id = {
        let i = row(&h, "azure-devops", Scope::User);
        h.state().snapshot.servers[i].id()
    };
    let start = |folder: &str, when: &str, outcome: StartOutcome| {
        LastStart {
        project: None,
        folder: folder.to_owned(),
        when: when.to_owned(),
        exact: format!("22 Oct 2026, {when} (UTC+02:00)"),
        outcome,
        server_said: Vec::new(),
        hints: Vec::new(),
        partly: false,
        file: "~/.cache/claude-cli-nodejs/-home-me-code-shop/mcp-logs-azure-devops/2026-10-22T07-32-16-377Z.jsonl"
            .to_owned(),
    }
    };
    let failed = LastStart {
        server_said: vec![
            "npm error 404 Not Found - GET https://registry.npmjs.org/@azure-devops%2fmcp"
                .to_owned(),
            "npm error 404  '@azure-devops/mcp@*' is not in this registry.".to_owned(),
        ],
        hints: vec![Hint {
            title: "Installing the server's package failed".to_owned(),
            code: None,
            help: vec!["Check the package name and that npm can reach its registry.".to_owned()],
            points_at: None,
        }],
        ..start(
            "~/code/shop",
            "Today 09:32",
            StartOutcome::Failed {
                code: Some("CONNECTION_CLOSED".to_owned()),
                said: "Connection closed".to_owned(),
                after: Some(Duration::from_millis(2380)),
            },
        )
    };
    let connected = start(
        "~",
        "Today 08:10",
        StartOutcome::Connected {
            after: Some(Duration::from_millis(1420)),
        },
    );
    engine
        .events
        .send(Event::LastStarts {
            id,
            starts: LastStarts {
                starts: vec![failed, connected],
                ..LastStarts::default()
            },
        })
        .unwrap();
    shot(&mut h, "last-starts");
}

/// The Status tab after "Copy Inspector command": the line that was copied, and what to know before running it.
#[test]
fn the_inspector_command_under_the_actions() {
    use klefki_core::api::InspectorCommand;
    let (mut h, engine) = app_at(PAGE);
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().tab = DetailTab::Status;
    h.run();
    let id = {
        let i = row(&h, "azure-devops", Scope::User);
        h.state().snapshot.servers[i].id()
    };
    h.state_mut().copy_inspector_command(id.clone());
    engine
        .events
        .send(Event::InspectorCommand {
            id,
            line: Ok(InspectorCommand {
                text: "npx @modelcontextprotocol/inspector -- npx -y @azure-devops/mcp contoso".to_owned(),
                to_do: vec![
                    "Enter AZURE_DEVOPS_PAT in the Inspector, under Environment Variables: its environment is not in the line, so no secret reaches your shell.".to_owned(),
                ],
                notes: vec![
                    "Needs Node 22.19 or newer. Running the line downloads the Inspector and opens it in your browser; Klefki does not run it.".to_owned(),
                ],
            }),
        })
        .unwrap();
    h.run();
    shot(&mut h, "inspector-command");
}

/// The Status tab of a server with no definition here, after "Copy debug report": the section and "Copied".
#[test]
fn the_debug_report_section_after_copying() {
    let (mut h, engine) = app_at(PAGE);
    select(&mut h, "company-wiki", Scope::Managed);
    h.state_mut().tab = DetailTab::Status;
    h.run();
    let id = {
        let i = row(&h, "company-wiki", Scope::Managed);
        h.state().snapshot.servers[i].id()
    };
    h.state_mut().copy_debug_report(id.clone());
    engine
        .events
        .send(Event::LastStarts {
            id: id.clone(),
            starts: klefki_core::api::LastStarts::default(),
        })
        .unwrap();
    engine
        .events
        .send(Event::RedactedDefinition {
            id,
            result: Err("your organisation sets it, for everyone on this machine".to_owned()),
            also: Vec::new(),
        })
        .unwrap();
    h.run();
    h.step();
    shot(&mut h, "debug-report-copied");
}

/// `azure-devops`, a user server, offered to be diagnosed from a project's folder, then diagnosed there.
#[test]
fn a_user_server_diagnosed_in_a_project() {
    let (mut h, engine) = app_at(PAGE);
    let mut snapshot = (*h.state().snapshot).clone();
    snapshot.projects = ["C:/Repos/Gateway", "C:/Repos/WebApp", "C:/Repos/shop"]
        .map(str::to_owned)
        .to_vec();
    h.state_mut().snapshot = Arc::new(snapshot);
    select(&mut h, "azure-devops", Scope::User);
    h.state_mut().tab = DetailTab::Status;
    shot(&mut h, "diagnose-in-project");
    let i = row(&h, "azure-devops", Scope::User);
    let id = h.state().snapshot.servers[i].id();
    engine
        .events
        .send(Event::Diagnosis {
            id,
            diagnosis: Diagnosis {
                folder: Some("C:/Repos/Gateway".to_owned()),
                detail: Some("CONNECTION_CLOSED: Connection closed".to_owned()),
                ..diagnosis(
                    Status::Failed,
                    &[(
                        "A file or module is missing",
                        "The server's command names a file that is not in this folder.",
                    )],
                    &["Error: Cannot find module 'C:\\Repos\\Gateway\\server.js'"],
                    &[],
                    None,
                )
            },
        })
        .unwrap();
    shot(&mut h, "diagnosed-in-project");
}

/// The list seen from one project (Gateway): what Claude Code loads there, off there, and a switch made there with
/// its Undo; and the same at the smallest window.
#[test]
fn the_list_seen_from_one_project() {
    let mut snapshot = (*fixture_snapshot()).clone();
    snapshot.projects = ["C:/Repos/Gateway", "C:/Repos/WebApp", "C:/Repos/shop"]
        .map(str::to_owned)
        .to_vec();
    for s in &mut snapshot.servers {
        if s.scope == Scope::User && s.name == "gateway" {
            s.disabled_projects = vec!["C:/Repos/Gateway".to_owned()];
        }
    }
    let snapshot = Arc::new(snapshot);
    let mut all = SnapshotResults::new();
    for (size, name) in [
        (PAGE, "connections-one-project"),
        (SMALL, "connections-one-project-800x480"),
    ] {
        let (mut h, _engine) =
            app_showing(Arc::clone(&snapshot), size, ThemeChoice::Dark, Tone::Cool);
        h.state_mut().look_in(Some("C:/Repos/Gateway".to_owned()));
        select(&mut h, "gateway", Scope::User);
        shot(&mut h, name);
        all.extend_harness(&mut h);
    }
    let (mut h, engine) = app_showing(snapshot, PAGE, ThemeChoice::Dark, Tone::Cool);
    h.state_mut().look_in(Some("C:/Repos/Gateway".to_owned()));
    select(&mut h, "issue-tracker", Scope::User);
    let mut switched = plan(false);
    switched.id = 3;
    switched.restart =
        Some("Restart Claude Code sessions already open in Gateway to pick it up.".to_owned());
    h.state_mut().plan = Some(Arc::new(switched));
    h.state_mut().quiet_plan = Some((3, "Turned off issue-tracker in Gateway.".to_owned()));
    engine
        .events
        .send(Event::Applied {
            plan: 3,
            changed: true,
            backup: Some("C:/Users/me/AppData/Roaming/klefki/backups/claude.json.1".to_owned()),
            notes: Vec::new(),
            undo: Some(klefki_core::api::Undo {
                switches: Vec::new(),
                records: Vec::new(),
            }),
        })
        .unwrap();
    h.run();
    h.step();
    shot(&mut h, "switched-in-one-project");
    all.extend_harness(&mut h);
}

/// Switching profiles (on/off plan S4): a profile's card where another is in use, and the profile menu of the list
/// seen from one project, open, at the default size and the smallest window.
#[test]
fn switching_profiles() {
    let Event::Snapshot(snapshot) = snapshot_with_work_in_use_in_notes() else {
        unreachable!()
    };
    let mut snapshot = (*snapshot).clone();
    let mut profiles = work_profile();
    profiles.profiles.push(klefki_core::api::Profile {
        name: "Notes".to_owned(),
        members: vec![snapshot.servers[1].connection.clone()],
    });
    snapshot.profiles = profiles.profiles.clone();
    let snapshot = Arc::new(snapshot);
    let mut all = SnapshotResults::new();
    let (mut h, engine) = app_showing(Arc::clone(&snapshot), SMALL, ThemeChoice::Dark, Tone::Cool);
    open_profiles(&mut h, &engine, profiles);
    h.state_mut().profiles.as_mut().unwrap().filter = "Notes".to_owned();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Use in")
        .click();
    h.run();
    h.get_by_label_contains("notes").click();
    shot(&mut h, "profile-switch-from");
    all.extend_harness(&mut h);
    for (size, name) in [
        (PAGE, "connections-profile-menu"),
        (SMALL, "connections-profile-menu-800x480"),
    ] {
        let (mut h, _engine) =
            app_showing(Arc::clone(&snapshot), size, ThemeChoice::Dark, Tone::Cool);
        h.state_mut().look_in(Some("/home/notes".to_owned()));
        h.run();
        h.get_by_label("Profile in notes: Work").click();
        shot(&mut h, name);
        all.extend_harness(&mut h);
    }
}

/// A profile's start command (on/off plan S5), with what would still differ from the profile, at the default size and
/// the smallest window.
#[test]
fn a_profiles_start_command() {
    let Event::Snapshot(snapshot) = snapshot_with_beta_off_in_notes(false) else {
        unreachable!()
    };
    let mut all = SnapshotResults::new();
    for (size, name) in [
        (PAGE, "profile-start-command"),
        (SMALL, "profile-start-command-800x480"),
    ] {
        let (mut h, engine) =
            app_showing(Arc::clone(&snapshot), size, ThemeChoice::Dark, Tone::Cool);
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Use in")
            .click();
        h.run();
        h.get_by_label_contains("notes").click();
        h.run();
        h.get_by_label("Copy start command").click();
        h.run();
        let settings = r#"{"allowedMcpServers":[{"serverName":"alpha"}]}"#;
        engine
            .events
            .send(Event::StartCommand {
                profile: "Work".to_owned(),
                project: "/home/notes".to_owned(),
                plan: Ok(klefki_core::api::StartPlan {
                    profile: "Work".to_owned(),
                    project: "/home/notes".to_owned(),
                    args: vec!["--settings".to_owned(), settings.to_owned()],
                    line: format!("cd /home/notes && claude --settings '{settings}'"),
                    to_do: Vec::new(),
                    loads: vec!["alpha".to_owned()],
                    missing: vec!["One server of Work is no longer configured.".to_owned()],
                    extra: vec![
                        "beta: allowedMcpServers in .claude/settings.local.json lets it load too."
                            .to_owned(),
                    ],
                }),
            })
            .unwrap();
        h.run();
        shot(&mut h, name);
        all.extend_harness(&mut h);
    }
}
