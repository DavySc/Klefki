//! The detail panel: the diagnosis in the Status tab, and long text that wraps instead of widening the panel.

use super::*;
use crate::shell::detail::diagnosis_report;

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

/// Where the detail panel's content can be seen: the height of its scroll bar, the rightmost one, when it has
/// one; otherwise under its tabs to the bottom of the window.
fn detail_view(h: &Harness<'static, ShellApp>, size: egui::Vec2) -> egui::Rect {
    use egui::accesskit::Role;
    let bar = h
        .query_all_by_role(Role::ScrollBar)
        .map(|b| b.rect())
        .max_by(|a, b| a.right().total_cmp(&b.right()))
        .filter(|b| b.right() > size.x - 40.0);
    let (top, bottom) = match bar {
        Some(b) => (b.top(), b.bottom()),
        None => (
            h.get_by_role_and_label(Role::Button, "General")
                .rect()
                .bottom(),
            size.y,
        ),
    };
    egui::Rect::from_min_max(
        egui::pos2(0.0, top.max(0.0)),
        egui::pos2(size.x, bottom.min(size.y)),
    )
}

/// Turns the wheel over the detail panel, a notch at a time, until `label` can be seen, and says where it was at
/// first and where it ended up. It must start out of view, or the test proves nothing.
fn wheel_the_detail_panel_to(
    h: &mut Harness<'static, ShellApp>,
    size: egui::Vec2,
    label: &str,
) -> (egui::Rect, egui::Rect) {
    let first = h.get_by_label(label).rect();
    let view = detail_view(h, size);
    assert!(
        !view.contains_rect(first),
        "{label} is already in view at {first:?} in {view:?}; the panel must be taller than that"
    );
    let over = egui::pos2(first.center().x, view.center().y);
    let mut rect = first;
    for _ in 0..60 {
        if detail_view(h, size).contains_rect(rect) {
            break;
        }
        h.event(egui::Event::PointerMoved(over));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        for _ in 0..10 {
            h.step();
        }
        rect = h.get_by_label(label).rect();
    }
    (first, rect)
}

/// The live rows `rows` at `size`, the last one shown on its Status tab with a diagnosis of `lines` lines of what
/// the server said: taller than the window.
fn long_diagnosis_shown(
    size: egui::Vec2,
    rows: &[ServerRow],
    lines: usize,
) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness();
    h.set_size(size);
    let row = rows.last().expect("a row to show");
    engine.events.send(snapshot_of(rows.to_vec())).unwrap();
    let said: Vec<String> = (0..lines).map(|i| format!("server line {i}")).collect();
    let said: Vec<&str> = said.iter().map(String::as_str).collect();
    engine
        .events
        .send(klefki_core::api::Event::Diagnosis {
            id: row.id(),
            diagnosis: diagnosis(
                Status::Failed,
                &[
                    ("The command was not found", "Claude could not start the server because the program is not on the PATH it uses."),
                    ("The server stopped while starting", "It closed its output before it answered, which usually means it crashed on start-up."),
                    ("A value it needs is missing", "Check the environment variables the server's documentation lists."),
                ],
                &said,
                &["[ERROR] MCP server \"bad-stdio\" Connection failed (CONNECTION_CLOSED)"],
                Some("the client wrote no debug output"),
            ),
        })
        .unwrap();
    h.run();
    open_status_tab(&mut h, &row.name);
    (h, engine)
}

#[test]
fn a_long_diagnosis_scrolls_into_view() {
    // Several causes and what the server said make the Status tab taller than the window; the report, and the
    // button that copies it, must still be reachable.
    for size in SIZES {
        let row = live_row("bad-stdio", Scope::User, None);
        let (mut h, _engine) = long_diagnosis_shown(size, &[row], 8);
        let (_, rect) = wheel_the_detail_panel_to(&mut h, size, "Copy report");
        assert!(
            detail_view(&h, size).contains_rect(rect),
            "{size:?}: Copy report at {rect:?}"
        );
        // What was scrolled past is still there, above.
        h.get_by_label("The command was not found");
    }
}

