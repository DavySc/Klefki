//! The Status tab's "Copy Inspector command" (decision 014): asked for on a click, copied when it arrives.

use super::*;
use crate::shell::inspector::COPY;
use klefki_core::api::{InspectorCommand, TransportKind};

const LINE: &str = "npx @modelcontextprotocol/inspector -- npx -y @scope/server";

fn found() -> InspectorCommand {
    InspectorCommand {
        text: LINE.to_owned(),
        to_do: vec!["Enter TOKEN in the Inspector, under Environment Variables.".to_owned()],
        notes: vec!["Needs Node 22.19 or newer.".to_owned()],
    }
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

fn shown(
    row: ServerRow,
) -> (
    Harness<'static, ShellApp>,
    klefki_core::api::Detached,
    ServerRow,
) {
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, vec![row.clone()]);
    open_status_tab(&mut h, &row.name);
    drain(&engine);
    (h, engine, row)
}

fn answer(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    row: &ServerRow,
    line: Result<InspectorCommand, String>,
) {
    engine
        .events
        .send(Event::InspectorCommand { id: row.id(), line })
        .unwrap();
    h.step();
}

#[test]
fn nothing_is_asked_until_the_button_is_pressed() {
    let (mut h, engine, _row) = shown(live_row("s", Scope::User, None));
    h.run();
    assert!(
        drain(&engine)
            .iter()
            .all(|c| !matches!(c, Command::InspectorCommand(_)))
    );
    assert!(h.query_by_label(LINE).is_none());
}

#[test]
fn pressing_it_asks_the_engine_once_and_copies_the_line_when_it_comes() {
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.get_by_label(COPY).click();
    h.step();
    assert_eq!(drain(&engine), vec![Command::InspectorCommand(row.id())]);
    // Nothing is copied before there is a line.
    assert!(copied(&h).is_empty());
    answer(&mut h, &engine, &row, Ok(found()));
    assert_eq!(copied(&h), vec![LINE.to_owned()]);
    // Shown too, with what to know before running it.
    h.get_by_label(LINE);
    h.get_by_label_contains("Enter TOKEN in the Inspector");
    h.get_by_label_contains("Needs Node 22.19 or newer.");
    // Once, not every frame; the button says Copied in the next one.
    h.step();
    assert!(copied(&h).is_empty());
    h.get_by_label("Copied");
}

#[test]
fn a_server_with_no_line_says_why_and_copies_nothing() {
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.get_by_label(COPY).click();
    h.step();
    answer(
        &mut h,
        &engine,
        &row,
        Err("the Inspector does not speak WebSocket".to_owned()),
    );
    h.step();
    assert!(copied(&h).is_empty());
    h.get_by_label_contains(
        "There is no Inspector command: the Inspector does not speak WebSocket",
    );
    assert!(h.query_by_label("Copied").is_none());
}

#[test]
fn an_answer_for_another_server_is_not_shown_or_copied() {
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.get_by_label(COPY).click();
    h.step();
    let other = live_row("other", Scope::User, None);
    answer(&mut h, &engine, &other, Ok(found()));
    h.step();
    assert!(copied(&h).is_empty());
    assert!(h.query_by_label(LINE).is_none());
    let _ = row;
}

#[test]
fn the_button_is_there_for_servers_the_inspector_can_open_only() {
    for (scope, transport, offered) in [
        (Scope::User, TransportKind::Stdio, true),
        (Scope::Local, TransportKind::Http, true),
        (Scope::Project, TransportKind::Sse, true),
        (Scope::User, TransportKind::Ws, false),
        (Scope::Plugin, TransportKind::Stdio, false),
        (Scope::Managed, TransportKind::Http, false),
        (Scope::ClaudeAi, TransportKind::Http, false),
    ] {
        let project = matches!(scope, Scope::Local | Scope::Project).then_some("/work/app");
        let mut row = live_row("s", scope, project);
        row.transport = transport;
        let (mut h, engine) = live_harness();
        show_rows(&mut h, &engine, vec![row]);
        open_status_tab_or_select(&mut h);
        assert_eq!(
            h.query_by_label(COPY).is_some(),
            offered,
            "{scope:?} {transport:?}"
        );
    }
}

/// The Status tab where there is one, else only the row.
fn open_status_tab_or_select(h: &mut Harness<'static, ShellApp>) {
    use egui::accesskit::Role;
    h.get_by_label("s").click();
    h.run();
    if let Some(tab) = h.query_by_role_and_label(Role::Button, "Status") {
        tab.click();
        h.run();
    }
}

#[test]
fn a_server_claude_code_does_not_load_still_offers_it() {
    let mut row = live_row("s", Scope::User, None);
    row.not_loaded = Some("Your organisation sets this server.".to_owned());
    let (h, _engine, _row) = shown(row);
    h.get_by_label(COPY);
}

#[test]
fn the_palette_copies_it_for_the_selected_server() {
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run();
    h.get_by_label_contains("Copy Inspector command for s")
        .click();
    h.run();
    assert_eq!(drain(&engine), vec![Command::InspectorCommand(row.id())]);
    answer(&mut h, &engine, &row, Ok(found()));
    assert_eq!(copied(&h), vec![LINE.to_owned()]);
}

#[test]
fn a_reloaded_configuration_forgets_the_line() {
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.get_by_label(COPY).click();
    h.step();
    answer(&mut h, &engine, &row, Ok(found()));
    h.run();
    h.get_by_label(LINE);
    show_rows(&mut h, &engine, vec![row]);
    assert!(h.query_by_label(LINE).is_none());
}

#[test]
fn copying_says_copied_for_a_moment_and_a_screen_reader_hears_it() {
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.get_by_label(COPY).click();
    h.step();
    answer(&mut h, &engine, &row, Ok(found()));
    h.step();
    let node = h.get_by_label("Copied");
    assert_eq!(node.accesskit_node().live(), egui::accesskit::Live::Polite);
    let mut steps = 0;
    while h.query_by_label("Copied").is_some() {
        h.step();
        steps += 1;
        assert!(steps < 100, "still Copied");
    }
    h.get_by_label(COPY);
}

#[test]
fn a_line_that_comes_after_the_user_went_to_another_tab_is_not_copied_then_or_later() {
    use egui::accesskit::Role;
    let (mut h, engine, row) = shown(live_row("s", Scope::User, None));
    h.get_by_label(COPY).click();
    h.step();
    h.get_by_role_and_label(Role::Button, "General").click();
    h.run();
    answer(&mut h, &engine, &row, Ok(found()));
    h.run();
    assert!(copied(&h).is_empty());
    // Back on the Status tab, the line is there to be seen, and the clipboard was not touched.
    h.get_by_role_and_label(Role::Button, "Status").click();
    h.step();
    assert!(copied(&h).is_empty());
    h.run();
    assert!(copied(&h).is_empty());
    h.get_by_label(LINE);
}

#[test]
fn a_diagnosis_stays_with_the_status_above_it_and_the_section_comes_after() {
    let (mut h, engine) = live_harness();
    let row = live_row("bad", Scope::User, None);
    engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: diagnosis(
                Status::Failed,
                &[("The command was not found", "It is not on the PATH.")],
                &[],
                &[],
                None,
            ),
        })
        .unwrap();
    h.run();
    open_status_tab(&mut h, "bad");
    let diagnosis = h.get_by_label("The command was not found").rect().top();
    let section = h.get_by_label("MCP Inspector").rect().top();
    assert!(diagnosis < section, "{diagnosis} {section}");
}
