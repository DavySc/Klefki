# Lock file: yanked tokio

Goal: `cargo deny check` failed on main because Cargo.lock pinned tokio 1.54.0, which is yanked and also carries an
advisory (race condition in `tokio::time::Interval`). CI did not catch it. Fix with the smallest lock change.

## Done
- `cargo update -p tokio`: 1.54.0 -> 1.54.1, nothing else moved. `Cargo.toml` and `deny.toml` untouched.

## Checks (Linux)
- `cargo deny check`: before: advisories FAILED (vulnerability + yanked); after: advisories ok, bans ok, licenses ok,
  sources ok (duplicate-crate warnings as before).
- `cargo fmt --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`:
  exit 0, 1094 + 26 tests passed.

## Open / next
- Not run here: macOS and Windows (CI). Merge after review and green CI on all three.
- Possible follow-up: run `cargo deny check` in CI so a yanked crate fails the build there too.
