# Decisions

## ADR-001: Shape: one CLI/TUI binary
Status: accepted (2026-11-23)
Context: nnn is one C program (src/nnn.c, ~11k lines), entry point main(); analyze --whole-app: shape cli.
Decision: Port the whole app to one Rust binary `nnn` in rust/app; the C source stays as reference.
Consequences: Front door owns argv first; the C binary is the fallback until the lift.

## ADR-002: Build and test host: WSL Ubuntu
Status: accepted (2026-11-23)
Context: No C toolchain on the Windows host; nnn is POSIX-only (fork, inotify, termios).
Decision: Record, replay and build in WSL Ubuntu (gcc 15, ncurses, readline, rustup, rust-anything 3.3.0). Repo path /mnt/d/Documents/Github/nnn. Linux x86_64 is the first target.
Consequences: Windows native is out of scope; macOS/BSD (kqueue, mach) later.

## ADR-003: Scope: default `make` build
Status: accepted (2026-11-23)
Context: Makefile toggles O_NORL, O_PCRE, O_ICONS, O_NERD, O_EMOJI, O_NOMOUSE, etc.
Decision: Port the default build only: readline prompt, POSIX regex, mouse, no icons. Icon variants after the lift.
Consequences: icons-hash.c, icons*.h, misc/haiku, misc/macos-legacy out of scope for now.

## ADR-004: Equivalence bar
Status: accepted (2026-11-23)
Context: A TUI cannot be fully captured as trycmd cases.
Decision: (a) Non-interactive paths (argv, NNN_OPTS, env errors, -h/-V/-K, exit codes, picker output) match byte for byte as rust/app-cases. (b) Interactive behaviour: a pty harness (rust/tui-cases) sends scripted keys, renders the screen with a terminal emulator and compares screen TEXT (no colour attributes) plus filesystem effects.
Consequences: Colour/attribute differences are not checked; layout and text are.

## ADR-005: Terminal stack: crossterm + built-in line editor
Status: superseded by ADR-013 (line editor) and ADR-017 (terminal) (2026-11-23)
Context: Q4: pure Rust preferred over ncurses bindings.
Decision: crossterm for raw mode, keys, mouse and drawing; nnn's curses layout reproduced by hand; readline replaced by a built-in line editor (history file format kept).
Consequences: Readline-specific features (inputrc, menu-complete) are approximated; the screen harness keeps the layout honest.

## ADR-006: Compatibility kept
Status: accepted (2026-11-23)
Context: Users have configs, sessions, plugins.
Decision: Keep every NNN_* environment variable, ~/.config/nnn layout, session file format, NNN_FIFO protocol, plugin invocation contract and selection file format unchanged.
Consequences: On-disk formats are part of the recorded contract.

## ADR-007: Checkpoints
Status: accepted (2026-11-23)
Context: Q5.
Decision: Stop for the user's go-ahead after each wave: W1 front door+recording, W2 options/config/env, W3 listing+sorting, W4 TUI core, W5 file operations, W6 lift.
Consequences: One wave per review.

## ADR-008: Cutover
Status: accepted (2026-11-23)
Context: Original is upstream nnn.
Decision: Run the Rust binary beside the original; the user decides the cutover; the C source stays in src/ as reference.
Consequences: No deletion of C sources.

## ADR-009: Draw boxes with the same terminal line-drawing codes as ncurses
Status: superseded by ADR-016 (2026-11-23)
Context: pyte does not translate the DEC line-drawing charset, so the recorded popups show ncurses box borders as the letters l q k x j m. A Rust port that draws Unicode box characters would fail the screen cases while looking the same.
Decision: The Rust renderer emits the same line-drawing codes ncurses sends for xterm (switch to the DEC special graphics charset, then back).
Consequences: Screens match byte for byte in the harness. Revisit if the harness gains charset support.

