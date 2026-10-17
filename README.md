# klefki

A fast, native, cross-platform manager for the MCP servers configured in Claude Code. It replaces the earlier Electron-based MCP manager.

- Built with Rust + egui, for Windows, macOS and Linux.
- Dark theme in a dark, desktop-app style.
- Lists every MCP server Claude Code knows about (user, local and project scope; stdio, HTTP, SSE and WebSocket) and lets you edit, duplicate, move, enable/disable, authorize and delete them.
- Keeps the original app's security model:
  - the UI never touches files, network, secrets or processes
  - OAuth registrations go through the Claude Code CLI
  - it never creates entries in the shared `.mcp.json`
  - client secrets live in the OS secret store

## Status

Planning — no code yet. See [docs/PLAN.md](docs/PLAN.md) for the architecture, security requirements, milestones and verification approach.
