//! The part of ncurses nnn uses (ADR-017): a virtual screen of cells, windows, attributes,
//! diff-based refresh with ANSI sequences, ncurses' terminal modes (cbreak, noecho, nonl)
//! and keypad decoding of raw input into curses key codes.

use crate::locale;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

// Key codes (ncurses values).
pub const KEY_DOWN: i32 = 0o402;
pub const KEY_UP: i32 = 0o403;
pub const KEY_LEFT: i32 = 0o404;
pub const KEY_RIGHT: i32 = 0o405;
pub const KEY_HOME: i32 = 0o406;
pub const KEY_BACKSPACE: i32 = 0o407;
pub const KEY_F0: i32 = 0o410;
pub const KEY_DC: i32 = 0o512;
pub const KEY_IC: i32 = 0o513;
pub const KEY_NPAGE: i32 = 0o522;
pub const KEY_PPAGE: i32 = 0o523;
pub const KEY_ENTER: i32 = 0o527;
pub const KEY_BTAB: i32 = 0o541;
pub const KEY_END: i32 = 0o550;
pub const KEY_RESIZE: i32 = 0o632;
pub const KEY_MOUSE: i32 = 0o631;
pub const ERR: i32 = -1;

// Attributes (ncurses bit layout, so callers can combine them as the C code does).
pub const A_NORMAL: u32 = 0;
pub const A_STANDOUT: u32 = 1 << 16;
pub const A_UNDERLINE: u32 = 1 << 17;
pub const A_REVERSE: u32 = 1 << 18;
pub const A_BLINK: u32 = 1 << 19;
pub const A_DIM: u32 = 1 << 20;
pub const A_BOLD: u32 = 1 << 21;
pub const A_ITALIC: u32 = 1 << 31;
pub const A_COLOR: u32 = 0xff << 8;

pub const fn color_pair(n: u32) -> u32 {
    (n << 8) & A_COLOR
}

/// Line-drawing characters (`ACS_*`), as a UTF-8 terminal shows them.
pub mod acs {
    pub const HLINE: char = '─';
    pub const VLINE: char = '│';
    pub const ULCORNER: char = '┌';
    pub const URCORNER: char = '┐';
    pub const LLCORNER: char = '└';
    pub const LRCORNER: char = '┘';
}

/// A mouse event (`MEVENT`) with ncurses button bits.
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub bstate: u32,
}

pub const BUTTON1_PRESSED: u32 = 0o2;
pub const BUTTON2_PRESSED: u32 = 0o100;
pub const BUTTON3_PRESSED: u32 = 0o4000;
pub const BUTTON4_PRESSED: u32 = 0o200000;
pub const BUTTON5_PRESSED: u32 = 0o10000000000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    /// The character, or empty for the right half of a wide one.
    ch: String,
    attr: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: " ".to_string(),
            attr: 0,
        }
    }
}

/// A window (`WINDOW`): a rectangle of the screen with its own cursor and attributes.
#[derive(Debug, Clone)]
pub struct Window {
    pub y0: i32,
    pub x0: i32,
    pub lines: i32,
    pub cols: i32,
    pub cy: i32,
    pub cx: i32,
    pub attr: u32,
    cells: Vec<Cell>,
    touched: bool,
}

/// The color of a pair: (foreground, background), -1 for the terminal default.
type Pair = (i16, i16);

/// The screen (`stdscr`, `newterm`/`initscr` state, the terminal).
pub struct Curses {
    pub stdscr: Window,
    /// Windows drawn over stdscr by `wrefresh` (popups), in order.
    overlays: Vec<Window>,
    front: Vec<Cell>,
    out: Box<dyn Write>,
    tty_in: std::fs::File,
    saved_termios: Option<rustix::termios::Termios>,
    pending: Vec<u8>,
    ungot: Vec<i32>,
    timeout_ms: i32,
    escdelay_ms: u64,
    pub pairs: [Pair; 256],
    pub colors_enabled: bool,
    cursor_visible: bool,
    mouse: bool,
    last_mouse: MouseEvent,
    pub resized: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ended: bool,
}

/// Display width of one wide character (`wcwidth`), 1 for unprintables as ncurses shows them.
pub fn wcwidth(c: i32) -> i32 {
    locale::wcswidth_n(&[c], 1)
}

