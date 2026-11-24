# Windows: Built-in Drive Picker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a built-in, zero-dependency Windows drive picker to `nnn-rs` so users can jump between logical drives (`C:/`, `D:/`, USB, network shares) without leaving `nnn`, while keeping `h` / `Left` stopping at the current drive root (`C:/`) just like Unix `nnn` stops at `/` (ADR-025, `win-drive-root`).

**Architecture:**
- **OS boundary (`platform/windows.rs`, `platform/unix.rs`, `platform/mod.rs`):** A small `DriveInfo` / `DriveKind` model and `platform::list_drives() -> Vec<DriveInfo>`. On Windows, it enumerates logical drives using `GetLogicalDrives()` + `GetDriveTypeW` + `GetVolumeInformationW` + `GetDiskFreeSpaceExW` under a `SetThreadErrorMode(SEM_FAILCRITICALERRORS, ...)` RAII guard (the pattern used in `yazi-fs/src/mounts/windows.rs` so empty removable/CD-ROM drives never trigger a Windows "Insert a disk" modal dialog). Network drives (`DRIVE_REMOTE`) skip `GetVolumeInformationW`/`GetDiskFreeSpaceExW` so disconnected SMB shares never stall the UI for 15–30 s. On Unix, `list_drives()` returns an empty `Vec` and is unused.
- **OS-neutral picker UI (`rust/app/src/drives.rs`):** Compiled on every OS so all formatting, cursor movement, scrolling, letter-jump, and fallback logic are unit-tested on both Linux and Windows (just like `platform/tools.rs` and `popup::floating_rows`). Renders a centered bordered `curses::Window` (reusing `popup::floating_geometry` / `newwin_fits` and `fmt::coolsize`) showing drive root, type tag, volume label, filesystem, and free/total space, with:
  - `j`/`k`/`Up`/`Down`/`PgUp`/`PgDn`/`Home`/`End` to move, `Enter`/`l`/`Right` to open
  - Direct single-key letter jump (`a`–`z` / `A`–`Z` jumps immediately to that drive if present)
  - `q`/`Esc`/`h`/`Left` to close without changing directory
  - Automatic fallback to a compact status-bar prompt (`drives: C D E > `) if the terminal is too small for a floating window (`xlines < 7 || xcols < 24`)
- **Keybinding & navigation (`keys.rs`, `browse.rs`, `popup.rs`):**
  - Bind `D` (`Key::Char(b'D')`, currently unbound in `nnn`) to `Action::Drives` under `#[cfg(windows)]` so Linux's `BINDINGS` (94 entries), `-K`, and `HELPSTR` remain 100% byte-identical.
  - Pre-select the current drive in the picker; selecting a drive runs `chdir` + `cdprep(true, &root)` (updating `c_last`, so `-` jumps right back to the previous directory on the old drive).
  - Bonus fix in `sel_cd`: `` ` `` (`Action::Cdroot`) uses `platform::current_root(&path)` (`C:/` or `//server/share/` on Windows, `/` on Unix) instead of hardcoded `b"/"`, which is invalid as a Windows canonical path under ADR-025.

**Tech Stack:** Rust (`nnn-rs` `rust/app`), `windows-sys 0.61` (no new crates; add `Win32_System_Diagnostics_Debug` and `Win32_System_WindowsProgramming` features under `[target.'cfg(windows)'.dependencies]`), existing `curses::Window` + `tui-check` harness.

**Reference:** `D:\Documents\Github\yazi\yazi-fs\src\mounts\windows.rs` (lines 32–36, 120–176) for `SetThreadErrorMode(SEM_FAILCRITICALERRORS)` + `GetLogicalDrives` / `GetDriveTypeW` / `GetVolumeInformationW` / `GetDiskFreeSpaceExW`, and `rust/DECISIONS.md` ADR-022, ADR-024, ADR-025, ADR-027.

---

## Global Constraints

- **Linux stays byte-identical.** After every task, in WSL at `/mnt/d/Documents/Github/nnn-rs`:
  - `cargo test` in `rust/app`
  - `rust-anything app replay . --build rust/app/target/debug/nnn` = 242/242
  - `python3 rust/bytes-check.py rust/app/target/debug/nnn` = 246/246
  - `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` = 48/48
  - `rust/oracle/run.sh rust/app/target/release/oracle` = 15/15 "ok"
