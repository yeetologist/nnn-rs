# Windows: detect and use Microsoft Coreutils — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When Microsoft's Coreutils for Windows (https://learn.microsoft.com/en-us/windows/core-utils/overview) is installed, the Windows build of nnn-rs finds it and runs its `cp`, `mv`, `rm`, `stat` and `xargs` exactly where Linux nnn runs GNU's, so copy/move/remove, duplicate, file details and archiving behave as on Linux. Without it, behaviour stays what `origin/main` does today.

**Architecture:** A new, OS-neutral `platform/tools.rs` finds programs on `PATH` natively (replacing the `which` spawn that Windows can't run) and detects Coreutils in its two shipped layouts. `platform::command` rewrites a bare `cp`/`mv`/`rm`/`stat`/`xargs` `argv[0]` to the detected Coreutils binary, so existing Unix call sites work unchanged. Call sites that today go native on Windows (selection copy/move/remove, single remove, the stat popup, archiving) take the Coreutils path when the tool is there. File names always travel as separate `argv` entries, never through `cmd`.

**Tech Stack:** Rust (nnn-rs `rust/app`), `std::process::Command`, the existing `tui-check` harness, GitHub Actions, and Microsoft Coreutils 2026.9.3 (uutils coreutils 0.11.0, findutils xargs 0.10.0).

**Spec:** the user's request of 2026-11-24 ("detect and support windows machines that have Microsoft Core Utils; ignore the windows-coreutils branch") plus `rust/DECISIONS.md` ADR-001…027 on `origin/main`. The facts below were measured on the dev box (2026-11-24) and are binding for the implementation:

- **Installed layout (winget `Microsoft.Coreutils`):** `C:\Program Files\coreutils\bin\` holds one hard-linked multi-call binary under 79 names, `cp.exe … xargs.exe` plus `coreutils-manager.exe`. `coreutils-manager disable <tool>` removes single names.
- **Portable layout (release zip `coreutils-<ver>-x64.zip`):** a directory with only `coreutils.exe`, run as `coreutils.exe cp …`.
- **Not shipped:** `sh`, `which`, `sed`, `file`, `less`. `tar.exe` comes from `C:\Windows\System32` (bsdtar).
- **Behaviour checked:** `cp -iRp --`, `mv -i --`, `rm -{f,i}vr --`, `xargs -0 -a FILE` and `stat` (GNU-format output) work as on Linux. A declined `cp -i`/`mv -i` exits 1, exactly like GNU coreutils 9.7. Prompts read `cp.exe: overwrite 'dst\f'?` (program name with `.exe`, `\` separators).

**Branch:** in `D:\Documents\Github\nnn-rs` run `git switch -c feat/coreutils-detect origin/main`. Do not read, merge or cherry-pick `feat/windows-coreutils` or the two local-only commits on `main` (`4e95a00`, `068a7d6`).

## Global Constraints

- Everything in W7's Global Constraints still holds:
  - Linux stays byte-identical. After every task, in WSL at `/mnt/d/Documents/Github/nnn-rs` (`myvps` has no Rust toolchain or checkout; use WSL):
    - `cargo test` in `rust/app`
    - `rust-anything app replay . --build rust/app/target/debug/nnn` = 242/242
    - `python3 rust/bytes-check.py rust/app/target/debug/nnn` = 246/246
    - `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` = 48/48
    - `rust/oracle/run.sh rust/app/target/release/oracle` = 15/15 "ok"
  - `unsafe` only in `platform/{unix,windows}.rs`.
  - No `#[allow]`.
  - `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean on both OSes.
  - Nothing is pushed without the user's OK.
- Windows, on this box (Coreutils installed), after every task: `rust-anything app replay . --build rust\app\target\debug\nnn.exe` = 242/242.
- Windows, from Task 4 on: `tui-check` = 40/40 on the existing cases, with Coreutils present.
- Detection never spawns a process; it only checks whether files exist. Its result is computed once per run (`OnceLock`).
- `NNN_COREUTILS=0` turns detection off (native behaviour, as on `origin/main`). Any other value, or unset, means auto-detect. This is the only new env var.
- Coreutils tools are always started by absolute path, never by `PATH` name, so Git for Windows' `usr\bin` and PowerShell aliases can't pick a different `cp`.
- A file name never reaches a `cmd` command line on the Coreutils paths. It is one `argv` entry after `--`.
- No new crate dependencies.

## Review Focus

1. **A declined overwrite (`n` at `cp.exe: overwrite`)** leaves the target unchanged and shows "Press Enter", then nnn's "failed!". The selection is kept, as on Linux (GNU exits 1 too). Pinned by Task 3 `coreutils_cp_declined_overwrite_keeps_target` and Task 4 case `win-cu-copy-overwrite`.
2. **Hostile names** (`a&calc.exe`, `100%`, `-rf`, a name with spaces, a lone surrogate) copy and remove correctly and run nothing. Pinned by Task 3 `coreutils_hostile_names`.
3. **A selection too long for one command line (32,767 UTF-16 units)** is split into several runs and every file is processed. Pinned by Task 3 `chunk_argv_splits` and `coreutils_rm_chunks_many_files`.
4. **A partly disabled install** (`coreutils-manager disable rm`) uses Coreutils for `cp` and the native code for remove. It never fails with "not found". Pinned by Task 2 `argv0_installed_missing_tool_is_none` and Task 3 `selection_uses_native_when_tool_missing`.
5. **The existing Windows screen cases on a box that has Coreutils** keep nnn's native prompts because `tui-check` runs them with `NNN_COREUTILS=0`. `NNN_COREUTILS=0` itself restores native behaviour. Pinned by Task 2 `enabled_reads_env_value` and Task 4 Step 1.

---

## File Structure

- Create `rust/app/src/platform/tools.rs`. It is OS-neutral (compiled on every OS so its tests run in WSL too) and does the `PATH`/`PATHEXT` search and the Coreutils detection.
- Modify `rust/app/src/platform/mod.rs`: declare `tools`, add the public wrappers, and rewrite `argv[0]` in `command`.
- Modify `rust/app/src/platform/windows.rs` (the `coreutils()` and `which()` backends) and `platform/unix.rs` (stubs).
- Modify `rust/app/src/browse.rs`: `getutil` and `xrm`.
- Modify `rust/app/src/popup.rs`: add `spawn_argv` and the Coreutils branch in `show_stats`.
- Modify `rust/app/src/fileops.rs`: `chunk_argv`, `selection_argv`, `cpmvrm_route` and `cpmvrm_coreutils`.
- Modify `rust/app/src/mounts.rs`: the archive Coreutils branch.
- Modify `rust/tui-check/src/main.rs`, `rust/tui-cases/cases.json` and `rust/tui-cases/expected/`.
- Modify `.github/workflows/ci.yml`, `rust/DECISIONS.md`, `rust/MAP.md`, `rust/HANDOFF.md` and `README.md`.

---

### Task 1: Native PATH search; `getutil` without `which`

**Files:**
- Create: `rust/app/src/platform/tools.rs` (tests in the same file)
- Modify: `platform/mod.rs` (`pub(crate) mod tools;`, compiled on all OSes, plus `pub fn which`), `platform/windows.rs`, `platform/unix.rs`, `browse.rs:2826-2829`

**Interfaces:**
- Produces:
  - `pub fn which_in(name: &[u8], path_var: Option<&OsStr>, pathext: Option<&OsStr>) -> Option<PathBuf>` in `tools.rs`.
    - A `name` containing `/` or `\` is checked as a path and nothing else.
    - Otherwise each `std::env::split_paths(path_var)` directory is tried in order, skipping empty entries and stripping one pair of surrounding `"`.
    - In each directory: try `name` as is if its extension is in `PATHEXT` (case-insensitive), then `name + ext` for each `PATHEXT` entry in order.
    - `PATHEXT` unset or empty means `.COM;.EXE;.BAT;.CMD`.
    - A hit is `is_file()`. The current directory is never searched.
  - `pub fn which(name: &[u8]) -> Option<Vec<u8>>` in `platform`. On Windows it is `which_in(name, PATH, PATHEXT)` as bytes. On Unix it is `None` and unused.
  - `Nnn::getutil`: on Windows it returns `platform::which(util).is_some()` and spawns nothing. On Unix it is unchanged (spawns `which`, as the C original does).

- [ ] **Step 1: Write failing tests** in `tools.rs`. Each builds a temp dir tree with empty files and a `PATH` from `std::env::join_paths` (`P(..)` below). Name the tests:
  - `which_finds_exe_via_pathext`: `d1/foo.EXE` exists, `which_in(b"foo", P(d1), Some(".COM;.EXE"))` returns `Some(d1/foo.EXE)`.
  - `which_respects_path_order`: `d1/foo.exe` and `d2/foo.exe` exist, `P(d2,d1)` returns `d2/foo.exe`.
  - `which_pathext_order_wins_within_dir`: `d/foo.bat` and `d/foo.exe` exist, `".EXE;.BAT"` returns `foo.exe`.
  - `which_name_with_known_extension`: `b"foo.exe"` finds `d/foo.exe`, and `foo.exe.exe` is never asked for.
  - `which_skips_empty_and_quoted_dirs`: a `PATH` of an empty entry plus `"<d>"` (with quotes) finds `d/foo.exe`.
  - `which_missing_is_none`, and `which_unset_path_is_none` (`path_var = None`).
  - `which_directory_named_like_tool_is_not_a_hit`: `d/foo.exe/` is a directory, so the result is `None`.
- [ ] **Step 2:** `cargo test --manifest-path rust/app/Cargo.toml tools` fails because `which_in` is not defined.
- [ ] **Step 3:** Implement the interfaces above.
- [ ] **Step 4: Verify.**
  - Windows: `cargo test tools` passes and replay is 242/242.
  - WSL: the five suites are green.
- [ ] **Step 5:** Commit `"Windows: find tools on PATH natively; getutil no longer needs which"`.

### Task 2: Detect Coreutils and route bare tool names to it

**Files:**
- Modify: `platform/tools.rs`, `platform/mod.rs` (`command`, around `mod.rs:380-407`, and the wrappers), `platform/windows.rs`, `platform/unix.rs`

**Interfaces:**
- Produces, in `tools.rs`:
  - `pub const COREUTILS_TOOLS: [&str; 5] = ["cp", "mv", "rm", "stat", "xargs"];`
  - `pub enum Layout { Installed(PathBuf /* bin dir */), Multicall(PathBuf /* coreutils.exe */) }` and `pub struct Coreutils { pub layout: Layout }`.
  - `pub fn detect_in(path_var: Option<&OsStr>, program_files: Option<&OsStr>) -> Option<Coreutils>` checks each `PATH` directory in order (same splitting rules as `which_in`):
    - `coreutils-manager.exe` is a file there: `Installed(dir)`.
    - Else `coreutils.exe` is a file there: `Multicall(dir/coreutils.exe)`.
    - The first hit wins. With no hit on `PATH`, it checks `<program_files>/coreutils/bin/coreutils-manager.exe` and returns `Installed` if found.
  - `impl Coreutils { pub fn argv0(&self, tool: &[u8]) -> Option<Vec<OsString>> }` returns `None` unless `tool` is in `COREUTILS_TOOLS`.
    - `Installed`: `[dir/<tool>.exe]`, only if that file exists.
    - `Multicall`: `[exe, tool]`.
  - `pub fn enabled(var: Option<&OsStr>) -> bool` is `var != Some("0")`.
- Produces, in `platform`:
  - `pub fn coreutils() -> Option<&'static tools::Coreutils>`. On Windows it is a `OnceLock` of `enabled(NNN_COREUTILS)` then `detect_in(PATH, ProgramFiles)`. On Unix it is always `None`.
  - `pub fn has_coreutil(tool: &[u8]) -> bool` is `coreutils().and_then(|c| c.argv0(tool)).is_some()`.
  - `command(argv, words)`: after the `is_cmd` branch, if `coreutils()` gives `argv0(&argv[0])`, the `Command` is `Command::new(a0[0]).args(&a0[1..]).args(argv[1..])`. On Unix nothing changes.
  - With this, the existing duplicate path (`browse.rs:2611`, `spawn(b"cp -rp --", …)`) runs Coreutils' `cp` with no edit there.

- [ ] **Step 1: Write failing tests** in `tools.rs`, using temp trees of empty files:
  - `detect_installed_layout`: `d/coreutils-manager.exe` and `d/cp.exe` exist. Expect `Installed(d)` and `argv0(b"cp") == Some([d/cp.exe])`.
  - `detect_multicall_layout`: `d/coreutils.exe` exists. Expect `argv0(b"rm") == Some([d/coreutils.exe, "rm"])`.
  - `detect_first_path_hit_wins`: `d1` is multicall, `d2` is installed, `P(d1,d2)`. Expect `Multicall`.
  - `detect_program_files_fallback`: `PATH` is empty and `pf/coreutils/bin/coreutils-manager.exe` exists. Expect `Installed(pf/coreutils/bin)`.
  - `detect_none`: no hits returns `None`.
  - `argv0_installed_missing_tool_is_none`: installed layout without `rm.exe`. Expect `argv0(b"rm") == None` and `argv0(b"cp")` is `Some`.
  - `argv0_unknown_tool_is_none`: `argv0(b"sed")` and `argv0(b"cp.exe")` are `None` in both layouts.
  - `enabled_reads_env_value`: `enabled(None)` is true, `enabled(Some("0"))` is false, `enabled(Some("1"))` is true.
  - `#[cfg(windows)] #[ignore = "needs Microsoft Coreutils"] real_cp_through_command`: `platform::command(&[b"cp", b"-rp", b"--", src, dst], 1)` exits 0, `dst` has `src`'s bytes, and `get_program()` is an absolute path.
- [ ] **Step 2:** `cargo test tools` fails.
- [ ] **Step 3:** Implement.
- [ ] **Step 4: Verify.**
  - Windows: `cargo test` passes, `cargo test -- --ignored real_cp_through_command` passes, and replay is 242/242.
  - Manual: in nnn, `r` on a file, choose `d`uplicate, and the copy appears.
  - WSL: the five suites are green.
- [ ] **Step 5:** Commit `"Windows: detect Microsoft Coreutils (installed and portable) and run its tools by path"`.

### Task 3: Selection copy/move/remove and single remove through Coreutils

**Files:**
- Modify: `popup.rs` (around `popup.rs:764-829`: `spawn` becomes a wrapper), `fileops.rs` (`cpmvrm_selection` Windows branch at `fileops.rs:441-445` plus the new functions), `browse.rs:2895-2911` (`xrm`)

**Interfaces:**
- Consumes: `platform::has_coreutil` and `platform::command` (Task 2).
- Produces:
  - `pub fn spawn_argv(&mut self, argv: &[Vec<u8>], words: usize, cwd: Option<&[u8]>, flag: u16) -> i32` in `popup.rs`. It is today's `spawn` body from `endwin` onwards, plus `cmd.current_dir(platform::path(cwd))` when given. `spawn` builds its `argv` as now and calls `spawn_argv(&argv, words, None, flag)`, so Linux is unchanged.
  - `pub(crate) const WIN_CMDLINE_MAX: usize = 32_000;` in `fileops.rs`.
  - `pub(crate) fn chunk_argv(head: &[Vec<u8>], items: &[Vec<u8>], tail: &[Vec<u8>], limit: usize) -> Vec<Vec<Vec<u8>>>` is OS-neutral and pure.
    - Each chunk is `head ++ some items ++ tail`, items kept in order.
    - An argument costs `len + 3` (quotes and a space).
    - A chunk is closed before an item that would push its cost past `limit`.
    - An item too big on its own gets a chunk alone.
    - No items means no chunks.
  - `pub(crate) fn selection_argv(sel: Action, force: u8) -> (Vec<Vec<u8>>, Vec<Vec<u8>>)` returns `(head, tail)`:
    - `Cp`: `(["cp","-iRp","--"], ["."])`
    - `Mv`: `(["mv","-i","--"], ["."])`
    - `RmRf` and `Trash`: `(["rm", [b'-', force, b'v', b'r'], "--"], [])`, which is the C original's `rm -%cvr`.
    - The progress variants (`cpg`/`mvg`) are never used on Windows.
  - `pub(crate) enum Route { Coreutils, Native }` and `pub(crate) fn cpmvrm_route(sel: Action, has: fn(&[u8]) -> bool) -> Route` map `Cp`→`cp`, `Mv`→`mv`, remove→`rm`, giving `Coreutils` when `has(tool)` is true.
  - `#[cfg(windows)] fn cpmvrm_coreutils(&mut self, sel: Action, dirpath: &[u8]) -> bool`:
    1. For a remove, ask `confirm_force(true, false)` first (0 means `MSG_CANCEL`).
    2. Read the NUL-separated selection file and run every chunk with `spawn_argv(chunk, 1, Some(dirpath), F_CLI | F_CHKRTN)`.
    3. If any run returns non-zero, show `MSG_FAILED` and return false, keeping the selection. Otherwise `clearselection()` and return true.
  - `cpmvrm_selection`'s Windows branch (unchanged guard: not `Cpmvas`, not a `Trash` with a trash command) calls `cpmvrm_coreutils` when `cpmvrm_route(sel, platform::has_coreutil) == Route::Coreutils`, else today's `cpmvrm_native`.
  - `xrm`: run the existing `#[cfg(unix)]` `rm` spawn when `cfg!(unix) || platform::has_coreutil(b"rm")`. `remove_native` runs only on Windows without it.

- [ ] **Step 1: Write failing tests** in `fileops.rs` (the first six run on both OSes):
  - `chunk_argv_fits_one`: 3 short items give 1 chunk, equal to `head ++ items ++ tail`.
  - `chunk_argv_splits`: 2,000 items of 40 bytes with `limit = WIN_CMDLINE_MAX` give at least 2 chunks. Each chunk's cost is at most the limit, the concatenated items are in the original order, and every chunk starts with `head` and ends with `tail`.
  - `chunk_argv_oversize_item_alone`: an item of `limit + 1` bytes is alone in its chunk.
  - `chunk_argv_empty`: no items give `vec![]`.
  - `selection_argv_forms`: the four forms above, including `force = b'i'` giving `b"-ivr"`.
  - `selection_uses_native_when_tool_missing`: with `has` = "only `cp`", `cpmvrm_route(Cp, has)` is `Coreutils` and `cpmvrm_route(RmRf, has)` is `Native`.
  - `#[cfg(windows)] #[ignore = "needs Microsoft Coreutils"] coreutils_cp_declined_overwrite_keeps_target`: run `platform::command` of the `Cp` chunk with `cwd = dst` and stdin `"n\n"`. Exit status is 1 and `dst/f` still holds `b`.
  - `#[cfg(windows)] #[ignore = …] coreutils_hostile_names`: create the files `a&calc.exe`, `100%`, `-rf`, `" x y "` (spaces) and `OsString::from_wide(&[0x61, 0xD800])`. The `Cp` chunk into `dst` exits 0 and all five exist in `dst`. The `RmRf` chunk (`force = b'f'`) on the sources exits 0 and none remain. No `calc` process is started (`tasklist /FI "IMAGENAME eq calc*"` count is unchanged).
  - `#[cfg(windows)] #[ignore = …] coreutils_rm_chunks_many_files`: 2,000 files with 40-char names give at least 2 chunks, all exiting 0, and the directory ends empty.
- [ ] **Step 2:** `cargo test fileops` fails.
- [ ] **Step 3:** Implement `spawn_argv` (pure refactor first; check that WSL `cargo test` and the tmux suite are still 48/48), then `chunk_argv`, `selection_argv`, `cpmvrm_route`, `cpmvrm_coreutils` and the `xrm` change.
- [ ] **Step 4: Verify.**
  - Windows: `cargo test` passes, `cargo test -- --ignored coreutils_` passes 3/3, and replay is 242/242.
  - WSL: the five suites are green.
- [ ] **Step 5:** Commit `"Windows: copy, move and remove the selection with Coreutils when installed"`.

### Task 4: Stat popup, archiving, screen cases

**Files:**
- Modify: `popup.rs:1089-1103` (`show_stats`), `mounts.rs:170-192` (archive), `rust/tui-check/src/main.rs`, `rust/tui-cases/cases.json`, `rust/tui-cases/expected/`

**Interfaces:**
- Consumes: `spawn_argv` (Task 3) and `platform::has_coreutil` (Task 2).
- Produces:
  - `show_stats` on Windows: when `has_coreutil(b"stat")`, the content is `self.buffer_command_output(&[b"stat"], Some(pathbuf), None)` (the Unix helper, now compiled on Windows too), else `winops::file_info_text`. There are no `file` lines (Coreutils has no `file`).
  - Archive on Windows, when `has_coreutil(b"xargs")`:
    - `argv = ["xargs", "-0", "-a", <selpath or tmpfile>] ++ parseargs(cmd) ++ [<archive name, unescaped>]`, run with `spawn_argv(&argv, 1, relative.then_some(path), F_CLI | F_CONFIRM)`.
    - No `cd … &&`, no `< file`, no shell quoting.
    - The tmpfile cleanup stays.
    - Without `xargs`, today's shell line runs.
  - `tui-check`:
    - Every case gets `NNN_COREUTILS=0` in its environment unless its `requires` contains `"coreutils"`.
    - New optional case field `"requires": ["coreutils"]`.
    - New flag `--coreutils` runs those cases. Without it they print `SKIP <name> (requires coreutils)` and are not counted.
    - The new cases are Windows-only (`"platforms": ["windows"]`).

- [ ] **Step 1: Prove the env pin:** with the `NNN_COREUTILS=0` injection, `cargo run --manifest-path rust/tui-check/Cargo.toml -- check <abs nnn.exe>` on this box is 40/40. Temporarily remove the injection, and `win-copy-overwrite` must FAIL (the prompt becomes `cp.exe: overwrite`). Restore the injection.
- [ ] **Step 2: Write the failing cases.** Each starts from an existing case's keys in `cases.json`; first runs are recorded with `tui-check record --coreutils`, checked by hand, and the check goes in `"note"`.
  - `win-cu-copy-overwrite` uses the keys of `win-copy-overwrite`. The screen shows `cp.exe: overwrite`; answering `n` then Enter leaves `Gamma`'s hash unchanged and shows `failed!`.
  - `win-cu-copy` selects `Gamma`, enters an empty `dir2`, and presses `p`. The files hash includes `dir2/Gamma` and the selection is cleared.
  - `win-cu-stat` uses the keys of `file-info`. The popup shows `  File: ` and `Access: (` lines from `stat`; it normalizes `Inode:` and the `Access:/Modify:/Change:/Birth:` times like `file-info`.
  - `win-cu-archive` uses the keys of `archive-current`. `<name>.tar` exists after the case and lists the selected file (`tar -tf` is checked in the note).
- [ ] **Step 3:** `tui-check check <exe> --coreutils` fails on the new cases until the code above is in.
- [ ] **Step 4: Verify.**
  - Windows: `tui-check check <exe> --coreutils` = 44/44.
  - Windows: `tui-check check <exe>` = 40/40 with 4 SKIP.
  - Windows: replay is 242/242.
  - WSL: the five suites are green, and `tui-check` on Linux is 48/48.
- [ ] **Step 5:** Commit `"Windows: stat popup and archiving through Coreutils; tui-check coreutils cases"`.

### Task 5: CI, decision, docs

**Files:**
- Modify: `.github/workflows/ci.yml`, `rust/DECISIONS.md` (ADR-028), `rust/MAP.md` (wave W8), `rust/HANDOFF.md`, `README.md` (Windows section)

**Interfaces:**
- CI: a new job `windows-coreutils` on `windows-latest`, as a copy of the `windows` job steps plus:
  1. `gh release download v2026.9.3 --repo microsoft/coreutils -p 'coreutils-2026.9.3-x64.zip'`, with `GH_TOKEN: ${{ github.token }}`.
  2. `Expand-Archive`, then append `<dir>\coreutils-2026.9.3-x64` to `$env:GITHUB_PATH`. This is the portable (multicall) layout, so CI covers the layout this box doesn't have.
  3. `cargo test -- --include-ignored`.
  4. `tui-check check … --coreutils`.
  5. Replay 242/242.
  - The existing `windows` job stays as it is, covering "no Coreutils".

- [ ] **Step 1:** ADR-028, "Windows uses Microsoft Coreutils when installed", records:
  - the detection rules and both layouts;
  - the tool list;
  - `NNN_COREUTILS=0`;
  - spawning by absolute path, with names only as `argv` entries;
  - the 32,000 chunk limit;
  - the known differences from Linux: prompts say `cp.exe:` with `\` paths, the stat popup has no `file` lines, `stat` shows Uid 0 / Gid `None`;
  - what still needs Git for Windows (`sh` plugins, batch rename, copy/move-as, POSIX command prompt);
  - the ADR-024 feature-matrix lines it amends.
- [ ] **Step 2:** Edit `ci.yml` as above. Validate with `actionlint` if installed, else with a YAML parse.
- [ ] **Step 3:** README: add "Windows: with Microsoft Coreutils" covering `winget install Microsoft.Coreutils`, what changes, and `NNN_COREUTILS=0`. Add a MAP row for W8 with the numbers, and rewrite HANDOFF for a fresh session.
- [ ] **Step 4: Final verification.**
  - Windows with Coreutils: `cargo test -- --include-ignored`, clippy and fmt clean, replay 242/242, `tui-check --coreutils` 44/44.
  - Windows with `$env:NNN_COREUTILS='0'`: `cargo test` passes and replay is 242/242.
  - WSL: the five suites are green.
  - Then `rust-anything map --write`.
- [ ] **Step 5:** Commit `"CI job with Microsoft Coreutils; ADR-028 and docs"`. Pushing and opening a PR wait for the user's go-ahead.
