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

- Git for Windows (`sh.exe` on PATH) is needed for sh-based plugins and for POSIX syntax in the `!` shell and the `]` command prompt. Without it those fall back to `cmd /C` and plugins report "sh not found (Git for Windows)". Everything else — including copy/move/remove, duplicate, and archiving a selection — runs without extra tools.
- Built-in drive picker: press `D` to jump between logical drives (`C:/`, `D:/`, USB, mapped network shares) while `h` / `Left` stops at the current drive root like Unix `nnn` at `/` (ADR-030).
- List mode (paths from stdin) needs symlinks: turn on Developer Mode or run elevated.
- Not supported (nnn says so): NNN_FIFO, mounts and archive mounting, the permission editor (chmod).
- Differences: Ctrl-Break is ignored (like SIGQUIT on Linux); locale is C.UTF-8 behaviour (code point sorting, Unicode regex classes); archiving very large selections (>28 KB of paths) with non-ASCII names falls back to Windows `tar.exe -T`, which reads its list in the legacy ANSI codepage. Full list: ADR-022..030 in `rust/DECISIONS.md`.

### With Microsoft Coreutils

With [Coreutils for Windows](https://learn.microsoft.com/en-us/windows/core-utils/overview) installed (`winget install Microsoft.Coreutils`, or the release zip's `coreutils.exe` on PATH), nnn finds it and runs its `cp`, `mv`, `rm`, `stat` and `xargs` where Linux nnn runs GNU's: copy, move and remove of the selection (with their own prompts), remove, duplicate, the file details popup and archiving the selection. `NNN_COREUTILS=0` turns this off. Details and differences: ADR-028.

Proof commands (PowerShell, repo root):

    cargo test --manifest-path rust/app/Cargo.toml
    rust-anything app replay . --build rust\app\target\debug\nnn.exe          # 242/242
    cargo run --manifest-path rust/tui-check/Cargo.toml -- check <abs path to rust\app\target\debug\nnn.exe>   # 42/42 (+ --coreutils: 46/46)
    cargo test --manifest-path rust/app/Cargo.toml -- --include-ignored   # with Coreutils installed

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