- **`h` / `Left` at `C:/` stays unchanged** (`win-drive-root` remains green).
- **`unsafe` only in `platform/{unix,windows}.rs`**, each block with a `// SAFETY:` comment (ADR-022).
- **No `#[allow]`**; `#[expect(.., reason = "..")]` only when unavoidable.
- **No new crate dependencies** (only enabling two existing `windows-sys` features).
- **`cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`** clean on both OSes.
- **Windows verification** after every task: `cargo test`, `rust-anything app replay` 242/242, `tui-check` (both with `NNN_COREUTILS=0` and `--coreutils`).
- Nothing is pushed without the user's OK.

## Review Focus

1. **No modal dialogs on empty removable/CD-ROM drives:** `SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut prev)` is set before probing any drive and restored in a `Drop` guard so even panics/early returns restore the thread error mode.
2. **No 30-second UI freeze on disconnected network drives:** `GetDriveTypeW` runs before `GetVolumeInformationW` / `GetDiskFreeSpaceExW`; for `DRIVE_REMOTE` (and `DRIVE_NO_ROOT_DIR`), metadata/space queries are skipped so the picker opens in < 5 ms.
3. **Unready/inaccessible drive selected cleanly:** Selecting an empty CD-ROM or offline network drive fails `chdir` and reports nnn's `printwarn` without corrupting `c_path`, `c_last`, or directory state.
4. **History (`-` / `Cdlast`) works across drives:** Jumping from `C:/Users/Alice` to `D:/` via the picker saves `C:/Users/Alice` in `c_last`, so `-` returns there.
5. **Small terminals never panic or misdraw:** When `xlines < 7 || xcols < 24` (or `newwin_fits` is false), the picker falls back to a single-line status-bar drive-letter prompt (`drives: C D E`).
6. **Linux zero-diff:** `Action::Drives`, `Key::Char(b'D')`, and the `HELPSTR` entry are `#[cfg(windows)]`-gated; `drives.rs` compiles on Linux for unit testing only (or with `#[cfg_attr(all(not(windows), not(test)), expect(dead_code, ...))]`, matching `platform/tools.rs`).

---

## File Structure

- Modify `rust/app/Cargo.toml`: add `Win32_System_Diagnostics_Debug` and `Win32_System_WindowsProgramming` to Windows `windows-sys` features.
- Modify `rust/app/src/platform/mod.rs`: define `DriveKind`, `DriveInfo`, `pub fn list_drives() -> Vec<DriveInfo>`, and `pub fn current_root_with(os: Os, path: &[u8]) -> Vec<u8>` / `pub fn current_root(path: &[u8]) -> Vec<u8>`.
- Modify `rust/app/src/platform/windows.rs`: implement `ErrorModeGuard` (`SetThreadErrorMode`) and `list_drives()` + Windows unit tests.
- Modify `rust/app/src/platform/unix.rs`: stub `list_drives() -> Vec<DriveInfo>` returning `Vec::new()`.
- Create `rust/app/src/drives.rs`: OS-neutral formatting, picker state (`DrivePickerState`), key handling (`drive_key`), floating-window renderer (`paint_drive_picker`), testable `drive_picker_rows(...)`, and `Nnn::pick_drive(&mut self, cur_path: &[u8]) -> Option<Vec<u8>>` (with small-screen status-bar fallback).
- Modify `rust/app/src/lib.rs`: declare `pub mod drives;`.
- Modify `rust/app/src/keys.rs`: add `#[cfg(windows)] Drives` to `Action` and `(Key::Char(b'D'), Action::Drives)` to `BINDINGS`; update `no_collisions_and_lookup` length assertion (`#[cfg(windows)]` 95, `#[cfg(not(windows))]` 94).
- Modify `rust/app/src/browse.rs`: handle `Action::Drives` (call `pick_drive`, `chdir`, `cdprep`, watch/filter) and fix `Action::Cdroot` to use `platform::current_root(&path)`.
- Modify `rust/app/src/popup.rs`: expose `floating_geometry` / `newwin_fits` as `pub(crate)` (for `drives.rs`), and add `D  Drives` to `HELPSTR` on `#[cfg(windows)]` only.
- Modify `rust/tui-cases/cases.json` + create `rust/tui-cases/expected/win-drive-picker.json`: Windows-only ConPTY screen test (`D` → `Esc` cancels; `D` → `c` jumps to `C:/`; `-` returns).
- Modify `rust/DECISIONS.md` (add ADR-030), `rust/MAP.md`, and `README.md`.

---

### Task 1: Platform drive enumeration & `current_root` helper

**Files:**
- Modify: `rust/app/Cargo.toml`
- Modify: `rust/app/src/platform/mod.rs`
- Modify: `rust/app/src/platform/windows.rs`
- Modify: `rust/app/src/platform/unix.rs`

