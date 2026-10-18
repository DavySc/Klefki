# Lighter dark text fields

Goal: the dark text fields (crust, the darkest surface) read as holes; make them pleasant in both dark themes.

## Done
- `theme.rs`: `DARK.field` `#11111B` -> `#282839`, `WARM_DARK.field` `#131211` -> `#262422`: one step above the panel, below raised surfaces. Light themes unchanged (white).
- Removed the `DARK.field == Mocha crust` spot check (it pinned the old value); added `dark_fields_sit_above_the_panel_not_below_it` (lighter than panel, darker than raised, contrast >= 1.1 with panel, outline >= 1.5 against the field).

## Checks
- `cargo clippy -p klefki --all-targets -- -D warnings`: exit 0.
- `cargo test -p klefki`: 209 passed, 0 failed.
- Not run: `cargo deny check`, real-window screenshot (user confirmed the look: "Much nicer").

## Next
None.
