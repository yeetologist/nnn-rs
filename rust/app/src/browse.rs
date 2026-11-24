//! The main loop (browse), the TUI part of main() (startup and shutdown), initcurses, the
//! FIFO, the inotify watch and the preview pane.
//!
//! Conventions (shared with draw.rs): nnn.c's browse() keeps the pointers path, lastdir and
//! lastname into the current context, so they are `self.g_ctx[self.cfg.curctx]`'s c_path,
//! c_last and c_name. The labels browse() jumps to are the variants of [`Flow`]; its `break`
//! and `continue` go to `Flow::Top`.

use crate::config::{
    self, NNN_BMS, NNN_ORDER, NNN_PLUG, bmtarget, get_cwd_entry, get_kv_key, get_kv_val,
    is_bad_len_or_dir, mkpath, parsekvpair, xbasename, xdirname, xmktree,
};
use crate::curses::{
    self, A_DIM, BUTTON1_PRESSED, BUTTON2_PRESSED, BUTTON3_PRESSED, BUTTON4_PRESSED,
    BUTTON5_PRESSED, Curses, MouseEvent,
};
use crate::fmt::coolsize;
use crate::keys::Action;
use crate::locale::Regex;
use crate::opts::Opts;
use crate::platform::{self, Access, Errno};
use crate::popup::{
    F_CLI, F_CONFIRM, F_MULTI, F_NORMAL, F_NOTRACE, F_NOWAIT, F_SILENT, xchartohex, xgetenv,
};
use crate::prompt::xconfirm;
use crate::sort::{DIR_OR_DIRLNK, Entry, FILE_SELECTED, TimeType};
use crate::state::{CTX_MAX, NAME_MAX, Nnn, PATH_MAX};
use crate::sys;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// `FILTER`
pub const FILTER: i32 = b'/' as i32;
/// `RFILTER`
pub const RFILTER: i32 = b'\\' as i32;
/// `MSGWAIT`
pub const MSGWAIT: i32 = b'$' as i32;
/// `SELECT`
pub const SELECT: i32 = b' ' as i32;
/// `ESC`
pub const ESC: i32 = 27;
/// `CREATE_NEW_KEY`: pkey when nnn starts on a file that does not exist.
pub const CREATE_NEW_KEY: i32 = -1;
/// `CTX_INVALID`
pub const CTX_INVALID: usize = 0b111;
/// `LARGESEL`
const LARGESEL: i32 = 1000;
/// `RM_RECONFIRM`
const RM_RECONFIRM: i32 = 10;
/// `DBLCLK_INTERVAL_NS`
const DBLCLK_INTERVAL_NS: i64 = 400_000_000;
/// `BLK_SHIFT_512`
pub const BLK_SHIFT_512: u32 = 9;
/// `MSG_ZERO`
const MSG_ZERO: i32 = 0;

/// `CONTROL(c)` (nnn.h).
const fn control(c: u8) -> i32 {
    (c & 0x1f) as i32
}

/// `C_BLK`..`C_UND`: the file color pairs after the contexts' pairs.
pub const C_BLK: usize = CTX_MAX + 1;
pub const C_UND: usize = C_BLK + 11;
/// `gcolors`: the default NNN_FCOLORS.
const GCOLORS: &[u8] = b"c1e2272e006033f7c6d6abc4";
/// `COLOR_256`
const COLOR_256: i32 = 256;

/// `MIN_PREVIEW_COLS`
const MIN_PREVIEW_COLS: i32 = 40;
/// `PREVIEW_MAX_LINE`
const PREVIEW_MAX_LINE: usize = 4096;

/// `FILE_MIME_OPTS` on Linux.
const FILE_MIME_OPTS: &[u8] = b"-biL";

// utils[] entries this module uses.
const UTIL_BASH: &[u8] = b"bash";
const UTIL_NTFY: &[u8] = b".ntfy";
const UTIL_CBCP: &[u8] = b".cbcp";
const UTIL_NMV: &[u8] = b".nmv";
const UTIL_TRASH_CLI: &[u8] = b"trash-put";
const UTIL_GIO_TRASH: &[u8] = b"gio trash";
const UTIL_NPREVIEW: &[u8] = b".npreview";
/// `patterns[P_ARCHIVE]`
const P_ARCHIVE: &[u8] = b"\\.(bz|bz2|gz|tar|taz|tbz|tbz2|tgz|tzst|z|zip|zst)$";

// messages[] indexes.
const MSG_0_SELECTED: usize = 3;
pub(crate) const MSG_FAILED: usize = 5;
const MSG_SSN_NAME: usize = 6;
const MSG_NEW_OPTS: usize = 11;
const MSG_OVERWRITE: usize = 13;
const MSG_SSN_OPTS: usize = 14;
const MSG_QUIT_ALL: usize = 15;
const MSG_ARCHIVE_NAME: usize = 17;
const MSG_OPEN_WITH: usize = 18;
const MSG_NEW_PATH: usize = 19;
const MSG_LINK_PREFIX: usize = 20;
const MSG_COPY_NAME: usize = 21;
const MSG_EMPTY_FILE: usize = 25;
const MSG_UNSUPPORTED: usize = 26;
const MSG_NOT_SET: usize = 27;
const MSG_EXISTS: usize = 28;
const MSG_ARCHIVE_OPTS: usize = 33;
const MSG_INVALID_REG: usize = 35;
const MSG_ORDER: usize = 36;
const MSG_INVALID_KEY: usize = 40;
const MSG_NOCHANGE: usize = 41;
const MSG_DIR_CHANGED: usize = 42;
/// Appended after the original's messages (Windows).
pub(crate) const MSG_NOT_SUPPORTED: usize = 45;

/// `messages[]`
pub const MESSAGES: [&[u8]; 46] = [
    b"",
    b"0 entries",
    b"/.nnnXXXXXX",
    b"0 selected",
    b"cancelled",
    b"failed!",
    b"session name: ",
    b"'c'p/'m'v as?",
    b"'c'urrent/'s'el?",
    b"file limit exceeded",
    b"size limit exceeded",
    b"['f'ile]/'d'ir/'s'ym/'h'ard?",
    b"['g'ui]/'c'li?",
    b"overwrite?",
    b"'s'ave/'l'oad/'r'estore?",
    b"Quit all contexts?",
    b"remote name (- for hovered): ",
    b"archive [path/]name: ",
    b"open with: ",
    b"[path/]name: ",
    b"link prefix [@ for none]: ",
    b"copy [path/]name: ",
    b"\n'Enter' to continue",
    b"open failed",
    b"dir inaccessible",
    b"empty! edit/open with",
    b"?",
    b"not set",
    b"entry exists",
    b"too few cols!",
    b"'s'shfs/'r'clone?",
    b"refresh if slow",
    b"app: ",
    b"['l's]/'o'pen/e'x'tract/'m'nt?",
    b"keys:",
    b"invalid regex",
    b"'a'u/'d'u/'e'xt/'r'ev/'s'z/'t'm/'v'er/'c'lr/'^T'?",
    b"unmount failed! try lazy?",
    b"first file (\')/char?",
    b"remove tmp file?",
    b"invalid key",
    b"unchanged",
    b"dir changed, range sel off",
    b"name: ",
    b"add files with relative path?",
    b"not supported on Windows",
];

/// State this module keeps: the globals of nnn.c that only main(), browse() and the
/// FIFO/inotify/preview code use.
#[derive(Default)]
pub struct BrowseState {
    /// `inotify_fd` and `inotify_wd`.
    pub dir_watch: Option<platform::DirWatch>,
    /// `fifofd` (`None` is -1).
    pub fifofd: Option<std::fs::File>,
    /// notify_fifo()'s `static struct entry lastentry`.
    lastentry: Option<Entry>,
    /// `archive_re`
    pub archive_re: Option<Regex>,
    /// `previewer`: the .npreview plugin path, once looked up.
    pub previewer: Option<Vec<u8>>,
    /// `fcolors[]` in full: [1..=CTX_MAX] are also in `Nnn::fcolors` (which has only
    /// CTX_MAX + 1 slots); the file colors C_BLK..=C_UND are only here.
    pub fcolors: [u32; C_UND + 1],
    /// Set by SIGINT (`sigint_handler`); browse() moves it into `g_state.interrupt`.
    pub sigint: Arc<AtomicBool>,
    /// Set by SIGTERM/SIGHUP (`clean_exit_sighandler`).
    pub sigterm: Arc<AtomicBool>,
    /// The -f option (`hist_file` in main).
    pub hist_file: bool,
    /// The -0 option (`sepnul` in main).
    pub sepnul: bool,
    /// `homelen`
    pub homelen: usize,
    /// The terminal's number of colors (`COLORS`).
    pub colors: i32,
}

/// Where browse() goes next: its labels, and the loop's `continue`/`break` (Top).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// `begin:`
    Begin,
    /// Top of `while (1)`: redraw, then wait for a key.
    Top,
    /// `nochange:`
    Nochange,
    /// `cdfalse_begin:`
    CdfalseBegin,
    /// `setwatch_begin:`
    SetwatchBegin,
    /// `warn_nochange:`
    WarnNochange,
    /// `msgwait_nochange:`
    MsgwaitNochange,
    /// `statusbar_nochange:`
    StatusbarNochange,
    /// `return` from browse().
    Return(u8),
}

/// browse()'s locals.
struct Locals {
    ipath: Vec<u8>,
    newpath: Vec<u8>,
    runfile: Vec<u8>,
    presel: i32,
    pkey: i32,
    selstartid: i32,
    selendid: i32,
    watch: bool,
    cd: bool,
    inode: u64,
    mousetimings: [(i64, i64); 2],
    mousedent: [i32; 2],
    currentmouse: usize,
    rightclicksel: bool,
    clock: Instant,
}

use crate::platform::path as os;

/// `chdir() == 0`
fn chdir(p: &[u8]) -> bool {
    std::env::set_current_dir(os(p)).is_ok()
}

/// `access(p, F_OK) == 0`
fn exists(p: &[u8]) -> bool {
    std::fs::metadata(os(p)).is_ok()
}

/// `xstrsncpy(dst, src, n)`: what fits in a buffer of `n` bytes with its NUL.
fn ncpy(src: &[u8], n: usize) -> Vec<u8> {
    let end = src.iter().position(|&c| c == 0).unwrap_or(src.len());
    src[..end.min(n.saturating_sub(1))].to_vec()
}

fn is_lnk(mode: u32) -> bool {
    mode & platform::S_IFMT == 0o120000
}

/// `getenv`
fn getenv(name: &str) -> Option<Vec<u8>> {
    std::env::var_os(name).map(std::ffi::OsString::into_encoded_bytes)
}

/// `xgetenv(name, fallback)` with a fallback that is always there.
fn xgetenv_or(name: &str, fallback: &[u8]) -> Vec<u8> {
    xgetenv(name, Some(fallback)).unwrap_or_default()
}

/// `msg`: a line on stderr.
fn msg(m: &[u8]) {
    let mut e = std::io::stderr();
    let _ = e.write_all(m);
    let _ = e.write_all(b"\n");
}

/// `xitoa` of a flag.
fn xitoa(v: bool) -> &'static [u8] {
    if v { b"1" } else { b"0" }
}

/// `xdiraccess`: the path opens as a directory.
pub fn xdiraccess(p: &[u8]) -> bool {
    platform::Dir::open(p).is_ok()
}

/// `xextension(fname, len)`: from the last '.', if any.
pub fn xextension(fname: &[u8]) -> Option<&[u8]> {
    fname
        .iter()
        .rposition(|&c| c == b'.')
        .map(|at| &fname[at..])
}

/// `get_cwd_entry` as the name it points at, cut at NAME_MAX.
fn cwd_entry(cwdpath: &[u8], entrypath: &[u8]) -> Option<Vec<u8>> {
    get_cwd_entry(cwdpath, entrypath)
        .map(|(start, len)| ncpy(&config::cstr(entrypath)[start..start + len], len + 1))
}

