//! The part of ncurses nnn uses (ADR-017): a virtual screen of cells, windows, attributes,
//! diff-based refresh with ANSI sequences and keypad decoding of raw input into curses key
//! codes. The terminal itself (modes, size, input, resizes) is `platform::Term`.

use crate::locale;
use crate::platform::{ReadResult, Term};
use std::io::Write;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    /// The character, or empty for the right half of a wide one.
    ch: String,
    attr: u32,
    fg: Color,
    bg: Color,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: " ".to_string(),
            attr: 0,
            fg: Color::Default,
            bg: Color::Default,
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
    term: Box<dyn TermIo>,
    pending: Vec<u8>,
    ungot: Vec<i32>,
    timeout_ms: i32,
    escdelay_ms: u64,
    pub pairs: [Pair; 256],
    pub colors_enabled: bool,
    cursor_visible: bool,
    mouse: bool,
    last_mouse: MouseEvent,
    ended: bool,
    clear_on_refresh: bool,
}

/// The terminal `Curses` draws on and reads keys from: `platform::Term`, or a test double.
pub trait TermIo {
    /// (lines, columns)
    fn size(&self) -> (i32, i32);
    /// Waits up to `wait_ms` (-1: no limit) for input and appends it to `out`.
    fn read(&mut self, wait_ms: i32, out: &mut Vec<u8>) -> ReadResult;
    /// A resize not reported yet (SIGWINCH on Unix).
    fn resize_pending(&self) -> bool;
    fn set_resize_pending(&mut self, on: bool);
    /// The modes the terminal had before (`endwin`).
    fn restore(&mut self);
    /// Curses' modes again.
    fn resume(&mut self);
}

