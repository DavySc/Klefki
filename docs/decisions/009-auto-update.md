# 009: Klefki updates itself (2026-10-20)

## Context
The owner decided that Klefki updates itself ("We will be doing auto updating", 2026-10-20). That settles the open
question of an HTTP client in core: core may now reach the network, for this alone. Signing uses only free options
(owner, same day): releases carry a minisign signature over `SHA256SUMS` (M4 slice 3), the macOS app an ad-hoc
signature, Windows SignPath Foundation later. So Klefki's own key is the only thing that can vouch for an update.

**This changes the invariants in CLAUDE.md** (flagged for human review): `klefki-core` now makes https requests, to
GitHub only, and from slice 6b on will start an installer it downloaded.

## Decision
- **What is fetched.** `https://github.com/DavySc/Klefki/releases/latest/download/SHA256SUMS` and its `.minisig`,
  at most 64 KiB and 4 KiB. No GitHub API (no rate limit, no JSON to parse), no other host; GitHub's redirects must
  stay on https.
- **What is trusted.** Only a signature by the release key built into Klefki: `crates/core/build.rs` reads the
  second line of `minisign.pub` at the repository root into the binary. The version is the one in the signature's
  trusted comment (`Klefki SHA256SUMS, v1.2.3, commit …`, written by the release workflow), so a manual run on main
  (`main`), a pre-release, an altered checksum file or a signature by another key is refused, and an older release
  served again is never newer. Versions are `major.minor.patch`, compared as numbers.
- **No key, no check.** A build without `minisign.pub` (every build until the owner commits it, and builds from
  source) never fetches anything; Settings says it does not check for updates.
- **When.** Once a run, at least 3 seconds after the window opens (start-up never waits on it), and not within a day
  of the last answer. A failure is not an answer, so the next start tries again. "Check now" in Settings asks at any
  time. "Check for updates automatically" is on by default; off, Klefki contacts nobody unless asked.
- **What the user sees.** A newer release shows as a button in the top bar ("Update: 1.2.3") and in Settings ("1.2.3
  is available", What's new). Otherwise Settings says "Up to date · checked yesterday" (from the last answer, kept
  between runs), or "No signed release is published yet" when GitHub has none. Both open the release's page. The found version is remembered until a check finds none,
  and dropped once Klefki is that version or newer. No dialog, no badge count: it waits until the user wants it.
- **How.** ureq 3 (blocking, small) with rustls and the ring provider, trusting the system's certificates
  (`rustls-native-certs`, so a company proxy that the system trusts works), the proxy from the environment, a 30 s
  limit, and set up on first use rather than at start. Not cargo-packager's updater: it brings reqwest and tokio,
  its own signature files and a manifest server. Not `rustls-platform-verifier`: its licence (through
  `webpki-root-certs`, on WebAssembly only) is not in `deny.toml`, which is protected.
- The check runs on a thread of its own; the UI only sends `CheckForUpdates` and shows `Event::Update`.

## Who can sign (cross-review of #48)
The key is only as safe as the job that holds it. The `release` environment admits `v*` tags, and a tag can point at
any commit, with any `release.yml`; that workflow could sign anything and publish it, and every Klefki would offer it.
So, before the first signed release, the owner turns on a tag ruleset that lets only the owner create `v*` tags, and
a required reviewer (the owner) on the environment, so every signing waits for the owner's approval in the run. Both
are free on a public repository (`docs/RELEASING.md`). The `sign` job also refuses a commit that is not on main.

## If the key is lost or must be replaced
Every installed Klefki trusts one key, so a release signed with another is refused, and those installs stop updating.
- **Planned change** (the key is still there): the last release signed with the old key carries the new key in
  `minisign.pub`; after it, releases are signed with the new one. Installs move over by updating once in between.
- **Lost or leaked key:** installs cannot be moved over safely. The release notes and the README say to download the
  new version by hand once; a leaked key is also announced, and its releases withdrawn. Keep the key and its password
  backed up offline (`docs/RELEASING.md`).

## What GitHub sees
Each check is two downloads from GitHub: GitHub sees the user's IP address, the time, and Klefki's version in the
User-Agent, as with any download. Nothing about the user's servers or settings is sent. Settings says so next to the
switch.

## Next (slice 6b)
Download the right package, check it against the signed `SHA256SUMS`, and install it: the per-user Windows
installer silently, the macOS app and the AppImage by replacing them, each then restarted on request or when Klefki
closes. MSI, zip and `.deb` installs keep the notice: they are managed by an administrator or need sudo.
