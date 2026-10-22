//! The Status tab's "Copy debug report": asked for on a click, copied when the definition arrives.

use super::*;
use crate::shell::debug_report::COPY;
use klefki_core::api::{Exported, Hint, LastStart, LastStarts, StartOutcome, Variable};

fn definition() -> Exported {
    Exported {
        text: "{\n  \"mcpServers\": {\n    \"s\": {\"command\": \"npx\", \"env\": {\"TOKEN\": \"${TOKEN}\"}}\n  }\n}"
            .to_owned(),
        servers: 1,
        left_out: 0,
        variables: vec![Variable {
            name: "TOKEN".to_owned(),
            default: None,
            replaces: vec!["s: env TOKEN".to_owned()],
        }],
        notes: Vec::new(),
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

fn shown() -> (
    Harness<'static, ShellApp>,
    klefki_core::api::Detached,
    ServerRow,
) {
    let (mut h, engine) = live_harness();
    let row = live_row("s", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    open_status_tab(&mut h, "s");
    drain(&engine);
    (h, engine, row)
}

/// The engine's answers to a click: the last starts, then the definition (and the status line, hidden).
fn answer(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    row: &ServerRow,
    result: Result<Exported, String>,
) {
    answer_starts(h, engine, row, LastStarts::default());
    answer_definition(h, engine, row, result, Vec::new());
}

fn answer_starts(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    row: &ServerRow,
    starts: LastStarts,
) {
    engine
        .events
        .send(Event::LastStarts {
            id: row.id(),
            starts,
        })
        .unwrap();
    h.step();
}

fn answer_definition(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    row: &ServerRow,
    result: Result<Exported, String>,
    also: Vec<String>,
) {
    engine
        .events
        .send(Event::RedactedDefinition {
            id: row.id(),
            result,
            also,
        })
        .unwrap();
    h.step();
}

fn asked(engine: &klefki_core::api::Detached) -> Vec<Command> {
    drain(engine)
}

#[test]
fn nothing_is_asked_until_the_button_is_pressed() {
    let (mut h, engine, _row) = shown();
    h.run();
    assert!(
        drain(&engine)
            .iter()
            .all(|c| !matches!(c, Command::RedactedDefinition { .. }))
    );
    h.get_by_label(COPY);
}

#[test]
fn pressing_it_asks_once_and_copies_the_report_when_the_definition_comes() {
    let (mut h, engine, row) = shown();
    engine
        .events
        .send(Event::CliVersion(Some("2.1.308".to_owned())))
        .unwrap();
    engine
        .events
        .send(status_event(
            &row,
            Status::Failed,
            Some("Connection closed"),
        ))
        .unwrap();
    h.run();
    h.get_by_label(COPY).click();
    h.step();
    // A fresh read of the last starts and the definition, with the status line to hide what is secret in.
    let wanted = asked(&engine);
    assert!(
        wanted.contains(&Command::LastStarts(row.id())),
        "{wanted:?}"
    );
    assert_eq!(
        wanted
            .iter()
            .filter(|c| matches!(c, Command::RedactedDefinition { .. }))
            .collect::<Vec<_>>(),
        [&Command::RedactedDefinition {
            id: row.id(),
            also: vec!["Connection closed".to_owned()],
        }]
    );
    assert!(
        copied(&h).is_empty(),
        "nothing is copied before there is a definition"
    );
    answer_starts(&mut h, &engine, &row, LastStarts::default());
    answer_definition(
        &mut h,
        &engine,
        &row,
        Ok(definition()),
        vec!["Connection closed".to_owned()],
    );
    let [text] = &copied(&h)[..] else {
        panic!("copied once: {:?}", copied(&h));
    };
    for needle in [
        "Klefki debug report",
        concat!("Klefki ", env!("CARGO_PKG_VERSION")),
        "Claude Code 2.1.308",
        "server: s (User)",
        "status: Failed: Connection closed",
        "${TOKEN}",
        "s: env TOKEN",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
    }
    // Once, not every frame; the button says Copied now.
    h.step();
    assert!(copied(&h).is_empty());
    let node = h.get_by_label("Copied");
    assert_eq!(node.accesskit_node().live(), egui::accesskit::Live::Polite);
}

#[test]
fn the_report_holds_the_diagnosis_and_the_last_starts_when_there_are_some() {
    let (mut h, engine, row) = shown();
    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: diagnosis(
                Status::Failed,
                &[("The command was not found", "Install it.")],
                &["xyz is not recognized"],
                &["[ERROR] Connection failed"],
                None,
            ),
        })
        .unwrap();
    h.run();
    h.get_by_label(COPY).click();
    h.step();
    answer_starts(
        &mut h,
        &engine,
        &row,
        LastStarts {
            starts: vec![LastStart {
                folder: "~/proj".to_owned(),
                when: "Today 07:32".to_owned(),
                exact: String::new(),
                outcome: StartOutcome::Failed {
                    code: Some("CONNECTION_CLOSED".to_owned()),
                    said: "Connection closed".to_owned(),
                    after: None,
                },
                server_said: vec!["Error: Cannot find module 'foo'".to_owned()],
                hints: vec![Hint {
                    title: "A file the server needs is missing".to_owned(),
                    code: None,
                    help: Vec::new(),
                    points_at: None,
                }],
                partly: false,
                file: "~/.cache/x.jsonl".to_owned(),
            }],
            ..LastStarts::default()
        },
    );
    answer_definition(&mut h, &engine, &row, Ok(definition()), Vec::new());
    let text = copied(&h).concat();
    for needle in [
        "diagnosis:",
        "The command was not found",
        "xyz is not recognized",
        "Connection failed",
        "last starts by Claude Code:",
        "Today 07:32 in ~/proj: failed (CONNECTION_CLOSED)",
        "Cannot find module 'foo'",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
    }
    assert!(!text.contains('\u{1b}') && !text.contains('\r'), "{text:?}");
}

#[test]
fn a_server_with_no_definition_still_gets_a_report_that_says_why() {
    let (mut h, engine, row) = shown();
    h.get_by_label(COPY).click();
    h.step();
    answer(
        &mut h,
        &engine,
        &row,
        Err("it comes from a plugin".to_owned()),
    );
    let text = copied(&h).concat();
    assert!(
        text.contains("definition: not shown, it comes from a plugin"),
        "{text}"
    );
    assert!(text.contains("server: s (User)"), "{text}");
}

#[test]
fn an_answer_for_another_server_or_after_leaving_the_tab_is_not_copied() {
    use egui::accesskit::Role;
    let (mut h, engine, row) = shown();
    h.get_by_label(COPY).click();
    h.step();
    answer(
        &mut h,
        &engine,
        &live_row("other", Scope::User, None),
        Ok(definition()),
    );
    h.step();
    assert!(copied(&h).is_empty());
    h.get_by_role_and_label(Role::Button, "General").click();
    h.run();
    answer(&mut h, &engine, &row, Ok(definition()));
    h.run();
    assert!(copied(&h).is_empty());
    h.get_by_role_and_label(Role::Button, "Status").click();
    h.run();
    assert!(copied(&h).is_empty(), "not copied later either");
    h.get_by_label(COPY);
}

#[test]
fn the_palette_copies_it_for_the_selected_server() {
    let (mut h, engine, row) = shown();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run();
    h.get_by_label_contains("Copy debug report for s").click();
    h.run();
    let wanted = asked(&engine);
    assert!(
        wanted.contains(&Command::LastStarts(row.id())),
        "{wanted:?}"
    );
    assert!(
        wanted
            .iter()
            .any(|c| matches!(c, Command::RedactedDefinition { .. })),
        "{wanted:?}"
    );
    answer(&mut h, &engine, &row, Ok(definition()));
    assert!(copied(&h).concat().contains("Klefki debug report"));
}

#[test]
fn the_button_is_there_for_every_kind_of_server() {
    for scope in [
        Scope::User,
        Scope::Local,
        Scope::Project,
        Scope::Plugin,
        Scope::Managed,
        Scope::ClaudeAi,
    ] {
        let project = matches!(scope, Scope::Local | Scope::Project).then_some("/work/app");
        let (mut h, engine) = live_harness();
        show_rows(&mut h, &engine, vec![live_row("s", scope, project)]);
        h.get_by_label("s").click();
        h.run();
        if let Some(tab) = h.query_by_role_and_label(egui::accesskit::Role::Button, "Status") {
            tab.click();
            h.run();
        }
        assert!(h.query_by_label(COPY).is_some(), "{scope:?}");
    }
}

#[test]
fn copying_the_diagnosis_says_copied_and_its_hover_does_not_promise_all_secrets_are_gone() {
    let (mut h, engine, row) = shown();
    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: diagnosis(Status::Failed, &[], &["said"], &[], None),
        })
        .unwrap();
    h.run();
    h.get_by_label("Copy report").hover();
    h.run();
    h.get_by_label("Copy report").click();
    h.step();
    h.step();
    let node = h.get_by_label("Copied");
    assert_eq!(node.accesskit_node().live(), egui::accesskit::Live::Polite);
    let mut steps = 0;
    while h.query_by_label("Copied").is_some() {
        h.step();
        steps += 1;
        assert!(steps < 100, "still Copied");
    }
    h.get_by_label("Copy report");
}

