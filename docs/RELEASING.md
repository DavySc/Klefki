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

The installers are not code-signed yet; Windows and macOS signing come in later M4 slices (`docs/PLAN.md`).

## Checksum signature (minisign)

The `sign` job signs `SHA256SUMS` with `scripts/sign-checksums.sh`. Committing `minisign.pub` at the repository root switches it on; until then the job prints a notice and skips. Once the key is committed, a missing secret, a wrong password, a signature that does not verify, or a tag release without a signature fails the release. The publish job checks the signature against its own `SHA256SUMS` and `minisign.pub` before attesting, and the release notes gain a signature check (`docs/release-notes-minisign.md`, filled in where `{minisign}` stands in `docs/release-notes.md`).

The key is a secret of the `release` **environment**, not a repository secret: any workflow on any branch can read repository secrets, while an environment secret reaches only jobs that use the environment, from the branches and tags it allows. The `sign` job is the only job that uses it, and runs only for tags and on main; manual runs from other branches still build, checksum and attest without it.

To set it up (once, by the owner):

1. `minisign -G -p minisign.pub -s klefki.key`, with a strong password. Keep `klefki.key` and the password backed up offline: losing them means users must move to a new key.
2. Settings, Environments, **New environment** `release`. Under *Deployment branches and tags* choose *Selected branches and tags* and add the branch `main` and the tag pattern `v*`. Create it before the first run on main or a tag: otherwise GitHub creates it on that run with no rules.
3. In that environment, add the secrets `MINISIGN_SECRET_KEY` (the whole content of `klefki.key`) and `MINISIGN_PASSWORD`. Do not add them as repository secrets.
4. Commit `minisign.pub` through a pull request.
5. Start the release workflow by hand on main: the `sign` job should print `Signature and comment signature verified`.

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
