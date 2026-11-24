//! The Unix backend: byte paths are the OS's own bytes.

use super::{DType, Defaults, Errno, FileInfo, FsBlocks, ReadResult, SignalStep};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fd::{AsFd, OwnedFd};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, Stat};
use rustix::termios::{OptionalActions, Termios};
use std::borrow::Cow;
use std::ffi::{OsStr, c_char};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const NULL_DEVICE: &[u8] = b"/dev/null";
pub const TTY_DEVICE: &[u8] = b"/dev/tty";
pub const TTY_OUT_DEVICE: &[u8] = b"/dev/tty";

pub fn os(b: &[u8]) -> Cow<'_, OsStr> {
    Cow::Borrowed(OsStr::from_bytes(b))
}

pub fn bytes(s: &OsStr) -> Cow<'_, [u8]> {
    Cow::Borrowed(s.as_bytes())
}

fn info(sb: &Stat) -> FileInfo {
    FileInfo {
        mode: sb.st_mode,
        size: sb.st_size,
        nlink: sb.st_nlink,
        uid: sb.st_uid,
        gid: sb.st_gid,
        ino: sb.st_ino,
        dev: sb.st_dev,
        blocks512: sb.st_blocks as u64,
        atime: (sb.st_atime, sb.st_atime_nsec as u32),
        ctime: (sb.st_ctime, sb.st_ctime_nsec as u32),
        mtime: (sb.st_mtime, sb.st_mtime_nsec as u32),
    }
}

fn at_flags(follow: bool) -> AtFlags {
    if follow {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    }
}

pub fn stat(path: &[u8], follow: bool) -> io::Result<FileInfo> {
    Ok(info(&rustix::fs::statat(
        CWD,
        &*os(path),
        at_flags(follow),
    )?))
}

/// An open directory: names are looked up relative to it (`openat`, `fstatat`).
pub struct Dir {
    fd: OwnedFd,
}

impl Dir {
    pub fn open(path: &[u8]) -> io::Result<Dir> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        Ok(Dir {
            fd: rustix::fs::open(&*os(path), flags, Mode::empty())?,
        })
    }

    /// `stat` of `name` (a path relative to the directory; an absolute path ignores it).
    pub fn stat_at(&self, name: &[u8], follow: bool) -> io::Result<FileInfo> {
        Ok(info(&rustix::fs::statat(
            self.fd.as_fd(),
            &*os(name),
            at_flags(follow),
        )?))
    }

    /// The entries in directory order, without `.` and `..`.
    pub fn entries(&self) -> io::Result<Entries> {
        Ok(Entries {
            dir: rustix::fs::Dir::read_from(&self.fd)?,
            dots: false,
        })
    }

    /// The entries as `readdir` gives them, `.` and `..` included.
    pub fn raw_entries(&self) -> io::Result<Entries> {
        Ok(Entries {
            dir: rustix::fs::Dir::read_from(&self.fd)?,
            dots: true,
        })
    }
}

pub struct Entries {
    dir: rustix::fs::Dir,
    /// `.` and `..` are listed too.
    dots: bool,
}

impl Iterator for Entries {
    type Item = io::Result<(Vec<u8>, DType)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let e = match self.dir.next()? {
                Ok(e) => e,
                Err(err) => return Some(Err(err.into())),
            };
            let name = e.file_name().to_bytes();
            if !self.dots && (name == b"." || name == b"..") {
                continue;
            }
            let ty = match e.file_type() {
                FileType::Directory => DType::Dir,
                FileType::Symlink => DType::Symlink,
                FileType::RegularFile => DType::Regular,
                FileType::Unknown => DType::Unknown,
                _ => DType::Other,
            };
            return Some(Ok((name.to_vec(), ty)));
        }
    }
}

pub fn readlink(path: &[u8]) -> io::Result<Vec<u8>> {
    Ok(rustix::fs::readlink(&*os(path), Vec::new())?.into_bytes())
}

pub fn symlink(target: &[u8], link: &[u8], _is_dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(&*os(target), &*os(link))
}

/// `/tmp` if it can be read, else `$TMPDIR`.
pub fn temp_dir() -> Option<Vec<u8>> {
    if std::fs::read_dir("/tmp").is_ok() {
        return Some(b"/tmp".to_vec());
    }
    std::env::var_os("TMPDIR").map(std::ffi::OsString::into_encoded_bytes)
}

