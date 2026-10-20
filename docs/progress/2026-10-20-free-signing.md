# 2026-10-20: signing with free options only (M4 slice 4)

## Goal
The owner decided that signing uses only options that cost nothing. Update the plan and the release docs to
match, and do the free macOS part: an ad-hoc signature on the app, so macOS treats the download as an
unidentified app (allowed once in System Settings) rather than a damaged one.

## Done
- `docs/PLAN.md` M4: Developer ID and notarisation dropped; macOS gets an ad-hoc signature (slice 4); Windows
  Authenticode through SignPath Foundation (free for open-source projects) becomes slice 5, after the first
  release, because SignPath signs only projects that have already released.
- `docs/RELEASING.md`: new "Code signing: free options only" section with what users see on each system and
  SignPath Foundation's conditions (https://signpath.org/terms). Smart App Control behaviour checked against
  Microsoft's article KB5008908: a valid signature lets an unknown app run; there is no per-app override.
- `release.yml` (macOS): packages only the `app` format, signs `klefki-cli` and then the bundle ad hoc
  (`codesign --force --sign -`), and builds the `.dmg` with `hdiutil` (cargo-packager's `dmg` format would
  rebuild the app unsigned, `src/package/mod.rs`). The check now mounts the `.dmg` and checks the app inside.
- `scripts/check-release.py`: a macOS app must contain `Contents/_CodeSignature/CodeResources`, and on macOS
  pass `codesign --verify --deep --strict`.
- `docs/release-notes.md` and `README.md`: say why Windows and macOS warn; Smart App Control note for Windows.

## Checks run
- `check-release.py macos` on a synthetic universal app: without `_CodeSignature/CodeResources` it reports
  "not signed as a bundle" (exit 1); with it, OK (exit 0). The codesign part runs on macOS only.
- Workflow YAML parses.
- Trial release run 39371213528 on 67a9ad5 (manual, this branch): all three builds green. macOS: `codesign`
  signed both programs, `hdiutil` made the `.dmg`, the check mounted it and passed on the app inside
  ("check-release: macos 0.1.0 OK", so `codesign --verify --deep --strict` passed); disk image 15.7 MB zipped.
  The publish job attested and verified every file; signing skipped (not main).

## Not verified / open
- Gatekeeper's message for the ad-hoc signed download needs a person on a Mac.
- Cross-review (review 5580940735 on 38fd889, approved with three UX items): Smart App Control note moved above the
  Windows table (it blocks the installers too) with a way forward (follow releases, or the user's own choice to turn
  it off); macOS notes say a version downloaded by hand asks again. Not verifiable here: whether `klefki-cli`, run
  from Terminal through the symlink, is blocked on its first run. Try it on the first release; if it is, add one line
  saying to allow it in Privacy & Security (no `xattr` workaround).

## Next
Trial run, cross-review, merge. Then: first release (unsigned), then the owner applies to SignPath Foundation.
