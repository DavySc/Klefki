# Safe edits, move and copy (2026-10-18)
M2 slice: rollback-safe remove-then-add so servers that sign in with OAuth can be edited, moved and copied, plus Move
between the user and local scope. Acceptance: PLAN's model-based test. Design and spike findings: decision 006.

- **Core:** `swap.rs` (new). A swap removes and adds one server through Claude Code's `mcp remove` and
  `mcp add-json --client-secret` (secret only in `MCP_CLIENT_SECRET`), behind the `swap::Claude` trait (`Program` over
  `runner::run`). The old definition is saved to a private recovery file first; a failed add puts it back with the
  same secret; a failed put-back keeps the file, listed in `Snapshot::kept`. The file decides every outcome (re-read
  after each command). Plan-time `review` refuses taking another server's secret, warns about signing in again, and
  runs nothing for an unchanged OAuth edit.
- **Engine/ops:** `Change::Move { id, to }`, `Change::PutBack { kept }`, `Change::ForgetKept { kept }` (destructive).
  `Event::Applied` carries `notes` (settings Claude Code left out). Edit and Duplicate of an OAuth server become swaps.
  A non-OAuth move is one docstore plan (`CopyServer` + `RemoveServer`). Before Claude Code runs, klefki's pending
  journal for `.claude.json` is replayed and retired (`settle`). `add_with_claude` uses the same trait.
- **Docstore:** OAuth refusals stay, except for `Purpose::Preview`; `keep_copy` takes a private copy before a swap.
  Bug fixed: the journal's verify of an edit whose server was deleted afterwards failed the plan
  (`gone_is_done` now covers `ReplaceServer`); reproducer and variant.
- **App:** Move button (arrow icon) and palette entry; Move dialog with radio places, where it is now disabled and
  tagged, a taken name refused, an OAuth note. After a move or copy the result is selected and the message says
  "Moved sso to webapp." / "Copied …", with Sign in offered for OAuth. Applied notes are shown. A notice lists kept
  servers with Put back and Forget…. The OAuth edit form is no longer blocked; it says you sign in again.
- **Found trying it for real** (`cargo run -p klefki` under Xvfb, temp `HOME` and `CLAUDE_CONFIG_DIR`, real Claude
  Code 2.1.302): the Move icon (U+E0A0) drew JetBrains Mono's branch symbol, so the toolbar is now a const checked by
  `every_toolbar_and_nav_icon_is_drawn_by_the_icon_font` (failed with the old icon); a move warned that the server
  "is used instead of" itself in the target project (`moving_a_server_does_not_warn_about_itself`, failed before the
  fix, plus a variant that still warns about another server). The plain move then worked: the file kept the unknown
  key and the rest, a backup was made, the row was selected in its new place. The OAuth move was refused before
  anything ran: there is no Secret Service in the container. The same OAuth move through the engine with real
  Claude Code and a test store (a one-off test, not kept): Applied, the entry in the project, Claude's client secret
  under its scope-free key, Klefki's copy moved to the local id, no recovery file left.
- **Tests:** model test `no_change_ever_loses_a_server_or_its_secret` (256 cases by default, ~7 s; a 4000-case soak
  passed) and 3 explicit machine tests; engine via fake-claude: OAuth move (argv, cwd, secret only in env, Klefki's
  copy moved), remove that leaves it there, remove that hangs; moves without self-warnings; docstore 2; app 12 (move
  dialog, palette, taken name, selection and messages, OAuth note and Sign in, kept notice, icons). Mutations of the
  rollback (no put back, no recovery file, no secret moved, …) are caught by the model test.
- **Changed tests (behaviour changed on purpose):** `definition.rs` OAuth definitions now have no problem (they are
  saved through Claude Code); the app test that OAuth servers are not edited in the form is split into one for
  project servers (unchanged) and one that an OAuth server is edited and says it signs in again.
- Checks (after merging main with the process-tree kill, #24): see the PR; last run `cargo fmt --check` 0, clippy
  workspace and `-p klefki` 0, `cargo test --workspace` 460 core + 221 app passed, `cargo deny check` 0, no IO in
  `crates/app/src`.
- **Open / for human review:** OAuth servers are now removed through `claude mcp remove` (invariant area). A move or
  edit of an OAuth server signs it out (Claude deletes its credentials). Proposals in decision 006: letting the
  docstore remove OAuth entries (keeps the sign-in), and a fake-claude mode that writes the config. The OAuth move
  was not tried in the real app (no keyring here).
- **Next:** cross-review; merge after the free-port check lands (merge order set by the coordinator). Not merged by
  this session: the merge is left for Davy.
