#!/usr/bin/env bash
# Differential check of the W4 config/selection port: the C oracle (nnn.c's own
# parsekvpair, get_kv_val, get_kv_key, mkpath, xdirname, xbasename, common_prefix, xmktree,
# write_lastdir) against the Rust oracle, on generated calls; xmktree/write_lastdir results
# are also compared as the trees they leave on disk.
#   rust/oracle/run_config.sh <rust-oracle_config-binary> [seed] [calls-per-run] [runs]
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
n=${3:-20000}
runs=${4:-6}
(cd "$here" && cc -DNORL -DDIM_FILTERED -std=c11 -O1 -D_DEFAULT_SOURCE -D_XOPEN_SOURCE=600 -w \
  -o config_oracle config_oracle.c -lncursesw -ltinfo -lpthread) || exit 2
tree() { (cd "$1" && find . -mindepth 1 -printf '%y %m %p\n' | LC_ALL=C sort && find . -type f -exec md5sum {} + | LC_ALL=C sort -k2); }
hexof() { printf '%s' "$1" | xxd -p | tr -d '\n'; }
fail=0
for i in $(seq 1 "$runs"); do
  for loc in C C.UTF-8; do
    work=$(mktemp -d /tmp/nnn-cfg-oracle.XXXXXX)
    mkdir -p "$work/c/w/w" "$work/rs/w/w"
    python3 "$here/gen_config.py" "$seed" "$n" > "$work/in"
    (cd "$work" && LC_ALL=$loc ORACLE_ROOT="$work/c/w/w" "$here/config_oracle" < in > c.out 2> c.err)
    (cd "$work" && LC_ALL=$loc ORACLE_ROOT="$work/rs/w/w" "$rs" < in > rs.out 2> rs.err)
    sed -i "s|$(hexof "$work/c/w/w")|ROOT|g" "$work/c.out"
    sed -i "s|$(hexof "$work/rs/w/w")|ROOT|g" "$work/rs.out"
    tree "$work/c" > "$work/c.tree"; tree "$work/rs" > "$work/rs.tree"
    calls=$(wc -l < "$work/in")
    if cmp -s "$work/c.out" "$work/rs.out" && cmp -s "$work/c.tree" "$work/rs.tree"; then
      echo "ok   seed $seed $loc: $calls calls identical, trees identical ($(wc -l < "$work/c.tree") lines)"
      rm -rf "$work"
    else
      fail=1
      echo "FAIL seed $seed $loc: $(diff "$work/c.out" "$work/rs.out" | grep -c '^<') of $calls differ; trees $(cmp -s "$work/c.tree" "$work/rs.tree" && echo same || echo differ); kept $work"
      python3 - "$work" <<'EOF'
import sys
w = sys.argv[1]
ins, cs, rs = (open(f"{w}/{f}", errors="replace").read().splitlines() for f in ("in", "c.out", "rs.out"))
shown = 0
for i, c, r in zip(ins, cs, rs):
    if c != r and shown < 8:
        print("  in:", i[:200]); print("  c: ", c[:200]); print("  rs:", r[:200]); shown += 1
EOF
    fi
  done
  seed=$((seed + 1))
done
exit $fail
