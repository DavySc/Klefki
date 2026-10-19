# Detail panel scrolls (2026-10-18)
Found in the whole-app UI review of main (`e169d1b`). The detail panel had no scroll area, so whatever was taller than the window was drawn below it and could not be reached. At the size the app opens at (1200x720) this hid the Diagnose result and its Copy report button (Diagnose looked like it did nothing), and the edit form's "Review and save…" for a server with a few arguments. The app tests run at 1100x900, which is why none saw it.

## First version (`66a5383`)
- `shell.rs` `show()`: the detail panel's content in a vertical `ScrollArea`, with the right margin moved inside it, where egui's floating scroll bar shows, so the bar never covers a button at the panel's edge (it took clicks on the right of an argument's Remove button).
- Tests, each seen failing first against main: a long diagnosis scrolls to Copy report, a long form scrolls to its end, another server's details start at the top, and the scroll bar leaves the buttons beside it clickable.

## Review round 1 (cross-review by "Review core for correctness", PR comment 6256751942)
- **Place kept per server** (B1): the scroll place was keyed by the row's position, and rows are kept by position, so a server added or removed above moved it. Now keyed by the server's identity (`ConnectionId`) and tab.
- **Keyboard focus is brought into view** (B2): egui does not scroll to a widget that gains focus, so Tab went below the window. `bring_focus_into_view` scrolls the panel to a widget inside it that just gained focus, and only inside it (focus in the search field leaves the panel where it is).
- **Name and tabs stay** (S2): the panel draws them above the scroll area. The tab bodies moved into `detail_tab` unchanged, so the diff is not a re-indent and merges with #26.
- **Save bar stays** (S1): it is drawn under the scroll area while a definition is open on General or Env, with the reason saving is blocked. It is drawn after the form, so Tab reaches it last; the scroll area leaves it the height it took last frame, and a change in that height is laid out again before the frame shows (`request_discard`). The forms ask for it (`save_bar_wanted`) instead of drawing it, so it shows exactly where it did before; after #26, OAuth definitions get it too. Its buttons and "Edit as JSON" share one line, which wraps in a narrow panel.
- **Status actions first, and a diagnosis brought into view** (S3): Check now and Diagnose come before the status explanation. A diagnosis for the server shown on its Status tab scrolls to its top when it arrives; one for another server does not move anything later.
- **Both sizes** (S4): `OPENING_SIZE` and `SMALLEST_SIZE` live in `shell.rs`, used by `main.rs` and the tests, which run at both.
- Nits: `ScrollArea::content_margin` replaces the inner frame (N1); the room kept for the scroll bar comes from the scroll style, `bar_width + bar_outer_margin`, not a copy of egui's 10 (N2). `sentence()` replaces two copies of "capitalise the first letter".
- Not done: PageUp, PageDown, Home and End scrolling the panel (N3). The list will want them for paging too (4,000 rows), and which of the two gets them is a product call; noted for Davy.

### Tests
Seen failing first against `936f156`, with what they measured:
- `a_server_added_above_leaves_the_details_where_they_were` and its variant `a_server_removed_above_…`: Copy report at y=672, then y=1878.
- `keyboard_focus_brings_what_it_reaches_into_view`: "Add argument" focused at y=1150 in a 720 window.
- `the_name_and_tabs_stay_in_view_while_the_details_scroll`: the General tab at y=-532.
- `the_save_bar_stays_in_view_while_a_long_form_scrolls` and `why_saving_is_blocked_shows_beside_the_save_button`: both at y=1188.
- `the_status_actions_are_in_view_without_scrolling`: Check now at y=974.
- `a_diagnosis_that_arrives_is_brought_into_view`: its top at y=1042.
- `a_diagnosis_of_a_server_not_shown_waits_where_it_is`: Diagnose at y=974, below the long explanation.
- `the_detail_panels_scroll_bar_leaves_the_buttons_beside_it_clickable`, new pass with a 16 px bar: the click landed on the bar.

Guards that passed before and were checked by mutation:
- `focus_elsewhere_leaves_the_details_where_they_are` fails without the "inside the panel" check.
- `a_diagnosis_of_a_server_not_shown_waits_where_it_is` fails when every diagnosis is revealed (Diagnose at y=-644).

