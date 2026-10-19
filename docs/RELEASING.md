# Releasing

Releases are built by `.github/workflows/release.yml` with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager). Its settings are under `[package.metadata.packager]` in `crates/app/Cargo.toml`.

## What a release contains

| System | Files | Built on |
|---|---|---|
| Windows x64 | NSIS installer (per-user, no admin), MSI, portable zip | `windows-latest` |
| macOS 11+ | universal (Intel + Apple silicon) `.app` in a `.dmg` | `macos-latest` |
| Linux x86_64 | `.deb`, AppImage | `ubuntu-22.04`, for older glibc |

Each package holds both `klefki` and `klefki-cli`. The Linux job also writes a CycloneDX SBOM per program (`cargo cyclonedx --target all`, so one SBOM covers all three systems). The publish job adds `SHA256SUMS` and a build provenance attestation for every file (`actions/attest-build-provenance`, signed through Sigstore), then checks each one with `gh attestation verify`. It does this on manual runs too; only a tag creates the release (a manual run keeps the combined files as the `klefki-all` artifact). Attestations need the repository to be public, or a paid plan. Install instructions for users are in `docs/release-notes.md`, which becomes the top of the release notes with every `{version}` replaced by the release's version (so each command and file name can be pasted as is); GitHub appends the list of merged pull requests. Keep its file names in step with the `Package` step of the workflow.

Before upload, `scripts/check-release.py` looks inside each package: the Windows app is a GUI program with its icon embedded and the CLI a console program, the macOS binaries hold both architectures and `Info.plist` carries the version, and the `.deb` installs both programs, a menu entry and an icon.

The builds are not signed yet; signing comes in later M4 slices (`docs/PLAN.md`).

## Cutting a release

1. Set `version` in the root `Cargo.toml` (`[workspace.package]`), run `cargo check` so `Cargo.lock` follows, and merge that through a pull request.
2. Tag the merge commit on main and push the tag:
   ```
   git tag v0.2.0 origin/main
   git push origin v0.2.0
   ```
   The workflow refuses a tag that does not match the workspace version.
3. When the three builds pass, the workflow creates a **draft** release with every file. Check it, edit the notes if needed, and press *Publish*.

To try the packaging without releasing, start the workflow by hand (Actions, *release*, *Run workflow*). It builds and checks the packages and keeps them as workflow artifacts; nothing is published. Each run costs about ten minutes of macOS time, billed at 10x, so do this only when the packaging changes.

## Building a package locally

```
cargo install cargo-packager --locked --version 0.11.8
cargo build --release --locked -p klefki -p klefki-cli
cargo packager --release -p klefki -f deb,appimage -o target/packages   # nsis,wix on Windows; app,dmg on macOS
```
