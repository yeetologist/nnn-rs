//! The native prompt (xreadline), messages and command history (src/nnn.c, NORL build:
//! ADR-013).

use crate::curses::{ERR, Input, KEY_BACKSPACE, KEY_DC, KEY_DOWN, KEY_END, KEY_ENTER};
use crate::curses::{KEY_HOME, KEY_LEFT, KEY_RESIZE, KEY_RIGHT, KEY_UP, Window};
use crate::locale;
use crate::state::{NAME_MAX, Nnn, PATH_MAX};
use crate::sys;
use std::io::{self, BufRead, Write};

/// `MAX_HISTORY`: commands kept in the prompt history.
pub const MAX_HISTORY: usize = 0x10;
/// `INVALID_POS`: no history entry.
pub const INVALID_POS: u8 = 0xFF;
/// `READLINE_MAX`: wide characters the prompt holds.
pub const READLINE_MAX: usize = 256;
/// `CMD_LEN_MAX`
pub const CMD_LEN_MAX: usize = PATH_MAX + ((NAME_MAX + 1) << 1);
/// `PROMPT`: the command prompt, the only one that recalls history.
pub const PROMPT: &[u8] = b">>> ";
/// `MSGWAIT`: the `presel` value that keeps a message on screen.
pub const MSGWAIT: i32 = b'$' as i32;
/// `XDELAY_INTERVAL_MS` (microseconds, despite the name).
pub const XDELAY_INTERVAL_MS: u64 = 350_000;
/// `ESC`
pub const ESC: i32 = 27;
/// `DEL`
pub const DEL: i32 = 127;
/// `ASCII_MAX`
const ASCII_MAX: i32 = 128;

/// `CONTROL(c)`
pub const fn control(c: u8) -> i32 {
    (c & 0x1f) as i32
}

/// `ISSPACE(x)`
fn isspace(c: i32) -> bool {
    c == i32::from(b' ')
        || c == i32::from(b'\t')
        || c == i32::from(b'\n')
        || c == i32::from(b'\r')
        || c == 0x0c
        || c == 0x0b
}

/// `iswalnum` (glibc, current locale).
fn iswalnum(c: i32) -> bool {
    locale::is_walnum(c)
}

/// `keyname(c)[0] == '^'` for `c < ASCII_MAX`: ncurses names 0..=31 and 127 `^X`.
fn is_ctrl_name(c: i32) -> bool {
    (0..0x20).contains(&c) || c == DEL
}

/// The C string a buffer holds: up to its first NUL.
fn cstr(s: &[u8]) -> &[u8] {
    &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
}

/// Ports `shell_escape` with a caller buffer of `buflen` bytes (the only way xreadline calls
/// it): `None` where the original fails with `EINVAL`/`ENAMETOOLONG`.
fn shell_escape(inbuf: &[u8], buflen: usize) -> Option<Vec<u8>> {
    let inbuf = cstr(inbuf);
    if buflen < 3 {
        return None;
    }
    let n = inbuf.len();
    let mut buf = Vec::with_capacity(n + 2);
    for r in 0..n + 2 {
        if buf.len() + 5 >= buflen {
            return None;
        }
        if r == 0 || r == n + 1 {
            buf.push(b'\'');
        } else if inbuf[r - 1] == b'\'' {
            buf.extend_from_slice(b"'\\''");
        } else {
            buf.push(inbuf[r - 1]);
        }
    }
    Some(buf)
}

/// `clrtobot`: clear from the cursor to the end of the window; the cursor stays.
fn clrtobot(w: &mut Window) {
    let (cy, cx) = (w.cy, w.cx);
    w.clrtoeol();
    for y in cy + 1..w.lines {
        w.cx = 0;
        w.cy = y;
        w.clrtoeol();
    }
    w.cy = cy;
    w.cx = cx;
}

/// The prompt's state: the command history (`cmd_hist`, `selcmdpos`, `lastcmdpos`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptState {
    /// `cmd_hist[0..=lastcmdpos]`, oldest first.
    pub cmd_hist: Vec<Vec<u8>>,
    /// `selcmdpos`: the entry recalled last, or `INVALID_POS` (callers reset it before a prompt).
    pub selcmdpos: u8,
}

