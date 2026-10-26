# CLAUDE.md

Rust + egui manager for Claude Code MCP servers. Plan: `docs/PLAN.md`.
"Done" needs evidence you produced by running something.

## Commands (available after M0)
- Verify all: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`
- Fast: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --lib`
- End-to-end: `cargo test -p klefki-core --tests` (fake-claude), `cargo test -p klefki` (egui_kittest), then `cargo run -p klefki`
- Architecture: `cargo clippy -p klefki -- -D warnings` (disallowed-methods) and `cargo deny check`
- Cloud sessions: `scripts/cloud-setup.sh` (run by the SessionStart hook in `.claude/settings.json`) installs what these need.

If a command can't run, report the work as **unverified**.

## Rules
- Write acceptance criteria first and see them fail; implement general behavior, never hard-coded examples.
- Work in small slices: implement, verify, update the slice's progress file. Commit only when asked.
- Never weaken, skip or delete tests or checks. If one seems wrong, stop and explain.
- Protected; propose changes instead of editing: `fixtures/malicious/`, `crates/fake-claude/`, `crates/app/clippy.toml`, `deny.toml`.
- No suppressions (`#[allow]`, ignore lists, `--no-verify`) to get a pass.
- Tests never touch the real `~/.claude.json`, `~/.claude/` or the keychain. Use a temp `CLAUDE_CONFIG_DIR` and a mock secret store.
- Every bug gets a failing reproducer that stays as a regression test, plus a nearby variant.

## Invariants (security first; flag changes here for human review)
- `crates/app` performs no IO: no fs, process, net or URL opening. Only `klefki_core::api`.
- All writes go Prepare → diff → Apply, through `config/docstore.rs`: re-read, hash check, backup, atomic persist; key order and unknown keys preserved.
  One exception to showing the diff, approved by the owner (on/off plan S3): a switch in one project, with nothing the review would warn of, is applied straight away and offers Undo; its plan, backup and Activity log entry stay.
- One exception, approved by the owner (decision 012): Klefki's own `profiles.json` and `profile-uses.json`, beside the backups folder, are written whole and owner-only by `profiles.rs` and `profile_uses.rs`, and only when they could be read in full. They hold server ids, names and on/off switches, never secrets; using or stopping a profile still goes through the plan above.
- OAuth entries are written only via `claude mcp add-json --client-secret`. The secret is passed only in the `MCP_CLIENT_SECRET` env var, never in argv or logs.
- Never create entries in `project` scope (`Target` is `User | Local` only). Project servers are toggled through `disabledMcpServers` in the project entry of `~/.claude.json`, like every other server. The repository's `.claude/settings.local.json` is written only to approve a project server (`enabledMcpjsonServers`), after showing its full command.
- Secrets live in the OS keyring, keyed by `ConnectionId`. No plaintext fallback. Never read `~/.claude/.credentials.json`.
- Claude is spawned with no shell, no `.cmd` or `.bat`, and names matching `^[A-Za-z0-9_-]{1,64}$`. URLs opened are https only. TLS verification is always on.
- Besides Claude, Klefki starts a user or local stdio server's own command, only to check it (decision 015).
  Which: the servers its checks would have Claude Code start (enabled, loaded, not blocked, not switched off where it
  runs, and only when Claude Code is found), only the definition the check was decided for; project servers stay
  with Claude Code.
  How: no shell; the program is found on the server's PATH, never in the working folder, and started by full path; on
  Windows a `.cmd` or `.bat` is checked through Claude. Environment as Claude Code's (Klefki's minus what Claude Code
  removes, the settings files' `env`, the definition's with `${VAR}` filled in), never Claude Code's own credentials
  (`CLAUDE_CODE_OAUTH_TOKEN` and the like), only to the child, never in argv or logs. The process tree ends with it.
  What is sent: only Claude Code's hello (`server/discover` first, then `initialize` and `notifications/initialized`
  when the server does not answer the newer way), one `tools/list`, and answers to `ping` and `roots/list`; replies
  are untrusted (size-limited, sanitised).
  Fallback: any answer other than Connected is checked again with `claude mcp get`, and that answer is shown. The
  setting "Check servers with Claude Code only" turns it off.
- Network: only Klefki's own updates in `klefki-core` (decision 009): https to GitHub, and nothing trusted unless signed by the release key built in from `minisign.pub`. A downloaded package is installed or started only if its SHA-256 matches the signed `SHA256SUMS`. That key signs only in the `release` environment, behind the owner's required approval, for `v*` tags only the owner can create (`docs/RELEASING.md`).
- `.mcp.json`, server metadata and CLI output are untrusted: size-limit, validate and sanitize them (ANSI, control and bidi characters).
- Claude Code's MCP logs (`claude-cli-nodejs/<folder>/mcp-logs-<server>/`) are read only by `last_start.rs` (decision 013): names listed, at most a few files, only both ends of each, never written or deleted. They are untrusted, and what is shown or copied is sanitised with the server's own values and secret-looking text hidden.

## Progress
Each slice has its own file, `docs/progress/<YYYY-MM-DD>-<slug>.md`, written only by the session doing it. Keep it factual: the goal, done work with commits, checks run and their results, open failures, and the next action. `docs/progress.md` is the log up to 2026-10-18; add nothing to it, but strike an open item there when you fix it. Re-run checks rather than trusting either.

## Parallel sessions
- Several sessions work at once, each on its own branch from the latest main, one slice per branch and pull request.
- Before starting, read the open pull requests and the newest progress files. If your slice changes the same code as one in flight, say so first.
- Bring main in with a merge. Never rebase or force-push a branch that has a pull request.
- Other sessions do not see your chat: write what they need in your progress file or the pull request description.

## Final report
**Changed / Verified** (command, exit code, evidence) **/ Not verified / Open issues / Next.**
