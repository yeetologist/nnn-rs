#!/usr/bin/env python3
"""Differential check of list mode: nnn.c's load_input (rust/oracle/listmode_oracle) against
the port (oracle_listmode). Each case feeds the same stdin (a file, so reads are
deterministic; one case uses a pipe) to both, then compares stdout (ret/listroot/listpath),
stderr (messages), the exit code and the temp tree created (paths, types, modes, symlink
targets), with the random mkdtemp name normalized.

    rust/oracle/listmode_run.py <rust-oracle-binary>
"""

import os, shutil, stat, subprocess, sys, tempfile

here = os.path.dirname(os.path.abspath(__file__))
rs = os.path.abspath(sys.argv[1])
c_bin = os.path.join(here, "listmode_oracle")
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
        os.path.join(here, "listmode_oracle.c"),
        "-lncursesw",
        "-ltinfo",
        "-lpthread",
    ],
    check=True,
)

work = tempfile.mkdtemp(prefix="nnn-lm-oracle.")
fx = os.path.join(work, "fx")
fxb = fx.encode()


def mk(rel, data=b""):
    p = os.path.join(fxb, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "wb") as f:
        f.write(data)


for r in [
    b"a",
    b"b",
    b"d/e/f",
    b"d/g",
    b"d/h i",
    "é/ü".encode(),
    b"\xff\xfe/x",
    b"x\ny",
    b"deep/1/2/3/4/5/f",
    b".hidden",
    b"~",
    b"sub/a",
    b"sub2/b",
]:
    mk(r)
os.symlink(b"nowhere", os.path.join(fxb, b"dang"))
os.symlink(b"d", os.path.join(fxb, b"dlink"))
os.makedirs(os.path.join(fxb, b"noperm/in"))
mk(b"noperm/in/f")
os.makedirs(os.path.join(fxb, b"many"))
for i in range(16400):
    open(os.path.join(fxb, b"many", b"%05d" % i), "wb").close()
os.chmod(os.path.join(fxb, b"noperm"), 0)

J = lambda *xs: b"\0".join(xs)
many = [b"many/%05d" % i for i in range(16400)]
cases = [
    ("relative", J(b"a", b"b", b""), ""),
    ("no trailing NUL", J(b"a", b"d/e/f"), ""),
    ("empty entries", b"\0\0a\0\0\0d/g\0\0", ""),
    ("dot and dotdot only", J(b".", b"..", b"."), ""),
    ("newline-led", J(b"\nfoo", b"a", b"\n"), ""),
    ("missing mixed", J(b"nope/x", b"a", b"zz"), ""),
    ("only missing", J(b"nope/x", b"nope/y"), ""),
    ("dangling link", J(b"dang", b"dlink", b"d/g"), ""),
    ("abs outside", J(fxb + b"/a", b"/etc/hostname", b"/etc/passwd"), ""),
    ("root", J(b"/"), ""),
    ("top level", J(b"/etc", b"/usr", b"/nonexistent"), ""),
    (
        "dot paths",
        J(b"../fx/a", b"d/../b", b"./d/e/f", b"d/./g", b"d//h i", b"d/e/"),
        "",
    ),
    ("tilde no home", J(b"a", b"~/b", b"d/g"), ""),
    ("tilde first", J(b"~/a"), ""),
    ("bare tilde file", J(b"~", b"a"), ""),
    ("overlong", J(b"a", b"x" * 5000, b"b"), ""),
    ("overlong dots", J(b"a/" + b"./" * 2100 + b"b"), ""),
    ("empty input", b"", ""),
    ("only NULs", b"\0" * 100, ""),
    (
        "unicode/invalid/newline",
        J("é/ü".encode(), b"\xff\xfe/x", b"x\ny", b"d/h i"),
        "",
    ),
    ("deep", J(b"deep/1/2/3/4/5/f", b"deep/1/2", b"a"), ""),
    ("parent then child", J(b"d", b"d/e/f", b"d/g"), ""),
    ("child then parent", J(b"d/e/f", b"d"), ""),
    ("hidden", J(b".hidden", b"sub/a", b"sub2/b"), ""),
    ("eacces", J(b"noperm/in/f", b"a"), ""),
    ("duplicates", J(b"a", b"a", b"./a"), ""),
    ("cwd arg", J(b"e/f", b"g", b"../a", b"/etc/hostname"), "d"),
    ("cwd arg abs entries", J(fxb + b"/d/g", fxb + b"/d/e/f"), "d"),
    ("cwd arg long", J(b"f"), "x" * 5000),
    ("16384 files", J(*many[:16384]) + b"\0", ""),
    ("16385 files", J(*many[:16385]) + b"\0", ""),
    ("16385 no trailing", J(*many[:16385]), ""),
    ("16384 + empties", J(*many[:16384]) + b"\0\0\0\0", ""),
    ("16384 no trailing", J(*many[:16384]), ""),
    ("20000 same", J(*([b"a"] * 20000)), ""),
    ("70MB NULs then file", b"\0" * (70 << 20) + b"a\0", ""),
    ("size limit long entries", J(*([b"a/" + b"./" * 2020 + b".."] * 16384)), ""),
    ("one big entry", b"y" * (3 << 20), ""),
]


