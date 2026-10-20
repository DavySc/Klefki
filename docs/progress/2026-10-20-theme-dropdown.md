# Theme dropdown lists every theme

Goal: the top-bar theme dropdown shows every theme that can be set, each with a sun (light) or moon (dark) icon.

## Done
- `theme::ThemeOption`: System, Dark/Light x Cool/Warm greys (five entries). Warm greys were only reachable in Settings.
- Dropdown uses it; picking an entry sets mode and greys. System keeps the greys, shows the desktop icon (it is neither sun nor moon). Popup height raised so all five show without a scroll bar at 800x480.
- Labels: "Dark (cool)", "Light (cool)", "Dark (warm)", "Light (warm)". Icon plus text, never icon alone. Icons are the bundled Phosphor ones.
- Tests: `dropdown_lists_every_theme_with_sun_or_moon`, `the_current_theme_maps_to_its_entry` (theme.rs); the dropdown shell tests now walk all five entries.
- Screenshots at 800x480 (dark and light): `/mnt/project-files/reviews/2026-10-20-theme-selector/`.

## Checks
See the pull request description.

## Next
Cross-review; Settings > Appearance still has the separate System/Light/Dark and Greys rows (left as is).
