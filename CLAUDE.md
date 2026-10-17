# CLAUDE.md

Rust + egui manager for Claude Code MCP servers. Plan: `docs/PLAN.md`.
"Done" needs evidence you produced by running something.

## Commands (available after M0)
- Verify all: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`
- Fast: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --lib`
- End-to-end: `cargo test -p klefki-core --tests` (fake-claude), `cargo test -p klefki` (egui_kittest), then `cargo run -p klefki`
- Architecture: `cargo clippy -p klefki -- -D warnings` (disallowed-methods) and `cargo deny check`

If a command can't run, report the work as **unverified**.

## Rules
- Write acceptance criteria first and see them fail; implement general behavior, never hard-coded examples.
- Work in small slices: implement, verify, update `docs/progress.md`. Commit only when asked.
- Never weaken, skip or delete tests or checks. If one seems wrong, stop and explain.
- Protected; propose changes instead of editing: `fixtures/malicious/`, `crates/fake-claude/`, `crates/app/clippy.toml`, `deny.toml`.
- No suppressions (`#[allow]`, ignore lists, `--no-verify`) to get a pass.
- Tests never touch the real `~/.claude.json`, `~/.claude/` or the keychain. Use a temp `CLAUDE_CONFIG_DIR` and a mock secret store.
- Every bug gets a failing reproducer that stays as a regression test, plus a nearby variant.

## Invariants (security first; flag changes here for human review)
- `crates/app` performs no IO: no fs, process, net or URL opening. Only `klefki_core::api`.
- All writes go Prepare → diff → Apply, through `config/docstore.rs`: re-read, hash check, backup, atomic persist; key order and unknown keys preserved.
- OAuth entries are written only via `claude mcp add-json --client-secret`. The secret is passed only in the `MCP_CLIENT_SECRET` env var, never in argv or logs.
- Never create entries in `project` scope (`Target` is `User | Local` only). Project servers are toggled through `disabledMcpServers` in the project entry of `~/.claude.json`, like every other server. The repository's `.claude/settings.local.json` is written only to approve a project server (`enabledMcpjsonServers`), after showing its full command.
- Secrets live in the OS keyring, keyed by `ConnectionId`. No plaintext fallback. Never read `~/.claude/.credentials.json`.
- Claude is spawned with no shell, no `.cmd` or `.bat`, and names matching `^[A-Za-z0-9_-]{1,64}$`. URLs opened are https only. TLS verification is always on.
- `.mcp.json`, server metadata and CLI output are untrusted: size-limit, validate and sanitize them (ANSI, control and bidi characters).

## Progress
Keep `docs/progress.md` factual: the current slice, done work with commits, checks run and their results, open failures, and the next action. Re-run checks rather than trusting it.

## Final report
**Changed / Verified** (command, exit code, evidence) **/ Not verified / Open issues / Next.**