pub fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME").map(std::ffi::OsString::into_encoded_bytes)
}

fn name_from(p: *const c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: p points into the buffer getpwuid_r/getgrgid_r filled, which is still alive in the
    // caller, with a NUL-terminated name.
    Some(unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().to_vec())
}

/// `getpwuid(uid)->pw_name`
pub fn user_name(uid: u32) -> Option<Vec<u8>> {
    let mut buf = vec![0 as c_char; 16384];
    // SAFETY: passwd is plain data; getpwuid_r fills it and points its strings into buf.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call and buf.len() is its size.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    name_from(pw.pw_name)
}

/// `getgrgid(gid)->gr_name`
pub fn group_name(gid: u32) -> Option<Vec<u8>> {
    let mut buf = vec![0 as c_char; 16384];
    // SAFETY: group is plain data; getgrgid_r fills it and points its strings into buf.
    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::group = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call and buf.len() is its size.
    let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr(), buf.len(), &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    name_from(gr.gr_name)
}

pub fn private_mode(o: &mut OpenOptions) -> &mut OpenOptions {
    o.mode(0o600)
}

/// `max_openfds`: raise the soft limit on open files to the hard limit.
pub fn raise_open_files_limit() {
    use rustix::process::{Resource, getrlimit, setrlimit};
    let mut rl = getrlimit(Resource::Nofile);
    let below = match (rl.current, rl.maximum) {
        (Some(cur), Some(max)) => cur < max,
        (Some(_), None) => true,
        _ => false,
    };
    if below {
        rl.current = rl.maximum;
        let _ = setrlimit(Resource::Nofile, rl);
    }
}

pub fn error(errno: Errno) -> io::Error {
    io::Error::from_raw_os_error(raw_errno(errno))
}

fn raw_errno(errno: Errno) -> i32 {
    match errno {
        Errno::NoEnt => libc::ENOENT,
        Errno::Exist => libc::EEXIST,
        Errno::Inval => libc::EINVAL,
        Errno::NameTooLong => libc::ENAMETOOLONG,
        Errno::Io => libc::EIO,
        Errno::Fault => libc::EFAULT,
        Errno::NxIo => libc::ENXIO,
        Errno::NotEmpty => libc::ENOTEMPTY,
    }
}

pub fn is_errno(e: &io::Error, n: Errno) -> bool {
    e.raw_os_error() == Some(raw_errno(n))
}

pub fn access(path: &[u8], mode: super::Access) -> io::Result<()> {
    use rustix::fs::Access as A;
    let flags = match mode {
        super::Access::Exists => A::EXISTS,
        super::Access::Write => A::WRITE_OK,
        super::Access::ReadWrite => A::READ_OK | A::WRITE_OK,
        super::Access::Exec => A::EXEC_OK,
    };
    Ok(rustix::fs::access(&*os(path), flags)?)
}

pub fn mkdir(path: &[u8], mode: u32) -> io::Result<()> {
    Ok(rustix::fs::mkdir(&*os(path), Mode::from_raw_mode(mode))?)
}

pub fn mkfifo(path: &[u8]) -> io::Result<()> {
    Ok(rustix::fs::mknodat(
        CWD,
        &*os(path),
        FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )?)
}

pub fn open_fifo_write(path: &[u8]) -> io::Result<File> {
    let f = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&*os(path))?;
    match f.metadata() {
        Ok(m) if m.file_type().is_fifo() => Ok(f),
        _ => Err(error(Errno::Inval)),
    }
}

