#!/usr/bin/env bash
# Builds the C oracle with the default build's flags (make -n).
set -e
cd "$(dirname "$0")"
cc -DNORL -DDIM_FILTERED -std=c11 -O1 -D_DEFAULT_SOURCE -D_XOPEN_SOURCE=600 -w -o oracle oracle.c -lncursesw -ltinfo -lpthread
