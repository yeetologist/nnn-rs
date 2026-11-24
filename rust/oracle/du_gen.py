#!/usr/bin/env python3
"""Generates du-mode list calls (see oracle.c's list, flags d/a) over random directory trees.

du_gen.py <seed> <trees> <fixture-root>   prints calls to stdout, builds fixtures under the root

Hard links stay inside one directory: across directories the original's own result for an
entry depends on which worker thread sees the inode first.
"""

import os, random, sys

SIZES = [0, 1, 7, 511, 512, 4095, 4096, 4097, 70000, 300000]


def build(r, d, depth):
    os.makedirs(d, exist_ok=True)
    files, dirs = [], []
    for i in range(r.randint(0, 9)):
        n = (r.choice(["", "", "", "."]) + r.choice("abcXYZ") + str(i)).encode()
        p = os.path.join(d, n)
        k = r.random()
        try:
            if k < 0.45:
                with open(p, "wb") as f:
                    sz = r.choice(SIZES)
                    if r.random() < 0.03:
                        f.truncate(
                            2**41 + 12345
                        )  # sparse, past the 40-bit blocks field
                    elif r.random() < 0.2 and sz:
                        f.truncate(sz)  # sparse
                    else:
                        f.write(b"x" * sz)
                files.append(n)
            elif k < 0.65 and depth < 3:
                build(r, p, depth + 1)
                dirs.append(n)
            elif k < 0.72 and files:
                os.link(os.path.join(d, r.choice(files)), p)
            elif k < 0.78 and dirs:
                os.symlink(r.choice(dirs), p)
            elif k < 0.82 and files:
                os.symlink(r.choice(files), p)
            elif k < 0.85:
                os.symlink(b"/nonexistent/" + n, p)
            elif k < 0.88:
                os.symlink(b".", p)
            elif k < 0.91:
                os.mkfifo(p)
            elif k < 0.95:
                os.mkdir(p)
                dirs.append(n)
        except OSError:
            pass


def main():
    seed, n, root = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    r = random.Random(seed)
    out = []
    for i in range(n):
        d = os.path.join(root, f"t{i}").encode()
        build(r, d, 0)
        for flags in ["d", "a", "dH", "aH", "ds", "dr", "aHt", "dHv", "aHr"]:
            out.append("list\t" + d.hex() + "\t" + flags)
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
