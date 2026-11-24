#!/usr/bin/env python3
"""Commands for session_oracle.c / oracle_session.rs: random contexts to save, and session
files (valid, truncated, with a wrong version, oversized lengths, flipped bytes) to load.
    session_gen.py <seed> <saves> <loads>"""

import random
import struct
import sys

CTX_MAX, PATH_MAX, NAME_MAX, REGEX_MAX = 8, 4096, 255, 48
RESERVED = (1 << 12) | (1 << 20) | (3 << 22)
seed, nsave, nload = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
rnd = random.Random(seed)


def h(b):
    return b.hex() if b else "-"


def rstr(maxlen):
    n = rnd.choice([0, 1, 3, 8, 20, rnd.randint(0, maxlen), maxlen])
    n = min(n, maxlen)
    pool = rnd.choice([b"abcxyz/._- ", "çé日本/ab".encode(), bytes(range(1, 256))])
    return bytes(rnd.choice(pool) for _ in range(n)).replace(b"\t", b"t")


def bits(curctx=None, active=None):
    w = rnd.getrandbits(32) & ~RESERVED
    if (w >> 27) & 3 == 3:
        w &= ~(1 << 27)
    if curctx is not None:
        w = (w & ~(7 << 13)) | (curctx << 13)
    if active is not None:
        w = (w & ~(1 << 9)) | (int(active) << 9)
    return w


def state():
    cur = rnd.randrange(CTX_MAX)
    cfg = bits(cur)
    ctxs = []
    for i in range(CTX_MAX):
        active = i == cur or rnd.random() < 0.4
        fl = bytes(rnd.choice(b"/\\abc\0\xc3") for _ in range(REGEX_MAX))
        ctxs.append(
            (
                bits(rnd.randrange(8), active),
                rnd.randint(0, 2**32 - 1),
                rstr(NAME_MAX),
                rstr(PATH_MAX - 1),
                fl,
                b"/" + rstr(PATH_MAX - 2),
            )
        )
    return cfg, ctxs


def build(cfg, ctxs):
    """The bytes save_session writes (no hovered name)."""
    lens = [[0] * CTX_MAX for _ in range(4)]
    for i, (w, _, name, last, _, path) in enumerate(ctxs):
        if w & (1 << 9):
            lens[0][i] = len(path) + 1
            lens[1][i] = len(last) + 1
            lens[2][i] = len(name) + 1
            lens[3][i] = REGEX_MAX
    out = struct.pack("<Q", 1) + b"".join(struct.pack("<8Q", *l) for l in lens)
    out += struct.pack("<I", cfg)
    for i, (w, color, name, last, fl, path) in enumerate(ctxs):
        out += struct.pack("<II", w, color)
        if lens[2][i]:
            out += name + b"\0" + last + b"\0" + fl + path + b"\0"
    return out


def sname():
    return rnd.choice([b"s1", b"work", b"@", "日本".encode(), b"a.b-c_d"])


for _ in range(nsave):
    cfg, ctxs = state()
    nd = rnd.choice([0, 1])
    sn = rnd.choice([sname()] * 9 + [b"nodir/x"])
    f = ["save", "%x" % cfg, str(nd), h(rstr(NAME_MAX)), h(sn)]
    for w, color, name, last, fl, path in ctxs:
        f += ["%x" % w, str(color), h(name), h(last), fl.hex(), h(path)]
    print("\t".join(f))

for _ in range(nload):
    cfg, ctxs = state()
    data = bytearray(build(cfg, ctxs))
    kind = rnd.choice(["ok"] * 4 + ["trunc", "ver", "len", "flip", "none", "empty"])
    if kind == "trunc":
        data = data[: rnd.randrange(len(data))]
    elif kind == "ver":
        data[0:8] = struct.pack("<Q", rnd.choice([0, 2, 1 << 40]))
    elif kind == "len":
        arr = rnd.randrange(4)
        i = rnd.randrange(CTX_MAX)
        lim = [PATH_MAX, PATH_MAX, NAME_MAX + 1, REGEX_MAX][arr]
        v = rnd.choice([lim, lim + 1, 0, 1, 2**64 - 1, rnd.randrange(lim)])
        off = 8 + 64 * arr + 8 * i
        data[off : off + 8] = struct.pack("<Q", v)
    elif kind == "flip":
        for _ in range(rnd.randint(1, 4)):
            p = rnd.randrange(len(data))
            data[p] ^= 1 << rnd.randrange(8)
    elif kind == "empty":
        data = bytearray()
    restore = int(rnd.random() < 0.15)
    sn = "~" if restore and rnd.random() < 0.8 else h(sname())
    picker = int(rnd.random() < 0.2)
    payload = "NOFILE" if kind == "none" else (data.hex() or "-")
    print(
        "\t".join(
            [
                "load",
                "%x" % bits(rnd.randrange(8)),
                sn,
                str(restore),
                str(picker),
                payload,
            ]
        )
    )
