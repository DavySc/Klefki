# Proposed protected files

Status 2026-10-17: `crates/app/clippy.toml` and `crates/fake-claude/` were approved and created (see `fake-claude.md`). `deny.toml` was created at the owner's request on 2026-10-17 (needs review; see `docs/progress.md`). `fixtures/malicious/` is still pending.

## `crates/app/clippy.toml`
Enforces "the UI performs no IO". Disallowed paths are checked by `cargo clippy -p klefki -- -D warnings`.
```toml
disallowed-methods = [
  { path = "std::fs::read", reason = "UI does no IO; go through klefki_core::api" },
  { path = "std::fs::write", reason = "UI does no IO; go through klefki_core::api" },
  { path = "std::fs::read_to_string", reason = "UI does no IO; go through klefki_core::api" },
  { path = "std::fs::File::open", reason = "UI does no IO; go through klefki_core::api" },
  { path = "std::fs::File::create", reason = "UI does no IO; go through klefki_core::api" },
  { path = "std::process::Command::new", reason = "UI spawns no processes" },
  { path = "std::net::TcpStream::connect", reason = "UI does no network IO" },
  { path = "open::that", reason = "URLs are opened through core, https only" },
]
```
Since `crates/app` has no matches today (grep), this adds no failures. Lists are not exhaustive; extend as needed.

## `deny.toml`
Licence allowlist, advisories and bans for `cargo deny check`. Needs `cargo install cargo-deny` and a decision on the allowed licences (the egui tree pulls MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unicode-3.0, OFL-1.1 for fonts). Proposal: generate with `cargo deny init`, then trim.

## `crates/fake-claude/` and `fixtures/malicious/`
Needed from M1 (CLI runner tests, `.mcp.json` parser). Design will follow the plan's verification section; I'll draft them when M1 starts.
