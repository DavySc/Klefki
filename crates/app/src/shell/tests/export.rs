//! Export: what it takes, copying, and saving to a file.

use super::*;
use klefki_core::api::{Exported, FileChoice, SaveError};

/// An answer to Export. A variable's name ending in `=value` is a plain setting with that default.
fn exported_block(text: &str, variables: &[(&str, &str)], notes: &[&str]) -> Event {
    Event::Exported(Exported {
        text: text.to_owned(),
        servers: 1,
        left_out: 0,
        variables: variables
            .iter()
            .map(|(name, replaces)| {
                let (name, default) = match name.split_once('=') {
                    Some((name, default)) => (name, Some(default.to_owned())),
                    None => (*name, None),
                };
                klefki_core::api::Variable {
                    name: name.to_owned(),
                    default,
                    replaces: vec![(*replaces).to_owned()],
                }
            })
            .collect(),
        notes: notes.iter().map(|n| (*n).to_owned()).collect(),
    })
}

fn copied(h: &Harness<'_, ShellApp>) -> Vec<String> {
    h.output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn export_without_a_selection_takes_the_list_and_shows_the_block_to_copy() {
    let (mut h, engine) = live_harness();
    let rows = vec![
        live_row("beta", Scope::User, None),
        live_row("alpha", Scope::User, None),
        live_row("gamma", Scope::Local, Some("/work/app")),
    ];
    show_rows(&mut h, &engine, rows.clone());
    drain(&engine);
    tool(&h, Tool::Export).hover();
    h.run();
    h.get_by_label_contains("Export the servers in the list");
    press(&mut h, Tool::Export);
    // In the list's order.
    assert_eq!(
        drain(&engine),
        vec![Command::Export(vec![
            rows[1].id(),
            rows[0].id(),
            rows[2].id()
        ])]
    );
    h.get_by_label("Export 3 servers");
    // Which ones, without recalling the list.
    h.get_by_label("alpha, beta and gamma");
    h.get_by_label("Reading the definitions…");
    let block = "{\n  \"mcpServers\": {}\n}";
    engine
        .events
        .send(exported_block(
            block,
            &[
                ("GITHUB_TOKEN", "beta: env GITHUB_TOKEN"),
                ("LOG_LEVEL=debug", "gamma: env LOG_LEVEL"),
            ],
            &["beta signs in with OAuth: its client secret is not in the block. Type it in when you add it."],
        ))
        .unwrap();
    h.run();
    h.get_by_label("1 variable to set where you add them");
    h.get_by_label("GITHUB_TOKEN");
    h.get_by_label("beta: env GITHUB_TOKEN");
    h.get_by_label("LOG_LEVEL");
    h.get_by_label("= debug (default)");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy names")
        .click();
    h.step();
    assert_eq!(copied(&h), ["GITHUB_TOKEN=\n"], "only the ones to set");
    h.run();
    // The button that copied says so; the block's Copy does not claim the names.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
    assert!(h.query_by_label("Copy names").is_none());
    h.get_by_label_contains("its client secret is not in the block");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy")
        .click();
    h.step();
    assert_eq!(copied(&h), [block]);
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy names");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .click();
    h.run();
    assert!(h.query_by_label_contains("Export 3").is_none());
    assert!(h.state().exporting.is_none());
}

#[test]
fn export_with_a_selection_takes_only_that_and_enter_copies() {
    let (mut h, engine) = live_harness();
    let rows = vec![
        live_row("alpha", Scope::User, None),
        live_row("beta", Scope::User, None),
    ];
    show_rows(&mut h, &engine, rows.clone());
    click(&mut h, "beta");
    drain(&engine);
    tool(&h, Tool::Export).hover();
    h.run();
    h.get_by_label_contains("Export the selected servers");
    press(&mut h, Tool::Export);
    assert_eq!(drain(&engine), vec![Command::Export(vec![rows[1].id()])]);
    h.get_by_label("Export beta");
    // Nothing to copy yet: Enter does nothing.
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().exporting.as_ref().unwrap().copied.is_none());
    engine.events.send(exported_block("{}", &[], &[])).unwrap();
    h.run();
    h.get_by_label("Nothing was left out: no value needed a reference.");
    assert!(h.query_by_label("Copy names").is_none(), "no names to copy");
    h.key_press(egui::Key::Enter);
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
    // The confirmation goes after a few seconds, so a second copy is confirmed again.
    for _ in 0..20 {
        h.step();
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
    // Enter on a focused Close is Close's: it closes and copies nothing.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .focus();
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    assert!(copied(&h).is_empty(), "{:?}", copied(&h));
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().exporting.is_none());
}

