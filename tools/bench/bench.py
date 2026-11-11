#!/usr/bin/env python3
"""Repeatable memory/CPU/start-up benchmark: BrowseCraft vs Firefox.

Each run starts the browser with a fresh temporary profile and N URLs, then
measures, over the whole process tree (bwrap/WebKit helpers and Firefox
content processes included):

  * window_s   - launch until the first visible X11 window
  * title_s    - launch until the window title contains the first page's title
  * pss_mib    - total PSS (smaps_rollup) after the settle time
  * cpu_avg    - average CPU % (one core = 100) during the idle window
  * cpu_peak   - highest 1 s CPU sample from launch to the end
  * procs      - number of processes in the tree

Both browsers run as X11 clients (BrowseCraft always does; Firefox with
MOZ_ENABLE_WAYLAND=0) so windows can be found the same way.

Usage: bench.py --browser browsecraft|firefox --tabs N [--runs 3]
                [--bin PATH] [--containers] [--no-plugins] [--json out.json]
"""

import argparse
import json
import os
import shutil
import signal
import statistics
import subprocess
import tempfile
import time

# Public, stable pages of varied weight. The first one sets the title probe.
SITES = [
    ("https://en.wikipedia.org/wiki/Web_browser", "Web browser"),
    ("https://github.com/servo/servo", "servo"),
    ("https://developer.mozilla.org/en-US/docs/Web/JavaScript", "JavaScript"),
    ("https://react.dev/learn", "React"),
    ("https://vuejs.org/guide/introduction.html", "Vue"),
    ("https://www.rust-lang.org/", "Rust"),
    ("https://news.ycombinator.com/", "Hacker News"),
    ("https://docs.python.org/3/", "Python"),
    ("https://gitlab.com/gitlab-org/gitlab", "GitLab"),
    ("https://stackoverflow.com/questions", "Questions"),
    ("https://www.bbc.com/news", "BBC"),
    ("https://archlinux.org/", "Arch Linux"),
    ("https://kernel.org/", "Kernel"),
    ("https://www.w3.org/", "W3C"),
    ("https://crates.io/", "crates.io"),
    ("https://doc.rust-lang.org/book/", "Rust"),
    ("https://en.wikipedia.org/wiki/Linux", "Linux"),
    ("https://manjaro.org/", "Manjaro"),
    ("https://www.gnome.org/", "GNOME"),
    ("https://example.com/", "Example"),
]

SETTLE_S = 25     # after launch, before the idle measurement
IDLE_S = 20       # idle window for the average CPU
HZ = os.sysconf("SC_CLK_TCK")


def tree(root):
    """PIDs of root and all its descendants."""
    children = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
        children.setdefault(ppid, []).append(int(pid))
    out, todo = [], [root]
    while todo:
        p = todo.pop()
        out.append(p)
        todo.extend(children.get(p, []))
    return out


def pss_kib(pids):
    total = 0
    for p in pids:
        try:
            with open(f"/proc/{p}/smaps_rollup") as f:
                for line in f:
                    if line.startswith("Pss:"):
                        total += int(line.split()[1])
                        break
        except OSError:
            pass
    return total


def cpu_ticks(pids):
    total = 0
    for p in pids:
        try:
            with open(f"/proc/{p}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])  # utime + stime
        except (OSError, IndexError, ValueError):
            pass
    return total


def windows(pids):
    found = []
    for p in pids:
        r = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(p)],
                           capture_output=True, text=True)
        found += r.stdout.split()
    return found


def title_of(wid):
    r = subprocess.run(["xdotool", "getwindowname", wid], capture_output=True, text=True)
    return r.stdout.strip()


def firefox_profile(dir_):
    prefs = {
        "browser.aboutwelcome.enabled": False,
        "browser.startup.homepage_override.mstone": "ignore",
        "datareporting.policy.dataSubmissionEnabled": False,
        "toolkit.telemetry.reportingpolicy.firstRun": False,
        "browser.shell.checkDefaultBrowser": False,
        "browser.tabs.warnOnClose": False,
        "browser.sessionstore.resume_from_crash": False,
        "app.update.enabled": False,
    }
    with open(os.path.join(dir_, "user.js"), "w") as f:
        for k, v in prefs.items():
            f.write(f'user_pref("{k}", {json.dumps(v)});\n')


