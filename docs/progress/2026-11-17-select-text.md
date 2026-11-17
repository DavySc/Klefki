# Select and copy text anywhere (2026-11-17)

Goal: any text Klefki shows (detail panel, pages, dialogs, messages, diagnoses, logs, tooltips) can be selected with
the mouse and copied with Ctrl+C / ⌘C, without an edit box. Places where a click picks something keep working as
before.

## Done
- `theme::style`: `interaction.selectable_labels` on (it was turned off on purpose, because selectable text in the
  list's rows took the click meant for the row). egui's labels then select by drag, double-click selects a word,
  selection runs across labels, and Ctrl+C copies; labels do not take Tab focus (egui removes `FOCUSABLE` from them).
- `theme::unselectable(ui, add)`: draws `add` with selection off. Used for:
  - the Connections list (`list.rs` grid): a row stays one click (and Ctrl+click) target. A right click on a row
    offers **Copy name** and **Copy target**; everything else about the server is selectable in the detail panel.
  - links (`theme::link`, used by Diagnose in a project's outcome, Add's "import it as …" and Export's "Type a path
    instead"): egui 0.36 gives a selectable link the accessibility role Label, so screen readers stopped calling them
    links (caught by `diagnose_in::the_outcome_is_said_by_the_button`).
- Last starts: each start's line opens and closes on a click, so its text is not selectable (`.selectable(false)`);
  what the line shows when opened is.
- Everything drawn with the painter (side menu, command palette, place rows in dialogs, group headers) is a control
  and was never a label; unchanged.

## Look
- `page-profiles.png` changed: the "New profile" button is drawn in its normal (raised) fill. On main the picture
  was taken one frame too early, while the button was still drawn flat: main with one more `h.run()` before the
  shot draws exactly the new picture (checked by putting the new picture in place on main and running the test with
  that extra frame: pass). Selectable labels ask egui for one more frame, so the test now reaches the settled frame.
  No other picture changed.

## Tests
- `shell/tests/select_text.rs` (new): a status message in the detail panel and a sentence on the Settings page are
  selected by dragging and copied in full; dragging over a row's text copies nothing and a click still picks the row;
  right-click Copy name / Copy target copy the row's values and close the menu.
- `shell/tests/last_starts.rs`: `a_lines_text_is_not_selected_but_what_it_shows_is`.
- `add.rs`, `export.rs`: their links are still read out as links (role asserted).
- Seen failing: with `selectable_labels` left off, the two selection tests copy nothing; with the list not wrapped
  in `unselectable`, the row tests fail (the click selects text, not the row); with the Last starts line selectable,
  its test copies the line; the link role assertions fail without `theme::link`.

## Checks
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`: pass. `cargo test --workspace`:
  1845 passed, 0 failed (look tests included, Mesa 25.2.8 as CI).
- `cargo deny check`: exit 1, `advisories FAILED` on a yanked `async-executor` in Cargo.lock; this branch does not
  touch Cargo.lock, so main has the same failure. Bans, licenses, sources ok. Left for its own slice.

## After the cross-review (review on f0ff129)
- B1 (blocking, fixed): a click or selection in the detail panel, then ↓/↑ in the list, left egui's selection on
  the next server's label (same place, same id; the arrow also moved its caret, so the range was empty) and Ctrl+C
  copied that label whole, unseen. `ShellApp::drop_stale_text_selection` clears egui's `LabelSelectionState` when
  the page, the servers picked or the detail tab change. Tests: `moving_to_another_server_drops_the_text_selection`
  (↓ and ↑, after a click and after a drag; failed before: copied "npx something"),
  `going_to_another_page_drops_the_text_selection`.
- Polish folded in: the right-clicked row is drawn hovered while its menu is open (`menu_row`); Copy name / Copy
  target say "Copied the name of beta." on the outcome message, unless that message offers something (Undo, sign
  in) or is a problem (`copying_keeps_a_message_that_offers_undo`, seen failing with the guard removed); the
  palette has "Copy name of X" and "Copy target of X" for the selected server, after the debug report and Inspector
  entries so those stay in the first rows.
- Left as follow-ups: icon glyphs (Private Use Area) end up in text copied from labels that start with an icon
  (banner, notes, outcome titles); drawing them as separate labels moves text by the item spacing, so every look
  picture with one changes. Copied key/value rows read "Project-" (egui joins labels without a space before
  punctuation); a "Copy details" action would give clean lines.
- Test gotcha: setting the palette's query with `command.query_mut()` and then clicking an entry closes the palette
  without running it in kittest; typing with `Event::Text` works (as on main). Enter works either way.
- Checks after merging main (0e221bb): fmt, clippy (exit 0), `cargo test --workspace` 1849 passed, 0 failed,
  `cargo deny check` advisories, bans, licenses, sources ok.

## Not done / next
- Ctrl+A selects all of one label (egui's behaviour), not a whole page or dialog.
- No right-click Copy on selected text (egui has no such menu for labels); Ctrl+C and ⌘C work.
- Tooltips hold selectable text, so egui keeps a tooltip open while the pointer is on it (as WCAG 1.4.13 asks).
- CI on Windows: clippy 1.100 (`stable` moved) flags `needless_bool` in `preflight.rs` `is_program`, where the
  `#[cfg(unix)]` block disappears; main has the same failure. Rewritten to end in `meta.is_file()`, same behaviour. Main fixed it too in #113; after merging main this branch
  keeps main's version, so the PR no longer touches `preflight.rs`.