impl Default for PromptState {
    fn default() -> Self {
        PromptState {
            cmd_hist: Vec::with_capacity(MAX_HISTORY),
            selcmdpos: INVALID_POS,
        }
    }
}

impl PromptState {
    /// `lastcmdpos`: the newest entry, or `INVALID_POS` when the history is empty.
    pub fn lastcmdpos(&self) -> u8 {
        if self.cmd_hist.is_empty() {
            INVALID_POS
        } else {
            // INFALLIBLE: the history never holds more than MAX_HISTORY (16) entries.
            (self.cmd_hist.len() - 1) as u8
        }
    }

    /// Ports `getcmdfromhist`: the previous (`up`) or next command, wrapping around.
    pub fn getcmdfromhist(&mut self, up: bool) -> Option<Vec<u8>> {
        let last = self.lastcmdpos();
        if last == INVALID_POS {
            return None;
        }
        if self.selcmdpos == INVALID_POS {
            self.selcmdpos = if up { last } else { 0 };
        } else if up {
            self.selcmdpos = if self.selcmdpos == 0 {
                last
            } else {
                self.selcmdpos - 1
            };
        } else {
            self.selcmdpos = if self.selcmdpos == last {
                0
            } else {
                self.selcmdpos + 1
            };
        }
        self.cmd_hist.get(usize::from(self.selcmdpos)).cloned()
    }

    /// Ports `addcmdtohist`: a new command goes last (the oldest drops out when full); a
    /// recalled command that was run unchanged moves to the end.
    pub fn addcmdtohist(&mut self, cmd: &[u8]) {
        let cmd = cstr(cmd);
        let last = self.lastcmdpos();
        let sel = usize::from(self.selcmdpos);
        let new = if last == INVALID_POS {
            true
        } else if self.selcmdpos == INVALID_POS
            || self.cmd_hist.get(sel).is_none_or(|c| c.as_slice() != cmd)
        {
            if usize::from(last) == MAX_HISTORY - 1 {
                self.cmd_hist.remove(0);
            }
            true
        } else {
            if self.selcmdpos != last {
                let c = self.cmd_hist.remove(sel);
                self.cmd_hist.push(c);
            }
            false
        };
        if new {
            self.cmd_hist.push(cmd.to_vec());
        }
    }

    /// Ports `xread_history`: up to `MAX_HISTORY` lines of the file, each added as a
    /// command; a file that cannot be opened is ignored.
    pub fn xread_history(&mut self, histpath: &[u8]) {
        let Ok(f) = std::fs::File::open(&*crate::platform::os(histpath)) else {
            return;
        };
        let mut r = io::BufReader::new(f);
        let mut line = Vec::new();
        for _ in 0..MAX_HISTORY {
            line.clear();
            match r.read_until(b'\n', &mut line) {
                Ok(n) if n > 0 => {}
                _ => break,
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            self.addcmdtohist(&line);
        }
    }

    /// Ports `xwrite_history`: one command per line; nothing when the history is empty.
    pub fn xwrite_history(&self, histpath: &[u8]) {
        if self.cmd_hist.is_empty() {
            return;
        }
        match std::fs::File::create(&*crate::platform::os(histpath)) {
            Ok(f) => {
                let mut w = io::BufWriter::new(f);
                for c in &self.cmd_hist {
                    let _ = w.write_all(c);
                    let _ = w.write_all(b"\n");
                }
                let _ = w.flush();
            }
            Err(e) => sys::xerror(4271, &e),
        }
    }
}

/// Ports `xconfirm`.
pub fn xconfirm(c: i32) -> bool {
    c == i32::from(b'y') || c == i32::from(b'Y')
}

impl Nnn {
    /// Ports `handle_key_resize`: `endwin(); refresh();`.
    pub fn handle_key_resize(&mut self) {
        let cs = self.cs();
        cs.endwin();
        cs.resume();
    }

