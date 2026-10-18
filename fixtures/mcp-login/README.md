# `claude mcp login <name> --no-browser` fixtures

Raw terminal output (escapes, CR LF) from Claude Code 2.1.302 on Linux, run in a pseudo-terminal against the local test server in `docs/decisions/spikes/sign-in/` (decision 005).

| File | Origin |
|---|---|
| `real-waiting-http.txt` | real: everything up to the paste prompt. The authorization server is the spike's `http://127.0.0.1:8765`, so Klefki must refuse to open it. |
| `synthetic-waiting-https.txt` | **synthetic**: the same bytes with the authorization server changed to `https://auth.example.com`, as a real server has. |
| `real-paste-refused-tail.txt` | real: the echoed paste (wrong `state`), Claude's refusal and the prompt again. |
| `real-signed-in-tail.txt` | real: what follows an accepted paste or callback; exit code 0. |
| `real-port-in-use.txt` | real: a fixed callback port that another program holds; exit code 1. |
| `real-not-oauth.txt` | real: `login` for a stdio server; exit code 1. |
