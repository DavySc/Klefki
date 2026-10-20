# 2026-10-20: First release, v0.1.0 (M4)

## Goal
Publish Klefki 0.1.0, the first signed release, so installs can update themselves and Windows signing (SignPath
Foundation) can be applied for.

## Done
- Trial run on main (run 39386637747, commit 1730401): packages for all three systems built and checked; the owner
  approved the `sign` job in the `release` environment; it printed "Signature and comment signature verified"
  (trusted comment `Klefki SHA256SUMS, main, commit 1730401…`); the publish job checked the signature and attested
  all 11 files.
- Release notes and README say Klefki checks for and installs updates, what GitHub sees, and how to turn it off.

## Next
Merge this, then the owner tags the merge commit: `git tag v0.1.0 origin/main` and `git push origin v0.1.0`,
approves the `sign` job, checks the draft release and publishes it. Then apply to SignPath Foundation.