#[test]
fn the_name_and_tabs_stay_in_view_while_the_details_scroll() {
    use egui::accesskit::Role;
    for size in SIZES {
        let row = live_row("bad-stdio", Scope::User, None);
        let (mut h, _engine) = long_diagnosis_shown(size, &[row], 8);
        let tabs = h.get_by_role_and_label(Role::Button, "General").rect();
        wheel_the_detail_panel_to(&mut h, size, "Copy report");
        assert_eq!(
            h.get_by_role_and_label(Role::Button, "General").rect(),
            tabs,
            "{size:?}: the tabs moved"
        );
        // The heading, and the row in the list, which both name the server.
        for name in h.query_all_by_label("bad-stdio") {
            assert!(
                name.rect().top() >= 0.0,
                "{size:?}: a name at {:?}",
                name.rect()
            );
        }
        h.get_by_role_and_label(Role::Button, "General").click();
        h.run();
        h.get_by_label("Shadowed");
    }
}

#[test]
fn another_servers_details_start_at_the_top() {
    let (mut h, engine) = live_harness();
    h.set_size(OPENING_SIZE);
    let a = live_row("first-stdio", Scope::User, None);
    let b = live_row("second-stdio", Scope::User, None);
    engine
        .events
        .send(snapshot_of(vec![a.clone(), b.clone()]))
        .unwrap();
    let said: Vec<String> = (0..30).map(|i| format!("server line {i}")).collect();
    let said: Vec<&str> = said.iter().map(String::as_str).collect();
    for row in [&a, &b] {
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(Status::Failed, &[], &said, &[], None),
            })
            .unwrap();
    }
    h.run();
    open_status_tab(&mut h, "first-stdio");
    let (first, scrolled) = wheel_the_detail_panel_to(&mut h, OPENING_SIZE, "Copy report");
    assert!(scrolled.top() < first.top());
    click(&mut h, "second-stdio");
    let rect = h.get_by_label("Copy report").rect();
    assert!(
        (rect.top() - first.top()).abs() < 1.0,
        "at {rect:?}, not where the first one started, {first:?}"
    );
}

/// Scrolls the last of `rows` to its Copy report, then lists `then` instead, and says where Copy report was
/// before and after.
fn copy_report_across_a_new_list(
    rows: &[ServerRow],
    then: Vec<ServerRow>,
) -> (egui::Rect, egui::Rect) {
    let (mut h, engine) = long_diagnosis_shown(OPENING_SIZE, rows, 30);
    let (_, before) = wheel_the_detail_panel_to(&mut h, OPENING_SIZE, "Copy report");
    show_rows(&mut h, &engine, then);
    (before, h.get_by_label("Copy report").rect())
}

#[test]
fn a_server_added_above_leaves_the_details_where_they_were() {
    // Rows are kept by position, so the server shown moves down a place when one is added above it (Add,
    // Duplicate, or `claude mcp add` in a terminal). Where its details were scrolled to goes with it.
    let (a, b, c) = (
        live_row("a-stdio", Scope::User, None),
        live_row("b-stdio", Scope::User, None),
        live_row("c-stdio", Scope::User, None),
    );
    let (before, after) = copy_report_across_a_new_list(&[b.clone(), c.clone()], vec![a, b, c]);
    assert!(
        (after.top() - before.top()).abs() < 1.0,
        "{before:?} then {after:?}"
    );
}

#[test]
fn a_server_removed_above_leaves_the_details_where_they_were() {
    let (a, b, c) = (
        live_row("a-stdio", Scope::User, None),
        live_row("b-stdio", Scope::User, None),
        live_row("c-stdio", Scope::User, None),
    );
    let (before, after) = copy_report_across_a_new_list(&[a, b.clone(), c.clone()], vec![b, c]);
    assert!(
        (after.top() - before.top()).abs() < 1.0,
        "{before:?} then {after:?}"
    );
}

