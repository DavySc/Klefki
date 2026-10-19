# Save dialog owner window (2026-10-19)

## Goal
The Export "Save to file…" dialog belongs to Klefki's window. Before, it had no owner: on Windows one click on
Klefki sent it behind, so the app looked stuck, and on Linux the desktop portal could not place it over Klefki.

## Done
- Core (`export_file.rs`): `Owner` (any `HasWindowHandle + HasDisplayHandle`), `OpenDialog` (opens and returns the
  answer as a future), `ShownDialog` (an open dialog with its opening time, answer taken once) and `answered`
  (the 150 ms "never shown" rule, now timed from the moment the dialog opened).
- `Core::choose_export_file(name, owner)` opens the dialog on the calling thread, where the window lives, and sends
  `Command::ChooseExportFile { name, shown }`. The engine waits for the answer on a thread of its own
  (`pollster::block_on`). With `shown: None` the engine opens no dialog (AppKit is main-thread only) and answers
  `NoDialog`, so a path is typed instead (cross-review round 1).
- `api::open_save_dialog`: rfd's `AsyncFileDialog` with `set_parent(owner)`. On macOS the sheet starts on the main
  thread; on Windows the common dialog runs on rfd's thread with Klefki's window as owner; on Linux the portal gets
  `x11:<xid>` or, with the core's `wayland` feature, an exported Wayland handle. No `unsafe` in Klefki.
- App: Save to file… records the file name; `App::ui` opens the dialog after drawing with `eframe::Frame` as owner.
- `wayland` feature of klefki-core (rfd's Wayland support, `wayland-backend` with `dlopen`): only the app turns it
  on, so klefki-cli neither links nor needs libwayland (checked: `cargo build -p klefki-cli` with rfd's Wayland
  support on unconditionally fails without the wayland-client dev files; with the feature it builds).

## Checks
- Tests written first: `export_file::a_shown_dialog_is_opened_once_where_asked_and_its_answer_taken_once`,
  `engine::the_save_dialog_opens_over_the_window_where_it_lives_and_its_answer_is_awaited_elsewhere`,
  `shell::the_save_dialog_opens_over_klefkis_window_from_the_ui_thread` (seen failing with `None` as owner).
- Full verify exit 0: fmt, clippy `-D warnings`, tests (core 648, app 315, cli 18 + 24, on main 5fc9dc1), `cargo deny` ok.
- Real app under Xvfb (X11, no portal, no zenity): Save to file… with a real X11 owner falls back to the path field
  as before.
- Linux release size: app 17,903,424 (PR #26 head) → 17,964,864 bytes (+61 KB: +10 KB the owned async dialog,
  +51 KB Wayland). klefki-cli 6,006,784 → 6,016,000 (+9 KB). Nothing runs at start-up.

## Not verified here
- Linux without a portal: rfd's zenity fallback ignores the owner, so that dialog still has none.
- Windows: an owner on another thread shares its input queue with the dialog's thread (AttachThreadInput), so a
  long Klefki frame could make the dialog feel slow. Worth a look on a real machine.
- Windows: the dialog stays above Klefki and Klefki is disabled while it is open.
- macOS: the dialog is Klefki's sheet.
- Linux desktop: GNOME/KDE portal places the dialog over Klefki on X11 and Wayland.

## Cross-review
Round 1 (Export save thread): no objection; folded in: `shown: None` opens no dialog off the UI thread.

## Next
Merge after #25 and #30, merging main in first.
