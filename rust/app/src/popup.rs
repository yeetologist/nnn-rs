//! Spawning programs, command output, the floating window, file stats, help and the locker
//! (src/nnn.c: parseargs, xfork, join, spawn, spawn_redirected, xgetenv, get_output,
//! buffer_command_output, show_content_in_floating_window, show_stats, xchmod, get_fs_info,
//! printkv, show_help, lock_terminal, handle_openwith, create_tmp_file, mkpath).
//!
//! fork + execvp become `std::process::Command` (no `unsafe`): the child's half of `xfork`
//! (`enable_signals`, `dup2` of the standard streams, `execvp`) is what `Command` does
//! between its fork and exec. Signals nnn catches are reset to the default by exec itself,
//! which is all `enable_signals` achieves for them.
//!
//! Conventions: `self.g_tmpfpath` is the temp directory (as listmode.rs reads it); the file
//! `create_tmp_file` makes is returned with its path instead of being written over it.
//! Case 1 of `get_output` leaves the command's output in `self.popup.g_buf`.

use crate::curses::{
    A_BOLD, ERR, Input, KEY_DOWN, KEY_END, KEY_HOME, KEY_LEFT, KEY_NPAGE, KEY_PPAGE, KEY_RESIZE,
    KEY_RIGHT, KEY_UP, Window,
};
use crate::fmt;
use crate::keys::Action;
use crate::platform::{self, Errno};
use crate::state::{Kv, Nnn, PATH_MAX};
use crate::sys;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Child, Command, Stdio};
#[cfg(windows)]
use std::sync::mpsc;
#[cfg(windows)]
use std::time::Duration;

// Flags of spawn() (nnn.c 267-279).
pub const F_NONE: u16 = 0x00;
pub const F_MULTI: u16 = 0x01;
pub const F_NOWAIT: u16 = 0x02;
pub const F_NOTRACE: u16 = 0x04;
pub const F_NORMAL: u16 = 0x08;
pub const F_CONFIRM: u16 = 0x10;
pub const F_CHKRTN: u16 = 0x20;
pub const F_NOSTDIN: u16 = 0x40;
pub const F_PAGE: u16 = 0x80;
pub const F_TTY: u16 = 0x100;
pub const F_WINDOW: u16 = 0x200;
pub const F_CLI: u16 = F_NORMAL | F_MULTI;
pub const F_SILENT: u16 = F_CLI | F_NOTRACE;

pub const EXEC_ARGS_MAX: usize = 10;
pub const CMD_LEN_MAX: usize = PATH_MAX + ((crate::state::NAME_MAX + 1) << 1);
const SIZE_8KB: usize = 8 * 1024;
const SIZE_16MB: usize = 16 * 1024 * 1024;
const ESC: i32 = 27;

pub const VFS_AVAIL: u8 = 0;
pub const VFS_USED: u8 = 1;
pub const VFS_SIZE: u8 = 2;

const VERSION: &str = "5.3";
const GENERAL_INFO: &str = "BSD 2-Clause\nhttps://github.com/jarun/nnn";
/// `messages[MSG_ENTER]`
const MSG_ENTER: &[u8] = b"\n'Enter' to continue";
/// `messages[MSG_CLI_MODE]`
const MSG_CLI_MODE: &[u8] = b"['g'ui]/'c'li?";
/// `messages[STR_TMPFILE]`
const STR_TMPFILE: &[u8] = b"/.nnnXXXXXX";
/// `utils[UTIL_LOCKER]` (Linux)
const UTIL_LOCKER: &[u8] = b"vlock";
/// `FILE_MIME_OPTS` (Linux)
#[cfg(unix)]
const FILE_MIME_OPTS: &str = "-biL";
/// `env_cfg[NNN_OPENER..=NNN_TRASH]`
const ENV_CFG_SHOWN: [&str; 11] = [
    "NNN_OPENER",
    "NNN_COLORS",
    "NNN_FCOLORS",
    "NNNLVL",
    "NNN_PIPE",
    "NNN_MCLICK",
    "NNN_SEL",
    "NNN_ARCHIVE",
    "NNN_ORDER",
    "NNN_HELP",
    "NNN_TRASH",
];

/// `helpstr` of show_help(): each line starts with a hex count of leading spaces, "%NN"
/// inserts NN spaces.
const HELPSTR: &str = concat!(
    "2|V\\_\n",
    "2/. \\\\\n",
    "1(;^; ||\n",
    "3/___3\n",
    "2(___n))\n",
    "0\n",
    "1NAVIGATION\n",
    "9Up k  Prev%14PgUp ^U  Page up\n",
    "9Dn j  Next%14PgDn ^D  Page down\n",
    "9Lt h  Parent%12~ ` @ -  ~, /, start, prev\n",
    "5Ret Rt l  Open%20'  First file/match\n",
    "9g ^A  Top%21J  Jump to entry/offset\n",
    "9G ^E  End%20^J  Toggle auto-advance on open\n",
    "c.  Toggle hidden%11d  Detail mode toggle\n",
    "8B (,)  Book(mark)%11b ^/  Select bookmark\n",
    "a1-8  Context%11(Sh)Tab  Cycle/new context\n",
    "62Esc ^Q  Quit%19^y  Next young\n",
    "b^G  QuitCD%18Q  Pick/err, quit\n",
    "4q Alt+Esc  Quit context%12R  Reopen context\n",
    "0\n",
    "1FILTER & PROMPT\n",
    "c/  Filter%17^N  Toggle type-to-nav\n",
    "aEsc  Exit prompt%12^L  Clear/apply filter\n",
    "0\n",
    "1FILES\n",
    "9o ^O  Open with%15n  Create new/link\n",
    "9f ^F  File stats%14P  Preview toggle\n",
    "b^R  Rename/dup%14r  Batch rename\n",
    "cz  Archive%17e  Edit file\n",
    "c*  Toggle exe%14>  Export list\n",
    "6Space +  (Un)select%12m-m  Select range/clear\n",
    "ca  Select all%14A  Invert sel\n",
    "9p ^P  Copy here%12w ^W  Cp/mv sel as\n",
    "9v ^V  Move here%15E  Edit sel list\n",
    "9x ^X  Delete or trash%09S  Listed sel size\n",
    "cX  Delete (rm -rf)%07Esc  Send to FIFO\n",
    "0\n",
    "1MISC\n",
    "8Alt ;  Select plugin%11=  Launch app\n",
    "9! ^]  Shell%19]  Cmd prompt\n",
    "cc  Connect remote%10u  Unmount remote/archive\n",
    "9t ^T  Sort toggles%12s  Manage session\n",
    "cT  Set time type%110  Lock\n",
    "b^L  Redraw%18?  Help, conf\n",
);