/// A row that failed, with an explanation of several steps on its Status tab.
fn failed_with_steps(row: &ServerRow) -> klefki_core::api::Event {
    klefki_core::api::Event::Status {
        id: row.id(),
        status: Status::Failed,
        detail: Some("spawn npx ENOENT".to_owned()),
        hint: Some(klefki_core::api::Hint {
            title: "The command was not found".to_owned(),
            code: Some("ENOENT".to_owned()),
            help: vec![
                "Claude Code could not find npx on the PATH it starts servers with.".to_owned(),
                "Install Node.js, which brings npx with it, then open a new terminal so the PATH is read again.".to_owned(),
                "If it is installed somewhere unusual, write the whole path to npx as the command instead.".to_owned(),
                "On Windows, a server started through npx may need `cmd /c npx …` as its command.".to_owned(),
                "Check that the server's package name is spelled as its documentation gives it, scope included.".to_owned(),
                "Run the command yourself in a terminal to see the error it prints before Claude Code does.".to_owned(),
                "If the server needs a newer Node.js than the one installed, update Node.js and check again.".to_owned(),
            ],
            points_at: None,
        }),
        timing: None,
    }
}

#[test]
fn the_status_actions_are_in_view_without_scrolling() {
    for size in SIZES {
        let (mut h, engine) = live_harness();
        h.set_size(size);
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(failed_with_steps(&row)).unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        h.get_by_label_contains("write the whole path to npx");
        let view = detail_view(&h, size);
        for action in ["Check now", "Diagnose"] {
            let rect = h.get_by_label(action).rect();
            assert!(view.contains_rect(rect), "{size:?}: {action} at {rect:?}");
        }
    }
}

#[test]
fn a_diagnosis_that_arrives_is_brought_into_view() {
    for size in SIZES {
        let (mut h, engine) = live_harness();
        h.set_size(size);
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(failed_with_steps(&row)).unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(
                    Status::Failed,
                    &[("The command was not found", "Install Node.js.")],
                    &["server line"],
                    &[],
                    None,
                ),
            })
            .unwrap();
        h.run();
        let rect = h.get_by_label("What probably went wrong").rect();
        assert!(
            detail_view(&h, size).contains_rect(rect),
            "{size:?}: the diagnosis starts at {rect:?}"
        );
    }
}

#[test]
fn a_diagnosis_of_a_server_not_shown_waits_where_it_is() {
    // Brought into view only for the server being looked at: one opened later starts at the top.
    let (mut h, engine) = live_harness();
    h.set_size(OPENING_SIZE);
    let (a, b) = (
        live_row("a-stdio", Scope::User, None),
        live_row("b-stdio", Scope::User, None),
    );
    engine
        .events
        .send(snapshot_of(vec![a.clone(), b.clone()]))
        .unwrap();
    engine.events.send(failed_with_steps(&b)).unwrap();
    h.run();
    open_status_tab(&mut h, "a-stdio");
    engine
        .events
        .send(klefki_core::api::Event::Diagnosis {
            id: b.id(),
            diagnosis: diagnosis(Status::Failed, &[], &["server line"; 40], &[], None),
        })
        .unwrap();
    h.run();
    click(&mut h, "b-stdio");
    let view = detail_view(&h, OPENING_SIZE);
    let diagnose = h.get_by_label("Diagnose").rect();
    assert!(view.contains_rect(diagnose), "Diagnose at {diagnose:?}");
    let report = h.get_by_label("Copy report").rect();
    assert!(!view.contains_rect(report), "Copy report at {report:?}");
}