#[test]
fn from_the_palette_on_another_tab_it_waits_for_the_last_starts() {
    use egui::accesskit::Role;
    let (mut h, engine, row) = shown();
    h.get_by_role_and_label(Role::Button, "General").click();
    h.run();
    drain(&engine);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run();
    h.get_by_label_contains("Copy debug report for s").click();
    h.run();
    let wanted = asked(&engine);
    // The starts are asked for before the definition, since the tab that would ask is not shown yet.
    let starts_at = wanted
        .iter()
        .position(|c| matches!(c, Command::LastStarts(_)));
    let definition_at = wanted
        .iter()
        .position(|c| matches!(c, Command::RedactedDefinition { .. }));
    assert!(
        starts_at < definition_at && starts_at.is_some(),
        "{wanted:?}"
    );
    // The definition first, as the engine may answer it first: nothing is copied until the starts are in.
    answer_definition(&mut h, &engine, &row, Ok(definition()), Vec::new());
    h.step();
    assert!(copied(&h).is_empty(), "{:?}", copied(&h));
    answer_starts(
        &mut h,
        &engine,
        &row,
        LastStarts {
            problem: Some("Klefki could not tell where Claude Code keeps its logs.".to_owned()),
            ..LastStarts::default()
        },
    );
    let text = copied(&h).concat();
    assert!(
        text.contains("last starts by Claude Code:")
            && text.contains("Klefki could not tell where Claude Code keeps its logs."),
        "{text}"
    );
}