## ADR-010: Interactive harness normalizers
Status: accepted (2026-11-23)
Context: Screens show "now" timestamps (files created during a case) and stat's Change/Birth/Inode values, which differ run to run.
Decision: rust/tui-cases/harness.py replaces any date other than the fixture's mtime (2023-11-14 22:13) with <NOW>; file-info also replaces stat's Change/Birth/Inode/Device values. Every case is recorded twice and refused if the two runs differ.
Consequences: Times of new files are not compared; everything else on screen is.

## ADR-011: nnn.c is one tangled unit, ported as modules of rust/app
Status: accepted (2026-11-23)
Context: analyze marks src/nnn.c "keep": one 11k-line file of static functions sharing globals; `bridge` cannot expose static C functions, so per-unit `record`/`diff`/`gate` does not apply.
Decision: Port it wave by wave as Rust modules of rust/app (getopt, opts, keys, sys, startup, ...). Proof per wave: `app replay` (rust/app-cases), the byte-exact sweep (rust/bytes-check.py), the pty harness (rust/tui-cases), plus fmt, clippy -D warnings and unit tests in rust/app. Error messages printed by `xerror()` keep the original's C source line numbers (e.g. "10567: No such file or directory"), as constants at each call site.
Consequences: No `gate` per unit; each wave ends with all three suites green on the Rust build. Kill switch: RUST_ANYTHING_UNIT_STARTUP=original.

## ADR-012: Program path in recorded getopt messages
Status: accepted (2026-11-23), user to confirm
Context: glibc prints argv[0] in "invalid option" lines; recording ran the original by absolute path, so 28 expected stderr files held /mnt/d/.../nnn, which no other binary can print. trycmd ignores trailing newlines and cannot wildcard inside binary cases.
Decision: In rust/app-cases the path prefix became "[..]". The 4 binary cases (`-é`) moved to rust/argv0-cases/removed and are checked by rust/bytes-check.py, which runs every case (all 246) for the original and the build with argv[0]="nnn" and compares stdout, stderr and exit code byte for byte.
Consequences: Byte-exact equality is checked for every CLI case, including the program-name prefix.

## ADR-013: Default build has no readline (supersedes ADR-005 in part)
Status: accepted (2026-11-23)
Context: The Makefile sets O_NORL := 1: the default build uses nnn's own xreadline prompt and xread_history/xwrite_history; `-N` is in the option string but unhandled (usage, exit 1).
Decision: Port nnn's native xreadline and history functions instead of writing a new line editor. crossterm stays the terminal layer (ADR-005).
Consequences: Prompt behaviour is the original's own code, ported.

## ADR-014: Locale, regex and time come from glibc, through one FFI module
Status: accepted (2026-11-23), user to confirm; the "only module with unsafe" rule is superseded by ADR-022
Context: The original sorts names with strcoll (LC_COLLATE), folds case and measures widths with mbstowcs/towlower/wcswidth (LC_CTYPE), folds bytes with tolower in strcasestr/strcasecmp, filters with POSIX regcomp/regexec (REG_EXTENDED|REG_ICASE), and prints times with localtime_r (TZ). No Rust crate reproduces glibc's locale tables, Unicode version or POSIX regex syntax; the oracle showed them matter (tr_TR, de_DE, en_US collation).
Decision: rust/app/src/locale.rs calls these glibc functions through the libc crate (and four extern declarations libc lacks). It is the only module with `unsafe`, each block with a SAFETY comment. main() calls setlocale(LC_COLLATE/LC_CTYPE, "") as the original does. Directory reading uses rustix (safe API).
Consequences: Identical results in every locale; the binary stays glibc/Linux-bound (ADR-002). Replacing glibc later means accepting differences, as a new ADR.