/// A definition with 14 arguments: its form is taller than the window.
fn long_definition() -> String {
    let args: Vec<String> = (0..14).map(|i| format!("\"--flag-{i}\"")).collect();
    format!(
        "{{\n  \"command\": \"node\",\n  \"args\": [{}]\n}}",
        args.join(", ")
    )
}

/// The form of [`long_definition`], at `size`.
fn long_form(size: egui::Vec2) -> Harness<'static, ShellApp> {
    let (mut h, _engine, _) = form_harness(&long_definition(), true);
    h.set_size(size);
    h.run();
    h
}

#[test]
fn a_long_definition_form_scrolls_to_its_last_field() {
    for size in SIZES {
        let mut h = long_form(size);
        let (_, rect) = wheel_the_detail_panel_to(&mut h, size, "Add argument");
        assert!(
            detail_view(&h, size).contains_rect(rect),
            "{size:?}: Add argument at {rect:?}"
        );
    }
}

#[test]
fn the_save_bar_stays_in_view_while_a_long_form_scrolls() {
    for size in SIZES {
        let mut h = long_form(size);
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let save = h.get_by_label("Review and save…").rect();
        assert!(
            window.contains_rect(save),
            "{size:?}: Review and save… at {save:?}"
        );
        wheel_the_detail_panel_to(&mut h, size, "Add argument");
        assert_eq!(h.get_by_label("Review and save…").rect(), save, "{size:?}");
        h.get_by_label("Revert");
    }
}

/// Where the outcome message saying `text` is: from the start of its text to its Dismiss button, the last thing
/// on it. (The text's own rect runs on below what is shown when it scrolls.)
fn outcome_rect(h: &Harness<'static, ShellApp>, text: &str) -> egui::Rect {
    egui::Rect::from_min_max(
        h.get_by_label(text).rect().min,
        h.get_by_label("Dismiss").rect().max,
    )
}

#[test]
fn the_outcome_message_never_covers_the_save_bar() {
    // Found in the UI review: for four seconds after a change was applied, its message sat over Review and
    // save… and Revert; one that offers Undo or warns stays until dismissed, and covered them all that time.
    // As long as one seen in the app, whose backup folder was deep in a temporary home.
    let long = "Moved files to webapp. The previous file is kept in \
        /home/someone/work/clients/acme/sandboxes/2026-10-19/home/.local/share/klefki/backups/\
        claude.json.0001792408049204-0.bak";
    let outcomes = [
        ("success", "Saved.", false, None),
        ("warning", long, true, None),
        (
            "undo",
            long,
            false,
            Some(Change::Move {
                id: live_row("alpha", Scope::User, None).id(),
                to: Target::User,
            }),
        ),
    ];
    for size in SIZES {
        for (kind, text, warn, undo) in outcomes.clone() {
            let mut h = long_form(size);
            form(&mut h).command = "node22".into();
            h.run();
            let save = h.get_by_label("Review and save…").rect();
            h.state_mut().outcome = Some(Outcome {
                text: text.into(),
                ok: true,
                shown_at: None,
                sign_in: None,
                warn,
                // With the button that opens the log, the widest the card's buttons get.
                logged: kind != "success",
                type_path: false,
                undo,
            });
            h.run();
            let toast = outcome_rect(&h, text);
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            // The bar, and the panel's width, stay put.
            assert_eq!(
                h.get_by_label("Review and save…").rect(),
                save,
                "{size:?} {kind}"
            );
            assert!(window.contains_rect(toast), "{size:?} {kind}: {toast:?}");
            for label in ["Review and save…", "Revert", "Unsaved"] {
                let rect = h.get_by_label(label).rect();
                assert!(
                    !toast.intersects(rect),
                    "{size:?} {kind}: the message at {toast:?} covers {label} at {rect:?}"
                );
            }
            // Nor the form: its last field can still be scrolled to, and is not under the message. A success
            // fades while the panel is wheeled, so only the ones that stay are tried.
            if kind != "success" {
                let (_, rect) = wheel_the_detail_panel_to(&mut h, size, "Add argument");
                let toast = outcome_rect(&h, text);
                assert!(
                    detail_view(&h, size).contains_rect(rect),
                    "{size:?} {kind}: Add argument can't be scrolled to while the message shows: {rect:?}"
                );
                assert!(
                    !toast.intersects(rect),
                    "{size:?} {kind}: the message at {toast:?} covers Add argument at {rect:?}"
                );
                click(&mut h, "Dismiss");
                assert_eq!(h.state().outcome, None, "{size:?} {kind}: Dismiss works");
            }
            click(&mut h, "Revert");
            assert_eq!(form(&mut h).command, "node", "{size:?} {kind}");
        }
    }
}

