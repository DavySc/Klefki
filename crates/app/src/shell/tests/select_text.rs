//! Text can be selected with the mouse and copied anywhere it is read, not only in fields: the detail panel, the
//! pages, dialogs and messages. The list's rows stay one click target, and offer their text on a right click instead.

use super::*;
use egui::accesskit::Role;

pub(super) fn copied(h: &Harness<'_, ShellApp>) -> Vec<String> {
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

/// Drags over the whole of `rect` with the mouse, as a person selecting text does, then copies (Ctrl+C or ⌘C,
/// which the window turns into a copy event). Returns what was copied.
pub(super) fn drag_and_copy(h: &mut Harness<'static, ShellApp>, rect: egui::Rect) -> String {
    // From the first line's start to the last line's end, for text that wraps.
    let from = rect.left_top() + egui::vec2(1.0, 2.0);
    let to = rect.right_bottom() - egui::vec2(1.0, 2.0);
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(rect.center());
    h.run();
    h.hover_at(to);
    h.run();
    h.drop_at(to);
    h.run();
    h.event(egui::Event::Copy);
    // Copied in the frame that reads the event; `run` would go on to frames with nothing in their output.
    h.step();
    copied(h).concat()
}

/// The text shown as `text`, exactly (a label's words are its value for a screen reader).
pub(super) fn text_rect(h: &Harness<'static, ShellApp>, text: &str) -> egui::Rect {
    h.query_all_by(|n| n.role() == Role::Label && n.value().as_deref() == Some(text))
        .next()
        .unwrap_or_else(|| panic!("no text {text:?}"))
        .rect()
}

#[test]
fn a_status_message_in_the_detail_panel_can_be_selected_and_copied() {
    let (mut h, engine) = live_harness();
    let row = live_row("alpha", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    let said = "connect ECONNREFUSED 127.0.0.1:3000";
    engine
        .events
        .send(status_event(&row, Status::Failed, Some(said)))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "alpha");
    let shown = format!("Claude Code said: {said}");
    let rect = text_rect(&h, &shown);
    assert_eq!(drag_and_copy(&mut h, rect), shown);
}

#[test]
fn text_on_a_page_can_be_selected_and_copied() {
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, Vec::new());
    h.get_by_role_and_label(Role::Button, "Settings").click();
    h.run();
    let node = h
        .query_all_by(|n| {
            n.role() == Role::Label && n.value().is_some_and(|v| v.split_whitespace().count() >= 5)
        })
        .next()
        .expect("a sentence on the Settings page");
    let text = node.accesskit_node().value().unwrap_or_default();
    let rect = node.rect();
    let got = drag_and_copy(&mut h, rect);
    assert!(
        !got.is_empty() && text.starts_with(got.trim_end()),
        "copied {got:?} from {text:?}"
    );
}

#[test]
fn dragging_across_a_rows_text_selects_no_text_and_a_click_still_picks_the_row() {
    let (mut h, engine) = live_harness();
    let (a, b) = two_rows();
    show_rows(&mut h, &engine, vec![a, b]);
    let rect = text_rect(&h, "beta");
    assert_eq!(
        drag_and_copy(&mut h, rect),
        "",
        "the row is not text to select"
    );
    click(&mut h, "beta");
    assert_eq!(h.state().selected.iter().copied().collect::<Vec<_>>(), [1]);
}

#[test]
fn right_clicking_a_row_offers_its_name_and_target_to_copy() {
    for (item, expected) in [("Copy name", "beta"), ("Copy target", "npx something")] {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a, b]);
        h.get_by_label("beta").click_secondary();
        h.run();
        assert_eq!(
            h.state().menu_row,
            Some(1),
            "{item}: the row the menu is for is marked"
        );
        h.get_by_label(item).click();
        h.step();
        assert_eq!(copied(&h), [expected], "{item}");
        h.run();
        assert!(h.query_by_label(item).is_none(), "{item}: the menu closes");
        assert_eq!(
            h.state().menu_row,
            None,
            "{item}: and the mark goes with it"
        );
        let what = if item == "Copy name" {
            "name"
        } else {
            "target"
        };
        h.get_by_label_contains(&format!("Copied the {what} of beta."));
    }
}