/// `is_text_file`: the first 512 bytes look like text.
pub fn is_text_file(fpath: &[u8]) -> bool {
    let Ok(mut f) = std::fs::File::open(os(fpath)) else {
        return false;
    };
    let mut buf = [0u8; 512];
    let n = loop {
        match f.read(&mut buf) {
            Ok(n) => break n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    };
    if n == 0 {
        return false;
    }
    const MAGICS: &[&[u8]] = &[
        b"%PDF",
        b"\x89PNG",
        b"GIF8",
        b"\xFF\xD8\xFF",
        b"PK\x03\x04",
        b"\x7FELF",
        b"\x1F\x8B",
        b"BM",
        b"RIFF",
        b"OggS",
        b"fLaC",
        b"ID3",
        b"\xFE\xED\xFA",
        b"\xFD7zXZ\x00",
        b"7z\xBC\xAF\x27\x1C",
        b"SQLite",
    ];
    let buf = &buf[..n];
    if MAGICS.iter().any(|m| buf.starts_with(m)) {
        return false;
    }
    // MP3 sync word.
    if n >= 2 && buf[0] == 0xFF && (buf[1] & 0xE0) == 0xE0 {
        return false;
    }
    !buf.iter()
        .any(|&c| c == 0 || (c < 0x20 && !matches!(c, b'\n' | b'\r' | b'\t' | 0x0c | 0x1b)))
}

/// `fgets(line, PREVIEW_MAX_LINE, fp)`: up to 4095 bytes, through the next newline.
fn fgets(r: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    while line.len() < PREVIEW_MAX_LINE - 1 {
        let buf = r.fill_buf().ok()?;
        if buf.is_empty() {
            break;
        }
        let take = buf.len().min(PREVIEW_MAX_LINE - 1 - line.len());
        if let Some(nl) = buf[..take].iter().position(|&c| c == b'\n') {
            line.extend_from_slice(&buf[..=nl]);
            r.consume(nl + 1);
            return Some(line);
        }
        line.extend_from_slice(&buf[..take]);
        r.consume(take);
    }
    (!line.is_empty()).then_some(line)
}

/// `COLORS` as terminfo gives it, from the terminal's name.
fn term_colors() -> i32 {
    let term = getenv("TERM").unwrap_or_default();
    let has = |s: &[u8]| term.windows(s.len()).any(|w| w == s);
    if has(b"256color") || has(b"direct") {
        256
    } else if has(b"88color") {
        88
    } else {
        8
    }
}

/// `_ABSSUB`
fn abssub(a: i64, b: i64) -> i64 {
    if a > b { a - b } else { b - a }
}

/// The context colors of NNN_COLORS as initcurses() reads them; Err when an extended (#)
/// code lacks its second hex digit. `fcolors[i + 1]` is set for each code given.
pub fn parse_ctx_colors(
    colors: Option<&[u8]>,
    oldcolor: bool,
    ncolors: i32,
    fcolors: &mut [u32],
) -> Option<[u32; CTX_MAX]> {
    let mut colors: Option<Vec<u8>> = colors.map(|c| config::cstr(c).to_vec());
    let mut ext = false;
    if let Some(c) = colors.clone()
        && c.first() == Some(&b'#')
    {
        let sep = c.iter().position(|&x| x == b';');
        if !oldcolor && ncolors >= COLOR_256 {
            // The 8-color fallback after ';' is not read.
            let mut v = c[1..].to_vec();
            if let Some(s) = sep {
                v.truncate(s - 1);
            }
            colors = Some(v);
            ext = true;
        } else {
            colors = sep.map(|s| c[s + 1..].to_vec());
        }
    }
    let c = colors.unwrap_or_default();
    let mut pos = 0;
    let mut out = [4u32; CTX_MAX];
    for (i, code) in out.iter_mut().enumerate() {
        if pos < c.len() {
            if ext {
                *code = u32::from(xchartohex(c[pos])) << 4;
                pos += 1;
                if pos < c.len() {
                    *code += u32::from(xchartohex(c[pos]));
                    fcolors[i + 1] = *code;
                } else {
                    // Each color code must be 2 hex symbols.
                    return None;
                }
            } else {
                *code = if (b'0'..=b'7').contains(&c[pos]) {
                    u32::from(c[pos] - b'0')
                } else {
                    4
                };
                fcolors[i + 1] = *code;
            }
            pos += 1;
        }
    }
    Some(out)
}

/// `init_fcolors` without its init_pair calls: the file colors of NNN_FCOLORS (or the
/// defaults); `false` when a code lacks its second digit.
pub fn parse_fcolors(f_colors: &[u8], fcolors: &mut [u32]) -> bool {
    let f = config::cstr(f_colors);
    let f = if f.is_empty() { GCOLORS } else { f };
    let mut pos = 0;
    let mut id = C_BLK;
    while pos < f.len() && id <= C_UND {
        fcolors[id] = u32::from(xchartohex(f[pos])) << 4;
        pos += 1;
        if pos < f.len() {
            fcolors[id] += u32::from(xchartohex(f[pos]));
        } else {
            return false;
        }
        pos += 1;
        id += 1;
    }
    true
}

/// The NNN_FIFO checks of main(): create it (one that exists is fine); it must be a FIFO
/// we may write to. Err holds the exit code, after the error message.
fn make_fifo(fp: &[u8]) -> Result<(), u8> {
    if let Err(e) = platform::mkfifo(fp)
        && !platform::is_errno(&e, Errno::Exist)
    {
        sys::xerror(10910, &e);
        return Err(1);
    }
    match platform::stat(fp, false) {
        Err(e) => {
            sys::xerror(10914, &e);
            return Err(1);
        }
        Ok(sb) if sb.mode & platform::S_IFMT != platform::S_IFIFO => {
            sys::xerror(10919, &platform::error(Errno::Inval));
            return Err(1);
        }
        Ok(_) => {}
    }
    if let Err(e) = platform::access(fp, Access::Write) {
        sys::xerror(10923, &e);
        return Err(1);
    }
    Ok(())
}

/// What `tui_main` needs from crate::startup.
pub struct TuiArgs<'a> {
    /// The parsed options (NNN_OPTS, then argv).
    pub opts: &'a Opts,
    /// The operands left after the options (`argv[optind..]`).
    pub operands: &'a [Vec<u8>],
    /// `initpath` as load_input() set it in list mode (stdin is the tty again by then).
    pub initpath: Option<Vec<u8>>,
}

/// The TUI part of `main()`: from the HOME check to `return opt`, then the original's
/// `atexit(cleanup)`. Returns the exit code.
pub fn tui_main(nnn: &mut Nnn, args: TuiArgs<'_>) -> u8 {
    let code = nnn.tui_run(args);
    nnn.cleanup();
    code
}

impl Nnn {
    /// The globals parse_opts() sets in the original, from the parsed options.
    fn apply_opts(&mut self, st: &Opts) {
        self.opts = st.clone();
        self.g_state.autofifo = st.autofifo;
        if st.no_autoenter {
            self.cfg.autoenter = false;
        }
        self.g_state.usebsdtar = st.usebsdtar;
        self.cfg.cliopener = st.cliopener;
        self.g_state.oldcolor = st.oldcolor;
        self.cfg.sort.showdetail = st.showdetail;
        self.g_state.dirctx = st.dirctx;
        self.cfg.useeditor = st.useeditor;
        self.cfg.waitedit = st.waitedit;
        self.browse.hist_file = st.hist_file;
        self.g_state.fifomode = st.fifomode == 1;
        self.cfg.regex = st.regex;
        self.cfg.fuzzy = st.fuzzy;
        self.cfg.sort.showhidden = st.showhidden;
        self.cfg.fileinfo = st.fileinfo;
        self.g_state.stayonsel = st.stayonsel;
        self.scroll_lines = st.scroll_lines.unwrap_or(1);
        self.cfg.filtermode = st.filtermode;
        self.cfg.nonavopen = st.nonavopen;
        self.g_state.picker = st.picker;
        self.selpath.clone_from(&st.selpath);
        self.g_state.forcequit = st.forcequit;
        if st.no_rollover {
            self.cfg.rollover = false;
        }
        self.popup.curssn.clone_from(&st.session);
        self.g_state.prstssn = st.persistent_session;
        self.idletimeout = st.idletimeout.unwrap_or(0);
        self.cfg.prefersel = st.prefersel;
        self.g_state.uidgid = st.uidgid;
        self.cfg.x11 = st.x11;
        self.browse.sepnul = st.sepnul;
    }

    /// main() from `home = getenv("HOME")` to `return opt`.
    fn tui_run(&mut self, args: TuiArgs<'_>) -> u8 {
        let st = args.opts;
        // Static initialisers of the globals.
        self.g_state.lastctx = CTX_INVALID;
        self.g_state.closedctx = CTX_INVALID;
        self.blk_shift = BLK_SHIFT_512;
        self.cfg.sort.blk_shift = BLK_SHIFT_512;

        // NNN_MCLICK, read first in main().
        let mc = xgetenv_or("NNN_MCLICK", b"\0");
        self.middle_click_key = if mc.first() == Some(&b'^') && mc.get(1).is_some_and(|&c| c != 0) {
            control(mc[1])
        } else {
            i32::from(mc.first().copied().unwrap_or(0))
        };

        self.apply_opts(st);
        let sort = st.sort.map_or(0, i32::from);
        let mut pkey = st.pkey.map_or(0, i32::from);

        // set_tmp_path(): startup.rs checked it, the path is kept here.
        if self.g_tmpfpath.is_empty() {
            self.g_tmpfpath = if xdiraccess(b"/tmp") {
                b"/tmp".to_vec()
            } else {
                getenv("TMPDIR").unwrap_or_default()
            };
        }

        let mut initpath = args.initpath;
        if initpath.is_some() {
            // List mode: no session.
            self.popup.curssn.clear();
        }

        let Some(home) = crate::platform::home() else {
            msg(b"set HOME");
            return 1;
        };
        self.home = home;
        self.browse.homelen = self.home.len();

        if !self.setup_config() {
            return 1;
        }

        // A custom opener.
        let defaults = platform::defaults();
        self.opener = xgetenv_or("NNN_OPENER", &defaults.opener);
        self.opener_multi = self.opener == defaults.opener && defaults.opener.contains(&b' ');

        // Bookmarks and NNN_ORDER.
        let (ok, bm) = parsekvpair(getenv("NNN_BMS").as_deref());
        self.bookmark = bm;
        if !ok {
            msg(b"NNN_BMS");
            return 1;
        }
        let (ok, order) = parsekvpair(getenv("NNN_ORDER").as_deref());
        self.order = order;
        if !ok {
            msg(b"NNN_ORDER");
            return 1;
        }

        if initpath.is_none() {
            if let Some(a) = st.bookmark.clone() {
                // Open a bookmark directly; bookmark keys are single chars.
                if a.len() == 1 {
                    initpath = get_kv_val(&self.bookmark, i32::from(a[0]), NNN_BMS, &self.home);
                }
                if initpath.is_none() {
                    msg(MESSAGES[MSG_INVALID_KEY]);
                    return 1;
                }
                self.popup.curssn.clear();
            } else if args.operands.is_empty() {
                // Start in the current directory.
                initpath = match std::env::var_os("PWD") {
                    Some(p) if !p.is_empty() => Some(crate::platform::into_bytes(p)),
                    _ => std::env::current_dir()
                        .ok()
                        .map(|p| crate::platform::into_bytes(p.into_os_string())),
                };
                if initpath.is_none() {
                    initpath = Some(b"/".to_vec());
                }
            } else {
                // Open a file.
                let mut a: &[u8] = &args.operands[0];
                if a.len() > 7 && a.starts_with(b"file://") {
                    a = &a[7..];
                }
                let Some(mut ip) = sys::abspath_in(a, None, Some(&self.home)) else {
                    sys::xerror(10819, &std::io::Error::last_os_error());
                    return 1;
                };
                // A hidden file turns hidden files on.
                if xbasename(&ip).first() == Some(&b'.') {
                    self.cfg.sort.showhidden = true;
                }
                match std::fs::metadata(os(&ip)) {
                    Err(_) => {
                        let dir = a.last() == Some(&b'/');
                        let full = ip.clone();
                        if !dir {
                            ip = xdirname(&ip);
                            // Override the plugin key: create the file.
                            pkey = CREATE_NEW_KEY;
                            self.g_state.initfile = true;
                        }
                        if !xdiraccess(&ip) && !xmktree(&ip, true, true) {
                            sys::xerror(10846, &std::io::Error::last_os_error());
                            return 1;
                        }
                        // "*--arg = '/'" restores the complete path (not in the root,
                        // where xdirname cut it after the first '/').
                        if !dir && ip != b"/" {
                            ip = full;
                        }
                    }
                    Ok(sb) => {
                        if !sb.is_dir() {
                            self.g_state.initfile = true;
                        }
                    }
                }
                initpath = Some(ip);
                self.popup.curssn.clear();
            }
        }
        // INFALLIBLE: every branch above sets initpath or returns.
        let initpath = initpath.unwrap_or_else(|| b"/".to_vec());
        self.initpath.clone_from(&initpath);

        // Archive handling.
        let pat = getenv("NNN_ARCHIVE").unwrap_or_else(|| P_ARCHIVE.to_vec());
        match Regex::new(&pat, true) {
            Some(re) => self.browse.archive_re = Some(re),
            None => {
                msg(MESSAGES[MSG_INVALID_REG]);
                return 1;
            }
        }

        // An all-CLI opener overrides -e.
        if self.cfg.cliopener {
            self.cfg.useeditor = false;
        }

        self.enveditor = xgetenv_or("EDITOR", &defaults.editor);
        self.editor = xgetenv_or("VISUAL", &self.enveditor);
        self.pager = xgetenv_or("PAGER", &defaults.pager);
        self.shell = xgetenv_or("SHELL", &defaults.shell);

        // Create the FIFO.
        let mut fifopath = getenv("NNN_FIFO").filter(|p| !p.is_empty());
        if self.g_state.autofifo {
            let (mut p, _) = mkpath(&self.g_tmpfpath, b"nnn-fifo.", &self.home);
            p.extend_from_slice(std::process::id().to_string().as_bytes());
            crate::locale::setenv("NNN_FIFO", &p);
            fifopath = Some(p);
        }
        self.fifopath = fifopath;
        if let Some(fp) = self.fifopath.clone()
            && let Err(code) = make_fifo(&fp)
        {
            return code;
        }
        // SIGPIPE: the Rust runtime already ignores it.

        // inotify
        match platform::DirWatch::new() {
            Ok(w) => self.browse.dir_watch = Some(w),
            Err(e) => {
                sys::xerror(10935, &e);
                return 1;
            }
        }

        // Trash preference.
        self.trashcmd = getenv("NNN_TRASH").map(|t| match t.as_slice() {
            b"1" => UTIL_TRASH_CLI.to_vec(),
            b"2" => UTIL_GIO_TRASH.to_vec(),
            _ => t,
        });

        // Signals: SIGINT sets the interrupt flag, SIGTERM/SIGHUP exit cleanly.
        if let Err((step, e)) =
            platform::register_signals(self.browse.sigint.clone(), self.browse.sigterm.clone())
        {
            let line = match step {
                platform::SignalStep::Interrupt => 10965,
                platform::SignalStep::Terminate => 10972,
                platform::SignalStep::Quit => 10979,
            };
            sys::xerror(line, &e);
            return 1;
        }

        if self.browse.hist_file {
            let (p, _) = mkpath(&self.cfgpath, b".history", &self.home);
            self.prompt.xread_history(&p);
        }

        if self.cfg.x11 && !self.g_state.picker {
            // Save the terminal window title.
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[22;0t");
            let _ = out.flush();
            // gethostname()
            self.hostname = platform::hostname();
            self.hostname.truncate(255);
        }

        if !self.initcurses() {
            return 1;
        }

        if sort != 0 {
            self.set_sort_flags(sort);
        }

        let opt = self.browse(initpath, pkey);

        if !self.popup.curssn.is_empty() && self.g_state.prstssn {
            let ssn = self.popup.curssn.clone();
            self.save_session(&ssn, None);
        }

        if let Some(c) = self.curses.as_mut() {
            c.mousemask(false);
        }
        self.exitcurses();

        if self.browse.hist_file {
            let (p, _) = mkpath(&self.cfgpath, b".history", &self.home);
            self.prompt.xwrite_history(&p);
        }

        if self.g_state.picker {
            if !self.pselbuf.is_empty() {
                let sep = if self.browse.sepnul { 0 } else { b'\n' };
                let want = self.pselbuf.len();
                match self.selpath.clone() {
                    Some(sp) => match platform::private_mode(
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true),
                    )
                    .open(os(&sp))
                    {
                        Ok(mut f) => {
                            if self.seltofile(&mut f, sep).0 != want {
                                sys::xerror(11068, &std::io::Error::last_os_error());
                            }
                        }
                        Err(e) => sys::xerror(11068, &e),
                    },
                    None => {
                        let mut out = std::io::stdout();
                        let n = self.seltofile(&mut out, sep).0;
                        let _ = out.flush();
                        if n != want {
                            sys::xerror(11068, &std::io::Error::last_os_error());
                        }
                    }
                }
            }
        } else if let Some(sp) = &self.selpath {
            sys::unlink(sp);
        }

        // Remove the tmp dir of list mode.
        self.rmlistpath();

        self.browse.archive_re = None;
        self.pselbuf.clear();

        // Shut down inotify.
        self.unwatch();
        self.browse.dir_watch = None;

        if !self.g_state.fifomode {
            self.notify_fifo(false);
        }
        self.browse.fifofd = None;

        opt
    }

