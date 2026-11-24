#!/usr/bin/env bash
# Differential check of the W3 functions: the C oracle (nnn.c's own functions) against the
# Rust oracle, on generated calls, under several locales and time zones.
#   rust/oracle/run.sh <rust-oracle-binary> [seed] [calls-per-run]
# LOCALES overrides the locale list: the portable backend (--features portable,oracles) reproduces
# glibc's C.UTF-8 only, so it is proven with
#   LOCALES=C.UTF-8 rust/oracle/run.sh <oracle built with --features portable,oracles>
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
n=${3:-20000}
"$here/build.sh"
fail=0
for loc in ${LOCALES:-C C.UTF-8 en_US.UTF-8 tr_TR.UTF-8 de_DE.UTF-8}; do
  for tz in UTC America/New_York Asia/Kolkata; do
    work=$(mktemp -d /tmp/nnn-oracle.XXXXXX)
    python3 "$here/gen.py" "$seed" "$n" "$work/fx" > "$work/in"
    LC_ALL=$loc TZ=$tz "$here/oracle" < "$work/in" > "$work/c.out"
    LC_ALL=$loc TZ=$tz "$rs" < "$work/in" > "$work/rs.out"
    calls=$(wc -l < "$work/in")
    if cmp -s "$work/c.out" "$work/rs.out"; then
      echo "ok   $loc $tz: $calls calls identical"
      rm -rf "$work"
    else
      fail=1
      echo "FAIL $loc $tz: $(diff "$work/c.out" "$work/rs.out" | grep -c '^<') of $calls differ; kept $work"
      python3 - "$work" <<'EOF'
import sys
w = sys.argv[1]
ins, cs, rs = (open(f"{w}/{f}", errors="replace").read().splitlines() for f in ("in", "c.out", "rs.out"))
shown = 0
for i, c, r in zip(ins, cs, rs):
    if c != r and shown < 8:
        print("  in:", i[:160]); print("  c: ", c[:160]); print("  rs:", r[:160]); shown += 1
EOF
    fi
    seed=$((seed + 1))
  done
done
exit $fail