## ADR-015: Differential oracle for functions with no visible output yet
Status: accepted (2026-11-23)
Context: Sorting, filtering and formatting only show on screen, which is still the original's until W4.
Decision: rust/oracle/oracle.c includes reference/nnn/src/nnn.c (main renamed) and answers calls to its own static functions; rust/app/src/bin/oracle.rs answers the same calls with the port. rust/oracle/run.sh generates calls and fixture directories (gen.py: unicode, invalid UTF-8, controls, numbers, symlinks, dangling links, hard links, fifos, fixed mtimes) and compares outputs byte for byte under 5 locales x 3 time zones. A mutation in the port (coolsize rounding) was caught in 14 of 15 runs.
Consequences: Listing by access time is not compared: reading the fixture changes directory atimes between the C and the Rust run.

## ADR-016: The interactive harness runs in tmux (supersedes ADR-009)
Status: accepted (2026-11-23)
Context: pyte mis-renders ncurses' minimal updates (the recorded filter screen showed "fir1/", "fimma"), so its "expected" screens held emulator bugs a different renderer cannot reproduce.
Decision: rust/tui-cases/harness.py runs each case in a detached 80x24 tmux pane (TERM=screen-256color, rust/tui-cases/tmux.conf), sends named keys with send-keys so tmux encodes them for the pane's mode, and reads capture-pane once the screen is stable for 300 ms. All cases were re-recorded from the original and pass 30/30 against it. The help case masks the VOLUME line (live free space of the temp filesystem).
Consequences: Screens are compared as a real terminal shows them; line-drawing characters are compared as Unicode box characters.

## ADR-017: Own curses layer instead of crossterm (supersedes ADR-005's terminal part)
Status: accepted (2026-11-23)
Context: nnn depends on ncurses' exact terminal modes (cbreak keeps ISIG and IXON, nonl), raw key codes compared against bindings (keypad decoding, ESC with escdelay, wide chars from get_wch), getch's implicit refresh, and windows for popups.
Decision: rust/app/src/curses.rs implements the subset nnn uses: a cell screen with windows and attributes, diff refresh with ANSI sequences, termios modes via rustix, keypad/mouse decoding, SIGWINCH via signal-hook, resize keeping contents.
Consequences: No crossterm dependency. Known gap: ncurses' per-window pending-multibyte buffer is not modelled (a multibyte character cut by a byte limit inside a popup renders differently; found by the popup oracle, not by any recorded screen).

## ADR-018: The Rust TUI stays behind a switch until W5
Status: superseded by ADR-019 (2026-11-23)
Context: W4 ports the screen, the main loop and every key action except copy/move/remove of the selection, archives, mounts, plugins, sessions, batch rename, the command prompt and editing the selection list (W5). A user hitting those keys in a Rust session would get "failed!" where the original works.
Decision: `RUST_ANYTHING_UNIT_TUI=rust` runs the Rust TUI (and list mode); without it the front door hands any run that reaches the screen to the original, as before. W5 ports the remaining actions and then makes Rust the default.
Consequences: Recorded behaviour is proved on both paths: replay 242/242, bytes 246/246, tui 30/30 with and without the switch.

## ADR-019: The Rust TUI is the default (supersedes ADR-018)
Status: superseded by ADR-020 (2026-11-23)
Context: W5 ported every remaining action (file operations, archives and mounts, plugins and the command prompt/shell/launcher, sessions, disk-usage mode, child environment via glibc setenv). The 48 recorded screen cases pass for the original and the Rust build.
Decision: Rust runs the whole program by default. Kill switches until the lift: RUST_ANYTHING_UNIT_TUI=original (screen and list mode go to the original), RUST_ANYTHING_UNIT_STARTUP=original (everything goes to the original).
Consequences: The fallback rate is 0% on every recorded case. Known differences, all outside recorded behaviour and listed in the agents' reports: no fork in run_plugin (a thread holds the FIFO's write end), F_NOWAIT uses a new process group instead of setsid, SIGTERM/SIGHUP are handled before the next key instead of at once, du mode avoids a data race the C has on the entry bit-field, ncurses' pending-multibyte buffer in popups (ADR-017), C buffer overflows on over-long input (Rust grows the buffer), and damaged session files with reserved bits.

