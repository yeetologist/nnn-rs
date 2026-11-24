# nnn-rs

The Rust port of [nnn](https://github.com/jarun/nnn), the terminal file manager, with the proofs that it behaves like the C original.

- `rust/app`: the crate (pure Rust `nnn`).
- `rust/`: proofs, decisions (`DECISIONS.md`), plan (`MAP.md`), handoff (`HANDOFF.md`).
- `reference/nnn`: the C original, a git submodule pinned at a93d0bd4. Get and build it with:

      git submodule update --init
      make -C reference/nnn

## Windows

Windows 10 1809 or later (ConPTY), x86_64. Install from the repo root:

    cargo install --path rust/app

or take the release binary `nnn.exe`. Use Windows Terminal (conhost works too).

- Git for Windows (`sh.exe` on PATH) is needed for sh-based plugins and for POSIX syntax in the `!` shell and the `]` command prompt. Without it those fall back to `cmd /C` and plugins report "sh not found (Git for Windows)". Everything else runs without it.
- List mode (paths from stdin) needs symlinks: turn on Developer Mode or run elevated.
- Not supported (nnn says so): NNN_FIFO, mounts and archive mounting, the permission editor (chmod).
- Differences: Ctrl-Break is ignored (like SIGQUIT on Linux); `rm -rf` of read-only files fails with "Permission denied"; locale is C.UTF-8 behaviour (code point sorting, Unicode regex classes). Full list: ADR-022..027 in `rust/DECISIONS.md`.

Proof commands (PowerShell, repo root):

    cargo test --manifest-path rust/app/Cargo.toml
    rust-anything app replay . --build rust\app\target\debug\nnn.exe          # 242/242
    cargo run --manifest-path rust/tui-check/Cargo.toml -- check <abs path to rust\app\target\debug\nnn.exe>   # 40/40

## Proof commands (WSL/Linux, repo root)

    cd rust/app && cargo test
    rust-anything app replay . --build rust/app/target/debug/nnn          # 242/242
    python3 rust/bytes-check.py rust/app/target/debug/nnn                 # 246/246
    python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn   # 48/48 (tmux)
    cargo run --manifest-path rust/tui-check/Cargo.toml -- check $(pwd)/rust/app/target/debug/nnn   # 48/48 (pty + vt100)
    rust/oracle/run.sh rust/app/target/release/oracle                     # 15 "ok" (cargo build --release --features oracles first)

`rust-anything app replay .` (no `--build`) replays the recorded cases against the original.

## License

BSD-2-Clause, same as nnn. See [LICENSE](LICENSE).