#[test]
fn the_palette_offers_export_and_an_earlier_answer_does_not_fill_a_later_export() {
    let (mut h, engine) = live_harness();
    let rows = vec![live_row("alpha", Scope::User, None)];
    show_rows(&mut h, &engine, rows.clone());
    assert!(
        h.state()
            .commands()
            .iter()
            .any(|e| e.title == "Export the servers in the list"),
    );
    press(&mut h, Tool::Export);
    h.key_press(egui::Key::Escape);
    h.run();
    press(&mut h, Tool::Export);
    drain(&engine);
    let shown = |h: &Harness<'_, ShellApp>| {
        h.state()
            .exporting
            .as_ref()
            .and_then(|v| v.result.as_ref())
            .map(|r| r.text.clone())
    };
    engine
        .events
        .send(exported_block("first", &[], &[]))
        .unwrap();
    h.run();
    h.get_by_label("Reading the definitions…");
    assert_eq!(shown(&h), None);
    engine
        .events
        .send(exported_block("second", &[], &[]))
        .unwrap();
    h.run();
    assert_eq!(shown(&h).as_deref(), Some("second"));
}

#[test]
fn export_takes_the_whole_selection_even_rows_the_search_hides_now() {
    let (mut h, engine) = live_harness();
    let rows = vec![
        live_row("alpha", Scope::User, None),
        live_row("beta", Scope::User, None),
    ];
    show_rows(&mut h, &engine, rows.clone());
    click(&mut h, "beta");
    h.state_mut().search = "alpha".into();
    h.run();
    assert_eq!(h.state().visible(), [0], "the search hides beta");
    drain(&engine);
    press(&mut h, Tool::Export);
    assert_eq!(drain(&engine), vec![Command::Export(vec![rows[1].id()])]);
    h.get_by_label("Export beta");
    h.get_by_label("Includes 1 selected server the search hides.");
}

#[test]
fn export_says_when_the_search_narrowed_the_list() {
    let (mut h, engine) = live_harness();
    let rows = vec![
        live_row("alpha", Scope::User, None),
        live_row("alps", Scope::User, None),
        live_row("beta", Scope::User, None),
    ];
    show_rows(&mut h, &engine, rows.clone());
    h.state_mut().search = "alp".into();
    h.run();
    press(&mut h, Tool::Export);
    h.get_by_label("Export 2 servers");
    h.get_by_label("alpha and alps");
    h.get_by_label("Only the servers that match \u{201c}alp\u{201d}.");
}

/// Opens Export on `rows` (nothing selected) and fills it with `block`; the commands sent so far are drained.
fn export_open(
    rows: Vec<ServerRow>,
    block: &str,
) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, rows);
    press(&mut h, Tool::Export);
    engine.events.send(exported_block(block, &[], &[])).unwrap();
    h.run();
    drain(&engine);
    (h, engine)
}

