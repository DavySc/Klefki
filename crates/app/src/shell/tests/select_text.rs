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
        h.get_by_label(item).click();
        h.step();
        assert_eq!(copied(&h), [expected], "{item}");
        h.run();
        assert!(h.query_by_label(item).is_none(), "{item}: the menu closes");
    }
}