def one_run(args, urls, expect):
    home = tempfile.mkdtemp(prefix="bc-bench-")
    env = dict(os.environ)
    env["XDG_CONFIG_HOME"] = os.path.join(home, "config")
    env["XDG_DATA_HOME"] = os.path.join(home, "data")
    env["XDG_CACHE_HOME"] = os.path.join(home, "cache")
    if args.browser == "firefox":
        prof = os.path.join(home, "ff")
        os.makedirs(prof)
        firefox_profile(prof)
        env["MOZ_ENABLE_WAYLAND"] = "0"
        cmd = [args.bin or "firefox", "--new-instance", "--profile", prof] + urls
    else:
        if args.no_plugins:
            os.makedirs(env["XDG_CONFIG_HOME"] + "/BrowseCraft", exist_ok=True)
        cmd = [args.bin or "browsecraft"]
        for u in urls:
            if args.containers:
                cmd.append("--temporary-container")
            cmd.append(u)
    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, start_new_session=True)
    window_s = title_s = None
    peak = 0.0
    last_t, last_ticks = t0, 0
    try:
        while time.monotonic() - t0 < SETTLE_S:
            time.sleep(0.5)
            pids = tree(proc.pid)
            now = time.monotonic()
            ticks = cpu_ticks(pids)
            if now - last_t >= 1.0:
                peak = max(peak, (ticks - last_ticks) / HZ / (now - last_t) * 100)
                last_t, last_ticks = now, ticks
            wins = windows(pids)
            if wins and window_s is None:
                window_s = now - t0
            if wins and title_s is None and any(expect.lower() in title_of(w).lower() for w in wins):
                title_s = now - t0
        pids = tree(proc.pid)
        c0, s0 = cpu_ticks(pids), time.monotonic()
        samples = []
        while time.monotonic() - s0 < IDLE_S:
            time.sleep(1)
            pids = tree(proc.pid)
            samples.append(pss_kib(pids))
            now, ticks = time.monotonic(), cpu_ticks(pids)
            peak = max(peak, (ticks - last_ticks) / HZ / (now - last_t) * 100)
            last_t, last_ticks = now, ticks
        cpu_avg = (cpu_ticks(pids) - c0) / HZ / (time.monotonic() - s0) * 100
        return {
            "window_s": round(window_s, 2) if window_s else None,
            "title_s": round(title_s, 2) if title_s else None,
            "pss_mib": round(statistics.median(samples) / 1024, 1),
            "cpu_avg": round(cpu_avg, 1),
            "cpu_peak": round(peak, 1),
            "procs": len(pids),
        }
    finally:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(timeout=10)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        time.sleep(2)
        shutil.rmtree(home, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--browser", choices=["browsecraft", "firefox"], required=True)
    ap.add_argument("--tabs", type=int, default=1)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--bin")
    ap.add_argument("--containers", action="store_true", help="BrowseCraft: each tab in its own temporary container")
    ap.add_argument("--no-plugins", action="store_true")
    ap.add_argument("--json")
    args = ap.parse_args()
    urls = [u for u, _ in SITES[: args.tabs]]
    runs = [one_run(args, urls, SITES[0][1]) for _ in range(args.runs)]
    keys = runs[0].keys()
    med = {k: (statistics.median([r[k] for r in runs if r[k] is not None])
               if any(r[k] is not None for r in runs) else None) for k in keys}
    result = {"browser": args.browser, "tabs": args.tabs, "containers": args.containers,
              "no_plugins": args.no_plugins, "runs": runs, "median": med}
    print(json.dumps(result))
    if args.json:
        with open(args.json, "a") as f:
            f.write(json.dumps(result) + "\n")


if __name__ == "__main__":
    main()