**Interfaces:**
- In `platform/mod.rs`:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum DriveKind {
      Fixed,
      Removable,
      Remote,
      CdRom,
      RamDisk,
      Unknown,
  }

  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct DriveInfo {
      /// Uppercase ASCII drive letter, e.g. `b'C'`.
      pub letter: u8,
      /// Canonical internal root path, e.g. `b"C:/"`.
      pub root: Vec<u8>,
      pub kind: DriveKind,
      /// Volume label (UTF-8 / WTF-8 bytes), empty if unavailable or skipped.
      pub label: Vec<u8>,
      /// Filesystem name (e.g. `b"NTFS"`, `b"exFAT"`), empty if unavailable or skipped.
      pub fs_type: Vec<u8>,
      /// `(avail_bytes, total_bytes)` when `GetDiskFreeSpaceExW` succeeds.
      pub space: Option<(u64, u64)>,
  }
  ```
  - `pub fn list_drives() -> Vec<DriveInfo>` — delegates to `backend::list_drives()`.
  - `pub fn current_root_with(os: Os, path: &[u8]) -> Vec<u8>` — on `Os::Unix` returns `b"/".to_vec()`; on `Os::Windows` returns `&path[..n]` when `n = root_len_with(Os::Windows, path) > 0` (ensuring trailing `/` for `//server/share`), falling back to `b"/"` if relative.
  - `pub fn current_root(path: &[u8]) -> Vec<u8>` — calls `current_root_with(Os::CURRENT, path)`.
- In `platform/windows.rs`:
  - `struct ErrorModeGuard { prev: u32 }` calling `SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut prev)` on creation and `SetThreadErrorMode(self.prev, ptr::null_mut())` on `Drop` (both with `// SAFETY:` comments).
  - `pub fn list_drives() -> Vec<DriveInfo>`:
    1. Acquire `ErrorModeGuard`.
    2. Call `GetLogicalDrives()`. Iterate `i in 0..26u8`; skip if `mask & (1 << i) == 0`.
    3. Build wide root `[b'A' as u16 + i as u16, b':' as u16, b'\\' as u16, 0]`.
    4. Call `GetDriveTypeW(root.as_ptr())` and map to `DriveKind`. Skip `DRIVE_NO_ROOT_DIR`.
    5. Unless `kind == DriveKind::Remote`, call `GetVolumeInformationW` (261-u16 buffers for label and fs_type, converted via `OsString::from_wide` → `bytes`) and `GetDiskFreeSpaceExW` (for `avail` / `total`).
    6. Push `DriveInfo { letter: b'A' + i, root: vec![b'A' + i, b':', b'/'], kind, label, fs_type, space }`.

- [ ] **Step 1: Write failing unit tests** in `platform/mod.rs` (testing `current_root_with` for Unix `/a/b`, Windows `C:/Users/x`, `D:/`, `//srv/share/dir`, `//srv/share`) and in `platform/windows.rs` (`list_drives_includes_system_drive`, `list_drives_sorted_and_well_formed`).
- [ ] **Step 2:** Run `cargo test --manifest-path rust/app/Cargo.toml` and confirm compilation failure for missing symbols.
- [ ] **Step 3:** Add the two `windows-sys` features to `Cargo.toml` and implement `DriveKind`, `DriveInfo`, `current_root_with`, `current_root`, `ErrorModeGuard`, and `list_drives` in `platform/{mod,windows,unix}.rs`.
- [ ] **Step 4: Verify** on Windows (`cargo test`, `clippy`, `fmt`) and WSL (all 5 suites).
- [ ] **Step 5:** Commit `"platform: add Windows logical drive enumeration and current_root"`.

---

### Task 2: OS-neutral drive picker UI (`rust/app/src/drives.rs`)

**Files:**
- Create: `rust/app/src/drives.rs` (unit tests in the same file)
- Modify: `rust/app/src/lib.rs` (`pub mod drives;`)
- Modify: `rust/app/src/popup.rs` (make `floating_geometry` and `newwin_fits` `pub(crate)`)

