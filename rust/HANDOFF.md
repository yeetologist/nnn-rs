# Handoff (2026-11-24, port complete; repo moved to nnn-rs)

State: all waves W1-W6 done; W7 (Windows) claimed, see MAP.md. rust/app builds the pure Rust `nnn` (no fallback). The C original is the git submodule reference/nnn (a93d0bd4); build it with `git submodule update --init && make -C reference/nnn`.
Proof suites (run in WSL Ubuntu, repo /mnt/d/Documents/Github/nnn-rs, ADR-002):
- `cd rust/app && cargo test` (unit tests + rust/app-cases through trycmd)
- `rust-anything app replay . --build rust/app/target/debug/nnn`
- `python3 rust/bytes-check.py rust/app/target/debug/nnn` (needs the C build: `make -C reference/nnn`; default original is reference/nnn/nnn)
- `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` (tmux, ADR-016; `record $(pwd)/reference/nnn/nnn` re-records from the original)
- `rust-anything app replay .` replays the cases against the original (`run` = reference/nnn/nnn)
- function oracles: `rust/oracle/run.sh rust/app/target/release/oracle` (needs `cd rust/app && cargo build --release`) and the per-module *_run.* scripts
Open: user confirmation of ADR-012 and ADR-014; cutover is the user's (rust/cutover.sh). Possible next work: a reshape pass (idiomatic Rust, merging modules), icon builds, macOS/BSD.
Decisions: [DECISIONS.md](DECISIONS.md); plan and state: [MAP.md](MAP.md).