/// State this module keeps.
#[derive(Debug, Default)]
pub struct PopupState {
    /// `g_buf` as case 1 of `get_output` leaves it: the bytes the command printed.
    pub g_buf: Vec<u8>,
    /// `curssn`: the current session's name (sessions are W5; empty: none). Belongs in
    /// `Nnn` once the sessions module lands.
    pub curssn: Vec<u8>,
    /// The SIGWINCH flag as it was when `xfork` started a child (`oldsigwinch`).
    winch: Option<bool>,
}

/// A child started by `xfork` (the pid of the C original).
enum Forked {
    /// The parent's view of a child it waits for.
    Child(Child),
    /// `F_NOWAIT`: the detached grandchild, never waited for.
    Detached,
    /// fork worked but execvp failed: the child `_exit(EXIT_SUCCESS)`s.
    ExecFailed,
}

/// The bytes of a C string: up to the first NUL.
fn cstr(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

fn os(b: &[u8]) -> Cow<'_, OsStr> {
    crate::platform::os(cstr(b))
}

/// `ISBLANK`
fn isblank(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// `xitoa`: the decimal digits of an unsigned value.
fn xitoa(val: u32) -> String {
    val.to_string()
}

/// `xchartohex`
pub fn xchartohex(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => c,
    }
}

/// `mkpath`: "dir/name", "/name" in the root, `home` for a leading "~/", name itself when
/// absolute; cut to `PATH_MAX - 1` bytes as `xstrsncpy` does.
pub fn mkpath(home: &[u8], dir: &[u8], name: &[u8]) -> Vec<u8> {
    let (dir, name) = (cstr(dir), cstr(name));
    let mut out: Vec<u8> = Vec::new();
    let mut name = name;
    if name.starts_with(b"~/") {
        out.extend_from_slice(&cstr(home)[..cstr(home).len().min(PATH_MAX - 1)]);
        name = &name[1..];
    } else if name.first() != Some(&b'/') {
        if dir != b"/" {
            out.extend_from_slice(&dir[..dir.len().min(PATH_MAX - 1)]);
        }
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out.truncate(PATH_MAX - 1);
    out
}

/// `parseargs`: splits `cmd` at blanks as nnn does (a blank right after a blank starts an
/// argument holding it). `None` for more than `EXEC_ARGS_MAX - 4` arguments.
pub fn parseargs(cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut line = cstr(cmd).to_vec();
    let mut starts = vec![0usize];
    let mut i = 0;
    let mut overflow = false;
    while i < line.len() && line[i] != 0 {
        if isblank(line[i]) {
            line[i] = 0;
            i += 1;
            if i >= line.len() || line[i] == 0 {
                break;
            }
            starts.push(i);
            if starts.len() == EXEC_ARGS_MAX {
                overflow = true;
                break;
            }
        }
        i += 1;
    }
    if overflow || starts.len() > EXEC_ARGS_MAX - 4 {
        return None;
    }
    Some(starts.iter().map(|&s| cstr(&line[s..]).to_vec()).collect())
}

/// `xgetenv`: the variable's value when set and not empty, else `fallback`.
pub fn xgetenv(name: &str, fallback: Option<&[u8]>) -> Option<Vec<u8>> {
    match std::env::var_os(name) {
        Some(v) if !v.is_empty() => Some(v.into_encoded_bytes()),
        _ => fallback.map(<[u8]>::to_vec),
    }
}

/// `get_fs_info`: available, used or total bytes of the file system holding `path`
/// (shifted by `ffs(f_frsize >> 1)` as the original does); 0 when statvfs fails.
pub fn get_fs_info(path: &[u8], ty: u8) -> u64 {
    let Some(svb) = platform::fs_blocks(path) else {
        return 0;
    };
    let half = (svb.frsize >> 1) as i32;
    let shift = if half == 0 {
        0
    } else {
        half.trailing_zeros() + 1
    };
    let blocks = match ty {
        VFS_AVAIL => svb.bavail,
        VFS_USED => svb.blocks.wrapping_sub(svb.bfree),
        _ => svb.blocks,
    };
    blocks.wrapping_shl(shift)
}

impl Nnn {
    /// `get_fs_info(path, VFS_AVAIL)` as draw.rs calls it for the status bar
    /// (`avail` false: `VFS_SIZE`); show_help uses the free function with all three types.
    pub fn get_fs_info(&self, path: &[u8], avail: bool) -> u64 {
        get_fs_info(path, if avail { VFS_AVAIL } else { VFS_SIZE })
    }
}

/// `printkv`: " k: value" per pair, up to the first unset key.
fn printkv(kvarr: &[Kv], f: &mut Vec<u8>) {
    for kv in kvarr.iter().take_while(|kv| kv.key != 0) {
        f.extend_from_slice(b" ");
        f.push(kv.key as u8);
        f.extend_from_slice(b": ");
        f.extend_from_slice(&kv.val);
        f.push(b'\n');
    }
}

/// The `helpstr` part of show_help's text, expanded.
pub fn expand_helpstr() -> Vec<u8> {
    let s = HELPSTR.as_bytes();
    let mut f = Vec::new();
    let mut hex = true;
    let mut i = 0;
    while i < s.len() {
        if hex {
            f.resize(f.len() + usize::from(xchartohex(s[i])), b' ');
        } else if s[i] == b'%' {
            let d = |c: Option<&u8>| i32::from(c.copied().unwrap_or(0)) - i32::from(b'0');
            let n = d(s.get(i + 1)) * 10 + d(s.get(i + 2));
            f.resize(f.len() + n.max(0) as usize, b' ');
            i += 2;
        } else {
            f.push(s[i]);
        }
        hex = s[i] == b'\n';
        i += 1;
    }
    f
}

/// `strchr(p, '\n')` within `content` (which ends in a NUL in the original).
fn strchr_nl(content: &[u8], from: usize) -> Option<usize> {
    for (i, &c) in content.iter().enumerate().skip(from) {
        match c {
            b'\n' => return Some(i),
            0 => return None,
            _ => {}
        }
    }
    None
}

/// The line count and widest line (in bytes) of the floating window's content.
fn measure(content: &[u8]) -> (i32, i32) {
    let content_len = content.len();
    let mut line_count = content.iter().filter(|&&c| c == b'\n').count() as i32;
    if content_len > 0 && content[content_len - 1] != b'\n' {
        line_count += 1;
    }
    let mut max_line_width = 0;
    let mut p = 0;
    while p < content_len {
        let next = strchr_nl(content, p);
        let line_len = next.map_or(content_len - p, |n| n - p);
        if line_len > max_line_width as usize {
            max_line_width = line_len as i32;
        }
        match next {
            Some(n) => p = n + 1,
            None => break,
        }
    }
    (line_count, max_line_width)
}

/// What one frame of the floating window shows.
struct Frame<'a> {
    content: &'a [u8],
    perfile: bool,
    /// `(cur + 1, ndents, name)` when `perfile && ndents`.
    title: Option<(u32, u32, Vec<u8>)>,
    line_count: i32,
    max_line_width: i32,
    scroll_offset: i32,
    hscroll_offset: i32,
}

