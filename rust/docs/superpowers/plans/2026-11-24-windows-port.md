# nnn-rs: standalone repo + Windows support (rust-anything wave W7) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the Rust port of nnn into its own git repository, then make it build and run natively on Windows 10/11 (x86_64-pc-windows-msvc) while Linux behaviour stays proven-identical to the C original.

**Architecture:** A new repo `D:\Documents\Github\nnn-rs` keeps the current `rust/` tree unchanged (so the rust-anything tools and every proof script keep working) and pins the C original as a git submodule in `reference/nnn`. All OS-specific code moves behind `rust/app/src/platform/` (`unix.rs` = today's behaviour, `windows.rs` = new), so the ported nnn logic stays one code base. Windows has no C original to compare against, so its reference is "the Rust build on Linux under LC_ALL=C.UTF-8", checked by a new cross-platform screen harness (ConPTY/pty + a VT parser) that must first agree with the tmux harness on Linux.

**Method:** rust-anything 3.3.0 whole-app workflow (its `whole-app.md`), continued as wave W7: claim W7 in `rust/MAP.md`, decisions as ADRs, `rust-anything app replay` as the equivalence gate on **both** OSes (the 242 CLI cases recorded from the C original replay against the Windows `nnn.exe` too), `rust-anything map --write` and `rust/HANDOFF.md` at every checkpoint, and a stop for the user's go-ahead after each checkpoint (ADR-007). rust-anything has no screen recorder, so the screen proof stays the recorded tmux cases, replayed on Windows by `tui-check` (Task 8). nnn.c is one tangled unit (ADR-011), so `bridge`/`gate` do not apply; the module oracles remain the function-level proof.

**Tech Stack:** rust-anything 3.3.0 (WSL and Windows host), Rust 1.99+ (MSVC on Windows, WSL Ubuntu for Linux), `windows-sys` (console, file info), `unicode-width`, `regex`, `chrono` (portable locale backend), `portable-pty` + `vt100` (harness), trycmd, GitHub Actions (CI file only; pushing needs the user's OK).

**Spec:** the user's request of 2026-11-24 ("plan so it can run in windows, also create a git repo for our port") plus `rust/DECISIONS.md` ADR-001…020, which stay binding on Linux. New decisions in this plan become ADR-021…027 (Task 10).

## Global Constraints

- **Equivalence gate on Windows (rust-anything):** from Task 6 on, on the Windows host at the repo root: `rust-anything app replay . --build rust\app\target\debug\nnn.exe` must report `242/242 cases passed`; before Task 6 the count is recorded in the task's commit message and may only go up. A failing case is fixed in the port, never in `rust/app-cases` (whole-app.md §7); `rust-anything app replay . --build <exe> --case <name>` is the loop for one case.
- **Checkpoints:** after Tasks 1, 3, 5, 7 and 9: run `rust-anything map --write`, update `rust/MAP.md` (W7 row: claimed/done with the numbers) and `rust/HANDOFF.md`, report to the user, and wait for the go-ahead before the next task.
- Linux stays byte-identical to the C original: after every task, in WSL: `cargo test` (rust/app), `rust-anything app replay . --build rust/app/target/debug/nnn` = 242/242, `python3 rust/bytes-check.py rust/app/target/debug/nnn` = 246/246, `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` = 48/48, `rust/oracle/run.sh rust/app/target/release/oracle` = 15/15 "ok".
- Windows target: `x86_64-pc-windows-msvc`, Windows 10 1809+ (ConPTY and VT sequences), Windows Terminal and conhost.
- `unsafe` only in `rust/app/src/platform/unix.rs` and `rust/app/src/platform/windows.rs`, each block with `// SAFETY:`. No `#[allow]`; `#[expect(.., reason = "..")]` only.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean on both Linux and Windows.
- Unix-only crates go under `[target.'cfg(unix)'.dependencies]` (`rustix`, `libc`), Windows-only under `[target.'cfg(windows)'.dependencies]` (`windows-sys`, `chrono`).
- Internal paths stay byte strings with `/` separators on every OS; on Windows bytes are WTF-8 from `OsStr::as_encoded_bytes` and are only ever split at `/`.
- Config, session and selection file formats are unchanged (ADR-006).
- Messages shown on Windows reuse nnn's texts; errors use glibc's `strerror` wording (e.g. "No such file or directory") so CLI output matches Linux.
- Nothing is pushed or published anywhere without the user's explicit OK.

## Review Focus

1. **Drive roots and UNC paths:** at `C:/` pressing `h` stays put (like `/` on Linux); starting in `\\server\share\dir` shows `//server/share/dir` and going up stops at `//server/share/`. Tests in Task 2 (`root_len`) and Task 9 (case `win-drive-root`).
2. **Names that are not valid UTF-16 (lone surrogates) on NTFS:** listed (shown with `?`/replacement), and renaming/deleting them works because bytes round-trip through WTF-8. Test in Task 3 (`wtf8_round_trip_unpaired_surrogate`).
3. **Console resize while a prompt or popup is open:** the screen redraws at the new size, no panic. Test in Task 5 (`resize_event_becomes_key_resize`) and Task 9 (case `win-resize-in-prompt`).
4. **Ctrl-C at the main loop and inside a prompt, and any exit (panic included):** the console input/output modes are restored. Test in Task 5 (`console_mode_restored_on_drop`).
5. **Locked files and paths over 260 characters on copy/move/delete:** an nnn error message (glibc wording, e.g. "Permission denied"), never a panic. Tests in Task 7 (`delete_locked_file_reports_error`, `copy_long_path`).

---

## File Structure

New repo `nnn-rs/` (root of the rust-anything `<repo>`):
- `rust/` — copied unchanged from `nnn/rust/` (crate, proofs, docs), then edited by the tasks below.
- `reference/nnn` — git submodule `https://github.com/jarun/nnn` at `a93d0bd4c1871bc442165c792c1433984bd1cde5`; `make -C reference/nnn` builds the C original used by the proofs.
- `README.md`, `.gitignore`, `.gitattributes`, `.github/workflows/ci.yml`.

Inside `rust/app/src/`:
- `platform/mod.rs` — the OS-independent API (re-exports the active backend) + path helpers.
- `platform/unix.rs` — today's code moved out of `locale.rs`, `curses.rs`, `dir.rs`, `sys.rs`, `du.rs`, `startup.rs`.
- `platform/windows.rs` — console, file info, WTF-8 conversion, defaults.
- `platform/portable_locale.rs` — pure-Rust locale backend (feature `portable` on Linux, always on Windows).
- `winops.rs` — native copy/move/remove, file-info panel and dir-change polling for Windows.
- `locale.rs` becomes a thin façade over `platform` (same public functions, so callers don't change).

New crate `rust/tui-check/` — cross-platform screen harness (bin `tui-check`).

---

### Task 1: Create the nnn-rs repository

**Files:**
- Create: `nnn-rs/` (copy of `nnn/rust/` into `nnn-rs/rust/`, without `rust/app/target/`), `nnn-rs/.gitignore`, `nnn-rs/.gitattributes`, `nnn-rs/README.md`
- Modify: `rust/oracle/*.c` (`#include "../../src/nnn.c"` → `"../../reference/nnn/src/nnn.c"`), `rust/oracle/build.sh` and the other `*_run.*` scripts' C paths, `rust/bytes-check.py` default original (`HERE/../reference/nnn/nnn`), `rust/app-cases/rust-anything.json` (`"run": ["reference/nnn/nnn"]`), `rust/HANDOFF.md` commands

**Interfaces:**
- Produces: repo root `D:\Documents\Github\nnn-rs` (WSL: `/mnt/d/Documents/Github/nnn-rs`); the C original at `reference/nnn/nnn` after `make -C reference/nnn`.

- [ ] **Step 0:** `rust-anything doctor` in WSL and on the Windows host; both report the CLI, runtimes and MCP setup ready (fix what it prints before continuing). Add the W7 row to `rust/MAP.md`: "W7 | Windows: platform layer, console, portable locale, native file ops, tui-check | claimed by session <date>".
- [ ] **Step 1:** `git init nnn-rs` (branch `main`), copy `rust/` excluding `target/`, `git submodule add https://github.com/jarun/nnn reference/nnn` and check out `a93d0bd4` in it.
- [ ] **Step 2:** `.gitignore`: `target/`, `rust/app-cases/replay-report.md`, `rust/oracle/oracle`, `rust/oracle/*_oracle` (built C oracles). `.gitattributes`: `* text=auto eol=lf`; `rust/app-cases/** -text`; `rust/argv0-cases/** -text`; `rust/tui-cases/expected/** -text` (recorded bytes must not be rewritten).
- [ ] **Step 3:** Rewire the paths listed under **Modify**; README states: what this is, the proof commands from `rust/HANDOFF.md`, that the C source lives in `reference/nnn`, license BSD-2-Clause (same as nnn; copy `LICENSE` from the original).
- [ ] **Step 4: Verify the move broke nothing (WSL, repo root):** `make -C reference/nnn`; `rust-anything app replay .` (the original via the rewired `run`) → 242/242; then all five Global-Constraint suites. Expected: 242/242, 246/246, 48/48, `cargo test` ok, oracle 15 "ok". `rust-anything map --write` regenerates `rust/PORT_MAP.md` in the new location without errors.
- [ ] **Step 5: Verify line endings survived:** `git ls-files --eol rust/app-cases | grep crlf` prints nothing.
- [ ] **Step 6: Commit** `git add -A && git commit -m "Import the Rust port of nnn with its proofs; pin the C original as reference/nnn"`. Ask the user before deleting `nnn/rust/` from the old checkout.

### Task 2: Platform module and path helpers (Linux unchanged)

**Files:**
- Create: `rust/app/src/platform/{mod.rs,unix.rs,windows.rs}`
- Modify: every `use std::os::unix::ffi::{OsStrExt, OsStringExt}` site (21 files; `rg -l 'OsStrExt|OsStringExt' rust/app/src`), `draw.rs` `istopdir`, `config.rs` `xdirname`/`istopdir`, `sys.rs` `abspath_in`, `listmode.rs` `xdirname`/`common_prefix`, `Cargo.toml`
- Test: `rust/app/src/platform/mod.rs` (unit tests)

**Interfaces:**
- Produces (in `crate::platform`):
  - `pub fn os(b: &[u8]) -> std::borrow::Cow<'_, std::ffi::OsStr>` and `pub fn bytes(s: &std::ffi::OsStr) -> std::borrow::Cow<'_, [u8]>` (unix: zero-copy; windows: WTF-8, backslashes → `/` in `bytes`).
  - `pub fn root_len(path: &[u8]) -> usize` — unix: 1 for `/…`, else 0; windows: 3 for `C:/…`, the length of `//server/share/` for UNC, 0 for relative.
  - `pub fn is_root(path: &[u8]) -> bool` (= `root_len(path) == path.len() && path.len() > 0`), `pub fn is_absolute(path: &[u8]) -> bool`.
  - `istopdir` everywhere calls `platform::is_root`; `xdirname` never cuts inside the root.
- Cargo: `rustix`, `libc` → `[target.'cfg(unix)'.dependencies]`; `windows-sys = { version = "0.61", features = ["Win32_System_Console", "Win32_Storage_FileSystem", "Win32_Foundation", "Win32_System_Threading"] }` → `[target.'cfg(windows)'.dependencies]`; feature `oracles` with every `src/bin/oracle*.rs` declared `required-features = ["oracles"]` (oracle scripts build with `--features oracles`).

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn root_len_unix_and_windows_forms() {
    assert_eq!(root_len_with(Os::Unix, b"/a/b"), 1);
    assert_eq!(root_len_with(Os::Unix, b"a/b"), 0);
    assert_eq!(root_len_with(Os::Windows, b"C:/a"), 3);
    assert_eq!(root_len_with(Os::Windows, b"C:/"), 3);
    assert_eq!(root_len_with(Os::Windows, b"//srv/share/x"), 12);
    assert_eq!(root_len_with(Os::Windows, b"//srv/share"), 11);
    assert_eq!(root_len_with(Os::Windows, b"a/b"), 0);
}
#[test] fn xdirname_stops_at_root() {
    assert_eq!(xdirname_with(Os::Windows, b"C:/a"), b"C:/");
    assert_eq!(xdirname_with(Os::Windows, b"C:/"), b"C:/");
    assert_eq!(xdirname_with(Os::Unix, b"/a"), b"/");
}
```

`root_len_with(os: Os, ..)` / `xdirname_with` take an explicit `enum Os { Unix, Windows }` so both forms are tested on either host; `root_len` = `root_len_with(Os::CURRENT, ..)`.
- [ ] **Step 2:** `cargo test platform` → FAIL (not defined).
- [ ] **Step 3:** Implement the interfaces above; replace the 21 sites' `OsStr::from_bytes` / `as_bytes` / `into_vec` with `platform::os` / `platform::bytes`.
- [ ] **Step 4:** `cargo test` passes; all five Linux suites green.
- [ ] **Step 5:** Commit `"Add platform module: byte paths and roots for Unix and Windows"`.

### Task 3: Platform file system and process environment

**Files:**
- Modify: `platform/{unix,windows}.rs`, `dir.rs` (uses `platform::stat_at`/`read_dir` instead of rustix), `du.rs`, `sys.rs`, `startup.rs` (`/dev/null`, `/tmp`), `listmode.rs` (`/dev/tty`, symlinks), `config.rs` (`setup_config` home), `draw.rs` (readlink/stat in `statusbar`), `locale.rs` (`pw_name`/`gr_name`)

**Interfaces:**
- Produces (in `crate::platform`):
  - `pub struct FileInfo { pub mode: u32, pub size: i64, pub nlink: u64, pub uid: u32, pub gid: u32, pub ino: u64, pub blocks512: u64, pub atime: (i64, u32), pub ctime: (i64, u32), pub mtime: (i64, u32) }` — Unix fields as today. Windows: `mode` synthesized (`0o040755` dir, `0o100644` file with `0o111` added for `.exe .bat .cmd .com .ps1`, `0o120777` symlink/junction, write bits cleared for READONLY), `uid = gid = 0`, `nlink`/`ino` from `GetFileInformationByHandle`, `blocks512` = `FILE_STANDARD_INFO.AllocationSize / 512`, `ctime` = change time from `FILE_BASIC_INFO`.
  - `pub fn stat(path: &[u8], follow: bool) -> std::io::Result<FileInfo>`, `pub enum DType { Dir, Symlink, Unknown, Other }`, `pub fn read_dir(path: &[u8]) -> std::io::Result<impl Iterator<Item = std::io::Result<(Vec<u8>, DType)>>>`, `pub fn readlink(path: &[u8]) -> std::io::Result<Vec<u8>>`, `pub fn symlink(target: &[u8], link: &[u8], is_dir: bool) -> std::io::Result<()>`.
  - `pub fn null_device() -> &'static [u8]` (`/dev/null` | `NUL`), `pub fn tty_device() -> &'static [u8]` (`/dev/tty` | `CONIN$`), `pub fn temp_dir() -> Option<Vec<u8>>` (unix: `/tmp` if accessible else `$TMPDIR`, unchanged; windows: `std::env::temp_dir()`), `pub fn home() -> Option<Vec<u8>>` (unix `$HOME`; windows `$HOME` else `%USERPROFILE%`), `pub fn user_name(uid: u32) -> Option<Vec<u8>>` / `group_name(gid)` (windows: `None`; `-U` then shows nothing extra).
  - `sys::strerror(&io::Error) -> String` on Windows maps `io::ErrorKind` to glibc text: NotFound → "No such file or directory", PermissionDenied → "Permission denied", AlreadyExists → "File exists", DirectoryNotEmpty → "Directory not empty", ResourceBusy → "Device or resource busy", InvalidFilename → "File name too long"; anything else → Rust's message without " (os error N)".

- [ ] **Step 1: Write failing tests** in `platform/mod.rs` (run on both OSes):

```rust
#[test] fn stat_dir_and_file_modes() { /* temp dir with "d/" and "f": stat(d).mode & 0o170000 == 0o040000; stat(f).mode & 0o170000 == 0o100000; stat(f).size == 3 */ }
#[test] fn read_dir_reports_types() { /* entries include ("d", DType::Dir); no "." or ".." */ }
#[cfg(windows)] #[test] fn wtf8_round_trip_unpaired_surrogate() {
    // name = OsString::from_wide(&[0x61, 0xD800]); create it; read_dir returns bytes b;
    // std::fs::rename(platform::os(&dir_slash_b), ..) succeeds and the renamed file exists.
}
#[cfg(windows)] #[test] fn strerror_uses_glibc_words() {
    let e = std::fs::File::open("C:/no/such/file").unwrap_err();
    assert_eq!(crate::sys::strerror(&e), "No such file or directory");
}
```
- [ ] **Step 2:** `cargo test platform` → FAIL.
- [ ] **Step 3:** Implement; move the rustix calls from `dir.rs`/`du.rs` into `platform/unix.rs` without changing behaviour (d_type Unknown → nofollow path stays); Windows `read_dir` via `std::fs::read_dir` + `file_type()`.
- [ ] **Step 4:** Linux: five suites green. Windows (PowerShell, repo root): `cargo test --manifest-path rust/app/Cargo.toml platform` passes. **Checkpoint** (map --write, MAP, HANDOFF, report, wait). (Windows may not build fully until Task 5; run only this module's tests with `--lib platform`, or `cargo check` errors limited to `curses.rs`/`locale.rs`.)
- [ ] **Step 5:** Commit `"platform: file info, directories, devices and errno text for Unix and Windows"`.

### Task 4: Portable locale backend

**Files:**
- Create: `rust/app/src/platform/portable_locale.rs`
- Modify: `locale.rs` (façade: `#[cfg(any(windows, feature = "portable"))]` → portable, else the glibc functions moved to `platform/unix.rs`), `Cargo.toml` (feature `portable = ["dep:unicode-width", "dep:regex", "dep:chrono"]`; on Windows these are plain dependencies), `rust/oracle/run.sh` (env `LOCALES` overrides the locale list)
- Create: `rust/oracle/portable-known-diffs.md` (only if Step 4 leaves any)

