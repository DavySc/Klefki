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

Not verified: Windows and macOS (CI runs the tests there); a real Claude Code (only fake-claude).

Next: cross-review (CLI UX), then merge. Later: `export`, `--project`/`--scope` filters, natural sort of names (`server-10` sorts before `server-9`).
