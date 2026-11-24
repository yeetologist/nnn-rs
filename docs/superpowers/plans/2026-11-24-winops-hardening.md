# Windows file operations: hardening winops and closing the no-tool gaps — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On Windows without Git for Windows or Microsoft Coreutils, nnn's file operations should survive briefly locked files, duplicate works, and archiving a selection works. Linux stays byte-identical.

**Architecture:**
- `winops.rs` gets a retry wrapper for Windows "file in use" errors, as yazi does (`yazi-fs/src/remover/windows.rs` retries a sharing violation up to 50 times).
- Duplicate gets a native `winops::copy_to` when there's no Coreutils `cp`.
- Archiving a selection passes the NUL-separated list straight to tar (`--null -T`) when the archiver is a tar. That needs no `xargs` or shell, and one tar run means the archive is never overwritten by a later batch.

**Tech Stack:** Rust (nnn-rs `rust/app`), std only (no new crates), `tui-check`.

**Spec:** the user's request of 2026-11-24 ("harden winops… in a new branch") after the yazi comparison. These facts were measured on the dev box on 2026-11-24 and are binding:
- Windows' `C:\Windows\System32\tar.exe -acvf g.tar --null -T list` archives the NUL-separated names in `list`, including `x y`.
- Rust std's `fs::remove_file` deletes a read-only file and `fs::remove_dir_all` deletes a tree holding one. .NET's `File.Delete` refuses the same file, but nnn uses std.
- ADR-026's "`rm -rf` of a read-only file fails with Permission denied" is therefore stale.
- Remaining gaps on `main` (`72d0dce`) without Git or Coreutils:
  - duplicate (`browse.rs:2611` spawns `cp -rp --`) silently does nothing;
  - archiving a selection (`mounts.rs` `archive_selection`) runs `xargs` through `cmd /C` and fails.
- Out of scope (decided with the user): progress and cancel for copies, a Windows Recycle Bin backend, bundling uutils crates.

**Branch:** `feat/winops-hardening` (already created from `main` `72d0dce`).

## Global Constraints

- **Linux stays byte-identical.** After every task, in WSL at `/mnt/d/Documents/Github/nnn-rs` (`export PATH="$HOME/.cargo/bin:$PATH"` first):
  - `cargo test` in `rust/app`
  - `rust-anything app replay . --build rust/app/target/debug/nnn` = 242/242
  - `python3 rust/bytes-check.py rust/app/target/debug/nnn` = 246/246
  - `python3 rust/tui-cases/harness.py check $(pwd)/rust/app/target/debug/nnn` = 48/48
  - `rust/oracle/run.sh rust/app/target/release/oracle` = 15 "ok"
  - `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean
- **Windows, after every task:**
  - `cargo test --manifest-path rust/app/Cargo.toml -- --include-ignored` passes (Coreutils is installed on the dev box)
  - `rust-anything app replay . --build rust\app\target\debug\nnn.exe` = 242/242
  - clippy and fmt clean
  - from Task 2 on: `tui-check check <exe>` = all non-Coreutils cases pass, and `tui-check check <exe> --coreutils` = all cases pass
- **Retry budget:**
  - only Windows OS errors 32 (`ERROR_SHARING_VIOLATION`) and 33 (`ERROR_LOCK_VIOLATION`) count as "busy";
  - 50 tries, 10 ms apart (about 0.5 s);
  - every other error returns at once, and on Linux nothing is retried.
- `unsafe` only in `platform/{unix,windows}.rs`; no `#[allow]`; no new crate dependencies.
- **Errors keep glibc wording through `sys::strerror`.** Duplicate stays silent on failure, as Linux (`F_SILENT`) is.
- **New Windows screen cases set `"env": {"PATH": "C:\\Windows\\System32"}`,** so neither Git's nor Coreutils' tools are found, and rely on tui-check's default `NNN_COREUTILS=0`.

## Review Focus

1. **A file held open longer than the retry budget** (an editor holding an exclusive lock): delete and copy report an error after about 0.5 s and never hang. Pinned by Task 1 `delete_locked_file_reports_error` with a time bound.
2. **Duplicating a directory** (Ctrl-R on a directory, new name): the whole tree is copied with modification times. Pinned by Task 2 `copy_to_copies_tree`.
3. **Duplicating onto an existing file** after "overwrite?" is answered y: the file is replaced. Pinned by Task 2 `copy_to_overwrites_existing_file`.
4. **Archiving a selection whose names hold spaces, `&` or non-ASCII:** every file ends up in the archive. Pinned by Task 3 `tar_list_argv_archives_odd_names`.
5. **Errors that aren't busy** (missing file, access denied by ACL): they fail at once with no 0.5 s wait. Pinned by Task 1 `retry_with_does_not_retry_other_errors`.