**Interfaces:**
- Pure, testable state & rendering in `drives.rs`:
  - `pub fn format_drive_row(d: &DriveInfo, cur_letter: Option<u8>, label_w: usize, fs_w: usize) -> Vec<u8>`
    - Formats e.g. `> C:/  [Local]  Windows  NTFS   120G / 476G` (using `fmt::coolsize` for sizes; omits size column when `space` is `None`, e.g. unready USB/CD-ROM or Network; marks the active drive with `*` or `>`).
  - `pub enum PickerOutcome { Stay, Cancel, Pick(Vec<u8>) }`
  - `pub struct DrivePicker { drives: Vec<DriveInfo>, cur: usize, scroll: usize, cur_letter: Option<u8> }`
    - `DrivePicker::new(drives: Vec<DriveInfo>, cur_path: &[u8]) -> Self` — extracts `cur_letter` from `cur_path` (uppercase ASCII if `X:/...`) and initializes `cur` to the matching drive index (or `0`).
    - `pub fn handle_key(&mut self, ch: i32, max_rows: usize) -> PickerOutcome`:
      - `q`, `Esc`, `h`, `KEY_LEFT`, `KEY_RESIZE` -> `Cancel`
      - `Enter` (`\r`, `\n`, `KEY_ENTER`), `l`, `KEY_RIGHT` -> `Pick(self.drives[self.cur].root.clone())`
      - `j`, `KEY_DOWN` -> move `cur` down (with wrap or clamp matching `nnn` rollover? Clamp to `0..len-1`, keeping `scroll` in view)
      - `k`, `KEY_UP` -> move `cur` up
      - `KEY_HOME`, `g` -> `0`; `KEY_END`, `G` -> `len - 1`; `KEY_PPAGE` / `KEY_NPAGE` -> page up/down
      - ASCII letter `a`..=`z` or `A`..=`Z` (excluding navigation keys? **Design note:** Wait — `h`, `j`, `k`, `l`, `g`, `q` are letters! If a user has a `G:/` or `H:/` or `J:/` drive, what should pressing `h`/`j`/`k`/`l`/`q` do?
        - **Clean rule:** Reserve lowercase `h`/`j`/`k`/`l`/`q` for vim navigation/close, and allow **uppercase** `A`–`Z` for ANY drive letter (including `G`, `H`, `J`, `K`, `L`, `Q`), PLUS lowercase `a`–`z` for all other letters (`c`, `d`, `e`, `f`, etc.)! That way `D` then `c` or `d` works with one lowercase tap, vim keys `j`/`k`/`l`/`h`/`q` always behave as expected, and `G`/`H`/`J`/`K`/`L`/`Q` drives are still directly jumpable with Shift!)
  - `pub fn paint_drive_picker(win: &mut Window, picker: &DrivePicker)` — draws box border, title `Drives (N)`, visible rows with `A_REVERSE` on `picker.cur`, scroll arrows `^`/`v` if `drives.len() > max_rows`, and footer hint `j/k:move  Enter/A-Z:open  q/Esc:close`.
  - `pub fn drive_picker_rows(xlines: i32, xcols: i32, drives: &[DriveInfo], cur_path: &[u8], keys: &[i32]) -> (Option<Vec<String>>, Option<Vec<u8>>)` — headless test helper (same pattern as `popup::floating_rows`).
  - `impl Nnn { pub fn pick_drive(&mut self, cur_path: &[u8]) -> Option<Vec<u8>> }`:
    - Calls `platform::list_drives()`. If empty, `self.printmsg(b"no drives")` and returns `None`.
    - Computes geometry from `popup::floating_geometry(self.xlines, self.xcols)` (tightened height to `(drives.len() + 3).min(20).min(xlines - 4)` so a 3-drive box is compact rather than 20 empty lines!).
    - If `self.xlines < 7 || self.xcols < 24 || !popup::newwin_fits(...)`: falls back to status-bar prompt `drives: C D E > ` via `self.get_input(...)`, matching typed letter case-insensitively.
    - Otherwise runs the `Window` event loop (`paint_drive_picker` -> `wrefresh` -> `get_wch` -> `handle_key`), then `delwin` + `refresh`, returning `Some(root)` or `None`.

- [ ] **Step 1: Write failing unit tests** in `drives.rs`:
  - `preselects_current_drive` (`cur_path = b"D:/Projects"` selects `D:/` in `[C, D, E]`).
  - `nav_keys_move_and_select` (`j`, `k`, `KEY_DOWN`, `KEY_UP`, `Enter`, `l`).
  - `direct_letter_jump` (`c` jumps to `C:/`, `D` jumps to `D:/`, `H` jumps to `H:/`, lowercase `h` cancels, `q`/`Esc` cancels, unknown letter stays).
  - `formats_labels_and_missing_space` (fixed drive with space, network drive without space, long label truncated cleanly to window width).
  - `scrolls_when_more_drives_than_rows` (10 drives in a 9-line terminal).
