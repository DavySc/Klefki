# Settings names Claude Code's version (2026-10-19)
Follow-up 4 of four left by the UI-fix pull requests: #10 shows the top bar's "Claude CLI x" only where it fits, so below about 900 px wide the version showed nowhere in the app.
- `shell.rs` `settings`: a "Programs" section with a "Claude Code" row: the version; "Not found" (warning colour) with the reason under it while Claude Code cannot be found; "Not known yet" before the engine has said. `shown_cli_version` is shared with the top bar, which keeps its rule of not naming a version while Claude Code cannot be found.
- Tests (seen failing first, no "Claude Code" row): `settings_names_the_claude_code_version_at_any_width` (800x480, 900x600, 1200x720), `settings_says_when_claude_code_is_not_found_or_not_known_yet`.
- Full verify: exit 0, 841 passed, 0 failed, deny ok.
- Real app (release, Xvfb 800x480, temp HOME and CLAUDE_CONFIG_DIR): Settings shows "Claude Code 2.1.303" on one line; started with `claude` off PATH, it shows "Not found" and "Live status is unavailable: the Claude CLI was not found."
- Moved onto main `3549d87` (#12 to #17 merged): full verify exit 0, 928 passed, 0 failed, deny "advisories ok, bans ok, licenses ok, sources ok". Real app at 800x480 on it: Settings shows "Programs / Claude Code 2.1.303".
- Next: none of the four follow-ups left once this merges.
