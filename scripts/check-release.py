#!/usr/bin/env python3
"""Checks the release files for one system before they are published.

    python3 scripts/check-release.py windows dist/ 0.1.0
    python3 scripts/check-release.py macos   dist/ 0.1.0 --app path/to/Klefki.app
    python3 scripts/check-release.py linux   dist/ 0.1.0

It looks inside the files rather than trusting the build: the Windows app is a GUI program with the icon
embedded and the CLI is a console program; the macOS app runs natively on Intel and Apple silicon and
carries the release version; the Linux packages install both programs, the menu entry and the icon. Exits 1
with every problem listed. Standard library only, so it runs on any CI runner.
"""

import argparse
import io
import plistlib
import struct
import sys
import tarfile
import zipfile
from pathlib import Path

PE_GUI, PE_CONSOLE = 2, 3
RT_GROUP_ICON = 14


def pe_info(data):
    """(subsystem, resource types) of a PE image, or raises ValueError."""
    if data[:2] != b"MZ":
        raise ValueError("not a Windows executable")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        raise ValueError("no PE header")
    sections = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    subsystem = struct.unpack_from("<H", data, opt + 68)[0]
    dirs = opt + (96 if magic == 0x10B else 112)
    res_rva = struct.unpack_from("<I", data, dirs + 2 * 8)[0]
    types = set()
    if res_rva:
        table = opt + opt_size
        for i in range(sections):
            s = table + 40 * i
            vsize, va, _, raw = struct.unpack_from("<IIII", data, s + 8)
            if va <= res_rva < va + max(vsize, 1):
                root = raw + res_rva - va
                named, ids = struct.unpack_from("<HH", data, root + 12)
                for e in range(named + ids):
                    ident = struct.unpack_from("<I", data, root + 16 + 8 * e)[0]
                    if not ident & 0x80000000:
                        types.add(ident)
    return subsystem, types


def check_windows(dist, version, problems):
    portable = dist / f"klefki-{version}-windows-x64.zip"
    for name in (f"klefki-{version}-windows-x64-setup.exe", f"klefki-{version}-windows-x64.msi"):
        if not (dist / name).is_file():
            problems.append(f"missing {name}")
    if not portable.is_file():
        problems.append(f"missing {portable.name}")
        return
    with zipfile.ZipFile(portable) as z:
        names = set(z.namelist())
        for exe, want in (("klefki.exe", PE_GUI), ("klefki-cli.exe", PE_CONSOLE)):
            if exe not in names:
                problems.append(f"{portable.name} has no {exe}")
                continue
            try:
                subsystem, types = pe_info(z.read(exe))
            except (ValueError, struct.error) as err:
                problems.append(f"{exe}: {err}")
                continue
            if subsystem != want:
                kind = "GUI" if want == PE_GUI else "console"
                problems.append(f"{exe} should be a {kind} program (subsystem {want}), is {subsystem}")
            if exe == "klefki.exe" and RT_GROUP_ICON not in types:
                problems.append("klefki.exe has no embedded icon")


def mach_o_archs(data):
    """CPU types in a Mach-O file: 7 is x86 (with the 64-bit flag 0x01000007), 12 is ARM (0x0100000c)."""
    magic = struct.unpack_from(">I", data, 0)[0]
    if magic == 0xCAFEBABE:
        count = struct.unpack_from(">I", data, 4)[0]
        return {struct.unpack_from(">I", data, 8 + 20 * i)[0] for i in range(count)}
    if magic in (0xCFFAEDFE, 0xCEFAEDFE):
        return {struct.unpack_from("<I", data, 4)[0]}
    raise ValueError("not a Mach-O file")


def check_macos(dist, version, app, problems):
    dmg = dist / f"klefki-{version}-macos-universal.dmg"
    if not dmg.is_file():
        problems.append(f"missing {dmg.name}")
    if app is None:
        problems.append("pass --app with the Klefki.app that went into the disk image")
        return
    plist_path = app / "Contents" / "Info.plist"
    if not plist_path.is_file():
        problems.append(f"{app} has no Info.plist")
        return
    with plist_path.open("rb") as f:
        info = plistlib.load(f)
    if info.get("CFBundleShortVersionString") != version:
        problems.append(f"Info.plist version is {info.get('CFBundleShortVersionString')!r}, not {version}")
    for exe in ("klefki", "klefki-cli"):
        path = app / "Contents" / "MacOS" / exe
        if not path.is_file():
            problems.append(f"{app.name} has no Contents/MacOS/{exe}")
            continue
        try:
            archs = mach_o_archs(path.read_bytes())
        except (ValueError, struct.error) as err:
            problems.append(f"{exe}: {err}")
            continue
        if not {0x01000007, 0x0100000C} <= archs:
            problems.append(f"{exe} is not universal (Intel and Apple silicon): {sorted(map(hex, archs))}")


def deb_files(data):
    """Paths inside a .deb (an ar archive holding data.tar.*)."""
    if data[:8] != b"!<arch>\n":
        raise ValueError("not a Debian package")
    pos = 8
    while pos + 60 <= len(data):
        name = data[pos : pos + 16].decode().strip().rstrip("/")
        size = int(data[pos + 48 : pos + 58].decode().strip())
        body = data[pos + 60 : pos + 60 + size]
        if name.startswith("data.tar"):
            with tarfile.open(fileobj=io.BytesIO(body)) as t:
                return {m.name.lstrip("./") for m in t.getmembers()}
        pos += 60 + size + (size & 1)
    raise ValueError("no data.tar in the package")


def check_linux(dist, version, problems):
    deb = dist / f"klefki-{version}-linux-amd64.deb"
    appimage = dist / f"klefki-{version}-linux-x86_64.AppImage"
    if deb.is_file():
        try:
            files = deb_files(deb.read_bytes())
        except (ValueError, tarfile.TarError) as err:
            problems.append(f"{deb.name}: {err}")
            files = None
        if files is not None:
            for want in ("usr/bin/klefki", "usr/bin/klefki-cli"):
                if want not in files:
                    problems.append(f"{deb.name} does not install {want}")
            if not any(f.startswith("usr/share/applications/") and f.endswith(".desktop") for f in files):
                problems.append(f"{deb.name} has no menu entry (.desktop file)")
            if not any(f.startswith("usr/share/icons/") for f in files):
                problems.append(f"{deb.name} has no icon")
    else:
        problems.append(f"missing {deb.name}")
    if appimage.is_file():
        if appimage.read_bytes()[:4] != b"\x7fELF":
            problems.append(f"{appimage.name} is not an executable")
    else:
        problems.append(f"missing {appimage.name}")


def main(argv):
    parser = argparse.ArgumentParser(description="Checks the release files for one system.")
    parser.add_argument("system", choices=("windows", "macos", "linux"))
    parser.add_argument("dist", type=Path, help="the folder holding the renamed release files")
    parser.add_argument("version", help="the workspace version, such as 0.1.0")
    parser.add_argument("--app", type=Path, help="macOS: the Klefki.app that went into the disk image")
    args = parser.parse_args(argv)
    problems = []
    if args.system == "windows":
        check_windows(args.dist, args.version, problems)
    elif args.system == "macos":
        check_macos(args.dist, args.version, args.app, problems)
    else:
        check_linux(args.dist, args.version, problems)
    for p in problems:
        print(f"check-release: {p}", file=sys.stderr)
    if not problems:
        print(f"check-release: {args.system} {args.version} OK")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
