# Ban the remaining network calls in the app crate (2026-10-19)
Goal: close the gaps found when cross-reviewing #20. `crates/app/clippy.toml` banned `TcpStream::connect`, `UdpSocket::bind` and `TcpListener::bind`, but other ways to do network IO still passed `cargo clippy -p klefki`.

`crates/app/clippy.toml` is protected. Davy approved this change on 2026-10-19 in the "Review engine check fixes PR" thread.

Branch `claude/project-thread-1ugrxv`, from main `2279dd4`.

## Done
- `crates/app/clippy.toml`: added these to `disallowed-methods`:
  - `std::net::TcpStream::connect_timeout`
  - `std::net::ToSocketAddrs::to_socket_addrs` (a DNS lookup)
  - `std::os::unix::net::UnixListener::bind`
  - `std::os::unix::net::UnixStream::connect`

## Checks
- Acceptance first: a throwaway function in `crates/app/src/main.rs` called all four, with `to_socket_addrs` both as a path and as a method call. Before the change, `cargo clippy -p klefki -- -D warnings` exited 0. After it, clippy exited 101 with five "use of a disallowed method" errors, one per call. The probe was removed.
- No code in `crates/app` uses `std::net` or `std::os::unix::net`.
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0, 945 passed, 0 failed; deny "advisories ok, bans ok, licenses ok, sources ok".

## Known
- On Windows, the two `std::os::unix` paths do not exist. Clippy then prints "does not refer to a reachable function" as a warning. I checked with a made-up path on Linux: that warning does not fail `-D warnings` (exit 0). I did not add `allow-invalid = true`, because it would also hide a mistyped path on Unix, and CLAUDE.md rules out suppressions.

## Next
Cross-review, CI green on Linux, macOS and Windows, merge.
