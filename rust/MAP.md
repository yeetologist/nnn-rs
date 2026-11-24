# nnn → Rust port map

Goal: pure Rust `nnn` (default build) behaving as the C original. Decisions: [DECISIONS.md](DECISIONS.md) (ADR-001..008; W7 adds 021-027). Terms: [GLOSSARY.md](GLOSSARY.md). Measured state: PORT_MAP.md (generated).

| Wave | Unit | State |
|---|---|---|
| W1 | recording (app-cases, tui-cases) + front door (rust/app) | done 2026-11-23: app replay 105/105 (original and front door), tui harness 30/30 (original and front door), fallback 100% |
| W2 | options: getopt (glibc), parse_opts, parse_env_opts (NNN_OPTS), usage, -V, -K + bindings table, -p file, /dev/null and TMPDIR checks, empty list-mode input, x11 cleanup | done 2026-11-23: replay 242/242, bytes 246/246, tui 30/30, fallback 0.4% (1/242) |
| W3 | sort (xstricmp, xstrverscasecmp, entrycmp, reventrycmp, fuzzyentrycmp, set_sort_flags, getorderstr), filters (strstr, fuzzy, regex, match positions), coolsize, get_lsperms, get_detail_ind, unescape, print_time, dentfill (no du) | done 2026-11-23: oracle 15/15 runs x 32,100 calls identical; replay 242/242, bytes 246/246, tui 30/30 |
| W4 | curses layer, drawing/navigation/filters/contexts (draw.rs), browse loop + TUI main + colors + FIFO + inotify + preview (browse.rs), prompts + native line editor + history (prompt.rs), spawn/popups/stats/help (popup.rs), config/bookmarks/kv (config.rs), selection buffer (selection.rs), list mode (listmode.rs) | done 2026-11-23 behind RUST_ANYTHING_UNIT_TUI=rust (ADR-018): tui 30/30, replay 242/242 (fallback 0%), bytes 246/246; module oracles: prompt 150k, config 240k, browse 15k, listmode 39 cases, popup (ASCII windows identical) |
| W5 | fileops.rs (copy/move/remove selection, batch rename, edit selection), mounts.rs (archives, mounts), plugin.rs (plugins, cmd prompt, shell, launcher, child env), session.rs (save/load, byte-identical files), du.rs (disk usage mode); Rust TUI made the default (ADR-019) | done 2026-11-23: tui 48/48 (original 48/48), replay 242/242 fallback 0%, bytes 246/246, function oracle 15/15; module oracles: fileops 15k, session 6 seeds + 1,323 cross-loads, du 5,400 calls |
| W6 | lift: fallback and kill switches removed, app-cases as cargo test (ADR-020) | done 2026-11-23: pure binary tui 48/48, replay 242/242, bytes 246/246, cargo test green, oracle 15/15 |
| W7 | Windows: platform layer, console, portable locale, native file ops, tui-check | in progress: T1-T9 done (Linux 91 unit, 242/242 replay, 246/246 bytes, 48/48 tui via harness.py and tui-check vs Rust and C, 15 oracle ok; Windows: cargo test 110 unit + 1 + 8 integration, clippy clean, replay 242/242, tui-check 40/40) |

Out of scope (ADR-003): icon builds, haiku, macos-legacy, PCRE build.

Open questions: ADR-012 and ADR-014 await the user's confirmation. Out of scope still (ADR-003): icon builds, haiku, macOS/BSD, PCRE build.
