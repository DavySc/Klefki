# Proposal: `crates/fake-claude` (protected path, needs approval)

A test-only stand-in for the `claude` CLI, so the runner, live status checks and (later) the transactional edit operations can be tested without the real Claude Code, the network, a login, or `~/.claude.json`.

## Why now
Live status checks (this slice) spawn `claude mcp get <name>`. The plan's verification list for that code cannot be tested against the real CLI: timeouts that kill the child, an output size cap, refusal of `.cmd`/`.bat` shims, invalid names never reaching a spawn, the secret appearing only in the environment and never in argv, and failure injection for the rollback tests in M2.

## Shape
- A binary `fake-claude` in its own crate: `publish = false`, not part of any release, built only for tests.
- Refuses to run unless `FAKE_CLAUDE_SCENARIO` points to a scenario file. It never reads `HOME`, `~/.claude.json` or `CLAUDE_CONFIG_DIR`, and never touches the network.
- **Scenario file** (JSON): an ordered list of rules. A rule matches an argv prefix (`["mcp","get","azure-devops"]`) and says what to do:
  - `stdout_file` / `stderr_file` (paths relative to the scenario), `exit_code`
  - `delay_ms` before answering
  - `hang: true` (sleep until killed, to test timeouts)
  - `flood_bytes: N` (write N bytes, to test the output cap)
  - `write_pid_to: <path>` (so a test can check the child really died)
  - first matching rule wins; no match exits 2 with a clear message.
- **Invocation log**: with `FAKE_CLAUDE_LOG=<path>` it appends one JSON line per run: argv, working directory, whether `MCP_CLIENT_SECRET` was set (and a SHA-256 of its value, never the value), and whether `NO_COLOR` was set. Tests assert the secret is in the environment and not in argv.
- Used from tests as the program path handed to the runner (`CARGO_BIN_EXE_fake-claude`).

## Not included
- No attempt to imitate Claude's real behaviour beyond what a scenario says. Scenarios use output captured from the real CLI (`fixtures/mcp-get/`).
- No writes to any Claude config. The M2 edit-operation tests will extend scenarios (for example "`remove` fails the second time") when those tests are written, and I will propose those changes separately.

## Other protected files, still pending
- `crates/app/clippy.toml` (architecture check): see `protected-files.md`.
- `deny.toml`: needs `cargo-deny` installed and a licence decision.
- `fixtures/malicious/`: not needed yet, the hostile-input cases are covered by inline tests.
