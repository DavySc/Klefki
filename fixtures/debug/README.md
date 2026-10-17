# `claude --debug mcp --debug-file` fixtures

Real debug files from Claude Code 2.1.302 on Windows, written while running `claude --debug mcp --debug-file <path> mcp get <name>` against a throwaway configuration with a server that cannot start. The user name in paths is anonymised and line endings are LF.

| File | Server |
|---|---|
| `failed-stdio.log` | a stdio server whose command does not exist; includes the server's own stderr |
| `failed-http.log` | an HTTP server on a port nothing listens on; includes the transport options and proxy environment |
