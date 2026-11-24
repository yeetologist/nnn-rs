# Hardening, Terminal Hygiene & Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the non-feature correctness, rendering, filesystem/sorting performance, reliability, and build-config weaknesses identified in the `nnn-rs` vs `yazi` engineering audit without adding user-facing features or breaking Linux parity.

**Architecture:** Fix bugs and bottlenecks directly within `rust/app`'s existing module boundaries (`curses.rs`, `draw.rs`, `browse.rs`, `sort.rs`, `fileops.rs`, `selection.rs`, `winops.rs`, `du.rs`, `platform/{unix,windows,portable_locale}.rs`, `Cargo.toml`). Keep all `unsafe` strictly inside `platform/{unix,windows}.rs`, avoid adding any new crate dependencies, and preserve Linux byte-identical replay/oracle behavior.

**Tech Stack:** Rust 2024 (`rust/app`), std + existing dependencies (`rustix`, `libc`, `windows-sys`, `unicode-width`, `chrono`, `signal-hook`, `trycmd`, `portable-pty`), `tui-check`, `rust-anything`.

**Spec:** Comparative engineering audit of `D:\Documents\Github\nnn-rs` vs `D:\Documents\Github\yazi` (2026-11-24, strictly non-feature weaknesses). Out of scope: adding new user-facing features, adding an async runtime (e.g. Tokio) or new crate dependencies, or refactoring `Nnn` into a multi-crate workspace.

**Branch:** `fix/hardening-and-perf` (created from `main`).

## Global Constraints

- **Linux stays byte-identical.** After every task, in WSL at `/mnt/d/Documents/Github/nnn-rs` (`export PATH="$HOME/.cargo/bin:$PATH"` first):
  - `cargo test --manifest-path rust/app/Cargo.toml` passes
  - `rust-anything app replay . --build rust/app/target/debug/nnn` = 242/242
  - `python3 rust/bytes-check.py rust/app/target/debug/nnn` = 246/246
  - `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` = 48/48
  - `rust/oracle/run.sh rust/app/target/release/oracle` = 15 "ok"
  - `cargo clippy --manifest-path rust/app/Cargo.toml --all-targets -- -D warnings` and `cargo fmt --manifest-path rust/app/Cargo.toml --check` clean
- **Windows, after every task:**
  - `cargo test --manifest-path rust/app/Cargo.toml -- --include-ignored` passes
  - `rust-anything app replay . --build rust\app\target\debug\nnn.exe` = 242/242
  - `cargo clippy --manifest-path rust/app/Cargo.toml --all-targets -- -D warnings` and `cargo fmt --manifest-path rust/app/Cargo.toml --check` clean
- **Safety & dependency rules:** `unsafe` only in `platform/{unix,windows}.rs`; no `#[allow]` attributes; no new crate dependencies in `rust/app/Cargo.toml`.

## Review Focus

1. **Minimized or tiny terminal (`cols == 0` or `lines <= 4`):** `Curses::resize`, `Curses::refresh`, and `draw.rs` must never panic with division by zero or underflow into negative scroll indices. Pinned by Task 2 `resize_to_zero_cols_and_refresh_does_not_panic`.
2. **CJK wide characters and combining marks overwritten or truncated:** overwriting either half of a 2-cell glyph must clear its partner cell without leaving terminal artifacts, and path/preview truncation must never slice mid-UTF-8 codepoint. Pinned by Task 2 `overwrite_wide_char_halves_clears_orphan` and Task 3 `preview_lines_wraps_multibyte_utf8_cleanly`.
3. **Unknown CSI escape sequences (`Ctrl+Arrow`, `F1-F12`, bracketed paste):** terminal input decoder must consume the complete CSI sequence (`\x1b[...<0x40..=0x7e>`) rather than leaking trailing characters (`1;5C`) as typed commands. Pinned by Task 3 `decode_escape_consumes_unknown_csi`.
4. **Paths containing single quotes `'` or Windows `C:/` + CRLF in selection/batch ops:** shell interpolation must escape `'` without injection, and `parse_edited_sel` must parse Windows drive/UNC paths with `\r\n` endings cleanly. Pinned by Task 6 `opstr_escapes_single_quotes_in_selpath` and `parse_edited_sel_handles_windows_crlf_paths`.
5. **Deeply nested directory trees on Windows (>1000 levels) and `Ctrl-C` during DU:** iterative `rm_entries`/`copy_entry` must not overflow the stack, and DU workers must stop before opening queued directories once interrupted. Pinned by Task 6 `rm_entries_iterative_handles_deep_tree` and Task 7 `du_worker_stops_immediately_on_interrupt`.