pub fn open_fifo_pair(path: &[u8]) -> (Option<File>, Option<File>) {
    use rustix::fs::{fcntl_getfl, fcntl_setfl, open};
    let rfd = open(
        &*os(path),
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok();
    let wfd = rfd.as_ref().and_then(|_| {
        open(
            &*os(path),
            OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()
    });
    for fd in rfd.iter().chain(wfd.iter()) {
        if let Ok(fl) = fcntl_getfl(fd) {
            let _ = fcntl_setfl(fd, fl - OFlags::NONBLOCK);
        }
    }
    (rfd.map(File::from), wfd.map(File::from))
}

pub fn stdin_to_tty() {
    let stdout = rustix::stdio::stdout();
    if !rustix::termios::isatty(stdout) {
        if let Ok(fd) =
            rustix::fs::open(&*os(TTY_DEVICE), OFlags::RDONLY, Mode::from_raw_mode(0o400))
        {
            let _ = rustix::stdio::dup2_stdin(fd.as_fd());
        }
    } else {
        let _ = rustix::stdio::dup2_stdin(stdout);
    }
}

pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
    Ok(rustix::io::read(rustix::stdio::stdin(), buf)?)
}

pub fn parent_is_init() -> bool {
    std::os::unix::process::parent_id() == 1
}

pub fn detach(cmd: &mut Command) {
    cmd.process_group(0);
}

pub fn wait_status(st: ExitStatus) -> i32 {
    st.code().unwrap_or_else(|| st.into_raw())
}

pub const SUPPORTS_FIFO: bool = true;
pub const SUPPORTS_MOUNTS: bool = true;
pub const SUPPORTS_CHMOD: bool = true;

pub fn defaults() -> Defaults {
    Defaults {
        opener: b"xdg-open".to_vec(),
        editor: b"vi".to_vec(),
        pager: b"less".to_vec(),
        shell: b"sh".to_vec(),
    }
}

pub fn shell_exec_prefix() -> &'static [u8] {
    b"sh -c"
}

pub fn has_sh() -> bool {
    true
}

pub fn which(_name: &[u8]) -> Option<Vec<u8>> {
    None
}

pub fn coreutils() -> Option<&'static super::tools::Coreutils> {
    None
}

pub fn cmd_args(cmd: &mut Command, words: &[Vec<u8>], added: &[Vec<u8>]) {
    cmd.args(words.iter().chain(added).map(|a| os(a)));
}

pub fn set_pipe_nonblocking(r: &io::PipeReader, w: &io::PipeWriter) {
    for fd in [r.as_fd(), w.as_fd()] {
        if let Ok(fl) = rustix::fs::fcntl_getfl(fd) {
            let _ = rustix::fs::fcntl_setfl(fd, fl | OFlags::NONBLOCK);
        }
    }
}

/// The kernel's host name, without its trailing newline.
pub fn hostname() -> Vec<u8> {
    std::fs::read("/proc/sys/kernel/hostname")
        .map(|mut h| {
            while h.last() == Some(&b'\n') {
                h.pop();
            }
            h
        })
        .unwrap_or_default()
}

/// `spawn(utils[UTIL_RM_RF], path, NULL, NULL, F_NOTRACE | F_MULTI)`
pub fn remove_tree(path: &[u8]) {
    let _ = Command::new("rm")
        .arg("-rf")
        .arg(&*os(path))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn fs_blocks(path: &[u8]) -> Option<FsBlocks> {
    let svb = rustix::fs::statvfs(&*os(path)).ok()?;
    Some(FsBlocks {
        frsize: svb.f_frsize,
        blocks: svb.f_blocks,
        bfree: svb.f_bfree,
        bavail: svb.f_bavail,
    })
}

pub fn toggle_exec(path: &[u8]) -> Option<u32> {
    let fd = rustix::fs::open(&*os(path), OFlags::RDONLY | OFlags::NOFOLLOW, Mode::empty()).ok()?;
    let sb = rustix::fs::fstat(&fd).ok()?;
    let mut mode = sb.st_mode;
    // (Un)set (S_IXUSR | S_IXGRP | S_IXOTH)
    if mode & 0o100 != 0 {
        mode &= !0o111;
    } else {
        mode |= 0o111;
    }
    rustix::fs::fchmod(&fd, Mode::from_bits_retain(mode & 0o7777)).ok()?;
    Some(mode)
}

/// `INOTIFY_MASK`
const INOTIFY_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MODIFY
    | libc::IN_MOVE_SELF
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO;
/// `EVENT_BUF_LEN`: 32 slots of `struct inotify_event`.
const EVENT_BUF_LEN: usize = 16 * 32;

/// The directory watch (`inotify_fd` and `inotify_wd`, `None` for -1).
pub struct DirWatch {
    fd: OwnedFd,
    wd: Option<i32>,
}

impl DirWatch {
    /// `inotify_init1(IN_NONBLOCK | IN_CLOEXEC)`
    pub fn new() -> io::Result<DirWatch> {
        use rustix::fs::inotify::CreateFlags;
        Ok(DirWatch {
            fd: rustix::fs::inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)?,
            wd: None,
        })
    }

    /// `inotify_wd = inotify_add_watch(inotify_fd, path, INOTIFY_MASK)`
    pub fn watch(&mut self, path: &[u8]) {
        self.wd = rustix::fs::inotify::add_watch(
            &self.fd,
            &*os(path),
            rustix::fs::inotify::WatchFlags::from_bits_retain(INOTIFY_MASK),
        )
        .ok();
    }

    /// `inotify_rm_watch(inotify_fd, inotify_wd); inotify_wd = -1;`
    pub fn unwatch(&mut self) {
        if let Some(wd) = self.wd.take() {
            let _ = rustix::fs::inotify::remove_watch(&self.fd, wd);
        }
    }

    /// `inotify_wd >= 0`
    pub fn is_watching(&self) -> bool {
        self.wd.is_some()
    }

    /// The read of `nextsel()`: the watched directory changed.
    pub fn changed(&mut self) -> bool {
        if self.wd.is_none() {
            return false;
        }
        let mut buf = [0u8; EVENT_BUF_LEN];
        let Ok(n) = rustix::io::read(&self.fd, &mut buf) else {
            return false;
        };
        let field = |at: usize| -> Option<u32> {
            buf.get(at..at + 4)
                .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        };
        let mut ptr = 0usize;
        // for (ptr; ptr + ev->len < buf + i; ptr += sizeof(*ev) + ev->len)
        while let Some(len) = field(ptr + 12) {
            if ptr + len as usize >= n {
                break;
            }
            let (Some(wd), Some(mask)) = (field(ptr), field(ptr + 4)) else {
                return false;
            };
            if wd == 0 {
                break;
            }
            if mask & INOTIFY_MASK != 0 {
                return true;
            }
            ptr += 16 + len as usize;
        }
        false
    }
}