#[test]
fn the_save_dialog_opens_over_klefkis_window_from_the_ui_thread() {
    let opened: Arc<std::sync::Mutex<Vec<(String, bool, std::thread::ThreadId)>>> = Arc::default();
    let (core, engine) = klefki_core::api::Core::detached_with_dialog({
        let opened = Arc::clone(&opened);
        move |name, owned| {
            opened
                .lock()
                .unwrap()
                .push((name.to_owned(), owned, std::thread::current().id()));
            None
        }
    });
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_eframe(move |cc| {
            crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
            ShellApp::with_core(core, ThemeChoice::Dark)
        });
    show_rows(&mut h, &engine, vec![live_row("github", Scope::User, None)]);
    press(&mut h, Tool::Export);
    engine
        .events
        .send(exported_block("{\n  \"mcpServers\": {}\n}", &[], &[]))
        .unwrap();
    h.run();
    drain(&engine);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    // Opened once, by the UI's own thread, with Klefki's window as its owner.
    assert_eq!(
        *opened.lock().unwrap(),
        vec![(
            "github-mcp.json".to_owned(),
            true,
            std::thread::current().id()
        )]
    );
    // The engine is handed that open dialog to wait for.
    let sent = drain(&engine);
    assert!(
        matches!(
            sent.as_slice(),
            [Command::ChooseExportFile { name, shown: Some(_) }] if name == "github-mcp.json"
        ),
        "{sent:?}"
    );
    // Nearby: more frames open no second dialog while the first is waiting.
    h.run();
    assert_eq!(opened.lock().unwrap().len(), 1);
    assert!(drain(&engine).is_empty());
}

#[test]
fn save_to_file_asks_where_in_the_systems_dialog_and_says_where_it_was_saved() {
    let block = "{\n  \"mcpServers\": {}\n}";
    let (mut h, engine) = export_open(vec![live_row("github", Scope::User, None)], block);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    // One server: its name is the file's.
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseExportFile {
            name: "github-mcp.json".into(),
            shown: None
        }]
    );
    // While the system's dialog is open, a second one is not asked for.
    assert!(
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .accesskit_node()
            .is_disabled()
    );
    let file = std::path::PathBuf::from("/home/me/github-mcp.json");
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::Chosen(file.clone())))
        .unwrap();
    h.run();
    // The system's dialog asked before replacing a file, so this does not ask again.
    assert_eq!(
        drain(&engine),
        vec![Command::SaveExport {
            path: file.clone(),
            text: block.into(),
            replace: true
        }]
    );
    engine
        .events
        .send(Event::ExportSaved {
            path: file.clone(),
            shown: "/home/me/github-mcp.json".into(),
            result: Ok(()),
        })
        .unwrap();
    h.run();
    // Said where a screen reader announces it too.
    let status = h.get_by_label("Saved to /home/me/github-mcp.json");
    assert_eq!(
        status.accesskit_node().live(),
        egui::accesskit::Live::Polite
    );
    assert!(
        !h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .accesskit_node()
            .is_disabled()
    );
    // Closing the system's dialog changes nothing.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    drain(&engine);
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::Cancelled {
            suggested: "/home/me/github-mcp.json".into(),
        }))
        .unwrap();
    h.run();
    assert!(drain(&engine).is_empty());
    assert!(h.state().exporting.is_some());
}

#[test]
fn a_long_saved_path_keeps_to_one_line_and_its_file_name_in_view() {
    let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    let long = format!("/{}/a-mcp.json", "a-very-long-folder-name/".repeat(12));
    let file = std::path::PathBuf::from(&long);
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::Chosen(file.clone())))
        .unwrap();
    h.run();
    engine
        .events
        .send(Event::ExportSaved {
            path: file,
            shown: long.clone(),
            result: Ok(()),
        })
        .unwrap();
    h.run();
    // Read out whole; drawn as one line ending in the file's name, inside the dialog.
    let said = format!("Saved to {long}");
    let rect = h.get_by_label(&said).rect();
    let copy = h
        .get_by_role_and_label(egui::accesskit::Role::Button, "Copy")
        .rect();
    assert!(rect.height() < copy.height(), "{rect:?}");
    // The dialog's content is 640 wide, starting where Copy does.
    assert!(
        rect.left() >= copy.left() && rect.right() <= copy.left() + 640.0,
        "{rect:?}"
    );
}

