//! Drawing and navigation (src/nnn.c): redraw, statusbar, printent, the cursor and screen
//! moves, populate, the filter prompt (filterentries), nextsel, contexts and parent dirs.
//!
//! Conventions shared with browse.rs: the C pointers `path`, `lastdir` and `lastname` always
//! point into the current context, so here they are `self.g_ctx[self.cfg.curctx]`'s
//! `c_path`, `c_last` and `c_name`; functions that take them in C read and write those
//! fields. Keys are raw curses values (`i32`): characters and `KEY_*` codes alike, as nnn.c
//! compares them. `c_fltr` is a fixed `REGEX_MAX` byte buffer (see [`Nnn::fltr`]).
//! Needs from browse.rs: `notify_fifo(force)` and `poll_dir_watch() -> Option<i32>` (the
//! inotify part of nextsel). Needs from selection.rs: `scanselforpath`, `findmarkentry`,
//! `isselfileempty`, `clearselection`. From popup.rs: `get_output`. From prompt.rs:
//! `printmsg`, `printmsg_nc`, `printwarn`, `get_input`, `xreadline`, `xdelay`,
//! `clearoldprompt`.

use crate::curses::{
    self, A_BOLD, A_DIM, A_REVERSE, A_STANDOUT, A_UNDERLINE, Input, Window, color_pair,
};
use crate::fmt;
use crate::keys::{Action, BINDINGS};
use crate::locale;
use crate::platform;
use crate::sort::{
    self, DIR_OR_DIRLNK, Entry, FILE_MISSING, FILE_SELECTED, FILE_YOUNG, HARD_LINK, SYM_ORPHAN,
};
use crate::state::{CTX_MAX, NAME_MAX, Nnn, REGEX_MAX, Settings};

pub const FILTER: i32 = b'/' as i32;
pub const RFILTER: i32 = b'\\' as i32;
pub const CASE: i32 = b':' as i32;
pub const MSGWAIT: i32 = b'$' as i32;
pub const SELECT: i32 = b' ' as i32;
pub const ESC: i32 = 27;
pub const DEL: i32 = 127;
pub const SCROLLOFF: i32 = 3;
pub const CTX_INVALID: usize = 0b111;
pub const MIN_DISPLAY_COL: i32 = (CTX_MAX * 2) as i32;
pub const XDELAY_INTERVAL_MS: u64 = 350_000;
const PATH_MAX: usize = 4096;

pub const fn control(c: u8) -> i32 {
    (c & 0x1f) as i32
}

// Color pair numbers (C_*).
pub const C_BLK: u32 = CTX_MAX as u32 + 1;
pub const C_CHR: u32 = C_BLK + 1;
pub const C_DIR: u32 = C_CHR + 1;
pub const C_EXE: u32 = C_DIR + 1;
pub const C_FIL: u32 = C_EXE + 1;
pub const C_HRD: u32 = C_FIL + 1;
pub const C_LNK: u32 = C_HRD + 1;
pub const C_MIS: u32 = C_LNK + 1;
pub const C_ORP: u32 = C_MIS + 1;
pub const C_PIP: u32 = C_ORP + 1;
pub const C_SOC: u32 = C_PIP + 1;
pub const C_UND: u32 = C_SOC + 1;

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFSOCK: u32 = 0o140000;
const S_IFIFO: u32 = 0o010000;
const S_IFBLK: u32 = 0o060000;
const S_IFCHR: u32 = 0o020000;

/// State this module keeps.
#[derive(Debug, Clone)]
pub struct CoreState {
    /// `fnstrstr == &strcasestr` (and `REG_ICASE` in `regflags`): filters ignore case.
    pub filter_ic: bool,
}

impl Default for CoreState {
    fn default() -> Self {
        CoreState { filter_ic: true }
    }
}

/// `istopdir`
pub fn istopdir(path: &[u8]) -> bool {
    crate::platform::is_root(path)
}

