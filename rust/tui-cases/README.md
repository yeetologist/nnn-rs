# tui-cases: the screen proof

Each case in `cases.json` runs a build of nnn in an 80x24 terminal with a fresh fixture tree,
types its keys and records, after the start and after every key, the screen text once it has
stopped changing (no colours), then the exit code and what the run left in the work tree.
`expected/<case>.json` holds that record from the C original (`reference/nnn/nnn`).

Two harnesses run the same cases against the same expected files:

- `harness.py` (Python + tmux, ADR-016), Linux only; it also records:

      python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn
      python3 rust/tui-cases/harness.py record $(pwd)/reference/nnn/nnn

- `rust/tui-check` (a pty, ConPTY on Windows, and the vt100 crate as the terminal), Linux and
  Windows; check only:

      cargo run --manifest-path rust/tui-check/Cargo.toml -- check $(pwd)/rust/app/target/debug/nnn
      cargo run --manifest-path rust/tui-check/Cargo.toml -- check rust\app\target\debug\nnn.exe

  Both take `--case NAME` (repeatable) and `--verbose`, print `FAIL <case>` with a diff for
  each mismatch and `N/M tui cases passed`, and exit 0 only when all pass. On Linux the two
  agree on every case, with both the Rust build and the C original.

## Case fields

- `name`; `args` (`{work}`/`{home}` are replaced by the run's directories); `env` (added to
  the fixed environment: `TERM=screen-256color`, `LANG` and `LC_ALL` `C.UTF-8`, `TZ=UTC`, `HOME`, ...).
- `keys`: literal text, or `<Enter> <Esc> <Tab> <BS> <Space> <Up> <Down> <Right> <Left> <Home>
  <End> <PgUp> <PgDn>` and `<C-x>`, sent as the bytes tmux sends (cursor keys follow the
  application-cursor mode).
- `fixture`: path to content (`null` = directory); every entry gets mtime 1700000000
  (2023-11-14 22:13 UTC).
- `read`: files under the run's temp dir to record (`<TMP>` stands for that dir).
- `normalize`: `[regex, replacement]` pairs (Python syntax) applied to every screen line after the
  global one, which turns every time other than the fixture's into `<NOW>`.
- `nohash`: work-tree files compared by name only; `quiet`: seconds without change that end a
  step (default 0.3; 0.4 for the start).
- `platforms`: optional, e.g. `["unix"]`; tui-check skips a case whose list does not name its
  platform (`unix` or `windows`). harness.py ignores it. The Unix-only cases need Unix tools
  (sh, sed, tar, chmod), Unix file details or a sh-syntax `.lastd`.

## Windows

On Windows tui-check also wildcards, in expected and actual alike, what NTFS cannot match:
permission strings (`<PERM>`), the detail-mode octal column (`<OCT>`), directory sizes
(`<DSIZE>`) and the status bar's `N-INODE` hard-link suffix; the temp dir is `<TMP>` in both its
`\` and `/` spellings. The build gets the fixed environment plus `SystemRoot`, `windir`,
`ComSpec`, `PATHEXT`, `TEMP` and `TMP`.
