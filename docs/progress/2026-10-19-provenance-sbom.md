# Build provenance and SBOM (2026-10-19)
Goal: M4 slice 2 of 6. Every release file gets a GitHub build provenance attestation, and each program gets a
CycloneDX SBOM. Follows slice 1 (`2026-10-19-release-builds.md`, PR #40). Branch `claude/packaging-release-407cg1`,
restarted from main after #40 merged.

- **SBOM.** The Linux build job runs `cargo cyclonedx -f json --spec-version 1.5 --describe binaries
  --no-build-deps --target all` (cargo-cyclonedx 0.5.9) and ships `klefki-<v>.cdx.json` and
  `klefki-cli-<v>.cdx.json`. `--target all` lists the Windows and macOS-only crates too, so one SBOM per program
  covers all three systems; `--no-build-deps` leaves out build-time crates such as `winresource` (nothing of
  them reaches a binary). Locally: 445 components for klefki, 219 for klefki-cli, licences as SPDX expressions.
- **Provenance.** The publish job now runs on manual runs as well as tags: it writes `SHA256SUMS`, attests every
  file in `dist/` with `actions/attest-build-provenance@v4` (`id-token: write`, `attestations: write` on that job
  only), then runs `gh attestation verify` on each file against the release workflow, and uploads everything as
  the `klefki-all` artifact. Only the draft-release step is limited to tags. The repository is public, which
  attestations need on a free plan.
- **Check.** `scripts/check-release.py linux` now requires both SBOMs and checks they are CycloneDX for the
  right program and version with at least one component. Seen failing first ("missing klefki-0.1.0.cdx.json").
- **Docs.** `docs/release-notes.md` tells users how to verify a download with `gh attestation verify` and what
  the SBOM files are; `docs/RELEASING.md` describes both.

## Checks run
- Local: SBOMs generated with the same command; `check-release.py linux` OK with them (exit 0), and reports
  missing SBOMs, an SBOM for the wrong program and a non-CycloneDX file (exit 1). `cargo fmt --check` exit 0.
  No Rust code changed in this slice.
- CI, manual run 39332472155 on 8ef983a: all three builds green, `check-release.py` OK on each; the publish job
  attested and then verified all 9 files (`SHA256SUMS`, 6 packages, 2 SBOMs) with `gh attestation verify`
  against the release workflow; draft-release step skipped as intended (not a tag). Attestation
  https://github.com/DavySc/Klefki/attestations/57570117, Rekor log index 3327068904.

- Cross-review (review 5578471289 on 2902da4), one blocker fixed: the users' verify command now pins
  `--signer-workflow .../release.yml --source-ref refs/tags/v{version}`, since `--repo` alone accepts any
  workflow or branch (and manual runs from any branch now attest). The CI self-check adds `--source-ref
  "$GITHUB_REF"`. Polish: the notes say to `gh auth login` first (checked: gh 2.89 without a token asks for it)
  and that success prints `✓ Verification succeeded!` (string checked in the gh binary); `klefki-all` is uploaded
  on manual runs only.

## Not verified / open
- `--source-ref` in the CI self-check first runs on the next manual or tag run (the trial artifacts could not be
  downloaded here to test it by hand: the proxy refuses the artifact storage host).
- The draft-release step itself still first runs on a real tag.
- Next slice: minisign signature on `SHA256SUMS`, which needs a key from the owner.
