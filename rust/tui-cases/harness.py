#!/usr/bin/env python3
"""Pty screen harness for nnn (ADR-004 b).

Runs a build in an 80x24 tmux pane (a full terminal emulator, ADR-016) with a fresh
fixture tree, sends scripted keys, snapshots the screen text (capture-pane; no colours)
after each step once it stops changing, and records exit code and filesystem effects.

  harness.py record <build...>   write expected/<case>.json from the build
  harness.py check  <build...>   compare the build against expected/
  options: --case NAME (repeatable) --verbose
"""

import json, os, re, shlex, shutil, subprocess, sys, tempfile, time, hashlib, difflib

HERE = os.path.dirname(os.path.abspath(__file__))
COLS, ROWS = 80, 24
SPECIAL = {
    "<Enter>": "\r",
    "<Esc>": "\x1b",
    "<Tab>": "\t",
    "<BS>": "\x7f",
    "<Space>": " ",
    "<Up>": "\x1b[A",
    "<Down>": "\x1b[B",
    "<Right>": "\x1b[C",
    "<Left>": "\x1b[D",
    "<Home>": "\x1b[H",
    "<End>": "\x1b[F",
    "<PgUp>": "\x1b[5~",
    "<PgDn>": "\x1b[6~",
}
# Times other than the fixture's are "now": wildcard them.
GLOBAL_NORMALIZE = [[r"(?!2023-11-14 22:13)\d{4}-\d\d-\d\d \d\d:\d\d", "<NOW>"]]
FIXED_MTIME = 1700000000  # 2023-11-14 22:13:20 UTC


def key_bytes(tok):
    if tok in SPECIAL:
        return SPECIAL[tok].encode()
    if tok.startswith("<C-") and tok.endswith(">") and len(tok) == 5:
        return bytes([ord(tok[3].upper()) & 0x1F])
    return tok.encode()


TMUX_NAMES = {
    "<Enter>": "Enter",
    "<Esc>": "Escape",
    "<Tab>": "Tab",
    "<BS>": "BSpace",
    "<Space>": "Space",
    "<Up>": "Up",
    "<Down>": "Down",
    "<Right>": "Right",
    "<Left>": "Left",
    "<Home>": "Home",
    "<End>": "End",
    "<PgUp>": "PPage",
    "<PgDn>": "NPage",
}


def tmux_keys(tok):
    """send-keys arguments: named keys let tmux encode them for the pane's current mode."""
    if tok in TMUX_NAMES:
        return [TMUX_NAMES[tok]]
    if tok.startswith("<C-") and tok.endswith(">") and len(tok) == 5:
        return ["C-" + tok[3].lower()]
    return ["-l", tok]


def build_fixture(root, spec):
    for rel, content in sorted(spec.items()):
        p = os.path.join(root, rel)
        if content is None:
            os.makedirs(p, exist_ok=True)
        else:
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "w") as f:
                f.write(content)
    for dirpath, dirs, files in os.walk(root, topdown=False):
        for n in files + dirs:
            os.utime(
                os.path.join(dirpath, n),
                (FIXED_MTIME, FIXED_MTIME),
                follow_symlinks=False,
            )
    os.utime(root, (FIXED_MTIME, FIXED_MTIME))


def tree(root):
    out = []
    for dirpath, dirs, files in os.walk(root):
        dirs.sort()
        rel = os.path.relpath(dirpath, root)
        for n in sorted(files):
            p = os.path.join(dirpath, n)
            with open(p, "rb") as f:
                h = hashlib.sha1(f.read()).hexdigest()[:10]
            out.append(f"{os.path.normpath(os.path.join(rel, n))} {h}")
        for n in dirs:
            out.append(os.path.normpath(os.path.join(rel, n)) + "/")
    return sorted(out)


def nohash(lines, case):
    """Files whose bytes differ run to run (archives hold timestamps): compare by name only."""
    pats = case.get("nohash", [])
    out = []
    for line in lines:
        name = line.rsplit(" ", 1)[0] if not line.endswith("/") else line
        out.append(
            name + " <HASH>"
            if any(re.fullmatch(p, name) for p in pats) and not line.endswith("/")
            else line
        )
    return out


def tmux(sock, *args, check=True):
    return subprocess.run(
        ["tmux", "-L", sock, "-f", os.path.join(HERE, "tmux.conf"), *args],
        capture_output=True,
        check=check,
    ).stdout


def screen(sock, tmp, case):
    out = tmux(sock, "capture-pane", "-p", "-t", "h").decode("utf-8", "replace")
    lines = (out.split("\n") + [""] * ROWS)[:ROWS]
    lines = [l.rstrip().replace(tmp, "<TMP>") for l in lines]
    for pat, rep in GLOBAL_NORMALIZE + case.get("normalize", []):
        lines = [re.sub(pat, rep, l) for l in lines]
    return lines


