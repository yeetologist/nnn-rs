#!/usr/bin/env bash
# Differential check of the prompt history (W4, prompt.rs): nnn.c's own functions against the
# Rust port, on generated op sequences and history files.
#   rust/oracle/prompt_run.sh <rust-oracle_prompt-binary> [seed] [calls-per-run]
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
n=${3:-5000}
cc -DNORL -DDIM_FILTERED -std=c11 -O1 -D_DEFAULT_SOURCE -D_XOPEN_SOURCE=600 -w \
  -o "$here/prompt_oracle" "$here/prompt_oracle.c" -lncursesw -ltinfo -lpthread || exit 2
fail=0
for loc in C C.UTF-8 en_US.UTF-8; do
  for run in 1 2; do
    work=$(mktemp -d /tmp/nnn-prompt-oracle.XXXXXX)
    python3 "$here/prompt_gen.py" "$seed" "$n" "$work/fx" > "$work/in"
    (cd "$work" && LC_ALL=$loc "$here/prompt_oracle" < in > c.out 2>&1)
    rm -f "$work"/fx/w*
    (cd "$work" && LC_ALL=$loc "$rs" < in > rs.out 2>&1)
    calls=$(wc -l < "$work/in")
    if cmp -s "$work/c.out" "$work/rs.out"; then
      echo "ok   $loc seed $seed: $calls calls identical"
      rm -rf "$work"
    else
      fail=1
      echo "FAIL $loc seed $seed: $(diff "$work/c.out" "$work/rs.out" | grep -c '^<') of $calls differ; kept $work"
      diff "$work/c.out" "$work/rs.out" | head -8
    fi
    seed=$((seed + 1))
  done
done
exit $fail