**Interfaces:**
- Consumes: the existing `locale` API (unchanged signatures): `init, strcoll, strcasecmp, tolower, mbstowcs, mblen_at, to_lower_wide, wcswidth_n, wcstombs, localtime, Regex::{new,is_match}, is_walnum, setenv, unsetenv`.
- Produces: the same API, defined as glibc does in `C.UTF-8`: `strcoll` = byte order; `strcasecmp`/`tolower` = ASCII only; `mbstowcs` = UTF-8 → Unicode scalars, `None` on invalid UTF-8; `to_lower_wide` = `char::to_lowercase` when it yields exactly one char, else unchanged; `wcswidth_n` = `unicode-width` (-1 for C0/C1 controls); `Regex` = `regex::bytes::Regex` built from the POSIX ERE (case-insensitive flag for `REG_ICASE`, `unicode(false)` classes so `[[:alpha:]]` etc. are ASCII as in C.UTF-8); `localtime` = `chrono::Local`.

- [ ] **Step 1: Write the failing proof:** add to `run.sh` usage: `LOCALES=C.UTF-8 rust/oracle/run.sh <oracle built with --features portable,oracles>`.
- [ ] **Step 2:** Run it against the C oracle on Linux → FAIL (feature missing).
- [ ] **Step 3:** Implement `portable_locale.rs`.
- [ ] **Step 4:** `LOCALES=C.UTF-8 rust/oracle/run.sh rust/app/target/release/oracle 1 20000` (built with `--features portable,oracles`) → 3/3 "ok". Each remaining diff gets fixed, or written up in `portable-known-diffs.md` with the input class and why (e.g. a glibc-only width table entry); the default build's run (no feature) stays 15/15.
- [ ] **Step 5:** Commit `"Portable locale backend matching glibc C.UTF-8; used on Windows"`.