def settle(sock, tmp, case, quiet=0.3, limit=5.0):
    """Wait until the screen stops changing for `quiet` seconds."""
    end = time.time() + limit
    last, since = None, time.time()
    while time.time() < end:
        cur = screen(sock, tmp, case)
        if cur != last:
            last, since = cur, time.time()
        elif time.time() - since >= quiet:
            break
        time.sleep(0.05)
    return last


def dead(sock):
    out = tmux(
        sock,
        "display-message",
        "-p",
        "-t",
        "h",
        "#{pane_dead} #{pane_dead_status}",
        check=False,
    )
    parts = out.decode().split()
    return (
        parts[0] == "1",
        int(parts[1]) if len(parts) > 1 and parts[1].lstrip("-").isdigit() else None,
    )


def run_case(build, case):
    tmp = tempfile.mkdtemp(prefix="nnnh-")
    sock = "nnnh-" + os.path.basename(tmp)
    try:
        home, work = os.path.join(tmp, "home"), os.path.join(tmp, "work")
        os.makedirs(home)
        build_fixture(work, case.get("fixture", {}))
        env = {
            "HOME": home,
            "PWD": work,
            "PATH": os.environ["PATH"],
            "TERM": "screen-256color",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "TZ": "UTC",
            "USER": "tester",
            "SHELL": "/bin/sh",
            "EDITOR": "true",
            "VISUAL": "true",
            "PAGER": "cat",
        }
        env.update(case.get("env", {}))
        args = [
            a.replace("{work}", work).replace("{home}", home)
            for a in case.get("args", [])
        ]
        cmd = " ".join(
            shlex.quote(x)
            for x in ["env", "-i", *[f"{k}={v}" for k, v in env.items()], *build, *args]
        )
        tmux(
            sock,
            "new-session",
            "-d",
            "-s",
            "h",
            "-x",
            str(COLS),
            "-y",
            str(ROWS),
            "-c",
            work,
            cmd,
        )
        snaps = [{"after": "start", "screen": settle(sock, tmp, case, quiet=0.4)}]
        for tok in case.get("keys", []):
            if dead(sock)[0]:
                break
            tmux(sock, "send-keys", "-t", "h", *tmux_keys(tok))
            snaps.append(
                {
                    "after": tok,
                    "screen": settle(sock, tmp, case, quiet=case.get("quiet", 0.3)),
                }
            )
        status = "killed"
        for _ in range(100):
            is_dead, code = dead(sock)
            if is_dead:
                status = code
                break
            time.sleep(0.05)
        files = {}
        for rel in case.get("read", []):
            p = os.path.join(tmp, rel)
            files[rel] = (
                open(p, encoding="utf-8", errors="replace").read().replace(tmp, "<TMP>")
                if os.path.exists(p)
                else None
            )
        return {
            "exit": status,
            "snaps": snaps,
            "work": nohash(tree(work), case),
            "files": files,
        }
    finally:
        tmux(sock, "kill-server", check=False)
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    argv = sys.argv[1:]
    mode, rest = argv[0], argv[1:]
    only, verbose, build = [], False, []
    i = 0
    while i < len(rest):
        if rest[i] == "--case":
            only.append(rest[i + 1])
            i += 2
        elif rest[i] == "--verbose":
            verbose = True
            i += 1
        else:
            build.append(rest[i])
            i += 1
    build[0] = os.path.abspath(build[0]) if os.sep in build[0] else build[0]
    cases = json.load(open(os.path.join(HERE, "cases.json")))
    exp_dir = os.path.join(HERE, "expected")
    os.makedirs(exp_dir, exist_ok=True)
    passed = total = 0
    for case in cases:
        if only and case["name"] not in only:
            continue
        total += 1
        got = run_case(build, case)
        path = os.path.join(exp_dir, case["name"] + ".json")
        if mode == "record":
            again = run_case(build, case)
            if again != got:
                print(f"NOISE {case['name']}: two runs of the build differ")
                continue
            json.dump(got, open(path, "w"), indent=1, ensure_ascii=False)
            passed += 1
            continue
        want = json.load(open(path))
        if got == want:
            passed += 1
            continue
        print(f"FAIL {case['name']}")
        a = json.dumps(want, indent=1, ensure_ascii=False).splitlines()
        b = json.dumps(got, indent=1, ensure_ascii=False).splitlines()
        diff = list(difflib.unified_diff(a, b, "expected", "actual", lineterm="", n=2))
        print("\n".join(diff if verbose else diff[:60]))
    print(f"{passed}/{total} tui cases {'recorded' if mode == 'record' else 'passed'}")
    sys.exit(0 if passed == total else 1)


if __name__ == "__main__":
    main()