/// One pass of show_content_in_floating_window's drawing loop, up to before `wrefresh`.
fn paint_floating(win: &mut Window, fr: &Frame) {
    let (win_height, win_width) = (win.lines, win.cols);
    let max_lines = win_height - 3;
    let max_display_width = win_width - 2;
    let content = fr.content;
    let content_len = content.len();

    win.erase();
    win.border();

    if let Some((n, total, name)) = &fr.title {
        let (n, total) = (*n, *total);
        win.mv(0, 2);
        win.addch(' ');
        win.addstr(xitoa(n).as_bytes());
        win.addch('/');
        win.addstr(xitoa(total).as_bytes());
        win.addch(' ');
        let x = win.cx;
        win.attron(A_BOLD);
        win.addnstr(name, max_display_width - x - 1);
        win.attroff(A_BOLD);
        win.addch(' ');
    }

    // Skip to scroll offset
    let mut line_start = 0;
    let mut i = 0;
    while i < fr.scroll_offset && line_start < content_len {
        match strchr_nl(content, line_start) {
            Some(n) => line_start = n + 1,
            None => break,
        }
        i += 1;
    }

    // Display visible lines (from row 2: a blank line under long multi-line names)
    let mut current = line_start;
    let mut display_line = 2;
    while display_line <= max_lines && current < content_len {
        let next = strchr_nl(content, current);
        let line_len = next.map_or(content_len - current, |n| n - current);
        let (mut display_start, mut display_len) = (current, line_len);
        if fr.hscroll_offset > 0 && (fr.hscroll_offset as usize) < line_len {
            display_start = current + fr.hscroll_offset as usize;
            display_len = line_len - fr.hscroll_offset as usize;
        }
        let n = display_len.min(max_display_width.max(0) as usize);
        if win.mv(display_line, 1) != ERR {
            win.addnstr(&content[display_start..], n as i32);
        }
        match next {
            Some(nl) => {
                current = nl + 1;
                display_line += 1;
            }
            None => break,
        }
    }

    // Vertical scroll indicators
    if fr.line_count > max_lines {
        if fr.scroll_offset > 0 && win.mv(1, win_width - 2) != ERR {
            win.addch('^');
        }
        if fr.scroll_offset + max_lines < fr.line_count && win.mv(max_lines, win_width - 2) != ERR {
            win.addch('v');
        }
    }

    // Horizontal scroll indicators
    if fr.max_line_width > max_display_width {
        if fr.hscroll_offset > 0 && win.mv(max_lines + 1, 1) != ERR {
            win.addch('<');
        }
        if fr.hscroll_offset + max_display_width < fr.max_line_width
            && win.mv(max_lines + 1, win_width - 2) != ERR
        {
            win.addch('>');
        }
    }

    // Help hint
    win.mvaddstr(win_height - 1, 1, b"q/ESC:close  arrows:scroll");
    if fr.perfile {
        win.addstr(b"  n/j:next  p/k:prev");
    }
}

/// The floating window's size and place for an `xlines` x `xcols` screen:
/// (height, width, y, x).
fn floating_geometry(xlines: i32, xcols: i32) -> (i32, i32, i32, i32) {
    let mut win_height = 20.min(xlines - 4);
    let mut win_width = (xcols * 3) / 4;
    if win_height < 5 {
        win_height = 5;
    }
    if win_width < 20 {
        win_width = 20;
    }
    (
        win_height,
        win_width,
        (xlines - win_height) / 2,
        (xcols - win_width) / 2,
    )
}

/// What a key does in the floating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloatKey {
    Stay,
    Close,
    /// n/j or p/k with `perfile` and an `action` to set: close with this action.
    Move(Action),
}

/// The key switch of show_content_in_floating_window: scrolls `fr` or closes.
fn float_key(
    fr: &mut Frame,
    win_height: i32,
    win_width: i32,
    ch: i32,
    has_action: bool,
) -> FloatKey {
    let max_lines = win_height - 3;
    let max_display_width = win_width - 2;
    match ch {
        c if c == i32::from(b'q') || c == ESC => return FloatKey::Close,
        c if c == i32::from(b'j') || c == i32::from(b'n') => {
            if fr.perfile && has_action {
                return FloatKey::Move(Action::Next);
            }
        }
        c if c == i32::from(b'k') || c == i32::from(b'p') => {
            if fr.perfile && has_action {
                return FloatKey::Move(Action::Prev);
            }
        }
        KEY_UP => {
            if fr.scroll_offset > 0 {
                fr.scroll_offset -= 1;
            }
        }
        KEY_DOWN => {
            if fr.scroll_offset + max_lines < fr.line_count {
                fr.scroll_offset += 1;
            }
        }
        KEY_PPAGE => fr.scroll_offset = 0.max(fr.scroll_offset - max_lines),
        KEY_NPAGE => {
            fr.scroll_offset = (fr.line_count - max_lines).min(fr.scroll_offset + max_lines);
        }
        KEY_HOME => fr.scroll_offset = 0,
        KEY_END => fr.scroll_offset = 0.max(fr.line_count - max_lines),
        KEY_LEFT => {
            if fr.hscroll_offset > 0 {
                fr.hscroll_offset = 0.max(fr.hscroll_offset - 10);
            }
        }
        KEY_RIGHT => {
            if fr.max_line_width > max_display_width {
                fr.hscroll_offset =
                    (fr.max_line_width - max_display_width).min(fr.hscroll_offset + 10);
            }
        }
        KEY_RESIZE => return FloatKey::Close,
        _ => {}
    }
    FloatKey::Stay
}

