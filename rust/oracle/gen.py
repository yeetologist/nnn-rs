#!/usr/bin/env python3
"""Generates oracle calls (one per line, see oracle.c) and fixture directories.

gen.py <seed> <n> <fixture-root>   prints calls to stdout, builds fixtures under the root
"""

import os, random, sys

ALPHA = [
    "a",
    "b",
    "Z",
    "x",
    "0",
    "1",
    "2",
    "9",
    "10",
    "007",
    ".",
    "-",
    "_",
    " ",
    "+",
    "é",
    "É",
    "ß",
    "İ",
    "ı",
    "I",
    "i",
    "ç",
    "Ç",
    "ö",
    "Ö",
    "ğ",
    "日本",
    "ｱ",
    "👍",
    "👍🏽",
    chr(0x200D),
    chr(0x301),
    chr(9),
    chr(1),
    chr(0x7F),
    "~",
    "[",
    "(",
    "*",
    "Ⅷ",
    "ﬁ",
    "K",
    chr(0x212A),
]
RAW = [bytes([0xFF]), bytes([0xC3]), bytes([0xE2, 0x82])]
REGEX = [
    "a",
    "^a",
    "b$",
    "[0-9]+",
    "x|z",
    "(ab)*",
    "[[:alpha:]]",
    "[[:digit:]]{2}",
    ".",
    "é",
    "É",
    "BSBS.",
    "[a-",
    "(",
    "*a",
    "a{2,1}",
    "ß",
    "İ",
    "BSBSw",
    "[[:upper:]]",
    "i",
    "I",
    "",
]
REGEX = [x.replace("BSBS", chr(92)) for x in REGEX]
NL = chr(10)
TAB = chr(9)


def parts(r, lo=0, hi=8):
    out = []
    for _ in range(r.randint(lo, hi)):
        if r.random() < 0.04:
            out.append(r.choice(RAW))
        else:
            out.append(r.choice(ALPHA).encode())
    return out


def word(r, lo=0, hi=8):
    return b"".join(parts(r, lo, hi))


def filter_from(r, ps):
    """A filter cut from a name's characters: a run, a scattered subsequence, maybe case-flipped."""
    k = r.random()
    if k < 0.45:
        s = r.randint(0, len(ps))
        sel = ps[s : s + r.randint(1, 4)]
    elif k < 0.8:
        sel = [p for p in ps if r.random() < 0.5]
    else:
        sel = parts(r, 1, 3)
    f = b"".join(sel)
    if r.random() < 0.3:
        try:
            t = f.decode()
            f = (t.upper() if r.random() < 0.5 else t.lower()).encode()
        except UnicodeDecodeError:
            pass
    return f


def name(r):
    w = word(r, 1, 6)
    return w.replace(b"/", b"_").replace(bytes([0]), b"_") or b"x"


def build_fixture(r, root, idx):
    d = os.path.join(root, f"d{idx}").encode()
    os.makedirs(d, exist_ok=True)
    made = []
    for _ in range(r.randint(0, 14)):
        n = name(r)
        if n in (b".", b"..") or n in made:
            continue
        p = os.path.join(d, n)
        kind = r.random()
        try:
            if kind < 0.55:
                with open(p, "wb") as f:
                    f.write(b"x" * r.choice([0, 1, 7, 1023, 1024, 5000, 70000]))
            elif kind < 0.7:
                os.mkdir(p)
            elif kind < 0.8 and made:
                os.symlink(r.choice(made), p)
            elif kind < 0.85:
                os.symlink(b"/nonexistent/" + n, p)
            elif kind < 0.9 and made:
                os.link(os.path.join(d, r.choice(made)), p)
            elif kind < 0.95:
                os.mkfifo(p)
            else:
                os.symlink(b".", p)
        except OSError:
            continue
        made.append(n)
        t = r.choice([0, 86400, 1700000000, 1700000000, 1700000001, 2000000000])
        try:
            os.utime(
                p,
                ns=(
                    t * 10**9 + r.randint(0, 999999999),
                    t * 10**9 + r.choice([0, 5, 500000000]),
                ),
                follow_symlinks=False,
            )
        except OSError:
            pass
    return d


def main():
    seed, n, root = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    r = random.Random(seed)
    lines = []
    emit = lambda *f: lines.append(TAB.join(str(x) for x in f))
    hx = lambda b: b.hex() or "00"
    for _ in range(n):
        op = r.choice(
            [
                "stricmp",
                "verscmp",
                "fuzzy",
                "fscore",
                "strstr",
                "fpos",
                "spos",
                "regex",
                "coolsize",
                "perms",
                "unescape",
                "time",
                "sortflags",
            ]
        )
        if op in ("stricmp", "verscmp"):
            a = word(r)
            b = a if r.random() < 0.1 else (a[: r.randint(0, len(a))] + word(r, 0, 3))
            emit(op, hx(a), hx(b))
        elif op in ("fuzzy", "fscore", "strstr", "fpos", "spos", "regex"):
            ps = parts(r, 1, 10)
            fname = b"".join(ps)
            if op == "regex":
                flt = r.choice(REGEX).encode() + (
                    word(r, 0, 1) if r.random() < 0.3 else b""
                )
            else:
                flt = filter_from(r, ps)
            emit(op, r.randint(0, 1), hx(flt or b"a"), hx(fname))
        elif op == "coolsize":
            emit(
                op,
                r.choice(
                    [
                        0,
                        1,
                        1023,
                        1024,
                        1025,
                        1535,
                        1536,
                        1048575,
                        1048576,
                        1073741823,
                        1099511627775,
                        2**50 - 1,
                        2**60 - 1,
                        2**63 - 1,
                        r.randint(0, 2**63 - 1),
                        r.randint(0, 2**34),
                        r.randint(0, 2**21),
                        r.randint(0, 10**12),
                    ]
                ),
            )
        elif op == "perms":
            kind = r.choice(
                [
                    0o140000,
                    0o120000,
                    0o100000,
                    0o060000,
                    0o040000,
                    0o020000,
                    0o010000,
                    0,
                    0o170000,
                ]
            )
            emit(op, kind | r.randint(0, 0o7777))
        elif op == "unescape":
            emit(op, hx(word(r, 0, 12)), r.choice([0, 1, 2, 3, 5, 8, 20, 80]))
        elif op == "time":
            emit(
                op,
                r.choice(
                    [
                        0,
                        -1,
                        86399,
                        1700000000,
                        2**31 - 1,
                        2**31,
                        -(2**31),
                        r.randint(-(2**34), 2**34),
                    ]
                ),
            )
        else:
            emit(
                op,
                "".join(r.choice("cesrtvSETRVx^C") for _ in range(r.randint(0, 6)))
                or "-",
            )
    for i in range(max(1, n // 200)):
        d = build_fixture(r, root, i)
        for flags in [
            "x",
            "H",
            "Ht",
            "Hs",
            "He",
            "Hr",
            "Hv",
            "Htr",
            "HsrC",
            "HeC",
            "HtC",
            "Hvr",
            "e",
            "s",
        ]:
            emit("list", hx(d), flags)
    sys.stdout.write(NL.join(lines) + NL)


if __name__ == "__main__":
    main()
