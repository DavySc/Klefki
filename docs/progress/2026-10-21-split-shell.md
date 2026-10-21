# Split shell.rs into modules (simplification A1)

Goal: slice A1 of `docs/proposals/simplification-plan.md`. `crates/app/src/shell.rs` (about 20,000 lines, half of
them tests) becomes one module per dialog or page, one module per pull request, while no other open pull request
touches `shell.rs`. Pure moves and visibility changes: behaviour and look unchanged, no golden file or look picture
may change. `ShellApp` keeps the routing (`apply_event`, `show`, the commands).

## Layout
- The code of a dialog or page goes to `crates/app/src/shell/<name>.rs`, declared in `shell.rs`: its state, its
  free functions and an `impl ShellApp` block with its methods. What `shell.rs` still uses is `pub(super)`;
  imports are named, not globbed, so each module shows what it depends on.
- Its tests go to `crates/app/src/shell/tests/<name>.rs`, a child of `shell::tests` like `look.rs`, so they use
  the shared helpers (`harness`, `click`, `press`, `show_rows`, …) through `use super::*` without making each one
  `pub(super)`.
- Each move is done by a script that cuts whole items by name (doc comments and attributes included) and pastes
  them unchanged; the only edits after it are visibility, imports and `cargo fmt` joining a line that now fits.
  Checked per pull request: the removed and added lines are the same lines, apart from those.
- Before each push, besides CLAUDE.md's verify: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --document-private-items --workspace`, as CI runs it. A moved doc comment can link a name its new file does not
  import.

## Modules
1. Export dialog: `shell/export.rs` (state, `export_dialog`, `open_export`, `can_export`, `export_rows`, the
   file-name selection, the live-region helper) and `shell/tests/export.rs` (16 tests and their three helpers).
   `shell.rs` 19,982 to 18,724 lines.
   Merged in #73 (efee4bd).
2. Move, Fix casing and the places list: `shell/move_dialog.rs` (`MoveForm`, `open_move`, `move_dialog`),
   `shell/casing.rs` (`CasingForm`, `open_casing`, `casing_dialog`), `shell/places.rs` (`Places`, `Taken`, the tick
   and radio rows Add, Import, Duplicate and Move share), and the tests of the move and fix casing sections.
   `casing_harness` and `open_move_of` stay in `shell::tests`: the add and edit-form sections use them too.
   `shell.rs` 18,724 to 17,690 lines. Review: `Taken` moved to right after `impl Places`. Merged in #74 (aaef3d4).
   A Fix casing bug the reviewer found (both spellings in one place: the dialog offers a rename that is then refused)
   is older than A1 and left for a follow-up.
3. Duplicate: `shell/duplicate.rs` (`DuplicateForm`, `CopyRead`, `copy_fields`, `COPY_LABEL_WIDTH`, `duplicable`,
   `open_duplicate`, `duplicate_dialog`) and the duplicate section's tests. `duplicate_harness` and
   `duplicate_requested` stay in `shell::tests`: other sections use them. `field_row`, `field_note` and
   `field_warning` stay in `shell.rs` until Add moves. `shell.rs` 17,690 to 16,839 lines. CI's `cargo doc` failed
   on a moved comment linking `Event`, which the new file does not import: the link now names its full path. Review:
   the types now come before `copy_fields`.

Next, one pull request each: Add and Import, the review and outcome, the detail panel and editor, the server list,
the top bar, toolbar and Settings.

## Checks
See each pull request description.