### Task 5: Windows console in curses.rs

**Files:**
- Modify: `rust/app/src/curses.rs` (terminal setup, `size`, `fill` and the SIGWINCH flag move behind `platform::Term`), `platform/{unix,windows}.rs`, `browse.rs` (signal registration via `platform::register_signals`)

**Interfaces:**
- Produces (in `crate::platform`):
  - `pub struct Term` with `pub fn open() -> std::io::Result<Term>` (unix: today's termios cbreak/noecho/nonl on `/dev/stdin`; windows: `GetStdHandle(STD_INPUT_HANDLE)`, input mode = `ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT | ENABLE_PROCESSED_INPUT`, output mode adds `ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN`, `SetConsoleCP`/`SetConsoleOutputCP(65001)`), `pub fn size(&self) -> (i32, i32)` (lines, cols; `LINES`/`COLUMNS` still win), `pub fn read(&mut self, wait_ms: i32, out: &mut Vec<u8>) -> ReadResult` with `pub enum ReadResult { Data, Resized, Timeout }` (windows: `WaitForSingleObject` + `ReadConsoleInputW`; key-down `UnicodeChar` UTF-16 → UTF-8 bytes; `WINDOW_BUFFER_SIZE_EVENT` → `Resized`), `pub fn restore(&mut self)`, `pub fn resume(&mut self)`; `impl Drop for Term` restores the saved modes.
  - `pub fn register_signals(int: Arc<AtomicBool>, term: Arc<AtomicBool>)` (unix: SIGINT, SIGTERM, SIGHUP, SIGQUIT ignored, SIGWINCH via `Term`; windows: SIGINT and SIGTERM via signal-hook).
  - `Curses::get_wch` turns `ReadResult::Resized` into `resize()` + `KEY_RESIZE`; the key decoder is unchanged (VT input gives the same escape sequences).

- [ ] **Step 1: Write failing tests** (`curses.rs` tests module):
  - `resize_event_becomes_key_resize` — a fake `Term` (trait `TermIo` implemented by `Term` and a test double) returns `Resized`, then fills `out` with `b"q"` and returns `Data`; `get_wch` yields `Input::Key(KEY_RESIZE)` then `Input::Char('q')`.
  - `#[cfg(windows)] console_mode_restored_on_drop` — runs under a ConPTY (`portable-pty` dev-dependency): child binary `tui-check --probe-modes` prints the input/output modes before `Term::open` and after drop (including after a caught panic); they are equal.
- [ ] **Step 2:** `cargo test curses` → FAIL.
- [ ] **Step 3:** Implement `TermIo` / `Term` and move curses.rs onto it.
- [ ] **Step 4:** Windows: `cargo build --manifest-path rust/app/Cargo.toml` succeeds; `cargo test` passes; first full `rust-anything app replay . --build rust\app\target\debug\nnn.exe` run, count recorded. Linux: five suites green (the tmux suite proves the Unix `Term` is unchanged). **Checkpoint.**
- [ ] **Step 5:** Commit `"curses: terminal backend for the Windows console"`.

### Task 6: Programs, defaults and Unix-only features on Windows

**Files:**
- Modify: `popup.rs` (`spawn`/`xfork` shell selection), `browse.rs` (defaults, FIFO, `-a`, x11 hostname, `set_time_type` unchanged), `mounts.rs`, `plugin.rs` (pipe as file), `listmode.rs` (symlink failure), `platform/*.rs`

**Interfaces:**
- Produces (in `crate::platform`):
  - `pub fn shell_exec_prefix() -> &'static [u8]` — unix `b"sh -c"`; windows `b"sh -c"` if `sh.exe` is found on `PATH` (Git for Windows), else `b"cmd /C"`. Every `UTIL_SH_EXEC` constant becomes a call to it.
  - `pub fn defaults() -> Defaults` with `opener` (unix `xdg-open`; windows `cmd /C start ""`), `editor` (`vi` | `notepad`), `pager` (`less` | `more`), `shell` (`sh` | `%COMSPEC%`) — still overridden by `NNN_OPENER`, `VISUAL`/`EDITOR`, `PAGER`, `SHELL` exactly as on Linux.
  - `pub const SUPPORTS_FIFO: bool`, `SUPPORTS_MOUNTS`, `SUPPORTS_CHMOD` (all `false` on Windows).
- Behaviour on Windows (new message `MSG_NOT_SUPPORTED = "not supported on Windows"`, appended to `browse::MESSAGES` so indexes don't shift):
  - `NNN_FIFO` set or `-a`: stderr `nnn: NNN_FIFO: not supported on Windows`, exit 1, before curses.
  - `c`, `u`, archive `m`ount, `*` (chmod): `printwait(MSG_NOT_SUPPORTED)`.
  - Plugins: `NNN_PIPE` is a regular temp file created empty before the plugin runs and read after it exits (`readpipe` unchanged); script plugins from the plugins dir run through `sh` if found, else `printwait("sh not found (Git for Windows)")`.
  - List mode: if creating a symlink fails, `msg("list mode needs symlinks: enable Developer Mode")`, exit 1.

- [ ] **Step 1: Write failing tests:** `#[cfg(windows)]` trycmd cases in new dir `rust/app-cases-windows/`: `fifo-unsupported` (env `NNN_FIFO=x`, stdin empty → stderr `nnn: NNN_FIFO: not supported on Windows`, code 1), `autofifo-unsupported` (`-a`, same); unit test `shell_prefix_prefers_sh` (PATH with a dir containing `sh.exe` → `sh -c`, without → `cmd /C`); `rust/app/tests/app_cases.rs` also runs `../app-cases-windows/*.toml` under `cfg(windows)`.
- [ ] **Step 2:** Windows `cargo test` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Windows: `rust-anything app replay . --build rust\app\target\debug\nnn.exe` → 242/242 (from here on a hard gate); `cargo test` passes, including `app_cases` on all 242 shared cases + the new ones (any shared case that cannot pass on Windows is listed with its reason in ADR-024 and filtered by name in `app_cases.rs` under `cfg(windows)`; target: none). Linux: five suites green.
- [ ] **Step 5:** Commit `"Windows: shell, default programs, unsupported features"`.

### Task 7: Native file operations, file info and directory watching on Windows

**Files:**
- Create: `rust/app/src/winops.rs` (compiled on all OSes so its tests run on Linux too; wired in only under `cfg(windows)`)
- Modify: `fileops.rs` (`cpmvrm_selection`: Windows branch), `popup.rs` (`show_stats`: Windows branch), `browse.rs` (`poll_dir_watch`: Windows branch)

**Interfaces:**
- Produces:
  - `pub fn copy_into(src: &[u8], dest_dir: &[u8], ask: &mut dyn FnMut(&[u8]) -> bool) -> std::io::Result<()>` (recursive, keeps mtime; `ask(name)` on an existing target = nnn's "overwrite?" prompt, `false` skips), `pub fn move_into(..same..)` (rename, falls back to copy + remove across volumes), `pub fn remove_all(path: &[u8]) -> std::io::Result<()>`. Errors are reported through `printwarn` with `sys::strerror` (Task 3); the operation continues with the next selected path, as `cp -i` does.
  - `pub fn file_info_text(path: &[u8]) -> Vec<u8>` — lines `  File: <path>`, `  Size: <n>  Blocks: <n>`, `Access: (<mode octal>/<lsperms>)`, `Modify: YYYY-MM-DD HH:MM:SS`, `Change: …`, `Attrib: <READONLY HIDDEN SYSTEM ARCHIVE …>`, shown in the same floating window as on Linux.
  - `pub struct DirWatch` with `fn new(path: &[u8]) -> DirWatch` and `fn changed(&mut self) -> bool` (directory mtime + entry count compared with the last call); `poll_dir_watch` uses it on Windows at the same odd-idle-second cadence.
  - Prompts and their texts are the Linux ones: `rm -rf N file(s)?`, `rm -rf '<name>'?`, `'c'urrent/'s'el?`.

- [ ] **Step 1: Write failing tests** in `winops.rs` (run on Linux and Windows):
  - `copy_into_copies_tree_and_mtime`, `copy_into_asks_on_conflict` (`ask` called once with `b"f"`; returning false leaves the target unchanged), `move_into_renames`, `remove_all_removes_tree`, `dir_watch_sees_new_file`.
  - `copy_long_path` — source path of 300 chars (nested dirs) copies successfully (Windows: `\\?\` prefix applied inside `platform::os` for absolute paths ≥ 260).
  - `#[cfg(windows)] delete_locked_file_reports_error` — a file held open without `FILE_SHARE_DELETE`; `remove_all` returns `Err` and `sys::strerror` of it is `"Permission denied"` or `"Device or resource busy"`; no panic.
- [ ] **Step 2:** `cargo test winops` → FAIL.
- [ ] **Step 3:** Implement and wire the Windows branches.
- [ ] **Step 4:** Both OSes: `cargo test`; Windows replay 242/242; Linux five suites green. **Checkpoint.**
- [ ] **Step 5:** Commit `"Windows: native copy/move/remove, file info and directory watching"`.

### Task 8: Cross-platform screen harness

**Files:**
- Create: `rust/tui-check/Cargo.toml` (deps `portable-pty`, `vt100`, `serde_json`, `regex`, `sha1_smol`, `tempfile`), `rust/tui-check/src/main.rs`
- Modify: `rust/tui-cases/cases.json` (optional per-case `"platforms": ["unix"]`), `rust/tui-cases/README.md` (new)

**Interfaces:**
- Consumes: `rust/tui-cases/cases.json` and `expected/*.json` exactly as `harness.py` writes them (fields `name, args, env, keys, fixture, read, normalize, nohash, quiet`; snapshot JSON `{exit, snaps:[{after, screen}], work, files}`).
- Produces: `tui-check check <build> [--case NAME]... [--verbose]` with the same output lines as `harness.py` (`FAIL <name>`, `N/M tui cases passed`, exit 0 only when all pass). Behaviour mirrors `harness.py`: 80×24 pty, fixture with mtime 1700000000, env reset to the same keys (TERM `screen-256color`, as recorded), named keys as the VT bytes tmux sends in application-cursor mode, settle = no change for `quiet` (default 0.3 s), GLOBAL_NORMALIZE + per-case normalize, work-tree hashes with `nohash`. On Windows it also applies `WINDOWS_NORMALIZE` to expected and actual alike: permission strings `[-dlcbsp?][-rwxsStTl]{9}` → `<PERM>`; detail-mode octal column; directory sizes; the `N-INODE` hard-link suffix; `\` → `/` in `<TMP>`; and it skips cases whose `platforms` excludes `windows`.

- [ ] **Step 1:** Mark the Unix-only cases `"platforms": ["unix"]`: `file-info`, `chmod-x`, `du-mode`, `du-apparent`, `unmount-nothing`, `plugin-startup`, `cmd-prompt`, `shell`, `batch-rename`, `edit-selection`, `archive-current`, `quitcd` (reads `.lastd` with a sh-syntax path).
- [ ] **Step 2: The harness must agree with tmux on Linux first:** `cargo run -p tui-check -- check $(pwd)/rust/app/target/debug/nnn` → expected 48/48; and against the C original `… check reference/nnn/nnn` → 48/48. Any disagreement is a harness bug (fix the emulator side, never the expected files).
- [ ] **Step 3: Negative control:** a build with `printent` forced to skip the indicator char (temporary patch, reverted) fails ≥ 1 case.
- [ ] **Step 4:** Commit `"tui-check: cross-platform screen harness (ConPTY/pty + vt100)"`.

### Task 9: Prove it on Windows

**Files:**
- Modify: `rust/tui-cases/cases.json` (Windows-only cases with `"platforms": ["windows"]`), `rust/tui-cases/expected/` (their recordings)

**Interfaces:**
- Consumes: `tui-check` (Task 8), Windows build (Tasks 2–7).

- [ ] **Step 1: Write the Windows-only cases** (recorded with `tui-check record` from the Rust build **on Linux** where the case is OS-independent, else on Windows by hand-verified first run, noted in the case's `"note"`): `win-drive-root` (args `["C:/"]`, keys `h h q` → path stays `C:/`), `win-backslash-arg` (args `["{work}\\dir1"]` → header shows `<TMP>/work/dir1`), `win-copy-overwrite` (select `Gamma`, enter `dir1` where `Gamma` already exists, `p` → "overwrite?" → `n` → file unchanged), `win-resize-in-prompt` (open `n` prompt, harness resizes pty to 100×30, `<Esc> q` → last screen 30 rows, exit 0).
- [ ] **Step 2:** Windows (PowerShell): `cargo run --manifest-path rust/tui-check/Cargo.toml -- check <abs path to rust\app\target\debug\nnn.exe>` → all non-Unix cases pass (36 shared + 4 Windows). Fix the port, never the expected files; a shared case that truly cannot match gets `platforms` + an ADR-024 line with the reason.
- [ ] **Step 3:** Windows: `rust-anything app replay . --build rust\app\target\debug\nnn.exe` → 242/242; `python rust/bytes-check.py rust\app\target\debug\nnn.exe` cannot run (no C original on Windows), so instead `cargo test` (unit + 242 shared app cases through trycmd, which compares bytes) and `cargo clippy --all-targets -- -D warnings` clean.
- [ ] **Step 4:** Linux: five suites + `tui-check` 48/48 still green. **Checkpoint.**
- [ ] **Step 5:** Commit `"Windows: screen cases pass under ConPTY"`.

### Task 10: CI, decisions and docs

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `rust/DECISIONS.md` (ADR-021…027), `rust/MAP.md` (wave W7: Windows), `rust/HANDOFF.md`, `README.md` (Windows install/use section)

**Interfaces:**
- CI jobs: `linux` (ubuntu-latest: apt `build-essential libncurses-dev tmux python3-pyte locales`, generate en_US/tr_TR/de_DE, `make -C reference/nnn`, the five suites + `tui-check`; install rust-anything with `cargo install rust-anything --version 3.3.0`) and `windows` (windows-latest: `cargo install rust-anything --version 3.3.0`, `rust-anything app replay . --build rust\app\target\debug\nnn.exe`, `cargo test`, clippy, `tui-check`).

- [ ] **Step 1:** Write ADRs: 021 standalone repo + submodule; 022 platform layer and where `unsafe` may live (supersedes the "only locale.rs" rule of ADR-014); 023 Windows reference = Rust on Linux under C.UTF-8 (portable locale) + known diffs file; 024 Windows feature matrix (unsupported: FIFO, mounts, chmod; requires Git for Windows for sh-based plugins, batch rename, cmd prompt with POSIX syntax) and any filtered shared cases; 025 Windows paths: `/` separators, WTF-8 bytes, roots, `\\?\` for long paths; 026 native file ops on Windows (prompt texts kept); 027 tui-check harness and its Windows normalizations.
- [ ] **Step 2:** `ci.yml` as above; validate with `actionlint` if installed, else a YAML parse (`python3 -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))" .github/workflows/ci.yml`).
- [ ] **Step 3:** README: install on Windows (`cargo install --path rust/app` or the release binary), recommended terminal (Windows Terminal), what needs Git for Windows, what is unsupported.
- [ ] **Step 4:** Final verification on both OSes (Task 9 Steps 2–4), then `rust-anything map --write`; mark W7 "done <date>" in `rust/MAP.md` with: Windows replay 242/242, tui-check Windows N/N and Linux 48/48, portable-locale oracle result, Linux suites; rewrite `rust/HANDOFF.md` for a fresh session. Report to the user: replay results on both OSes, every known Windows difference (ADR-024), and what needs Git for Windows.
- [ ] **Step 5:** Commit `"CI for Linux and Windows; decisions and docs for the Windows port"`. Creating a GitHub remote and pushing waits for the user's explicit go-ahead (name, visibility, account).