    /// Ports `clearoldprompt`: clear the info line to the bottom of the screen.
    pub fn clearoldprompt(&mut self) {
        let y = self.xlines - 2;
        let w = &mut self.cs().stdscr;
        w.mv(y, 0);
        clrtobot(w);
        self.handle_key_resize();
    }

    /// Ports `printmsg_nc`: a message on the last line, without the context color.
    pub fn printmsg_nc(&mut self, msg: &[u8]) {
        self.tolastln();
        let w = &mut self.cs().stdscr;
        w.addstr(msg);
        w.clrtoeol();
    }

    /// Ports `printmsg`.
    pub fn printmsg(&mut self, msg: &[u8]) {
        self.attron_curctx();
        self.printmsg_nc(msg);
        self.attroff_curctx();
    }

    /// Ports `printwait`: show `msg`; with `presel`, keep it up (`MSGWAIT`) and remember the
    /// hovered name so the cursor returns to it.
    pub fn printwait(&mut self, msg: &[u8], presel: Option<&mut i32>) {
        self.printmsg(msg);
        if let Some(p) = presel {
            *p = MSGWAIT;
            if self.ndents != 0 {
                let name = self
                    .pdents
                    .get(self.cur as usize)
                    .map(|e| cstr(&e.name))
                    .unwrap_or_default();
                let name = name[..name.len().min(NAME_MAX)].to_vec();
                self.g_ctx[self.cfg.curctx].c_name = name;
            }
        }
    }

    /// Ports `printwarn`: `printwait(strerror(errno), presel)`.
    pub fn printwarn(&mut self, presel: Option<&mut i32>) {
        let s = sys::strerror(&io::Error::last_os_error());
        self.printwait(s.as_bytes(), presel);
    }

    /// Ports `printerr` (`errexit()` passes its own line): leave curses, print
    /// `<linenum>: <strerror(errno)>`, drop the selection file unless picking, exit 1.
    pub fn printerr(&mut self, linenum: u32) -> ! {
        let err = io::Error::last_os_error();
        if let Some(c) = self.curses.as_mut() {
            c.endwin();
        }
        sys::xerror(linenum, &err);
        if !self.g_state.picker
            && let Some(p) = &self.selpath
        {
            sys::unlink(p);
        }
        std::process::exit(1);
    }

    /// Ports `get_input`: show `prompt` (if any) and wait for one key, redrawing the prompt
    /// on resize. A read error (`ERR`, where the original keeps an unset value) gives `ERR`.
    pub fn get_input(&mut self, prompt: Option<&[u8]>) -> i32 {
        if let Some(p) = prompt {
            self.printmsg(p);
        }
        self.cleartimeout();
        let key = |i: Option<Input>| match i {
            Some(Input::Key(k) | Input::Char(k)) => k,
            None => ERR,
        };
        let mut ch = key(self.cs().get_wch());
        while ch == KEY_RESIZE {
            if let Some(p) = prompt {
                self.clearoldprompt();
                self.xlines = self.cs().lines();
                self.printmsg(p);
            }
            ch = key(self.cs().get_wch());
        }
        self.settimeout();
        ch
    }

    /// Ports `xdelay`: show the screen, then wait `delay` microseconds.
    pub fn xdelay(&mut self, delay: u64) {
        self.cs().refresh();
        std::thread::sleep(std::time::Duration::from_micros(delay));
    }

