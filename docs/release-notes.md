## Install

Klefki needs [Claude Code](https://code.claude.com/docs) installed. **These builds are not code-signed yet**, so Windows and macOS warn the first time you open Klefki; the steps below get you past that once.

### Windows 10 and 11

| File | Use it for |
|---|---|
| **`klefki-{version}-windows-x64-setup.exe`** | **Recommended.** Installs for your user only; no administrator rights needed. |
| `klefki-{version}-windows-x64.msi` | IT admins: managed installs through Group Policy or Intune. |
| `klefki-{version}-windows-x64.zip` | No install: unzip anywhere and run `klefki.exe`. |

If SmartScreen says "Windows protected your PC", choose **More info**, then **Run anyway**.

The command-line tool is installed next to the app, in `%LOCALAPPDATA%\Klefki` (the MSI uses `C:\Program Files\Klefki`). It is not on your `PATH`; run it in PowerShell with:

```
& "$env:LOCALAPPDATA\Klefki\klefki-cli.exe" list
```

### macOS 11 and later (Intel and Apple silicon)

Download **`klefki-{version}-macos-universal.dmg`**, open it and drag Klefki to Applications. Then, the first time:

1. Open Klefki. macOS says it "cannot be opened" or "cannot verify" it; click **Done** or **OK**.
2. Open **System Settings**, then **Privacy & Security**, and scroll down to **Security**.
3. Click **Open Anyway** next to the message about Klefki. The button is there for about an hour after step 1.
4. Confirm with your password or Touch ID. Klefki opens, and from then on it opens normally.

On macOS 11 to 14 you can instead right-click Klefki in Applications, choose **Open**, then **Open** again.

To use the command-line tool from a terminal:

```
sudo mkdir -p /usr/local/bin && sudo ln -s /Applications/Klefki.app/Contents/MacOS/klefki-cli /usr/local/bin/klefki-cli
```

### Linux (x86_64)

**Debian, Ubuntu and derivatives (recommended):** download `klefki-{version}-linux-amd64.deb`, then in the download folder:

```
sudo apt install ./klefki-{version}-linux-amd64.deb
```

This installs `klefki` (also in your app menu) and `klefki-cli`.

**Other distributions:** download `klefki-{version}-linux-x86_64.AppImage`, then:

```
chmod +x klefki-{version}-linux-x86_64.AppImage
./klefki-{version}-linux-x86_64.AppImage
```

AppImages need FUSE 2. If it does not start, install it: `sudo apt install libfuse2` on Ubuntu 22.04, `sudo apt install libfuse2t64` on 24.04 and later, or your distribution's `fuse2` / `fuse-libs` package.

### Check a download

Every file is listed in `SHA256SUMS`. In the download folder:

- Linux: `sha256sum -c SHA256SUMS --ignore-missing`
- macOS: `shasum -a 256 -c SHA256SUMS --ignore-missing`
- Windows (PowerShell): `Get-FileHash .\klefki-{version}-windows-x64-setup.exe`, then compare the hash with the line for that file in `SHA256SUMS`.

To check that a file was built from this repository by its release workflow, and not changed since, use the [GitHub CLI](https://cli.github.com/):

```
gh attestation verify klefki-{version}-linux-amd64.deb --repo DavySc/Klefki
```

`klefki-{version}.cdx.json` and `klefki-cli-{version}.cdx.json` list every library in each program (CycloneDX SBOMs), for vulnerability scanners.
