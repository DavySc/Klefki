#!/usr/bin/env python3
"""Start-up time and memory baseline for Klefki on Linux, so a refactor can be compared with main.

Runs the release `klefki` and `klefki-cli` against generated configurations in a temporary folder (never the real
~/.claude.json), with no Claude CLI on PATH, so only Klefki's own work is measured: no checks start. Prints a
Markdown table; `--json FILE` also writes the numbers.

    cargo build --release -p klefki -p klefki-cli
    scripts/perf-baseline.py --runs 7

What it measures, per configuration size:
  - cli list: wall time and peak RSS of `klefki-cli list` (reads and prints the whole configuration).
  - app window: time from start until Klefki's window is mapped (xdotool), under the X server in DISPLAY or a private
    Xvfb when there is none. The window appears before the configuration is read, so this does not include loading.
  - app first snapshot: time from start until the engine has sent the UI its first snapshot of the configuration
    (Klefki then writes the file named by KLEFKI_STARTUP_MARK). This is the one that includes loading.
  - app memory: RSS, peak RSS and private dirty memory (what Klefki itself holds, without shared libraries) two
    seconds after start, or one second after the window and the first snapshot if that is later, so the list has
    been drawn. The update check starts at three seconds, so this sample comes before it while both arrive within
    two seconds.

Each run has a fresh HOME, so Mesa's shader cache is always cold: the same every time, but slower than a user's second
start. Software OpenGL (Mesa llvmpipe) is most of the app's RSS on a machine without a GPU, so compare numbers from the
same machine only. Needs: python3, xdotool, Xvfb (when DISPLAY is unset).
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

SIZES = [("small", 70), ("large", 950)]


def config(rows):
    """A `.claude.json` with `rows` servers: a quarter for the user, the rest local, ten to a project."""
    user = rows // 4
    servers = {f"user-{i:04}": server(i) for i in range(user)}
    projects = {}
    for i in range(rows - user):
        project = projects.setdefault(f"/work/project-{i // 10:03}", {"mcpServers": {}, "disabledMcpServers": []})
        project["mcpServers"][f"local-{i:04}"] = server(i)
    return {"numStartups": 12, "mcpServers": servers, "projects": projects, "tipsHistory": {"x": 1}}


def server(i):
    if i % 2:
        return {"type": "http", "url": f"https://mcp-{i}.example.com/mcp", "headers": {"X-Team": "${TEAM}"}}
    return {"type": "stdio", "command": "npx", "args": ["-y", f"server-{i}-mcp", "--root", "/work"],
            "env": {"API_KEY": "${API_KEY}"}}


def environment(root, rows):
    cfg = os.path.join(root, "cfg")
    home = os.path.join(root, "home")
    os.makedirs(cfg)
    os.makedirs(home)
    with open(os.path.join(cfg, ".claude.json"), "w") as f:
        json.dump(config(rows), f, indent=2)
    env = {k: v for k, v in os.environ.items() if k in ("DISPLAY", "XDG_RUNTIME_DIR", "LANG")}
    env.update({
        "HOME": home,
        "CLAUDE_CONFIG_DIR": cfg,
        "KLEFKI_DATA_DIR": os.path.join(root, "data"),
        "KLEFKI_MANAGED_DIR": os.path.join(root, "org"),
        # No Claude CLI: nothing is checked, so only Klefki's own start-up is measured.
        "PATH": "/usr/bin:/bin",
    })
    return env


def cli_list(binary, env):
    start = time.perf_counter()
    proc = subprocess.Popen([binary, "list"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    _, status, usage = os.wait4(proc.pid, 0)
    elapsed = time.perf_counter() - start
    if status != 0:
        sys.exit(f"klefki-cli list failed: {proc.stderr.read().decode(errors='replace')}")
    return elapsed * 1000, usage.ru_maxrss / 1024


def memory(pid):
    fields = {}
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            key, _, value = line.partition(":")
            if key in ("VmRSS", "VmHWM"):
                fields[key] = int(value.split()[0]) / 1024
    with open(f"/proc/{pid}/smaps_rollup") as f:
        for line in f:
            key, _, value = line.partition(":")
            if key == "Private_Dirty":
                fields[key] = int(value.split()[0]) / 1024
    return fields["VmRSS"], fields["VmHWM"], fields["Private_Dirty"]


def app_start(binary, env, root, timeout=30):
    mark = os.path.join(root, "first-snapshot")
    start = time.perf_counter()
    log = os.path.join(root, "klefki.stderr")
    with open(log, "wb") as err:
        proc = subprocess.Popen([binary], env={**env, "KLEFKI_STARTUP_MARK": mark}, stdout=subprocess.DEVNULL,
                                stderr=err)
    try:
        window = loaded = None
        while window is None or loaded is None:
            now = (time.perf_counter() - start) * 1000
            if loaded is None and os.path.exists(mark):
                loaded = now
            if window is None:
                found = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                                       env=env, capture_output=True, text=True)
                if found.stdout.strip():
                    window = (time.perf_counter() - start) * 1000
            if proc.poll() is not None:
                sys.exit(f"klefki exited with {proc.returncode} before showing a window and its first snapshot:\n"
                         + open(log, errors="replace").read()[-2000:])
            if now > timeout * 1000:
                sys.exit("klefki showed no window" if window is None else
                         "klefki never reported its first snapshot (a build without KLEFKI_STARTUP_MARK?)")
            time.sleep(0.005)
        # Memory settles over the first two seconds (Mesa frees its start-up buffers), so sample at the same uptime
        # every run: two seconds, or one after the later of the two if that is later.
        sample_at = max(2, max(window, loaded) / 1000 + 1)
        time.sleep(max(sample_at - (time.perf_counter() - start), 0))
        rss, peak, private = memory(proc.pid)
        return window, loaded, rss, peak, private
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()


def summary(values):
    return {"median": statistics.median(values), "min": min(values), "max": max(values)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--bin", default="target/release", help="folder holding klefki and klefki-cli")
    parser.add_argument("--json", help="also write the numbers to this file")
    parser.add_argument("--no-app", action="store_true", help="only measure klefki-cli")
    args = parser.parse_args()

    app = os.path.join(args.bin, "klefki")
    cli = os.path.join(args.bin, "klefki-cli")
    for binary in [cli] + ([] if args.no_app else [app]):
        if not os.access(binary, os.X_OK):
            sys.exit(f"{binary} not found: run `cargo build --release -p klefki -p klefki-cli` first")

    xvfb = None
    if not args.no_app and "DISPLAY" not in os.environ:
        if not shutil.which("Xvfb"):
            sys.exit("no DISPLAY and no Xvfb: install Xvfb or pass --no-app")
        # Xvfb picks a free display and writes its number once it accepts connections.
        read, write = os.pipe()
        xvfb = subprocess.Popen(["Xvfb", "-displayfd", str(write), "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, pass_fds=(write,))
        os.close(write)
        with os.fdopen(read) as f:
            display = f.readline().strip()
        if not display:
            sys.exit("Xvfb did not start")
        os.environ["DISPLAY"] = f":{display}"

    results = {}
    try:
        for label, rows in SIZES:
            measured = {"cli_ms": [], "cli_rss_mb": [], "window_ms": [], "loaded_ms": [], "rss_mb": [],
                        "peak_mb": [], "private_dirty_mb": []}
            for _ in range(args.runs):
                with tempfile.TemporaryDirectory() as root:
                    env = environment(root, rows)
                    ms, rss = cli_list(cli, env)
                    measured["cli_ms"].append(ms)
                    measured["cli_rss_mb"].append(rss)
                    if not args.no_app:
                        window, loaded, rss, peak, private = app_start(app, env, root)
                        measured["window_ms"].append(window)
                        measured["loaded_ms"].append(loaded)
                        measured["rss_mb"].append(rss)
                        measured["peak_mb"].append(peak)
                        measured["private_dirty_mb"].append(private)
            results[f"{label} ({rows} rows)"] = {k: summary(v) for k, v in measured.items() if v}
    finally:
        if xvfb:
            xvfb.terminate()

    names = {"cli_ms": "cli list, ms", "cli_rss_mb": "cli list peak RSS, MB", "window_ms": "app window, ms",
             "loaded_ms": "app first snapshot, ms",
             "rss_mb": "app RSS, MB", "peak_mb": "app peak RSS, MB", "private_dirty_mb": "app private dirty, MB"}
    print(f"{args.runs} runs each; median (min to max)\n")
    print("| Measure | " + " | ".join(results) + " |")
    print("|---|" + "---|" * len(results))
    for key, name in names.items():
        cells = []
        for numbers in results.values():
            if key in numbers:
                n = numbers[key]
                cells.append(f"{n['median']:.1f} ({n['min']:.1f} to {n['max']:.1f})")
        if cells:
            print(f"| {name} | " + " | ".join(cells) + " |")
    if args.json:
        with open(args.json, "w") as f:
            json.dump({"runs": args.runs, "results": results}, f, indent=2)


if __name__ == "__main__":
    main()
