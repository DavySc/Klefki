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
(filled in below as they run)