#[test]
fn why_saving_is_blocked_shows_beside_the_save_button() {
    for size in SIZES {
        let mut h = long_form(size);
        form(&mut h).command.clear();
        h.run();
        let Some(Editor {
            state:
                EditorState::Ready {
                    problem: Some(problem),
                    ..
                },
            ..
        }) = &h.state().editor
        else {
            panic!("an empty command is a problem");
        };
        let problem = capitalized(problem);
        let rect = h.get_by_label(&problem).rect();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        assert!(
            window.contains_rect(rect),
            "{size:?}: {problem:?} at {rect:?}"
        );
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
    }
}

#[test]
fn tab_from_the_last_field_goes_on_to_the_save_bar() {
    // The save bar sits below the form, so it comes next, not first; the way to the JSON ends the form.
    let mut h = long_form(OPENING_SIZE);
    form(&mut h).args[13].push('x');
    h.run();
    h.get_by_label("Add argument").focus();
    h.run();
    for next in ["Edit as JSON", "Review and save…"] {
        h.key_press(egui::Key::Tab);
        h.run();
        assert!(h.get_by_label(next).is_focused(), "{next}");
    }
}

#[test]
fn keyboard_focus_brings_what_it_reaches_into_view() {
    // egui does not bring a widget that gains focus into view, as GTK and WinUI do.
    for size in SIZES {
        let mut h = long_form(size);
        h.get_by_label("Remove argument 1").focus();
        h.run();
        for _ in 0..40 {
            if h.get_by_label("Add argument").is_focused() {
                break;
            }
            h.key_press(egui::Key::Tab);
            h.run();
        }
        let add = h.get_by_label("Add argument");
        assert!(add.is_focused(), "{size:?}: Tab reaches Add argument");
        let rect = add.rect();
        assert!(
            detail_view(&h, size).contains_rect(rect),
            "{size:?}: Add argument has focus at {rect:?}, out of view"
        );
    }
}

#[test]
fn focus_elsewhere_leaves_the_details_where_they_are() {
    let mut h = long_form(OPENING_SIZE);
    let (_, scrolled) = wheel_the_detail_panel_to(&mut h, OPENING_SIZE, "Add argument");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert_eq!(h.get_by_label("Add argument").rect(), scrolled);
}

#[test]
fn the_detail_panels_scroll_bar_leaves_the_buttons_beside_it_clickable() {
    // Also with a wider bar than egui's own: the room kept for it follows the style.
    for bar_width in [None, Some(16.0)] {
        let mut h = long_form(OPENING_SIZE);
        if let Some(width) = bar_width {
            h.ctx.all_styles_mut(|s| s.spacing.scroll.bar_width = width);
            h.run();
        }
        // The very edge of a button at the panel's right side, where the scroll bar shows.
        let rect = h.get_by_label("Remove argument 1").rect();
        click_at(&mut h, egui::pos2(rect.right() - 2.0, rect.center().y));
        assert_eq!(
            form(&mut h).args.len(),
            13,
            "{bar_width:?}: the click reached the button"
        );
        assert_eq!(form(&mut h).args[0], "--flag-1");
    }
}