#[test]
fn after_the_dialog_closes_without_a_file_a_path_can_still_be_typed() {
    // Found in review: a desktop portal without a file chooser fails slowly, which looks like a cancel; without
    // this the user clicked Save to file… and nothing happened.
    let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    drain(&engine);
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::Cancelled {
            suggested: whole("/home/me/Documents/a-mcp.json").into(),
        }))
        .unwrap();
    h.run();
    assert!(
        h.query_by_label("Save to").is_none(),
        "a cancel opens nothing"
    );
    let said = h.get_by_label("Not saved.");
    assert_eq!(said.accesskit_node().live(), egui::accesskit::Live::Polite);
    click(&mut h, "Type a path instead");
    let field = h.get_by_label("Save to");
    assert_eq!(
        field.accesskit_node().value().as_deref(),
        Some(whole("/home/me/Documents/a-mcp.json").as_str())
    );
    assert!(
        h.query_by_label_contains("No save dialog could be opened")
            .is_none()
    );
    // The file's name is selected, as a save dialog does: typing replaces it and keeps the folder and `.json`.
    h.get_by_label("Save to").type_text("servers");
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
        .click();
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::SaveExport {
            path: whole("/home/me/Documents/servers.json").into(),
            text: "{}".into(),
            replace: false
        }]
    );
}

#[test]
fn on_a_short_window_the_variables_fold_when_saving_needs_the_room() {
    let (mut h, engine) = live_harness();
    h.set_size(egui::vec2(800.0, 480.0));
    show_rows(
        &mut h,
        &engine,
        vec![
            live_row("a", Scope::User, None),
            live_row("b", Scope::User, None),
        ],
    );
    press(&mut h, Tool::Export);
    engine
        .events
        .send(exported_block("{}", &[("TOKEN", "a: env TOKEN")], &[]))
        .unwrap();
    h.run();
    // Opened by hand (as one left open at a larger size stays open).
    h.get_by_label("1 variable to set where you add them")
        .click();
    h.run();
    h.get_by_label("a: env TOKEN");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::NoDialog {
            suggested: "/home/me/mcp-servers.json".into(),
        }))
        .unwrap();
    h.run();
    assert!(
        h.query_by_label("a: env TOKEN").is_none(),
        "folded for the path row"
    );
    // Once: opened again, it stays open.
    h.get_by_label("1 variable to set where you add them")
        .click();
    h.run();
    h.get_by_label("a: env TOKEN");
    let close = h
        .get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .rect();
    assert!(close.bottom() <= 480.0, "{close:?}");
}

#[test]
fn copied_is_announced_to_screen_readers() {
    let (mut h, _engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
    for label in ["Copy", "Copied"] {
        let button = h.get_by_role_and_label(egui::accesskit::Role::Button, label);
        // A polite live region: the change of label is read out, without moving the focus.
        assert_eq!(
            button.accesskit_node().live(),
            egui::accesskit::Live::Polite,
            "{label}"
        );
        if label == "Copy" {
            button.click();
            h.run();
        }
    }
}

#[test]
fn a_failed_save_says_why_and_the_block_stays() {
    let (mut h, engine) = export_open(
        vec![
            live_row("a", Scope::User, None),
            live_row("b", Scope::User, None),
        ],
        "{}",
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseExportFile {
            name: "mcp-servers.json".into(),
            shown: None
        }]
    );
    let file = std::path::PathBuf::from("/home/me/.claude.json");
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::Chosen(file.clone())))
        .unwrap();
    h.run();
    drain(&engine);
    engine
        .events
        .send(Event::ExportSaved {
            path: file,
            shown: "/home/me/.claude.json".into(),
            result: Err(SaveError::Refused(
                "That is Claude Code's own configuration.".into(),
            )),
        })
        .unwrap();
    h.run();
    let status = h.get_by_label("Not saved: That is Claude Code's own configuration.");
    assert_eq!(
        status.accesskit_node().live(),
        egui::accesskit::Live::Polite
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
}