---

## File Structure

- Modify `rust/app/src/winops.rs`: retry helpers, `copy_to`, and their tests.
- Modify `rust/app/src/browse.rs`: the duplicate branch (around `browse.rs:2609-2614`).
- Modify `rust/app/src/mounts.rs`: `tar_list_argv` and the `archive_selection` branch.
- Modify `rust/tui-cases/cases.json` and `rust/tui-cases/expected/`: two Windows cases.
- Modify `rust/DECISIONS.md` (ADR-029, plus amendments to ADR-024 and ADR-026), `rust/MAP.md` (W9), `rust/HANDOFF.md` and `README.md`.

---

### Task 1: Retry briefly locked files in winops

**Files:**
- Modify: `rust/app/src/winops.rs` (helpers, plus every `fs::copy`, `fs::rename`, `fs::remove_*` and `open_for_times` call; tests in its `mod tests`)

**Interfaces:**
- Produces, in `winops.rs`:
  - `fn retry_with<T>(op: impl FnMut() -> io::Result<T>, busy: fn(&io::Error) -> bool, pause: Duration, tries: usize) -> io::Result<T>`: calls `op` up to `tries` times, sleeping `pause` between tries while `busy(&err)`, and returns the last result.
  - `fn is_busy(e: &io::Error) -> bool`: on Windows, `raw_os_error()` is `Some(32)` or `Some(33)`; elsewhere always `false`.
  - `fn busy_retry<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T>` = `retry_with(op, is_busy, Duration::from_millis(10), 50)`.
  - Every `fs::copy`, `fs::rename`, `fs::remove_file`, `fs::remove_dir`, `fs::remove_dir_all` and `open_for_times` call in `winops.rs` (outside tests) goes through `busy_retry`.

