# App test helpers and names for icon buttons (simplification B1)

Goal: slice B1 of `docs/proposals/simplification-plan.md`. The app tests find the toolbar by its icon glyphs and
repeat the same set-up lines; give the icon-only controls accessible names (WCAG 2.2 SC 4.1.2) and let the tests
find them by those names through a few shared helpers, so A1 (splitting `shell.rs`) moves shorter tests. Behaviour
and look unchanged.

## Done
- Toolbar: the `ACTIONS` table of (glyph, tip) became `enum Tool` with `glyph`, `name` and `tip`; the toolbar
  matches on it instead of comparing glyph strings. Same order, same enabling rules, same hover texts.
- Accessible names, set on the accessibility node only (`name_for_screen_readers`), nothing drawn changes:
  - Toolbar: "Add server", "Import from a file", "Edit server", "Duplicate server", "Move server",
    "Sign in to server", "Turn on servers", "Turn off servers", "Delete servers", "Export servers". Not the bare
    verb: dialogs have buttons called "Sign in", "Turn off", "Delete", and the two must not read the same.
  - Top bar: Refresh "Refresh and re-check all"; the theme menu "Theme", its value the theme in words
    ("Dark (cool)") instead of icon plus words; the palette button "Commands (Ctrl+K)" (contains its visible text,
    WCAG 2.5.3); the filter box by what it filters ("Filter servers", "Filter changes", "Filter secrets",
    "Filter profiles"), its hint left as the placeholder.
- New tests: `every_control_is_named_in_words` (no button, combo box, text box, check box or radio button on any
  page has a name without a letter; on main it listed exactly the 13 controls above),
  `the_top_bar_and_the_toolbar_are_named_by_what_they_do`.
- Test helpers in `shell.rs`'s tests: `tool(&h, Tool::X)`, `press(&mut h, Tool::X)`, `click(&mut h, "label")`,
  `show_rows(&mut h, &engine, rows)`. Replaced every toolbar glyph lookup (about 80, loops included), 308 click-then-run pairs and 72
  snapshot-then-run pairs. `shell.rs` is 296 lines shorter.
- Test queries changed with the names, same strength: the theme tests find the closed menu by its value in words;
  the sign-in test's "not offered in the detail panel" check skips the toolbar's "Sign in to server", which is
  always there; `the_toolbar_shows_only_what_works` checks the retired glyphs are not in `Tool::ALL`.

## Review round 1 (folded in)
- "Turn servers on/off" renamed "Turn on/off servers": the name starts with the tooltip's verb (WCAG Understanding
  2.5.3, APG "most distinguishing words first"; speech input matches "Turn off").
- Each toolbar button's hover text, why it is off included ("Edit (Ctrl+E): select one server"), is also its
  AccessKit description, so a screen reader says it without hovering.
- Mutants on the changed lines: 12 caught, 7 unviable, 2 missed, both `Tool::tip` (no test read the hover texts;
  on main they were a constant table, which cargo-mutants does not mutate). New test
  `each_toolbar_button_says_on_hover_what_it_does_and_its_shortcut` spells out every hover text and checks the
  description.

## Checks
See the pull request description.

## Next
Cross-review, merge. Then A1 can split `shell.rs` with these helpers.
