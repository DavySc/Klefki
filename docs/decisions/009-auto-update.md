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

## Installing (slice 6b)
- **Which installs update themselves.** Worked out at start from where Klefki runs (`crates/core/src/install.rs`):
  the per-user Windows setup (its `uninstall.exe` is next to `klefki.exe`, wherever the user installed it), a macOS
  `Klefki.app`, and an AppImage (the `APPIMAGE` variable its runtime sets, naming a file that exists). The MSI (an
  administrator's), the zip (unpacked by hand) and the `.deb` (apt, with sudo) keep the notice and the release page,
  with a sentence saying why.
- **What is downloaded.** Only the one file that install needs, from
  `https://github.com/DavySc/Klefki/releases/download/v<version>/`: `…-windows-x64-setup.exe`,
  `…-macos-universal.app.zip` (new: the signed app, zipped with `ditto` so its signature survives) or
  `…-linux-x86_64.AppImage`. Only a file named in the signed `SHA256SUMS` of the release the check accepted is asked
  for; it streams to an owner-only folder in Klefki's data folder (`updates/`), at most 256 MB, hashed as it is
  written. A file whose SHA-256 differs is deleted and never run.
- **When.** With "Check for updates automatically" on, a found release downloads in the background at once; off, on
  a click ("Update: 1.2.3" in the top bar, Download in Settings). A release found on an earlier day is looked up again
  at start, so it can be downloaded. The top bar then offers "Restart to update", disabled while a change is open
  (restarting would lose it). Otherwise the update installs when Klefki closes (`eframe::App::on_exit`).
- **How it is installed.**
  - Windows: the setup runs with `/P /R` (progress, no questions, start Klefki again) on restart, `/S` (silent) on
    close. The setup closes a running Klefki itself.
  - macOS: `/usr/bin/ditto -x -k` unpacks the app into a folder next to the old one; the old app is moved aside,
    the new one into its place (the old one is put back if that fails), then the old one deleted. Restart is
    `/usr/bin/open -n`.
  - AppImage: copied next to the old one, made executable, and renamed over it; restart runs the new file.
  - A folder Klefki cannot write (for example `/Applications` for a user who is not an administrator) leaves the old
    version as it was and says to download the new one by hand.
- **Not verified until the first signed release:** each install path on its real system (the tests replace bundles
  and AppImages in temporary folders, and the setup's flags are cargo-packager's documented ones), and whether macOS
  asks again for an app Klefki replaced itself (the download carries no quarantine flag, so it should not).
