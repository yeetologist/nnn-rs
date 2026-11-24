#!/usr/bin/env bash
# Cutover to the Rust nnn, one step at a time (run inside WSL/Linux, from the repo root).
# Each step says what it does, waits for Enter, checks the result, and says how to go back.
set -u
cd "$(dirname "$0")/.." || exit 1
dest="${HOME}/.local/bin"
step() { printf '\n== %s\n' "$1"; read -r -p "Enter to run this step, Ctrl-C to stop: " _; }

step "1/3 Build the release binary (rust/app/target/release/nnn)"
(cd rust/app && cargo build --release) || { echo "Build failed; nothing changed."; exit 1; }
rust/app/target/release/nnn -V || exit 1

step "2/3 Install it as $dest/nnn (an existing file there is kept as nnn.c-backup)"
mkdir -p "$dest"
if [ -e "$dest/nnn" ] && [ ! -e "$dest/nnn.c-backup" ]; then cp "$dest/nnn" "$dest/nnn.c-backup"; fi
install -m 755 rust/app/target/release/nnn "$dest/nnn" || exit 1
echo "Installed. Way back: mv \"$dest/nnn.c-backup\" \"$dest/nnn\" (or rm \"$dest/nnn\" if there was none)."

step "3/3 Check the installed binary answers"
command -v nnn
nnn -V && echo "OK: run nnn as usual. Your ~/.config/nnn, sessions, plugins and NNN_* settings are unchanged (ADR-006)."
echo "The C build (make) still works as before; the C source stays in reference/nnn (make -C reference/nnn)."