/// `newwin` returns NULL for a negative position or an empty size; a window that runs off
/// the screen is created, and only its part on the screen shows.
fn newwin_fits(h: i32, w: i32, y: i32, x: i32) -> bool {
    y >= 0 && x >= 0 && h > 0 && w > 0
}

/// The floating window's rows (as `Window::row_text`) after `keys` on an `xlines` x
/// `xcols` screen, `None` when it does not fit: what the screen shows, for the oracle
/// (rust/oracle/popup_oracle.c). `title` is `(cur + 1, ndents, name)`; keys after the
/// one that closes the window are ignored.
pub fn floating_rows(
    xlines: i32,
    xcols: i32,
    content: &[u8],
    title: Option<(u32, u32, &[u8])>,
    perfile: bool,
    keys: &[i32],
) -> Option<Vec<String>> {
    let (h, w, y, x) = floating_geometry(xlines, xcols);
    if !newwin_fits(h, w, y, x) {
        return None;
    }
    let mut win = Window::new(h, w, y, x);
    let (line_count, max_line_width) = measure(content);
    let mut fr = Frame {
        content,
        perfile,
        title: if perfile {
            title.map(|(n, t, name)| (n, t, name.to_vec()))
        } else {
            None
        },
        line_count,
        max_line_width,
        scroll_offset: 0,
        hscroll_offset: 0,
    };
    paint_floating(&mut win, &fr);
    for &k in keys {
        if float_key(&mut fr, h, w, k, false) != FloatKey::Stay {
            break;
        }
        paint_floating(&mut win, &fr);
    }
    // Only the part on the screen shows.
    let shown = (xcols - x).clamp(0, w) as usize;
    Some(
        (0..h)
            .map(|r| {
                if y + r >= xlines {
                    return String::new();
                }
                let mut cells = 0;
                let row = win.row_text(r);
                let mut out = String::new();
                for ch in row.chars() {
                    // combining characters take no cell of their own
                    let cw = crate::curses::wcwidth(ch as i32).max(0) as usize;
                    if cells + cw > shown {
                        break;
                    }
                    cells += cw;
                    out.push(ch);
                }
                out
            })
            .collect(),
    )
}

/// `mkstemp` on a template ending in "XXXXXX": the file, created 0600, and its path.
fn mkstemp(template: &[u8]) -> io::Result<(File, Vec<u8>)> {
    use std::hash::{BuildHasher, Hasher};
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let Some(start) = template.len().checked_sub(6) else {
        return Err(platform::error(Errno::Inval));
    };
    if &template[start..] != b"XXXXXX" {
        return Err(platform::error(Errno::Inval));
    }
    let mut name = template.to_vec();
    let seed = std::collections::hash_map::RandomState::new();
    // TMP_MAX attempts, as glibc's __gen_tempname
    for attempt in 0u32..238_328 {
        let mut h = seed.build_hasher();
        h.write_u32(attempt);
        h.write_u32(std::process::id());
        let mut v = h.finish();
        for c in &mut name[start..] {
            *c = LETTERS[(v % 62) as usize];
            v /= 62;
        }
        match platform::private_mode(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true),
        )
        .open(os(&name))
        {
            Ok(f) => return Ok((f, name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(platform::error(Errno::Exist))
}

/// `spawn_redirected`: runs `argv` (looked up in PATH) with stdout and stderr on `out`.
/// `close_stdin`: the input fd passed is STDIN_FILENO, which the original closes in the
/// child; here the child's stdin is /dev/null. A pipe's own read end is close-on-exec.
fn spawn_redirected(
    argv: &[Vec<u8>],
    words: usize,
    close_stdin: bool,
    out: &impl ChildOutput,
) -> io::Result<Child> {
    let mut c = platform::command(argv, words);
    if close_stdin {
        c.stdin(Stdio::null());
    }
    c.stdout(out.child_stdio()?).stderr(out.child_stdio()?);
    c.spawn()
}

/// Where `spawn_redirected` sends a child's output: each stream gets its own duplicate.
trait ChildOutput {
    fn child_stdio(&self) -> io::Result<Stdio>;
}

impl ChildOutput for File {
    fn child_stdio(&self) -> io::Result<Stdio> {
        Ok(self.try_clone()?.into())
    }
}

impl ChildOutput for io::PipeWriter {
    fn child_stdio(&self) -> io::Result<Stdio> {
        Ok(self.try_clone()?.into())
    }
}

/// `waitpid(pid, NULL, 0)`
fn waitpid(mut child: Child) {
    let _ = child.wait();
}

/// A thread that reads `r` to its end, sending each chunk read (Windows).
#[cfg(windows)]
fn drain(mut r: io::PipeReader) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; SIZE_8KB];
        while let Ok(n) = r.read(&mut buf)
            && n > 0
        {
            // Keep reading after the receiver is gone, so the writer never blocks.
            let _ = tx.send(buf[..n].to_vec());
        }
    });
    rx
}

