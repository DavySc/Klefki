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

## Review round 1 (cross-review on 5d2f401) and Windows CI
- Windows CI: ConPTY redrew the line break after the link as a cursor move, so the link read `…%2FmcpWaiting`. The link is now looked for in text where an escape or carriage return also ends a word (99cfeeb; regression test with four ways a terminal moves on).
- B1: a click beside the dialog no longer cancels a running sign-in (Esc and Cancel still do); after a failure it still closes.
- B2: failures are `SignInFailure { title, help, said, can_retry }`, built in core from `diagnose/hints.rs` plus two cases (DCR rejected / no OAuth, unknown server). Claude's words are kept below, cut before any HTML page. When trying again cannot help, Close is the default (Enter).
- U1: the dialog says how long Klefki still waits (repaints once a minute); in the last 30 s it counts down and offers "Keep waiting" (`Command::KeepWaitingForSignIn`, which restarts the wait in core).
- U2: the dialog is anchored at the top, so it grows downwards. U3: one name, "Sign in", everywhere. U4: Status tab and toolbar offer it only for Needs auth or a server with OAuth set up; Auth tab and palette for any HTTP/SSE server. U5: Auth tab copy.
- O1: the host is bold, and a page on another site than the server's is pointed out. O3: the answer to a paste is read from its own buffer (was a stale offset after the output was cut). O5: a sign-in counts as over when it reports its end.
- Not done: O2 (egui 0.36 has no live-region API), O4 (a query split across reads; ConPTY sends it whole), "Hide" to keep a sign-in running in the background.
- Mutations, each failed a test: backdrop click closing, Try again always shown, no host note, Keep waiting not restarting the clock, sign-in offered for every HTTP server, KeepWaiting ignored in core, the done flag set late, the paste answer buffer cleared.
- Real window with real Claude Code 2.1.302: a click beside kept the dialog; Cancel left no `mcp login`; a server that is down said "Nothing is listening at that address." with Try again; a plain HTTP server said "This server does not sign in through Claude Code." with Close only and no HTML; sign-in with the browser following ended Connected.
- Verify-all: exit 0, 653 passed, deny ok.

## Review round 2 (on 7dd1417): nothing blocking
- Polish folded in once: the host uses the bold face; "1 second"; the same-site rule needs one more label under shared suffixes (`co.uk`-style, `github.io`, `vercel.app`, …), with a unit test that fails under the old rule.
- Verify-all: exit 0, 654 passed, deny ok.

## Open
- Windows (ConPTY) and macOS run only in CI; not tried by hand.
- Not done: checking the link's host against the server's OAuth metadata (needs the planned HTTP client in core).
- PLAN.md planned a terminal pane; the transcript is not shown (decision 005 says why).

## Next
Cross-review, CI green on all three systems, merge.