pub fn register_signals(
    int: Arc<AtomicBool>,
    term: Arc<AtomicBool>,
) -> Result<(), (SignalStep, io::Error)> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use signal_hook::flag::register;
    register(SIGINT, int).map_err(|e| (SignalStep::Interrupt, e))?;
    for sig in [SIGTERM, SIGHUP] {
        register(sig, term.clone()).map_err(|e| (SignalStep::Terminate, e))?;
    }
    // SIG_IGN: a handler that only sets a flag nobody reads.
    register(SIGQUIT, Arc::new(AtomicBool::new(false))).map_err(|e| (SignalStep::Quit, e))?;
    Ok(())
}

/// ncurses' modes: cbreak (ISIG kept, VMIN 1, VTIME 0), noecho, nonl.
fn curses_modes(saved: &Termios) -> Termios {
    use rustix::termios::{InputModes, LocalModes, SpecialCodeIndex};
    let mut t = saved.clone();
    t.local_modes &= !(LocalModes::ICANON | LocalModes::ECHO);
    t.local_modes |= LocalModes::ISIG;
    t.input_modes &= !InputModes::ICRNL;
    t.special_codes[SpecialCodeIndex::VMIN] = 1;
    t.special_codes[SpecialCodeIndex::VTIME] = 0;
    t
}

/// The terminal on `/dev/stdin`; SIGWINCH sets `resized`.
pub struct Term {
    tty: File,
    saved: Termios,
    active: bool,
    resized: Arc<AtomicBool>,
}

impl Term {
    pub fn open() -> io::Result<Term> {
        let tty = OpenOptions::new().read(true).open("/dev/stdin")?;
        let saved = rustix::termios::tcgetattr(&tty)?;
        rustix::termios::tcsetattr(&tty, OptionalActions::Drain, &curses_modes(&saved))?;
        let resized = Arc::new(AtomicBool::new(false));
        let _ = signal_hook::flag::register(signal_hook::consts::SIGWINCH, resized.clone());
        Ok(Term {
            tty,
            saved,
            active: true,
            resized,
        })
    }

    pub fn size(&self) -> (i32, i32) {
        let ws = rustix::termios::tcgetwinsize(&self.tty).ok();
        super::term_size(ws.map(|w| (i32::from(w.ws_row), i32::from(w.ws_col))))
    }

