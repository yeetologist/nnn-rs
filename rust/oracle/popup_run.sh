#!/usr/bin/env bash
# Differential check of rust/app/src/popup.rs against nnn.c's own functions.
#   rust/oracle/popup_run.sh <oracle_popup-binary> [seed] [calls-per-run]
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
n=${3:-3000}
(cd "$here" && cc -DNORL -DDIM_FILTERED -std=c11 -O1 -D_DEFAULT_SOURCE -D_XOPEN_SOURCE=700 \
  -D_XOPEN_SOURCE_EXTENDED -w -o popup_oracle popup_oracle.c -lncursesw -ltinfo -lpthread) || exit 2
export NNN_OPENER=xdg-open NNN_TRASH=1 NNN_SEL=/tmp/sel NNNLVL=1
unset NNN_HELP NNN_PLUG NNN_BMS NNN_COLORS NNN_FCOLORS NNN_PIPE NNN_MCLICK NNN_ARCHIVE NNN_ORDER
fail=0
for loc in C.UTF-8 en_US.UTF-8 de_DE.UTF-8; do
  work=$(mktemp -d /tmp/nnn-poracle.XXXXXX)
  python3 "$here/popup_gen.py" "$seed" "$n" > "$work/in"
  LC_ALL=$loc setsid -w "$here/popup_oracle" < "$work/in" > "$work/c.out" 2>/dev/null
  LC_ALL=$loc setsid -w "$rs" < "$work/in" > "$work/rs.out" 2>/dev/null
  calls=$(wc -l < "$work/in")
  if cmp -s "$work/c.out" "$work/rs.out"; then
    echo "ok   $loc: $calls calls identical"
    rm -rf "$work"
  else
    fail=1
    echo "FAIL $loc: outputs differ ($calls calls); kept $work"
    diff "$work/c.out" "$work/rs.out" | head -20
  fi
  seed=$((seed + 1))
done
exit $fail
