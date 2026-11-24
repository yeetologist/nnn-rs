#!/usr/bin/env python3
"""Generates calls for the config/selection oracle (see config_oracle.c).

gen_config.py <seed> <n>   prints calls to stdout
"""

import random
import sys

TAB = chr(9)


def hx(b):
    return b.hex() if b else "-"


def comp(r):
    return r.choice(
        [
            b"a",
            b"b",
            b"c",
            b"ab",
            b"x y",
            b"'",
            b"it's",
            b"\xc3\xa9",
            b"\xff",
            b"..",
            b".",
            b"",
            b"~",
            b"-",
            b";",
            b":",
        ]
    )


def apath(r, lo=0, hi=6, absolute=True):
    p = b"/".join(comp(r) for _ in range(r.randint(lo, hi)))
    if absolute:
        p = b"/" + p
    if r.random() < 0.1:
        p += b"/"
    return p


def long(r, n):
    seg = b"/" + bytes(r.choice(b"abcdef'") for _ in range(r.randint(1, 40)))
    out = b""
    while len(out) < n:
        out += seg
    return out[:n]


KEYS = b"abcz0;:,~\xc3\xff "


def kvenv(r):
    k = r.random()
    if k < 0.1:
        return b""
    if k < 0.15:
        # more than 100 pairs
        return b";".join(
            bytes([KEYS[i % len(KEYS)]]) + b":v" + str(i).encode()
            for i in range(r.randint(95, 130))
        )
    items = []
    for _ in range(r.randint(1, 6)):
        key = bytes([r.choice(KEYS)])
        val = r.choice(
            [
                apath(r, 0, 3),
                apath(r, 0, 3, False),
                b"~",
                b"~/" + apath(r, 0, 2, False),
                b"~x",
                comp(r),
            ]
        )
        sep = r.choice([b":", b":", b":", b"", b"::"])
        items.append(key + sep + val)
    s = b";".join(items)
    if r.random() < 0.2:
        s += r.choice([b";", b";;", b":", b"x"])
    return s


def keyof(r, env):
    if env and r.random() < 0.8:
        return r.choice(env)
    return r.choice(KEYS)


def rel(r):
    # relative paths under the oracle root, never above root/.. (root is <work>/c/w/w)
    parts = []
    ups = 0
    for _ in range(r.randint(1, 5)):
        c = r.choice([b"a", b"b", b"c", b"d", b"", b".", b"..", b"e f"])
        if c == b"..":
            if ups >= 1:
                c = b"a"
            ups += 1
        parts.append(c)
    p = b"/".join(parts)
    if r.random() < 0.15:
        p += b"/"
    return p or b"a"


def call(r):
    k = r.random()
    if k < 0.12:
        return ["kv", hx(kvenv(r))]
    if k < 0.26:
        env = kvenv(r)
        home = r.choice(
            [b"/home/u", b"/", b"", b"/h/" + comp(r), b"rel", long(r, 4090)]
        )
        return ["kvval", hx(env), str(keyof(r, env)), r.choice("BP"), hx(home)]
    if k < 0.32:
        env = kvenv(r)
        if r.random() < 0.3 or b":" not in env:
            val = r.choice([b"", b"t", b"/x"])
        else:
            val = env.split(b":")[1].split(b";")[0]
        return ["kvkey", hx(env), hx(val)]
    if k < 0.48:
        dirk = r.random()
        if dirk < 0.1:
            d = b"/"
        elif dirk < 0.25:
            d = long(r, r.choice([4090, 4093, 4094, 4095, 4096, 5000]))
        else:
            d = apath(r)
        nk = r.random()
        if nk < 0.3:
            n = b"~/" + apath(r, 0, 3, False)
        elif nk < 0.4:
            n = apath(r)
        elif nk < 0.45:
            n = r.choice([b"~", b"~x", b""])
        elif nk < 0.5:
            n = long(r, r.choice([10, 4000, 4100]))
        else:
            n = apath(r, 1, 3, False)
        home = r.choice(
            [b"/home/u", b"", b"/", long(r, r.choice([100, 4094, 4095, 4096, 4200]))]
        )
        return ["mkpath", hx(d), hx(n), hx(home)]
    if k < 0.58:
        p = apath(r, 0, 5, r.random() < 0.8)
        if b"/" not in p:
            p = b"x/" + p
        return [r.choice(["dirname", "basename"]), hx(p)]
    if k < 0.61:
        return ["basename", hx(apath(r, 1, 3, False).replace(b"/", b""))]
    if k < 0.75:
        path = r.choice([b""] + [apath(r, 0, 5) for _ in range(4)])
        if r.random() < 0.4 and path:
            # a prefix sharing part of path
            cut = r.randint(1, len(path))
            pre = path[:cut] + r.choice([b"", b"/q", b"z", b"/" + comp(r)])
        else:
            pre = r.choice([b"", apath(r, 0, 5), b"/"])
        if pre and not pre.startswith(b"/"):
            pre = b"/" + pre
        if r.random() < 0.05:
            path = long(r, 5000)
        return ["cprefix", hx(path), hx(pre)]
    if k < 0.9:
        return ["mktree", hx(rel(r)), r.choice("01"), r.choice("01")]
    cur = r.choice(
        [apath(r), long(r, r.choice([1000, 1530, 1534, 1535, 1536, 4600, 4602, 4603]))]
    )
    out = r.choice([b"-", b"-", rel(r), b"~/" + rel(r), b"~", b"o/out"])
    cfg = r.choice([b"cfg", b"a", b"nocfg/x"])
    home = r.choice([b"h", b"a", b"b"])
    return ["lastdir", hx(cur), "-" if out == b"-" else hx(out), hx(cfg), hx(home)]


def main():
    seed, n = int(sys.argv[1]), int(sys.argv[2])
    r = random.Random(seed)
    for _ in range(n):
        print(TAB.join(call(r)))


main()
