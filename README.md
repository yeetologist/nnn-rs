# nnn-rs

The Rust port of [nnn](https://github.com/jarun/nnn), the terminal file manager, with the proofs that it behaves like the C original.

- `rust/app`: the crate (pure Rust `nnn`).
- `rust/`: proofs, decisions (`DECISIONS.md`), plan (`MAP.md`), handoff (`HANDOFF.md`).
- `reference/nnn`: the C original, a git submodule pinned at a93d0bd4. Get and build it with:

      git submodule update --init
      make -C reference/nnn

## Proof commands (WSL/Linux, repo root)

    cd rust/app && cargo test
    rust-anything app replay . --build rust/app/target/debug/nnn          # 242/242
    python3 rust/bytes-check.py rust/app/target/debug/nnn                 # 246/246
    python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn   # 48/48 (tmux)
    rust/oracle/run.sh rust/app/target/release/oracle                     # 15 "ok" (cargo build --release first)

`rust-anything app replay .` (no `--build`) replays the recorded cases against the original.

## License

BSD-2-Clause, same as nnn. See [LICENSE](LICENSE).