    /// `initcurses`: curses mode, mouse on, cursor off, the context colors (NNN_COLORS)
    /// and file colors (NNN_FCOLORS), a 1 s key timeout and a 25 ms ESC delay.
    pub fn initcurses(&mut self) -> bool {
        let Some(c) = Curses::init(self.g_state.picker) else {
            msg(if self.g_state.picker {
                b"newterm!"
            } else {
                b"initscr!"
            });
            return false;
        };
        self.curses = Some(c);
        let cs = self.cs();
        cs.mousemask(true);
        cs.curs_set(false);

        let colors = getenv("NNN_COLORS");
        if colors.is_some() || getenv("NO_COLOR").is_none() {
            self.cs().start_color();
            self.browse.colors = term_colors();

            // File colors.
            if self.browse.colors >= COLOR_256 {
                if !self.g_state.oldcolor && !self.init_fcolors() {
                    self.exitcurses();
                    msg(b"NNN_FCOLORS");
                    return false;
                }
            } else {
                self.g_state.oldcolor = true;
            }

            // Context colors.
            let mut fcolors = self.browse.fcolors;
            match parse_ctx_colors(
                colors.as_deref(),
                self.g_state.oldcolor,
                self.browse.colors,
                &mut fcolors,
            ) {
                Some(codes) => {
                    self.browse.fcolors = fcolors;
                    for (i, code) in codes.iter().enumerate() {
                        self.g_ctx[i].color = *code;
                        self.cs().init_pair(i + 1, *code as i16, -1);
                    }
                    self.fcolors[1..=CTX_MAX].copy_from_slice(&fcolors[1..=CTX_MAX]);
                }
                None => {
                    self.exitcurses();
                    msg(b"NNN_COLORS");
                    return false;
                }
            }
        }

        // settimeout(): one second.
        let cs = self.cs();
        cs.timeout(1000);
        cs.set_escdelay(25);
        true
    }

    /// `init_fcolors`
    fn init_fcolors(&mut self) -> bool {
        let f = getenv("NNN_FCOLORS").unwrap_or_default();
        let mut fcolors = self.browse.fcolors;
        let ok = parse_fcolors(&f, &mut fcolors);
        self.browse.fcolors = fcolors;
        for (id, &code) in fcolors.iter().enumerate().skip(C_BLK) {
            if code != 0 {
                self.cs().init_pair(id, code as i16, -1);
            }
        }
        ok
    }

    /// `set_sort_flags`: the sort flags of the key (crate::sort), the selection ended for a
    /// du reload, the context's sort key recorded (`cfgsort`). 0 if not a sort key.
    pub fn set_sort_flags(&mut self, r: i32) -> i32 {
        let Ok(key) = u8::try_from(r) else {
            return 0;
        };
        let (code, reload) = self.cfg.sort.set_sort_flags(key);
        if code == 0 {
            return 0;
        }
        self.blk_shift = self.cfg.sort.blk_shift;
        if reload {
            // We are going to reload the dir.
            self.endselection(true);
        }
        self.cfgsort[self.cfg.curctx] = code;
        i32::from(code)
    }

    /// `exitcurses()` (`endwin()`).
    pub fn exitcurses(&mut self) {
        if let Some(c) = self.curses.as_mut() {
            c.endwin();
        }
    }

