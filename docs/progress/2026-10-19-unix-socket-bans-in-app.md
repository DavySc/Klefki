# Ban the remaining Unix socket calls in the app crate (2026-10-19)
Goal: close the gaps the cross-review of #23 found. After #23, `crates/app/clippy.toml` banned `UnixListener::bind` and `UnixStream::connect`, but the rest of the Unix socket API still passed `cargo clippy -p klefki`.

`crates/app/clippy.toml` is protected. Davy approved this change on 2026-10-19 in the "Review engine check fixes PR" thread.

Branch `claude/project-thread-1ugrxv`, from main `f5724b3`.

## Done
- `crates/app/clippy.toml`: added these to `disallowed-methods`:
  - `UnixListener::bind_addr`
  - `UnixStream::connect_addr`
  - `UnixDatagram::bind`
  - `UnixDatagram::bind_addr`
  - `UnixDatagram::unbound`
  - `UnixDatagram::connect`
  - `UnixDatagram::connect_addr`

  All are under `std::os::unix::net`. `UnixStream::pair` and `UnixDatagram::pair` stay allowed: they create a connected pair inside the process and reach nothing outside it.

## Checks
- Acceptance first: a throwaway function in `crates/app/src/main.rs` called all seven. Before the change, `cargo clippy -p klefki -- -D warnings` exited 0. After it, clippy exited 101 with seven "use of a disallowed method" errors, one per call. The probe was removed.
- No code in `crates/app` uses `std::os::unix::net`.
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0, 1071 passed, 0 failed; deny "advisories ok, bans ok, licenses ok, sources ok".

## Known
- As with #23, on Windows these `std::os::unix` paths do not exist. Clippy prints "does not refer to a reachable function" for each one, which does not fail `-D warnings`. #23's Windows CI showed this for its two entries.

## Next
Cross-review, CI green on Linux, macOS and Windows, merge.