`tab_from_the_last_field_goes_on_to_the_save_bar` passes before and after; it keeps the save bar after the form in Tab order. The earlier reachability tests now run at both sizes. The tests measure what can be seen by the panel's scroll bar, whose accessibility node spans the scroll area, not by the window.

### Checks
- Before opening: full verify exit 0.
- After main (#27) was merged in (`1e61562`): full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`) exit 0, 704 tests passed, deny "advisories ok, bans ok, licenses ok, sources ok".
- Round 1: full verify exit 0, 715 tests passed, deny "advisories ok, bans ok, licenses ok, sources ok". `cargo test -p klefki` 225 passed.
- Trial merge into #26's head `349e33d`: no conflicts. The merged tree has one `save_bar` call, and `cargo test -p klefki` passes there (240).
- Real app (release build, Xvfb, a server that fails with ENOENT):
  - At 1200x720 the actions sit under the status. Diagnose brings "What probably went wrong" into view, with the name and tabs still at the top and Copy report on screen.
  - Edit definition shows the save bar under the form.
  - At 800x480 the save bar stays in view under the fields.
  - At the default panel width "Edit as JSON" wraps under the buttons, so the bar is two lines; noted in the findings for Davy.
- CI: GitHub Actions has not started jobs since about 23:55Z (no runner, no logs, on every PR), so CI on this round waits for that.

## Review round 2 (PR comment 6257122294)
Round 1 checked out; one new blocker, B3, from the pinned save bar at the smallest size.
- **The definition asked for comes into view** (B3): at 800x480 "Edit definition" sat at the bottom, so its form arrived below the summary, under the save bar, with none of its fields in view. Edit (the button, Ctrl+E, the palette) now marks the editor `reveal`, and the form is scrolled into view when the definition arrives. It scrolls no further than that takes (`scroll_to_me(None)`): a form that fits leaves the summary above it where there is room, and a taller one comes in from its top. The save bar that comes with the form shortens the view only in the next pass, so the reveal is repeated until a pass that is shown (`will_discard`); done once in the first pass, it left the form's end under the bar. A definition that arrives while another server or tab is shown does not move anything later.
- **No switch under the pointer** (B3): the save bar wrapped "Edit as JSON" onto a second line, which came in right under the pointer that had clicked "Edit definition", so a double click opened the JSON. The way to the JSON and back is now under the form's last field (`form_end`), as it was before round 1, and the save bar is one line: Review and save… and Revert, with what stops saving above them. This also gives the form one line more at 800x480 (the view runs to y=410, not 366), and needs no second "can this be edited" rule.
- `tab_from_the_last_field_goes_on_to_the_save_bar` now walks Add argument, then Edit as JSON, then Review and save….

### Tests
Seen failing first against `b023c4b`:
- `the_definition_asked_for_is_brought_into_view`, at both sizes, by Edit definition on General, Ctrl+E from Status, and Edit definition on Env: at 1200x720 the last argument at y=610..632 under a bar from y=606; at 800x480 the Command field at y=492, below a view ending at y=366. With a 14-argument definition, its first field must be in view (it was at y=492 in a view ending at 410 without the reveal). Finishing the reveal in the first pass instead of the shown one fails it too (the form's end at y=666..700 in a view ending at 650).
- `a_second_click_where_edit_definition_was_leaves_the_form_a_form`: at 800x480 the second click at (534, 462) opened the JSON.

Guard, passing before and checked by mutation: `a_definition_that_arrives_while_another_server_is_shown_waits_where_it_is` fails when every arriving definition is revealed (Scope at y=-81).

### Checks
- Full verify exit 0, 718 tests passed, deny "advisories ok, bans ok, licenses ok, sources ok". `cargo test -p klefki` 228 passed.
- Trial merge into #26's head `349e33d`: no conflicts; `cargo clippy -p klefki --all-targets -- -D warnings` 0 and `cargo test -p klefki` 243 passed on the result.
- Real app (release, Xvfb): at 800x480 Edit definition shows the Definition heading under the tabs, Type, Command and both arguments, over a one-line save bar; a second click at the same place lands on the disabled Review and save…. At 1200x720 the panel moves up 72 px, so the summary from Type down stays and the form ends with Edit as JSON above the bar. Ctrl+E at 800x480 with a long target in the summary brings the form in from its top.