impl Window {
    pub fn new(lines: i32, cols: i32, y0: i32, x0: i32) -> Window {
        let n = (lines.max(0) * cols.max(0)) as usize;
        Window {
            y0,
            x0,
            lines,
            cols,
            cy: 0,
            cx: 0,
            attr: 0,
            cells: vec![Cell::default(); n],
            touched: true,
        }
    }

    fn idx(&self, y: i32, x: i32) -> Option<usize> {
        (y >= 0 && x >= 0 && y < self.lines && x < self.cols).then(|| (y * self.cols + x) as usize)
    }

    /// `wmove`
    pub fn mv(&mut self, y: i32, x: i32) -> i32 {
        if y < 0 || x < 0 || y >= self.lines || x >= self.cols {
            return ERR;
        }
        self.cy = y;
        self.cx = x;
        0
    }

    fn put(&mut self, ch: String, width: i32, attr: u32) -> i32 {
        if width > self.cols - self.cx {
            // A wide character that does not fit wraps; pad the rest of the line.
            while self.cx < self.cols {
                if let Some(i) = self.idx(self.cy, self.cx) {
                    self.cells[i] = Cell {
                        ch: " ".into(),
                        attr,
                    };
                }
                self.cx += 1;
            }
            if self.cy + 1 >= self.lines {
                return ERR;
            }
            self.cy += 1;
            self.cx = 0;
        }
        let Some(i) = self.idx(self.cy, self.cx) else {
            return ERR;
        };
        self.cells[i] = Cell { ch, attr };
        for k in 1..width {
            if let Some(j) = self.idx(self.cy, self.cx + k) {
                self.cells[j] = Cell {
                    ch: String::new(),
                    attr,
                };
            }
        }
        self.cx += width;
        self.touched = true;
        if self.cx >= self.cols {
            if self.cy + 1 >= self.lines {
                // ncurses leaves the cursor in the last cell and reports an error.
                self.cx = self.cols - 1;
                return ERR;
            }
            self.cy += 1;
            self.cx = 0;
        }
        0
    }

    /// `waddch` for one wide character (controls shown as ^X, as ncurses does).
    pub fn addwch(&mut self, c: i32) -> i32 {
        let attr = self.attr;
        match c {
            0x0a => {
                self.clrtoeol();
                if self.cy + 1 >= self.lines {
                    return ERR;
                }
                self.cy += 1;
                self.cx = 0;
                0
            }
            0x09 => {
                let next = (self.cx / 8 + 1) * 8;
                while self.cx < next.min(self.cols) {
                    if self.put(" ".into(), 1, attr) == ERR {
                        return ERR;
                    }
                    if self.cx == 0 {
                        break;
                    }
                }
                0
            }
            0x08 => {
                if self.cx > 0 {
                    self.cx -= 1;
                }
                0
            }
            0..=0x1f | 0x7f => {
                let shown = if c == 0x7f {
                    '?'
                } else {
                    char::from((c + 64) as u8)
                };
                if self.put("^".into(), 1, attr) == ERR {
                    return ERR;
                }
                self.put(shown.to_string(), 1, attr)
            }
            _ => {
                let ch = char::from_u32(c as u32).unwrap_or('?');
                let w = wcwidth(c);
                if w <= 0 {
                    // Combining characters join the previous cell.
                    if let Some(i) = self.idx(self.cy, (self.cx - 1).max(0)) {
                        self.cells[i].ch.push(ch);
                    }
                    return 0;
                }
                self.put(ch.to_string(), w, attr)
            }
        }
    }

    /// `waddch` for a line-drawing or ASCII character.
    pub fn addch(&mut self, c: char) -> i32 {
        self.addwch(c as i32)
    }

    /// `waddnstr`: bytes in the locale's encoding, at most `n` bytes (-1: all).
    pub fn addnstr(&mut self, s: &[u8], n: i32) -> i32 {
        let s = if n >= 0 {
            &s[..s.len().min(n as usize)]
        } else {
            s
        };
        let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
        let mut i = 0;
        while i < s.len() {
            let len = locale::mblen_at(&s[i..]);
            let (c, step) = if len <= 0 {
                (i32::from(s[i]), 1)
            } else {
                let w = locale::mbstowcs(&s[i..i + len as usize], 1).unwrap_or_default();
                (w.first().copied().unwrap_or(i32::from(s[i])), len as usize)
            };
            if self.addwch(c) == ERR {
                return ERR;
            }
            i += step;
        }
        0
    }

