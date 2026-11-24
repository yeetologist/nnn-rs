# W4 brief for porting agents

You port part of src/nnn.c (the C original, ~11k lines) to Rust, into ONE module of the crate
rust/app. Read first: rust/DECISIONS.md (ADRs, especially 004, 006, 011, 013, 014, 017),
rust/GLOSSARY.md, and the modules you build on: rust/app/src/state.rs (the `Nnn` struct =
nnn.c's globals), rust/app/src/curses.rs (the ncurses subset: `self.cs()` is stdscr + terminal;
`self.cs().stdscr.mvaddstr(y, x, b"..")`, `attron`, `clrtoeol`, `refresh`, `getch`,
`get_wch`, `newwin`/`wrefresh`/`delwin`, `endwin`/`resume`, `timeout`), locale.rs (glibc
calls; the ONLY place for `unsafe`), sort.rs, fmt.rs, dir.rs, sys.rs, opts.rs.

Rules
- Translate faithfully, function by function, keeping nnn.c's names (snake_case) as methods of
  `Nnn` (`impl Nnn { pub fn xreadline(&mut self, ...) }`) or free functions when they use no
  globals. Default build only: NORL, DIM_FILTERED, no PCRE2, no icons; mouse, FIFO, sessions,
  X11 on. Keep the original's behavior exactly: messages, keys, limits, edge cases, error paths.
  Byte strings are `Vec<u8>`/`&[u8]` (paths are not UTF-8).
- Put this module's own extra state in its `...State` struct in YOUR file (Nnn has a field for
  it). Do NOT edit state.rs, curses.rs, lib.rs or any other module; if you truly need a change
  there, write it down in your final report instead. If the crate fails to build because of
  another module that is being written at the same time, wait a minute and retry.
- No `unsafe` (use locale.rs, rustix, std). No `unwrap`/`expect`/`panic!` where the original
  handles an error; mark truly impossible states `// INFALLIBLE: <why>`. Use `#[expect(..,
  reason = "..")]`, never `#[allow]`. Leave nothing `unimplemented!`/`todo!`.
- Each ported function gets a doc comment naming the C function it ports.
- Where a function you port calls one that another module ports, call it by its nnn.c name as a
  method on `Nnn` and list it in your report; the integrator wires it.
- Commands run in WSL Ubuntu (ADR-002): from Git Bash use
  `wsl.exe -e bash -lc ". ~/.cargo/env; cd /mnt/d/Documents/Github/nnn/rust/app && <cmd>"`.
  Before finishing: `cargo fmt` and `cargo clippy --all-targets -- -D warnings` clean for your
  file, `cargo test` passing.
- Prove what can be proved without the screen: for pure functions, add a differential oracle
  like rust/oracle (read oracle.c, gen.py, run.sh and src/bin/oracle.rs): your own
  rust/oracle/<module>_oracle.c (#include "../../reference/nnn/src/nnn.c" with main renamed) and
  rust/app/src/bin/oracle_<module>.rs speaking the same line protocol, plus a runner; run it and
  report the counts. Add Rust unit tests for the rest.
- Report at the end: functions ported (C name -> Rust), functions you call that others port,
  changes you need outside your file, test/oracle results, and any behavior you could not match.

## W5 additions
- The interactive proof is rust/tui-cases (tmux, ADR-016): run your cases with
  `python3 rust/tui-cases/harness.py check env RUST_ANYTHING_UNIT_TUI=rust $(pwd)/rust/app/target/debug/nnn --case <name> [--verbose]`
  from the repo root in WSL (build first: `cargo build` in rust/app). The original passes all of
  them: `... check ./nnn --case <name>`. Do not re-record expected files and do not edit
  cases.json; if a case looks wrong, say so in your report.
- Other agents run tests at the same time: NEVER pkill/killall nnn or tmux; only kill PIDs you
  started yourself. Keep the crate compiling at every step (others build it too): make small
  edits, run `cargo check` after each.
- Environment for children: `crate::locale::setenv(name, value)` / `unsetenv` (glibc setenv).
- Messages table: `crate::browse::MESSAGES[...]` with the MSG_* indexes in browse.rs.
- The placeholder functions for your module are already in your file with their current
  signatures (browse.rs calls them); keep those signatures unless you also fix every caller.