/// Up to `max` bytes of what `drain` reads once the command has exited: all of its output,
/// or (when a program it started keeps the pipe open) what arrives within `OUTPUT_GRACE`.
#[cfg(windows)]
fn collect(rx: &mpsc::Receiver<Vec<u8>>, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < max {
        match rx.recv_timeout(OUTPUT_GRACE) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    out.truncate(max);
    out
}

/// How long `collect` waits for more output from a command that has exited.
#[cfg(windows)]
const OUTPUT_GRACE: Duration = Duration::from_millis(100);

impl Nnn {
    /// `create_tmp_file`: `mkstemp("<tmpdir>/.nnnXXXXXX")`; `None` for the original's -1.
    pub fn create_tmp_file(&mut self) -> Option<(File, Vec<u8>)> {
        let mut template = cstr(&self.g_tmpfpath).to_vec();
        template.extend_from_slice(STR_TMPFILE);
        mkstemp(&template).ok()
    }

    /// `xfork` plus the child's part of `spawn` (redirections, `enable_signals`,
    /// `execvp`). The parent ignores SIGWINCH until `join`: a resize while the child runs
    /// is not reported afterwards.
    fn xfork(&mut self, mut cmd: Command, flag: u16) -> Forked {
        if let Some(cs) = self.curses.as_mut() {
            self.popup.winch = Some(cs.resize_pending());
        }
        if flag & F_NOWAIT != 0 {
            // The grandchild is detached in its own process group (setsid() in the original).
            platform::detach(&mut cmd);
            if let Ok(mut child) = cmd.spawn() {
                std::thread::spawn(move || child.wait());
            }
            return Forked::Detached;
        }
        match cmd.spawn() {
            Ok(child) => Forked::Child(child),
            Err(_) => Forked::ExecFailed,
        }
    }

    /// `join`: waits for the child unless `F_NOWAIT`; its exit status, the raw wait
    /// status when it did not exit, 0xFFFF when not waited for.
    fn join(&mut self, p: Forked, flag: u16) -> i32 {
        let mut status = 0xFFFF;
        if flag & F_NOWAIT == 0 {
            match p {
                Forked::Child(mut child) => {
                    if let Ok(st) = child.wait() {
                        status = platform::wait_status(st);
                    }
                }
                Forked::ExecFailed => status = 0,
                Forked::Detached => {}
            }
        }
        // restore parent's signal handling
        if let (Some(was), Some(cs)) = (self.popup.winch.take(), self.curses.as_mut()) {
            cs.set_resize_pending(was);
        }
        status
    }
}

impl Nnn {
    /// `spawn`: runs `command` with up to 3 arguments (`F_MULTI`: `command` holds its own
    /// arguments). `F_NORMAL` leaves curses for the child and comes back after; `F_CONFIRM`
    /// (or `F_CHKRTN` on failure) waits for Enter first. Returns the child's status, 0xFFFF
    /// for an empty command or `F_NOWAIT`, -1 for too many arguments.
    pub fn spawn(
        &mut self,
        command: &[u8],
        arg1: Option<&[u8]>,
        arg2: Option<&[u8]>,
        arg3: Option<&[u8]>,
        flag: u16,
    ) -> i32 {
        if cstr(command).is_empty() {
            return 0xFFFF;
        }

        let mut argv = if flag & F_MULTI != 0 {
            match parseargs(command) {
                Some(a) => a,
                None => return -1,
            }
        } else {
            vec![cstr(command).to_vec()]
        };
        let words = argv.len();
        argv.extend([arg1, arg2, arg3].into_iter().flatten().map(<[u8]>::to_vec));
        self.spawn_argv(&argv, words, None, flag)
    }

    /// `spawn` of a ready `argv` (`argv[..words]` is the command, see `platform::command`),
    /// run in `cwd` when given: each file name stays one argument, however many there are.
    pub fn spawn_argv(
        &mut self,
        argv: &[Vec<u8>],
        words: usize,
        cwd: Option<&[u8]>,
        flag: u16,
    ) -> i32 {
        if flag & F_NORMAL != 0
            && let Some(cs) = self.curses.as_mut()
        {
            cs.endwin(); // exitcurses()
        }

        let mut cmd = platform::command(argv, words);
        if let Some(d) = cwd {
            cmd.current_dir(platform::path(d));
        }
        if flag & F_NOTRACE != 0 {
            // Suppress stdout and stderr
            if flag & F_NOSTDIN != 0 {
                cmd.stdin(Stdio::null());
            }
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        } else if flag & F_TTY != 0 && !io::stdout().is_terminal() {
            // If stdout has been redirected to a non-tty, force output to tty
            if let Ok(tty) = std::fs::OpenOptions::new()
                .write(true)
                .open(platform::path(platform::tty_out_device()))
            {
                cmd.stdout(Stdio::from(tty));
            }
        }

        let pid = self.xfork(cmd, flag);
        let retstatus = self.join(pid, flag);

        if flag & F_CONFIRM != 0 || (flag & F_CHKRTN != 0 && retstatus != 0) {
            let mut out = io::stdout();
            let _ = out.write_all(MSG_ENTER);
            let _ = out.flush();
            let mut c = [0u8; 1];
            while matches!(platform::read_stdin(&mut c), Ok(n) if n > 0) && c[0] != b'\n' {}
        }

        if flag & F_NORMAL != 0
            && let Some(cs) = self.curses.as_mut()
        {
            cs.resume();
            cs.refresh();
        }

        retstatus
    }

    /// `errexit()` (`printerr`) at nnn.c line `line`: leave curses, print the error, remove
    /// the selection file and exit 1.
    fn popup_errexit(&mut self, line: u32, e: &io::Error) -> ! {
        if let Some(cs) = self.curses.as_mut() {
            cs.endwin();
        }
        sys::xerror(line, e);
        if !self.g_state.picker
            && let Some(sp) = &self.selpath
        {
            sys::unlink(sp);
        }
        std::process::exit(1);
    }

    /// `get_output`:
    ///  1) `fdout` None, `!page`: up to `CMD_LEN_MAX - 1` bytes of output into `popup.g_buf`;
    ///  2) `fdout` None, `page`: the whole output into a temp file shown in the pager;
    ///  3) `fdout` given, `!page`: the whole output into `fdout`;
    ///  4) `fdout` given, `page`: refused (false).
    pub fn get_output(
        &mut self,
        command: &[u8],
        arg1: Option<&[u8]>,
        arg2: Option<&[u8]>,
        fdout: Option<&File>,
        page: bool,
    ) -> bool {
        if cstr(command).is_empty() {
            return false;
        }
        if fdout.is_some() && page {
            return false;
        }

        // Set up file descriptors for the child command
        let mut tmp: Option<(File, Vec<u8>)> = None;
        let mut pipe: Option<(io::PipeReader, io::PipeWriter)> = None;
        if page {
            // Case 2
            match self.create_tmp_file() {
                Some(t) => tmp = Some(t),
                None => return false,
            }
        } else if fdout.is_none() {
            // Case 1
            let (r, w) = match io::pipe() {
                Ok(p) => p,
                Err(e) => self.popup_errexit(5493, &e),
            };
            #[cfg(unix)]
            platform::set_pipe_nonblocking(&r, &w);
            pipe = Some((r, w));
        }

        // Tokenize the command into an array
        let Some(mut argv) = parseargs(command) else {
            return false;
        };
        let words = argv.len();
        argv.extend([arg1, arg2].into_iter().flatten().map(<[u8]>::to_vec));

        let spawned = match (&tmp, fdout, &pipe) {
            (Some((f, _)), _, _) => spawn_redirected(&argv, words, true, f),
            (None, Some(f), _) => spawn_redirected(&argv, words, true, f),
            (None, None, Some((_, w))) => spawn_redirected(&argv, words, false, w),
            // INFALLIBLE: one of the three cases set an output above.
            (None, None, None) => return false,
        };
        let Ok(child) = spawned else {
            return false;
        };

        // Windows has no non-blocking anonymous pipes: only the child holds the write end
        // now, so the reader sees EOF when it is done; it reads while the child runs so a
        // full pipe cannot stop the child.
        #[cfg(windows)]
        let output = pipe.take().map(|(r, w)| {
            drop(w);
            drain(r)
        });

        // In parent
        waitpid(child);

        if let Some((f, path)) = tmp {
            // Case 2
            drop(f);
            self.spawn(&self.pager.clone(), Some(&path), None, None, F_CLI | F_TTY);
            sys::unlink(&path);
            return true;
        }

        if fdout.is_some() {
            return true; // Case 3
        }

        // Case 1
        let mut ret = false;
        #[cfg(unix)]
        if let Some((mut r, w)) = pipe {
            let mut buf = vec![0u8; CMD_LEN_MAX - 1];
            if let Ok(n) = r.read(&mut buf)
                && n > 0
            {
                buf.truncate(n);
                self.popup.g_buf = buf;
                ret = true;
            }
            drop(w);
        }
        #[cfg(windows)]
        if let Some(rx) = output {
            let buf = collect(&rx, CMD_LEN_MAX - 1);
            if !buf.is_empty() {
                self.popup.g_buf = buf;
                ret = true;
            }
        }
        ret
    }

    /// Case 1 of `get_output` as a value: the bytes the command printed, `None` for FALSE.
    pub fn get_output_buf(
        &mut self,
        command: &[u8],
        arg1: Option<&[u8]>,
        arg2: Option<&[u8]>,
    ) -> Option<Vec<u8>> {
        self.get_output(command, arg1, arg2, None, false)
            .then(|| self.popup.g_buf.clone())
    }

    /// `buffer_command_output`: runs `cmds` last to first, each with `arg1` and `arg2`,
    /// their output (stdout and stderr) together in one buffer of at most 16 MB (plus the last
    /// read). `None` when nothing was printed or the pipe failed.
    pub fn buffer_command_output(
        &mut self,
        cmds: &[&[u8]],
        arg1: Option<&[u8]>,
        arg2: Option<&[u8]>,
    ) -> Option<Vec<u8>> {
        #[cfg(unix)]
        let (mut r, w) = io::pipe().ok()?;
        #[cfg(windows)]
        let (output, w) = {
            // Read while the commands run, so a full (small) pipe cannot stop them.
            let (r, w) = io::pipe().ok()?;
            (drain(r), w)
        };

        // Execute all commands sequentially, writing to the pipe
        for command in cmds.iter().rev() {
            if cstr(command).is_empty() {
                continue;
            }
            let Some(mut argv) = parseargs(command) else {
                continue;
            };
            let words = argv.len();
            argv.extend([arg1, arg2].into_iter().flatten().map(<[u8]>::to_vec));
            if let Ok(child) = spawn_redirected(&argv, words, false, &w) {
                waitpid(child);
            }
        }

        drop(w);

        // Read directly from the pipe into the buffer
        let mut content = Vec::with_capacity(SIZE_8KB);
        #[cfg(unix)]
        {
            let mut read_buf = [0u8; SIZE_8KB];
            while content.len() < SIZE_16MB {
                match r.read(&mut read_buf) {
                    Ok(n) if n > 0 => content.extend_from_slice(&read_buf[..n]),
                    _ => break,
                }
            }
        }
        #[cfg(windows)]
        while content.len() < SIZE_16MB {
            match output.recv_timeout(OUTPUT_GRACE) {
                Ok(chunk) => content.extend_from_slice(&chunk),
                Err(_) => break,
            }
        }

        (!content.is_empty()).then_some(content)
    }

    /// `show_content_in_floating_window`: `content` in a bordered window (3/4 of the width,
    /// up to 20 lines) with scrolling; with `perfile` the title shows the hovered entry and
    /// n/j, p/k set `action` to `Next`/`Prev` and close it. False when the window does not
    /// fit on the screen (`newwin` fails).
    pub fn show_content_in_floating_window(
        &mut self,
        content: &[u8],
        mut action: Option<&mut Option<Action>>,
        perfile: bool,
    ) -> bool {
        let (win_height, win_width, start_y, start_x) = floating_geometry(self.xlines, self.xcols);
        if !newwin_fits(win_height, win_width, start_y, start_x) {
            return false;
        }
        let mut win = self.cs().newwin(win_height, win_width, start_y, start_x);

        let (line_count, max_line_width) = measure(content);
        let mut fr = Frame {
            content,
            perfile,
            title: None,
            line_count,
            max_line_width,
            scroll_offset: 0,
            hscroll_offset: 0,
        };

        loop {
            let title = (perfile && self.ndents != 0).then(|| {
                (
                    (self.cur + 1) as u32,
                    self.ndents as u32,
                    self.pdents[self.cur as usize].name.clone(),
                )
            });
            fr.title = title;
            paint_floating(&mut win, &fr);
            let cs = self.cs();
            cs.wrefresh(&win);

            // Get user input from window
            let ch = match cs.get_wch() {
                None => continue,
                Some(Input::Char(c) | Input::Key(c)) => c,
            };

            match float_key(&mut fr, win_height, win_width, ch, action.is_some()) {
                FloatKey::Stay => {}
                FloatKey::Close => break,
                FloatKey::Move(a) => {
                    if let Some(out) = action.as_deref_mut() {
                        *out = Some(a);
                    }
                    break;
                }
            }
        }

        let cs = self.cs();
        cs.delwin(&win);
        cs.refresh(); // Refresh main screen
        true
    }

    /// `show_stats`: `stat`, `file -b` and `file -biL` of the hovered entry in the floating
    /// window; n/p there move to the next/previous entry and show it. `pathbuf` gets the
    /// entry's path.
    pub fn show_stats(&mut self, pathbuf: &mut Vec<u8>, dir: &[u8]) -> bool {
        #[cfg(unix)]
        let mime = format!("file {FILE_MIME_OPTS}");
        #[cfg(unix)]
        let cmds: [&[u8]; 3] = [mime.as_bytes(), b"file -b", b"stat"];
        loop {
            *pathbuf = mkpath(&self.home, dir, &self.pdents[self.cur as usize].name);

            #[cfg(unix)]
            let Some(content) = self.buffer_command_output(&cmds, Some(pathbuf), None) else {
                return false;
            };
            // Windows has no stat or file: the entry's details come from winops.
            #[cfg(windows)]
            let content = crate::winops::file_info_text(pathbuf);

            let mut action: Option<Action> = None; // SEL_MAX
            if !self.show_content_in_floating_window(&content, Some(&mut action), true) {
                return false;
            }

            let Some(sel) = action else {
                return true;
            };
            if !self.handle_cur_move(sel) {
                return true;
            }
        }
    }

    /// `xchmod`: toggles the execute bits (all three, after the owner's) of the hovered entry
    /// (not following a symlink); `pathbuf` gets its path.
    pub fn xchmod(&mut self, pathbuf: &mut Vec<u8>, dir: &[u8]) -> bool {
        *pathbuf = mkpath(&self.home, dir, &self.pdents[self.cur as usize].name);
        match platform::toggle_exec(pathbuf) {
            Some(mode) => {
                self.pdents[self.cur as usize].mode = mode;
                true
            }
            None => false,
        }
    }

    /// `lock_terminal`: runs `$NNN_LOCKER` (default vlock).
    pub fn lock_terminal(&mut self) {
        let locker = xgetenv("NNN_LOCKER", Some(UTIL_LOCKER)).unwrap_or_default();
        self.spawn(&locker, None, None, None, F_CLI);
    }

    /// The text `show_help` writes after `$NNN_HELP`'s output.
    pub fn help_text(&mut self, path: &[u8]) -> Vec<u8> {
        let mut f = expand_helpstr();

        if !self.popup.curssn.is_empty() {
            f.extend_from_slice(b"\nSESSION: ");
            f.extend_from_slice(&self.popup.curssn);
            f.push(b'\n');
        }

        f.extend_from_slice(b"\nCONTEXTS\n");
        for (i, ctx) in self.g_ctx.iter().enumerate() {
            if ctx.c_cfg.ctxactive {
                let star = if self.cfg.curctx == i { '*' } else { ' ' };
                f.extend_from_slice(format!(" {}{star} ", i + 1).as_bytes());
                f.extend_from_slice(cstr(&ctx.c_path));
                f.push(b'\n');
            }
        }

        let size = |ty| fmt::coolsize(get_fs_info(path, ty) as i64);
        f.extend_from_slice(format!("\nVOLUME: avail:{} ", size(VFS_AVAIL)).as_bytes());
        f.extend_from_slice(format!("used:{} ", size(VFS_USED)).as_bytes());
        f.extend_from_slice(format!("size:{}\n\n", size(VFS_SIZE)).as_bytes());

        if !self.bookmark.is_empty() || self.mark.is_some() {
            f.extend_from_slice(b"BOOKMARKS\n");
            printkv(&self.bookmark, &mut f);
            if let Some(mark) = &self.mark {
                f.extend_from_slice(b" ,: ");
                f.extend_from_slice(mark);
                f.push(b'\n');
            }
            f.push(b'\n');
        }

        self.lazy_parse_plug();
        if !self.plug.is_empty() {
            f.extend_from_slice(b"PLUGIN KEYS\n");
            printkv(&self.plug, &mut f);
            f.push(b'\n');
        }

        for name in ENV_CFG_SHOWN {
            if let Some(s) = std::env::var_os(name) {
                f.extend_from_slice(name.as_bytes());
                f.extend_from_slice(b": ");
                f.extend_from_slice(&crate::platform::bytes(&s));
                f.push(b'\n');
            }
        }

        if let Some(sp) = &self.selpath {
            f.extend_from_slice(b"SELECTION FILE: ");
            f.extend_from_slice(sp);
            f.push(b'\n');
        }

        f.extend_from_slice(format!("\nv{VERSION}\n{GENERAL_INFO}\n").as_bytes());
        f
    }

    /// `show_help`: `$NNN_HELP`'s output, the key list, sessions, contexts, volume,
    /// bookmarks, plugins, settings and version in a temp file shown by the pager.
    pub fn show_help(&mut self, path: &[u8]) {
        let Some((file, tmpf)) = self.create_tmp_file() else {
            return;
        };

        if let Some(prog) = xgetenv("NNN_HELP", None) {
            self.get_output(&prog, None, None, Some(&file), false);
        }

        let text = self.help_text(path);
        // The child wrote through the same open file, so this goes after its output.
        let _ = (&file).write_all(&text);
        drop(file);

        self.spawn(&self.pager.clone(), Some(&tmpf), None, None, F_CLI | F_TTY);
        sys::unlink(&tmpf);
    }

    /// `handle_openwith`: asks GUI or CLI, then runs `tmp` (the "open with" program) on
    /// path/name, which goes into `newpath`.
    pub fn handle_openwith(&mut self, path: &[u8], name: &[u8], newpath: &mut Vec<u8>, tmp: &[u8]) {
        // Confirm if app is CLI or GUI
        let r = self.get_input(Some(MSG_CLI_MODE));

        let r = if r == i32::from(b'c') {
            F_CLI
        } else if r == i32::from(b'g') || r == i32::from(b'\r') {
            F_NOWAIT | F_NOTRACE | F_MULTI
        } else {
            0
        };
        if r != 0 {
            *newpath = mkpath(&self.home, path, name);
            let np = newpath.clone();
            self.spawn(tmp, Some(&np), None, None, r);
        }
    }

    /// `opener_flags` of browse(): how the opener is run for a regular file.
    pub fn opener_flags(&self) -> u16 {
        if self.cfg.cliopener {
            F_CLI
        } else {
            F_NOTRACE | F_NOSTDIN | F_NOWAIT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Option<Vec<Vec<u8>>> {
        Some(v.iter().map(|s| s.as_bytes().to_vec()).collect())
    }

    /// Case 1 of get_output with `cat`/`type` of a file through the shell prefix.
    fn output_of_file(tag: &str, content: &[u8]) -> Option<Vec<u8>> {
        let f = std::env::temp_dir().join(format!("nnn-get-output-{tag}-{}", std::process::id()));
        std::fs::write(&f, content).unwrap();
        let prefix = platform::shell_exec_prefix();
        let line = if prefix == b"cmd /C" {
            format!("type {}", f.display())
        } else {
            format!("cat {}", f.display().to_string().replace('\\', "/"))
        };
        let mut n = Nnn::default();
        let out = n.get_output_buf(prefix, Some(line.as_bytes()), None);
        let _ = std::fs::remove_file(&f);
        out
    }

    #[test]
    fn get_output_without_output_returns_false() {
        assert_eq!(output_of_file("empty", b""), None);
    }

    #[test]
    fn get_output_keeps_the_start_of_a_big_output() {
        // More than any pipe holds: the child must not block on a full pipe.
        let out = output_of_file("big", &vec![b'x'; 1 << 20]).unwrap();
        assert_eq!(out, vec![b'x'; CMD_LEN_MAX - 1]);
    }

    #[test]
    fn parseargs_splits_like_nnn() {
        assert_eq!(parseargs(b"sh -c"), args(&["sh", "-c"]));
        // A blank right after a blank is kept at the start of the next argument.
        assert_eq!(parseargs(b"a  b"), args(&["a", " b"]));
        assert_eq!(parseargs(b"a\t\t\tb"), args(&["a", "\t", "b"]));
        assert_eq!(parseargs(b"a "), args(&["a"]));
        assert_eq!(parseargs(b" a"), args(&["", "a"]));
        assert_eq!(
            parseargs(b"1 2 3 4 5 6"),
            args(&["1", "2", "3", "4", "5", "6"])
        );
        assert_eq!(parseargs(b"1 2 3 4 5 6 7"), None);
        assert_eq!(parseargs(b"1 2 3 4 5 6 7 8 9 10 11"), None);
    }

    #[test]
    fn mkpath_joins() {
        assert_eq!(mkpath(b"/h", b"/d", b"n"), b"/d/n");
        assert_eq!(mkpath(b"/h", b"/", b"n"), b"/n");
        assert_eq!(mkpath(b"/h", b"/d", b"/abs"), b"/abs");
        assert_eq!(mkpath(b"/h", b"/d", b"~/x"), b"/h/x");
    }

    #[test]
    fn helpstr_expands() {
        let h = expand_helpstr();
        let text = String::from_utf8(h).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "  |V\\_");
        assert_eq!(lines[1], "  /. \\\\");
        assert_eq!(lines[5], "");
        assert_eq!(lines[6], " NAVIGATION");
        assert_eq!(
            lines[7],
            "         Up k  Prev              PgUp ^U  Page up"
        );
        assert_eq!(
            lines[16],
            "      2Esc ^Q  Quit                   ^y  Next young"
        );
        assert_eq!(
            lines.last().copied(),
            Some("           ^L  Redraw                  ?  Help, conf")
        );
    }

    #[test]
    fn xchartohex_maps() {
        assert_eq!(xchartohex(b'9'), 9);
        assert_eq!(xchartohex(b'c'), 12);
        assert_eq!(xchartohex(b'F'), 15);
        assert_eq!(xchartohex(b'z'), b'z');
    }

    #[test]
    fn measure_counts_lines_and_width() {
        assert_eq!(measure(b"ab\ncde\n"), (2, 3));
        assert_eq!(measure(b"ab\ncde"), (2, 3));
        assert_eq!(measure(b""), (0, 0));
        // strchr stops at a NUL: the rest is one line
        assert_eq!(measure(b"a\0b\nc"), (2, 5));
    }

    /// The screen of the "f" step in rust/tui-cases/expected/file-info.json.
    fn expected_file_info() -> Vec<String> {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tui-cases/expected/file-info.json"
        ))
        .unwrap();
        let at = json.find("\"after\": \"f\"").unwrap();
        let rest = &json[at..];
        let start = rest.find('[').unwrap();
        let end = rest.find(']').unwrap();
        rest[start + 1..end]
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('"'))
            .map(|l| l.trim_end_matches(',').trim_matches('"').to_string())
            .collect()
    }

    #[test]
    fn file_info_popup_matches_the_original() {
        let content: &[u8] = b"inode/directory; charset=binary\n\
directory\n  File: /tmp/w/work/dir1\n  Size: 60        \tBlocks: 0          IO Block: 4096   directory\n\
Device: 8,48\tInode: 12345       Links: 2\n\
Access: (0755/drwxr-xr-x)  Uid: ( 1000/     bam)   Gid: ( 1000/     bam)\n\
Access: 2023-11-14 22:13:20.000000000 +0000\n\
Modify: 2023-11-14 22:13:20.000000000 +0000\n\
Change: 2026-11-23 20:00:00.000000000 +0000\n\
 Birth: 2026-11-23 20:00:00.000000000 +0000\n";
        // stat runs first (the last command), then file -b, then file -biL
        let lines: Vec<&[u8]> = content.split(|&c| c == b'\n').collect();
        let mut ordered = Vec::new();
        for l in lines[2..10].iter().chain([&lines[1], &lines[0]]) {
            ordered.extend_from_slice(l);
            ordered.push(b'\n');
        }
        let (h, w, y, x) = floating_geometry(24, 80);
        assert_eq!((h, w, y, x), (20, 60, 2, 10));
        let mut win = Window::new(h, w, y, x);
        let (line_count, max_line_width) = measure(&ordered);
        paint_floating(
            &mut win,
            &Frame {
                content: &ordered,
                perfile: true,
                title: Some((2, 11, b"dir1".to_vec())),
                line_count,
                max_line_width,
                scroll_offset: 0,
                hscroll_offset: 0,
            },
        );
        let screen = expected_file_info();
        let mut compared = 0;
        for row in 0..h {
            let want: String = screen[(y + row) as usize]
                .chars()
                .skip(x as usize)
                .collect();
            if want.contains('<') {
                continue; // normalized by the harness (ADR-010)
            }
            let got = win.row_text(row);
            let got = got.trim_end();
            assert_eq!(got, want.trim_end(), "row {row}");
            compared += 1;
        }
        assert!(compared >= 15, "{compared}");
        assert!(win.row_text(4).starts_with("dDevice: 8,48 "));
    }
}
