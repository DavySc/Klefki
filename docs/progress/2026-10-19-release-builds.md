# Release builds from a tag (2026-10-19)
Goal: M4 slice 1 of 6 (order in PLAN.md's M4): let someone install Klefki. A `v*` tag builds unsigned installers
for the three systems and publishes them, with checksums, as a draft GitHub Release. Branch
`claude/packaging-release-407cg1`.

- **Licence.** The owner asked for the most freely usable licence (2026-10-19 21:13Z): MIT OR Apache-2.0, with
  `LICENSE-MIT` and `LICENSE-APACHE` at the root and `license.workspace = true` in core, app and cli.
  `crates/fake-claude` is protected and untouched; it is a test helper and never shipped. `deny.toml` unchanged.
- **Windows exe.** `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` in the app: a release build
  opens no console window (debug builds keep it). `crates/app/build.rs` embeds `klefki.ico` and version details
  with `winresource` (build-dependency, no default features, so no extra crates; does nothing for other targets).
- **Packaging.** cargo-packager 0.11.8, configured under `[package.metadata.packager]` in `crates/app/Cargo.toml`
  (version, name and description come from Cargo). Windows: NSIS (per-user, no admin) + MSI + portable zip.
  macOS: lipo-merged universal binaries, `.app` + `.dmg`, `MACOSX_DEPLOYMENT_TARGET=11.0`. Linux: `.deb` (depends
  `libgl1 | libegl1, libxkbcommon0`, which eframe loads at run time) + AppImage, built on ubuntu-22.04 for older
  glibc. Both binaries in every package. `-o` must be absolute: cargo-packager resolves it from the crate folder.
- **Workflow** `.github/workflows/release.yml`: tags `v*` and manual runs only, never PRs. Refuses a tag that does
  not match the workspace version. The publish job (tags only) writes `SHA256SUMS` and runs
  `gh release create --draft --verify-tag` with `docs/release-notes.md` (install steps, unsigned-build warnings)
  plus generated notes.
- **Check** `scripts/check-release.py` (stdlib only) looks inside each system's files before upload: Windows app
  GUI subsystem + icon resource, CLI console subsystem; macOS both binaries fat with x86_64 and arm64, Info.plist
  version; `.deb` installs both binaries, a `.desktop` file and an icon; AppImage is ELF. Docs: `docs/RELEASING.md`.

## Checks run (Linux container)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: all exit 0 (1128 tests).
- `cargo packager --release -p klefki -f deb,appimage`: both built. `check-release.py linux`: missing files reported
  (exit 1) before copying, OK (exit 0) after. `apt install ./klefki_0.1.0_amd64.deb` put `klefki` and `klefki-cli`
  on PATH; `klefki-cli list` with a temp `CLAUDE_CONFIG_DIR` ran; the installed `klefki --demo` and the AppImage
  both started under Xvfb and drew the window (screenshots taken, not committed).
- Cross-built for `x86_64-pc-windows-gnu` with mingw: `objdump -p` shows klefki.exe "Windows GUI", klefki-cli.exe
  "Windows CUI". `check-release.py windows` OK on these; with the two exes swapped it reports all three problems
  (wrong subsystem twice, no icon), exit 1.
- macOS check exercised on synthetic Mach-O headers only (thin arm64 reported as not universal; wrong version reported).

## Not verified / open
- NSIS, MSI, `.app` and `.dmg` are only built on CI: run once on this branch before merging (see the PR).
- The publish job runs only on a real tag, so it first runs on the first release.
- Unsigned: SmartScreen and Gatekeeper warn. Next slices: provenance + SBOM, then minisign, Authenticode,
  notarisation (owner's keys), then the update check.