- [ ] **Step 2:** `cargo test --manifest-path rust/app/Cargo.toml drives` fails.
- [ ] **Step 3:** Implement `drives.rs`, expose `floating_geometry` & `newwin_fits` in `popup.rs`, wire `pub mod drives;` in `lib.rs`.
- [ ] **Step 4: Verify** on Windows and WSL (`cargo test`, `clippy`, `fmt`, Linux suites).
- [ ] **Step 5:** Commit `"drives: add OS-neutral floating drive picker UI and tests"`.

---

### Task 3: Wire `D` keybinding, `Action::Drives`, `Cdroot` fix, and Windows help text

**Files:**
- Modify: `rust/app/src/keys.rs`
- Modify: `rust/app/src/browse.rs`
- Modify: `rust/app/src/popup.rs`

**Interfaces:**
- `keys.rs`:
  - Add `#[cfg(windows)] Drives` variant to `Action`.
  - Add `#[cfg(windows)] (Key::Char(b'D'), Action::Drives),` to `BINDINGS`.
  - Update `no_collisions_and_lookup` expected length (`95` on Windows, `94` on Unix).
- `browse.rs`:
  - In `sel_cd`: replace `_ => b"/".to_vec()` with `_ => platform::current_root(&path)`.
  - Add `#[cfg(windows)] A::Drives => self.sel_drives(b),`:
    ```rust
    #[cfg(windows)]
    fn sel_drives(&mut self, b: &mut Locals) -> Flow {
        let path = self.cpath();
        let Some(dir) = self.pick_drive(&path) else {
            return Flow::Top;
        };
        self.g_state.selbm = false;
        if path == dir {
            return Flow::Top;
        }
        if !chdir(&dir) {
            self.printwarn(Some(&mut b.presel));
            return Flow::Nochange;
        }
        b.newpath = ncpy(&dir, PATH_MAX);
        let np = b.newpath.clone();
        if self.cdprep(true, &np) {
            b.presel = FILTER;
        } else {
            b.watch = true;
        }
        Flow::Begin
    }
    ```
- `popup.rs`:
  - In `HELPSTR` (or `expand_helpstr`), add `D  Drives` under `NAVIGATION` on `#[cfg(windows)]` only, leaving Unix `HELPSTR` byte-identical.

- [ ] **Step 1:** Update `keys.rs`, `browse.rs`, and `popup.rs`.
- [ ] **Step 2:** Verify `-K` reports no key collisions on both Windows and Linux.
- [ ] **Step 3: Verify** on Windows (`cargo test`, replay 242/242, `tui-check`, `clippy`, `fmt`) and WSL (all 5 suites).
- [ ] **Step 4:** Commit `"Windows: bind D to drive picker and fix Cdroot on drive paths"`.

---

### Task 4: Windows screen test (`win-drive-picker`) and documentation

**Files:**
- Modify: `rust/tui-cases/cases.json`
- Create: `rust/tui-cases/expected/win-drive-picker.json`
- Modify: `rust/DECISIONS.md` (add ADR-030: Windows built-in drive picker)
- Modify: `rust/MAP.md`
- Modify: `README.md`

**Details:**
- Add `win-drive-picker` (`platforms: ["windows"]`) to `rust/tui-cases/cases.json`:
  - Starts in `<TMP>/work` (which `tui-check` creates on `C:\nnnh-...`).
  - Keys: `D` -> `Esc` (cancels, still in `<TMP>/work`), `D` -> `c` (jumps to `C:/`, header shows `C:/`, body blanked like `win-drive-root` because host `C:/` entries vary), `-` (returns to `<TMP>/work`), `q`.
  - Record with `tui-check record` and verify with `tui-check check` (both without and with `--coreutils`).
- Document ADR-030 in `rust/DECISIONS.md` (key `D`, `h`/`Left` stays at drive root, `GetLogicalDrives` + `SEM_FAILCRITICALERRORS`, skipping metadata on `DRIVE_REMOTE`, small-terminal fallback, Linux byte-identical).
- Update `rust/MAP.md` and `README.md`.

- [ ] **Step 1:** Add `win-drive-picker` to `cases.json` and record `expected/win-drive-picker.json` on Windows.
- [ ] **Step 2:** Update `DECISIONS.md`, `MAP.md`, and `README.md`.
- [ ] **Step 3: Final verification:**
  - Windows (without Coreutils and with `--coreutils`): `cargo test --include-ignored`, `rust-anything app replay` 242/242, `tui-check`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
  - WSL: `cargo test`, replay 242/242, `bytes-check.py` 246/246, `harness.py check` 48/48, `tui-check` 48/48, `oracle/run.sh` 15/15, `clippy`, `fmt`.
- [ ] **Step 4:** Commit `"Windows: pin drive picker in tui-check and document ADR-030"`.
