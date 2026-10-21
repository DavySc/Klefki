# Split engine.rs into modules (simplification A2)

Goal: slice A2 of `docs/proposals/simplification-plan.md`. `crates/core/src/engine.rs` held 3,540 lines of code and
8,500 of tests. Split it into focused modules, the tests next to what they test. Behaviour and look unchanged: pure
moves, no golden file or look picture may change.

## Done
- Product code, each item moved whole and in its old order:
  - `engine.rs` (870 lines): what the UI and the engine exchange (`Command`, `Event`, `Change`, `PlanView`, `Hint`,
    `Diagnosis`, …), `Core`, `EngineConfig`, `start_with`, and the state the parts share (`Shared`, `Emitter`,
    `Input`, `Changes`, `ProgramStamp`, `State`, `lock`, `backup_dir`, `hint`).
  - `engine/command_loop.rs`: the engine thread (`engine_main`), reading the configuration again (`State::reload`,
    `refresh`, `reload_after_change`) and queueing the checks that calls for; `inspect`, `download_update`.
  - `engine/checks.rs`: the check queue with its locking (`Queue`, `Job`, `BackgroundSlot`, `Announced`), the
    workers, `claude mcp get` and `claude mcp list`, the CLI's version.
  - `engine/changes.rs`: `prepare`, `apply`, the changes made through Claude Code, the after-save check (`Journal`,
    `check_later`).
  - `engine/diagnose.rs`: `run_diagnose`.
  - `engine/client_secrets.rs`: one server's client secret and the Secrets page's list.
  - `sign_in.rs` and `machine.rs` stay; only their imports changed.
- Tests: `engine/tests.rs` holds the shared helpers (`Env` and the helpers the golden tests or several files use)
  and the tests of `Core` itself; `engine/tests/<topic>.rs` the rest, by topic (checks, diagnose, changes, updates,
  client_secrets, approvals, reload, edits, export, profiles, renames, spelling, oauth, sign_in, restore).
  `tests/golden.rs` and its files did not move. A helper used by one file only moved with it.
- Changes that are not moves: `pub(super)` on the items and fields one module uses from another; `use` lists;
  fixture paths in `include_bytes!` one or two `../` deeper; two doc links in `checks.rs` given a `super::` path; a
  module doc for each new file and four lines in `engine.rs`'s naming the parts; section comments that headed a whole
  topic dropped (the file's doc says it). rustfmt removed the braces around three one-expression closures in tests
  that now have 4 more columns.
- How the move was checked: a script cut the old file into its 430 items (each with its docs and attributes) and
  compared each, whitespace, `pub(super)` and trailing commas aside, with the items of the new files: 430 and 430,
  the only differences the two doc links and the three closures above.

## Review (#75, approved, no blocking findings)
Non-blocking, each more than a move, so for a later slice:
- `changes.rs` takes `kept_list`, `profiles_changed` and `reload_after_change` from `command_loop.rs`, which takes
  `prepare` and `apply` from `changes.rs`: the reload helpers might sit better in a `reload.rs` or in `engine.rs`.
- `claude_program` is in `checks.rs`, but `changes`, `diagnose` and `sign_in` use it too: perhaps `engine.rs`.
- A `Plans::forget(id)` would let `Plans`' fields and `Stored` be private again.

## Checks
See the pull request description. After merging main (8f8b88f, B2): fmt, clippy `-D warnings`, `cargo test
--workspace` (app 440, core 965, cli 25 + 43) and `cargo deny check`, all exit 0; no golden file or picture changed.

## Not run
- Mutants: a pure move (docs/quality.md: rely on the unchanged tests and golden files).

## Next
Merge after review; then A1 (shell.rs) module by module, A4 file by file.