    pub fn addstr(&mut self, s: &[u8]) -> i32 {
        self.addnstr(s, -1)
    }

    /// `waddnwstr`
    pub fn addnwstr(&mut self, s: &[i32], n: i32) -> i32 {
        let s = if n >= 0 {
            &s[..s.len().min(n as usize)]
        } else {
            s
        };
        for &c in s.iter().take_while(|&&c| c != 0) {
            if self.addwch(c) == ERR {
                return ERR;
            }
        }
        0
    }

    pub fn mvaddstr(&mut self, y: i32, x: i32, s: &[u8]) -> i32 {
        if self.mv(y, x) == ERR {
            return ERR;
        }
        self.addstr(s)
    }

    /// `wclrtoeol`
    pub fn clrtoeol(&mut self) {
        let attr = self.attr & A_COLOR;
        for x in self.cx..self.cols {
            if let Some(i) = self.idx(self.cy, x) {
                self.cells[i] = Cell {
                    ch: " ".into(),
                    attr,
                };
            }
        }
        self.touched = true;
    }

    /// `werase`
    pub fn erase(&mut self) {
        self.cells.iter_mut().for_each(|c| *c = Cell::default());
        self.cy = 0;
        self.cx = 0;
        self.touched = true;
    }

    pub fn attron(&mut self, a: u32) {
        if a & A_COLOR != 0 {
            self.attr &= !A_COLOR;
        }
        self.attr |= a;
    }

    pub fn attroff(&mut self, a: u32) {
        self.attr &= !a;
    }

    pub fn attrset(&mut self, a: u32) {
        self.attr = a;
    }

    /// `wborder` with the default line-drawing characters (`box(win, 0, 0)`).
    pub fn border(&mut self) {
        let (h, w) = (self.lines, self.cols);
        let attr = self.attr;
        let set = |win: &mut Window, y: i32, x: i32, c: char| {
            if let Some(i) = win.idx(y, x) {
                win.cells[i] = Cell {
                    ch: c.to_string(),
                    attr,
                };
            }
        };
        for x in 1..w - 1 {
            set(self, 0, x, acs::HLINE);
            set(self, h - 1, x, acs::HLINE);
        }
        for y in 1..h - 1 {
            set(self, y, 0, acs::VLINE);
            set(self, y, w - 1, acs::VLINE);
        }
        set(self, 0, 0, acs::ULCORNER);
        set(self, 0, w - 1, acs::URCORNER);
        set(self, h - 1, 0, acs::LLCORNER);
        set(self, h - 1, w - 1, acs::LRCORNER);
        self.touched = true;
    }

    /// `winch`-like: the text of a row, for tests.
    pub fn row_text(&self, y: i32) -> String {
        (0..self.cols)
            .filter_map(|x| self.idx(y, x))
            .map(|i| self.cells[i].ch.as_str())
            .collect()
    }
}

fn sgr(attr: u32, pairs: &[Pair; 256], colors: bool) -> String {
    let mut s = String::from("\x1b[0");
    for (bit, code) in [
        (A_BOLD, "1"),
        (A_DIM, "2"),
        (A_ITALIC, "3"),
        (A_UNDERLINE, "4"),
        (A_BLINK, "5"),
    ] {
        if attr & bit != 0 {
            s.push(';');
            s.push_str(code);
        }
    }
    if attr & (A_REVERSE | A_STANDOUT) != 0 {
        s.push_str(";7");
    }
    let pair = ((attr & A_COLOR) >> 8) as usize;
    if colors && pair != 0 {
        let (fg, bg) = pairs[pair];
        for (c, base) in [(fg, 30), (bg, 40)] {
            match c {
                0..=7 => s.push_str(&format!(";{}", base + c)),
                8..=255 => s.push_str(&format!(";{};5;{}", base + 8, c)),
                _ => {}
            }
        }
    }
    s.push('m');
    s
}