    /// Waits up to `wait_ms` (-1: no limit) for input; a SIGWINCH ends the wait.
    pub fn read(&mut self, wait_ms: i32, out: &mut Vec<u8>) -> ReadResult {
        let mut pfd = [PollFd::new(&self.tty, PollFlags::IN)];
        let t = (wait_ms >= 0).then(|| Timespec {
            tv_sec: i64::from(wait_ms / 1000),
            tv_nsec: i64::from(wait_ms % 1000) * 1_000_000,
        });
        if let Ok(n) = rustix::event::poll(&mut pfd, t.as_ref())
            && n > 0
        {
            let mut buf = [0u8; 256];
            if let Ok(n) = (&self.tty).read(&mut buf)
                && n > 0
            {
                out.extend_from_slice(&buf[..n]);
                return ReadResult::Data;
            }
        }
        if self.resized.load(Ordering::Relaxed) {
            ReadResult::Resized
        } else {
            ReadResult::Timeout
        }
    }

    /// A resize not reported yet.
    pub fn resize_pending(&self) -> bool {
        self.resized.load(Ordering::Relaxed)
    }

    pub fn set_resize_pending(&mut self, on: bool) {
        self.resized.store(on, Ordering::Relaxed);
    }

    /// The saved modes back (`endwin`).
    pub fn restore(&mut self) {
        if self.active {
            let _ = rustix::termios::tcsetattr(&self.tty, OptionalActions::Drain, &self.saved);
            self.active = false;
        }
    }

    /// Curses' modes again after `restore`.
    pub fn resume(&mut self) {
        if !self.active {
            let _ = rustix::termios::tcsetattr(
                &self.tty,
                OptionalActions::Drain,
                &curses_modes(&self.saved),
            );
            self.active = true;
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.restore();
    }
}

/// The glibc functions whose results depend on the locale, the time zone or POSIX regex syntax:
/// the original calls them, so the port calls the same ones (ADR-014). The portable backend
/// (`portable_locale.rs`) replaces them when the `portable` feature is on.
#[cfg(not(feature = "portable"))]
pub mod glibc {
    use crate::locale::{LocalTime, Wide};
    use std::ffi::{CString, c_char, c_int};

    unsafe extern "C" {
        fn towlower(wc: u32) -> u32;
        fn iswalnum(wc: u32) -> c_int;
        fn wcswidth(s: *const libc::wchar_t, n: libc::size_t) -> c_int;
        #[link_name = "mbstowcs"]
        fn c_mbstowcs(
            dest: *mut libc::wchar_t,
            src: *const c_char,
            n: libc::size_t,
        ) -> libc::size_t;
        #[link_name = "mblen"]
        fn c_mblen(s: *const c_char, n: libc::size_t) -> c_int;
    }

    fn cstr(s: &[u8]) -> CString {
        // INFALLIBLE: names and filters come from C strings and hold no NUL; cut at one if they do.
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        CString::new(&s[..end]).unwrap_or_default()
    }

    /// `setlocale(LC_COLLATE, ""); setlocale(LC_CTYPE, "")`, as `main()` does.
    pub fn init() {
        // SAFETY: called once from main before any other thread starts; the argument is a
        // valid NUL-terminated string.
        unsafe {
            libc::setlocale(libc::LC_COLLATE, c"".as_ptr());
            libc::setlocale(libc::LC_CTYPE, c"".as_ptr());
        }
    }

    pub fn strcoll(a: &[u8], b: &[u8]) -> i32 {
        let (a, b) = (cstr(a), cstr(b));
        // SAFETY: both pointers are valid NUL-terminated strings for the call.
        unsafe { libc::strcoll(a.as_ptr(), b.as_ptr()) }
    }

    pub fn strcasecmp(a: &[u8], b: &[u8]) -> i32 {
        let (a, b) = (cstr(a), cstr(b));
        // SAFETY: both pointers are valid NUL-terminated strings for the call.
        unsafe { libc::strcasecmp(a.as_ptr(), b.as_ptr()) }
    }

    /// `tolower()` on one byte, as glibc's `strcasestr` folds.
    pub fn tolower(c: u8) -> u8 {
        // SAFETY: tolower takes any unsigned char value.
        (unsafe { libc::tolower(c_int::from(c)) }) as u8
    }

    /// `mbstowcs(dst, s, max)`: at most `max` wide characters, `None` for `(size_t)-1`.
    pub fn mbstowcs(s: &[u8], max: usize) -> Option<Wide> {
        let src = cstr(s);
        let mut buf: Vec<libc::wchar_t> = vec![0; max + 1];
        // SAFETY: buf holds max + 1 wide chars and mbstowcs writes at most max of them;
        // src is NUL-terminated.
        let n = unsafe { c_mbstowcs(buf.as_mut_ptr(), src.as_ptr(), max) };
        if n == usize::MAX {
            return None;
        }
        buf.truncate(n);
        Some(buf)
    }