/// A click of the mouse at `pos`, wherever that is.
fn click_at(h: &mut Harness<'static, ShellApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.run();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
    }
}

/// alpha's definition, `json`, asked for by `ask` at `size`, and arrived.
fn definition_asked_for(
    size: egui::Vec2,
    json: &str,
    ask: impl FnOnce(&mut Harness<'static, ShellApp>),
) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness();
    h.set_size(size);
    let (a, _) = two_rows();
    show_rows(&mut h, &engine, vec![a.clone()]);
    click(&mut h, "alpha");
    ask(&mut h);
    h.run();
    assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
    engine.events.send(entry_event(&a, json, true)).unwrap();
    h.run();
    (h, engine)
}

/// The detail panel's text fields, top to bottom: those under its tabs, right of the list.
fn detail_fields(h: &Harness<'static, ShellApp>) -> Vec<egui::Rect> {
    use egui::accesskit::Role;
    let tab = h.get_by_role_and_label(Role::Button, "General").rect();
    let mut fields: Vec<egui::Rect> = h
        .query_all_by_role(Role::TextInput)
        .chain(h.query_all_by_role(Role::PasswordInput))
        .map(|f| f.rect())
        .filter(|f| f.left() >= tab.left() && f.top() >= tab.bottom())
        .collect();
    fields.sort_by(|a, b| a.top().total_cmp(&b.top()));
    fields
}

#[test]
fn the_definition_asked_for_is_brought_into_view() {
    // Found in review: at the smallest size the form arrived below the summary, under the save bar, so Edit
    // definition looked like it did nothing.
    use egui::accesskit::Role;
    let asks: [(&str, Ask); 3] = [
        ("Edit definition on General", |h| {
            h.get_by_label("Edit definition").click();
        }),
        ("Ctrl+E on Status", |h| {
            h.get_by_role_and_label(Role::Button, "Status").click();
            h.run();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
        }),
        ("Edit definition on Env", |h| {
            h.get_by_role_and_label(Role::Button, "Env").click();
            h.run();
            h.get_by_label("Edit definition").click();
        }),
    ];
    for size in SIZES {
        for (how, ask) in asks {
            let (h, _engine) = definition_asked_for(size, SHORT_DEFINITION, ask);
            let view = detail_view(&h, size);
            let fields = detail_fields(&h);
            assert!(fields.len() >= 2, "{size:?}, {how}: {fields:?}");
            for field in fields {
                assert!(
                    view.contains_rect(field),
                    "{size:?}, {how}: a field at {field:?}, out of {view:?}"
                );
            }
            let end = h.get_by_label("Edit as JSON").rect();
            assert!(
                view.contains_rect(end),
                "{size:?}, {how}: the form ends at {end:?}, out of {view:?}"
            );
        }
        // Taller than the view, it comes in from its top.
        let (h, _engine) = definition_asked_for(size, &long_definition(), asks[0].1);
        let view = detail_view(&h, size);
        let first = detail_fields(&h)[0];
        assert!(
            view.contains_rect(first),
            "{size:?}, a long form: its first field at {first:?}, out of {view:?}"
        );
    }
}

#[test]
fn a_definition_that_arrives_while_another_server_is_shown_waits_where_it_is() {
    // Brought into view only while its server is being looked at: going back to it later starts at its summary.
    use egui::accesskit::Role;
    let (mut h, engine) = live_harness();
    h.set_size(SMALLEST_SIZE);
    let (a, b) = two_rows();
    show_rows(&mut h, &engine, vec![a.clone(), b.clone()]);
    click(&mut h, "alpha");
    click(&mut h, "Edit definition");
    assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
    click(&mut h, "beta");
    engine
        .events
        .send(entry_event(&a, SHORT_DEFINITION, true))
        .unwrap();
    h.run();
    click(&mut h, "alpha");
    let tab = h.get_by_role_and_label(Role::Button, "General").rect();
    let scope = h
        .get_all_by_label("Scope")
        .map(|l| l.rect())
        .find(|r| r.left() >= tab.left())
        .expect("the summary's first row");
    let view = detail_view(&h, SMALLEST_SIZE);
    assert!(
        view.contains_rect(scope),
        "Scope at {scope:?}, out of {view:?}"
    );
    h.get_by_label("Edit as JSON");
}

