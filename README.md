<p align="center"><img src="crates/app/assets/icon/klefki-256.png" width="128" height="128" alt="Klefki icon: a hub with a keyhole, wired to four servers"></p>

# Klefki

A fast, native, cross-platform manager for the MCP servers configured in Claude Code. It replaces the earlier Electron-based MCP manager.

- Built with Rust + egui, for Windows, macOS and Linux.
- Catppuccin Mocha (dark) and Latte (light) with accents inspired by Klefki, set in JetBrains Mono. Switch with the theme dropdown in the top bar (System, Light, Dark), or `--theme light|dark|system`.
- Lists every MCP server Claude Code knows about (user, local and project scope; stdio, HTTP, SSE and WebSocket) and lets you edit, duplicate, move, enable/disable, authorize, export and delete them.
- Keeps the original app's security model:
  - the UI never touches files, network, secrets or processes
  - OAuth registrations go through the Claude Code CLI
  - it never creates entries in the shared `.mcp.json`
  - client secrets live in the OS secret store

## Status

M0/M1 in progress: a themed shell (nav, top bar, toolbar, grid, detail panel) that lists the servers from your real Claude Code config, read-only. Nothing is written yet and the toolbar actions are disabled. `cargo run -p klefki -- --demo` shows built-in sample data instead. See [docs/PLAN.md](docs/PLAN.md) for the architecture, security requirements, milestones and verification approach, and [docs/progress.md](docs/progress.md) for the current state.

```
cargo run -p klefki
cargo run -p klefki-cli -- list            # the servers, grouped by scope and project; runs nothing
cargo run -p klefki-cli -- status [NAME]   # asks Claude Code whether they connect; --json for scripts
cargo run -p klefki-cli -- export [NAME]   # one {"mcpServers": …} block, secrets as ${VAR} references
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