impl Curses {
    /// `initscr` (or `newterm(NULL, stderr, stdin)` in picker mode): the terminal goes to
    /// the alternate screen in ncurses' cbreak/noecho/nonl modes.
    pub fn init(to_stderr: bool) -> Option<Curses> {
        let tty_in = std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/stdin")
            .ok()?;
        let saved = rustix::termios::tcgetattr(&tty_in).ok()?;
        let mut t = saved.clone();
        use rustix::termios::{InputModes, LocalModes, SpecialCodeIndex};
        t.local_modes &= !(LocalModes::ICANON | LocalModes::ECHO);
        t.local_modes |= LocalModes::ISIG;
        t.input_modes &= !InputModes::ICRNL;
        t.special_codes[SpecialCodeIndex::VMIN] = 1;
        t.special_codes[SpecialCodeIndex::VTIME] = 0;
        rustix::termios::tcsetattr(&tty_in, rustix::termios::OptionalActions::Drain, &t).ok()?;
        let out: Box<dyn Write> = if to_stderr {
            Box::new(std::io::stderr())
        } else {
            Box::new(std::io::stdout())
        };
        let (lines, cols) = Self::size(&tty_in);
        let resized = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _ = signal_hook::flag::register(signal_hook::consts::SIGWINCH, resized.clone());
        let mut c = Curses {
            stdscr: Window::new(lines, cols, 0, 0),
            overlays: Vec::new(),
            front: Vec::new(),
            out,
            tty_in,
            saved_termios: Some(saved),
            pending: Vec::new(),
            ungot: Vec::new(),
            timeout_ms: -1,
            escdelay_ms: 1000,
            pairs: [(-1, -1); 256],
            colors_enabled: false,
            cursor_visible: true,
            mouse: false,
            last_mouse: MouseEvent::default(),
            resized,
            ended: false,
        };
        c.emit("\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\x1b[H\x1b[2J");
        c.front = vec![Cell::default(); (lines * cols) as usize];
        Some(c)
    }