#[test]
fn a_second_click_where_edit_definition_was_leaves_the_form_a_form() {
    // Found in review: the save bar came in under the pointer at the smallest size, with the way to the JSON on
    // its second line, so a double click on Edit definition opened the raw JSON.
    for size in SIZES {
        let mut at = egui::Pos2::ZERO;
        let (mut h, engine) = definition_asked_for(size, SHORT_DEFINITION, |h| {
            at = h.get_by_label("Edit definition").rect().center();
            click_at(h, at);
        });
        click_at(&mut h, at);
        assert!(
            h.query_by_label("Back to the form").is_none(),
            "{size:?}: the second click at {at:?} opened the JSON"
        );
        h.get_by_label("Edit as JSON");
        assert_eq!(drain(&engine), vec![], "{size:?}");
    }
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
    show_rows(&mut h, &engine, vec![row.clone()]);
    open_status_tab(&mut h, "bad-stdio");
    click(&mut h, "Diagnose");
    assert_eq!(
        next_command(&engine).unwrap(),
        klefki_core::api::Command::Diagnose(row.id())
    );
    assert!(next_command(&engine).is_none(), "exactly one command");
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
    show_rows(&mut h, &engine, vec![a.clone(), b.clone()]);
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
        show_rows(&mut h, &engine, vec![live_row("srv", Scope::User, None)]);
        click(&mut h, "srv");
        detail_left_edge(&h, "Scope")
    };
    let long = {
        let (mut h, engine) = live_harness();
        let mut row = live_row("srv", Scope::User, None);
        row.target = format!(
            "https://example.com/{}",
            "very-long-path-segment/".repeat(20)
        );
        show_rows(&mut h, &engine, vec![row]);
        click(&mut h, "srv");
        detail_left_edge(&h, "Scope")
    };
    assert_eq!(short, long, "a long target made the detail panel wider");
}

#[test]
fn a_name_is_taken_in_the_ticked_places_that_have_it_however_the_project_is_spelled() {
    // Performance review: the check now goes through the rows once for all names. It must find the same places.
    let projects: Vec<String> = ["C:/Repos/WebApp", "/home/u/b", "/home/u/a", "/home/u/c"]
        .map(str::to_owned)
        .to_vec();
    let rows = vec![
        live_row("alpha", Scope::User, None),
        live_row("alpha", Scope::Local, Some("c:\\Repos\\WebApp\\")),
        live_row("alpha", Scope::Local, Some("/home/u/a")),
        live_row("alpha", Scope::Local, Some("/home/u/c")),
        // Only hidden by a new one, not refused: the review says so.
        live_row("alpha", Scope::Project, Some("/home/u/b")),
        live_row("beta", Scope::Local, Some("/home/u/b")),
    ];
    let mut places = Places::new(&projects, None);
    places.user = true;
    for at in [0, 1, 2] {
        places.projects[at].1 = true;
    }
    assert_eq!(
        places.taken("alpha", &rows).as_deref(),
        Some("for the user and in WebApp and a")
    );
    assert_eq!(places.taken("beta", &rows).as_deref(), Some("in b"));
    assert_eq!(places.taken("gamma", &rows), None);
    // Variant: the user not ticked, and a project ticked that has none.
    places.user = false;
    places.projects[0].1 = false;
    assert_eq!(places.taken("alpha", &rows).as_deref(), Some("in a"));
    places.projects[2].1 = false;
    assert_eq!(places.taken("alpha", &rows), None);
}