    /// `mblen(s, MB_LEN_MAX)` at the start of `s`.
    pub fn mblen_at(s: &[u8]) -> i32 {
        let src = cstr(s);
        // SAFETY: src is NUL-terminated, so mblen never reads past it.
        unsafe { c_mblen(src.as_ptr(), 16) }
    }

    pub fn to_lower_wide(c: i32) -> i32 {
        // SAFETY: towlower takes any wint_t.
        (unsafe { towlower(c as u32) }) as i32
    }

    /// `wcswidth(s, n)`: -1 when a character in the first `n` is not printable.
    pub fn wcswidth_n(s: &[i32], n: usize) -> i32 {
        let mut buf: Vec<libc::wchar_t> = s.to_vec();
        buf.push(0);
        // SAFETY: buf is NUL-terminated and wcswidth stops at the NUL or after n chars.
        unsafe { wcswidth(buf.as_ptr(), n) }
    }

    /// `wcstombs`: the wide string back to the locale's bytes, `None` for `(size_t)-1`.
    pub fn wcstombs(s: &[i32]) -> Option<Vec<u8>> {
        let mut buf: Vec<libc::wchar_t> = s.to_vec();
        buf.push(0);
        let mut out = vec![0u8; s.len() * 16 + 1];
        // SAFETY: out has room for every char at the maximum multibyte length plus the NUL.
        let n = unsafe { libc::wcstombs(out.as_mut_ptr().cast(), buf.as_ptr(), out.len()) };
        if n == usize::MAX {
            return None;
        }
        out.truncate(n);
        Some(out)
    }

    pub fn localtime(secs: i64) -> LocalTime {
        // SAFETY: tm is plain data; zeroed is a valid value for every field.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t: libc::time_t = secs;
        // SAFETY: both pointers are valid for the call.
        unsafe { libc::localtime_r(&t, &mut tm) };
        LocalTime {
            year: tm.tm_year + 1900,
            mon: tm.tm_mon + 1,
            mday: tm.tm_mday,
            hour: tm.tm_hour,
            min: tm.tm_min,
            sec: tm.tm_sec,
        }
    }

    /// A compiled POSIX regex (`regcomp` with `REG_NOSUB | REG_EXTENDED [| REG_ICASE]`).
    pub struct Regex(Box<libc::regex_t>);

    impl Regex {
        pub fn new(pattern: &[u8], icase: bool) -> Option<Regex> {
            let p = cstr(pattern);
            // SAFETY: regex_t is plain data that regcomp initialises.
            let mut re: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
            let flags =
                libc::REG_NOSUB | libc::REG_EXTENDED | if icase { libc::REG_ICASE } else { 0 };
            // SAFETY: re points to writable storage for a regex_t, p is NUL-terminated.
            let rc = unsafe { libc::regcomp(&mut *re, p.as_ptr(), flags) };
            (rc == 0).then_some(Regex(re))
        }

        pub fn is_match(&self, s: &[u8]) -> bool {
            let s = cstr(s);
            // SAFETY: self.0 was compiled by regcomp; REG_NOSUB needs no match array.
            unsafe { libc::regexec(&*self.0, s.as_ptr(), 0, std::ptr::null_mut(), 0) == 0 }
        }
    }

    impl Drop for Regex {
        fn drop(&mut self) {
            // SAFETY: self.0 was compiled by regcomp and is freed once.
            unsafe { libc::regfree(&mut *self.0) };
        }
    }

    /// glibc `iswalnum` in the current LC_CTYPE.
    pub fn is_walnum(c: i32) -> bool {
        // SAFETY: iswalnum takes any wint_t.
        (unsafe { iswalnum(c as u32) }) != 0
    }
}

/// `setenv(name, value, 1)`: variables nnn exports to the programs it starts.
pub fn setenv(name: &str, value: &[u8]) {
    let v = &*os(value);
    // SAFETY: nnn sets these from its main thread at points where no other thread reads or
    // writes the environment (the du workers never touch it), as the C original does.
    unsafe { std::env::set_var(name, v) };
}

/// `unsetenv(name)`
pub fn unsetenv(name: &str) {
    // SAFETY: as for setenv.
    unsafe { std::env::remove_var(name) };
}
