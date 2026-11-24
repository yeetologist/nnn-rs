#!/usr/bin/env python3
"""Differential check of browse.rs's pure functions (is_text_file, xextension,
init_fcolors) against nnn.c's: rust/oracle/browse_oracle vs oracle_browse.

    rust/oracle/browse_run.py <rust-oracle-binary> [seed] [calls]
"""

import os, random, shutil, subprocess, sys, tempfile

here = os.path.dirname(os.path.abspath(__file__))
rs = os.path.abspath(sys.argv[1])
seed = int(sys.argv[2]) if len(sys.argv) > 2 else 1
calls = int(sys.argv[3]) if len(sys.argv) > 3 else 3000
rnd = random.Random(seed)
c_bin = os.path.join(here, "browse_oracle")
subprocess.run(
    [
        "cc",
        "-DNORL",
        "-DDIM_FILTERED",
        "-std=c11",
        "-O1",
        "-D_DEFAULT_SOURCE",
        "-D_XOPEN_SOURCE=600",
        "-w",
        "-o",
        c_bin,
        os.path.join(here, "browse_oracle.c"),
        "-lncursesw",
        "-ltinfo",
        "-lpthread",
    ],
    check=True,
)

MAGICS = [
    b"%PDF",
    b"\x89PNG",
    b"GIF8",
    b"\xff\xd8\xff",
    b"PK\x03\x04",
    b"\x7fELF",
    b"\x1f\x8b",
    b"BM",
    b"RIFF",
    b"OggS",
    b"fLaC",
    b"ID3",
    b"\xfe\xed\xfa",
    b"\xfd7zXZ\x00",
    b"7z\xbc\xaf\x27\x1c",
    b"SQLite",
    b"\xff\xe0",
    b"\xff\x1f",
]
work = tempfile.mkdtemp(prefix="nnn-browse-oracle.")


def text_content():
    k = rnd.randrange(8)
    if k == 0:
        return b""
    if k == 1:
        m = rnd.choice(MAGICS)
        return (
            m[: rnd.randrange(1, len(m) + 1)] if rnd.random() < 0.3 else m
        ) + b"rest\n"
    alphabet = b"abc \n\r\t\x0c\x1b" + bytes([rnd.randrange(256)])
    n = rnd.choice([1, 2, 10, 511, 512, 513, 2000])
    data = bytes(rnd.choice(alphabet) for _ in range(n))
    if k == 2 and n > 1:
        at = rnd.randrange(n)
        data = (
            data[:at]
            + bytes([rnd.choice([0, 1, 7, 0x1F, 0x7F, 0x80])])
            + data[at + 1 :]
        )
    return data


lines = []
for i in range(calls):
    kind = rnd.choice(["text", "ext", "fcolors"])
    if kind == "text":
        p = os.path.join(work, f"f{i}").encode()
        if rnd.random() < 0.05:
            p += b".missing"
        else:
            with open(p, "wb") as f:
                f.write(text_content())
        arg = p
    elif kind == "ext":
        arg = bytes(rnd.choice(b"a.b.\xc3\xa9tgz") for _ in range(rnd.randrange(0, 12)))
    else:
        arg = bytes(
            rnd.choice(b"0123456789abcdefABCDEFxyz#;")
            for _ in range(rnd.randrange(0, 30))
        )
    lines.append(f"{kind}\t{arg.hex()}")
inp = ("\n".join(lines) + "\n").encode()
c = subprocess.run([c_bin], input=inp, capture_output=True).stdout.split(b"\n")
r = subprocess.run([rs], input=inp, capture_output=True).stdout.split(b"\n")
shutil.rmtree(work)
bad = [(l, a, b) for l, a, b in zip(lines, c, r) if a != b]
if len(c) != len(r):
    bad.append(("line count", len(c), len(r)))
for l, a, b in bad[:10]:
    print("DIFF", l, a, b)
print(f"seed {seed}: {calls} calls, {calls - len(bad)} identical, {len(bad)} different")
sys.exit(1 if bad else 0)