---

## File Structure

- Modify `rust/app/src/draw.rs`: fix `NNN_FCOLORS` lookup (`self.browse.fcolors`), guard `xlines <= 4` / `ndents == 0`, fix UTF-8 path header truncation, add `#[cfg(test)] mod tests`.
- Modify `rust/app/src/browse.rs`: fix `term_colors()` for Windows ConPTY / `WT_SESSION` / `COLORTERM`, fix `CTX_INVALID`, fix UTF-8/column-width slicing in `preview_pane` and `preview_lines`.
- Modify `rust/app/src/curses.rs`: fix wide-char overwrite & combining marks, guard `cols == 0` / `lines == 0`, skip untouched `refresh()`, wrap frame emission in DEC Mode 2026 (`?2026h`/`l`), eliminate per-frame full-grid clones when no overlays exist, consume unknown CSI sequences in `decode_escape`.
- Modify `rust/app/src/platform/unix.rs`: drain all queued inotify events and fix bounds check in `DirWatch::changed()`, avoid heap `CString` allocation for short names in `glibc::strcoll`/`strcasecmp`, cache `user_name`/`group_name`.
- Modify `rust/app/src/platform/windows.rs`: normalize `.`/`..` in `verbatim()`, add native `DirWatch` backed by `FindFirstChangeNotificationW`, replace unconditional 4s `console_ctrl` sleep with an exit-completion event wait.
- Modify `rust/app/src/winops.rs`: delegate `DirWatch` to `platform::windows` native watcher on Windows, convert `rm_entries` and `copy_entry` from recursive to iterative.
- Modify `rust/app/src/sort.rs`: borrow slices in `extnorder` instead of `.to_vec()`, precompute fuzzy match scores in `O(N)` before sorting.
- Modify `rust/app/src/platform/portable_locale.rs`: replace per-char `regex::Regex` in `char_width()` with `UnicodeWidthChar::width(c)` + fast codepoint check.
- Modify `rust/app/src/fileops.rs`: fix `parse_edited_sel` for CRLF and Windows drive/UNC roots, escape single quotes in shell command builders, unlink temp file on all exit paths of `cpmv_rename`.
- Modify `rust/app/src/selection.rs`: use `write_all` in `write_once`/`writesel`, escape single quotes in `listselfile`.
- Modify `rust/app/src/du.rs`: check `sh.interrupt` before `platform::stat`/`Dir::open`/`entries()` in workers; lazily allocate/clear DU bitmaps only when DU runs.
- Modify `rust/app/Cargo.toml`: add `[profile.release]` tuning (`lto = "thin"`, `codegen-units = 1`, `strip = true`).

---

### Task 1: Fix `NNN_FCOLORS` lookup and Windows 256-color detection

**Files:**
- Modify: `rust/app/src/draw.rs:552-583`
- Modify: `rust/app/src/browse.rs:369-381`
- Test: `rust/app/src/draw.rs` (`mod tests`), `rust/app/src/browse.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Nnn::browse.fcolors: [u32; C_UND + 1]` (already populated by `init_fcolors` in `browse.rs:905-916`).
- Produces:
  - `draw.rs`: `fcolor_enabled(&self, color: usize) -> bool` (checks `self.browse.fcolors.get(color).is_some_and(|&c| c != 0)`) used by `printent` and `fcolors_mis(&self) -> bool`.
  - `browse.rs`: `fn term_colors_from(term: Option<&[u8]>, colorterm: Option<&[u8]>, wt_session: bool, is_windows: bool) -> i32`, called by `term_colors()`. Returns `256` when `term` contains `256color`/`direct`, `colorterm` contains `truecolor`/`24bit`, `wt_session` is true, or `is_windows` is true (ConPTY); `88` for `88color`; `8` otherwise.

