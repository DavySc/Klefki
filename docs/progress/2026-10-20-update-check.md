# 2026-10-20: update check (M4 slice 6a)

## Goal
The owner decided Klefki updates itself. First slice: find out, safely and without slowing start-up, whether a newer
release is out, and tell the user. Download and install follow in 6b. Design: `docs/decisions/009-auto-update.md`.

## Done
- `crates/core/src/update.rs`: fetches the latest release's `SHA256SUMS` and `.minisig` from GitHub (64 KiB / 4 KiB
  limits, https only, redirects kept on https, 30 s limit, system certificates, proxy from the environment, agent set
  up on first use), verifies the signature with the key built in from `minisign.pub` (`crates/core/build.rs`), and
  takes the version from the trusted comment. Plain `major.minor.patch` only, compared as numbers.
- Engine: `Command::CheckForUpdates` (own thread) answers `Event::Update`; `Command::OpenReleasePage`. Only
  `api::start` (the app) gets the https fetcher; `start_headless` (the CLI) gets none, so the CLI carries no TLS code.
- App (`crates/app/src/updates.rs`, `shell.rs`): automatic check once a run, 3 s after the window opens, at most
  once a day after an answer (failures retried next start; a last check in the future does not block); remembered
  found version, dropped once not newer; top-bar button and a Settings section (version, Checking…, N is available
  with What's new, Up to date, the failure, Check now, "Check for updates automatically", on by default). A build
  without a key says it does not check, and never sends the command.
- `CLAUDE.md`: new invariant line (network only for this, https to GitHub, signed). Flagged for human review.
- New crates: ureq 3 (no default features), rustls 0.23 (ring), rustls-native-certs, minisign-verify. Not
  rustls-platform-verifier: it pulls `webpki-root-certs` (CDLA-Permissive-2.0, WebAssembly only), which `deny.toml`
  (protected) does not allow.

## Checks run
- `cargo fmt --check` exit 0; `cargo clippy --workspace --all-targets -- -D warnings` exit 0; `cargo test --workspace`
  exit 0 (770 core lib, 360 app, 25 + 20 others; 1175 total); `cargo deny check` exit 0 (advisories, bans, licenses,
  sources ok). Re-run after rebasing onto main with #45 and #46: see the PR.
- New tests: 12 in `update.rs` (newer/older/same, numeric order, altered checksums, other key, `main`/pre-release/
  other comment, garbage, no key fetches nothing, fetch failure, version parsing, http refused), 2 engine tests
  (found + page opened; no key), 7 in `updates.rs` (timing, daily, failure retried, off stays off, remembered,
  dropped, default on), 5 kittest UI tests (starts after the delay and wakes itself, no-key build, top bar + Settings
  open the page, Check now progress and answers, the setting is saved).
- Mutation: with the signature check removed, "altered checksums" and "another key" fail.
- Live (temporary test, not kept): through the session proxy, the real `latest/download/SHA256SUMS` gives "no release
  is published yet" (404); size limit refuses a larger file; a README under the limit downloads.
- Size, release build (fat LTO, stripped), Linux: app 18,420,040 -> 20,124,288 bytes (+1.70 MB, ring + rustls);
  CLI 6,515,496 -> 6,408,072 (no TLS code).
- Memory, Linux under Xvfb, empty temp config, release builds: main 126.9 MB RSS at 2/6/10 s; with a test key
  128.4 MB at 2 s, 130.6 MB at 6 and 10 s (after the check). Threads 18 in both.

## Not verified / open
- Real releases: none is published yet, and the release key is not committed (owner), so no real check has passed.
- Windows and macOS: certificates from the system store and the proxy not tried.
- 6b: download, verify against `SHA256SUMS`, install per install type (decision 009, Next).
