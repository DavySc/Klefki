## Install

Klefki needs [Claude Code](https://docs.anthropic.com/en/docs/claude-code) on your `PATH`.

| System | File | How |
|---|---|---|
| Windows 10/11 | `klefki-…-windows-x64-setup.exe` | Run it. Installs for your user only, no administrator rights needed. |
| Windows | `klefki-…-windows-x64.msi` | For managed installs (Group Policy, Intune). |
| Windows | `klefki-…-windows-x64.zip` | Portable: unzip anywhere and run `klefki.exe`. |
| macOS 11+ | `klefki-…-macos-universal.dmg` | Open it and drag Klefki to Applications. Runs natively on Intel and Apple silicon. |
| Debian, Ubuntu | `klefki-…-linux-amd64.deb` | `sudo apt install ./klefki-…-linux-amd64.deb` |
| Other Linux | `klefki-…-linux-x86_64.AppImage` | `chmod +x` it and run it. |

Every package also contains the command-line tool `klefki-cli` (on macOS: `Klefki.app/Contents/MacOS/klefki-cli`).

Check a download against `SHA256SUMS`: `sha256sum -c SHA256SUMS --ignore-missing` (on macOS `shasum -a 256 -c`).

**These builds are not code-signed yet.** Windows SmartScreen shows "Windows protected your PC": choose *More info*, then *Run anyway*. macOS says the app "cannot be opened because Apple cannot check it": right-click Klefki in Applications, choose *Open*, then *Open* again (on macOS 15, allow it under System Settings, Privacy & Security).