## ADR-020: Lifted to a pure Rust binary (by hand)
Status: accepted (2026-11-23)
Context: `rust-anything app lift` expects unit crates wired with `app wire`; this port is one crate of modules (ADR-011), so the tool refused (no wired unit, front door changed).
Decision: The lift was done by hand: rust/app/src/original.rs, both kill switches and the fallback paths in startup.rs/main.rs are gone; rust/app/tests/app_cases.rs runs rust/app-cases with trycmd on `cargo test`. The C source stays in src/ as reference (ADR-008).
Consequences: The binary links no ncurses or readline. Proof on the pure binary: cargo test (57 unit tests + 242 app cases), app replay 242/242, bytes-check 246/246, tui 48/48 (archive-current waits for 1.5 s of stillness: tar's run made its snapshot racy for the original too), function oracle 15/15. Speed from tmpfs: startup about 0.1-0.3 ms slower (1.2 MB binary vs 168 KB), listing and sorting 20,000 files about 8% faster.

## ADR-021: Standalone repo with the C original as a submodule
Status: accepted (2026-11-24)
Context: The port lived in nnn/rust/, inside a checkout of the C project. A Windows port needs its own CI, its own history and a clean way to build the C original for the proofs.
Decision: nnn-rs is a separate repository. rust/ (crate, proofs, docs) is its own content; reference/nnn is a git submodule of https://github.com/jarun/nnn pinned at a93d0bd4. `git submodule update --init && make -C reference/nnn` builds the original used by replay, bytes-check, the screen harnesses and the oracles. .gitattributes marks recorded fixtures and cases `-text` so CRLF survives a Windows checkout.
Consequences: CI checks out with submodules. Nothing is pushed or published without the user's go-ahead (ADR-007).

## ADR-022: Platform layer; where unsafe may live (supersedes ADR-014's locale.rs rule)
Status: accepted (2026-11-24)
Context: ADR-014 made locale.rs the only module with `unsafe`. The Windows port needs console, file-information and process APIs, and the Unix code (curses termios, directory reading, ids, time) has to sit behind the same interface.
Decision: rust/app/src/platform/ is the OS boundary: mod.rs (the OS-independent API, re-exporting the active backend, and the path helpers), unix.rs, windows.rs and portable_locale.rs (ADR-023). `unsafe` is allowed only in platform/unix.rs and platform/windows.rs, each block with a `// SAFETY:` comment. locale.rs is a thin facade over platform with the same public functions. No `#[allow]`; `#[expect(.., reason = "..")]` only. Unix-only crates (rustix, libc) sit under `[target.'cfg(unix)'.dependencies]`, Windows-only (windows-sys, chrono) under `[target.'cfg(windows)'.dependencies]`. Linux behaviour stays byte-identical to the C original.
Consequences: Everything outside platform/ is safe Rust and OS-neutral. ADR-014's choice of glibc on Linux stands; only its "only locale.rs" sentence is replaced.

## ADR-023: Windows locale reference is C.UTF-8; the portable backend and its known differences
Status: accepted (2026-11-24)
Context: Windows has no glibc. nnn's sorting, case folding, widths, regex and time come from glibc locale tables (ADR-014).
Decision: platform/portable_locale.rs is a pure-Rust backend (unicode-width, regex, chrono), always used on Windows and on Unix with `--features portable`. Its reference is glibc's C.UTF-8, not ASCII: regex classes ([[:alpha:]], \w, ignore-case folding) are Unicode-aware as in C.UTF-8, collation is code point order, widths follow unicode-width. It is proven by the function oracle on Linux: `LOCALES=C.UTF-8 rust/oracle/run.sh <oracle built with --features portable,oracles>` gives 3/3 runs identical (seeds 1, 7, 100; 20000 calls each). What the oracle cannot reach (Unicode table versions, wcwidth oddities, [[:graph:]]/[[:print:]]/[[:punct:]], back-references, non-UTF-8 pattern bytes, malformed TZ) is listed in rust/oracle/portable-known-diffs.md.
Consequences: Other locales (en_US, tr_TR, de_DE collation) are not reproduced on Windows. The glibc backend stays the Linux default and its 15/15 oracle is unchanged.

## ADR-024: Windows feature matrix
Status: accepted (2026-11-24)
Context: Some nnn features rest on Unix concepts, others on tools Windows lacks. The target is x86_64-pc-windows-msvc, Windows 10 1809+ (ConPTY, VT sequences), Windows Terminal and conhost.
Decision:
- Unsupported, with nnn's own message: FIFO (the NNN_FIFO preview and selection feed), mounts (the mount/unmount key and FUSE archives), chmod (the permission editor).
- Ctrl-C is a key as on Linux. Ctrl-Break is ignored, like SIGQUIT on Linux, so the console modes stay intact; closing the console window exits.
- Git for Windows (sh.exe on PATH) is needed for sh-based plugins and for POSIX syntax in the shell and the command prompt; without it the shell and prompt fall back to `cmd /C` and plugins say "sh not found (Git for Windows)". Plain programs (editor, pager) run without it. The default opener is `cmd /C start ""`.
- List mode (paths from stdin, built as a tree of symlinks) needs symlink rights: Developer Mode or an elevated shell.
- Directory sizes, permission strings, link counts and the detail-mode octal column are NTFS approximations (ADR-027 wildcards them in screen cases).
- No shared recorded case is filtered on Windows: all 242 replay cases pass; 12 of the 48 screen cases are marked unix-only because they need Unix tools or Unix file details (36 shared + 4 Windows-only cases run).
Consequences: Config, session and selection file formats are unchanged (ADR-006). Windows-specific behaviour lives in rust/app-cases-windows and the Windows-only screen cases.

## ADR-025: Windows paths
Status: accepted (2026-11-24)
Context: nnn's code splits, joins and compares paths as bytes with `/`.
Decision: Internal paths are byte strings with `/` separators on every OS. On Windows the bytes are WTF-8 from `OsStr::as_encoded_bytes` (lone surrogates round-trip) and are only split at `/`; conversions to OsStr/UTF-16 happen at the platform boundary. Roots: `C:/` (drive roots; `h` there stays put like `/` on Linux), UNC `//server/share/` (going up stops there), and a drive-less `/x`, which abspath resolves to `x` on the drive (or share) of the directory it is resolved in. Elsewhere (root_len, is_absolute) a drive-less `/x` has no root and counts as relative, so only absolute paths (with a drive or a share) of 260 or more characters get the `\\?\` prefix when handed to the OS.
Consequences: The `\\?\` form bypasses GetFullPathNameW normalisation (no `..` or `/` fix-up past that point; the platform layer normalises first). Names never contain `\` separators on screen.

## ADR-026: Native file operations on Windows
Status: accepted (2026-11-24)
Context: On Linux nnn runs cp, mv and rm (and xdg tools) from the shell; Windows has none of them without Git for Windows.
Decision: rust/app/src/winops.rs does copy, move, remove, the file-information panel and directory-change polling natively. nnn's prompt texts are kept (overwrite, delete, "rm -rf" confirmations); the interactive answer to "rm -rf" (rm -ri on Linux) asks rm's own questions per entry ("rm: descend into directory 'd'?", "rm: remove regular file 'f'?"). A move across volumes onto an existing non-empty directory fails with "Directory not empty", as mv does; it never merges. Links are never followed and never replace a real directory; a tree is removed only after its prompt. File names are never put on a command line: they travel in environment variables (%NNN_CMD_ARGn%, `cmd /V:OFF`), so a name such as `a&calc.exe` cannot inject commands. Errors use glibc's strerror wording.
Known differences from Linux: `rm -rf` of a read-only file fails with "Permission denied" (rm removes it); `rm -v` output quotes names with control characters differently from GNU rm; directory-change polling misses content-only changes.
Consequences: Locked files and paths over 259 characters report an error, never panic.

## ADR-027: tui-check, the cross-platform screen harness
Status: accepted (2026-11-24)
Context: harness.py needs tmux and so runs on Linux only (ADR-016).
Decision: rust/tui-check (a standalone crate) runs the same rust/tui-cases/cases.json against the same expected files through a pty (ConPTY on Windows) and the vt100 crate. `check` compares, `record` re-records. On Windows it normalises what NTFS cannot match (permission strings, detail octal column, directory sizes, the N-INODE suffix, the temp directory in `\` and `/` spellings) and skips cases whose `platforms` list excludes it. Case directories are created at the drive root (C:\nnnh-...) so path lengths match the recorded ones; a UNC %TEMP% breaks that parity. ConPTY's host asks the program's terminal for the cursor position (DSR, ESC[6n) and waits for the answer, so the harness answers every request from the vt100 cursor, as tmux does.
Consequences: Linux: 48/48 against both Rust and the C original, agreeing with harness.py. Windows: 40/40 (36 shared + 4 Windows-only).

## ADR-028: Windows uses Microsoft Coreutils when installed
Status: accepted (2026-11-24)
Context: Microsoft's Coreutils for Windows (uutils coreutils 0.11.0, findutils xargs 0.10.0; `winget install Microsoft.Coreutils` or the release zip) provides the cp, mv, rm, stat and xargs nnn runs on Linux. ADR-026 replaced them with native code because Windows had none; with Coreutils, nnn can behave as on Linux.
Decision:
- Detection (rust/app/src/platform/tools.rs, once per run, files only, nothing is run): each PATH directory in order; `coreutils-manager.exe` there is the installed layout (one program per tool, `cp.exe`…), else `coreutils.exe` is the portable multicall layout (`coreutils.exe cp …`); with no hit, `%ProgramFiles%\coreutils\bin`. `NNN_COREUTILS=0` turns detection off.
- Tools: cp, mv, rm, stat, xargs. A tool the install lacks (`coreutils-manager disable rm`) falls back to the native code for that operation only.
- `platform::command` starts a bare `cp`/`mv`/`rm`/`stat`/`xargs` from the detected install by absolute path, so Git for Windows' usr/bin or a PowerShell alias never picks another one. getutil searches PATH/PATHEXT itself on Windows (no `which`).
- Selection copy/move/remove run `cp -iRp -- … .`, `mv -i -- … .`, `rm -{f,i}vr -- …` in the target directory with the selected paths as separate arguments (no cmd line, so `&`, `%` and the like are never acted on), split into several runs when the command line would pass 32,000 bytes. The single-file remove runs `rm -vr{f,i} --` as on Linux. Duplicate runs `cp -rp --`. The file details popup shows `stat`. Archiving the selection runs `xargs -0 -a <list> <archiver> <archive>`.
- The plain `tar -acvf` archiver is `%SystemRoot%\System32\tar.exe` (bsdtar) when present, with or without Coreutils: GNU tar from Git for Windows reads `C:/x.tar` as a remote host.
- rust/tui-check runs every case with `NNN_COREUTILS=0` except those with `"requires": ["coreutils"]`, which run only with `--coreutils` (otherwise `SKIP`).
Known differences from Linux: prompts and messages carry `cp.exe:` (installed layout) or `cp:` (portable) and `\` in paths (`overwrite '.\Gamma'?`); the popup has no `file` lines (Coreutils has no `file`); `stat` shows Uid 0 and Gid `None`. A declined `cp -i` exits 1, as GNU cp does, so nnn shows "failed!" and keeps the selection. Still needing Git for Windows (ADR-024): sh plugins, batch rename, copy/move as, POSIX syntax in the shell and command prompt.
Consequences: ADR-024's "Windows has none of them" and ADR-026's native operations now apply only without Coreutils (or with NNN_COREUTILS=0). Windows screen cases: 40 without Coreutils (4 skipped), 44 with it; CI runs both (jobs windows and windows-coreutils, the latter with the portable layout).
