#!/usr/bin/env bash
# Differential check of du mode (dentfill's blkorder path and the du workers): the C oracle
# against the Rust oracle on generated directory trees. Each input is run twice per side to
# also catch nondeterminism.
#   rust/oracle/du_run.sh <rust-oracle-binary> [seed] [trees]
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
n=${3:-40}
"$here/build.sh"
work=$(mktemp -d /tmp/nnn-du-oracle.XXXXXX)
python3 "$here/du_gen.py" "$seed" "$n" "$work/fx" > "$work/in"
calls=$(wc -l < "$work/in")
fail=0
for i in 1 2; do
  "$here/oracle" < "$work/in" > "$work/c$i.out"
  "$rs" < "$work/in" > "$work/rs$i.out"
done
cmp -s "$work/c1.out" "$work/c2.out" || echo "note: the C oracle itself differs between runs"
for i in 1 2; do
  if cmp -s "$work/c1.out" "$work/rs$i.out"; then
    echo "ok   run $i: $calls du list calls identical"
  else
    fail=1
    echo "FAIL run $i: $(diff "$work/c1.out" "$work/rs$i.out" | grep -c '^<') of $calls differ; kept $work"
  fi
done
[ $fail = 0 ] && rm -rf "$work"
exit $fail
