# Sign in and re-authorize from Klefki (2026-10-18)
Goal: PLAN.md M2 "PTY login and re-authorize", starting with the callback-port spike.

## Done
- Spike with real Claude Code 2.1.302: `docs/decisions/005-sign-in.md` (findings and decisions), scripts in `docs/decisions/spikes/sign-in/`, captured transcripts in `fixtures/mcp-login/` (kept as CR LF by `.gitattributes`). Key facts: `--no-browser` still listens on 127.0.0.1 and finishes on its own when the browser is local; the port is random per client registration; a terminal is required; re-authorize is the same `login`; a local server signs in from its project folder.
- fake-claude: `require_tty`, `read_line_to`, `after_line_stdout_file` (protected; proposal `docs/proposals/fake-claude-sign-in.md`, approved by Davy, applied as written).
- Core `cli/login.rs`: portable-pty (ConPTY on Windows), no shell, `NO_COLOR=1`, `MCP_CLIENT_SECRET` removed last. Finds the link only in complete words, checks it (https, no userinfo, loopback `redirect_uri` with a port, `state`), checks a paste (same callback and state, `code`/`error`, no control characters) before typing it with Enter and nothing else. Ends on exit, Cancel, a 5-minute timeout or Klefki closing; the process is killed.
- Engine: `Command::{SignIn, SignInPaste, OpenSignInPage, CancelSignIn}`, `Event::SignIn { step }`, one sign-in at a time, success triggers Claude's own check. `api::open_https` opens https only (`open` with `shellexecute-on-windows`, so no `cmd /c start`).
- App: sign-in dialog (`sign_in.rs`): "Finish signing in in your browser", Open the page again, Copy link (primary when the browser did not open), the paste fallback folded under "Your browser says it cannot reach localhost?", Try again on failure, Cancel/Esc. Entry points: Status tab (primary when it needs auth), General tab when it needs auth, Auth tab, toolbar, palette, and the toast after adding an OAuth server.
- Commits: a3d41ab, plus the follow-up with this file.

## Checks
- Mutations, each made a test fail: no complete-word rule; no state check; no Enter after the paste; http allowed in `find_page`; no `sign_in::stop` on close; loopback check dropped; `open_https` allowing any scheme; the add toast fading; two sign-ins at once; `MCP_CLIENT_SECRET` not removed (this one survived first, so the real-terminal test now passes the secret in and checks it never arrives).
- End to end with real Claude Code on Linux (Xvfb, https spike server, fake `xdg-open`): needs auth → Sign in → page opened; a bad paste refused with a message; the real callback pasted → Connected and a toast; browser-follow mode signed in by itself; closing Klefki during a sign-in left no `mcp login` process.
- Verify-all (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0, 644 tests passed, deny "advisories ok, bans ok, licenses ok, sources ok".

## Open
- Windows (ConPTY) and macOS run only in CI; not tried by hand.
- Not done: checking the link's host against the server's OAuth metadata (needs the planned HTTP client in core).
- PLAN.md planned a terminal pane; the transcript is not shown (decision 005 says why).

## Next
Cross-review, CI green on all three systems, merge.