    /// Ports `xreadline` (the NORL prompt; the default build has no `getreadline`): edit a
    /// line after `prompt`, starting from `prefill`. Returns the text in the locale's bytes
    /// (at most `READLINE_MAX - 1`), empty on Esc or ^D at an empty prompt.
    pub fn xreadline(&mut self, prefill: Option<&[u8]>, prompt: &[u8]) -> Vec<u8> {
        // One element more than the original's READLINE_MAX + 1 so buf[len + 1] always exists;
        // grown where the original would write past its buffer.
        let mut buf: Vec<i32> = vec![0; READLINE_MAX + 2];
        let mut len: usize;
        let mut pos: usize;
        let mut lpos: usize;
        let mut tab_cycle: i32 = -1;
        let mut tab_basepos: usize = 0;

        self.cleartimeout();
        self.printmsg(prompt);

        match prefill.and_then(|p| locale::mbstowcs(p, READLINE_MAX)) {
            Some(w) => {
                buf[..w.len()].copy_from_slice(&w);
                len = w.len();
            }
            None => {
                buf[0] = 0;
                len = 0;
            }
        }
        pos = len;

        let x = self.cs().stdscr.cx;
        self.cs().curs_set(true);

        loop {
            if buf.len() < len + 2 {
                buf.resize(len + 2, 0);
            }
            buf[len] = i32::from(b' ');
            buf[len + 1] = i32::from(b' '); // Handle full-width characters

            let (y, xcols) = (self.xlines - 1, self.xcols);
            self.attron_curctx_after_clear(y, x);
            {
                let w = &mut self.cs().stdscr;
                let room = xcols - x;
                if room >= 0 && pos > room as usize {
                    let start = pos - room as usize + 1;
                    if w.mv(y, x) != ERR {
                        w.addnwstr(&buf[start..], room + 1);
                    }
                    w.mv(y, xcols - 1);
                } else {
                    if w.mv(y, x) != ERR {
                        w.addnwstr(&buf[..len + 2], (len + 2) as i32);
                    }
                    w.mv(y, x + locale::wcswidth_n(&buf[..pos], pos));
                }
            }
            self.attroff_curctx();

            let Some(input) = self.cs().get_wch() else {
                continue; // ERR
            };
            let (ok, mut ch) = match input {
                Input::Char(c) => (true, c),
                Input::Key(k) => (false, k),
            };

            if !(ok && ch == i32::from(b'\t')) {
                tab_cycle = -1;
            }

            if ok {
                match ch {
                    KEY_ENTER | 0x0a | 0x0d => break,
                    c if c == control(b'D') => {
                        if pos < len {
                            pos += 1;
                            // fallthrough to DEL
                            buf.copy_within(pos..len, pos - 1);
                            len -= 1;
                            pos -= 1;
                        } else if pos == 0 && len == 0 {
                            // Exit on ^D at empty prompt
                            len = 0;
                            break;
                        }
                        continue;
                    }
                    DEL | 0x08 => {
                        // rhel25 sends '\b' for backspace
                        if pos > 0 {
                            buf.copy_within(pos..len, pos - 1);
                            len -= 1;
                            pos -= 1;
                        }
                        continue;
                    }
                    0x09 => {
                        if self.ndents != 0 {
                            let idx;
                            if tab_cycle < 0 {
                                // First Tab: start cycling from cur
                                tab_basepos = pos;
                                // Only allow appending at end
                                if len != pos {
                                    continue;
                                }
                                idx = self.cur;
                            } else {
                                // Subsequent Tab: remove previous insertion, advance
                                len = tab_basepos;
                                pos = tab_basepos;
                                idx = (tab_cycle + 1) % self.ndents;
                            }
                            let name = self
                                .pdents
                                .get(idx as usize)
                                .map(|e| e.name.clone())
                                .unwrap_or_default();
                            if let Some(esc) = shell_escape(&name, CMD_LEN_MAX) {
                                let elen = esc.len();
                                // size_t arithmetic, as in the original
                                if elen > 0 && tab_basepos < READLINE_MAX.wrapping_sub(elen) {
                                    buf[tab_basepos] = 0;
                                    let w = locale::mbstowcs(&esc, esc.len()).unwrap_or_default();
                                    if buf.len() < tab_basepos + w.len() + 2 {
                                        buf.resize(tab_basepos + w.len() + 2, 0);
                                    }
                                    buf[tab_basepos..tab_basepos + w.len()].copy_from_slice(&w);
                                    pos = tab_basepos + w.len();
                                    len = pos;
                                    tab_cycle = idx;
                                }
                            }
                        }
                        continue;
                    }
                    c if c == control(b'F') => {
                        if pos < len {
                            pos += 1;
                        }
                        continue;
                    }
                    c if c == control(b'B') => {
                        pos = pos.saturating_sub(1);
                        continue;
                    }
                    c if c == control(b'W') => {
                        self.printmsg(prompt);
                        lpos = pos;
                        while pos > 0 && isspace(buf[pos - 1]) {
                            pos -= 1;
                        }
                        while pos > 0 && !isspace(buf[pos - 1]) {
                            pos -= 1;
                        }
                        buf.copy_within(lpos..len, pos);
                        len -= lpos - pos;
                        continue;
                    }
                    c if c == control(b'K') => {
                        self.printmsg(prompt);
                        len = pos;
                        continue;
                    }
                    c if c == control(b'L') => {
                        self.printmsg(prompt);
                        len = 0;
                        pos = 0;
                        continue;
                    }
                    c if c == control(b'A') => {
                        pos = 0;
                        continue;
                    }
                    c if c == control(b'E') => {
                        pos = len;
                        continue;
                    }
                    c if c == control(b'U') => {
                        self.printmsg(prompt);
                        buf.copy_within(pos..len, 0);
                        len -= pos;
                        pos = 0;
                        continue;
                    }
                    ESC => {
                        // Exit prompt on Esc, but just filter out Alt+key
                        if let Some(k) = self.handle_alt_key() {
                            ch = k;
                            match ch {
                                0x64 => {
                                    // 'd'
                                    self.printmsg(prompt);
                                    lpos = pos;
                                    while pos < len && !iswalnum(buf[pos + 1]) {
                                        pos += 1;
                                    }
                                    while pos < len {
                                        pos += 1;
                                        if !iswalnum(buf[pos]) {
                                            break;
                                        }
                                    }
                                    buf.copy_within(pos..len, lpos);
                                    len -= pos - lpos;
                                    pos = lpos;
                                }
                                KEY_BACKSPACE => {
                                    self.printmsg(prompt);
                                    lpos = pos;
                                    while pos > 0 && !iswalnum(buf[pos - 1]) {
                                        pos -= 1;
                                    }
                                    while pos > 0 && iswalnum(buf[pos - 1]) {
                                        pos -= 1;
                                    }
                                    buf.copy_within(lpos..len, pos);
                                    len -= lpos - pos;
                                }
                                0x66 => {
                                    // 'f'
                                    while pos < len && !iswalnum(buf[pos + 1]) {
                                        pos += 1;
                                    }
                                    while pos < len {
                                        pos += 1;
                                        if !iswalnum(buf[pos]) {
                                            break;
                                        }
                                    }
                                }
                                0x62 => {
                                    // 'b'
                                    while pos > 0 && !iswalnum(buf[pos - 1]) {
                                        pos -= 1;
                                    }
                                    while pos > 0 && iswalnum(buf[pos - 1]) {
                                        pos -= 1;
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }
                        len = 0;
                        break;
                    }
                    _ => {}
                }

                // Filter out all other control chars
                if ch < ASCII_MAX && is_ctrl_name(ch) {
                    continue;
                }

                if pos < READLINE_MAX - 1 {
                    if buf.len() < len + 3 {
                        buf.resize(len + 3, 0);
                    }
                    buf.copy_within(pos..len, pos + 1);
                    buf[pos] = ch;
                    len += 1;
                    pos += 1;
                }
            } else {
                match ch {
                    KEY_RESIZE => {
                        self.clearoldprompt();
                        self.xlines = self.cs().lines();
                        self.printmsg(prompt);
                    }
                    KEY_LEFT => pos = pos.saturating_sub(1),
                    KEY_RIGHT => {
                        if pos < len {
                            pos += 1;
                        }
                    }
                    KEY_BACKSPACE => {
                        if pos > 0 {
                            buf.copy_within(pos..len, pos - 1);
                            len -= 1;
                            pos -= 1;
                        }
                    }
                    KEY_DC => {
                        if pos < len {
                            buf.copy_within(pos + 1..len, pos);
                            len -= 1;
                        }
                    }
                    KEY_END => pos = len,
                    KEY_HOME => pos = 0,
                    KEY_UP | KEY_DOWN => {
                        let cmd = self.prompt.getcmdfromhist(ch == KEY_UP);
                        if let Some(cmd) = cmd
                            && prompt == PROMPT
                        {
                            self.printmsg(prompt);
                            // The original's (size_t)-1 here is undefined; take it as empty.
                            let w = locale::mbstowcs(&cmd, READLINE_MAX).unwrap_or_default();
                            buf[..w.len()].copy_from_slice(&w);
                            len = w.len();
                            pos = len;
                        }
                    }
                    _ => {}
                }
            }
        }

        self.cs().curs_set(false);
        self.settimeout();
        self.printmsg(b"");

        wide_to_gbuf(&buf[..len])
    }

    /// `move(y, x); clrtoeol(); attron_curctx();` at the top of xreadline's loop.
    fn attron_curctx_after_clear(&mut self, y: i32, x: i32) {
        let w = &mut self.cs().stdscr;
        w.mv(y, x);
        w.clrtoeol();
        self.attron_curctx();
    }
}

/// The end of `xreadline`: `wcstombs(g_buf, buf, READLINE_MAX - 1)`, cut to the
/// characters that fit whole in `READLINE_MAX - 1` bytes. A character the locale cannot
/// encode ends the text (the original keeps stale `g_buf` bytes after it).
pub fn wide_to_gbuf(wide: &[i32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &c in wide {
        let Some(b) = locale::wcstombs(&[c]) else {
            break;
        };
        if out.len() + b.len() > READLINE_MAX - 1 {
            break;
        }
        out.extend_from_slice(&b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nnn-prompt-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    fn hist(cmds: &[&str]) -> PromptState {
        let mut p = PromptState::default();
        for c in cmds {
            p.addcmdtohist(c.as_bytes());
        }
        p
    }

    fn strs(p: &PromptState) -> Vec<String> {
        p.cmd_hist
            .iter()
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect()
    }

    #[test]
    fn empty_history() {
        let mut p = PromptState::default();
        assert_eq!(p.lastcmdpos(), INVALID_POS);
        assert_eq!(p.getcmdfromhist(true), None);
        assert_eq!(p.selcmdpos, INVALID_POS);
    }

    #[test]
    fn recall_wraps_both_ways() {
        let mut p = hist(&["a", "b", "c"]);
        assert_eq!(p.getcmdfromhist(true).as_deref(), Some(&b"c"[..]));
        assert_eq!(p.getcmdfromhist(true).as_deref(), Some(&b"b"[..]));
        assert_eq!(p.getcmdfromhist(true).as_deref(), Some(&b"a"[..]));
        assert_eq!(p.getcmdfromhist(true).as_deref(), Some(&b"c"[..]));
        assert_eq!(p.getcmdfromhist(false).as_deref(), Some(&b"a"[..]));
        p.selcmdpos = INVALID_POS;
        assert_eq!(p.getcmdfromhist(false).as_deref(), Some(&b"a"[..]));
    }

    #[test]
    fn full_history_drops_oldest() {
        let names: Vec<String> = (0..20).map(|i| format!("c{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let p = hist(&refs);
        assert_eq!(p.cmd_hist.len(), MAX_HISTORY);
        assert_eq!(p.lastcmdpos(), 15);
        assert_eq!(strs(&p)[0], "c4");
        assert_eq!(strs(&p)[15], "c19");
    }

    #[test]
    fn recalled_command_moves_last_without_duplicate() {
        let mut p = hist(&["a", "b", "c"]);
        p.selcmdpos = 0;
        p.addcmdtohist(b"a");
        assert_eq!(strs(&p), ["b", "c", "a"]);
        // A recalled but edited command is new.
        p.selcmdpos = 0;
        p.addcmdtohist(b"bx");
        assert_eq!(strs(&p), ["b", "c", "a", "bx"]);
        // The newest recalled unchanged stays put.
        p.selcmdpos = 3;
        p.addcmdtohist(b"bx");
        assert_eq!(strs(&p), ["b", "c", "a", "bx"]);
        // Without a recall, a repeat is added again.
        p.selcmdpos = INVALID_POS;
        p.addcmdtohist(b"bx");
        assert_eq!(strs(&p), ["b", "c", "a", "bx", "bx"]);
    }

    #[test]
    fn history_file_round_trip() {
        let f = tmp("rt");
        let path = crate::platform::bytes(f.as_os_str()).to_vec();
        let p = hist(&["ls -l", "", "echo 'x y'", "caf\u{e9}"]);
        p.xwrite_history(&path);
        assert_eq!(
            std::fs::read(&f).unwrap_or_default(),
            b"ls -l\n\necho 'x y'\ncaf\xc3\xa9\n"
        );
        let mut q = PromptState::default();
        q.xread_history(&path);
        assert_eq!(q, p);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn read_history_takes_16_lines_and_last_without_newline() {
        let f = tmp("16");
        let path = crate::platform::bytes(f.as_os_str()).to_vec();
        let mut body = Vec::new();
        for i in 0..20 {
            body.extend_from_slice(format!("l{i}\n").as_bytes());
        }
        let _ = std::fs::write(&f, &body);
        let mut p = PromptState::default();
        p.xread_history(&path);
        assert_eq!(p.cmd_hist.len(), 16);
        assert_eq!(strs(&p)[15], "l15");

        let _ = std::fs::write(&f, b"one\r\ntwo\0junk\nthree");
        let mut p = PromptState::default();
        p.xread_history(&path);
        assert_eq!(strs(&p), ["one\r", "two", "three"]);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn missing_history_file_is_ignored_and_empty_not_written() {
        let f = tmp("none");
        let path = crate::platform::bytes(f.as_os_str()).to_vec();
        let mut p = PromptState::default();
        p.xread_history(&path);
        assert!(p.cmd_hist.is_empty());
        p.xwrite_history(&path);
        assert!(!f.exists());
    }

    #[test]
    fn confirm_and_keys() {
        assert!(xconfirm(i32::from(b'y')) && xconfirm(i32::from(b'Y')));
        assert!(!xconfirm(i32::from(b'n')) && !xconfirm(ESC));
        assert_eq!(control(b'D'), 4);
        assert_eq!(control(b'L'), 12);
        assert!(is_ctrl_name(0) && is_ctrl_name(31) && is_ctrl_name(DEL));
        assert!(!is_ctrl_name(32) && !is_ctrl_name(i32::from(b'a')));
    }

    #[test]
    fn shell_escape_quotes() {
        assert_eq!(
            shell_escape(b"a b", CMD_LEN_MAX).as_deref(),
            Some(&b"'a b'"[..])
        );
        assert_eq!(
            shell_escape(b"it's", CMD_LEN_MAX).as_deref(),
            Some(&b"'it'\\''s'"[..])
        );
        assert_eq!(shell_escape(b"", CMD_LEN_MAX).as_deref(), Some(&b"''"[..]));
        assert_eq!(shell_escape(b"abc", 2), None);
        assert_eq!(shell_escape(b"abcdef", 8), None);
    }

    #[test]
    fn gbuf_is_cut_to_whole_chars() {
        let ascii: Vec<i32> = (0..300).map(|_| i32::from(b'a')).collect();
        assert_eq!(wide_to_gbuf(&ascii).len(), READLINE_MAX - 1);
        assert_eq!(wide_to_gbuf(&[0x61, 0x62]), b"ab");
    }

    #[test]
    fn clrtobot_clears_to_the_end() {
        let mut w = Window::new(3, 4, 0, 0);
        for y in 0..3 {
            w.mv(y, 0);
            w.addstr(b"abcd");
        }
        w.mv(1, 2);
        clrtobot(&mut w);
        assert_eq!((w.cy, w.cx), (1, 2));
        assert!(w.addch('x') == 0);
    }
}