def tree(top):
    out = []
    for root, dirs, files in os.walk(top.encode()):
        for n in sorted(dirs + files):
            p = os.path.join(root, n)
            st = os.lstat(p)
            rel = os.path.relpath(p, top.encode())
            if stat.S_ISLNK(st.st_mode):
                out.append((rel, "l", os.readlink(p)))
            else:
                out.append((rel, oct(st.st_mode)))
        dirs[:] = [d for d in dirs if not os.path.islink(os.path.join(root, d))]
    return sorted(out)


def run(binary, data, cwdarg, pipe):
    tmp = os.path.join(work, "t")
    os.makedirs(tmp, exist_ok=True)
    args = [binary, tmp] + ([os.path.join(fx, cwdarg)] if cwdarg else [])
    if pipe:
        p = subprocess.run(args, input=data, cwd=fx, capture_output=True)
    else:
        with open(os.path.join(work, "in"), "rb") as f:
            p = subprocess.run(args, stdin=f, cwd=fx, capture_output=True)
    fields = dict(l.split(" ", 1) for l in p.stdout.decode().splitlines())
    dec = {k: (None if v == "NULL" else bytes.fromhex(v)) for k, v in fields.items()}
    made = [n for n in os.listdir(tmp)]
    trees = {n: tree(os.path.join(tmp, n)) for n in made}
    topmodes = {n: oct(os.lstat(os.path.join(tmp, n)).st_mode) for n in made}
    norm = lambda b: (
        None
        if b is None
        else b.replace(os.path.join(tmp, made[0]).encode(), b"<TMP>")
        if made
        else b
    )
    res = (
        p.returncode,
        p.stderr,
        {k: norm(v) for k, v in dec.items()},
        sorted(topmodes.values()),
        [trees[n] for n in sorted(made)],
        len(made),
    )
    shutil.rmtree(tmp)
    return res


fail = 0
for i, (name, data, cwdarg) in enumerate(
    cases + [("pipe", J(b"a", b"d/e/f", b"b"), "")]
):
    pipe = name == "pipe"
    with open(os.path.join(work, "in"), "wb") as f:
        f.write(data)
    c = run(c_bin, data, cwdarg, pipe)
    r = run(rs, data, cwdarg, pipe)
    links = sum(1 for e in (c[4][0] if c[4] else []) if e[1] == "l")
    if c == r:
        print(
            f"ok   {name}: {len(data)} bytes, ret={'NULL' if c[2]['ret'] is None else 'dir'}, "
            f"{links} links, msg={c[1].decode().strip() or '-'}"
        )
    else:
        fail = 1
        print(f"FAIL {name}")
        for k, (a, b) in enumerate(zip(c, r)):
            if a != b:
                print("  field", k, "\n   c: ", str(a)[:600], "\n   rs:", str(b)[:600])

os.chmod(os.path.join(fxb, b"noperm"), 0o755)
shutil.rmtree(work)
print(f"{len(cases) + 1} cases, {'all identical' if not fail else 'DIFFERENCES'}")
sys.exit(fail)
