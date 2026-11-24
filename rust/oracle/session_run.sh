#!/usr/bin/env bash
# Differential check of rust/app/src/session.rs against nnn.c's save_session/load_session:
# the session files both write for the same contexts must be byte-identical, both must
# load the same files to the same state, and each must load the other's files.
#   rust/oracle/session_run.sh <oracle_session-binary> [seed] [saves] [loads]
set -u
here=$(cd "$(dirname "$0")" && pwd)
rs=$(realpath "$1")
seed=${2:-1}
nsave=${3:-300}
nload=${4:-300}
(cd "$here" && cc -DNORL -DDIM_FILTERED -std=c11 -O1 -D_DEFAULT_SOURCE -D_XOPEN_SOURCE=700 \
  -D_XOPEN_SOURCE_EXTENDED -w -o session_oracle session_oracle.c -lncursesw -ltinfo -lpthread) || exit 2
work=$(mktemp -d /tmp/nnn-ssnoracle.XXXXXX)
python3 "$here/session_gen.py" "$seed" "$nsave" "$nload" > "$work/in"
# The Rust side's curses needs a terminal on stdin: run it under script(1).
runrs() { LINES=24 COLUMNS=80 script -qec "$(printf '%q ' "$rs" "$1" "$2")" /dev/null > /dev/null 2>&1 < /dev/null; }
runc() { "$here/session_oracle" "$1" "$2" > /dev/null 2>&1 < /dev/null; }
fail=0
check() { # name a b
  if cmp -s "$2" "$3"; then
    echo "ok   $1: $(wc -l < "$2") commands identical ($(grep -c '^1 ' "$2" || true) loads succeeded)"
  else
    fail=1
    echo "FAIL $1: outputs differ; kept $work"
    diff "$2" "$3" | cut -c1-300 | head -10
  fi
}
runc "$work/in" "$work/c.out"
runrs "$work/in" "$work/rs.out"
check "save+load" "$work/c.out" "$work/rs.out"
# Cross loads: each side loads the files the other side saved.
mk() { grep -v '^NOFILE' | awk '{print "load\t0\t7331\t0\t0\t" $1}'; }
head -n "$nsave" "$work/c.out" | mk > "$work/from_c"
head -n "$nsave" "$work/rs.out" | mk > "$work/from_rs"
runrs "$work/from_c" "$work/x_rs.out"
runc "$work/from_rs" "$work/x_c.out"
check "cross (Rust loads C files vs C loads Rust files)" "$work/x_rs.out" "$work/x_c.out"
nok=$(grep -c '^1 ' "$work/x_rs.out" || true)
[ "$nok" = "$(wc -l < "$work/from_c")" ] || { fail=1; echo "FAIL cross: only $nok of $(wc -l < "$work/from_c") loads succeeded"; }
[ $fail = 0 ] && rm -rf "$work"
exit $fail