- [ ] **Step 1: Write failing tests**
  - In `draw.rs` `mod tests`:
    - `fcolors_lookup_uses_browse_fcolors_for_file_types`: build an `Nnn` with `browse.fcolors[C_EXE] = 0x2e` and `browse.fcolors[C_MIS] = 0xc4` (leaving `self.fcolors` zeroed for `> CTX_MAX`); assert `nnn.fcolors_mis()` is `true` and `nnn.fcolor_enabled(C_EXE)` is `true`.
  - In `browse.rs` `mod tests`:
    - `term_colors_from_detects_windows_and_colorterm`: assert `term_colors_from(None, None, false, true) == 256`, `term_colors_from(None, Some(b"truecolor"), false, false) == 256`, `term_colors_from(Some(b"xterm"), None, true, false) == 256`, and `term_colors_from(Some(b"vt100"), None, false, false) == 8`.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib fcolors_lookup` and `cargo test --manifest-path rust/app/Cargo.toml --lib term_colors_from`
  - Expected: FAIL (compile error for new helper / assertion failure on `fcolors_mis()`).

- [ ] **Step 3: Implement the fixes**
  - In `draw.rs:558, 581`: read `self.browse.fcolors.get(color as usize)` and `self.browse.fcolors.get(C_MIS as usize)` (via `fcolor_enabled(&self, color: usize) -> bool`).
  - In `browse.rs:370-380`: implement `term_colors_from` and wire `term_colors()` to pass `getenv("TERM")`, `getenv("COLORTERM")`, `getenv("WT_SESSION").is_some()`, and `cfg!(windows)`.

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL.

- [ ] **Step 5: Commit**
  - `git add rust/app/src/draw.rs rust/app/src/browse.rs`
  - `git commit -m "fix(draw,browse): read file colors from browse.fcolors and detect 256-color on Windows"`

---

### Task 2: Fix `curses.rs` wide-char corruption, 0-column resize panic, and per-frame churn/flicker

**Files:**
- Modify: `rust/app/src/curses.rs:205-295, 563-700`
- Modify: `rust/app/src/draw.rs:101-103, 225-255`
- Test: `rust/app/src/curses.rs` (`mod tests`), `rust/app/src/draw.rs` (`mod tests`)

**Interfaces:**
- Produces, in `curses.rs`:
  - `Window::clear_wide_overlap(&mut self, y: i32, x: i32, width: i32)`: before writing `[x..x+width)`, if cell `x` is a continuation cell (`ch.is_empty()`), replaces its preceding base cell with `" "`; if cell `x + width` is a continuation cell of a wide char that started inside `[x..x+width)`, replaces it with `" "`.
  - `Window::addwch`: when `w <= 0` (combining mark), if `self.cx == 0` return `0` without mutating cell 0; otherwise scan backwards from `self.cx - 1` past empty continuation cells to append to the actual base cell.
  - `Curses::resize`: if `lines <= 0 || cols <= 0`, keep valid non-negative dimensions (`0, 0`) and clamp `cy = 0, cx = 0` without underflow.
  - `Curses::refresh`: return early if `self.stdscr.cols <= 0 || self.stdscr.lines <= 0` or if `!self.stdscr.touched && self.overlays.is_empty()`. Borrow `&self.stdscr.cells` directly when `self.overlays.is_empty()` (only copy into `self.front` at the end, avoiding the extra `composed()` clone). Wrap non-empty frame writes in DEC Mode 2026 (`\x1b[?2026h` ... `\x1b[?2026l`).
  - `Curses::redraw_all`: invalidate `self.front`, mark `self.stdscr.touched = true`, and let `refresh()` emit `\x1b[0m\x1b[H\x1b[2J` *inside* the same synchronized update block instead of flushing a blank frame first.
- Produces, in `draw.rs`:
  - `Nnn::onscreen(&self) -> i32`: `(self.xlines - 4).max(1)` so `xlines <= 4` never produces negative scroll math; guard `% self.ndents` when `self.ndents <= 0`.

- [ ] **Step 1: Write failing tests**
  - In `curses.rs` `mod tests`:
    - `resize_to_zero_cols_and_refresh_does_not_panic`: construct a `Curses` with a mock `Term` returning `(0, 0)`, call `resize()` and `refresh()`; must not panic.
    - `overwrite_wide_char_halves_clears_orphan`: put `"中"` (width 2) at `(0, 0)`, then overwrite `(0, 0)` with `"a"` -> assert cell `(0, 1).ch == " "` (not `""`). Put `"中"` at `(0, 0)`, then overwrite `(0, 1)` with `"b"` -> assert cell `(0, 0).ch == " "`.
    - `combining_mark_attaches_to_wide_base_and_ignores_col_zero`: at `cx == 0`, `addwch(0x0301)` leaves cell `(0, 0)` unchanged; after putting `"中"` (`cx == 2`), `addwch(0x0301)` appends `\u{0301}` to cell `(0, 0)` (not `(0, 1)`).
    - `refresh_noop_when_untouched_and_wraps_sync_update`: after an initial `refresh()`, a second `refresh()` with no mutations emits `0` bytes; a dirty `refresh()` starts with `\x1b[?2026h` and ends with `\x1b[?2026l`.
  - In `draw.rs` `mod tests`:
    - `tiny_terminal_onscreen_never_negative`: with `xlines = 2`, assert `nnn.onscreen() >= 1`.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib curses`
  - Expected: FAIL on all 4 new `curses.rs` tests.

- [ ] **Step 3: Implement the fixes in `curses.rs` and `draw.rs`**

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL (verify `tui-cases` 48/48 passes cleanly with `?2026h`/`l`).

- [ ] **Step 5: Commit**
  - `git add rust/app/src/curses.rs rust/app/src/draw.rs`
  - `git commit -m "fix(curses,draw): prevent 0-col panic, wide-char orphans, and redundant redraw flicker"`

---

### Task 3: Fix UTF-8 slicing in path headers/previews and unknown CSI escape leaks

**Files:**
- Modify: `rust/app/src/draw.rs:709-748`
- Modify: `rust/app/src/browse.rs:1215-1231, 1263-1293`
- Modify: `rust/app/src/curses.rs:820-885`
- Test: `rust/app/src/draw.rs`, `rust/app/src/browse.rs`, `rust/app/src/curses.rs`

**Interfaces:**
- Produces:
  - `browse.rs`: `fn take_cols(s: &[u8], max_cols: usize) -> (&[u8], usize)` — returns the longest valid UTF-8 prefix whose terminal display width (`wcwidth`) fits in `max_cols` (consuming at least one character if `max_cols > 0` and the first char is wider than `max_cols` to guarantee progress), plus its byte length. Used by `preview_pane` (dir entries, symlink targets) and `preview_lines` (wrapping loop).
  - `draw.rs`: path-header shortening takes the first full UTF-8 character (not 1 raw byte) after each `/` and measures display width via `wcwidth` instead of raw byte length.
  - `curses.rs`: `Decoded::Ignore(usize)` (or `Input::None` consumed in `get_wch`) for complete unrecognized CSI sequences `\x1b[` followed by parameter bytes `0x30..=0x3f`, intermediate bytes `0x20..=0x2f`, and a final byte `0x40..=0x7e`. Returns `Decoded::Incomplete` while still inside parameter/intermediate bytes (capped at 32 bytes).

- [ ] **Step 1: Write failing tests**
  - In `browse.rs` `mod tests`:
    - `take_cols_respects_utf8_and_cjk_width`: `take_cols("日本語abc".as_bytes(), 5)` returns `("日本".as_bytes(), 6)` (width 4 <= 5, never splits a 3-byte char); `take_cols("éé".as_bytes(), 1)` returns `("é".as_bytes(), 2)`.
    - `preview_lines_wraps_multibyte_utf8_cleanly`: run `preview_lines` on `"日本語テスト\n"` with `pw = 4`; assert row 1 contains `"日本"` and row 2 contains `"語テ"` with no U+FFFD replacement characters.
  - In `draw.rs` `mod tests`:
    - `shorten_path_preserves_multibyte_initial_char`: shorten `"/日本語/フォルダ/file.txt"` to width 20; assert every output cell is valid UTF-8 (`"/日/フ/file.txt"`), never split mid-character.
  - In `curses.rs` `mod tests`:
    - `decode_escape_consumes_unknown_csi`: assert `decode_escape(b"\x1b[1;5C")` consumes all 6 bytes as ignored (not `NotEscape`), `decode_escape(b"\x1b[1;5")` returns `Incomplete`, and `decode_escape(b"\x1b[200~")` consumes all 6 bytes.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib take_cols` and `decode_escape_consumes_unknown_csi`
  - Expected: FAIL.

- [ ] **Step 3: Implement `take_cols`, path-header UTF-8 shortening, and CSI consumption in `curses.rs`**

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL.

- [ ] **Step 5: Commit**
  - `git add rust/app/src/draw.rs rust/app/src/browse.rs rust/app/src/curses.rs`
  - `git commit -m "fix(curses,draw,browse): respect UTF-8 column widths and consume unknown CSI sequences"`

---

### Task 4: Fix filesystem watching (Unix inotify drain + Windows native watcher), `verbatim()`, and Windows close stall

**Files:**
- Modify: `rust/app/src/platform/unix.rs:455-487`
- Modify: `rust/app/src/platform/windows.rs:91-114, 645-690`
- Modify: `rust/app/src/winops.rs:425-454`
- Test: `rust/app/src/platform/unix.rs`, `rust/app/src/platform/windows.rs`, `rust/app/src/winops.rs`

**Interfaces:**
- Produces, in `platform/unix.rs`:
  - `DirWatch::changed(&mut self) -> bool`: loops `rustix::io::read(&self.fd, &mut buf)` until `EAGAIN`/`WouldBlock` to drain all queued events, fixes bounds check to `ptr + 16 + len as usize > n`, and returns `true` if any drained event matched `INOTIFY_MASK`.
- Produces, in `platform/windows.rs`:
  - `fn normalize_verbatim_components(b: &[u8]) -> Vec<u8>` used by `verbatim(b: &[u8])`: resolves `.` and `..` path segments before prepending `\\?\` or `\\?\UNC\`.
  - `pub struct NativeDirWatch` wrapping `FindFirstChangeNotificationW` (`FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE`) with non-blocking `changed(&mut self) -> bool` via `WaitForSingleObject(handle, 0) == WAIT_OBJECT_0` + `FindNextChangeNotification`, and `Drop` calling `FindCloseChangeNotification`.
  - `pub fn signal_exit_complete()`: sets a static `EXIT_DONE` Win32 event so `console_ctrl` waits `WaitForSingleObject(exit_done, 4000)` instead of sleeping 4s unconditionally.
- Produces, in `winops.rs`:
  - `DirWatch` uses `platform::windows::NativeDirWatch` on `#[cfg(windows)]` (falling back to mtime check only if `FindFirstChangeNotificationW` fails), avoiding `fs::read_dir().count()` every 2 seconds.

- [ ] **Step 1: Write failing tests**
  - In `platform/windows.rs` `mod tests`:
    - `verbatim_normalizes_dot_and_dotdot_on_long_paths`: pass a 280-byte path containing `/a/../b/./c`; assert output contains `\b\c` and contains neither `\..\` nor `\.\`.
  - In `winops.rs` `mod tests` (`#[cfg(windows)]`):
    - `dir_watch_detects_create_rename_and_modify`: create a temp dir, construct `DirWatch::new`, assert `changed() == false`; create a file -> `changed() == true`; next call `changed() == false`; rename the file in-place (entry count unchanged) -> `changed() == true`.
  - In `platform/unix.rs` `mod tests` (`#[cfg(unix)]`):
    - `dir_watch_drains_multiple_events_in_one_call`: watch a temp dir, create 3 files rapidly, call `changed()` once (returns `true`), then immediately call `changed()` again (returns `false` because queue was drained).

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib verbatim_normalizes` and `dir_watch_detects_create_rename_and_modify`
  - Expected: FAIL (`verbatim` leaves `..` intact; old `DirWatch` misses in-place rename when mtime granularity matches).

- [ ] **Step 3: Implement the fixes in `platform/unix.rs`, `platform/windows.rs`, and `winops.rs`** (and call `platform::signal_exit_complete()` at process exit in `main.rs` on Windows).

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL.

- [ ] **Step 5: Commit**
  - `git add rust/app/src/platform/unix.rs rust/app/src/platform/windows.rs rust/app/src/winops.rs rust/app/src/main.rs`
  - `git commit -m "fix(platform,winops): drain inotify, use native Windows DirWatch, normalize verbatim paths"`

---

### Task 5: Eliminate sorting, locale `wcwidth`, and `getpwuid_r` allocation hotspots

**Files:**
- Modify: `rust/app/src/sort.rs:225-279` (and its callers in `dir.rs` / `browse.rs` where fuzzy sort is invoked)
- Modify: `rust/app/src/platform/unix.rs:163-190, 627-654`
- Modify: `rust/app/src/platform/portable_locale.rs:195-223`
- Test: `rust/app/src/sort.rs`, `rust/app/src/platform/portable_locale.rs`, `rust/app/src/platform/unix.rs`

**Interfaces:**
- Produces:
  - `sort.rs`: `extnorder` borrows `&n[i..]` slices (zero heap allocs); add `pub fn sort_fuzzy(&self, entries: &mut [Entry], filter: &[u8], ic: bool)` that precomputes `Vec<(i32, usize)>` or caches scores in `O(N)` before sorting (preserving exact `fuzzyentrycmp` ordering, keeping `fuzzyentrycmp` public for oracle compatibility).
  - `platform/unix.rs`: `glibc::with_cstr<R>(s: &[u8], f: impl FnOnce(*const c_char) -> R) -> R` using a 256-byte stack buffer for names `<= 255` bytes (falling back to `CString` for longer names); cache `uid -> Vec<u8>` and `gid -> Vec<u8>` in a `Mutex<HashMap<u32, Vec<u8>>>` (or `OnceLock`) inside `user_name`/`group_name`.
  - `platform/portable_locale.rs`: `char_width(c: char) -> Option<usize>` uses `UnicodeWidthChar::width(c)` directly (handling `'\u{ad}' => Some(1)` and unassigned ranges without invoking `regex::Regex`).

- [ ] **Step 1: Write failing tests**
  - In `sort.rs` `mod tests`:
    - `sort_fuzzy_matches_fuzzyentrycmp_order`: build 20 entries (dirs + files, mixed case), sort one clone with `sort_by(|a, b| sort.fuzzyentrycmp(filter, ic, a, b))` and another with `sort.sort_fuzzy(&mut entries, filter, ic)`; assert identical order for both `reverse = false` and `reverse = true`.
  - In `platform/portable_locale.rs` `mod tests`:
    - `char_width_matches_glibc_c_utf8`: assert `wcwidth('a' as i32) == 1`, `wcwidth('中' as i32) == 2`, `wcwidth('\u{ad}' as i32) == 1`, `wcwidth('\u{0301}' as i32) == 0`, `wcwidth('\u{0378}' as i32) == -1` (unassigned).
  - In `platform/unix.rs` `mod tests`:
    - `strcoll_handles_short_and_long_names`: test `strcoll` and `strcasecmp` with both short (`< 256`) and long (`300`-byte) names and names containing embedded NUL.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib sort_fuzzy_matches`
  - Expected: FAIL (`sort_fuzzy` not defined).

- [ ] **Step 3: Implement the optimizations in `sort.rs`, `platform/unix.rs`, and `platform/portable_locale.rs`** (and switch the `sort_by(|a, b| ... fuzzyentrycmp ...)` call sites in `dir.rs`/`browse.rs` to `sort_fuzzy`).

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL (verify `oracle` 15/15 and `bytes-check.py` 246/246 pass).

- [ ] **Step 5: Commit**
  - `git add rust/app/src/sort.rs rust/app/src/dir.rs rust/app/src/browse.rs rust/app/src/platform/unix.rs rust/app/src/platform/portable_locale.rs`
  - `git commit -m "perf(sort,platform): remove per-comparison allocs, cache uid/gid, drop regex in char_width"`

---

### Task 6: Harden `fileops.rs`, `selection.rs`, `winops.rs`, and `CTX_INVALID`

**Files:**
- Modify: `rust/app/src/fileops.rs:163-245, 340-355, 425-503`
- Modify: `rust/app/src/selection.rs:24-28, 99-120, 190-206`
- Modify: `rust/app/src/winops.rs:115-145, 221-238`
- Modify: `rust/app/src/browse.rs:47`, `rust/app/src/draw.rs:35`
- Test: `rust/app/src/fileops.rs`, `rust/app/src/selection.rs`, `rust/app/src/winops.rs`, `rust/app/src/browse.rs`

**Interfaces:**
- Produces:
  - `fileops.rs`:
    - `fn push_sh_single_quoted(dst: &mut Vec<u8>, s: &[u8])`: appends `'...'` with every interior `'` escaped as `'\''`. Used in `opstr`, `rmmulcmd`, `p_cpmvfmt`, `p_cpmvrnm`, `batchrenamecmd`, `cpmv_rename`, and `selection.rs::listselfile`.
    - `fn parse_edited_sel(buf: Vec<u8>) -> (Vec<u8>, i32)`: strips `\r` before `\n`, splits paths on `\n` followed by `/` or (on Windows) `<ASCII letter>:/` or `<ASCII letter>:\`.
    - `struct TempGuard(Vec<u8>)`: `Drop` removes the temp file so `cpmv_rename` never leaks `tmp` on early return.
  - `selection.rs`: `write_once` and `writesel` use `f.write_all(buf)` instead of a single `f.write(buf)`.
  - `winops.rs`: `rm_entries` and `copy_entry` use an explicit `Vec` work stack (no recursion).
  - `browse.rs` & `draw.rs`: `CTX_INVALID = CTX_MAX` (8, out of `0..CTX_MAX` range) instead of `0b111` (7).

- [ ] **Step 1: Write failing tests**
  - In `fileops.rs` `mod tests`:
    - `parse_edited_sel_handles_windows_crlf_paths`: pass `b"C:/foo/a.txt\r\nC:/foo/b.txt\r\n".to_vec()`; assert `(b"C:/foo/a.txt\0C:/foo/b.txt\0", 2)`.
    - `opstr_escapes_single_quotes_in_selpath`: pass `selpath = b"/tmp/a'b/.selection"`; assert output contains `'/tmp/a'\''b/.selection'` and no unescaped `'`.
  - In `selection.rs` `mod tests`:
    - `write_once_handles_short_writes`: pass a custom `Write` that writes at most 2 bytes per `write()` call; assert `write_once(&mut w, b"abcdef")` returns `true` and captures `b"abcdef"`.
  - In `winops.rs` `mod tests`:
    - `rm_entries_iterative_handles_deep_tree`: create a 60-level nested temp dir; assert `rm_entries` returns all 61 entries in post-order (children before parents).
  - In `browse.rs` `mod tests`:
    - `ctx_invalid_does_not_collide_with_valid_contexts`: assert `CTX_INVALID >= CTX_MAX`.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib parse_edited_sel_handles_windows` and `write_once_handles_short_writes`
  - Expected: FAIL.

- [ ] **Step 3: Implement the fixes in `fileops.rs`, `selection.rs`, `winops.rs`, `browse.rs`, and `draw.rs`** (Note: check `oracle_fileops` for `opstr` / `p_cpmvfmt` when `selpath` has no `'`; `push_sh_single_quoted` produces identical output for paths without `'` so Linux oracle stays 15/15).

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL.

- [ ] **Step 5: Commit**
  - `git add rust/app/src/fileops.rs rust/app/src/selection.rs rust/app/src/winops.rs rust/app/src/browse.rs rust/app/src/draw.rs`
  - `git commit -m "fix(fileops,selection,winops): escape shell quotes, fix CRLF/short writes, make winops iterative"`

---

### Task 7: Responsive DU cancellation and lazy DU bitmap allocation

**Files:**
- Modify: `rust/app/src/du.rs:195-270, 510-540`
- Test: `rust/app/src/du.rs` (`mod tests`)

**Interfaces:**
- Produces, in `du.rs`:
  - Worker loop checks `sh.interrupt.load(Ordering::Relaxed)` immediately after popping a task and before `platform::stat`, `Dir::open`, and `dfd.entries()`, so `Ctrl-C` aborts queued work without extra syscalls.
  - `DuState` allocates the 4 MiB inode/dir bitmaps lazily on first DU run and clears/drops them on `DuState::release()` when leaving DU mode (zero 4 MiB baseline RAM when DU is never used).

- [ ] **Step 1: Write failing tests**
  - In `du.rs` `mod tests`:
    - `du_worker_stops_immediately_on_interrupt`: set `sh.interrupt.store(true, Ordering::Relaxed)` before calling `du_walk_dir` on a populated temp dir; assert `tfiles == 0` and `tblocks == 0`.
    - `du_state_default_does_not_allocate_bitmaps`: assert `DuState::default()` holds empty bitmap vecs until DU is started.

- [ ] **Step 2: Run tests to verify they fail**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib du_worker_stops_immediately`
  - Expected: FAIL (`du_walk_dir` currently runs `stat` + `count_root` and `Dir::open` before checking `interrupt`).

- [ ] **Step 3: Implement early interrupt check and lazy bitmap allocation in `du.rs`**

- [ ] **Step 4: Verify**
  - Run: `cargo test --manifest-path rust/app/Cargo.toml --lib du`
  - Expected: PASS, plus Global Constraints checks on Windows and WSL.

- [ ] **Step 5: Commit**
  - `git add rust/app/src/du.rs`
  - `git commit -m "perf(du): short-circuit worker on Ctrl-C interrupt and lazily allocate DU bitmaps"`

---

### Task 8: Tune release build profile in `Cargo.toml` and run full verification

**Files:**
- Modify: `rust/app/Cargo.toml`

**Interfaces:**
- Produces, in `rust/app/Cargo.toml`:
  ```toml
  [profile.release]
  codegen-units = 1
  lto = "thin"
  strip = true
  ```
  (Keep default `panic = "unwind"` so terminal state restoration and `Drop` guards run cleanly on panic across both Unix and Windows.)

- [ ] **Step 1: Add `[profile.release]` to `rust/app/Cargo.toml`**

- [ ] **Step 2: Build release binary and run full Windows + WSL verification suites**
  - Windows:
    - `cargo build --release --manifest-path rust/app/Cargo.toml`
    - `cargo test --manifest-path rust/app/Cargo.toml -- --include-ignored`
    - `rust-anything app replay . --build rust\app\target\debug\nnn.exe` (242/242)
    - `cargo clippy --manifest-path rust/app/Cargo.toml --all-targets -- -D warnings`
    - `cargo fmt --manifest-path rust/app/Cargo.toml --check`
  - WSL (`/mnt/d/Documents/Github/nnn-rs`):
    - `cargo build --release --features oracles --manifest-path rust/app/Cargo.toml`
    - `cargo test --manifest-path rust/app/Cargo.toml`
    - `rust-anything app replay . --build rust/app/target/debug/nnn` (242/242)
    - `python3 rust/bytes-check.py rust/app/target/debug/nnn` (246/246)
    - `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` (48/48)
    - `rust/oracle/run.sh rust/app/target/release/oracle` (15 "ok")
    - `cargo clippy --manifest-path rust/app/Cargo.toml --all-targets -- -D warnings`
    - `cargo fmt --manifest-path rust/app/Cargo.toml --check`

- [ ] **Step 3: Commit**
  - `git add rust/app/Cargo.toml`
  - `git commit -m "build: add LTO, single codegen unit, and strip to release profile"`