/// The bytes of a C string held in a buffer: up to the first NUL.
pub fn cstr(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

/// `xstrsncpy(dst, src, n)` into a C-string Vec: at most n - 1 bytes.
pub fn strsncpy(src: &[u8], n: usize) -> Vec<u8> {
    let s = cstr(src);
    s[..s.len().min(n.saturating_sub(1))].to_vec()
}

impl Nnn {
    fn w(&mut self) -> &mut Window {
        &mut self.cs().stdscr
    }

    fn onscreen(&self) -> i32 {
        self.xlines - 4
    }

    /// The current context's `c_fltr` as the C buffer it is: `REGEX_MAX` bytes.
    pub fn fltr(&mut self) -> &mut Vec<u8> {
        let ctx = self.cfg.curctx;
        let f = &mut self.g_ctx[ctx].c_fltr;
        if f.len() != REGEX_MAX {
            f.resize(REGEX_MAX, 0);
        }
        f
    }

    /// `filterset()`
    pub fn filterset(&mut self) -> bool {
        self.fltr()[1] != 0
    }

    /// `clearfilter`
    pub fn clearfilter(&mut self) {
        let f = self.fltr();
        if f[1] != 0 {
            f[REGEX_MAX - 1] = f[1];
            f[1] = 0;
        }
    }

    /// `resetfilter`
    pub fn resetfilter(&mut self, presel: &mut i32) {
        if self.cfg.filtermode {
            *presel = FILTER;
            self.clearfilter();
        }
    }

    /// `attron_curctx`
    pub fn attron_curctx(&mut self) {
        let p = color_pair(self.cfg.curctx as u32 + 1);
        self.w().attron(p);
    }

    /// `attroff_curctx`
    pub fn attroff_curctx(&mut self) {
        let p = color_pair(self.cfg.curctx as u32 + 1);
        self.w().attroff(p);
    }

    /// `tolastln()`
    pub fn tolastln(&mut self) {
        let y = self.xlines - 1;
        self.w().mv(y, 0);
    }

    /// `tocursor()`
    pub fn tocursor(&mut self) {
        let y = self.cur + 2 - self.curscroll;
        self.w().mv(y, 0);
    }

    /// `settimeout()`
    pub fn settimeout(&mut self) {
        self.cs().timeout(1000);
    }

    /// `cleartimeout()`
    pub fn cleartimeout(&mut self) {
        self.cs().timeout(-1);
    }

    /// `setcfg`: the comparator and filter function pointers follow `cfg`.
    pub fn setcfg(&mut self, newcfg: Settings) {
        self.cfg = newcfg;
        self.cfg.sort.rev_fn = self.cfg.sort.reverse;
        self.cfg.sort.vers_fn = self.cfg.sort.version;
    }

    /// `copycurname`
    pub fn copycurname(&mut self) {
        let name = if self.ndents > 0 {
            strsncpy(&self.pdents[self.cur as usize].name, NAME_MAX + 1)
        } else {
            Vec::new()
        };
        let ctx = self.cfg.curctx;
        self.g_ctx[ctx].c_name = name;
    }

    /// `copynextname`
    pub fn copynextname(&mut self) {
        if self.cur != 0 {
            self.cur += if self.cur != self.ndents - 1 { 1 } else { -1 };
            self.copycurname();
        } else {
            let ctx = self.cfg.curctx;
            self.g_ctx[ctx].c_name.clear();
        }
    }

    /// `dentfind`: the index of the entry named `fname`, 0 when none is.
    pub fn dentfind(&self, fname: &[u8], n: i32) -> i32 {
        let fname = cstr(fname);
        (0..n)
            .find(|&i| self.pdents[i as usize].name == fname)
            .unwrap_or(0)
    }

    /// `move_cursor`
    pub fn move_cursor(&mut self, target: i32, ignore_scrolloff: bool) {
        let onscreen = self.onscreen();
        let target = 0.max((self.ndents - 1).min(target));
        self.last_curscroll = self.curscroll;
        self.last = self.cur;
        self.cur = target;
        if !ignore_scrolloff {
            let delta = target - self.last;
            let scrolloff = SCROLLOFF.min(onscreen >> 1);
            if (self.cur < self.curscroll + scrolloff && delta < 0)
                || (self.cur > self.curscroll + onscreen - scrolloff - 1 && delta > 0)
            {
                self.curscroll += delta;
            }
        }
        self.curscroll = self.curscroll.min(self.cur.min(self.ndents - onscreen));
        self.curscroll = self.curscroll.max((self.cur - (onscreen - 1)).max(0));
        if !self.g_state.fifomode {
            self.notify_fifo(false);
        }
    }

    /// `move_cursor_next`
    pub fn move_cursor_next(&mut self) {
        let next = (self.cur + 1) % self.ndents;
        self.move_cursor(next, false);
    }

    /// `handle_cur_move`
    pub fn handle_cur_move(&mut self, sel: Action) -> bool {
        match sel {
            Action::Next => {
                if self.cfg.rollover || self.cur != self.ndents - 1 {
                    self.move_cursor_next();
                }
                true
            }
            Action::Prev => {
                if self.cfg.rollover || self.cur != 0 {
                    let t = (self.cur + self.ndents - 1) % self.ndents;
                    self.move_cursor(t, false);
                }
                true
            }
            _ => false,
        }
    }

    /// `handle_screen_move`
    pub fn handle_screen_move(&mut self, sel: Action) {
        let onscreen = self.onscreen();
        match sel {
            Action::Next | Action::Prev => {
                self.handle_cur_move(sel);
            }
            Action::Pgdn => {
                self.move_cursor(self.curscroll + (onscreen - 1), true);
                self.curscroll += onscreen - 1;
            }
            Action::CtrlD => {
                self.move_cursor(self.curscroll + (onscreen - 1), true);
                self.curscroll += onscreen >> 1;
            }
            Action::Pgup => {
                self.move_cursor(self.curscroll, true);
                self.curscroll -= onscreen - 1;
            }
            Action::CtrlU => {
                self.move_cursor(self.curscroll, true);
                self.curscroll -= onscreen >> 1;
            }
            Action::Jump => {
                let input = self.xreadline(None, b"jump (+n/-n/n): ");
                if input.is_empty() {
                    return;
                }
                if input[0] == b'-' {
                    self.cur -= crate::opts::atoi(&input[1..]);
                    if self.cur < 0 {
                        self.cur = 0;
                    }
                } else if input[0] == b'+' {
                    self.cur += crate::opts::atoi(&input[1..]);
                    if self.cur >= self.ndents {
                        self.cur = self.ndents - 1;
                    }
                } else {
                    let index = crate::opts::atoi(&input);
                    if index < 1 || index > self.ndents {
                        return;
                    }
                    self.cur = index - 1;
                }
                self.move_cursor(self.cur, true);
                self.curscroll -= onscreen >> 1;
            }
            Action::Home => self.move_cursor(0, true),
            Action::End => self.move_cursor(self.ndents - 1, true),
            Action::Young => {
                let mut r = self.cur;
                loop {
                    r += 1;
                    if r >= self.ndents {
                        r = 0;
                    }
                    if r == self.cur {
                        break;
                    }
                    if self.pdents[r as usize].flags & FILE_YOUNG != 0 {
                        self.move_cursor(r, false);
                        break;
                    }
                }
            }
            _ => {
                // SEL_FIRST
                let c = self.get_input(Some(b"first file (')/char?"));
                if c == 0 {
                    return;
                }
                let up = |c: i32| {
                    if (b'a' as i32..=b'z' as i32).contains(&c) {
                        c - 32
                    } else {
                        c
                    }
                };
                let c = up(c);
                let first = |e: &Entry| i32::from(e.name.first().copied().unwrap_or(0));
                let mut r = if c == up(first(&self.pdents[self.cur as usize])) {
                    self.cur + 1
                } else {
                    0
                };
                while r < self.ndents {
                    let e = &self.pdents[r as usize];
                    if (c == b'\'' as i32 && e.flags & DIR_OR_DIRLNK == 0) || c == up(first(e)) {
                        self.move_cursor(r % self.ndents, false);
                        break;
                    }
                    r += 1;
                }
            }
        }
    }

    /// `populate`
    pub fn populate(&mut self, path: &[u8], lastname: &[u8]) {
        let fill = crate::dir::FillCfg {
            showhidden: self.cfg.sort.showhidden,
            timetype: self.cfg.sort.timetype,
            listpath: self.listpath.is_some(),
        };
        self.gtimesecs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.pdents = self.dentfill(path, fill);
        self.ndents = self.pdents.len() as i32;
        if self.ndents == 0 {
            return;
        }
        let c = self.cfg.sort.clone();
        self.pdents.sort_by(|a, b| c.cmp(a, b));
        let target = if lastname.first().is_some_and(|&b| b != 0) {
            self.dentfind(lastname, self.ndents)
        } else {
            0
        };
        self.move_cursor(target, false);
        self.last_curscroll = -1;
    }

    /// `get_color_pair_name_ind`: the color pair, the indicator and extra attributes.
    pub fn get_color_pair_name_ind(&self, ent: &Entry, pind: &mut u8, pattr: &mut u32) -> u32 {
        match ent.mode & S_IFMT {
            S_IFREG => {
                if ent.size == 0 {
                    if ent.mode & 0o100 != 0 {
                        *pind = b'*';
                    }
                    return C_UND;
                }
                if ent.flags & HARD_LINK != 0 {
                    if ent.mode & 0o100 != 0 {
                        *pind = b'*';
                    }
                    return C_HRD;
                }
                if ent.mode & 0o100 != 0 {
                    *pind = b'*';
                    return C_EXE;
                }
                C_FIL
            }
            S_IFDIR => {
                *pind = b'/';
                if self.g_state.oldcolor {
                    return C_DIR;
                }
                *pattr |= A_BOLD;
                if self.g_state.dirctx {
                    self.cfg.curctx as u32 + 1
                } else {
                    C_DIR
                }
            }
            S_IFLNK => {
                if ent.flags & DIR_OR_DIRLNK != 0 {
                    *pind = b'/';
                    *pattr |= if self.g_state.oldcolor { A_DIM } else { A_BOLD };
                } else {
                    *pind = b'@';
                    if self.g_state.oldcolor {
                        *pattr |= A_DIM;
                    }
                }
                if !self.g_state.oldcolor || self.cfg.sort.showdetail {
                    return if ent.flags & SYM_ORPHAN != 0 {
                        C_ORP
                    } else {
                        C_LNK
                    };
                }
                0
            }
            S_IFSOCK => {
                *pind = b'=';
                C_SOC
            }
            S_IFIFO => {
                *pind = b'|';
                C_PIP
            }
            S_IFBLK => C_BLK,
            S_IFCHR => C_CHR,
            _ => {
                *pind = b'?';
                C_UND
            }
        }
    }

    /// `resetdircolor`
    pub fn resetdircolor(&mut self, flags: u8) {
        if self.g_state.dircolor && flags & DIR_OR_DIRLNK == 0 {
            let a = color_pair(self.cfg.curctx as u32 + 1) | A_BOLD;
            self.w().attroff(a);
            self.g_state.dircolor = false;
        }
    }

    /// `printent_name` (with DIM_FILTERED)
    fn printent_name(&mut self, ent: &Entry, namecols: usize) {
        let fltr = self.fltr().clone();
        let wbuf = fmt::unescape(&ent.name, namecols);
        if fltr[1] == 0 || self.cfg.regex {
            self.w().addnwstr(&wbuf, -1);
            return;
        }
        let ic = self.core.filter_ic;
        let f = cstr(&fltr[1..]);
        let matched = if self.cfg.fuzzy {
            sort::fuzzy_match_positions(f, &ent.name, ic)
        } else {
            sort::string_match_positions(f, &ent.name, ic)
        };
        for (col, &c) in wbuf.iter().enumerate().take(namecols) {
            if c == 0 {
                break;
            }
            let dim = matched.contains(&col);
            let w = self.w();
            if dim {
                w.attron(A_DIM);
            }
            w.addnwstr(&[c], 1);
            if dim {
                w.attroff(A_DIM);
            }
        }
    }

    /// `printent`
    pub fn printent(&mut self, idx: usize, mut namecols: i32, sel: bool) {
        let ent = self.pdents[idx].clone();
        let mut ind = 0u8;
        if self.cfg.sort.showdetail {
            let ty = ent.mode & S_IFMT;
            let perms = format!(
                "  {}{}{}",
                (ent.mode >> 6) & 7,
                (ent.mode >> 3) & 7,
                ent.mode & 7
            );
            self.w().addch(' ');
            let attrs = if self.g_state.oldcolor {
                self.resetdircolor(ent.flags);
                A_DIM
            } else if self.fcolors_mis() {
                color_pair(C_MIS)
            } else {
                0
            };
            if attrs != 0 {
                self.w().attron(attrs);
            }
            self.print_time(ent.sec, ent.flags);
            let size = if ty == S_IFREG || ty == S_IFDIR {
                let v = if self.cfg.sort.blkorder {
                    (ent.blocks as i64) << self.blk_shift
                } else {
                    ent.size
                };
                fmt::coolsize(v)
            } else {
                fmt::get_detail_ind(ent.mode).to_string()
            };
            let text = format!("{perms}{size:>9} ");
            self.w().addstr(text.as_bytes());
            if attrs != 0 {
                self.w().attroff(attrs);
            }
        }
        if self.g_state.showlines {
            let rel = idx as i32 - self.cur;
            let s = if rel == 0 {
                format!("{rel:4}")
            } else {
                format!("{rel:+4}")
            };
            self.w().addstr(s.as_bytes());
        }
        let mut attrs = 0u32;
        let mut color = self.get_color_pair_name_ind(&ent, &mut ind, &mut attrs);
        if ent.flags & FILE_SELECTED != 0 {
            let w = self.w();
            let keep = w.attr;
            w.attrset(keep | A_REVERSE | A_BOLD);
            w.addch('+');
            w.attrset(keep);
        } else {
            self.w().addch(' ');
        }
        if self.g_state.oldcolor {
            self.resetdircolor(ent.flags);
        } else {
            if ent.flags & FILE_MISSING != 0 {
                color = C_MIS;
            }
            if color != 0 && self.fcolors.get(color as usize).is_some_and(|&c| c != 0) {
                attrs |= color_pair(color);
            }
        }
        if sel {
            attrs |= A_REVERSE | A_BOLD;
        }
        if attrs != 0 {
            self.w().attron(attrs);
        }
        if ind == 0 {
            namecols += 1;
        }
        self.printent_name(&ent, namecols.max(0) as usize);
        if attrs != 0 {
            self.w().attroff(attrs);
        }
        if ind != 0 {
            self.w().addch(char::from(ind));
        }
    }

    fn fcolors_mis(&self) -> bool {
        self.fcolors.get(C_MIS as usize).is_some_and(|&c| c != 0)
    }

    /// `print_time`
    pub fn print_time(&mut self, secs: i64, flags: u8) {
        let s = fmt::print_time(secs);
        let w = self.w();
        if flags & FILE_YOUNG != 0 {
            w.attron(A_REVERSE);
        }
        w.addstr(s.as_bytes());
        if flags & FILE_YOUNG != 0 {
            w.attroff(A_REVERSE);
        }
    }

    /// `adjust_cols`
    pub fn adjust_cols(&mut self, mut n: i32) -> i32 {
        if self.cfg.sort.showdetail {
            if n < 36 {
                self.cfg.sort.showdetail = !self.cfg.sort.showdetail;
            } else {
                n -= 32;
            }
        }
        n - 2
    }

    /// `markhovered`
    pub fn markhovered(&mut self) {
        if self.cfg.sort.showdetail && self.ndents > 0 {
            self.tocursor();
            let w = self.w();
            let keep = w.attr;
            w.attrset(keep | A_BOLD);
            w.addch('>');
            w.attrset(keep);
        }
    }

    /// `draw_line`: the fast redraw of the previous and the current entry.
    pub fn draw_line(&mut self, ncols: i32) {
        let ncols = self.adjust_cols(ncols);
        let dirattr = color_pair(self.cfg.curctx as u32 + 1) | A_BOLD;
        let mut dir = false;
        if self.g_state.oldcolor && self.pdents[self.last as usize].flags & DIR_OR_DIRLNK != 0 {
            self.w().attron(dirattr);
            dir = true;
        }
        let y = 2 + self.last - self.curscroll;
        self.w().mv(y, 0);
        self.printent(self.last as usize, ncols, false);
        if self.g_state.oldcolor && self.pdents[self.cur as usize].flags & DIR_OR_DIRLNK != 0 {
            if !dir {
                self.w().attron(dirattr);
                dir = true;
            }
        } else if dir {
            self.w().attroff(dirattr);
            dir = false;
        }
        let y = 2 + self.cur - self.curscroll;
        self.w().mv(y, 0);
        self.printent(self.cur as usize, ncols, true);
        if dir {
            self.w().attroff(dirattr);
        }
        self.markhovered();
    }

    /// `set_tilde_in_path` + printing: the path as shown, with `~` for HOME.
    fn path_shown(&self, path: &[u8]) -> (Vec<u8>, bool) {
        let home = cstr(&self.home);
        if !home.is_empty() && path.starts_with(home) {
            let mut s = vec![b'~'];
            s.extend_from_slice(&path[home.len()..]);
            (s, true)
        } else {
            (path.to_vec(), false)
        }
    }

    /// `redraw`
    pub fn redraw(&mut self, path: &[u8]) {
        let (lines, cols) = (self.cs().lines(), self.cs().cols());
        self.xlines = lines;
        self.xcols = cols;
        let mut ncols = if self.xcols <= PATH_MAX as i32 {
            self.xcols
        } else {
            PATH_MAX as i32
        };
        if self.cfg.preview && self.xcols >= 81 {
            ncols = ncols.min(self.xcols / 2);
        }
        if self.g_state.mv {
            self.g_state.mv = false;
            if self.ndents > 0 && self.last_curscroll == self.curscroll && !self.cfg.preview {
                self.draw_line(ncols);
                return;
            }
        }
        self.w().erase();
        self.move_cursor(self.cur, true);
        if ncols <= MIN_DISPLAY_COL {
            self.printmsg(b"too few cols!");
            return;
        }
        for i in 0..CTX_MAX {
            let c = char::from(b'1' + i as u8);
            if !self.g_ctx[i].c_cfg.ctxactive {
                self.w().addch(c);
            } else {
                let extra = if self.cfg.curctx != i {
                    A_UNDERLINE
                } else {
                    A_REVERSE
                };
                let w = self.w();
                let keep = w.attr;
                w.attrset(keep | color_pair(i as u32 + 1) | A_BOLD | extra);
                w.addch(c);
                w.attrset(keep);
            }
            self.w().addch(' ');
        }
        let pathattr = A_UNDERLINE | color_pair(self.cfg.curctx as u32 + 1);
        self.w().attron(pathattr);
        let (shown, in_home) = self.path_shown(cstr(path));
        let i = shown.len() as i32;
        if i + MIN_DISPLAY_COL <= ncols {
            self.w().addnstr(&shown, ncols - MIN_DISPLAY_COL);
        } else {
            let base = shown.iter().rposition(|&c| c == b'/');
            let mut p = 0usize;
            let mut used: i32;
            if in_home {
                let c = shown[0];
                self.w().addch(char::from(c));
                p = 1;
                used = 1;
            } else {
                used = 0;
            }
            let mut base = base;
            if let Some(b) = base
                && b != p
            {
                while p < b {
                    if shown[p] == b'/' {
                        used += 2;
                        if ncols < used + MIN_DISPLAY_COL {
                            base = None;
                            break;
                        }
                        let (c1, c2) = (shown[p], shown.get(p + 1).copied().unwrap_or(0));
                        self.w().addch(char::from(c1));
                        p += 1;
                        self.w().addnstr(&[c2], 1);
                    }
                    p += 1;
                }
            }
            if let Some(b) = base {
                self.w()
                    .addnstr(&shown[b..], ncols - (MIN_DISPLAY_COL + used));
            }
        }
        self.w().attroff(pathattr);
        if self.curscroll > 0 {
            self.w().mv(1, 0);
            self.w().addch('^');
        }
        if self.g_state.oldcolor {
            let a = color_pair(self.cfg.curctx as u32 + 1) | A_BOLD;
            self.w().attron(a);
            self.g_state.dircolor = true;
        }
        let onscreen = (self.onscreen() + self.curscroll).min(self.ndents);
        let (len, gsel) = self.scanselforpath(cstr(path), false);
        let ncols = self.adjust_cols(ncols);
        let mut j = 1;
        for i in self.curscroll..onscreen {
            j += 1;
            self.w().mv(j, 0);
            if len != 0 {
                self.findmarkentry(&gsel, i as usize);
            }
            self.printent(i as usize, ncols, i == self.cur);
        }
        if self.g_state.dircolor {
            let a = color_pair(self.cfg.curctx as u32 + 1) | A_BOLD;
            self.w().attroff(a);
            self.g_state.dircolor = false;
        }
        if onscreen < self.ndents {
            let y = self.xlines - 2;
            self.w().mv(y, 0);
            self.w().addch('v');
        }
        self.markhovered();
        if self.cfg.preview {
            self.preview_pane(cstr(path));
        }
    }

    /// `statusbar`
    pub fn statusbar(&mut self, path: &[u8]) {
        if self.ndents == 0 {
            self.printmsg(b"0/0");
            return;
        }
        let pent = self.pdents[self.cur as usize].clone();
        let mut ext: Vec<u8> = b"\x08".to_vec();
        if pent.mode & S_IFMT == S_IFREG
            && let Some(dot) = pent.name.iter().rposition(|&c| c == b'.')
        {
            let len = pent.name.len() - dot;
            if (2..=5).contains(&len) {
                ext = pent.name[dot..].to_vec();
            }
        }
        self.attron_curctx();
        if self.cfg.fileinfo {
            let name = pent.name.clone();
            if let Some(out) = self.get_output_buf(b"file", Some(b"-b"), Some(&name)) {
                let y = self.xlines - 2;
                self.w().mvaddstr(y, 2, &out);
            }
        }
        self.tolastln();
        let head = format!("{}/{} ", self.cur + 1, self.ndents);
        self.w().addstr(head.as_bytes());
        if self.g_state.selmode || self.nselected != 0 {
            let (range, selmode, n) = (self.g_state.rangesel, self.g_state.selmode, self.nselected);
            let w = self.w();
            w.attron(A_REVERSE);
            w.addch(' ');
            if range {
                w.addch('*');
            } else if selmode {
                w.addch('+');
            }
            if n != 0 {
                w.addstr((n as u32).to_string().as_bytes());
            }
            w.addch(' ');
            w.attroff(A_REVERSE);
            w.addch(' ');
        }
        if self.cfg.sort.blkorder {
            let du = fmt::coolsize(self.dir_blocks << self.blk_shift);
            let avail = fmt::coolsize(self.get_fs_info(path, true) as i64);
            let line = format!(
                "{}u:{} avail:{} files:{} {}B ",
                if self.cfg.sort.apparentsz { 'a' } else { 'd' },
                du,
                avail,
                self.num_files,
                pent.blocks << self.blk_shift
            );
            let w = self.w();
            w.addstr(line.as_bytes());
            w.addstr(&ext);
            w.addch('\n');
        } else {
            if self.cfg.filtermode {
                self.w().addch('F');
            }
            let sort = self.cfg.sort.getorderstr();
            if sort.is_empty() {
                self.w().addch(' ');
            } else {
                self.w().addstr(sort.as_bytes());
            }
            self.print_time(pent.sec, pent.flags);
            let perms = fmt::get_lsperms(pent.mode);
            let w = self.w();
            w.addch(' ');
            w.addstr(perms.as_bytes());
            w.addch(' ');
            if self.g_state.uidgid {
                let u = platform::user_name(pent.uid)
                    .unwrap_or_else(|| pent.uid.to_string().into_bytes());
                let g = platform::group_name(pent.gid)
                    .unwrap_or_else(|| pent.gid.to_string().into_bytes());
                let w = self.w();
                w.addstr(&u);
                w.addch(':');
                w.addstr(&g);
                w.addch(' ');
            }
            if pent.mode & S_IFMT == S_IFLNK {
                if !self.cfg.fileinfo {
                    let link = platform::readlink(&pent.name);
                    let (i, target) = match link {
                        Ok(b) => (b.len().min(PATH_MAX) as i64, b),
                        Err(_) => (-1, Vec::new()),
                    };
                    self.w()
                        .addstr(fmt::coolsize(if i >= 0 { i } else { pent.size }).as_bytes());
                    if i > 1 {
                        self.w().addstr(b" ->");
                        let x = self.w().cx;
                        let n = (i as i32).min(self.xcols - x).max(0) as usize;
                        self.w().addstr(&target[..n.min(target.len())]);
                    }
                }
            } else {
                let w = self.w();
                w.addstr(fmt::coolsize(pent.size).as_bytes());
                w.addch(' ');
                w.addstr(&ext);
                if pent.flags & HARD_LINK != 0
                    && let Ok(sb) = platform::stat(&pent.name, true)
                {
                    let s = format!(" {}-{}", sb.nlink as i32 as u32, sb.ino as i32 as u32);
                    let mut parts = s.splitn(2, '-');
                    let (a, b) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                    let w = self.w();
                    w.addstr(a.as_bytes());
                    w.addch('-');
                    w.addstr(b.as_bytes());
                }
            }
            self.w().clrtoeol();
        }
        self.attroff_curctx();
        self.tocursor();
    }

    /// `showfilterinfo`
    fn showfilterinfo(&mut self) {
        let mut info = self.cfg.sort.getorderstr().into_bytes();
        let name = if self.ndents > 0 {
            Some(self.pdents[self.cur as usize].name.clone())
        } else {
            None
        };
        let fileinfo = if self.cfg.fileinfo {
            name.and_then(|n| self.get_output_buf(b"file", Some(b"-b"), Some(&n)))
        } else {
            None
        };
        if let Some(out) = fileinfo {
            let y = self.xlines - 2;
            self.w().mvaddstr(y, 2, &out);
        } else {
            let mode = if self.cfg.regex {
                "reg"
            } else if self.cfg.fuzzy {
                "fzy"
            } else {
                "str"
            };
            let ic = if self.core.filter_ic { "ic" } else { "noic" };
            let add = format!("  {mode} [/], {ic} [:]");
            let room = REGEX_MAX.saturating_sub(info.len() + 1).saturating_sub(1);
            info.extend_from_slice(&add.as_bytes()[..add.len().min(room)]);
        }
        let (y, x) = (self.xlines - 2, self.xcols - info.len() as i32);
        self.w().mvaddstr(y, x, &info);
    }

    /// `showfilter`
    fn showfilter(&mut self, ln: &[u8]) {
        self.attron_curctx();
        self.showfilterinfo();
        self.printmsg_nc(cstr(ln));
        self.attroff_curctx();
    }

    /// `fill` + `matches`: keep the entries the filter shows, sorted; -1 for a bad regex.
    pub fn matches(&mut self, fltr: &[u8]) -> i32 {
        let ic = self.core.filter_ic;
        let re = if self.cfg.regex {
            match locale::Regex::new(fltr, ic) {
                Some(r) => Some(r),
                None => return -1,
            }
        } else {
            None
        };
        let visible = |name: &[u8]| -> bool {
            if let Some(re) = &re {
                re.is_match(name)
            } else if self.cfg.fuzzy {
                sort::fuzzy_match(fltr, name, ic)
            } else {
                sort::visible_str(fltr, name, ic)
            }
        };
        let mut count = 0usize;
        let mut ndents = self.ndents as usize;
        let mut keep: Vec<bool> = Vec::new();
        let _ = &mut keep;
        while count < ndents {
            if !visible(&self.pdents[count].name) {
                ndents -= 1;
                if count != ndents {
                    self.pdents.swap(count, ndents);
                }
            } else {
                count += 1;
            }
        }
        self.ndents = ndents as i32;
        let c = self.cfg.sort.clone();
        let (head, _) = self.pdents.split_at_mut(ndents);
        if self.cfg.fuzzy && !fltr.is_empty() {
            head.sort_by(|a, b| c.fuzzyentrycmp(fltr, ic, a, b));
        } else {
            head.sort_by(|a, b| c.cmp(a, b));
        }
        self.ndents
    }

    /// `handle_alt_key`
    pub fn handle_alt_key(&mut self) -> Option<i32> {
        self.cs().timeout(0);
        let r = self.cs().get_wch().map(input_value);
        self.cleartimeout();
        r
    }

    /// `filterentries`: the filter prompt; returns the key that ended it.
    pub fn filterentries(&mut self, path: &[u8]) -> i32 {
        let total = self.ndents;
        let pln_set = {
            let f = self.fltr();
            (f[0] as i32 == FILTER || f[0] as i32 == RFILTER) && f[1] != 0
        };
        let mut wln: Vec<i32>;
        let mut ch: i32 = 0;
        if self.ndents > 0 && pln_set {
            let pln = cstr(&self.fltr()[1..]).to_vec();
            if self.matches(&pln) != -1 {
                let lastname = self.g_ctx[self.cfg.curctx].c_name.clone();
                let t = self.dentfind(&lastname, self.ndents);
                self.move_cursor(t, false);
                self.redraw(path);
            }
            if !self.cfg.filtermode {
                self.statusbar(path);
                return 0;
            }
            let ln = cstr(self.fltr()).to_vec();
            wln = locale::mbstowcs(&ln, REGEX_MAX).unwrap_or_default();
        } else {
            let t = if self.cfg.regex { RFILTER } else { FILTER } as u8;
            let f = self.fltr();
            f[0] = t;
            f[1] = 0;
            wln = vec![i32::from(t)];
        }
        let set_ln = |nnn: &mut Nnn, wln: &[i32]| {
            let bytes = wcstombs_n(wln, REGEX_MAX);
            let f = nnn.fltr();
            let keep = f[REGEX_MAX - 1];
            f.iter_mut().for_each(|b| *b = 0);
            f[..bytes.len()].copy_from_slice(&bytes);
            f[REGEX_MAX - 1] = if bytes.len() < REGEX_MAX - 1 {
                keep
            } else {
                f[REGEX_MAX - 1]
            };
        };
        self.cleartimeout();
        self.cs().curs_set(true);
        let ln = self.fltr().clone();
        self.showfilter(&ln);
        'outer: while let Some(input) = self.cs().get_wch() {
            ch = input_value(input);
            let is_key = matches!(input, Input::Key(_));
            let len = wln.len();
            match ch {
                0 | curses::KEY_RESIZE => {
                    self.clearoldprompt();
                    self.redraw(path);
                    let ln = self.fltr().clone();
                    self.showfilter(&ln);
                    continue;
                }
                curses::KEY_DC | curses::KEY_BACKSPACE | 8 | DEL => {
                    if len != 1 {
                        wln.pop();
                        set_ln(self, &wln);
                        self.ndents = total;
                    } else {
                        ch = FILTER;
                        break 'outer;
                    }
                    self.cur = 0;
                    let pln = cstr(&self.fltr()[1..]).to_vec();
                    if self.matches(&pln) != -1 {
                        self.redraw(path);
                    }
                    let ln = self.fltr().clone();
                    self.showfilter(&ln);
                    continue;
                }
                c if c == control(b'L') => {
                    if wln.len() > 1 {
                        let f = self.fltr();
                        f[REGEX_MAX - 1] = f[1];
                        f[1] = 0;
                        wln.truncate(1);
                        self.ndents = total;
                    } else if self.fltr()[REGEX_MAX - 1] != 0 {
                        let f = self.fltr();
                        f[1] = f[REGEX_MAX - 1];
                        f[REGEX_MAX - 1] = 0;
                        let ln = cstr(self.fltr()).to_vec();
                        wln = locale::mbstowcs(&ln, REGEX_MAX).unwrap_or_default();
                    } else {
                        break 'outer;
                    }
                    self.cur = 0;
                    let pln = cstr(&self.fltr()[1..]).to_vec();
                    if self.matches(&pln) != -1 {
                        self.redraw(path);
                    }
                    let ln = self.fltr().clone();
                    self.showfilter(&ln);
                    continue;
                }
                curses::KEY_MOUSE => {
                    if self.cs().getmouse().bstate == 0 {
                        continue;
                    }
                    self.cs().ungetch(curses::KEY_MOUSE);
                    break 'outer;
                }
                ESC => {
                    if let Some(c) = self.handle_alt_key() {
                        if c == ESC {
                            ch = b'q' as i32;
                        } else {
                            self.cs().ungetch(c);
                            ch = b';' as i32;
                        }
                    }
                    break 'outer;
                }
                _ => {}
            }
            if is_key {
                break;
            }
            if ch < 128 && (ch < 32 || ch == 127) && ch != b'^' as i32 {
                break 'outer;
            }
            if len == 1 {
                if ch == b'?' as i32 {
                    break 'outer;
                }
                if self.cfg.filtermode && b"'+,-.;=>@]`~".contains(&(ch as u8)) && ch < 128 {
                    break 'outer;
                }
                if ch == CASE {
                    self.core.filter_ic = !self.core.filter_ic;
                    let ln = self.fltr().clone();
                    self.showfilter(&ln);
                    continue;
                }
                if ch == FILTER {
                    if self.cfg.regex {
                        self.fltr()[0] = FILTER as u8;
                        self.cfg.regex = false;
                    } else if !self.cfg.fuzzy {
                        self.fltr()[0] = FILTER as u8;
                        self.cfg.fuzzy = true;
                    } else {
                        self.fltr()[0] = RFILTER as u8;
                        self.cfg.regex = true;
                        self.cfg.fuzzy = false;
                    }
                    wln[0] = i32::from(self.fltr()[0]);
                    let ln = self.fltr().clone();
                    self.showfilter(&ln);
                    continue;
                }
                self.cur = 0;
            } else if len == REGEX_MAX - 1 {
                continue;
            }
            if ch != FILTER {
                wln.push(ch);
                set_ln(self, &wln);
            }
            let pln = cstr(&self.fltr()[1..]).to_vec();
            let r = self.matches(&pln);
            if r <= 0 {
                if r == 0 {
                    self.cs().ungetch(curses::KEY_BACKSPACE);
                }
                let a = color_pair(self.cfg.curctx as u32 + 1) | A_REVERSE | A_STANDOUT;
                self.w().attron(a);
                let ln = cstr(self.fltr()).to_vec();
                self.printmsg_nc(&ln);
                self.w().attroff(a);
                continue;
            }
            if self.cfg.autoenter {
                if self.ndents == 1
                    && self.pdents[0].flags & DIR_OR_DIRLNK != 0
                    && crate::sys::dir_access(&self.pdents[0].name)
                {
                    ch = curses::KEY_ENTER;
                    self.cur = 0;
                    break 'outer;
                } else if ch == FILTER && !pln.is_empty() {
                    let r = self.dentfind(&pln, self.ndents) as usize;
                    if (r > 0 || pln == self.pdents[r].name)
                        && self.pdents[r].flags & DIR_OR_DIRLNK != 0
                        && crate::sys::dir_access(&self.pdents[r].name)
                    {
                        ch = curses::KEY_ENTER;
                        self.cur = r as i32;
                        break 'outer;
                    }
                }
            }
            self.redraw(path);
            let ln = self.fltr().clone();
            self.showfilter(&ln);
            continue;
        }
        self.end_filter();
        ch
    }

    fn end_filter(&mut self) {
        let y = self.xlines - 2;
        let w = self.w();
        w.mv(y, 2);
        w.clrtoeol();
        let f = self.fltr();
        if f[1] != 0 {
            f[REGEX_MAX - 1] = f[1];
        }
        self.copycurname();
        self.cs().curs_set(false);
        self.settimeout();
    }

    /// `nextsel`: the action of the next key (or `presel`), `None` when unbound.
    pub fn nextsel(&mut self, presel: i32) -> Option<Action> {
        let mut c = presel;
        let mut got = true;
        let mut escaped = false;
        if c == 0 || c == MSGWAIT {
            loop {
                let input = self.cs().get_wch();
                got = input.is_some();
                c = input.map_or(c, input_value);
                if c == curses::KEY_RESIZE {
                    self.handle_key_resize();
                }
                if c == ESC && got {
                    self.cs().timeout(0);
                    let next = self.cs().get_wch();
                    if let Some(n) = next {
                        let n = input_value(n);
                        if n == ESC {
                            c = b'q' as i32;
                        } else {
                            self.cs().ungetch(n);
                            c = b';' as i32;
                        }
                        self.settimeout();
                    } else if escaped {
                        self.settimeout();
                        c = control(b'Q');
                    } else {
                        if !self.g_state.fifomode {
                            self.notify_fifo(true);
                        }
                        escaped = true;
                        self.settimeout();
                        continue;
                    }
                }
                break;
            }
            if !got && presel == MSGWAIT {
                c = if self.cfg.filtermode || self.filterset() {
                    FILTER
                } else {
                    control(b'L')
                };
            } else if c == FILTER || c == control(b'L') {
                self.clearfilter();
            }
        }
        if !got {
            self.idle += 1;
            if !self.cfg.sort.blkorder
                && (self.idle & 1) != 0
                && let Some(k) = self.poll_dir_watch()
            {
                c = k;
            }
        } else {
            self.idle = 0;
        }
        BINDINGS
            .iter()
            .find(|(k, _)| k.code() as i32 == c)
            .map(|&(_, a)| a)
    }

    /// `savecurctx`
    pub fn savecurctx(&mut self, path: &[u8], curname: Option<&[u8]>, nextctx: usize) {
        let mut tmpcfg = self.cfg.clone();
        if self.g_state.closedctx == nextctx {
            self.g_state.closedctx = CTX_INVALID;
        }
        self.g_state.lastctx = self.cfg.curctx;
        let cc = tmpcfg.curctx;
        self.g_ctx[cc].c_name = curname
            .map(|n| strsncpy(n, NAME_MAX + 1))
            .unwrap_or_default();
        self.g_ctx[cc].c_cfg = tmpcfg.clone();
        if self.g_ctx[nextctx].c_cfg.ctxactive {
            tmpcfg = self.g_ctx[nextctx].c_cfg.clone();
            if !self.order.is_empty() {
                self.cfgsort[CTX_MAX] = self.cfgsort[nextctx];
                self.cfgsort[nextctx] = b'0';
            }
        } else {
            let ctxr = &mut self.g_ctx[nextctx];
            ctxr.c_cfg.ctxactive = true;
            ctxr.c_path = strsncpy(path, PATH_MAX);
            ctxr.c_last.clear();
            ctxr.c_name.clear();
            ctxr.c_fltr.resize(REGEX_MAX, 0);
            ctxr.c_fltr[0] = 0;
            ctxr.c_fltr[1] = 0;
            ctxr.c_cfg = tmpcfg.clone();
            if self.cfgsort[self.cfg.curctx] == b'z' {
                self.cfgsort[nextctx] = b'z';
            }
        }
        tmpcfg.curctx = nextctx;
        self.setcfg(tmpcfg);
    }

    /// `get_free_ctx`
    pub fn get_free_ctx(&self) -> usize {
        let mut r = self.cfg.curctx;
        loop {
            r = (r + 1) & !CTX_MAX;
            if !(self.g_ctx[r].c_cfg.ctxactive && r != self.cfg.curctx) {
                break;
            }
        }
        r
    }

    /// `set_smart_ctx`: `ctx` 1..8, `'+'` for a free one, 0 for the current one.
    pub fn set_smart_ctx(&mut self, mut ctx: i32, nextpath: &[u8], file: Option<&[u8]>) {
        if ctx == b'+' as i32 {
            ctx = self.get_free_ctx() as i32 + 1;
        }
        if ctx == 0 || ctx == self.cfg.curctx as i32 + 1 {
            self.clearfilter();
            let cc = self.cfg.curctx;
            self.g_ctx[cc].c_last = strsncpy(&self.g_ctx[cc].c_path, PATH_MAX);
            self.g_ctx[cc].c_path = strsncpy(nextpath, PATH_MAX);
        } else {
            let ctx = (ctx - 1) as usize;
            self.g_ctx[ctx].c_cfg.ctxactive = false;
            self.savecurctx(nextpath, file, ctx);
        }
    }

    /// `handle_context_switch`: the context to switch to, or -1.
    pub fn handle_context_switch(&self, sel: Action) -> i32 {
        let cur = self.cfg.curctx as i32;
        let mut r: i32 = -1;
        let ctxn = |a: Action| -> Option<i32> {
            [
                Action::Ctx1,
                Action::Ctx2,
                Action::Ctx3,
                Action::Ctx4,
                Action::Ctx5,
                Action::Ctx6,
                Action::Ctx7,
                Action::Ctx8,
            ]
            .iter()
            .position(|&x| x == a)
            .map(|p| p as i32)
        };
        let max = CTX_MAX as i32;
        match sel {
            Action::Cycle => {
                r = cur;
                loop {
                    r = (r + 1) & !max;
                    if self.g_ctx[r as usize].c_cfg.ctxactive {
                        break;
                    }
                }
            }
            Action::Cycler => {
                r = cur;
                loop {
                    r = (r + 1) & !max;
                    if !(self.g_ctx[r as usize].c_cfg.ctxactive && r != cur) {
                        break;
                    }
                }
                if r == cur {
                    loop {
                        r = (r + (max - 1)) & (max - 1);
                        if self.g_ctx[r as usize].c_cfg.ctxactive {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(n) = ctxn(sel) {
            r = n;
        }
        if cur == r {
            if sel == Action::Cycle {
                r = if r == max - 1 { 0 } else { r + 1 };
            } else if sel == Action::Cycler {
                r = if r == 0 { max - 1 } else { r - 1 };
            } else {
                return -1;
            }
        }
        r
    }

    /// `visit_parent`: chdir to the parent of `path`; the parent, or `None`.
    pub fn visit_parent(&mut self, path: &[u8], presel: Option<&mut i32>) -> Option<Vec<u8>> {
        if istopdir(path) {
            if self.cfg.filtermode
                && let Some(p) = presel
            {
                *p = FILTER;
            }
            return None;
        }
        let dir = crate::config::xdirname(path);
        if std::env::set_current_dir(&*crate::platform::os(&dir)).is_err() {
            self.printwarn(presel);
            return None;
        }
        Some(dir)
    }

    /// `valid_parent`: walk up `c_path` to a directory that can be entered.
    pub fn valid_parent(&mut self) {
        let cc = self.cfg.curctx;
        let path = self.g_ctx[cc].c_path.clone();
        self.g_ctx[cc].c_name = strsncpy(crate::config::xbasename(&path), NAME_MAX + 1);
        let mut p = path;
        while !istopdir(&p) {
            if let Some(d) = self.visit_parent(&p, None) {
                p = d;
                break;
            }
            p = crate::config::xdirname(&p);
        }
        self.g_ctx[cc].c_path = p;
        self.printwarn(None);
        self.xdelay(XDELAY_INTERVAL_MS);
    }

    /// `cdprep`: the current context moves to `newpath`; returns `cfg.filtermode`.
    pub fn cdprep(&mut self, clear_lastname: bool, newpath: &[u8]) -> bool {
        let cc = self.cfg.curctx;
        if clear_lastname {
            self.g_ctx[cc].c_name.clear();
        }
        self.g_ctx[cc].c_last = strsncpy(&self.g_ctx[cc].c_path, PATH_MAX);
        self.g_ctx[cc].c_path = strsncpy(newpath, PATH_MAX);
        self.clearfilter();
        self.cfg.filtermode
    }
}

/// The value of a key as nnn.c sees it (`wint_t` from `get_wch`).
pub fn input_value(i: Input) -> i32 {
    match i {
        Input::Key(k) | Input::Char(k) => k,
    }
}

/// `wcstombs(buf, wln, n)`: whole characters only, at most `n - 1` bytes plus the NUL.
pub fn wcstombs_n(w: &[i32], n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for &c in w {
        let Some(b) = locale::wcstombs(&[c]) else {
            break;
        };
        if out.len() + b.len() >= n {
            break;
        }
        out.extend_from_slice(&b);
    }
    out
}