    fn size(fd: &std::fs::File) -> (i32, i32) {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<i32>().ok())
                .filter(|&v| v > 0)
        };
        let ws = rustix::termios::tcgetwinsize(fd).ok();
        let lines = env("LINES")
            .or(ws.map(|w| i32::from(w.ws_row)).filter(|&v| v > 0))
            .unwrap_or(24);
        let cols = env("COLUMNS")
            .or(ws.map(|w| i32::from(w.ws_col)).filter(|&v| v > 0))
            .unwrap_or(80);
        (lines, cols)
    }

    fn emit(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
    }

    pub fn lines(&self) -> i32 {
        self.stdscr.lines
    }

    pub fn cols(&self) -> i32 {
        self.stdscr.cols
    }

    /// `curs_set`
    pub fn curs_set(&mut self, visible: bool) {
        self.cursor_visible = visible;
        self.emit(if visible { "\x1b[?25h" } else { "\x1b[?25l" });
    }

    pub fn start_color(&mut self) {
        self.colors_enabled = true;
    }

    pub fn init_pair(&mut self, n: usize, fg: i16, bg: i16) {
        if n < 256 {
            self.pairs[n] = (fg, bg);
        }
    }

    /// `mousemask`: report presses (X10 and SGR encodings).
    pub fn mousemask(&mut self, on: bool) {
        self.mouse = on;
        self.emit(if on {
            "\x1b[?1000h\x1b[?1006h"
        } else {
            "\x1b[?1006l\x1b[?1000l"
        });
    }

    pub fn getmouse(&self) -> MouseEvent {
        self.last_mouse
    }

    /// `timeout`: milliseconds `getch` waits; -1 blocks.
    pub fn timeout(&mut self, ms: i32) {
        self.timeout_ms = ms;
    }

    pub fn set_escdelay(&mut self, ms: u64) {
        self.escdelay_ms = ms;
    }

    /// `newwin`
    pub fn newwin(&self, lines: i32, cols: i32, y: i32, x: i32) -> Window {
        Window::new(lines, cols, y, x)
    }

    /// `wrefresh` of a popup window: it is drawn over stdscr until `delwin`.
    pub fn wrefresh(&mut self, win: &Window) {
        self.overlays
            .retain(|w| (w.y0, w.x0, w.lines, w.cols) != (win.y0, win.x0, win.lines, win.cols));
        self.overlays.push(win.clone());
        self.refresh();
    }

    /// `delwin`: stop drawing the popup; the screen shows stdscr again on the next refresh.
    pub fn delwin(&mut self, win: &Window) {
        self.overlays
            .retain(|w| (w.y0, w.x0, w.lines, w.cols) != (win.y0, win.x0, win.lines, win.cols));
        self.stdscr.touched = true;
    }

    fn composed(&self) -> Vec<Cell> {
        let mut cells = self.stdscr.cells.clone();
        let cols = self.stdscr.cols;
        for w in &self.overlays {
            for y in 0..w.lines {
                for x in 0..w.cols {
                    let (sy, sx) = (w.y0 + y, w.x0 + x);
                    if sy < self.stdscr.lines
                        && sx < cols
                        && sy >= 0
                        && sx >= 0
                        && let Some(i) = w.idx(y, x)
                    {
                        cells[(sy * cols + sx) as usize] = w.cells[i].clone();
                    }
                }
            }
        }
        cells
    }

    /// `refresh`: send what changed since the last refresh.
    pub fn refresh(&mut self) {
        let cells = self.composed();
        let cols = self.stdscr.cols;
        let mut s = String::new();
        let mut cur_attr = u32::MAX;
        let mut at: Option<(i32, i32)> = None;
        for (i, cell) in cells.iter().enumerate() {
            if self.front.get(i) == Some(cell) || cell.ch.is_empty() {
                continue;
            }
            let (y, x) = (i as i32 / cols, i as i32 % cols);
            if at != Some((y, x)) {
                s.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
            }
            if cell.attr != cur_attr {
                s.push_str(&sgr(cell.attr, &self.pairs, self.colors_enabled));
                cur_attr = cell.attr;
            }
            s.push_str(&cell.ch);
            let w = cell
                .ch
                .chars()
                .next()
                .map_or(1, |c| wcwidth(c as i32).max(1));
            at = Some((y, x + w));
            for k in 1..w as usize {
                if let Some(next) = cells.get(i + k)
                    && next.ch.is_empty()
                    && let Some(f) = self.front.get_mut(i + k)
                {
                    *f = next.clone();
                }
            }
        }
        if cur_attr != u32::MAX {
            s.push_str("\x1b[0m");
        }
        let (cy, cx) = (self.stdscr.cy, self.stdscr.cx);
        s.push_str(&format!("\x1b[{};{}H", cy + 1, cx + 1));
        self.front = cells;
        self.stdscr.touched = false;
        self.emit(&s);
        let _ = self.out.flush();
    }

    /// `clearok` + `refresh`: repaint everything.
    pub fn redraw_all(&mut self) {
        self.front = vec![
            Cell {
                ch: "\0".into(),
                attr: 0
            };
            self.front.len()
        ];
        self.emit("\x1b[0m\x1b[H\x1b[2J");
        self.refresh();
    }

    /// `endwin`: give the terminal back (main screen, cooked modes); `refresh` resumes.
    pub fn endwin(&mut self) {
        if self.ended {
            return;
        }
        if self.mouse {
            self.emit("\x1b[?1006l\x1b[?1000l");
        }
        self.emit("\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?1049l\x1b[23;0;0t");
        let _ = self.out.flush();
        if let Some(t) = &self.saved_termios {
            let _ = rustix::termios::tcsetattr(
                &self.tty_in,
                rustix::termios::OptionalActions::Drain,
                t,
            );
        }
        self.ended = true;
    }

    /// Back from `endwin`, as ncurses does on the next refresh.
    pub fn resume(&mut self) {
        if !self.ended {
            return;
        }
        self.ended = false;
        if let Some(saved) = self.saved_termios.clone() {
            let mut t = saved;
            use rustix::termios::{InputModes, LocalModes, SpecialCodeIndex};
            t.local_modes &= !(LocalModes::ICANON | LocalModes::ECHO);
            t.local_modes |= LocalModes::ISIG;
            t.input_modes &= !InputModes::ICRNL;
            t.special_codes[SpecialCodeIndex::VMIN] = 1;
            t.special_codes[SpecialCodeIndex::VTIME] = 0;
            let _ = rustix::termios::tcsetattr(
                &self.tty_in,
                rustix::termios::OptionalActions::Drain,
                &t,
            );
        }
        self.emit("\x1b[?1049h\x1b[?1h\x1b=");
        if self.mouse {
            self.emit("\x1b[?1000h\x1b[?1006h");
        }
        if !self.cursor_visible {
            self.emit("\x1b[?25l");
        }
        self.redraw_all();
    }

    /// Resize to the terminal's current size, as ncurses does before it returns
    /// `KEY_RESIZE`: the contents are kept where they fit; everything is repainted.
    pub fn resize(&mut self) {
        let (lines, cols) = Self::size(&self.tty_in);
        if (lines, cols) == (self.stdscr.lines, self.stdscr.cols) {
            return;
        }
        let mut win = Window::new(lines, cols, 0, 0);
        for y in 0..lines.min(self.stdscr.lines) {
            for x in 0..cols.min(self.stdscr.cols) {
                if let (Some(d), Some(s)) = (win.idx(y, x), self.stdscr.idx(y, x)) {
                    win.cells[d] = self.stdscr.cells[s].clone();
                }
            }
        }
        win.cy = self.stdscr.cy.min(lines - 1);
        win.cx = self.stdscr.cx.min(cols - 1);
        win.attr = self.stdscr.attr;
        self.stdscr = win;
        self.front = vec![
            Cell {
                ch: "\0".into(),
                attr: 0
            };
            (lines * cols) as usize
        ];
    }

    pub fn ungetch(&mut self, c: i32) {
        self.ungot.push(c);
    }

    fn fill(&mut self, wait_ms: i32) -> bool {
        let fd = &self.tty_in;
        let mut pfd = [rustix::event::PollFd::new(fd, rustix::event::PollFlags::IN)];
        let t = if wait_ms < 0 {
            None
        } else {
            Some(rustix::event::Timespec {
                tv_sec: i64::from(wait_ms / 1000),
                tv_nsec: i64::from(wait_ms % 1000) * 1_000_000,
            })
        };
        match rustix::event::poll(&mut pfd, t.as_ref()) {
            Ok(n) if n > 0 => {
                let mut buf = [0u8; 256];
                match (&self.tty_in).read(&mut buf) {
                    Ok(n) if n > 0 => {
                        self.pending.extend_from_slice(&buf[..n]);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// `get_wch`: a key code (`Key`), a character, or `None` on timeout.
    pub fn get_wch(&mut self) -> Option<Input> {
        // wgetch refreshes stdscr first, as ncurses does.
        if !self.ended {
            self.refresh();
        }
        if let Some(c) = self.ungot.pop() {
            return Some(Input::Key(c));
        }
        if self
            .resized
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            {
                self.resize();
                return Some(Input::Key(KEY_RESIZE));
            }
        }
        if self.pending.is_empty() && !self.fill(self.timeout_ms) {
            if self
                .resized
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                {
                    self.resize();
                    return Some(Input::Key(KEY_RESIZE));
                }
            }
            return None;
        }
        if self.pending.first() == Some(&0x1b) {
            let deadline = Instant::now() + Duration::from_millis(self.escdelay_ms);
            loop {
                match decode_escape(&self.pending) {
                    Decoded::Key(k, n) => {
                        let seq: Vec<u8> = self.pending.drain(..n).collect();
                        if k == Input::Key(KEY_MOUSE) {
                            self.last_mouse = parse_mouse(&seq).unwrap_or_default();
                        }
                        return Some(k);
                    }
                    Decoded::Incomplete => {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() || !self.fill(left.as_millis() as i32) {
                            break;
                        }
                    }
                    Decoded::NotEscape => break,
                }
            }
            self.pending.remove(0);
            return Some(Input::Char(0x1b));
        }
        // One character in the locale's encoding.
        let len = locale::mblen_at(&self.pending).max(1) as usize;
        while self.pending.len() < len && self.fill(50) {}
        let take = len.min(self.pending.len());
        let bytes: Vec<u8> = self.pending.drain(..take).collect();
        let c = locale::mbstowcs(&bytes, 1)
            .and_then(|w| w.first().copied())
            .unwrap_or(i32::from(bytes[0]));
        Some(Input::Char(c))
    }

    /// `getch`: like `get_wch` with characters as bytes; `ERR` on timeout.
    pub fn getch(&mut self) -> i32 {
        match self.get_wch() {
            Some(Input::Key(k)) => {
                if k == KEY_MOUSE {
                    return k;
                }
                k
            }
            Some(Input::Char(c)) => c,
            None => ERR,
        }
    }

    pub fn set_mouse(&mut self, m: MouseEvent) {
        self.last_mouse = m;
    }
}

impl Drop for Curses {
    fn drop(&mut self) {
        self.endwin();
    }
}

/// A key from `get_wch`: `KEY_CODE_YES` keys or a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(i32),
    Char(i32),
}

enum Decoded {
    Key(Input, usize),
    Incomplete,
    NotEscape,
}

/// Escape sequences ncurses decodes with keypad on (xterm, screen and tmux terminfo).
fn decode_escape(b: &[u8]) -> Decoded {
    const SEQS: &[(&[u8], i32)] = &[
        (b"\x1b[A", KEY_UP),
        (b"\x1bOA", KEY_UP),
        (b"\x1b[B", KEY_DOWN),
        (b"\x1bOB", KEY_DOWN),
        (b"\x1b[C", KEY_RIGHT),
        (b"\x1bOC", KEY_RIGHT),
        (b"\x1b[D", KEY_LEFT),
        (b"\x1bOD", KEY_LEFT),
        (b"\x1b[H", KEY_HOME),
        (b"\x1bOH", KEY_HOME),
        (b"\x1b[1~", KEY_HOME),
        (b"\x1b[7~", KEY_HOME),
        (b"\x1b[F", KEY_END),
        (b"\x1bOF", KEY_END),
        (b"\x1b[4~", KEY_END),
        (b"\x1b[8~", KEY_END),
        (b"\x1b[5~", KEY_PPAGE),
        (b"\x1b[6~", KEY_NPAGE),
        (b"\x1b[3~", KEY_DC),
        (b"\x1b[2~", KEY_IC),
        (b"\x1b[Z", KEY_BTAB),
        (b"\x1bOM", KEY_ENTER),
    ];
    let mut prefix = false;
    for (seq, key) in SEQS {
        if b.starts_with(seq) {
            return Decoded::Key(Input::Key(*key), seq.len());
        }
        if seq.starts_with(b) {
            prefix = true;
        }
    }
    // Mouse: SGR "\x1b[<b;x;yM" or X10 "\x1b[M" + 3 bytes.
    if b.starts_with(b"\x1b[<") {
        if let Some(end) = b.iter().position(|&c| c == b'M' || c == b'm') {
            return Decoded::Key(Input::Key(KEY_MOUSE), end + 1);
        }
        return Decoded::Incomplete;
    }
    if b.starts_with(b"\x1b[M") {
        return if b.len() >= 6 {
            Decoded::Key(Input::Key(KEY_MOUSE), 6)
        } else {
            Decoded::Incomplete
        };
    }
    if b == b"\x1b[<" || b == b"\x1b[" || b == b"\x1bO" || prefix {
        return Decoded::Incomplete;
    }
    Decoded::NotEscape
}

/// Parse the mouse report `get_wch` consumed (SGR or X10) into ncurses button bits.
pub fn parse_mouse(seq: &[u8]) -> Option<MouseEvent> {
    let (btn, x, y, press) = if let Some(rest) = seq.strip_prefix(b"\x1b[<") {
        let s = std::str::from_utf8(rest).ok()?;
        let press = s.ends_with('M');
        let mut it = s
            .trim_end_matches(['M', 'm'])
            .split(';')
            .map(|v| v.parse::<i32>().ok());
        (it.next()??, it.next()?? - 1, it.next()?? - 1, press)
    } else if seq.len() == 6 {
        (
            i32::from(seq[3]) - 32,
            i32::from(seq[4]) - 33,
            i32::from(seq[5]) - 33,
            (seq[3] & 3) != 3,
        )
    } else {
        return None;
    };
    if !press {
        return None;
    }
    let bstate = match btn & 0x43 {
        0 => BUTTON1_PRESSED,
        1 => BUTTON2_PRESSED,
        2 => BUTTON3_PRESSED,
        0x40 => BUTTON4_PRESSED,
        0x41 => BUTTON5_PRESSED,
        _ => 0,
    };
    Some(MouseEvent { x, y, bstate })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_wraps_and_clips_like_ncurses() {
        let mut w = Window::new(2, 4, 0, 0);
        assert_eq!(w.addstr(b"abcdef"), 0);
        assert_eq!(w.row_text(0), "abcd");
        assert_eq!(w.row_text(1), "ef  ");
        assert_eq!(w.addstr(b"xyz"), ERR);
    }

    #[test]
    fn escapes() {
        assert!(matches!(
            decode_escape(b"\x1bOA"),
            Decoded::Key(Input::Key(KEY_UP), 3)
        ));
        assert!(matches!(decode_escape(b"\x1b["), Decoded::Incomplete));
        assert!(matches!(decode_escape(b"\x1bx"), Decoded::NotEscape));
    }
}
