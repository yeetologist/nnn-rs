#!/usr/bin/env python3
"""Byte-exact check of every recorded CLI case (ADR-012).

trycmd ignores trailing-newline differences and cannot wildcard the program path inside
binary cases. This runs the original and a build on each case in rust/app-cases (and the
binary cases moved to rust/argv0-cases) with argv[0] = "nnn", the same args, env and stdin,
each in a fresh empty directory, and compares stdout, stderr and exit code byte for byte.

  python3 rust/bytes-check.py [--env K=V ...] <build> [<original>]

--env sets a variable for the build only (e.g. RUST_ANYTHING_UNIT_TUI=rust).
"""

import glob, os, subprocess, sys, tempfile, tomllib

HERE = os.path.dirname(os.path.abspath(__file__))


EXTRA_ENV = {}


def run(binary, case, stdin):
    with tempfile.TemporaryDirectory() as cwd:
        env = dict(os.environ)
        env.update(EXTRA_ENV)
        env.update(case.get("env", {}).get("add", {}))
        try:
            p = subprocess.run(
                ["nnn", *case.get("args", [])],
                executable=binary,
                cwd=cwd,
                env=env,
                input=stdin,
                capture_output=True,
                timeout=10,
            )
        except subprocess.TimeoutExpired:
            return ("timeout",)
        return (p.returncode, p.stdout, p.stderr)


def main():
    args = sys.argv[1:]
    while args and args[0] == "--env":
        k, _, v = args[1].partition("=")
        EXTRA_ENV[k] = v
        args = args[2:]
    build = os.path.abspath(args[0])
    orig = os.path.abspath(
        args[1] if len(args) > 1 else os.path.join(HERE, "..", "reference", "nnn", "nnn")
    )
    tomls = sorted(glob.glob(os.path.join(HERE, "app-cases", "*.toml")))
    tomls += sorted(glob.glob(os.path.join(HERE, "argv0-cases", "removed", "*.toml")))
    passed = 0
    for t in tomls:
        case = tomllib.load(open(t, "rb"))
        base = t[: -len(".toml")]
        stdin = (
            open(base + ".stdin", "rb").read()
            if os.path.exists(base + ".stdin")
            else b""
        )
        a, b = run(orig, case, stdin), run(build, case, stdin)
        if a == b:
            passed += 1
        else:
            print(
                f"FAIL {os.path.basename(base)}: original={a!r:.200} build={b!r:.200}"
            )
    print(f"{passed}/{len(tomls)} cases byte-identical")
    sys.exit(0 if passed == len(tomls) else 1)


if __name__ == "__main__":
    main()