#[test]
fn without_a_save_dialog_a_path_is_typed_and_an_existing_file_is_replaced_only_when_confirmed() {
    let (mut h, engine) = export_open(
        vec![
            live_row("a", Scope::User, None),
            live_row("b", Scope::User, None),
        ],
        "{}",
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    drain(&engine);
    let suggested = std::path::PathBuf::from(whole("/home/me/Documents/mcp-servers.json"));
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::NoDialog {
            suggested: suggested.clone(),
        }))
        .unwrap();
    h.run();
    h.get_by_label_contains("No save dialog could be opened");
    let field = h.get_by_label("Save to");
    assert_eq!(
        field.accesskit_node().value().as_deref(),
        Some(whole("/home/me/Documents/mcp-servers.json").as_str())
    );
    // The field has the keyboard, so Enter saves (and does not copy).
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(copied(&h).is_empty());
    assert_eq!(
        drain(&engine),
        vec![Command::SaveExport {
            path: suggested.clone(),
            text: "{}".into(),
            replace: false
        }]
    );
    engine
        .events
        .send(Event::ExportSaved {
            path: suggested.clone(),
            shown: whole("/home/me/Documents/mcp-servers.json"),
            result: Err(SaveError::Exists),
        })
        .unwrap();
    h.run();
    h.get_by_label("mcp-servers.json is there already. Replace it?");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Keep it")
        .click();
    h.run();
    assert!(drain(&engine).is_empty());
    assert!(h.query_by_label_contains("is there already").is_none());
    h.get_by_label("Save to");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
        .click();
    h.run();
    drain(&engine);
    engine
        .events
        .send(Event::ExportSaved {
            path: suggested.clone(),
            shown: whole("/home/me/Documents/mcp-servers.json"),
            result: Err(SaveError::Exists),
        })
        .unwrap();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Replace")
        .click();
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::SaveExport {
            path: suggested.clone(),
            text: "{}".into(),
            replace: true
        }]
    );
    engine
        .events
        .send(Event::ExportSaved {
            path: suggested,
            shown: whole("/home/me/Documents/mcp-servers.json"),
            result: Ok(()),
        })
        .unwrap();
    h.run();
    h.get_by_label(&format!(
        "Saved to {}",
        whole("/home/me/Documents/mcp-servers.json")
    ));
    assert!(
        h.query_by_label("Save to").is_none(),
        "the path field is done"
    );
}

#[test]
fn a_typed_path_must_be_a_whole_path_and_quotes_around_it_are_dropped() {
    let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::NoDialog {
            suggested: std::path::PathBuf::from("/home/me/a-mcp.json"),
        }))
        .unwrap();
    h.run();
    drain(&engine);
    h.get_by_label("Save to").focus();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Save to").type_text("servers.json");
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
        .click();
    h.run();
    assert!(drain(&engine).is_empty());
    h.get_by_label_contains("the whole path");
    let whole = if cfg!(windows) {
        r"C:\me\servers.json"
    } else {
        "/me/servers.json"
    };
    h.get_by_label("Save to").focus();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Save to").type_text(&format!("\"{whole}\""));
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
        .click();
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::SaveExport {
            path: std::path::PathBuf::from(whole),
            text: "{}".into(),
            replace: false
        }]
    );
}

#[test]
fn a_dialog_answer_after_export_was_closed_saves_nothing() {
    let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
        .click();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .click();
    h.run();
    // Open again, with another block.
    press(&mut h, Tool::Export);
    engine
        .events
        .send(exported_block("second", &[], &[]))
        .unwrap();
    h.run();
    drain(&engine);
    engine
        .events
        .send(Event::ExportFileChosen(FileChoice::Chosen(
            "/x/a.json".into(),
        )))
        .unwrap();
    h.run();
    assert!(
        drain(&engine).is_empty(),
        "the file was picked for the export that was closed"
    );
    assert!(
        !h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn export_is_off_when_the_list_is_empty() {
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, Vec::new());
    assert!(tool(&h, Tool::Export).accesskit_node().is_disabled());
    tool(&h, Tool::Export).hover();
    h.run();
    h.get_by_label_contains("no server to export");
}
