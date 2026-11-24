#!/usr/bin/env python3
"""Calls for the popup oracle (popup_oracle.c / oracle_popup.rs).
popup_gen.py <seed> <calls>"""

import os
import random
import sys

seed, n = int(sys.argv[1]), int(sys.argv[2])
R = random.Random(seed)
# POPUP_ASCII=1: window contents in ASCII only (no multibyte cut by the byte limit)
ASCII = os.environ.get("POPUP_ASCII") == "1"


def hx(s):
    b = s.encode() if isinstance(s, str) else s
    return b.hex() if b else "-"


WORDS = [
    "a",
    "sh",
    "-c",
    "ls",
    "printf",
    "%s",
    "x",
    "--",
    "é",
    "ñandú",
    "漢字",
    "file.txt",
    "1",
    "*",
]
BLANKS = [" ", "  ", "\t", " \t", "   "]


def cmdline():
    parts = []
    for _ in range(R.randint(0, 12)):
        if R.random() < 0.2:
            parts.append(R.choice(BLANKS))
        parts.append(R.choice(WORDS))
        parts.append(R.choice(BLANKS) if R.random() < 0.3 else " ")
    s = "".join(parts)
    return s if R.random() < 0.7 else s.strip()


def content():
    toks = [
        "word",
        " ",
        "  ",
        "\t",
        "é",
        "ñandú",
        "漢字",
        "\x01",
        "0123456789",
        "Size: 60",
        "IO Block: 4096",
        "x" * 40,
        "-",
        "/tmp/a/b/c",
    ]
    if ASCII:
        toks = [t for t in toks if t.isascii()]
    lines = []
    for _ in range(R.randint(0, 40)):
        lines.append("".join(R.choice(toks) for _ in range(R.randint(0, 25))))
    s = "\n".join(lines)
    if lines and R.random() < 0.6:
        s += "\n"
    b = s.encode()
    if b and R.random() < 0.05:
        i = R.randrange(len(b))
        b = b[:i] + b"\0" + b[i:]
    return b


KEYS = [
    0o403,
    0o402,
    0o522,
    0o523,
    0o406,
    0o550,
    0o404,
    0o405,
    ord("x"),
    ord("j"),
    ord("k"),
]
RARE = [ord("q"), 0o632]  # ESC is left out: ncurses waits for the rest of a sequence

ops = []
for _ in range(n):
    op = R.choice(
        ["parseargs"] * 3
        + ["mkpath"] * 2
        + ["float"] * 6
        + ["bufout", "getout", "help", "fsinfo"]
    )
    if op == "parseargs":
        ops.append(f"parseargs\t{hx(cmdline())}")
    elif op == "mkpath":
        d = R.choice(["/", "/a/b", "", "/a", "/tmp/x y", "/é"])
        nm = R.choice(["x", "~/y", "/abs", "~", "~x", "", "a/b", "ñ"])
        ops.append(f"mkpath\t{hx(d)}\t{hx(nm)}")
    elif op == "bufout":
        c1 = R.choice(
            [
                "printf a%sb\\n",
                "echo",
                "printf",
                "true",
                "false",
                "nonexistent-xyz",
                "",
                "printf %s",
            ]
        )
        c2 = R.choice(["echo second", "printf b", "true", "", "nonexistent-xyz"])
        arg = R.choice(["", "x y", "-n", "é", "1"])
        ops.append(f"bufout\t{hx(c1)}\t{hx(c2)}\t{hx(arg)}")
    elif op == "getout":
        c = R.choice(
            [
                "echo",
                "printf %s",
                "printf",
                "true",
                "nonexistent-xyz",
                "echo a  b",
                "",
                "printf a\\nb",
            ]
        )
        arg = R.choice(["", "x", "é", "-n"])
        ops.append(f"getout\t{hx(c)}\t{hx(arg)}")
    elif op == "fsinfo":
        p, t = R.choice(
            [("/", 2), ("/proc", 0), ("/proc", 1), ("/proc", 2), ("/nonexistent", 0)]
        )
        ops.append(f"fsinfo\t{hx(p)}\t{t}")
    elif op == "help":
        paths = ",".join(
            R.choice(["2f", "2f746d70", "2f612f62", "2fc3a9"]) for _ in range(8)
        )
        ops.append(
            "help\t%d\t%d\t%s\t%s\t%s"
            % (
                R.randrange(8),
                R.randrange(256),
                hx(R.choice(["/proc", "/nonexistent"])),
                paths,
                hx(R.choice(["", "work", "@", "é"])),
            )
        )
    else:
        lines = R.choice([R.randint(3, 45), 24])
        cols = R.choice([R.randint(8, 130), 80])
        nd = R.randint(0, 3)
        cur = R.randrange(nd) if nd else 0
        name = R.choice(["dir1", "ñandú.txt", "漢字", "x" * 120, "a b"])
        keys = [R.choice(KEYS) for _ in range(R.randint(0, 12))]
        if R.random() < 0.1:
            keys.insert(R.randrange(len(keys) + 1), R.choice(RARE))
        ops.append(
            "float\t%d\t%d\t%d\t%d\t%d\t%s\t%s\t%s"
            % (
                lines,
                cols,
                R.randint(0, 1),
                nd,
                cur,
                hx(name),
                hx(content()),
                ",".join(map(str, keys)) or "-",
            )
        )
print("\n".join(ops))
