#!/usr/bin/env bash
# Differential check of fileops.rs (W5): nnn.c's own entries_in_file, opstr and the
# P_CPMVFMT/P_CPMVRNM patterns against the Rust port, on generated inputs.
#   rust/oracle/fileops_run.sh <rust-oracle_fileops-binary> [seed] [calls]
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
n=${3:-5000}
cc -DNORL -DDIM_FILTERED -std=c11 -O1 -D_DEFAULT_SOURCE -D_XOPEN_SOURCE=600 -w \
  -o "$here/fileops_oracle" "$here/fileops_oracle.c" -lncursesw -ltinfo -lpthread || exit 2
work=$(mktemp -d /tmp/nnn-fileops-oracle.XXXXXX)
python3 - "$seed" "$n" > "$work/in" <<'EOF'
import random, sys
r = random.Random(int(sys.argv[1]))
alpha = [b'a', b'/', b'\n', b'\0', b"'", b' ', b'\\', b'|', b'#', b'\xc3\xb1', b'"', b'$']
def s(k):
    return b''.join(r.choice(alpha) for _ in range(r.randint(0, k)))
def nz(k):
    return s(k).replace(b'\0', b'').replace(b'\n', b'').hex()
for _ in range(int(sys.argv[2])):
    op = r.choice('EEOFN')
    if op == 'E':
        data = s(r.choice([0, 3, 40, 900]))
        print(f"E\t{r.choice([1, 2, 3, 7, 300, 20480])}\t{r.choice([0, 10])}\t{data.hex()}")
    elif op == 'O':
        print(f"O\t{nz(20)}\t{nz(30)}")
    elif op == 'F':
        print(f"F\t{nz(30)}")
    else:
        print("N\t" + "\t".join(nz(30) for _ in range(3)))
EOF
"$here/fileops_oracle" < "$work/in" > "$work/c.out" 2>&1
"$rs" < "$work/in" > "$work/rs.out" 2>&1
calls=$(wc -l < "$work/in")
if cmp -s "$work/c.out" "$work/rs.out"; then
  echo "ok   seed $seed: $calls calls identical"
  rm -rf "$work"
else
  echo "FAIL seed $seed: $(diff "$work/c.out" "$work/rs.out" | grep -c '^<') of $calls differ; kept $work"
  diff "$work/c.out" "$work/rs.out" | head -8
  exit 1
fi
