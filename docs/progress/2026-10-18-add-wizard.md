# Add wizard: fill in, OAuth through Claude Code, select what was added (2026-10-18)
Goal: the rest of the M2 add wizard, on top of paste-to-add (#17). It adds a form for typing a server in part by part. Servers that sign in with OAuth are added through `claude mcp add-json --client-secret`, as CLAUDE.md requires. The new rows are selected once they are in the list.

Branch `claude/add-wizard-y3wsyn`, from main `3472538`. Commits: `5d17f79` (the slice), then the fixes for the first cross-review (the next commit; section "Review round 1").

## Done
- **What the real `claude mcp add-json` does** (2.1.302, in a temp `HOME`/`CLAUDE_CONFIG_DIR`). The form and checks follow this behaviour:
  - `oauth` takes `clientId`, `callbackPort` (an integer from 1 to 65535), `scopes` (one string) and `authServerMetadataUrl` (https).
  - Unknown `oauth` keys are dropped silently, `clientSecret` included.
  - `oauth` on stdio or ws servers is dropped.
  - `-s local` uses the cwd as the project key.
  - The secret goes to Claude Code's own `.credentials.json` or keychain.
  - `--client-secret` with no `MCP_CLIENT_SECRET` and no TTY silently adds the server *without* a secret. Klefki therefore always sets the variable, non-empty, whenever it passes the flag.
  - `--` before the positional arguments works, also for a name that starts with `-`.
  - A name that is already taken is refused ("MCP server X already exists in user config").
- **core `entry::validate_new`:** checks a new definition, which may carry `oauth` as above.
  - The type must be http or sse.
  - Client id: at most 512 characters, sanitised, one line.
  - Port, scopes and metadata URL are checked as listed above.
  - `clientSecret` is refused, because the secret never goes in the definition. Other keys are refused by name.
  - `validate`, used for every docstore write, still refuses `oauth`.
- **docstore `Purpose::Preview`:** `preview(ops)` builds a plan (diff and `base_hash`) that may contain an OAuth server, but no Preview plan is ever written. `still_applies(plan)` re-reads the file and re-runs the edit, so a name taken after review stops the add.
- **ops:** `Resolved` gained `cli: Vec<CliAdd>` and `preview`.
  - `add(..., secret)` sends OAuth servers to `cli`. Every other server keeps going through the docstore.
  - Refused:
    - a secret for a server that does not sign in with OAuth;
    - an empty secret;
    - an OAuth add for a project whose folder is missing (Claude Code adds a local server from inside that folder).
  - The review note says Claude Code adds the server with its own command and keeps the secret, and says to sign in from `/mcp`.
  - `describe` shows an OAuth line: client, port, scopes, metadata, or "registering itself".
- **engine:** `Change::Add` gained `secret: Option<Secret>`.
  - Prepare needs Claude Code's program and, when a secret is given, a working secret store, checked before review.
  - Apply calls `still_applies`, then runs `claude mcp add-json -s user|local [--client-secret] -- <name> <json>` with no shell. The env holds `MCP_CLIENT_SECRET` only when a secret is given. The cwd is the project folder (for local) or home, and the run has the check timeout.
  - After the run, the engine reloads and finds the row by scope and project, then stores Klefki's copy of the secret under the row's `ConnectionId`.
  - Each failure says what was and was not added: could not start, timed out, Claude Code's own reason, said it was added but is not there, or Klefki's copy could not be kept.
- **core `form.rs`** (`api::ServerFields`, `api::secret_reference`): the form's fields read into the same `PastedServer` the paste uses. That covers the type (Command/HTTP/SSE), the command line (read like a paste: no shell, `${VAR}` note), the address, env and header rows, and the OAuth settings.
  - It also reports what is missing or wrong.
  - `from_definition` fills the form from a pasted definition.
- **paste:**
  - `claude mcp add --client-id/--callback-port/--client-secret` and `add-json ... --client-secret` are read rather than refused. They show a "✎ Fill in" button that opens the form to finish them; the secret is never pasted.
  - A value after `--client-secret` is refused without echoing it.
  - stdio with OAuth is refused.
- **app** dialog:
  - "Paste" | "Fill in" tabs, remembered for the session.
  - Fill in has these fields:
    - Name (focused).
    - Type as radio buttons.
    - Command with an "e.g." hint.
    - Env or header rows in the Env tab's style: hidden values, a remove button, and "looks like a secret" with a **Use ${X} instead** button.
    - Address.
    - A "Signs in with OAuth" toggle, then optional client id, a hidden client secret, callback port and scopes.
  - A field that is still empty shows muted text; only a wrong value is red.
  - The form area has a fixed height and the summary line keeps its space, so the buttons do not move while typing.
  - After Add (and after a partly failed add), the new rows are selected. A hiding filter is cleared, and the grid scrolls to the row.

## Tests
- **core:**
  - `a_new_server_may_sign_in_with_oauth_as_claude_code_takes_it`
  - `a_server_that_signs_in_with_oauth_is_only_ever_previewed_never_written`
  - 6 form tests, including a round trip with special characters.
  - Engine:
    - `an_oauth_server_is_added_by_claude_code_with_its_secret_only_in_the_environment` (fake-claude logs argv and the secret's sha256: the secret is not in argv; it is in env).
    - `..._for_a_project_is_added_from_that_project_folder`
    - `..._without_a_client_secret_is_added_without_one`
    - `when_claude_code_refuses_the_add_its_reason_is_shown_and_no_secret_is_kept`
    - `claude_code_saying_it_added_a_server_that_is_not_there_is_reported`
    - `without_a_secret_store_an_oauth_server_with_a_secret_is_not_added`
    - `an_oauth_add_that_cannot_be_made_is_refused_before_anything_runs`
    - `an_oauth_server_is_not_added_without_claude_codes_command`
    - `a_name_taken_after_review_stops_the_oauth_add_before_it_runs`
- **app:**
  - `a_server_can_be_filled_in_part_by_part`
  - `an_oauth_server_is_added_with_its_client_secret_typed_hidden`
  - `an_empty_client_secret_is_none_and_a_secret_is_only_sent_for_oauth`
  - `the_form_says_what_to_fix_and_offers_a_reference_for_a_typed_secret`
  - `a_pasted_oauth_server_is_finished_in_the_form`
  - `the_dialog_opens_the_way_it_was_last_used`
  - `what_was_added_is_selected_once_it_is_in_the_list`
  - `nothing_is_selected_for_an_add_that_was_cancelled`
- **Mutation checks** (each reverted afterwards):
  - dropping `MCP_CLIENT_SECRET` from the env fails the secret test;
  - dropping `still_applies` fails the name-taken test;
  - dropping `select_added` fails the selection test.
- **Tests changed, for review:**
  - Engine refusal test: its OAuth case was an http server with `oauth`, which is now a valid add. It is now `{"command": "x", "oauth": {...}}` and still expects a refusal naming OAuth.
  - paste: the assertions that OAuth commands are refused were replaced by `a_server_that_signs_in_with_oauth_is_read_to_be_finished_in_the_form`. OAuth used to be refused only because no path existed yet.

## Checks (this branch, after the last UI change)
- `cargo fmt --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0, 409 core and 176 app tests passed.
- `cargo deny check`: exit 0 (advisories, bans, licenses and sources ok).
- `cargo clippy -p klefki -- -D warnings`: clean.
- No `std::fs`, `process::Command`, `std::net` or `open::` in `crates/app/src`.
- **Real window** (Xvfb, temp `HOME` with one user server and one project, real Claude CLI 2.1.302 on PATH):
  - Fill in: name `sso`, HTTP, `https://sso.example.com/mcp`, OAuth on, client id `klefki-test`.
  - The review showed the diff with `oauth.clientId`, "Signs in with OAuth as client klefki-test" and the add-json note.
  - Add ran the real CLI. `.claude.json` then held `sso` with `oauth: {clientId: "klefki-test"}`, and the row appeared selected with its details open. It shows Failed because the address is made up.

## For human review (invariants)
- "OAuth entries are written only via `claude mcp add-json --client-secret`": an OAuth server *without* a client secret (one that registers itself, or a public client) is added through `add-json` without `--client-secret`. Passing the flag with no secret would make Claude Code prompt, or silently add nothing. The docstore never writes `oauth` either way.
- Klefki keeps its own copy of the client secret in the OS keyring (keyed by `ConnectionId`) in addition to Claude Code's copy. This matches the existing Auth tab. When the store is unavailable, an add with a secret is refused before anything runs, so there is no plaintext fallback.

## Review round 1 (cross-review of `5d17f79`: 4 blocking, 9 should-fix, 5 nits)
Each fix below has a test, and every app test was seen failing with its fix undone, except B1 and B3, whose reproducers were seen failing before the fix.
- **B1:** a word right after `--client-secret` (or `--client-secret=…`) that is not an option, address or JSON is taken for a secret. The whole paste is then refused and nothing of it is shown. A name there is refused too, since it cannot be told from a secret.
  - Reproducers: secret before an address with no name (which made the secret the name); `add-json --client-secret S name`; `=` forms; the secret after the name; the secret at the end.
- **B2:** the Add dialog is kept, hidden, while its review is asked for.
  - When the review fails, or the add fails after review, the dialog comes back as it was, with the reason shown in it rather than in a toast.
  - The review has **← Back**, which returns to the form. Cancel drops the whole add.
- **B3:** on the add-json path, an env or header value that `secretlike` flags is refused before anything runs. The message names the variable or header and the reference to use, never the value. `Bearer ${X}` and plain values pass.
- **B4:**
  - The dialog's top is anchored, so the tabs never move.
  - The body (form or paste, plus places) is one scroll area with a solid, visible bar, sized to the window. The summary line and buttons stay below it.
  - Ticking OAuth scrolls its settings into view.
- **S1:** the form has a Metadata URL field (`authServerMetadataUrl`). A pasted server that asks for a secret but has settings the form cannot hold (e.g. `headersHelper`) is left out with a reason and no Fill in button.
- **S2:** a field's problem is said once the field has been left, or Review was pressed; never while it is first typed in. A message goes as soon as it is fixed. An address without a scheme gets `https://` when left. A missing scheme reads "Start the address with https://."
- **S3:** `ServerFields::problems()` returns every problem with its `Part`. Each is drawn under its field, in the form's order.
- **S4:** Review is always pressable. Pressed too soon, it says everything that stops it and focuses the first field that needs work.
- **S5:**
  - The toggle reads "Use OAuth client settings from the provider", with a help line.
  - Ticked with nothing filled in and no secret, no `oauth` is written, so the server goes through the docstore and no Node start is needed.
  - Settings left from an address are not used for a command (they were a hidden error).
- **S6:**
  - `PastedServer.asks_secret`: only a paste with `--client-secret` goes to the form. Other OAuth pastes are added as pasted.
  - The summary says "N need finishing in the form" separately from what is left out.
- **S7:**
  - Show/hide buttons on the client secret and on each value row.
  - On close, egui's memory of the secret, value and paste fields (undo steps) is removed (`forget_typed`). The undoer is not serialised by egui's persistence.
- **S8:** "Use ${X} instead" then says "Set X in the environment Claude Code starts from…" and offers **Undo**. The button sits in the field column.
- **S9:** `api::same_project` (the engine's `normalise_project`) is used by `select_added` and by `Places::taken`.
- **N1:** tabs on the left as framed buttons, at a fixed place. **N2:** labels are left-aligned in a column. **N3:** "SSE (older servers)".
- **N4:** the name follows the address or command until typed: `mcp.linear.app` gives `linear`; `@modelcontextprotocol/server-filesystem` gives `filesystem`. The first field is now the command or address.
- **N5:** the toast says "Added sso for the user. Sign in to it from Claude Code (/mcp)."
- **Code:**
  - `still_applies` compares changes only.
  - A failed `store.set` keeps the "Already added" suffix for earlier adds. This is a message change, with no test: the mock store cannot fail on `set` only.
- **Tests changed, for review:**
  - App tests that checked Review was disabled now check `ready()`, the same `changes()` that used to enable it. Tests that checked the dialog closed on Review now check that it waits behind the review.
  - `add_explains_what_it_cannot_add…` now uses a stdio `--client-id` line as its refused server, since an http one with a client id is now addable.
  - The form test's "OAuth on stdio is refused" case became "settings left from an address are not used".
  - In `an_address_with_headers_and_oauth…`, ticking OAuth with nothing filled in now writes no `oauth`.
- Real window (Xvfb 960×600, real CLI 2.1.302):
  - `mcp.linear.app/mcp` became `https://mcp.linear.app/mcp`, and the name became `linear`.
  - Ticking OAuth scrolled the settings into view. Show and hide worked on the secret.
  - Review with a secret failed here (no Secret Service). The failure was shown in the dialog with every field kept.
  - With the secret cleared: Review, then Back, then Review, then Add. `.claude.json` held `linear` with `oauth.clientId`, and the row was selected.
- Checks after the round: `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo clippy -p klefki -- -D warnings` 0; `cargo test --workspace` 412 core + 188 app passed; `cargo deny check` 0. No IO calls in `crates/app/src`.

## Open / known limits
- For OAuth servers, typed env and header values that do not look like secrets still travel in `add-json`'s argv, as Claude Code's own command takes them. Those that do are refused (B3).
- Not tried on Windows or macOS. In particular, the cwd-based project key for `-s local` on Windows is unverified.
- The Paste/Fill in choice is remembered for the session only.
- `ServerFields` is separate from the edit form's `Definition`; they could share code later.
- The plan's free-port check for the callback port is not done. Neither is sign-in itself (PTY login): the user signs in from Claude Code's `/mcp`.

## Next
Re-review of the round-1 fixes, then merge once nothing blocking is open and CI is green on all 3 OSes. After that: PTY sign-in and re-authorise (the callback-port spike), and the free-port check.