    /// `cleanup()`, the original's atexit handler.
    pub fn cleanup(&mut self) {
        if self.cfg.x11 && !self.g_state.picker {
            // Reset the terminal window title.
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[23;0t");
            let _ = out.flush();
        }
        if self.g_state.autofifo
            && let Some(fp) = &self.fifopath
        {
            sys::unlink(fp);
        }
        if self.g_state.pluginit {
            let p = crate::plugin::pipepath_for(&self.g_tmpfpath);
            sys::unlink(&p);
        }
        self.dentfree_du();
    }

    /// `clean_exit_sighandler`, run when browse() next looks: after SIGTERM/SIGHUP, leave
    /// curses and exit 0 (through cleanup()).
    fn check_clean_exit(&mut self) {
        if self.browse.sigterm.swap(false, Ordering::Relaxed) {
            self.exitcurses();
            self.cleanup();
            std::process::exit(0);
        }
    }

    /// `xterm_cfg`: the directory to the terminal (OSC 7) and the window title (OSC 2),
    /// with `~` for the home prefix (`set_tilde_in_path`/`reset_tilde_in_path`).
    pub fn xterm_cfg(&mut self, path: &[u8]) {
        if self.cfg.x11 && !self.g_state.picker {
            let mut out = b"\x1b]7;file://".to_vec();
            out.extend_from_slice(&self.hostname);
            out.extend_from_slice(path);
            out.extend_from_slice(b"\x1b\\\x1b]2;");
            let homelen = self.browse.homelen.min(self.home.len());
            if homelen > 0 && path.starts_with(&self.home[..homelen]) {
                out.push(b'~');
                out.extend_from_slice(&path[homelen..]);
            } else {
                out.extend_from_slice(path);
            }
            out.push(0x07);
            let mut o = std::io::stdout();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
    }

    /// `handle_event`: the directory changed: reload (^L).
    pub fn handle_event(&mut self) -> i32 {
        if self.nselected != 0 && self.isselfileempty() {
            self.clearselection();
        }
        control(b'L')
    }

    /// The LINUX_INOTIFY read in `nextsel()` (nextsel checks du mode and odd idle seconds):
    /// `Some(handle_event())` when the watched directory changed.
    pub fn poll_dir_watch(&mut self) -> Option<i32> {
        let changed = self.browse.dir_watch.as_mut()?.changed();
        changed.then(|| self.handle_event())
    }

    /// `inotify_wd >= 0`
    fn watching(&self) -> bool {
        self.browse
            .dir_watch
            .as_ref()
            .is_some_and(platform::DirWatch::is_watching)
    }

    /// `inotify_rm_watch(inotify_fd, inotify_wd); inotify_wd = -1;`
    fn unwatch(&mut self) {
        if let Some(w) = self.browse.dir_watch.as_mut() {
            w.unwatch();
        }
    }

    /// `inotify_wd = inotify_add_watch(inotify_fd, path, INOTIFY_MASK)`
    fn watch(&mut self, path: &[u8]) {
        if let Some(w) = self.browse.dir_watch.as_mut() {
            w.watch(path);
        }
    }

    /// `open_fifo`: NNN_FIFO opened to write without blocking; it must be a FIFO.
    pub fn open_fifo(&self) -> std::io::Result<std::fs::File> {
        let Some(fp) = &self.fifopath else {
            return Err(platform::error(Errno::NoEnt));
        };
        platform::open_fifo_write(fp)
    }

    /// `notify_fifo`: the hovered path to NNN_FIFO, when it changed or with `force`.
    pub fn notify_fifo(&mut self, force: bool) {
        if self.fifopath.is_none() {
            return;
        }
        if self.browse.fifofd.is_none() {
            match self.open_fifo() {
                Ok(f) => self.browse.fifofd = Some(f),
                Err(e) => {
                    if !platform::is_errno(&e, Errno::NxIo) {
                        // The FIFO may be gone: give up notifying.
                        self.fifopath = None;
                    }
                    return;
                }
            }
        }
        let curent = usize::try_from(self.cur)
            .ok()
            .and_then(|c| self.pdents.get(c))
            .cloned();
        if !force && self.browse.lastentry == curent {
            return;
        }
        self.browse.lastentry.clone_from(&curent);
        let name = if self.ndents > 0 {
            curent.map(|e| e.name).unwrap_or_default()
        } else {
            Vec::new()
        };
        let (mut path, _) = mkpath(&self.g_ctx[self.cfg.curctx].c_path, &name, &self.home);
        path.push(b'\n');
        if let Some(f) = &self.browse.fifofd {
            let mut f: &std::fs::File = f;
            let _ = f.write(&path);
        }
    }

    /// `send_to_explorer`: the selection (or the opened path) to NNN_FIFO in -F 1 mode.
    pub fn send_to_explorer(&mut self, presel: &mut i32) {
        if self.nselected != 0 {
            match self.open_fifo() {
                Ok(mut f) => {
                    if self.seltofile(&mut f, b'\n').0 != self.pselbuf.len() {
                        self.printwarn(Some(presel));
                    } else {
                        self.resetselind();
                        self.clearselection();
                    }
                }
                Err(_) => self.printwarn(Some(presel)),
            }
        } else {
            // Send the opened path to NNN_FIFO.
            self.notify_fifo(true);
        }
    }

    /// `set_time_type`: ask which time to show and sort by.
    pub fn set_time_type(&mut self, presel: &mut i32) -> bool {
        let mut ret = false;
        let cur = match self.cfg.sort.timetype {
            TimeType::Mod => b'm',
            TimeType::Access => b'a',
            TimeType::Change => b'c',
        };
        let mut buf = b"'a'ccess / 'c'hange / 'm'od [ ]".to_vec();
        let at = buf.len() - 2;
        buf[at] = cur;
        let r = self.get_input(Some(&buf));
        let m = if r == i32::from(b'a') || r == i32::from(b'c') || r == i32::from(b'm') {
            let t = if r == i32::from(b'm') {
                TimeType::Mod
            } else if r == i32::from(b'a') {
                TimeType::Access
            } else {
                TimeType::Change
            };
            if self.cfg.sort.timetype != t {
                self.cfg.sort.timetype = t;
                if self.cfg.filtermode || self.fltr_at(self.cfg.curctx, 1) != 0 {
                    *presel = FILTER;
                }
                ret = true;
                0
            } else {
                MSG_NOCHANGE
            }
        } else {
            MSG_INVALID_KEY
        };
        if !ret {
            self.printwait(MESSAGES[m], Some(presel));
        }
        ret
    }

    /// `c_fltr[i]` of a context, 0 past its end.
    fn fltr_at(&self, ctx: usize, i: usize) -> u8 {
        self.g_ctx[ctx].c_fltr.get(i).copied().unwrap_or(0)
    }

    /// `preview_pane`: the right half of the screen shows the hovered file: the .npreview
    /// plugin's output, a directory's entries, a link's target, a text file's lines.
    pub fn preview_pane(&mut self, path: &[u8]) {
        if self.ndents == 0 || !self.cfg.preview {
            return;
        }
        let previewcol = self.xcols / 2;
        let pcol = previewcol + 2;
        let previewwidth = self.xcols - pcol - 1;
        if previewwidth < MIN_PREVIEW_COLS {
            return;
        }
        let xlines = self.xlines;
        {
            // The vertical border.
            let w = &mut self.cs().stdscr;
            for i in 0..xlines - 1 {
                w.mv(i, previewcol);
                w.attron(A_DIM);
                w.addch(curses::acs::VLINE);
                w.attroff(A_DIM);
            }
        }
        let name = self.pdents[self.cur as usize].name.clone();
        let (fpath, _) = mkpath(path, &name, &self.home);
        let pw = previewwidth as usize;

        // Auto-detect the .npreview plugin.
        if self.browse.previewer.is_none() {
            self.browse.previewer = Some(mkpath(&self.plgpath, UTIL_NPREVIEW, &self.home).0);
        }
        if let Some(previewer) = self.browse.previewer.clone()
            && platform::access(&previewer, Access::Exec).is_ok()
        {
            // spawn_redirected(): stdout and stderr into a pipe.
            let Ok((reader, writer)) = std::io::pipe() else {
                return;
            };
            let Ok(werr) = writer.try_clone() else {
                return;
            };
            let child = std::process::Command::new(&*crate::platform::os(&previewer))
                .arg(&*crate::platform::os(&fpath))
                .arg(previewwidth.to_string())
                .arg((xlines - 2).to_string())
                .stdout(writer)
                .stderr(werr)
                .spawn();
            if let Ok(mut child) = child {
                let mut r = BufReader::new(reader);
                self.preview_lines(&mut r, pcol, pw);
                drop(r);
                let _ = child.wait();
            }
            return;
        }

        let Ok(sb) = std::fs::symlink_metadata(os(&fpath)) else {
            return;
        };
        // A directory: its entries.
        if sb.is_dir() {
            let Ok(rd) = std::fs::read_dir(os(&fpath)) else {
                return;
            };
            let maxlines = xlines - 4;
            let w = &mut self.cs().stdscr;
            w.mvaddstr(1, pcol, b"[directory]");
            for (count, e) in (0..maxlines).zip(rd) {
                let Ok(e) = e else {
                    break;
                };
                let n = e.file_name().into_encoded_bytes();
                w.mvaddstr(2 + count, pcol, &n[..n.len().min(pw)]);
            }
            return;
        }
        let mut size = sb.len();
        let mut is_reg = sb.is_file();
        // A symlink: its target, then the file it resolves to.
        if sb.file_type().is_symlink() {
            if let Ok(t) = std::fs::read_link(os(&fpath)) {
                let t = crate::platform::into_bytes(t.into_os_string());
                let t = &t[..t.len().min(PATH_MAX - 1)];
                if !t.is_empty() {
                    let mut line = b"-> ".to_vec();
                    line.extend_from_slice(&t[..t.len().min(pw.saturating_sub(3))]);
                    self.cs().stdscr.mvaddstr(1, pcol, &line);
                }
            }
            let Ok(sb) = std::fs::metadata(os(&fpath)) else {
                return;
            };
            if !sb.is_file() {
                return;
            }
            is_reg = true;
            size = sb.len();
        }
        // A regular file: its lines if it is text.
        if is_reg {
            if size == 0 {
                self.cs().stdscr.mvaddstr(1, pcol, b"[empty file]");
                return;
            }
            if !is_text_file(&fpath) {
                let line = format!("[binary {}]", coolsize(size as i64));
                self.cs().stdscr.mvaddstr(1, pcol, line.as_bytes());
                return;
            }
            let Ok(f) = std::fs::File::open(os(&fpath)) else {
                return;
            };
            let mut r = BufReader::new(f);
            self.preview_lines(&mut r, pcol, pw);
        }
    }

    /// The line loop of preview_pane() (plugin output and text files): tabs as spaces,
    /// long lines wrapped, rows 1 to `xlines - 3`.
    fn preview_lines(&mut self, r: &mut impl BufRead, pcol: i32, pw: usize) {
        let maxrows = self.xlines - 2;
        let mut row = 1;
        let w = &mut self.cs().stdscr;
        while row < maxrows {
            let Some(mut line) = fgets(r) else {
                break;
            };
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            for c in line.iter_mut() {
                if *c == b'\t' {
                    *c = b' ';
                }
            }
            let mut off = 0usize;
            loop {
                let end = (off + pw).min(line.len());
                w.mvaddstr(row, pcol, line.get(off..end).unwrap_or(&[]));
                row += 1;
                off += pw;
                if !(off < line.len() && row < maxrows) {
                    break;
                }
            }
        }
    }

    /// `showselsize`: the size of the selected entries of this directory.
    pub fn showselsize(&mut self, path: &[u8]) {
        let mut sz: i64 = 0;
        let (_, prefix) = self.scanselforpath(path, false);
        let mut selcount = self.nselected;
        let mut r = 0;
        while r < self.ndents && selcount != 0 {
            let e = &self.pdents[r as usize];
            let mut p = prefix.clone();
            p.extend_from_slice(&e.name);
            let size = if self.cfg.sort.blkorder {
                e.blocks as i64
            } else {
                e.size
            };
            if self.findinsel(self.findselpos, &p).is_some() {
                sz += size;
                selcount -= 1;
            }
            r += 1;
        }
        let shown = if self.cfg.sort.blkorder {
            sz << self.blk_shift
        } else {
            sz
        };
        self.printmsg(coolsize(shown).as_bytes());
    }

    /// `init_browse_context`: context 0 at the start path, or the session's contexts.
    fn init_browse_context(&mut self, ipath: &mut Vec<u8>) -> bool {
        if !self.popup.curssn.is_empty() {
            let ssn = self.popup.curssn.clone();
            if self.load_session(Some(&ssn), false) {
                return true;
            }
        }
        self.g_ctx[0].c_last.clear();
        if self.g_state.initfile {
            self.g_ctx[0].c_name = ncpy(xbasename(ipath), NAME_MAX + 1);
            *ipath = xdirname(ipath);
        } else {
            self.g_ctx[0].c_name.clear();
        }
        self.g_ctx[0].c_path = ncpy(ipath, PATH_MAX);
        if self.g_state.initfile {
            match std::env::current_dir() {
                Ok(d) => {
                    *ipath = crate::platform::into_bytes(d.into_os_string());
                    self.initpath.clone_from(ipath);
                }
                Err(_) => return false,
            }
        }
        let f = &mut self.g_ctx[0].c_fltr;
        if f.len() < 2 {
            f.resize(2, 0);
        }
        f[0] = 0;
        f[1] = 0;
        self.g_ctx[0].c_cfg = self.cfg.clone();
        true
    }

    /// The name under the cursor (`ndents ? pdents[cur].name : NULL`).
    fn curname(&self) -> Option<Vec<u8>> {
        (self.ndents > 0).then(|| self.pdents[self.cur as usize].name.clone())
    }

    /// `path`: the current context's directory.
    fn cpath(&self) -> Vec<u8> {
        self.g_ctx[self.cfg.curctx].c_path.clone()
    }

    /// `lastname = ...`
    fn set_lastname(&mut self, name: &[u8]) {
        let cc = self.cfg.curctx;
        self.g_ctx[cc].c_name = ncpy(name, NAME_MAX + 1);
    }

    /// `setdirwatch()`
    fn setdirwatch(&self, b: &mut Locals) {
        if self.cfg.filtermode {
            b.presel = FILTER;
        } else {
            b.watch = true;
        }
    }

    /// `cfg.filtermode ? presel = FILTER : statusbar(path)`
    fn filter_or_statusbar(&mut self, b: &mut Locals) {
        if self.cfg.filtermode {
            b.presel = FILTER;
        } else {
            let path = self.cpath();
            self.statusbar(&path);
        }
    }

    /// `listpath && xstrcmp(path, listpath) == 0 ? listroot : path`
    fn list_or_path(&self, path: &[u8]) -> Vec<u8> {
        match (&self.listpath, &self.listroot) {
            (Some(lp), Some(lr)) if lp.as_slice() == path => lr.clone(),
            _ => path.to_vec(),
        }
    }

    /// `browse`: the main loop. Returns main()'s exit code.
    pub fn browse(&mut self, ipath: Vec<u8>, pkey: i32) -> u8 {
        let mut b = Locals {
            ipath,
            newpath: Vec::new(),
            runfile: Vec::new(),
            presel: 0,
            pkey,
            selstartid: 0,
            selendid: 0,
            watch: false,
            cd: true,
            inode: 0,
            mousetimings: [(0, 0); 2],
            mousedent: [-1, -1],
            currentmouse: 1,
            rightclicksel: false,
            clock: Instant::now(),
        };

        let cs = self.cs();
        (self.xlines, self.xcols) = (cs.lines(), cs.cols());

        let mut ipath = std::mem::take(&mut b.ipath);
        if !self.init_browse_context(&mut ipath) {
            return 0;
        }
        b.ipath = ipath;

        let c = self.cfg.curctx;
        let f0 = i32::from(self.fltr_at(c, 0));
        b.presel = if pkey != 0 {
            if pkey == CREATE_NEW_KEY {
                i32::from(b'n')
            } else {
                i32::from(b';')
            }
        } else if (self.cfg.filtermode
            || (!self.popup.curssn.is_empty()
                && (f0 == FILTER || f0 == RFILTER)
                && self.fltr_at(c, 1) != 0))
            && !self.cfg.sort.blkorder
        {
            FILTER
        } else {
            0
        };

        // A broken window at the start.
        if b.presel == FILTER {
            self.handle_key_resize();
        }

        let mut flow = Flow::Begin;
        loop {
            flow = match flow {
                Flow::Begin => self.browse_begin(&mut b),
                Flow::Top => {
                    // No double redraw in filterentries.
                    if b.presel != FILTER || !self.filterset() {
                        let path = self.cpath();
                        self.redraw(&path);
                        self.statusbar(&path);
                    }
                    Flow::Nochange
                }
                Flow::Nochange => self.browse_nochange(&mut b),
                Flow::CdfalseBegin => {
                    b.cd = false;
                    Flow::Begin
                }
                Flow::SetwatchBegin => {
                    self.setdirwatch(&mut b);
                    Flow::Begin
                }
                Flow::WarnNochange => {
                    self.printwarn(Some(&mut b.presel));
                    Flow::Nochange
                }
                Flow::MsgwaitNochange => {
                    b.presel = MSGWAIT;
                    Flow::Nochange
                }
                Flow::StatusbarNochange => {
                    let path = self.cpath();
                    self.statusbar(&path);
                    Flow::Nochange
                }
                Flow::Return(code) => return code,
            };
        }
    }

    /// `begin:` in browse(): enter the directory, sort by NNN_ORDER, list it, watch it.
    fn browse_begin(&mut self, b: &mut Locals) -> Flow {
        // Can fail when permissions change while browsing.
        if !chdir(&self.cpath()) {
            self.valid_parent();
            self.setdirwatch(b);
        }
        let path = self.cpath();
        self.xterm_cfg(&path);

        if (b.presel == FILTER || b.watch) && self.watching() {
            self.unwatch();
            b.watch = false;
        }

        if !self.order.is_empty() && b.cd {
            let c = self.cfg.curctx;
            if self.cfgsort[c] != b'0' {
                if self.cfgsort[c] == b'z' {
                    self.set_sort_flags(i32::from(b'c'));
                }
                if self.cfgsort[c] == 0 || self.cfgsort[c] == b'c' {
                    let r = get_kv_key(&self.order, &path, NNN_ORDER);
                    if r > 0 {
                        self.set_sort_flags(r);
                        self.cfgsort[c] = b'z';
                    }
                }
            } else {
                self.cfgsort[c] = self.cfgsort[CTX_MAX];
            }
        }
        b.cd = true;

        let lastname = self.g_ctx[self.cfg.curctx].c_name.clone();
        self.populate(&path, &lastname);
        if self.browse.sigint.swap(false, Ordering::Relaxed) {
            self.g_state.interrupt = true;
        }
        if self.g_state.interrupt {
            self.g_state.interrupt = false;
            self.cfg.sort.apparentsz = false;
            self.cfg.sort.blkorder = false;
            self.blk_shift = BLK_SHIFT_512;
            self.cfg.sort.blk_shift = BLK_SHIFT_512;
            b.presel = control(b'L');
        }

        if b.presel != FILTER && !self.watching() {
            self.watch(&path);
        }
        Flow::Top
    }

    /// `nochange:` in browse(): the checks, the next key, the switch on its action.
    fn browse_nochange(&mut self, b: &mut Locals) -> Flow {
        // Exit if the parent has exited.
        if platform::parent_is_init() {
            std::process::exit(1);
        }
        self.check_clean_exit();
        // If CWD is deleted or moved or perms changed, find an accessible parent.
        if !chdir(&self.cpath()) {
            return Flow::Begin;
        }
        // If STDIN is no longer a tty (closed) we should exit.
        if !std::io::stdin().is_terminal() && !self.g_state.picker {
            return Flow::Return(1);
        }

        let sel = self.nextsel(b.presel);
        if b.presel != 0 {
            b.presel = 0;
        }

        let Some(sel) = sel else {
            return self.sel_default(b);
        };
        use Action as A;
        match sel {
            A::Click => self.sel_click(b),
            A::Back => self.sel_back(b),
            A::NavIn | A::Open => self.sel_open(b, sel),
            A::Next
            | A::Prev
            | A::Pgdn
            | A::CtrlD
            | A::Pgup
            | A::CtrlU
            | A::Home
            | A::End
            | A::First
            | A::Young => {
                if self.ndents != 0 {
                    self.g_state.mv = true;
                    self.handle_screen_move(sel);
                }
                Flow::Top
            }
            A::Jump => {
                if self.ndents != 0 {
                    self.g_state.showlines = true;
                    let path = self.cpath();
                    self.redraw(&path);
                    self.handle_screen_move(sel);
                    self.g_state.showlines = false;
                }
                Flow::Top
            }
            A::Cdhome | A::Cdbegin | A::Cdlast | A::Cdroot | A::Bmopen => self.sel_cd(b, sel),
            A::Remote => {
                if !self.remote_mount(&mut b.newpath) {
                    return Flow::MsgwaitNochange;
                }
                let np = b.newpath.clone();
                let cn = self.curname();
                self.set_smart_ctx(i32::from(b'+'), &np, cn.as_deref());
                self.clearfilter();
                Flow::Begin
            }
            A::Cycle
            | A::Cycler
            | A::Ctx1
            | A::Ctx2
            | A::Ctx3
            | A::Ctx4
            | A::Ctx5
            | A::Ctx6
            | A::Ctx7
            | A::Ctx8 => {
                let r = self.handle_context_switch(sel);
                if r < 0 {
                    return Flow::Top;
                }
                let r = r as usize;
                let path = self.cpath();
                let cn = self.curname();
                self.savecurctx(&path, cn.as_deref(), r);
                // path, lastdir and lastname now point into context r.
                let f0 = i32::from(self.fltr_at(r, 0));
                if self.cfg.filtermode
                    || ((f0 == FILTER || f0 == RFILTER) && self.fltr_at(r, 1) != 0)
                {
                    b.presel = FILTER;
                } else {
                    b.watch = true;
                }
                Flow::Begin
            }
            A::Mark => {
                let path = self.cpath();
                self.mark = Some(path.clone());
                self.printwait(&path, Some(&mut b.presel));
                Flow::Nochange
            }
            A::Bmark => {
                let path = self.cpath();
                self.add_bookmark(&path, &mut b.newpath, &mut b.presel);
                Flow::Nochange
            }
            A::Fltr => {
                // Unwatch the dir while in a filtered view.
                self.unwatch();
                let path = self.cpath();
                b.presel = self.filterentries(&path);
                if b.presel == ESC {
                    b.presel = 0;
                    return Flow::Top;
                }
                if b.presel == FILTER {
                    // Refresh the dir and filter again.
                    return Flow::CdfalseBegin;
                }
                Flow::Nochange
            }
            A::Mfltr | A::Hidden | A::Detail | A::Preview | A::Sort => self.sel_view(b, sel),
            A::Stats | A::Chmodx => {
                if self.ndents != 0 {
                    if sel == A::Chmodx && !platform::SUPPORTS_CHMOD {
                        self.printwait(MESSAGES[MSG_NOT_SUPPORTED], Some(&mut b.presel));
                        return Flow::Nochange;
                    }
                    let path = self.cpath();
                    let tmp = self.list_or_path(&path);
                    let ok = if sel == A::Stats {
                        self.show_stats(&mut b.newpath, &tmp)
                    } else {
                        self.xchmod(&mut b.newpath, &tmp)
                    };
                    if !ok {
                        return Flow::WarnNochange;
                    }
                }
                Flow::Top
            }
            A::Redraw | A::Renamemul | A::Help | A::Autonext | A::Edit | A::Lock => {
                self.sel_misc(b, sel)
            }
            A::Sel => self.sel_sel(b),
            A::Selmul | A::Selall | A::Selinv => self.sel_range(b, sel),
            A::Seledit => {
                let r = self.editselection(false);
                if r <= 0 {
                    let m = if r == 0 { MSG_0_SELECTED } else { MSG_FAILED };
                    self.printwait(MESSAGES[m], Some(&mut b.presel));
                } else {
                    self.copytoclipboard();
                    self.filter_or_statusbar(b);
                }
                Flow::Begin
            }
            A::Cp | A::Mv | A::Cpmvas | A::Trash | A::RmRf => self.sel_cpmvrm(b, sel),
            A::Archive | A::Openwith | A::New | A::Rename => self.sel_create(b, sel),
            A::Plugin => self.sel_plugin(b),
            A::Selsize => {
                let path = self.cpath();
                self.showselsize(&path);
                Flow::Nochange
            }
            A::Shell | A::Launch | A::Prompt => {
                let path = self.cpath();
                let r = self.handle_cmd(sel, &path, &mut b.newpath);
                // Continue in type-to-nav mode, if enabled.
                if self.cfg.filtermode {
                    b.presel = FILTER;
                }
                self.copycurname();
                if !r {
                    return Flow::Nochange;
                }
                // Repopulate: the directory may have changed.
                Flow::CdfalseBegin
            }
            A::Umount => {
                b.presel = MSG_ZERO;
                let path = self.cpath();
                let cn = self.curname();
                if !self.unmount(cn.as_deref(), &mut b.newpath, &mut b.presel, &path) {
                    if b.presel == MSG_ZERO {
                        self.statusbar(&path);
                    }
                    return Flow::Nochange;
                }
                // Dir removed, go to the next entry.
                self.copynextname();
                Flow::CdfalseBegin
            }
            A::Sessions => {
                let r = self.get_input(Some(MESSAGES[MSG_SSN_OPTS]));
                if r == i32::from(b's') {
                    let ssn = self.popup.curssn.clone();
                    let pre = if ssn.is_empty() || ssn == b"@" {
                        None
                    } else {
                        Some(ssn)
                    };
                    let tmp = self.xreadline(pre.as_deref(), MESSAGES[MSG_SSN_NAME]);
                    if !tmp.is_empty() {
                        self.save_session(&tmp, Some(&mut b.presel));
                    }
                } else if (r == i32::from(b'l') || r == i32::from(b'r'))
                    && self.load_session(None, r == i32::from(b'r'))
                {
                    return Flow::SetwatchBegin;
                }
                Flow::StatusbarNochange
            }
            A::Export => {
                self.export_file_list();
                self.filter_or_statusbar(b);
                Flow::Nochange
            }
            A::Timetype => {
                if !self.set_time_type(&mut b.presel) {
                    return Flow::Nochange;
                }
                Flow::CdfalseBegin
            }
            A::Reopenctx => {
                let r = self.g_state.closedctx;
                if r != CTX_INVALID && !self.g_ctx[r].c_cfg.ctxactive {
                    self.g_state.closedctx = CTX_INVALID;
                    self.g_ctx[r].c_cfg.ctxactive = true;
                    self.g_ctx[r].c_cfg.curctx = r;
                    let c = self.g_ctx[r].c_cfg.clone();
                    self.setcfg(c);
                    return Flow::SetwatchBegin;
                }
                Flow::StatusbarNochange
            }
            A::Quitctx | A::Quitcd | A::Quit | A::Quiterr => self.sel_quit(b, sel),
        }
    }

    /// The `default:` case: an unbound key, or a timeout.
    fn sel_default(&mut self, _b: &mut Locals) -> Flow {
        let (lines, cols) = {
            let cs = self.cs();
            (cs.lines(), cs.cols())
        };
        if self.xlines != lines || self.xcols != cols {
            return Flow::Top;
        }
        if self.idletimeout != 0 && self.idle == self.idletimeout {
            // Locker.
            self.lock_terminal();
            self.idle = 0;
        }
        self.copycurname();
        Flow::Nochange
    }

    /// `case SEL_CLICK:`
    fn sel_click(&mut self, b: &mut Locals) -> Flow {
        let ev: MouseEvent = self.cs().getmouse();
        // A click on a context at the top: "[1 2 3 4]...".
        if ev.bstate == BUTTON1_PRESSED && ev.y == 0 {
            let r = ev.x >> 1;
            // After the contexts: go to the parent.
            if r >= CTX_MAX as i32 {
                return self.sel_back(b);
            } else if r >= 0 && r as usize != self.cfg.curctx {
                let path = self.cpath();
                let cn = self.curname();
                self.savecurctx(&path, cn.as_deref(), r as usize);
                return Flow::SetwatchBegin;
            }
        }

        // Middle click action.
        if ev.bstate == BUTTON2_PRESSED {
            b.presel = self.middle_click_key;
            return Flow::Nochange;
        }
        let (cur, ndents, sl) = (self.cur, self.ndents, self.scroll_lines);
        // Scroll up.
        if ev.bstate == BUTTON4_PRESSED && ndents != 0 && (self.cfg.rollover || cur != 0) {
            let t = if !self.cfg.rollover && cur < sl {
                0
            } else {
                (cur + ndents - sl) % ndents
            };
            self.move_cursor(t, false);
            return Flow::Top;
        }
        // Scroll down.
        if ev.bstate == BUTTON5_PRESSED && ndents != 0 && (self.cfg.rollover || cur != ndents - 1) {
            let t = if !self.cfg.rollover && cur >= ndents - sl {
                ndents - 1
            } else {
                (cur + sl) % ndents
            };
            self.move_cursor(t, false);
            return Flow::Top;
        }

        // Toggle filter mode on a left click on the last 2 lines.
        if ev.y >= self.xlines - 2 && ev.bstate == BUTTON1_PRESSED {
            self.clearfilter();
            self.cfg.filtermode = !self.cfg.filtermode;
            if self.cfg.filtermode {
                b.presel = FILTER;
                return Flow::Nochange;
            }
            // Start watching the directory.
            b.watch = true;
            self.copycurname();
            return Flow::CdfalseBegin;
        }

        // A click on a file.
        if ev.y >= 2
            && ev.y <= ndents + 1
            && (ev.bstate == BUTTON1_PRESSED || ev.bstate == BUTTON3_PRESSED)
        {
            let r = self.curscroll + (ev.y - 2);
            if r != self.cur {
                self.move_cursor(r, true);
            } else if ev.bstate == BUTTON1_PRESSED && !self.g_state.fifomode {
                // Send the clicked path to NNN_FIFO.
                self.notify_fifo(true);
            }
            // Right click selection.
            if ev.bstate == BUTTON3_PRESSED {
                b.rightclicksel = true;
                b.presel = SELECT;
                return Flow::Nochange;
            }

            b.currentmouse ^= 1;
            let now = b.clock.elapsed();
            b.mousetimings[b.currentmouse] = (now.as_secs() as i64, i64::from(now.subsec_nanos()));
            b.mousedent[b.currentmouse] = self.cur;

            // A single click just selects, a double click opens.
            let (t0, t1) = (b.mousetimings[0], b.mousetimings[1]);
            if b.mousedent[0] != b.mousedent[1]
                || ((abssub(t0.0, t1.0) << 30) + abssub(t0.1, t1.1)) > DBLCLK_INTERVAL_NS
            {
                return Flow::Top;
            }
            b.mousetimings[b.currentmouse].0 = 0;
            b.mousedent[b.currentmouse] = -1;
            return self.sel_open(b, Action::Open);
        }
        if self.cfg.filtermode || self.filterset() {
            b.presel = FILTER;
        }
        self.copycurname();
        Flow::Nochange
    }

    /// `case SEL_BACK:`
    fn sel_back(&mut self, b: &mut Locals) -> Flow {
        let path = self.cpath();
        let Some(dir) = self.visit_parent(&path, Some(&mut b.presel)) else {
            return Flow::Nochange;
        };
        b.newpath.clone_from(&dir);
        // Save history.
        self.set_lastname(xbasename(&path));
        if self.cdprep(false, &dir) {
            b.presel = FILTER;
        } else {
            b.watch = true;
        }
        Flow::Begin
    }

    /// `case SEL_NAV_IN:` and `case SEL_OPEN:`
    fn sel_open(&mut self, b: &mut Locals, sel: Action) -> Flow {
        // Cannot descend in empty directories.
        if self.ndents == 0 {
            b.cd = false;
            self.g_state.selbm = false;
            self.g_state.runplugin = false;
            return Flow::Begin;
        }
        let cc = self.cfg.curctx;
        let pent = self.pdents[self.cur as usize].clone();
        let mut path = self.cpath();
        let mut bm = None;
        if self.g_state.selbm && is_lnk(pent.mode) {
            bm = bmtarget(&pent.name, &path, &self.home);
        }
        match bm {
            Some(t) => {
                b.newpath = t;
                // Back in the dir the bookmark was opened from.
                self.g_ctx[cc].c_path = ncpy(&self.g_ctx[cc].c_last.clone(), PATH_MAX);
                path = self.cpath();
            }
            None => b.newpath = mkpath(&path, &pent.name, &self.home).0,
        }
        self.g_state.selbm = false;

        // Visit the directory.
        if pent.flags & DIR_OR_DIRLNK != 0 {
            if !chdir(&b.newpath) {
                return Flow::WarnNochange;
            }
            let np = b.newpath.clone();
            if self.cdprep(true, &np) {
                b.presel = FILTER;
            } else {
                b.watch = true;
            }
            return Flow::Begin;
        }

        // Cannot use stale data in the entry, the file may be missing by now.
        let Ok(sb) = std::fs::metadata(os(&b.newpath)) else {
            return Flow::WarnNochange;
        };

        // Do not open non-regular files.
        if !sb.is_file() {
            self.printwait(MESSAGES[MSG_UNSUPPORTED], Some(&mut b.presel));
            return Flow::Nochange;
        }

        // Plugin selection mode.
        if self.g_state.runplugin {
            self.g_state.runplugin = false;
            // Must be in the plugin dir and the same context to pick a plugin.
            if self.g_state.runctx == self.cfg.curctx && path == self.plgpath {
                if !self.script_plugin_runnable(&pent.name, &mut b.presel) {
                    return Flow::Nochange;
                }
                self.endselection(false);
                // Back to the earlier dir afterwards.
                let ld = self.g_ctx[cc].c_last.clone();
                self.g_ctx[cc].c_path = ncpy(&ld, PATH_MAX);
                self.clearfilter();
                let rf = b.runfile.clone();
                if chdir(&ld) {
                    let _ = self.run_plugin_aliased(&pent.name, Some(&rf), &mut b.runfile, None);
                }
                if self.g_state.picked {
                    return Flow::Return(0);
                }
                if !b.runfile.is_empty() {
                    let rf = std::mem::take(&mut b.runfile);
                    self.set_lastname(&rf);
                }
                return Flow::SetwatchBegin;
            }
        }

        if self.g_state.fifomode && sel == Action::Open {
            // Write the selection to the explorer FIFO.
            self.send_to_explorer(&mut b.presel);
            return Flow::Top;
        }

        // Opened as a vim plugin and Enter pressed: pick.
        if self.g_state.picker && sel == Action::Open {
            if self.nselected == 0 {
                let (np, len) = mkpath(&path, &pent.name, &self.home);
                b.newpath.clone_from(&np);
                self.appendfpath(&np, len);
            }
            return Flow::Return(0);
        }

        if sel == Action::NavIn {
            // In a listing dir, go to the target on 'l' or Right on a symlink.
            if let Some(lp) = self.listpath.clone()
                && is_lnk(pent.mode)
                && path.starts_with(&lp)
            {
                let Ok(rp) = std::fs::canonicalize(os(&pent.name)) else {
                    return Flow::WarnNochange;
                };
                b.newpath = xdirname(&crate::platform::into_bytes(rp.into_os_string()));
                if !chdir(&b.newpath) {
                    return Flow::WarnNochange;
                }
                let np = b.newpath.clone();
                if self.cdprep(false, &np) {
                    b.presel = FILTER;
                } else {
                    b.watch = true;
                }
                self.set_lastname(&pent.name);
                return Flow::Begin;
            }
            // Opening files is disabled on Right or 'l'.
            if self.cfg.nonavopen {
                return Flow::Nochange;
            }
        }

        if sb.len() == 0 {
            self.printwait(MESSAGES[MSG_EMPTY_FILE], Some(&mut b.presel));
            return Flow::Nochange;
        }

        if self.cfg.useeditor {
            let np = b.newpath.clone();
            if self.get_output(b"file", Some(FILE_MIME_OPTS), Some(&np), None, false)
                && self.popup.g_buf.starts_with(b"text/")
            {
                let editor = self.editor.clone();
                self.spawn(&editor, Some(&np), None, None, F_CLI);
                self.resetfilter(&mut b.presel);
                return Flow::Top;
            }
        }

        // The extension for the archive regex match.
        let is_archive = xextension(&pent.name).is_some_and(|ext| {
            self.browse
                .archive_re
                .as_ref()
                .is_some_and(|re| re.is_match(ext))
        });
        if is_archive {
            let mut r = self.get_input(Some(MESSAGES[MSG_ARCHIVE_OPTS]));
            if r == i32::from(b'\r') {
                r = i32::from(b'l');
            }
            if r == i32::from(b'l') || r == i32::from(b'x') {
                b.newpath = mkpath(&path, &pent.name, &self.home).0;
                if !self.handle_archive(&mut b.newpath, r as u8) {
                    return Flow::MsgwaitNochange;
                }
                if r == i32::from(b'l') {
                    return Flow::StatusbarNochange;
                }
            }
            if r == i32::from(b'm') && !self.archive_mount(&mut b.newpath) {
                return Flow::MsgwaitNochange;
            }
            if r == i32::from(b'x') || r == i32::from(b'm') {
                if !b.newpath.is_empty() {
                    let np = b.newpath.clone();
                    let cn = self.curname();
                    self.set_smart_ctx(i32::from(b'+'), &np, cn.as_deref());
                } else {
                    self.copycurname();
                }
                self.clearfilter();
                return Flow::Begin;
            }
            if r != i32::from(b'o') {
                self.printwait(MESSAGES[MSG_INVALID_KEY], Some(&mut b.presel));
                return Flow::Nochange;
            }
        }

        // The desktop opener as the last resort.
        let mut flags = self.opener_flags();
        if self.opener_multi {
            flags |= F_MULTI;
        }
        let (opener, np) = (self.opener.clone(), b.newpath.clone());
        self.spawn(&opener, Some(&np), None, None, flags);

        // To the next entry unless this is the last one.
        if self.g_state.autonext && self.cur != self.ndents - 1 {
            self.move_cursor_next();
        }
        self.resetfilter(&mut b.presel);
        Flow::Top
    }

    /// `case SEL_CDHOME/CDBEGIN/CDLAST/CDROOT:` falling into `case SEL_BMOPEN:`
    fn sel_cd(&mut self, b: &mut Locals, sel: Action) -> Flow {
        let path = self.cpath();
        if sel != Action::Bmopen {
            let lastdir = self.g_ctx[self.cfg.curctx].c_last.clone();
            let mut dir = match sel {
                Action::Cdhome => self.home.clone(),
                Action::Cdbegin => b.ipath.clone(),
                Action::Cdlast => lastdir.clone(),
                _ => b"/".to_vec(),
            };
            if dir.is_empty() {
                self.printwait(MESSAGES[MSG_NOT_SET], Some(&mut b.presel));
                return Flow::Nochange;
            }
            self.g_state.selbm = false;
            if path == dir {
                if sel == Action::Cdbegin {
                    if self.cfg.filtermode {
                        b.presel = FILTER;
                    }
                    return Flow::Nochange;
                }
                // Go to the last dir on a home/root key repeat.
                dir = lastdir;
            }
            if !chdir(&dir) {
                return Flow::MsgwaitNochange;
            }
            b.newpath = ncpy(&dir, PATH_MAX);
        } else {
            let mark = self.mark.clone();
            let r = self.handle_bookmark(mark.as_deref(), &mut b.newpath);
            if r != 0 {
                self.printwait(MESSAGES[r], Some(&mut b.presel));
                return Flow::Nochange;
            }
            // Allow filtering in the bookmarks directory.
            if self.g_state.selbm {
                b.presel = FILTER;
            }
            if path == b.newpath {
                return Flow::Top;
            }
        }
        // In list mode, keep the last file name to highlight it, if possible.
        let keep = self.listpath.is_some() && sel == Action::Cdlast;
        let np = b.newpath.clone();
        if self.cdprep(!keep, &np) {
            b.presel = FILTER;
        } else {
            b.watch = true;
        }
        Flow::Begin
    }

    /// `case SEL_MFLTR/HIDDEN/DETAIL/PREVIEW/SORT:`
    fn sel_view(&mut self, b: &mut Locals, sel: Action) -> Flow {
        let r;
        match sel {
            Action::Mfltr | Action::Hidden => {
                if sel == Action::Mfltr {
                    self.cfg.filtermode = !self.cfg.filtermode;
                    if self.cfg.filtermode {
                        self.resetfilter(&mut b.presel);
                        return Flow::Nochange;
                    }
                    b.watch = true;
                } else {
                    self.cfg.sort.showhidden = !self.cfg.sort.showhidden;
                    if self.cfg.filtermode {
                        b.presel = FILTER;
                    }
                    self.clearfilter();
                }
                self.copycurname();
                return Flow::CdfalseBegin;
            }
            Action::Detail => {
                self.cfg.sort.showdetail = !self.cfg.sort.showdetail;
                self.cfg.sort.blkorder = false;
                return Flow::Top;
            }
            Action::Preview => {
                self.cfg.preview = !self.cfg.preview;
                return Flow::Top;
            }
            _ => {
                let key = self.get_input(Some(MESSAGES[MSG_ORDER]));
                r = self.set_sort_flags(key);
                if r == 0 {
                    self.printwait(MESSAGES[MSG_INVALID_KEY], Some(&mut b.presel));
                    return Flow::Nochange;
                }
            }
        }

        if self.cfg.filtermode || self.filterset() {
            b.presel = FILTER;
        }
        if self.ndents != 0 {
            self.copycurname();
            if r == i32::from(b'd') || r == i32::from(b'a') {
                b.presel = 0;
                return Flow::Begin;
            }
            // ENTSORT(pdents, ndents, entrycmpfn)
            let s = self.cfg.sort.clone();
            let n = self.ndents as usize;
            self.pdents[..n].sort_by(|x, y| s.cmp(x, y));
            let lastname = self.g_ctx[self.cfg.curctx].c_name.clone();
            let t = self.dentfind(&lastname, self.ndents);
            self.move_cursor(t, false);
        }
        Flow::Top
    }

    /// `case SEL_REDRAW/RENAMEMUL/HELP/AUTONEXT/EDIT/LOCK:`
    fn sel_misc(&mut self, b: &mut Locals, sel: Action) -> Flow {
        let mut refresh = false;
        let path = self.cpath();
        if self.ndents != 0 {
            let name = self.pdents[self.cur as usize].name.clone();
            b.newpath = mkpath(&path, &name, &self.home).0;
        } else if sel == Action::Edit {
            // Do not try to edit a file that is not there.
            return Flow::Nochange;
        }

        match sel {
            Action::Redraw => refresh = true,
            Action::Renamemul => {
                self.endselection(true);
                crate::locale::setenv("NNN_INCLUDE_HIDDEN", xitoa(self.cfg.sort.showhidden));
                crate::locale::setenv("NNN_PREFER_SELECTION", xitoa(self.cfg.prefersel));
                let list = if self.listpath.is_some() {
                    self.listroot.clone().unwrap_or_default()
                } else {
                    Vec::new()
                };
                crate::locale::setenv("NNN_LIST", &list);

                if !(self.getutil(UTIL_BASH) && self.plugscript(UTIL_NMV, F_CLI))
                    && !self.batch_rename()
                {
                    self.printwait(MESSAGES[MSG_FAILED], Some(&mut b.presel));
                    return Flow::Nochange;
                }
                self.clearselection();
                refresh = true;
            }
            Action::Help | Action::Autonext => {
                if sel == Action::Help {
                    self.show_help(&path);
                } else {
                    self.g_state.autonext = !self.g_state.autonext;
                }
                if self.cfg.filtermode {
                    b.presel = FILTER;
                }
                self.copycurname();
                return Flow::Nochange;
            }
            Action::Edit => {
                if !(self.g_state.picker || self.g_state.fifomode) {
                    let (editor, np) = (self.editor.clone(), b.newpath.clone());
                    self.spawn(&editor, Some(&np), None, None, F_CLI);
                }
                return Flow::Top;
            }
            _ => self.lock_terminal(),
        }

        // Continue in type-to-nav mode, if enabled.
        if (self.cfg.filtermode || self.filterset()) && !refresh {
            b.presel = FILTER;
            return Flow::Nochange;
        }
        // Save the current entry; repopulate, the directory may have changed.
        self.copycurname();
        Flow::CdfalseBegin
    }

    /// `case SEL_SEL:`
    fn sel_sel(&mut self, b: &mut Locals) -> Flow {
        if self.ndents == 0 {
            return Flow::Nochange;
        }
        self.startselection();
        if self.g_state.rangesel {
            self.g_state.rangesel = false;
        }
        let cur = self.cur as usize;
        // Toggle the selection status.
        self.pdents[cur].flags ^= FILE_SELECTED;
        let path = self.cpath();
        let (p, len) = mkpath(&path, &self.pdents[cur].name, &self.home);
        if self.pdents[cur].flags & FILE_SELECTED != 0 {
            self.nselected += 1;
            b.newpath.clone_from(&p);
            self.appendfpath(&p, len);
            // Without the NUL at the end.
            self.writesel(self.pselbuf.len().saturating_sub(1));
        } else {
            self.nselected -= 1;
            self.rmfromselbuf(&p);
        }
        self.copytoclipboard();
        if b.rightclicksel {
            b.rightclicksel = false;
        } else if !self.g_state.stayonsel && self.cur != self.ndents - 1 {
            // To the next entry unless this is the last one.
            self.move_cursor_next();
        }
        Flow::Top
    }

    /// `case SEL_SELMUL:` falling into `case SEL_SELALL/SELINV:`
    fn sel_range(&mut self, b: &mut Locals, sel: Action) -> Flow {
        if sel == Action::Selmul {
            if self.ndents == 0 {
                return Flow::Nochange;
            }
            self.startselection();
            self.g_state.rangesel = !self.g_state.rangesel;
            let path = self.cpath();
            let ino = match platform::stat(&path, true) {
                Ok(sb) => sb.ino,
                Err(_) => return Flow::WarnNochange,
            };
            if self.g_state.rangesel {
                // Range selection started.
                b.inode = ino;
                b.selstartid = self.cur;
                return Flow::Top;
            }
            if b.inode != ino {
                self.printwait(MESSAGES[MSG_DIR_CHANGED], Some(&mut b.presel));
                return Flow::Nochange;
            }
            if self.cur < b.selstartid {
                b.selendid = b.selstartid;
                b.selstartid = self.cur;
            } else {
                b.selendid = self.cur;
            }
            // Clear the selection on a repeat on the same file.
            if b.selstartid == b.selendid {
                self.resetselind();
                self.clearselection();
                return Flow::Top;
            }
        } else {
            if self.ndents == 0 {
                return Flow::Nochange;
            }
            self.startselection();
            if self.g_state.rangesel {
                self.g_state.rangesel = false;
            }
            b.selstartid = 0;
            b.selendid = self.ndents - 1;
        }

        if self.nselected > LARGESEL || (self.nselected != 0 && self.ndents > LARGESEL) {
            self.printmsg(b"processing...");
            self.cs().refresh();
        }
        let path = self.cpath();
        // The path with its '/'.
        let (_, prefix) = self.scanselforpath(&path, true);
        if sel == Action::Selinv && self.findselpos.is_some() {
            self.invertselbuf(&prefix);
        } else {
            self.addtoselbuf(&prefix, b.selstartid, b.selendid);
        }
        self.copytoclipboard();
        Flow::Top
    }

    /// `case SEL_CP/MV/CPMVAS/TRASH/RM_RF:`
    fn sel_cpmvrm(&mut self, b: &mut Locals, sel: Action) -> Flow {
        let path = self.cpath();
        if sel == Action::Trash || sel == Action::RmRf {
            let r = self.get_cur_or_sel();
            if r == 0 {
                return Flow::StatusbarNochange;
            }
            if r == i32::from(b'c') {
                let tmp = self.list_or_path(&path);
                let name = self.pdents[self.cur as usize].name.clone();
                b.newpath = mkpath(&tmp, &name, &self.home).0;
                let np = b.newpath.clone();
                if !self.xrm(&np, self.trashcmd.is_some() && sel == Action::Trash) {
                    return Flow::Top;
                }
                self.xrmfromsel(&tmp, &np);
                self.copynextname();
                if self.cfg.filtermode || self.filterset() {
                    b.presel = FILTER;
                }
                return Flow::CdfalseBegin;
            }
        }

        if self.nselected == 1 && (sel == Action::Cp || sel == Action::Mv) {
            let first = config::cstr(&self.pselbuf).to_vec();
            b.newpath = mkpath(&path, xbasename(&first), &self.home).0;
        } else {
            b.newpath.clear();
        }

        self.endselection(true);

        if !self.cpmvrm_selection(sel, &path) {
            return Flow::MsgwaitNochange;
        }

        if self.cfg.filtermode {
            b.presel = FILTER;
        }
        self.clearfilter();

        // A notification when the operation completes.
        if self.cfg.x11 {
            self.plugscript(UTIL_NTFY, F_NOWAIT | F_NOTRACE);
        }

        if !b.newpath.is_empty() && exists(&b.newpath) {
            let np = b.newpath.clone();
            self.set_lastname(xbasename(&np));
        } else {
            self.copycurname();
        }
        Flow::CdfalseBegin
    }

    /// `case SEL_ARCHIVE/OPENWITH/NEW/RENAME:`
    fn sel_create(&mut self, b: &mut Locals, sel: Action) -> Flow {
        let mut ret = i32::from(b'n');
        let mut r = 0;
        if self.ndents == 0 && (sel == Action::Openwith || sel == Action::Rename) {
            return Flow::Top;
        }
        if sel != Action::Openwith {
            self.endselection(true);
        }
        let path = self.cpath();
        let curname = self.curname();

        let mut tmp: Vec<u8> = match sel {
            Action::Archive => {
                r = self.get_cur_or_sel();
                if r == 0 {
                    return Flow::StatusbarNochange;
                }
                let pre = if r == i32::from(b's') {
                    if !self.selsafe() {
                        return Flow::MsgwaitNochange;
                    }
                    None
                } else {
                    curname.clone()
                };
                self.xreadline(pre.as_deref(), MESSAGES[MSG_ARCHIVE_NAME])
            }
            Action::Openwith => self.xreadline(None, MESSAGES[MSG_OPEN_WITH]),
            Action::New => {
                let pre = if b.pkey == 0 {
                    r = self.get_input(Some(MESSAGES[MSG_NEW_OPTS]));
                    if r == i32::from(b'\r') {
                        r = i32::from(b'f');
                    }
                    None
                } else {
                    r = i32::from(b'f');
                    b.pkey = 0;
                    Some(self.g_ctx[0].c_name.clone())
                };
                if r == i32::from(b'f') || r == i32::from(b'd') {
                    self.xreadline(pre.as_deref(), MESSAGES[MSG_NEW_PATH])
                } else if r == i32::from(b's') || r == i32::from(b'h') {
                    let first = config::cstr(&self.pselbuf).to_vec();
                    let pre = (self.nselected == 1 && self.cfg.prefersel)
                        .then(|| xbasename(&first).to_vec());
                    let m = if self.nselected <= 1 {
                        MSG_NEW_PATH
                    } else {
                        MSG_LINK_PREFIX
                    };
                    self.xreadline(pre.as_deref(), MESSAGES[m])
                } else {
                    Vec::new()
                }
            }
            _ => self.xreadline(curname.as_deref(), b""),
        };

        if tmp.is_empty() || is_bad_len_or_dir(&tmp) {
            return Flow::Top;
        }
        // Empty only for ARCHIVE and NEW in an empty directory.
        let cn = curname.clone().unwrap_or_default();

        match sel {
            Action::Archive => {
                // Cannot overwrite the hovered file.
                if r == i32::from(b'c') && tmp == cn {
                    return Flow::Top;
                }
                let Some(t) = sys::abspath_in(&tmp, None, Some(&self.home)) else {
                    return Flow::Top;
                };
                b.newpath.clone_from(&t);
                if exists(&t) && !xconfirm(self.get_input(Some(MESSAGES[MSG_OVERWRITE]))) {
                    return Flow::StatusbarNochange;
                }
                let cmd = self.get_archive_cmd(&t);
                if r == i32::from(b's') {
                    self.archive_selection(&cmd, &t, &path);
                } else {
                    self.spawn(&cmd, Some(&t), Some(&cn), None, F_CLI | F_CONFIRM);
                }
                if exists(&t) {
                    // File created.
                    if r == i32::from(b's') {
                        // Archive operation complete.
                        self.clearselection();
                    }
                    // Was an entry created in the current directory?
                    if let Some(e) = cwd_entry(&path, &t) {
                        self.set_lastname(&e);
                        // The archive name may not match.
                        self.clearfilter();
                    }
                    if self.cfg.filtermode {
                        b.presel = FILTER;
                    }
                    return Flow::CdfalseBegin;
                }
                return Flow::Top;
            }
            Action::Openwith => {
                self.handle_openwith(&path, &cn, &mut b.newpath, &tmp);
                self.filter_or_statusbar(b);
                self.copycurname();
                return Flow::Nochange;
            }
            Action::Rename => {
                r = 0;
                // Renaming to the same name: offer a copy.
                if tmp == cn {
                    let t = self.xreadline(Some(&cn), MESSAGES[MSG_COPY_NAME]);
                    if t.is_empty() || is_bad_len_or_dir(&t) || t == cn {
                        self.filter_or_statusbar(b);
                        self.copycurname();
                        return Flow::Nochange;
                    }
                    tmp = t;
                    ret = i32::from(b'd');
                }
            }
            _ => {}
        }

        if !(r == i32::from(b's') || r == i32::from(b'h')) {
            let Some(t) = sys::abspath_in(&tmp, Some(&path), Some(&self.home)) else {
                return Flow::WarnNochange;
            };
            b.newpath.clone_from(&t);
            tmp = t;
        }

        // Another file with the same name?
        if let Ok(sb) = std::fs::symlink_metadata(os(&tmp)) {
            if sel == Action::Rename || (r == i32::from(b'f') && sb.is_file()) {
                // Overwrite the file with the same name?
                if !xconfirm(self.get_input(Some(MESSAGES[MSG_OVERWRITE]))) {
                    return Flow::Top;
                }
            } else {
                // Nothing for SEL_NEW when a non-regular entry exists.
                self.printwait(MESSAGES[MSG_EXISTS], Some(&mut b.presel));
                return Flow::Nochange;
            }
        }

        if sel == Action::Rename {
            if ret == i32::from(b'd') {
                if cfg!(windows) && !crate::platform::has_coreutil(b"cp") {
                    let _ = crate::winops::copy_to(&cn, &tmp);
                } else {
                    self.spawn(b"cp -rp --", Some(&cn), Some(&tmp), None, F_SILENT);
                }
            } else if std::fs::rename(os(&cn), os(&tmp)).is_err() {
                return Flow::WarnNochange;
            }
            // Was an entry created in the current directory?
            if let Some(e) = cwd_entry(&path, &tmp) {
                self.set_lastname(&e);
            }
            // The directory must be reloaded for a rename.
        } else {
            b.presel = 0;
            if r == i32::from(b'f') || r == i32::from(b'd') {
                ret = i32::from(xmktree(&tmp, r == i32::from(b'd'), true));
            } else if r == i32::from(b's') || r == i32::from(b'h') {
                if self.nselected > 1 && tmp == b"@" {
                    tmp.clear();
                }
                ret = self.xlink(&tmp, &path, curname.as_deref(), &mut b.newpath, r);
            }
            if ret == 0 {
                self.printwarn(Some(&mut b.presel));
            }
            if ret <= 0 {
                return Flow::Nochange;
            }
            if r == i32::from(b'f') || r == i32::from(b'd') {
                match cwd_entry(&path, &tmp) {
                    Some(e) => self.set_lastname(&e),
                    // No change in the directory.
                    None => return Flow::Top,
                }
            } else if self.ndents != 0 {
                if self.cfg.filtermode {
                    b.presel = FILTER;
                }
                self.copycurname();
            }
        }
        self.clearfilter();
        Flow::CdfalseBegin
    }

    /// `case SEL_PLUGIN:`
    fn sel_plugin(&mut self, b: &mut Locals) -> Flow {
        // Is the plugin directory accessible?
        if !xdiraccess(&self.plgpath) {
            return Flow::WarnNochange;
        }
        self.lazy_parse_plug();
        let r = if b.pkey == 0 {
            let plug = self.plug.clone();
            self.get_kv_input(&plug, b" ?")
        } else {
            let k = b.pkey;
            b.pkey = 0;
            k
        };

        if r != i32::from(b'\r') {
            self.endselection(false);
            let tmp = if r == i32::from(b'?') {
                let plg = self.plgpath.clone();
                crate::locale::setenv("NNN_PLUGLIST", &plg);
                Some(b"!>\"$NNN_PLUGLIST/.pluglist\"".to_vec())
            } else {
                get_kv_val(&self.plug, r, NNN_PLUG, &self.home)
            };
            let Some(mut tmp) = tmp else {
                self.printwait(MESSAGES[MSG_INVALID_KEY], Some(&mut b.presel));
                return Flow::Nochange;
            };
            // A leading '-': do not refresh the dir after completion.
            let refresh = if tmp.first() == Some(&b'-') && tmp.len() > 1 {
                tmp.remove(0);
                false
            } else {
                true
            };
            if !self.script_plugin_runnable(&tmp, &mut b.presel) {
                return Flow::Nochange;
            }
            loop {
                let mut action: Option<Action> = None;
                let cn = self.curname();
                if !self.run_plugin(&tmp, cn.as_deref(), &mut b.runfile, Some(&mut action)) {
                    self.printwait(MESSAGES[MSG_FAILED], Some(&mut b.presel));
                    return Flow::Nochange;
                }
                if self.g_state.picked {
                    return Flow::Return(0);
                }
                if !action.is_some_and(|a| self.handle_cur_move(a)) {
                    break;
                }
            }
            if b.runfile.is_empty() {
                self.copycurname();
            }
            if !refresh {
                self.filter_or_statusbar(b);
                return Flow::Top;
            }
        } else {
            // Enter goes into the plugin directory.
            let cc = self.cfg.curctx;
            self.g_state.runplugin = !self.g_state.runplugin;
            let path = self.cpath();
            if !self.g_state.runplugin {
                // Toggled while still in the plugin dir: back to the original dir.
                if path == self.plgpath {
                    let ld = self.g_ctx[cc].c_last.clone();
                    self.g_ctx[cc].c_path = ncpy(&ld, PATH_MAX);
                    let rf = std::mem::take(&mut b.runfile);
                    self.set_lastname(&rf);
                    return Flow::SetwatchBegin;
                }
                // Otherwise choose a plugin again.
                self.g_state.runplugin = true;
            }
            self.g_ctx[cc].c_last = ncpy(&path, PATH_MAX);
            self.g_ctx[cc].c_path = ncpy(&self.plgpath, PATH_MAX);
            if self.ndents != 0 {
                b.runfile = ncpy(&self.pdents[self.cur as usize].name, NAME_MAX);
            }
            self.g_state.runctx = self.cfg.curctx;
            self.g_ctx[cc].c_name.clear();
            self.clearfilter();
        }
        self.setdirwatch(b);
        // Allow filtering in the plugins directory.
        if self.g_state.runplugin {
            b.presel = FILTER;
        }
        Flow::Begin
    }

    /// `case SEL_QUITCTX/QUITCD/QUIT/QUITERR:`
    fn sel_quit(&mut self, _b: &mut Locals, sel: Action) -> Flow {
        if sel == Action::Quitctx {
            let ctx = self.cfg.curctx;
            let mut r;
            // A valid last context to return to?
            if self.g_state.lastctx != CTX_INVALID
                && self.g_ctx[self.g_state.lastctx].c_cfg.ctxactive
            {
                r = self.g_state.lastctx;
                self.g_state.lastctx = CTX_INVALID;
            } else {
                // The previous active context.
                r = ctx.wrapping_sub(1) & (CTX_MAX - 1);
                while r != ctx && !self.g_ctx[r].c_cfg.ctxactive {
                    r = r.wrapping_sub(1) & (CTX_MAX - 1);
                }
            }
            if r != ctx {
                self.g_ctx[ctx].c_cfg = self.cfg.clone();
                self.g_ctx[ctx].c_name = self
                    .curname()
                    .map(|n| ncpy(&n, NAME_MAX + 1))
                    .unwrap_or_default();
                self.g_state.closedctx = ctx;
                self.g_ctx[ctx].c_cfg.ctxactive = false;

                // Switch to the next active context.
                self.g_ctx[r].c_cfg.curctx = r;
                let c = self.g_ctx[r].c_cfg.clone();
                self.setcfg(c);
                return Flow::SetwatchBegin;
            }
        } else if !self.g_state.forcequit {
            let mut r = CTX_MAX as i32;
            for i in 0..CTX_MAX {
                if i != self.cfg.curctx && self.g_ctx[i].c_cfg.ctxactive {
                    r = self.get_input(Some(MESSAGES[MSG_QUIT_ALL]));
                    break;
                }
            }
            if !(r == CTX_MAX as i32 || xconfirm(r)) {
                return Flow::Top;
            }
        }

        // CD on quit.
        let tmp = getenv("NNN_TMPFILE");
        if sel == Action::Quitcd || tmp.is_some() {
            let path = self.cpath();
            self.write_lastdir(&path, tmp.as_deref());
            // ^G quits picker mode without picking anything.
            if sel == Action::Quitcd && self.g_state.picker {
                self.pselbuf.clear();
            }
        }

        if sel != Action::Quiterr {
            return Flow::Return(0);
        }
        if !self.pselbuf.is_empty() && !self.g_state.picker {
            // Pick files to stdout and exit.
            self.g_state.picker = true;
            self.selpath = None;
            return Flow::Return(0);
        }
        Flow::Return(1)
    }

    /// `plugscript`: run a plugin of the plugin dir if it is executable.
    pub fn plugscript(&mut self, plugin: &[u8], flags: u16) -> bool {
        let (p, _) = mkpath(&self.plgpath, plugin, &self.home);
        if platform::access(&p, Access::Exec).is_ok() {
            self.spawn(&p, None, None, None, flags);
            return true;
        }
        false
    }

    /// `getutil`: the utility is in PATH (`which` succeeds; Windows, which has no `which`,
    /// searches PATH itself).
    pub fn getutil(&mut self, util: &[u8]) -> bool {
        if cfg!(windows) {
            return platform::which(util).is_some();
        }
        self.spawn(b"which", Some(util), None, None, F_NORMAL | F_NOTRACE) == 0
    }

    /// `copytoclipboard`: the selection to the clipboard (.cbcp) with -x.
    pub fn copytoclipboard(&mut self) {
        if self.cfg.x11 {
            self.plugscript(UTIL_CBCP, F_NOWAIT | F_NOTRACE);
        }
    }

    /// `confirm_force`: 'f' (forced), 'i' (interactive rm) or 0 (cancelled).
    pub fn confirm_force(&mut self, selection: bool, use_trash: bool) -> u8 {
        // Count the selection in the file when nothing is selected here.
        if selection && self.nselected == 0 {
            let Some(sp) = self.selpath.clone() else {
                self.printwarn(None);
                return 0;
            };
            match std::fs::File::open(os(&sp)) {
                Ok(mut f) => {
                    self.nselected = crate::fileops::entries_in_file(&mut f, 300, 0) as i32;
                }
                Err(_) => {
                    self.printwarn(None);
                    return 0;
                }
            }
        }
        let mut s: Vec<u8> = if use_trash {
            UTIL_GIO_TRASH[4..].to_vec()
        } else {
            b"rm -rf".to_vec()
        };
        let mut r;
        if selection {
            s.extend_from_slice(format!(" {} file(s)?", self.nselected).as_bytes());
            r = self.get_input(Some(&s));
            // Confirm again before removing more than 10 files.
            if !use_trash && self.nselected > RM_RECONFIRM && xconfirm(r) {
                r = self.get_input(Some(b"Are you sure?"));
            }
        } else {
            let mut tail = b" '".to_vec();
            tail.extend_from_slice(&self.pdents[self.cur as usize].name);
            tail.extend_from_slice(b"'?");
            tail.truncate(279);
            s.extend_from_slice(&tail);
            r = self.get_input(Some(&s));
        }
        if r == ESC {
            return 0;
        }
        if xconfirm(r) {
            return b'f';
        }
        if r == i32::from(b'n') || r == i32::from(b'N') {
            return 0;
        }
        if use_trash { 0 } else { b'i' }
    }

    /// `xrm`: remove (or trash) one file after confirmation; true when it is gone.
    pub fn xrm(&mut self, fpath: &[u8], use_trash: bool) -> bool {
        let r = self.confirm_force(false, use_trash);
        if r == 0 {
            return false;
        }
        if !use_trash {
            if cfg!(unix) || platform::has_coreutil(b"rm") {
                let opts = [b'-', b'v', b'r', r];
                self.spawn(
                    b"rm",
                    Some(&opts),
                    Some(b"--"),
                    Some(fpath),
                    F_NORMAL | crate::popup::F_CHKRTN,
                );
            } else {
                // Windows without Coreutils' rm: the entry is removed here.
                #[cfg(windows)]
                if let Err(e) = self.remove_native(fpath, r == b'f') {
                    self.printwait(sys::strerror(&e).as_bytes(), None);
                }
            }
        } else {
            let t = self.trashcmd.clone().unwrap_or_default();
            self.spawn(&t, Some(fpath), None, None, F_NORMAL | F_MULTI);
        }
        // Removed?
        std::fs::symlink_metadata(os(fpath)).is_err() && !exists(fpath)
    }

    /// `xrmfromsel`: drop a removed entry (or a removed dir's contents) from the selection.
    pub fn xrmfromsel(&mut self, path: &[u8], fpath: &[u8]) {
        let e = self.pdents[self.cur as usize].clone();
        let mut selected = true;
        if e.flags & DIR_OR_DIRLNK != 0 && self.scanselforpath(fpath, false).0 != 0 {
            self.clearselection();
        } else if e.flags & FILE_SELECTED != 0 {
            self.nselected -= 1;
            let (p, _) = mkpath(path, &e.name, &self.home);
            self.rmfromselbuf(&p);
        } else {
            selected = false;
        }
        if selected {
            self.copytoclipboard();
        }
    }

    /// `xlink`: symbolic ('s') or hard ('h') links to the current file or the selection;
    /// the number made, -1 when cancelled.
    pub fn xlink(
        &mut self,
        prefix: &[u8],
        path: &[u8],
        curfname: Option<&[u8]>,
        buf: &mut Vec<u8>,
        ty: i32,
    ) -> i32 {
        let choice = self.get_cur_or_sel();
        if choice == 0 {
            return -1;
        }
        let link = |src: &[u8], dst: &[u8]| {
            if ty == i32::from(b's') {
                let is_dir = cfg!(windows)
                    && platform::stat(src, true)
                        .is_ok_and(|i| i.mode & platform::S_IFMT == platform::S_IFDIR);
                platform::symlink(src, dst, is_dir)
            } else {
                std::fs::hard_link(os(src), os(dst))
            }
        };
        if choice == i32::from(b'c') || self.nselected == 1 {
            // The link path.
            let Some(lnpath) = sys::abspath_in(prefix, Some(path), Some(&self.home)) else {
                return -1;
            };
            // The target file path.
            if choice == i32::from(b'c') {
                *buf = mkpath(path, curfname.unwrap_or_default(), &self.home).0;
            }
            let target = if choice == i32::from(b'c') {
                buf.clone()
            } else {
                config::cstr(&self.pselbuf).to_vec()
            };
            if link(&target, &lnpath).is_ok() {
                if choice == i32::from(b's') {
                    self.clearselection();
                }
                return 1;
            }
            return 0;
        }

        let pre = ncpy(prefix, NAME_MAX + 1);
        let room = NAME_MAX.saturating_sub(pre.len() + 1);
        let mut count = 0;
        let sel: Vec<Vec<u8>> = self
            .pselbuf
            .split(|&c| c == 0)
            .filter(|p| !p.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        for psel in sel {
            // Suffix the target file name.
            let mut name = pre.clone();
            name.extend_from_slice(&ncpy(xbasename(&psel), room));
            *buf = name;
            let (lnpath, _) = mkpath(path, buf, &self.home);
            if link(&psel, &lnpath).is_ok() {
                count += 1;
            }
        }
        // Clear the selection when every link was made.
        if count == self.nselected {
            self.clearselection();
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(xextension(b"a.tar.gz"), Some(&b".gz"[..]));
        assert_eq!(xextension(b"abc"), None);
        assert_eq!(cwd_entry(b"/a", b"/a/b/c"), Some(b"b".to_vec()));
        assert_eq!(cwd_entry(b"/a", b"/x/b"), None);
        assert_eq!(ncpy(b"abcdef", 4), b"abc");
        assert_eq!(control(b'L'), 12);
        assert_eq!(abssub(3, 5), 2);
        assert_eq!(MESSAGES[MSG_INVALID_KEY], b"invalid key");
        assert_eq!(MESSAGES[MSG_ORDER][..4], *b"'a'u");
        assert!(xdiraccess(b"/"));
        assert!(!xdiraccess(b"/nonexistent-nnn"));
    }

    #[test]
    fn colors() {
        let mut f = [0u32; C_UND + 1];
        assert!(parse_fcolors(b"", &mut f));
        assert_eq!(f[C_BLK], 0xc1);
        assert_eq!(f[C_UND], 0xc4);
        let mut f = [0u32; C_UND + 1];
        assert!(!parse_fcolors(b"c1e", &mut f));
        let mut f = [0u32; C_UND + 1];
        assert_eq!(
            parse_ctx_colors(Some(b"1294"), false, 256, &mut f),
            Some([1, 2, 4, 4, 4, 4, 4, 4])
        );
        assert_eq!(f[3], 4);
        assert_eq!(f[5], 0);
        assert_eq!(
            parse_ctx_colors(Some(b"#0a1b;56"), false, 256, &mut f).map(|c| c[..3].to_vec()),
            Some(vec![0x0a, 0x1b, 4])
        );
        assert_eq!(
            parse_ctx_colors(Some(b"#0a1b;56"), false, 8, &mut f).map(|c| c[..3].to_vec()),
            Some(vec![5, 6, 4])
        );
        assert_eq!(
            parse_ctx_colors(Some(b"#0a1b"), true, 256, &mut f),
            Some([4; CTX_MAX])
        );
        assert!(parse_ctx_colors(Some(b"#0a1"), false, 256, &mut f).is_none());
    }

    #[test]
    fn text_files() {
        let dir = std::env::temp_dir().join(format!("nnn-browse-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = |n: &str, c: &[u8]| {
            let p = dir.join(n);
            std::fs::write(&p, c).unwrap();
            p.into_os_string().into_encoded_bytes()
        };
        assert!(is_text_file(&w("t", b"hello\n\tworld\x1b[0m\x0c")));
        assert!(!is_text_file(&w("p", b"%PDF-1.4")));
        assert!(!is_text_file(&w("n", b"ab\0c")));
        assert!(!is_text_file(&w("c", b"ab\x01c")));
        assert!(!is_text_file(&w("e", b"")));
        assert!(!is_text_file(&w("m", b"\xff\xfb")));
        assert!(!is_text_file(b"/nonexistent-nnn"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fgets_splits_long_lines() {
        let data = vec![b'a'; 5000];
        let mut r = BufReader::new(&data[..]);
        assert_eq!(fgets(&mut r).unwrap().len(), 4095);
        assert_eq!(fgets(&mut r).unwrap().len(), 905);
        assert!(fgets(&mut r).is_none());
        let mut r = BufReader::new(&b"x\ny"[..]);
        assert_eq!(fgets(&mut r).unwrap(), b"x\n");
        assert_eq!(fgets(&mut r).unwrap(), b"y");
    }

    // Windows has no FIFOs: NNN_FIFO is refused (rust/app-cases-windows/fifo-unsupported).
    #[cfg(unix)]
    #[test]
    fn fifo_checks() {
        let dir = std::env::temp_dir().join(format!("nnn-fifo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fp = dir.join("f").into_os_string().into_encoded_bytes();
        assert_eq!(make_fifo(&fp), Ok(()));
        // An existing FIFO is fine.
        assert_eq!(make_fifo(&fp), Ok(()));
        let reg = dir.join("r");
        std::fs::write(&reg, b"").unwrap();
        assert_eq!(
            make_fifo(&reg.into_os_string().into_encoded_bytes()),
            Err(1)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
