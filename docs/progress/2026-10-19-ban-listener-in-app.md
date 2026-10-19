# Ban listening sockets in the app crate (2026-10-19)
Goal: make the architecture lint enforce "`crates/app` performs no IO" for listening sockets too. `crates/app/clippy.toml` banned `TcpStream::connect` and `UdpSocket::bind` but not `TcpListener::bind`, so a bind in the UI crate passed `cargo clippy -p klefki`. Found in the callback port check cross-review, where the app briefly bound a socket on the UI thread before the check moved to the engine.

`crates/app/clippy.toml` is protected; Davy approved this change on 2026-10-19.

Branch `claude/project-thread-ospezn`, from main `3549d87`.

## Done
- `crates/app/clippy.toml`: added `std::net::TcpListener::bind` to `disallowed-methods`, with the same reason as the other network entries.

## Checks
- Acceptance first: a throwaway `std::net::TcpListener::bind(("127.0.0.1", 0))` in `crates/app/src/main.rs` passed `cargo clippy -p klefki -- -D warnings` (exit 0) before the change, and failed it after ("use of a disallowed method `std::net::TcpListener::bind`", exit 101). The probe was removed.
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: exit 0 (595 + 293 + 22 + 16 tests passed; advisories, bans, licenses, sources ok). No existing app code used `TcpListener`.

## Next
Cross-review, CI green on Linux, macOS and Windows, merge.