- [ ] **Step 1: Write failing tests** in `winops.rs` `mod tests`:
  - Both OSes, with `busy = |e| e.raw_os_error() == Some(32)`:
    - `retry_with_retries_busy_then_succeeds`: `op` fails 3 times with `io::Error::from_raw_os_error(32)`, then returns `Ok(7)`. Expect `Ok(7)` after 4 calls (pause 1 ms, tries 50).
    - `retry_with_gives_up_after_tries`: `op` is always busy, tries 5. Expect `Err` with raw code 32 after 5 calls.
    - `retry_with_does_not_retry_other_errors`: `op` returns `io::ErrorKind::NotFound`. Expect 1 call and `Err`.
  - `#[cfg(windows)]`, all of the form "another thread holds the file, then lets go":
    - `remove_all_waits_for_brief_lock`: a thread opens `f` with `OpenOptionsExt::share_mode(0)`, signals on an `mpsc` channel, sleeps 100 ms and drops it. After the signal, `remove_all(f)` returns `Ok` and `f` is gone.
    - `copy_into_waits_for_brief_lock`: the same lock held on the source file. `copy_into(src, dst_dir, never)` returns `Ok` and `dst_dir/f` has the source's bytes.
    - `move_into_waits_for_brief_lock`: the same lock on the source. `move_into` returns `Ok`; the source is gone and the target exists.
  - `#[cfg(windows)]` read-only pins (these pass on today's code; they pin the std behaviour ADR-026 got wrong):
    - `remove_all_removes_read_only_file`: a file with `set_readonly(true)` is removed by `remove_all`.
    - `remove_confirmed_force_removes_tree_with_read_only_file`: a directory holding a read-only file is removed by `remove_confirmed(d, true, never)`, which returns `Ok(true)`.
  - Change the existing `delete_locked_file_reports_error`: the lock is held for the whole test, `remove_all` still returns `Err`, and the elapsed time is below 2 s.
- [ ] **Step 2: Run it and watch it fail.** Windows: `cargo test --manifest-path rust/app/Cargo.toml --lib winops`. Expect a compile failure (`retry_with` is not defined). With temporary stubs, the three `*_waits_for_brief_lock` tests FAIL with "being used by another process".
- [ ] **Step 3:** Implement the interfaces above.
- [ ] **Step 4:** `cargo test --lib winops` passes on Windows and in WSL (the `cfg(windows)` tests don't run there). Then the Global Constraints suites on both OSes.
- [ ] **Step 5:** Commit `"winops: retry files another process holds briefly (sharing and lock violations)"`.

### Task 2: Duplicate without cp

**Files:**
- Modify: `rust/app/src/winops.rs` (`copy_to` plus tests), `rust/app/src/browse.rs:2609-2614`, `rust/tui-cases/cases.json`, `rust/tui-cases/expected/win-duplicate.json` (new)

**Interfaces:**
- Consumes: `busy_retry` (Task 1), through `copy_entry`.
- Produces:
  - `pub fn copy_to(src: &[u8], dst: &[u8]) -> io::Result<()>` in `winops.rs`: `cp -rp src dst` to an explicit target name. It is `copy_entry(src, dst, &mut |_| true)` after `check_target`'s "a directory into itself" test (`EINVAL` when `dst` is `src` or inside `src`). An existing file at `dst` is overwritten (nnn already asked).
  - In `browse.rs`, when `ret == b'd'`: if `cfg!(windows) && !platform::has_coreutil(b"cp")`, call `crate::winops::copy_to(&cn, &tmp)` and ignore the error (Linux's `F_SILENT` shows none). Otherwise run today's `spawn(b"cp -rp --", …)`.

- [ ] **Step 1: Write failing tests** in `winops.rs` (they run on both OSes):
  - `copy_to_copies_file_to_new_name`: `a` holding `b"x"` is copied to `b`; `b` holds `b"x"` and has `a`'s modification time.
  - `copy_to_copies_tree`: `d/e/f` is copied to `d2`; `d2/e/f` exists and `d2` has `d`'s modification time.
  - `copy_to_overwrites_existing_file`: with `a` (`b"new"`) and `b` (`b"old"`), `b` ends up holding `b"new"`.
  - `copy_to_refuses_dir_onto_file`: directory `d` onto existing file `f` gives an error with `is_errno(.., Errno::Exist)`.
  - `copy_to_into_itself_is_refused`: `copy_to(d, d/sub)` gives `Errno::Inval`.
  - Screen case `win-duplicate` in `cases.json`:
    - `platforms ["windows"]`, `env {"PATH": "C:\\Windows\\System32"}`, fixture `{"Gamma": "x\n"}`
    - keys `["<C-R>", "<Enter>", "<C-U>", "Gamma2", "<Enter>", "q"]` (Ctrl-R, Enter on the same name, then the copy-name prompt)
    - note: "Recorded on Windows without Git or Coreutils on PATH (hand-verified first run): duplicate copies Gamma to Gamma2 natively."
- [ ] **Step 2:** `cargo test --lib winops` fails (`copy_to` is not defined). Record `win-duplicate` on today's build with `tui-check record <exe> --case win-duplicate`; the recorded work tree has no `Gamma2`. That shows the gap. Delete that recording.
- [ ] **Step 3:** Implement `copy_to` and the `browse.rs` branch.
- [ ] **Step 4: Verify.**
  - Record `win-duplicate` again and check by hand: the work tree lists `Gamma2` with `Gamma`'s hash, and the cursor is on `Gamma2`.
  - `tui-check check <exe>` passes all non-Coreutils cases, and `--coreutils` passes all.
  - Then the Global Constraints suites.
- [ ] **Step 5:** Commit `"Windows: duplicate natively when there is no Coreutils cp"`.

### Task 3: Archive a selection with tar's own list reader

**Files:**
- Modify: `rust/app/src/mounts.rs` (`tar_list_argv` and the `archive_selection` branch, plus tests), `rust/tui-cases/cases.json`, `rust/tui-cases/expected/win-archive-sel.json` (new), and, if its screen changes, `rust/tui-cases/expected/win-cu-archive.json`

**Interfaces:**
- Consumes: `Nnn::spawn_argv(&[Vec<u8>], words: usize, cwd: Option<&[u8]>, flag: u16) -> i32` and `popup::parseargs` (both exist).
- Produces:
  - `pub(crate) fn tar_list_argv(cmd: &[u8], archive: &[u8], list: &[u8]) -> Option<Vec<Vec<u8>>>` in `mounts.rs` (compiled on all OSes).
    - It returns `Some(parseargs(cmd) ++ [archive, "--null", "-T", list])` when the first word's base name is `tar` or `bsdtar`. The base name is the part after the last `/` or `\`, case-insensitive, with any `.exe` dropped.
    - Otherwise it returns `None`.
  - `archive_selection` on Windows: if `tar_list_argv(cmd, archive, &selpath)` is `Some(argv)`, run `spawn_argv(&argv, 1, relative.then(|| cstr(path)), F_CLI | F_CONFIRM)` and skip both the Coreutils `xargs` branch and the shell line.
    - This is placed before the existing `if cfg!(windows) && has_coreutil(b"xargs")` branch.
    - The tmpfile cleanup stays.
    - Linux keeps the original's `xargs` line (byte-identical).

- [ ] **Step 1: Write failing tests** in `mounts.rs` `mod tests`:
  - `tar_list_argv_forms` (both OSes):
    - `"tar -acvf"` gives `[tar, -acvf, a.tar, --null, -T, l]`.
    - `"C:/WINDOWS/System32/tar.exe -acvf"` gives `Some` whose first word is unchanged.
    - `"bsdtar -acvf"` and `"BSDTAR.EXE -acvf"` give `Some`.
    - `"zip -r"` and `"atool -a"` give `None`.
  - `#[cfg(windows)] tar_list_argv_archives_odd_names`: in a temp directory, files `x y`, `a&b` and `ñandú.txt` plus a NUL-separated list of the three. Run the argv from `tar_list_argv(&windows_tar_cmd(SystemRoot), b"g.tar", list)` with `platform::command` in that directory. It exits 0, and `tar.exe -tf g.tar` lists all three names.
  - Screen case `win-archive-sel`:
    - `platforms ["windows"]`, `env {"PATH": "C:\\Windows\\System32"}`
    - the fixture and keys of `win-cu-archive`, without `requires`
    - `nohash ["g\\.tar"]`, `quiet 1.5`
    - note: "Recorded on Windows without Git or Coreutils on PATH (hand-verified first run): z, s, relative: tar.exe -acvf g.tar --null -T <list> prints 'a beta.md' and 'a Gamma'; g.tar is created and the selection cleared."
- [ ] **Step 2:** `cargo test --lib mounts` fails (`tar_list_argv` is not defined). On today's build `win-archive-sel` shows `failed!` or no `g.tar` in the work tree; record it once to see this, then delete the recording.
- [ ] **Step 3:** Implement `tar_list_argv` and the branch.
- [ ] **Step 4: Verify.**
  - Record `win-archive-sel` and check it against its note.
  - `tui-check check <exe> --coreutils --case win-cu-archive`: if it fails only because tar now prints names in list order (same set of lines), re-record it and say so in its note. Any other difference is a bug.
  - Then the full tui-check runs (with and without `--coreutils`) and the Global Constraints suites.
- [ ] **Step 5:** Commit `"Windows: archive the selection with tar --null -T (no xargs, one run)"`.

### Task 4: Decisions, docs, final verification

**Files:**
- Modify: `rust/DECISIONS.md`, `rust/MAP.md`, `rust/HANDOFF.md`, `README.md`

- [ ] **Step 1:** Write ADR-029, "Hardening Windows file operations", recording:
  - the busy-retry rule (codes 32 and 33, 50 × 10 ms), following yazi's remover, and why other errors aren't retried;
  - native duplicate when there is no Coreutils `cp`;
  - tar-family archivers getting the list through `--null -T` on Windows (one run, so no batch overwrites the archive; this closes the overwrite-per-batch minor from the Coreutils review);
  - that std already deletes read-only files, so ADR-026's "Known differences" sentence about read-only files is removed;
  - in ADR-024's matrix: duplicate and selection archiving work without Git or Coreutils.
- [ ] **Step 2:** README's Windows section: drop the read-only difference, and say that duplicate and archiving work without extra tools. Add a MAP W9 row with the numbers, and update HANDOFF.
- [ ] **Step 3: Final verification.**
  - Windows: `cargo test -- --include-ignored`; clippy and fmt; replay 242/242; `tui-check` (all non-Coreutils cases, with the 4 Coreutils cases skipped); `tui-check --coreutils` (all).
  - Windows with `NNN_COREUTILS=0`: `cargo test` and replay 242/242.
  - WSL: the five suites, `tui-check` 48/48, clippy and fmt.
- [ ] **Step 4:** Commit `"ADR-029 and docs: hardened Windows file operations"`. Pushing waits for the user.