impl TermIo for Term {
    fn size(&self) -> (i32, i32) {
        Term::size(self)
    }
    fn read(&mut self, wait_ms: i32, out: &mut Vec<u8>) -> ReadResult {
        Term::read(self, wait_ms, out)
    }
    fn resize_pending(&self) -> bool {
        Term::resize_pending(self)
    }
    fn set_resize_pending(&mut self, on: bool) {
        Term::set_resize_pending(self, on);
    }
    fn restore(&mut self) {
        Term::restore(self);
    }
    fn resume(&mut self) {
        Term::resume(self);
    }
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
        if self.cy != y || self.cx != x {
            self.cy = y;
            self.cx = x;
            self.touched = true;
        }
        0
    }

    fn clear_wide_overlap(&mut self, y: i32, x: i32, width: i32) {
        if width <= 0 {
            return;
        }
        if let Some(i) = self.idx(y, x)
            && self.cells[i].ch.is_empty()
        {
            let mut bx = x - 1;
            while bx >= 0 {
                if let Some(j) = self.idx(y, bx) {
                    let was_empty = self.cells[j].ch.is_empty();
                    self.cells[j].ch = " ".into();
                    if !was_empty {
                        break;
                    }
                }
                bx -= 1;
            }
        }
        let mut rx = x + width;
        while let Some(j) = self.idx(y, rx) {
            if !self.cells[j].ch.is_empty() {
                break;
            }
            self.cells[j].ch = " ".into();
            rx += 1;
        }
    }

    fn put_styled(&mut self, y: i32, x: i32, ch: String, width: i32, style: (u32, Color, Color)) {
        let (attr, fg, bg) = style;
        let Some(i) = self.idx(y, x) else {
            return;
        };
        self.clear_wide_overlap(y, x, width);
        self.cells[i] = Cell { ch, attr, fg, bg };
        for k in 1..width {
            if let Some(j) = self.idx(y, x + k) {
                self.cells[j] = Cell {
                    ch: String::new(),
                    attr,
                    fg,
                    bg,
                };
            }
        }
        self.touched = true;
    }

    fn put(&mut self, ch: String, width: i32, attr: u32) -> i32 {
        if width > self.cols - self.cx {
            // A wide character that does not fit wraps; pad the rest of the line.
            if self.cx < self.cols {
                self.clear_wide_overlap(self.cy, self.cx, self.cols - self.cx);
            }
            while self.cx < self.cols {
                if let Some(i) = self.idx(self.cy, self.cx) {
                    self.cells[i] = Cell {
                        ch: " ".into(),
                        attr,
                        fg: Color::Default,
                        bg: Color::Default,
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
        if self.idx(self.cy, self.cx).is_none() {
            return ERR;
        }
        self.put_styled(
            self.cy,
            self.cx,
            ch,
            width,
            (attr, Color::Default, Color::Default),
        );
        self.cx += width;
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
                    // Combining characters join the previous base cell.
                    if self.cx == 0 {
                        return 0;
                    }
                    let mut x = self.cx - 1;
                    while x > 0 {
                        if let Some(i) = self.idx(self.cy, x)
                            && !self.cells[i].ch.is_empty()
                        {
                            break;
                        }
                        x -= 1;
                    }
                    if let Some(i) = self.idx(self.cy, x)
                        && !self.cells[i].ch.is_empty()
                    {
                        self.cells[i].ch.push(ch);
                        self.touched = true;
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

    /// Render a single line with ANSI SGR styling at `(y, x)`, clipped to `max_cols`.
    pub fn mvaddansi(&mut self, y: i32, x: i32, max_cols: usize, line: &[u8]) {
        if y < 0 || y >= self.lines || x < 0 || x >= self.cols || max_cols == 0 {
            return;
        }
        self.mv(y, x);
        let limit = max_cols.min((self.cols - x) as usize);
        let mut col: usize = 0;
        let mut attr: u32 = 0;
        let mut fg = Color::Default;
        let mut bg = Color::Default;
        let mut i = 0;
        while i < line.len() {
            match line[i] {
                0x1b => {
                    if i + 1 < line.len() && line[i + 1] == b'[' {
                        let start = i + 2;
                        let mut j = start;
                        while j < line.len() && !(0x40..=0x7e).contains(&line[j]) {
                            j += 1;
                        }
                        if j < line.len() {
                            if line[j] == b'm' {
                                apply_sgr(&line[start..j], &mut attr, &mut fg, &mut bg);
                            }
                            i = j + 1;
                        } else {
                            i = j;
                        }
                    } else if i + 1 < line.len() && line[i + 1] == b']' {
                        let mut j = i + 2;
                        while j < line.len() {
                            if line[j] == 0x07 {
                                j += 1;
                                break;
                            }
                            if line[j] == 0x1b && j + 1 < line.len() && line[j + 1] == b'\\' {
                                j += 2;
                                break;
                            }
                            j += 1;
                        }
                        i = j;
                    } else {
                        i = (i + 2).min(line.len());
                    }
                }
                b'\r' | b'\n' => break,
                b'\t' => {
                    let target = ((col / 4 + 1) * 4).min(limit);
                    while col < target {
                        self.put_styled(y, x + col as i32, " ".into(), 1, (attr, fg, bg));
                        col += 1;
                    }
                    i += 1;
                }
                0x00..=0x1f | 0x7f => {
                    i += 1;
                }
                _ => {
                    let len = locale::mblen_at(&line[i..]);
                    let (c, step) = if len <= 0 {
                        (i32::from(line[i]), 1)
                    } else {
                        let w = locale::mbstowcs(&line[i..i + len as usize], 1).unwrap_or_default();
                        (w.first().copied().unwrap_or(i32::from(line[i])), len as usize)
                    };
                    i += step;
                    let ch = char::from_u32(c as u32).unwrap_or('?');
                    let w = wcwidth(c);
                    if w < 0 {
                        // Drop unprintable / C1 control characters so they cannot corrupt output.
                        continue;
                    }
                    if w == 0 {
                        if col > 0 {
                            let mut bx = x + col as i32 - 1;
                            while bx > x {
                                if let Some(idx) = self.idx(y, bx)
                                    && !self.cells[idx].ch.is_empty()
                                {
                                    break;
                                }
                                bx -= 1;
                            }
                            if let Some(idx) = self.idx(y, bx)
                                && !self.cells[idx].ch.is_empty()
                            {
                                self.cells[idx].ch.push(ch);
                                self.touched = true;
                            }
                        }
                        continue;
                    }
                    if col + w as usize > limit {
                        break;
                    }
                    self.put_styled(y, x + col as i32, ch.to_string(), w, (attr, fg, bg));
                    col += w as usize;
                }
            }
        }
        self.cx = (x + col as i32).min(self.cols - 1);
    }

    /// `wclrtoeol`
    pub fn clrtoeol(&mut self) {
        if self.cx < self.cols {
            self.clear_wide_overlap(self.cy, self.cx, self.cols - self.cx);
        }
        let attr = self.attr & A_COLOR;
        for x in self.cx..self.cols {
            if let Some(i) = self.idx(self.cy, x) {
                self.cells[i] = Cell {
                    ch: " ".into(),
                    attr,
                    fg: Color::Default,
                    bg: Color::Default,
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
                    fg: Color::Default,
                    bg: Color::Default,
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

struct SgrParams<'a>(&'a [u8], bool);

impl<'a> SgrParams<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self(b, false)
    }

    fn next(&mut self) -> Option<u16> {
        if self.1 {
            return None;
        }
        let part = match self.0.iter().position(|&c| c == b';') {
            Some(pos) => {
                let head = &self.0[..pos];
                self.0 = &self.0[pos + 1..];
                head
            }
            None => {
                self.1 = true;
                self.0
            }
        };
        let mut val: u16 = 0;
        for &b in part {
            if b.is_ascii_digit() {
                val = val.saturating_mul(10).saturating_add(u16::from(b - b'0'));
            }
        }
        Some(val)
    }
}

fn apply_sgr(params: &[u8], attr: &mut u32, fg: &mut Color, bg: &mut Color) {
    let mut it = SgrParams::new(params);
    while let Some(n) = it.next() {
        match n {
            0 => {
                *attr = 0;
                *fg = Color::Default;
                *bg = Color::Default;
            }
            1 => *attr |= A_BOLD,
            2 => *attr |= A_DIM,
            3 => *attr |= A_ITALIC,
            4 => *attr |= A_UNDERLINE,
            5 => *attr |= A_BLINK,
            7 => *attr |= A_REVERSE,
            22 => *attr &= !(A_BOLD | A_DIM),
            23 => *attr &= !A_ITALIC,
            24 => *attr &= !A_UNDERLINE,
            25 => *attr &= !A_BLINK,
            27 => *attr &= !A_REVERSE,
            30..=37 => *fg = Color::Indexed((n - 30) as u8),
            38 => match it.next() {
                Some(5) => {
                    if let Some(idx) = it.next() {
                        *fg = Color::Indexed(idx.min(255) as u8);
                    }
                }
                Some(2) => {
                    if let (Some(r), Some(g), Some(b)) = (it.next(), it.next(), it.next()) {
                        *fg = Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8);
                    }
                }
                _ => {}
            },
            39 => *fg = Color::Default,
            40..=47 => *bg = Color::Indexed((n - 40) as u8),
            48 => match it.next() {
                Some(5) => {
                    if let Some(idx) = it.next() {
                        *bg = Color::Indexed(idx.min(255) as u8);
                    }
                }
                Some(2) => {
                    if let (Some(r), Some(g), Some(b)) = (it.next(), it.next(), it.next()) {
                        *bg = Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8);
                    }
                }
                _ => {}
            },
            49 => *bg = Color::Default,
            90..=97 => *fg = Color::Indexed((n - 90 + 8) as u8),
            100..=107 => *bg = Color::Indexed((n - 100 + 8) as u8),
            _ => {}
        }
    }
}

fn sgr(attr: u32, fg: Color, bg: Color, pairs: &[Pair; 256], colors: bool) -> String {
    use std::fmt::Write as _;
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
    if colors {
        let pair = ((attr & A_COLOR) >> 8) as usize;
        let (pfg, pbg) = if pair != 0 { pairs[pair] } else { (-1, -1) };
        for (direct, fallback, base) in [(fg, pfg, 30u16), (bg, pbg, 40u16)] {
            match direct {
                Color::Indexed(n @ 0..=7) => {
                    let _ = write!(s, ";{}", base + u16::from(n));
                }
                Color::Indexed(n) => {
                    let _ = write!(s, ";{};5;{}", base + 8, n);
                }
                Color::Rgb(r, g, b) => {
                    let _ = write!(s, ";{};2;{};{};{}", base + 8, r, g, b);
                }
                Color::Default => match fallback {
                    0..=7 => {
                        let _ = write!(s, ";{}", base as i16 + fallback);
                    }
                    8..=255 => {
                        let _ = write!(s, ";{};5;{}", base + 8, fallback);
                    }
                    _ => {}
                },
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
        let term = Term::open().ok()?;
        let out: Box<dyn Write> = if to_stderr {
            Box::new(std::io::stderr())
        } else {
            Box::new(std::io::stdout())
        };
        Some(Self::with_term(Box::new(term), out))
    }

    /// Curses on `term` (already in curses' modes), drawing to `out`.
    pub fn with_term(term: Box<dyn TermIo>, out: Box<dyn Write>) -> Curses {
        let (lines, cols) = term.size();
        let (lines, cols) = if lines <= 0 || cols <= 0 {
            (0, 0)
        } else {
            (lines, cols)
        };
        let mut c = Curses {
            stdscr: Window::new(lines, cols, 0, 0),
            overlays: Vec::new(),
            front: Vec::new(),
            out,
            term,
            pending: Vec::new(),
            ungot: Vec::new(),
            timeout_ms: -1,
            escdelay_ms: 1000,
            pairs: [(-1, -1); 256],
            colors_enabled: false,
            cursor_visible: true,
            mouse: false,
            last_mouse: MouseEvent::default(),
            ended: false,
            clear_on_refresh: false,
        };
        c.emit("\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\x1b[H\x1b[2J");
        c.front = vec![Cell::default(); (lines * cols) as usize];
        c
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
        if self.stdscr.cols <= 0 || self.stdscr.lines <= 0 {
            self.clear_on_refresh = false;
            return;
        }
        if !self.stdscr.touched && self.overlays.is_empty() && !self.clear_on_refresh {
            return;
        }
        let clear = std::mem::take(&mut self.clear_on_refresh);
        let composed_buf = (!self.overlays.is_empty()).then(|| self.composed());
        let cells: &[Cell] = composed_buf.as_deref().unwrap_or(&self.stdscr.cells);
        let cols = self.stdscr.cols;
        let mut s = String::from("\x1b[?2026h");
        if clear {
            s.push_str("\x1b[0m\x1b[H\x1b[2J");
        }
        let mut cur_style: Option<(u32, Color, Color)> = None;
        let mut at: Option<(i32, i32)> = None;
        for (i, cell) in cells.iter().enumerate() {
            if self.front.get(i) == Some(cell) || cell.ch.is_empty() {
                continue;
            }
            let (y, x) = (i as i32 / cols, i as i32 % cols);
            if at != Some((y, x)) {
                s.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
            }
            let style = (cell.attr, cell.fg, cell.bg);
            if cur_style != Some(style) {
                s.push_str(&sgr(
                    cell.attr,
                    cell.fg,
                    cell.bg,
                    &self.pairs,
                    self.colors_enabled,
                ));
                cur_style = Some(style);
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
        if cur_style.is_some() {
            s.push_str("\x1b[0m");
        }
        let (cy, cx) = (self.stdscr.cy, self.stdscr.cx);
        s.push_str(&format!("\x1b[{};{}H", cy + 1, cx + 1));
        s.push_str("\x1b[?2026l");
        if let Some(buf) = composed_buf {
            self.front = buf;
        } else {
            self.front.clone_from(&self.stdscr.cells);
        }
        self.stdscr.touched = false;
        self.emit(&s);
        let _ = self.out.flush();
    }

    /// `clearok` + `refresh`: repaint everything.
    pub fn redraw_all(&mut self) {
        self.front = vec![
            Cell {
                ch: "\0".into(),
                attr: 0,
                fg: Color::Default,
                bg: Color::Default,
            };
            self.front.len()
        ];
        self.stdscr.touched = true;
        self.clear_on_refresh = true;
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
        self.term.restore();
        self.ended = true;
    }

    /// Back from `endwin`, as ncurses does on the next refresh.
    pub fn resume(&mut self) {
        if !self.ended {
            return;
        }
        self.ended = false;
        self.term.resume();
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
        let (lines, cols) = self.term.size();
        let (lines, cols) = if lines <= 0 || cols <= 0 {
            (0, 0)
        } else {
            (lines, cols)
        };
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
        win.cy = if lines > 0 {
            self.stdscr.cy.min(lines - 1)
        } else {
            0
        };
        win.cx = if cols > 0 {
            self.stdscr.cx.min(cols - 1)
        } else {
            0
        };
        win.attr = self.stdscr.attr;
        self.stdscr = win;
        self.front = vec![
            Cell {
                ch: "\0".into(),
                attr: 0,
                fg: Color::Default,
                bg: Color::Default,
            };
            (lines * cols) as usize
        ];
    }

    pub fn ungetch(&mut self, c: i32) {
        self.ungot.push(c);
    }

    /// Waits up to `wait_ms` for input into `pending`; false on timeout or a resize (which
    /// stays pending until `get_wch` reports it).
    fn fill(&mut self, wait_ms: i32) -> bool {
        match self.term.read(wait_ms, &mut self.pending) {
            ReadResult::Data => true,
            ReadResult::Resized => {
                self.term.set_resize_pending(true);
                false
            }
            ReadResult::Timeout => false,
        }
    }

    /// Takes the pending resize, if any.
    fn take_resize(&mut self) -> bool {
        let r = self.term.resize_pending();
        if r {
            self.term.set_resize_pending(false);
        }
        r
    }

    /// A resize not reported yet (`spawn` saves it and puts it back after the child).
    pub fn resize_pending(&self) -> bool {
        self.term.resize_pending()
    }

    pub fn set_resize_pending(&mut self, on: bool) {
        self.term.set_resize_pending(on);
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
        loop {
            if self.take_resize() {
                self.resize();
                return Some(Input::Key(KEY_RESIZE));
            }
            if self.pending.is_empty() && !self.fill(self.timeout_ms) {
                if self.take_resize() {
                    self.resize();
                    return Some(Input::Key(KEY_RESIZE));
                }
                return None;
            }
            if self.pending.first() == Some(&0x1b) {
                let deadline = Instant::now() + Duration::from_millis(self.escdelay_ms);
                let mut ignored = false;
                loop {
                    match decode_escape(&self.pending) {
                        Decoded::Key(k, n) => {
                            let seq: Vec<u8> = self.pending.drain(..n).collect();
                            if k == Input::Key(KEY_MOUSE) {
                                self.last_mouse = parse_mouse(&seq).unwrap_or_default();
                            }
                            return Some(k);
                        }
                        Decoded::Ignore(n) => {
                            self.pending.drain(..n);
                            ignored = true;
                            break;
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
                if ignored {
                    continue;
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
            return Some(Input::Char(c));
        }
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
    Ignore(usize),
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
    // Consume unrecognized CSI sequences: ESC '[' param(0x30..=0x3f)* inter(0x20..=0x2f)* final(0x40..=0x7e).
    const MAX_CSI_LEN: usize = 32;
    if b.starts_with(b"\x1b[") {
        let mut i = 2;
        while i < b.len() && i < MAX_CSI_LEN && (0x30..=0x3f).contains(&b[i]) {
            i += 1;
        }
        while i < b.len() && i < MAX_CSI_LEN && (0x20..=0x2f).contains(&b[i]) {
            i += 1;
        }
        if i < b.len() && i < MAX_CSI_LEN && (0x40..=0x7e).contains(&b[i]) {
            return Decoded::Ignore(i + 1);
        }
        if i == b.len() && i < MAX_CSI_LEN {
            return Decoded::Incomplete;
        }
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

    /// A terminal that plays a script of reads; a resize makes it 30x100.
    struct FakeTerm {
        script: std::collections::VecDeque<(ReadResult, &'static [u8])>,
        size: (i32, i32),
        resize_pending: bool,
    }

    impl TermIo for FakeTerm {
        fn size(&self) -> (i32, i32) {
            self.size
        }
        fn read(&mut self, _wait_ms: i32, out: &mut Vec<u8>) -> ReadResult {
            let Some((r, bytes)) = self.script.pop_front() else {
                return ReadResult::Timeout;
            };
            if r == ReadResult::Resized {
                self.size = (30, 100);
                self.resize_pending = true;
            }
            out.extend_from_slice(bytes);
            r
        }
        fn resize_pending(&self) -> bool {
            self.resize_pending
        }
        fn set_resize_pending(&mut self, on: bool) {
            self.resize_pending = on;
        }
        fn restore(&mut self) {}
        fn resume(&mut self) {}
    }

    #[test]
    fn resize_event_becomes_key_resize() {
        let term = FakeTerm {
            script: [(ReadResult::Resized, &b""[..]), (ReadResult::Data, b"q")].into(),
            size: (24, 80),
            resize_pending: false,
        };
        let mut cs = Curses::with_term(Box::new(term), Box::new(std::io::sink()));
        assert_eq!((cs.lines(), cs.cols()), (24, 80));
        assert_eq!(cs.get_wch(), Some(Input::Key(KEY_RESIZE)));
        assert_eq!((cs.lines(), cs.cols()), (30, 100));
        assert_eq!(cs.get_wch(), Some(Input::Char(i32::from(b'q'))));
        assert_eq!(cs.get_wch(), None);
    }

    struct SharedWriter(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct ResizableTerm(std::rc::Rc<std::cell::Cell<(i32, i32)>>);

    impl TermIo for ResizableTerm {
        fn size(&self) -> (i32, i32) {
            self.0.get()
        }
        fn read(&mut self, _wait_ms: i32, _out: &mut Vec<u8>) -> ReadResult {
            ReadResult::Timeout
        }
        fn resize_pending(&self) -> bool {
            false
        }
        fn set_resize_pending(&mut self, _on: bool) {}
        fn restore(&mut self) {}
        fn resume(&mut self) {}
    }

    #[test]
    fn resize_to_zero_cols_and_refresh_does_not_panic() {
        let size = std::rc::Rc::new(std::cell::Cell::new((24, 80)));
        let mut cs = Curses::with_term(
            Box::new(ResizableTerm(std::rc::Rc::clone(&size))),
            Box::new(std::io::sink()),
        );
        size.set((0, 0));
        cs.resize();
        assert_eq!((cs.lines(), cs.cols()), (0, 0));
        assert_eq!((cs.stdscr.cy, cs.stdscr.cx), (0, 0));
        cs.refresh();
    }

    fn init_locale() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(locale::init);
    }

    #[test]
    fn overwrite_wide_char_halves_clears_orphan() {
        init_locale();
        let mut w = Window::new(2, 6, 0, 0);
        w.mv(0, 0);
        w.addwch('中' as i32);
        w.mv(0, 0);
        w.addch('a');
        assert_eq!(w.cells[w.idx(0, 0).unwrap()].ch, "a");
        assert_eq!(w.cells[w.idx(0, 1).unwrap()].ch, " ");

        w.mv(0, 0);
        w.addwch('中' as i32);
        w.mv(0, 1);
        w.addch('b');
        assert_eq!(w.cells[w.idx(0, 0).unwrap()].ch, " ");
        assert_eq!(w.cells[w.idx(0, 1).unwrap()].ch, "b");
    }

    #[test]
    fn combining_mark_attaches_to_wide_base_and_ignores_col_zero() {
        init_locale();
        let mut w = Window::new(2, 6, 0, 0);
        w.mv(0, 0);
        assert_eq!(w.addwch(0x0301), 0);
        assert_eq!(w.cells[w.idx(0, 0).unwrap()].ch, " ");

        w.addwch('中' as i32);
        assert_eq!(w.cx, 2);
        assert_eq!(w.addwch(0x0301), 0);
        assert_eq!(w.cells[w.idx(0, 0).unwrap()].ch, "中\u{0301}");
        assert_eq!(w.cells[w.idx(0, 1).unwrap()].ch, "");
    }

    #[test]
    fn refresh_noop_when_untouched_and_wraps_sync_update() {
        let buf = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let term = FakeTerm {
            script: [].into(),
            size: (4, 10),
            resize_pending: false,
        };
        let mut cs = Curses::with_term(
            Box::new(term),
            Box::new(SharedWriter(std::rc::Rc::clone(&buf))),
        );
        cs.refresh();
        buf.borrow_mut().clear();

        cs.refresh();
        assert!(buf.borrow().is_empty(), "untouched refresh emitted bytes");

        cs.stdscr.mvaddstr(0, 0, b"hi");
        cs.refresh();
        let out = String::from_utf8(buf.borrow().clone()).unwrap();
        assert!(
            out.starts_with("\x1b[?2026h"),
            "missing sync start: {out:?}"
        );
        assert!(out.ends_with("\x1b[?2026l"), "missing sync end: {out:?}");
    }

    #[test]
    fn decode_escape_consumes_unknown_csi() {
        assert!(matches!(decode_escape(b"\x1b[1;5C"), Decoded::Ignore(6)));
        assert!(matches!(decode_escape(b"\x1b[1;5"), Decoded::Incomplete));
        assert!(matches!(decode_escape(b"\x1b[200~"), Decoded::Ignore(6)));
        // Known sequences still decode to keys.
        assert!(matches!(
            decode_escape(b"\x1b[A"),
            Decoded::Key(Input::Key(KEY_UP), 3)
        ));
        assert!(matches!(
            decode_escape(b"\x1b[1~"),
            Decoded::Key(Input::Key(KEY_HOME), 4)
        ));
        assert!(matches!(
            decode_escape(b"\x1b[<0;1;1M"),
            Decoded::Key(Input::Key(KEY_MOUSE), 9)
        ));

        // End-to-end: get_wch silently discards unknown CSI and returns the following key.
        let term = FakeTerm {
            script: [(ReadResult::Data, &b"\x1b[1;5Cq"[..])].into(),
            size: (24, 80),
            resize_pending: false,
        };
        let mut cs = Curses::with_term(Box::new(term), Box::new(std::io::sink()));
        assert_eq!(cs.get_wch(), Some(Input::Char(i32::from(b'q'))));
        assert_eq!(cs.get_wch(), None);
    }

    #[test]
    fn mvaddansi_parses_colors_attrs_resets_and_skips_non_sgr() {
        init_locale();
        let mut w = Window::new(3, 20, 0, 0);
        w.attrset(A_UNDERLINE);

        // 16-color, bright, 256-color, 24-bit RGB, attributes, reset, non-SGR CSI, OSC (BEL & ST)
        let line = concat!(
            "\x1b[1;31;42mA",
            "\x1b[22;91;104mB",
            "\x1b[38;5;208;48;5;235mC",
            "\x1b[38;2;10;20;30;48;2;40;50;60mD",
            "\x1b[0mE",
            "\x1b[2K\x1b]0;title\x07\x1b]8;;https://example.com\x1b\\F",
            "\r\nIGNORED"
        );
        w.mvaddansi(0, 1, 10, line.as_bytes());

        // self.attr must not be leaked/modified
        assert_eq!(w.attr, A_UNDERLINE);

        let cell = |x: i32| &w.cells[w.idx(0, x).unwrap()];
        assert_eq!(cell(1).ch, "A");
        assert_eq!(cell(1).attr, A_BOLD);
        assert_eq!(cell(1).fg, Color::Indexed(1));
        assert_eq!(cell(1).bg, Color::Indexed(2));

        assert_eq!(cell(2).ch, "B");
        assert_eq!(cell(2).attr, 0);
        assert_eq!(cell(2).fg, Color::Indexed(9));
        assert_eq!(cell(2).bg, Color::Indexed(12));

        assert_eq!(cell(3).ch, "C");
        assert_eq!(cell(3).fg, Color::Indexed(208));
        assert_eq!(cell(3).bg, Color::Indexed(235));

        assert_eq!(cell(4).ch, "D");
        assert_eq!(cell(4).fg, Color::Rgb(10, 20, 30));
        assert_eq!(cell(4).bg, Color::Rgb(40, 50, 60));

        assert_eq!(cell(5).ch, "E");
        assert_eq!(cell(5).attr, 0);
        assert_eq!(cell(5).fg, Color::Default);
        assert_eq!(cell(5).bg, Color::Default);

        assert_eq!(cell(6).ch, "F");
        assert_eq!(cell(7).ch, " ");
    }

    #[test]
    fn mvaddansi_clips_and_handles_wide_chars_and_tabs() {
        init_locale();
        let mut w = Window::new(3, 12, 0, 0);

        // CJK wide char fits in 4 cols: "a中b" = 1 + 2 + 1 = 4 cols, next 'c' clipped
        w.mvaddansi(0, 0, 4, "\x1b[32ma中bc".as_bytes());
        assert_eq!(w.row_text(0), "a中b        ");
        assert_eq!(w.cells[w.idx(0, 1).unwrap()].ch, "中");
        assert_eq!(w.cells[w.idx(0, 1).unwrap()].fg, Color::Indexed(2));
        assert_eq!(w.cells[w.idx(0, 2).unwrap()].ch, "");
        assert_eq!(w.cells[w.idx(0, 3).unwrap()].ch, "b");

        // CJK wide char does not split when only 1 col remains (max_cols = 3 -> "ab", not half of '中')
        w.mvaddansi(1, 0, 3, "ab中".as_bytes());
        assert_eq!(w.row_text(1), "ab          ");

        // Tab advances to 4-column stop
        w.mvaddansi(2, 0, 8, b"x\ty");
        assert_eq!(w.row_text(2), "x   y       ");
    }

    #[test]
    fn refresh_emits_rgb_sgr_and_preserves_normal_pair_sgr() {
        let buf = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let term = FakeTerm {
            script: [].into(),
            size: (2, 10),
            resize_pending: false,
        };
        let mut cs = Curses::with_term(
            Box::new(term),
            Box::new(SharedWriter(std::rc::Rc::clone(&buf))),
        );
        cs.start_color();
        cs.init_pair(1, 2, -1);
        cs.refresh();
        buf.borrow_mut().clear();

        // Normal pair cell on row 0
        cs.stdscr.attrset(color_pair(1));
        cs.stdscr.mvaddstr(0, 0, b"N");
        cs.stdscr.attrset(0);

        // Two adjacent RGB cells with different colors on row 1 (same attr = 0)
        cs.stdscr
            .mvaddansi(1, 0, 10, b"\x1b[38;2;12;34;56mR\x1b[38;2;78;90;12mG");
        cs.refresh();

        let out = String::from_utf8(buf.borrow().clone()).unwrap();
        assert!(
            out.contains("\x1b[0;32mN"),
            "normal pair SGR changed: {out:?}"
        );
        assert!(
            out.contains("\x1b[0;38;2;12;34;56mR\x1b[0;38;2;78;90;12mG"),
            "missing RGB SGR transitions: {out:?}"
        );
    }
}