#[test]
fn what_is_missing_is_said_and_not_left_out() {
    let (mut h, engine, row) = shown();
    h.get_by_label(COPY).click();
    h.step();
    answer(&mut h, &engine, &row, Ok(definition()));
    let text = copied(&h).concat();
    assert!(
        text.contains("diagnosis: not run (Diagnose on the Status tab runs it)"),
        "{text}"
    );
    assert!(
        text.contains("last starts by Claude Code:\n  none in Claude Code's logs"),
        "{text}"
    );
}

#[test]
fn the_status_line_in_the_report_is_the_one_the_engine_hid_secrets_in() {
    let (mut h, engine, row) = shown();
    engine
        .events
        .send(status_event(
            &row,
            Status::Failed,
            Some("bad key plain-secret-value"),
        ))
        .unwrap();
    h.run();
    h.get_by_label(COPY).click();
    h.step();
    answer_starts(&mut h, &engine, &row, LastStarts::default());
    answer_definition(
        &mut h,
        &engine,
        &row,
        Ok(definition()),
        vec!["bad key ***".to_owned()],
    );
    let text = copied(&h).concat();
    assert!(text.contains("status: Failed: bad key ***"), "{text}");
    assert!(!text.contains("plain-secret-value"), "{text}");
}

#[test]
fn what_was_copied_can_be_read_in_the_section() {
    let (mut h, engine, row) = shown();
    assert!(h.query_by_label("Show what was copied").is_none());
    h.get_by_label(COPY).click();
    h.step();
    answer(&mut h, &engine, &row, Ok(definition()));
    h.run();
    h.get_by_label("Show what was copied").click();
    h.run();
    h.get_by_label_contains("Klefki debug report");
    // Gone with the configuration it was made from.
    show_rows(&mut h, &engine, vec![row]);
    assert!(h.query_by_label("Show what was copied").is_none());
}
