# Deprecated and stale dependencies

Goal: find dependencies that are deprecated, unmaintained, yanked or held back, and fix the ones that change no behaviour and cost no size, start-up time or memory.

## Audit (main ba883ec)
- `cargo deny check`: passes. With `unmaintained = "all"` it reports RUSTSEC-2026-0370/0371/0372 (zvariant, zvariant_derive, zbus_names, 2026-10-16): merged into zbus 6. Every user still needs zbus 5 (secret-service 5.2.0, accesskit_unix 0.24.0 used by egui 0.37 too), so it is not fixable here; deny.toml's `unmaintained = "workspace"` already covers it. No yanked crates, no vulnerabilities.
- `rust-version = "1.88"` was wrong: eframe/egui 0.36 need 1.95. Because resolver 3 respects it, the wrong value also held back aes 0.9.3 and uuid 1.28.
- GitHub Actions: CI logs warn "Node.js 20 is deprecated ... actions/checkout@v4"; release.yml also used upload-artifact@v4 and download-artifact@v4 (node20). GitHub removes Node 20 from runners in autumn 2026. Latest: checkout v7, upload-artifact v7, download-artifact v8 (node24). Their inputs are a superset of v4's (new: `archive`, `skip-decompress`, `digest-mismatch` default `error`). Swatinem/rust-cache@v2 and attest-build-provenance@v4 are already fine.
- Duplicates: sha2 0.10 (core) next to sha2 0.11 (secret-service, Linux). pollster 1.0 exists but rfd 0.17 uses 0.4, so it stays.
- Up to date: cargo-packager 0.11.8, cargo-cyclonedx 0.5.9, cargo-deny 0.20.2.
- Not done (needs a decision or upstream): egui 0.37 (egui-phosphor 0.14 is on 0.36); zbus 6 (upstream crates); ubuntu-22.04 release runner (supported to about April 2027).

## Done
- `rust-version` 1.95; PLAN.md says so. `cargo update`: aes 0.9.3, cc 1.9.0, find-msvc-tools 0.1.15, inout 0.2.5, redox_syscall 0.9.5, rustls-webpki 0.103.16, tokio 1.54.0, uuid 1.28.0.
- Actions: checkout v7 (ci.yml, release.yml), upload-artifact v7, download-artifact v8 (release.yml). Signing steps untouched.
- klefki-core on sha2 0.11. Its digest has no `{:x}`, so `hex::lower` writes lowercase hex (tests: the empty-input SHA-256, and two digits per byte). Hashes are unchanged: the runner test compares core's hex with fake-claude's sha2 0.10 `{:x}`. fake-claude (protected) stays on sha2 0.10; it is a test binary.

## Checks (Linux)
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`: all exit 0.
- Release size: klefki 20,388,744 -> 20,387,336 B; klefki-cli 6,773,288 -> 6,766,632 B. Crates in the app's Linux tree: 395 -> 389.
- Peak RSS 4 s after start under Xvfb (3 runs each): before 129.7/130.5/130.7 MB, after 130.3/130.8/130.9 MB, within noise.

## Next
Cross-review; CI on all three systems. The release workflow's new action versions run only on a tag or manual run.
