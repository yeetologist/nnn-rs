#!/usr/bin/env python3
"""Calls for the prompt-history oracle (prompt_oracle.c / oracle_prompt.rs).
prompt_gen.py <seed> <calls> <fixture-dir>  -> ops on stdout, history files in the dir."""

import os, random, sys

seed, n, fx = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
rnd = random.Random(seed)
os.makedirs(fx, exist_ok=True)
POOL = [
    b"ls",
    b"ls -l",
    b"echo 'a b'",
    b"",
    b"caf\xc3\xa9",
    b"\xff\xfe bad utf8",
    b"x" * 300,
    b"tab\there",
    b"cr\r",
    b"nul\x00after",
    b" lead",
    b"trail ",
]


def word():
    r = rnd.random()
    if r < 0.5:
        return rnd.choice(POOL)
    if r < 0.8:
        return ("c%d" % rnd.randrange(40)).encode()
    return bytes(
        rnd.choice([0, 9, 13, 32, 39, 65, 97, 0xC3, 0xA9, 0xFF, 0x80])
        for _ in range(rnd.randrange(0, 12))
    )


files = []
for i in range(30):
    p = os.path.join(fx, "h%d" % i)
    lines = [
        word().replace(b"\n", b"") for _ in range(rnd.choice([0, 1, 3, 15, 16, 17, 25]))
    ]
    body = b"\n".join(lines)
    if lines and rnd.random() < 0.7:
        body += b"\n"
    if rnd.random() < 0.1:
        body += b"\n\n"
    with open(p, "wb") as f:
        f.write(body)
    files.append(p.encode())
files.append(os.path.join(fx, "missing").encode())
wpaths = [os.path.join(fx, "w%d" % i).encode() for i in range(5)] + [
    os.path.join(fx, "nodir", "w").encode()
]


def hx(b):
    return b.hex()


out = []
for _ in range(n):
    r = rnd.random()
    if r < 0.40:
        out.append("A\t" + hx(word()))
    elif r < 0.70:
        out.append("G\t" + rnd.choice("01"))
    elif r < 0.78:
        out.append("S")
    elif r < 0.82:
        out.append("X")
    elif r < 0.90:
        out.append("R\t" + hx(rnd.choice(files)))
    elif r < 0.96:
        out.append("W\t" + hx(rnd.choice(wpaths)))
    else:
        out.append(
            "C\t%d" % rnd.choice([ord("y"), ord("Y"), ord("n"), 27, 0, 121 + 256])
        )
print("\n".join(out))
