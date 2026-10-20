# Every server Claude Code loads, slice 4: servers your organisation sets (2026-10-20)
Goal: roadmap item 2. List the MCP servers an administrator sets for the machine (`managed-mcp.json`,
`managedMcpServers`), and say when Claude Code loads one of the user's servers nowhere because of them.

## Done
- Spike against real Claude Code 2.1.304 in `/etc/claude-code` (removed afterwards): findings 1-7 of decision 009.
- `config/managed.rs` reads the organisation's folder: `managed-settings.json`, then `managed-settings.d/*.json` by
  name (http/sse only, a stdio one is a warning), then `managed-mcp.json`, each replacing a server of the same name.
  Untrusted: 1 MiB per file, 64 drop-ins, 500 servers, no links.
- `ConfigPaths::managed_dir`: the system's folder from `discover()`, or `KLEFKI_MANAGED_DIR` (empty: none); `in_dir`
  reads none. The CLI's end-to-end tests point it at a temp folder.
- Scope `Managed` ("Organisation"): read-only rows, switched per project by name; every other change is refused,
  naming the file. `ServerRow::managed` (the file), `ServerRow::not_loaded` (why Claude Code loads a server nowhere),
  `Snapshot::organisation_only`.
- A user, local or project server that the organisation's replaces, or every one of them while `managed-mcp.json`
  exists, is "not loaded": never checked, leaves no plugin server out, no relative-path warning. Adding a server says
  when Claude Code will not load it.
- Watched: both files, the drop-in folder and each drop-in.
- App: "not loaded" badge with the reason; status "Not loaded"; details say why first; organisation rows show "Set in"
  and that only an administrator changes them, "See /mcp" as status, no Edit/Env/Auth. A mixed selection says what
  it left out. CLI: "Organisation" group, notes, JSON `scope: "organisation"`, `managed`, `not_loaded`; `status`
  skips both kinds and says why.
- Fixture: `company-wiki`, set in `C:/Program Files/ClaudeCode/managed-settings.json`.

## Checks
- New tests, failing before the change: inventory (4: listed and switched per project; replaces user and local;
  `managed-mcp.json` takes the rest away; a replaced server leaves no plugin server out), engine
  `a_server_claude_code_does_not_load_is_never_checked`. Also: managed (7), ops (2), watch (1), app (2), CLI end to
  end (1) and render (relative-path note left out when not loaded, the variant of the badge fix).
- Real Claude Code vs `klefki-cli list`, same files: settings + drop-in replacing user `mine` and local `loc`, `wiki`
  off in the only project, a stdio `bad` dropped. Both agree on what loads; Klefki warns about `bad`. With
  `managed-mcp.json` added, both show only the organisation's servers; Klefki marks the rest "not loaded".
- Full verify on main `f2ee616` (#46 merged) + this change: exit 0; 352 + 20 + 26 + 778 passed, 0 failed; deny ok.

## Open
- Not verified: Windows/macOS folders; a project server of the same name being replaced; whether an organisation's
  server leaves a plugin duplicate out; no `.claude.json` means no rows at all.

## Next
Blocked by policy (`deniedMcpServers`, `allowedMcpServers`, `allowManagedMcpServersOnly`), reusing `not_loaded`; then
claude.ai connectors.
