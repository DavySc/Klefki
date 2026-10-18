# Free-port check for the OAuth callback port (2026-10-18)
Goal: PLAN.md M2's "free-port check" for the add wizard. A fixed `oauth.callbackPort` that another program holds makes Claude Code's sign-in fail at once (`docs/decisions/005-sign-in.md`, finding 3), so Klefki says so when the server is added.

Branch `claude/callback-port-check-eaow5h`, from main `86f2ad7`.

## Done
- **core `form::callback_port_warning(port)`** (exported in `api`): binds `127.0.0.1:<port>`, where Claude Code listens during sign-in, and lets go at once.
  - `AddrInUse`: "Another program is using port N right now, so signing in would fail. Close that program before you sign in, or use another port if the provider lets you register one."
  - `PermissionDenied` (a port below 1024 on Linux, a Windows excluded range): says the computer does not let programs listen there.
  - Any other error, and port 0: nothing, rather than a guess.
  - Rust's listener, like Node's in Claude Code, reuses addresses on Unix and not on Windows, so it finds a port taken where Claude Code would.
- **A warning, not a problem.** The port is the one registered with the provider, so changing it is often not an option, and the program holding it may be gone by sign-in. Review and Add are never stopped by it.
- **engine `prepare`:** for each server Claude Code's command adds (every OAuth server, pasted or filled in), a fixed callback port is checked once and the warning goes in the review's warnings as `name: …`. This covers the paste path too, and checks at the moment of review.
- **app, Fill in:** the warning shows under the Callback port field, with ⚠ in the warning colour, once the field is left; it is checked again when Review is pressed and stopped. While the port is typed it is neither checked nor shown, and an edited port hides the old result until it is checked again. A wrong port (e.g. 99999) shows its red problem instead and is not checked. The check is a fn pointer on `AddForm` (`port_check`), so app tests use a fake and open no sockets; the app itself still does no IO.
- Cost: one bind and close per leave of the field or per review, never per frame.

## Tests
- core `a_callback_port_another_program_holds_is_warned_about_and_a_free_one_is_not`: a real listener on `127.0.0.1:0` (and on Linux on `0.0.0.0:0`) is warned about, and not once dropped. Seen failing against a stub.
- engine `a_callback_port_another_program_holds_is_warned_about_in_the_review_and_the_add_goes_on`: one warning naming the server and port, the plan is still "Add"; no warning once the port is let go, nor for a server with no fixed port. Fails with the warning push removed.
- app `a_callback_port_another_program_holds_is_warned_about_once_left_and_does_not_stop_review`: not shown while typing; shown after Tab; Review stays ready; hidden for a changed port, none for a free one; re-checked on a stopped Review; a wrong port shows its problem only. Each mutation fails it: no check on leave, no check on Review, warning shown for a port other than the one checked.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: exit 0; 448 core + 210 app tests passed; advisories, bans, licenses, sources ok.
- `cargo clippy -p klefki -- -D warnings`: clean. No `std::fs`, `process::Command`, `std::net` or `open::` in `crates/app/src`.
- **Real window** (Xvfb 960×700, temp `HOME`, real Claude CLI 2.1.302, port 8766 held by a Python listener). Screenshots in the project folder `reviews/2026-10-18-callback-port/`.
  - Paste `claude mcp add -t http --client-id c-1 --callback-port 8766 corp https://corp.example/mcp`, Review: the review shows "⚠ corp: Another program is using port 8766 right now…" and Add stays available.
  - Fill in, HTTP, OAuth ticked, 8766 typed, Tab: the warning is under the field. Holder stopped, field left again: the warning is gone.

## Open / known limits
- The wildcard case (`0.0.0.0`) is asserted on Linux only; macOS lets a 127.0.0.1 listener sit beside it (for Claude Code too), and Windows is not verified.
- Not tried on Windows or macOS by hand.
- Re-authorize and sign-in from the Auth tab do not check the port first; Claude Code's own message ("OAuth callback port N is already in use") is shown there.

## Review round 1 (cross-review of `87d7805`: 2 blocking, 3 polish)
- **B1, no IO on the UI thread:** the form no longer calls the check. Leaving the port field (or a stopped Review) sets `port_wanted`; after the frame the app sends `Command::CheckCallbackPort(port)`, and the engine answers with `Event::CallbackPort { port, warning }`. The answer is shown only while the field still holds that port, so a late answer for another port is ignored. A port already asked about is not asked again until answered (egui reports a lost focus in both passes of a discarded frame). `callback_port_warning` is now `pub(crate)` and no longer in `api`.
- **B2, `::1`:** the check also binds `[::1]:<port>`, since the browser is sent to `localhost` and may try `::1` first; a program there would take the sign-in while Claude Code waits. Any error other than in-use or denied (e.g. no IPv6) says nothing. The core test holds `::1` too whenever this computer can bind it.
- **P1/P2:** "Port N is in use by another program. Signing in will fail until that program is closed, or register a different port with the provider." Denied: "Port N is reserved on this computer, so signing in would fail…".
- **P3:** the doc comment says Rust matches Node on Unix only; the native Claude Code's Windows bind options are unverified.
- **For Davy (protected file, not edited):** add `std::net::TcpListener::bind` to `disallowed-methods` in `crates/app/clippy.toml`.
- Tests: the app test now asserts the commands sent and answers with events: not asked while typing, asked once on Tab, warning shown, ready stays true, late answer for another port ignored, asked again on a stopped Review, a wrong port not asked. Fails with port matching removed, and with the ask-once guard removed. The `::1` holder could not be seen failing here: this container has no IPv6 (`EAFNOSUPPORT`); CI runners do.
- Checks: full verify exit 0, 448 core + 210 app tests; `cargo clippy -p klefki -- -D warnings` clean; no IO calls in `crates/app/src`.
- Real window again (127.0.0.1:8766 held): Fill in, port 8766, Tab: the engine's answer shows under the field (`reviews/2026-10-18-callback-port/r2-field-warning-from-engine.png`).

## Review round 2 (re-review of `826206d`: 1 blocking)
- **T1:** engine test `the_engine_answers_whether_a_callback_port_is_held_for_that_port`: holds 127.0.0.1:0, sends `CheckCallbackPort`, gets `CallbackPort` for the same port with a warning, then `None` once let go. Fails when the arm answers `None`, and when it answers for another port.
- Checks: full verify exit 0, 449 core + 210 app tests.

## Before merge
- Main (with #25) merged in as `763a54e`. Full verify exit 0: 453 core + 210 app + 13 + 19 tests; `cargo clippy -p klefki -- -D warnings` clean.

## Next
- Merge before the "Safe edits and move or copy" slice.
- Open for Davy's Windows machine: hold `0.0.0.0:<port>` and check the warning appears (the native `claude.exe` may use `SO_EXCLUSIVEADDRUSE`).
- F1: check the port again when Sign in is pressed, before `claude mcp login` starts. F2: name the program holding the port (`/proc/net/tcp`, `GetExtendedTcpTable`).
