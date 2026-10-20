# Theme dropdown lists every theme

Goal: the top-bar theme dropdown shows every theme that can be set, each with a sun (light) or moon (dark) icon.

## Done
- `theme::ThemeOption` (mode, greys): six entries in two groups split by a separator, System/Light/Dark (cool) then (warm), as in Settings. Warm greys were only reachable in Settings; System + warm is now one pick.
- Dropdown uses it; picking an entry sets mode and greys. System keeps the greys, shows the desktop icon (it is neither sun nor moon). Popup height raised so all six show without a scroll bar at 800x480.
- Labels: "System (cool)", "Light (cool)", "Dark (cool)", "System (warm)", "Light (warm)", "Dark (warm)". Icon plus text, never icon alone. Icons are the bundled Phosphor ones.
- Tests: `dropdown_lists_every_theme_with_sun_or_moon`, `each_tone_is_one_group_in_the_order_of_settings` (theme.rs); `only_the_current_theme_has_a_check_mark` and the dropdown shell tests walk all six entries.
- Screenshots at 800x480 (dark and light): `/mnt/project-files/reviews/2026-10-20-theme-selector/`.

## Checks
See the pull request description.

## Next
Cross-review; Settings > Appearance still has the separate System/Light/Dark and Greys rows (left as is).

## Review round 1 (folded in)
- Six entries instead of five (System shows its greys); pick closes the list; check mark on the current entry; pink focus outline on the focused entry. Left out as optional: accessible name, word consistency with Settings, returning focus to the dropdown after a keyboard pick.