/// The palette copies them too, for whoever works from the keyboard: typed and run with Enter, or clicked.
#[test]
fn the_palette_copies_the_selected_servers_name_and_target() {
    for (entry, what, expected) in [
        ("Copy name of beta", "name", "beta"),
        ("Copy target of beta", "target", "npx something"),
    ] {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a, b]);
        click(&mut h, "beta");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
        *h.state_mut().command.query_mut() = entry.to_owned();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(!h.state().command.is_open());
        h.get_by_label_contains(&format!("Copied the {what} of beta."));

        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a, b]);
        click(&mut h, "beta");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
        h.event(egui::Event::Text("of beta".to_owned()));
        h.run();
        h.get_by_label(entry).click();
        let mut got = Vec::new();
        for _ in 0..4 {
            h.step();
            got.extend(copied(&h));
        }
        assert_eq!(got, [expected], "{entry}");
    }
}

/// Saying "Copied" never takes the place of a message that offers something, such as Undo.
#[test]
fn copying_keeps_a_message_that_offers_undo() {
    let (mut h, engine) = live_harness();
    let (a, b) = two_rows();
    show_rows(&mut h, &engine, vec![a, b]);
    let offer = Outcome {
        undo: Some(Change::Delete {
            id: h.state().snapshot.servers[0].id(),
        }),
        ..Outcome::done("Turned off alpha.".to_owned())
    };
    h.state_mut().outcome = Some(offer.clone());
    h.get_by_label("beta").click_secondary();
    h.run();
    h.get_by_label("Copy name").click();
    h.step();
    assert_eq!(copied(&h), ["beta"]);
    assert_eq!(
        h.state().outcome.as_ref().map(|o| &o.text),
        Some(&offer.text)
    );
}

/// Clicks the middle of `rect`: a single click on text, which puts egui's text caret there.
fn click_at(h: &mut Harness<'static, ShellApp>, rect: egui::Rect) {
    h.hover_at(rect.center());
    h.run();
    h.drag_at(rect.center());
    h.run();
    h.drop_at(rect.center());
    h.run();
}

fn copy(h: &mut Harness<'static, ShellApp>) -> String {
    h.event(egui::Event::Copy);
    h.step();
    copied(h).concat()
}

/// A click (or a selection) in the detail panel, then the list moves on with ↓ or ↑: the next server's panel shows
/// nothing selected, so Ctrl+C must copy nothing, not one of its texts unseen.
#[test]
fn moving_to_another_server_drops_the_text_selection() {
    for (key, start, other) in [
        (egui::Key::ArrowDown, "alpha", "beta"),
        (egui::Key::ArrowUp, "beta", "alpha"),
    ] {
        for drag in [false, true] {
            let (mut h, engine) = live_harness();
            let (a, b) = two_rows();
            show_rows(&mut h, &engine, vec![a, b]);
            click(&mut h, start);
            let target = text_rect(&h, "npx something");
            if drag {
                assert_eq!(drag_and_copy(&mut h, target), "npx something");
            } else {
                click_at(&mut h, target);
            }
            h.key_press(key);
            h.run();
            let want = usize::from(other == "beta");
            assert_eq!(
                h.state().selected.iter().copied().collect::<Vec<_>>(),
                [want],
                "{key:?} moved the list"
            );
            assert_eq!(
                copy(&mut h),
                "",
                "{key:?}, drag {drag}: nothing is selected"
            );
        }
    }
}

/// The same after going to another page and back.
#[test]
fn going_to_another_page_drops_the_text_selection() {
    let (mut h, engine) = live_harness();
    let (a, b) = two_rows();
    show_rows(&mut h, &engine, vec![a, b]);
    click(&mut h, "alpha");
    let target = text_rect(&h, "npx something");
    assert_eq!(drag_and_copy(&mut h, target), "npx something");
    h.state_mut().go(Page::Settings);
    h.run();
    h.state_mut().go(Page::Connections);
    h.run();
    assert_eq!(copy(&mut h), "");
}
