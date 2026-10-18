# CLI list and status (2026-10-18)
Goal: `klefki-cli list` and `klefki-cli status` on top of core (PLAN.md: optional headless `klefki-cli`). `export` is left out: its format is open.

Done:
- Output, options and exit codes decided without the owner (overnight): `docs/decisions/006-cli-output.md`. Human table grouped like the app's grid, `--json` for scripts, progress only on a terminal's stderr, exit 0/1/2.
- `crates/cli`: `args.rs` (hand-written parser, no clap: two commands, two options), `render.rs` (pure table and JSON rendering), `check.rs` (drives core's engine: `Command::Check` per server, waits for the final `Event::Status`, deadline 35 s per round of 3), `main.rs`.
- `list` reads the configuration only (`api::load_snapshot`). `status` checks user and local servers that are on; project (`.mcp.json`) servers only when named, as in the app.
- Core: `api::start_headless(claude, wake)`: an explicit CLI path, no file watcher, and **no background checks** (new `EngineConfig::background_checks`, `true` for the app). Without it, `status docs` also ran `claude mcp get` for the user servers in the background (reproduced: the fake CLI's log showed `alpha, docs, beta`).
- `engine.rs` touched in two places only (the config field and one condition in `refresh`); other threads change it tonight, so a merge may need a trivial resolution.

Checks (Linux, this session):
- New tests first: 14 end-to-end tests in `crates/cli/tests/cli.rs` (real binary, temp `CLAUDE_CONFIG_DIR` and `HOME`, fake-claude), 13 failed on the stub. Now 14 + 8 unit tests pass.
- Regression: with `background_checks = true` in `start_headless`, `status_checks_a_named_project_server_and_exits_zero_when_it_connects` fails (`["alpha", "docs", "beta"]` vs `["docs"]`).
- Verify all: `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo clippy -p klefki -- -D warnings` 0; `cargo test --workspace` all pass (209 + 446 + 22); `cargo deny check` 0.
- Release build, 30 HTTP servers: `list` 3 ms, peak RSS 10.9 MB; `list --json` the same; `status` with a fake CLI taking 300 ms per check: 3.07 s (3 at a time), 10.8 MB. Binary 5.5 MB.

- Windows CI first failed: the tests set `USERPROFILE`, which broke Windows' home lookup (`BaseDirs` gave none: every user server "not checked: no home folder"). Fixed in the tests (6d3b805); the default-search test runs off Windows only, where the real home cannot be redirected and the real CLI could run. CI then green on all 3.

Cross-review round 1 (review 5565041236; 2 blocking, 7 should-fix, 4 polish), all taken:
- B1 secret-looking stdio arguments were printed (`--api-key sk-live-…`): core `secretlike::shown_args`, used by `inventory::classify`, shows them as `…` (flag values, `--x=`/`X=` forms, credential shapes, URLs with a password; paths and `${VAR}` stay). The app's shown command line gets it too. Reproducer fails without the core change.
- B2 a named server that is off was checked and said "Connected": off and rejected servers are never checked (`render::skip`).
- S1 exit 3 when Claude Code cannot be used (not found, shim, or no check answered). S2 JSON `schema: 1`, `claude_version`, `status: null` + `last_known` for skipped rows, `not_checked`/`check_failed`/`no_answer`. S3 a named project server's command goes to stderr first; the hint shows it; rejected ones get no "check it". S4 "Check failed"/"No answer" instead of "Not checked" for checks that ran. S5 typo suggestions (edit distance) for names and commands. S6 no arguments prints the help with examples; usage line names `--claude`. S7 progress names the server waited on.
- P1 `reload_after_change` also honours `background_checks`. P2 the hint's first line names the server. P4 `~/…` for project paths under home. Natural sort of names.
- Merged main (#24, process-tree kill) with no conflicts.

Not verified: a real Claude Code (only fake-claude).

Open, follow-up: Ctrl+C on `klefki-cli status` (or exit after a timeout) does not stop running checks and the servers they started: `Core` has no `Drop`, and children in their own process group (or with `CREATE_NO_WINDOW` on Windows) get no Ctrl+C. Needs a signal handler (a new dependency, `ctrlc`, or platform code) that drops the engine and lets #24's `cli/tree.rs` kill the trees; its own slice.

Next: re-review, merge. Later: `export`, `--project`/`--scope` filters.
