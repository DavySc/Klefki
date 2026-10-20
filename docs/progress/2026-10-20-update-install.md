# 2026-10-20: Klefki installs its own updates (M4 slice 6b)

## Goal
Decision 009, slice 6b: download the package this install needs, check it against the signed `SHA256SUMS`, and
install it (Windows per-user setup, macOS app, AppImage), on "Restart to update" or when Klefki closes. MSI, zip and
`.deb` installs keep the notice and say why. Builds on the update check (#48, `docs/progress/2026-10-20-update-check.md`).

## Done
- `crates/core/src/update.rs`: the check also returns the release's per-file checksums (`check_release`, `Release`,
  strict `parse_sums`); `Fetch::save` streams a download into a writer with a size limit (`Https` streams, the
  default writes what `get` returns).
- `crates/core/src/install.rs` (new): `InstallKind::detect` (Windows setup by its `uninstall.exe`, MSI under Program
  Files, zip otherwise; macOS `*.app/Contents/MacOS/klefki`; AppImage by an existing `APPIMAGE`; `.deb` otherwise),
  `package` names, `download` (only a file named in the signed sums, owner-only folder, hashed while written, deleted
  on mismatch, 256 MB limit), `apply` (setup `/P /R` or `/S`; bundle swap with `ditto -x -k` and renames, old one put
  back on failure; AppImage copy, chmod 755, rename).
- Engine: `Command::DownloadUpdate`, `Event::UpdateDownload(Ready | Manual | Failed)`, no second download of the same
  version, `Core::install_update(relaunch)` (synchronous, for closing), `Core::updated_by_hand()`.
  `EngineConfig { install, updates_dir }`; only `api::start` detects the install; the CLI and tests are Manual.
- App: a found release downloads at once when automatic updates are on (a release found on an earlier day is looked
  up again at start); top bar "Update: x" → "Downloading x…" → "Restart to update" (disabled while a change is open);
  Settings shows the same with Download / Restart now and the failure; installs in `on_exit` otherwise. Installs
  updated by hand keep 6a's behaviour plus the reason. The Settings text says it downloads and checks each file.
- Release workflow: `klefki-<v>-macos-universal.app.zip` (`ditto -c -k --keepParent` of the signed app);
  `check-release.py` requires it with the app binary and its `CodeResources`.
- Docs: decision 009 "Installing (slice 6b)", CLAUDE.md Network invariant (a package runs only if its SHA-256 matches
  the signed sums; flagged for the owner's review), PLAN slice 6, RELEASING table and checks.
- Changed test: `a_found_update_shows_in_the_top_bar_and_opens_its_page` now runs as an install updated by hand
  (the only kind whose button still opens the page) and also asserts nothing is downloaded; the self-updating flow
  has its own tests.

## Checks (worktree on top of #48's 89e57e9)
- `cargo fmt --check`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: 373 + 20 + 26 + 808 passed, 0 failed (1227).
- `cargo deny check`: advisories, bans, licenses, sources ok.
- `scripts/check-release.py macos` on a fake dist: reports a missing `.app.zip` entry for `CodeResources`, passes
  the zip check when present.
- Release build without a key: 18,624,848 bytes (no new dependencies).

## Release key (Davy, 2026-10-20 07:05Z)
- `minisign.pub` committed (key ID 07EC67D46C36E208; base64 decodes to 42 bytes, `Ed` + that ID + key). Builds now
  carry the key, so the app checks for updates and the release workflow signs `SHA256SUMS`.
- With the key built in, 70 app tests failed: `ShellApp::with_core` read the build-time constant, so every test
  harness started checking for updates. Fixed: `Core::checks_for_updates()` (true when the engine has a key; a
  detached test core has none), replacing `api::checks_for_updates()`. App tests pass with and without the key.
- Windows CI on 0dc0776: the AppImage path test used `Path::is_absolute`, false for `/home/...` on Windows; now a
  leading `/` (4ab5717). The model test `no_change_ever_loses_a_server_or_its_secret` failed with a new seed
  (`cc 7a12d6aa…c7d7c`) that also fails on main cd600a1: an existing bug, reported, not fixed here
  (`/mnt/project-files/packaging/model-test-seed-2026-10-20.md`).
- Release build with the key: 20,250,504 bytes (TLS, as measured in 6a). Workspace tests with the key: 1227 passed;
  app tests without it: 373 passed. clippy, fmt, deny ok.

## Not verified
Each install path on its real system (tests swap bundles and AppImages in temporary folders on Linux; the NSIS flags
are from cargo-packager 0.11.8's template), and whether macOS asks again for an app Klefki replaced. Both wait on the
first signed release.

## Next
Open the PR once #48 is merged (same branch, restarted from main), then cross-review.