/// A check of `row` that ended as `status`, with how long it took.
fn timed_event(
    row: &ServerRow,
    status: Status,
    detail: Option<&str>,
    took: u64,
    set_in: klefki_core::api::LimitSetIn,
    given: bool,
) -> klefki_core::api::Event {
    use std::time::Duration;
    let given = given.then_some(klefki_core::api::LONGER_CHECK);
    klefki_core::api::Event::Status {
        id: row.id(),
        status,
        detail: detail.map(str::to_owned),
        hint: None,
        timing: Some(klefki_core::api::Timing {
            took: Duration::from_secs(took),
            limit: klefki_core::api::TimeLimit {
                limit: Duration::from_secs(30),
                change_in: match &set_in {
                    klefki_core::api::LimitSetIn::Organisation(_) => None,
                    _ => Some("~/.claude/settings.json".to_owned()),
                },
                set_in,
                unreadable: false,
            },
            timed_out: matches!(status, Status::Failed | Status::Unknown),
            given,
        }),
    }
}

const LONGER: &str = "Check again with 2 minutes";

#[test]
fn a_timeout_says_how_long_claude_code_waits_and_offers_more_time() {
    let (mut h, engine) = live_harness();
    let row = live_row("slow", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    let default = klefki_core::api::LimitSetIn::Default;
    engine
        .events
        .send(timed_event(
            &row,
            Status::Failed,
            Some("Request timed out"),
            28,
            default,
            false,
        ))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "slow");
    h.get_by_label_contains("It gave up after 28 s. Claude Code waits 30 s");
    click(&mut h, LONGER);
    assert_eq!(
        next_command(&engine).unwrap(),
        klefki_core::api::Command::CheckLonger(row.id())
    );
    assert!(next_command(&engine).is_none(), "exactly one command");
    // While it runs, it says why it takes longer, and is not offered again.
    engine
        .events
        .send(status_event(
            &row,
            Status::Checking,
            Some("giving it up to 2 minutes to connect"),
        ))
        .unwrap();
    h.run();
    h.get_by_label_contains("giving it up to 2 minutes to connect");
    assert!(h.query_by_label(LONGER).is_none());
}

#[test]
fn a_server_that_connects_given_more_time_says_where_to_give_it_that_time() {
    let (mut h, engine) = live_harness();
    let row = live_row("slow", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    let default = klefki_core::api::LimitSetIn::Default;
    engine
        .events
        .send(timed_event(
            &row,
            Status::Connected,
            None,
            41,
            default,
            true,
        ))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "slow");
    h.get_by_label_contains("It needs more time to start than Claude Code gives it");
    h.get_by_label_contains(
        "set \"MCP_TIMEOUT\": \"70000\" under \"env\" in ~/.claude/settings.json",
    );
    assert!(h.query_by_label(LONGER).is_none(), "it was given more time");
}

#[test]
fn a_server_that_connects_in_time_given_more_has_a_quiet_note() {
    let (mut h, engine) = live_harness();
    let row = live_row("slow", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    let default = klefki_core::api::LimitSetIn::Default;
    engine
        .events
        .send(timed_event(&row, Status::Connected, None, 8, default, true))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "slow");
    h.get_by_label_contains("It connected after 8 s, within the 30 s Claude Code waits");
    assert!(h.query_by_label_contains("needs more time").is_none());
}

#[test]
fn a_limit_the_organisation_sets_is_not_offered_more_time() {
    let (mut h, engine) = live_harness();
    let row = live_row("slow", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    let org =
        klefki_core::api::LimitSetIn::Organisation("/etc/claude-code/managed-settings.json".into());
    engine
        .events
        .send(timed_event(
            &row,
            Status::Failed,
            Some("Request timed out"),
            28,
            org,
            false,
        ))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "slow");
    h.get_by_label_contains("so only it can raise it");
    assert!(h.query_by_label(LONGER).is_none());
}
