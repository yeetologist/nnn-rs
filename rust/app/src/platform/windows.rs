//! The Windows backend: byte paths are WTF-8 (`OsStr::as_encoded_bytes`), shown with `/`.

use super::{Access, DType, Defaults, Errno, FileInfo, FsBlocks, ReadResult, SignalStep};
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, IntoRawHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{FALSE, HANDLE, TRUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_STANDARD_INFO, FileBasicInfo,
    FileStandardInfo, GetDiskFreeSpaceExW, GetFileInformationByHandle,
    GetFileInformationByHandleEx,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT,
    CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle,
    INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP,
    SetStdHandle, WINDOW_BUFFER_SIZE_EVENT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CreateEventW, DETACHED_PROCESS, INFINITE, ResetEvent, SetEvent,
    WaitForMultipleObjects,
};
use windows_sys::core::BOOL;

pub const NULL_DEVICE: &[u8] = b"NUL";
pub const TTY_DEVICE: &[u8] = b"CONIN$";
pub const TTY_OUT_DEVICE: &[u8] = b"CONOUT$";

/// Decodes WTF-8 (UTF-8 that may hold unpaired surrogates) to UTF-16; a byte that is not part of
/// a sequence becomes U+FFFD.
fn wtf8_to_wide(b: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let (len, init) = match c {
            0x00..=0x7F => (1, u32::from(c)),
            0xC2..=0xDF => (2, u32::from(c & 0x1F)),
            0xE0..=0xEF => (3, u32::from(c & 0x0F)),
            0xF0..=0xF4 => (4, u32::from(c & 0x07)),
            _ => (0, 0),
        };
        let cont = b
            .get(i + 1..i + len)
            .filter(|t| t.iter().all(|&x| x & 0xC0 == 0x80));
        let cp = match (len, cont) {
            (0, _) | (_, None) => None,
            (_, Some(tail)) => {
                let cp = tail.iter().fold(init, |a, &x| a << 6 | u32::from(x & 0x3F));
                let min = [0, 0, 0x80, 0x800, 0x10000][len];
                (cp >= min && cp <= 0x10FFFF).then_some(cp)
            }
        };
        match cp {
            Some(cp) => {
                i += len;
                if cp >= 0x10000 {
                    let v = cp - 0x10000;
                    out.push(0xD800 | (v >> 10) as u16);
                    out.push(0xDC00 | (v & 0x3FF) as u16);
                } else {
                    out.push(cp as u16);
                }
            }
            None => {
                i += 1;
                out.push(0xFFFD);
            }
        }
    }
    out
}

/// `MAX_PATH`: an absolute path this long (in bytes, never fewer than its UTF-16 units) is
/// passed in its `\\?\` form, which has no length limit.
const MAX_PATH: usize = 260;

/// `C:/x` as `\\?\C:\x`, `//srv/share/x` as `\\?\UNC\srv\share\x` (the form takes
/// `\` separators only).
fn verbatim(b: &[u8]) -> OsString {
    let mut v: Vec<u8> = match b.strip_prefix(b"//") {
        Some(unc) => [&b"\\\\?\\UNC\\"[..], unc].concat(),
        None => [&b"\\\\?\\"[..], b].concat(),
    };
    for c in &mut v {
        if *c == b'/' {
            *c = b'\\';
        }
    }
    OsString::from_wide(&wtf8_to_wide(&v))
}

pub fn os(b: &[u8]) -> Cow<'_, OsStr> {
    if b.len() >= MAX_PATH && super::is_absolute(b) && !b.starts_with(b"//?/") {
        return Cow::Owned(verbatim(b));
    }
    match std::str::from_utf8(b) {
        Ok(s) => Cow::Borrowed(OsStr::new(s)),
        Err(_) => Cow::Owned(OsString::from_wide(&wtf8_to_wide(b))),
    }
}

pub fn bytes(s: &OsStr) -> Cow<'_, [u8]> {
    let b = s.as_encoded_bytes();
    if b.contains(&b'\\') {
        let slashed: Vec<u8> = b
            .iter()
            .map(|&c| if c == b'\\' { b'/' } else { c })
            .collect();
        // The `\\?\` form `os` gives long paths reads back as the usual one.
        Cow::Owned(strip_verbatim(slashed))
    } else {
        Cow::Borrowed(b)
    }
}

/// Seconds and nanoseconds since 1970 of a Windows time (100 ns ticks since 1601).
fn unix_time(ticks: i64) -> (i64, u32) {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let t = ticks - EPOCH_DIFF;
    (
        t.div_euclid(10_000_000),
        (t.rem_euclid(10_000_000) * 100) as u32,
    )
}

/// A Unix `st_mode` for a Windows file: `0o040755` for a directory, `0o100644` for a file
/// (`0o111` added for the extensions the shell runs), `0o120777` for a link; the write bits
/// are cleared for a read-only attribute.
fn synth_mode(attrs: u32, is_link: bool, path: &[u8]) -> u32 {
    let (kind, mut perm) = if is_link {
        (0o120000, 0o777)
    } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        (0o040000, 0o755)
    } else if is_executable_name(path) {
        (0o100000, 0o755)
    } else {
        (0o100000, 0o644)
    };
    if attrs & FILE_ATTRIBUTE_READONLY != 0 && !is_link {
        perm &= !0o222;
    }
    kind | perm
}

fn is_executable_name(path: &[u8]) -> bool {
    let name = path.rsplit(|&c| c == b'/').next().unwrap_or(path);
    let Some(dot) = name.iter().rposition(|&c| c == b'.') else {
        return false;
    };
    let ext = name[dot + 1..].to_ascii_lowercase();
    [&b"exe"[..], b"bat", b"cmd", b"com", b"ps1"].contains(&ext.as_slice())
}

/// A handle for querying a file's metadata (no data access, so locked files work).
fn query_handle(path: &Path, follow: bool) -> io::Result<File> {
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    OpenOptions::new()
        .access_mode(0)
        .custom_flags(flags)
        .open(path)
}

pub fn stat(path: &[u8], follow: bool) -> io::Result<FileInfo> {
    let p = super::path(path);
    let is_link = !follow && std::fs::symlink_metadata(&p)?.file_type().is_symlink();
    let file = query_handle(&p, follow)?;
    let handle = file.as_raw_handle();
    let mut by_handle = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: handle is open for the life of `file`; by_handle is a writable struct of the type
    // the call fills.
    if unsafe { GetFileInformationByHandle(handle, &mut by_handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: as above; the size passed is that of the FILE_BASIC_INFO the class asks for.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&raw mut basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut standard = FILE_STANDARD_INFO::default();
    // SAFETY: as above, for FILE_STANDARD_INFO.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            (&raw mut standard).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileInfo {
        mode: synth_mode(by_handle.dwFileAttributes, is_link, path),
        size: i64::from(by_handle.nFileSizeHigh) << 32 | i64::from(by_handle.nFileSizeLow),
        nlink: u64::from(by_handle.nNumberOfLinks),
        uid: 0,
        gid: 0,
        ino: u64::from(by_handle.nFileIndexHigh) << 32 | u64::from(by_handle.nFileIndexLow),
        dev: u64::from(by_handle.dwVolumeSerialNumber),
        blocks512: (standard.AllocationSize / 512).max(0) as u64,
        atime: unix_time(basic.LastAccessTime),
        ctime: unix_time(basic.ChangeTime),
        mtime: unix_time(basic.LastWriteTime),
    })
}

/// A directory, read by path (Windows has no `openat`).
pub struct Dir {
    path: Vec<u8>,
}

impl Dir {
    pub fn open(path: &[u8]) -> io::Result<Dir> {
        if !std::fs::metadata(super::path(path))?.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(Dir {
            path: path.to_vec(),
        })
    }

    /// `stat` of `name` (an absolute path ignores the directory).
    pub fn stat_at(&self, name: &[u8], follow: bool) -> io::Result<FileInfo> {
        if super::is_absolute(name) {
            return stat(name, follow);
        }
        let mut full = self.path.clone();
        if full.last() != Some(&b'/') {
            full.push(b'/');
        }
        full.extend_from_slice(name);
        stat(&full, follow)
    }

    pub fn entries(&self) -> io::Result<Entries> {
        Ok(Entries(std::fs::read_dir(super::path(&self.path))?))
    }

    /// `entries`: Windows lists no `.` and `..`.
    pub fn raw_entries(&self) -> io::Result<Entries> {
        self.entries()
    }
}

pub struct Entries(std::fs::ReadDir);

impl Iterator for Entries {
    type Item = io::Result<(Vec<u8>, DType)>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.0.next()?.and_then(|e| {
            let t = e.file_type()?;
            let ty = if t.is_dir() {
                DType::Dir
            } else if t.is_symlink() {
                DType::Symlink
            } else if t.is_file() {
                DType::Regular
            } else {
                DType::Other
            };
            Ok((bytes(&e.file_name()).into_owned(), ty))
        }))
    }
}

/// Drops the `//?/` of an extended-length path (`//?/UNC/srv/share` becomes `//srv/share`).
fn strip_verbatim(b: Vec<u8>) -> Vec<u8> {
    if let Some(rest) = b.strip_prefix(b"//?/UNC/") {
        return [&b"//"[..], rest].concat();
    }
    match b.strip_prefix(b"//?/") {
        Some(rest) if super::windows_root_len(rest) > 0 => rest.to_vec(),
        _ => b,
    }
}

pub fn readlink(path: &[u8]) -> io::Result<Vec<u8>> {
    let target = std::fs::read_link(super::path(path))?;
    Ok(strip_verbatim(bytes(target.as_os_str()).into_owned()))
}

pub fn symlink(target: &[u8], link: &[u8], is_dir: bool) -> io::Result<()> {
    // Link targets need \ separators to resolve.
    let target: PathBuf = super::path(target).components().collect();
    let link = super::path(link);
    let made = if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    // ERROR_PRIVILEGE_NOT_HELD: links need Developer Mode (or an administrator).
    made.map_err(|e| match e.raw_os_error() {
        Some(1314) => io::ErrorKind::PermissionDenied.into(),
        _ => e,
    })
}

pub fn temp_dir() -> Option<Vec<u8>> {
    Some(bytes(std::env::temp_dir().as_os_str()).into_owned())
}

pub fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| bytes(&h).into_owned())
}

pub fn user_name(_uid: u32) -> Option<Vec<u8>> {
    None
}

pub fn group_name(_gid: u32) -> Option<Vec<u8>> {
    None
}

pub fn private_mode(o: &mut OpenOptions) -> &mut OpenOptions {
    o
}

pub fn raise_open_files_limit() {}

/// `setenv(name, value, 1)`: variables nnn exports to the programs it starts.
pub fn setenv(name: &str, value: &[u8]) {
    let v = &*os(value);
    // SAFETY: nnn sets these from its main thread at points where no other thread reads or
    // writes the environment (the du workers never touch it).
    unsafe { std::env::set_var(name, v) };
}

/// `unsetenv(name)`
pub fn unsetenv(name: &str) {
    // SAFETY: as for setenv.
    unsafe { std::env::remove_var(name) };
}

pub fn error(errno: Errno) -> io::Error {
    use io::ErrorKind as K;
    let (kind, text) = match errno {
        Errno::NoEnt => (K::NotFound, "No such file or directory"),
        Errno::Exist => (K::AlreadyExists, "File exists"),
        Errno::Inval => (K::InvalidInput, "Invalid argument"),
        Errno::NameTooLong => (K::InvalidFilename, "File name too long"),
        Errno::Io => (K::Other, "Input/output error"),
        Errno::Fault => (K::Other, "Bad address"),
        Errno::NxIo => (K::Other, "No such device or address"),
        Errno::NotEmpty => (K::DirectoryNotEmpty, "Directory not empty"),
    };
    io::Error::new(kind, text)
}

pub fn is_errno(e: &io::Error, n: Errno) -> bool {
    use io::ErrorKind as K;
    match n {
        Errno::NoEnt => e.kind() == K::NotFound,
        Errno::Exist => e.kind() == K::AlreadyExists,
        Errno::Inval => e.kind() == K::InvalidInput,
        Errno::NameTooLong => e.kind() == K::InvalidFilename,
        Errno::NotEmpty => e.kind() == K::DirectoryNotEmpty,
        Errno::Io | Errno::Fault | Errno::NxIo => false,
    }
}

/// `access` from the synthesized mode: write needs no read-only attribute, exec a name the
/// shell runs.
pub fn access(path: &[u8], mode: Access) -> io::Result<()> {
    let m = stat(path, true)?.mode;
    let ok = match mode {
        Access::Exists => true,
        Access::Write | Access::ReadWrite => m & 0o200 != 0,
        Access::Exec => m & 0o111 != 0,
    };
    if ok {
        Ok(())
    } else {
        Err(io::ErrorKind::PermissionDenied.into())
    }
}

/// `mkdir`; an existing directory is `EEXIST` (Windows reports access denied for a drive
/// such as `C:` or the root `/`, which `xmktree` meets as its first prefix).
pub fn mkdir(path: &[u8], _mode: u32) -> io::Result<()> {
    let p = super::path(path);
    std::fs::create_dir(&p).map_err(|e| {
        if e.kind() != io::ErrorKind::AlreadyExists && p.is_dir() {
            error(Errno::Exist)
        } else {
            e
        }
    })
}

/// Windows has no named pipes in the file system (`SUPPORTS_FIFO`): NNN_FIFO is refused at
/// startup and the plugin control pipe is a regular file.
pub const SUPPORTS_FIFO: bool = false;
/// No FUSE: archivemount, sshfs, rclone mounts and unmounting say "not supported".
pub const SUPPORTS_MOUNTS: bool = false;
/// No execute bits (`toggle_exec` is never asked).
pub const SUPPORTS_CHMOD: bool = false;

pub fn mkfifo(_path: &[u8]) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_fifo_write(_path: &[u8]) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_fifo_pair(_path: &[u8]) -> (Option<File>, Option<File>) {
    (None, None)
}

pub fn defaults() -> Defaults {
    let shell = std::env::var_os("COMSPEC")
        .filter(|s| !s.is_empty())
        .map_or_else(|| b"cmd.exe".to_vec(), |s| bytes(&s).into_owned());
    Defaults {
        opener: br#"cmd /C start """#.to_vec(),
        editor: b"notepad".to_vec(),
        pager: b"more".to_vec(),
        shell,
    }
}

/// `sh -c` when a directory of `path` (a `PATH` value) holds `sh.exe`, else `cmd /C`.
fn shell_exec_prefix_in(path: Option<&OsStr>) -> &'static [u8] {
    let has_sh = path.is_some_and(|p| std::env::split_paths(p).any(|d| d.join("sh.exe").is_file()));
    if has_sh { b"sh -c" } else { b"cmd /C" }
}

pub fn shell_exec_prefix() -> &'static [u8] {
    static PREFIX: OnceLock<&'static [u8]> = OnceLock::new();
    PREFIX.get_or_init(|| shell_exec_prefix_in(std::env::var_os("PATH").as_deref()))
}

pub fn has_sh() -> bool {
    shell_exec_prefix() == b"sh -c"
}

/// A word of a command run by `cmd` on its command line: an empty one as `""`, one with a
/// quote as it is (`""` stays an empty title for `start`), one with a blank or a character
/// cmd acts on in quotes.
fn cmd_arg(a: &[u8]) -> Cow<'_, [u8]> {
    if a.is_empty() {
        return Cow::Borrowed(b"\"\"");
    }
    let special = |c: &u8| b" \t&|<>^()%!,;=".contains(c);
    if a.contains(&b'"') || !a.iter().any(special) {
        return Cow::Borrowed(a);
    }
    Cow::Owned([&b"\""[..], a, b"\""].concat())
}

/// `cmd`'s command line (see `platform::command`): `words` follow `cmd`; an `added` argument
/// right after `cmd /C` is the command line itself; any other one becomes
/// `"%NNN_CMD_ARGn%"` with the value in the child's environment: cmd expands a variable
/// once and never parses what it put there (inside quotes, and no file name holds a quote),
/// and `/V:OFF` keeps `!` literal too.
pub fn cmd_args(cmd: &mut Command, words: &[Vec<u8>], added: &[Vec<u8>]) {
    use std::os::windows::process::CommandExt;
    let line = matches!(words, [w] if w.eq_ignore_ascii_case(b"/C"));
    if !line && !added.is_empty() {
        cmd.raw_arg("/V:OFF");
    }
    for a in words {
        cmd.raw_arg(&*os(&cmd_arg(a)));
    }
    for (i, a) in added.iter().enumerate() {
        if line {
            cmd.raw_arg(&*os(a));
        } else if a.is_empty() {
            cmd.raw_arg("\"\"");
        } else {
            let name = format!("NNN_CMD_ARG{}", i + 1);
            cmd.env(&name, &*os(a));
            cmd.raw_arg(format!("\"%{name}%\""));
        }
    }
}

pub fn hostname() -> Vec<u8> {
    std::env::var_os("COMPUTERNAME")
        .map(|h| bytes(&h).into_owned())
        .unwrap_or_default()
}

pub fn remove_tree(path: &[u8]) {
    let _ = std::fs::remove_dir_all(super::path(path));
}

/// The console input (`CONIN$`) becomes the standard input.
pub fn stdin_to_tty() {
    let Ok(conin) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&*os(TTY_DEVICE))
    else {
        return;
    };
    let handle = conin.into_raw_handle();
    // SAFETY: handle is an open console handle this process owns from now on (never closed),
    // so the standard input stays valid.
    unsafe { SetStdHandle(STD_INPUT_HANDLE, handle) };
}

pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
    std::io::stdin().read(buf)
}

pub fn parent_is_init() -> bool {
    false
}

/// No console and a process group of its own (Ctrl-C in nnn's console does not reach it).
pub fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

pub fn wait_status(st: ExitStatus) -> i32 {
    st.code().unwrap_or(-1)
}

/// Free and total space of the volume holding `path`, counted in 1-byte blocks
/// (`get_fs_info` then shifts by nothing).
pub fn fs_blocks(path: &[u8]) -> Option<FsBlocks> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = os(path).encode_wide().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: wide is NUL-terminated and outlives the call; the three outputs are writable u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some(FsBlocks {
        frsize: 1,
        blocks: total,
        bfree: free,
        bavail: avail,
    })
}

/// Windows files have no execute bits to toggle.
pub fn toggle_exec(_path: &[u8]) -> Option<u32> {
    None
}

/// The directory watch: Windows polls the directory's time and entry count
/// (`winops::DirWatch`) where Linux reads inotify events.
pub struct DirWatch {
    watch: Option<crate::winops::DirWatch>,
}

impl DirWatch {
    pub fn new() -> io::Result<DirWatch> {
        Ok(DirWatch { watch: None })
    }

    pub fn watch(&mut self, path: &[u8]) {
        self.watch = Some(crate::winops::DirWatch::new(path));
    }

    pub fn unwatch(&mut self) {
        self.watch = None;
    }

    pub fn is_watching(&self) -> bool {
        self.watch.is_some()
    }

    pub fn changed(&mut self) -> bool {
        self.watch
            .as_mut()
            .is_some_and(crate::winops::DirWatch::changed)
    }
}

/// The flags the console control handler sets: Ctrl-C's (`int`) and a close's (`term`).
static CTRL_FLAGS: OnceLock<(Arc<AtomicBool>, Arc<AtomicBool>)> = OnceLock::new();
/// The event the handler sets so `Term::read` stops waiting, as a signal ends `poll` on
/// Unix (0 until `register_signals`).
static WAKE: AtomicUsize = AtomicUsize::new(0);
/// How long the handler keeps a closing console's process alive for nnn to leave curses and
/// exit (Windows ends the process when the handler returns, and after 5 s in any case).
const CLOSE_GRACE: Duration = Duration::from_secs(4);

fn wake_handle() -> Option<HANDLE> {
    let h = WAKE.load(Ordering::Acquire);
    (h != 0).then_some(h as HANDLE)
}

/// The console's events as nnn's signals: Ctrl-C is SIGINT (sets `int`), Ctrl-Break is
/// SIGQUIT (ignored), closing the console, logoff and shutdown are SIGHUP/SIGTERM (set
/// `term`, then give the main loop time to exit cleanly). Each wakes `Term::read`.
extern "system" fn console_ctrl(event: u32) -> BOOL {
    let Some((int, term)) = CTRL_FLAGS.get() else {
        return FALSE;
    };
    let (flag, closing) = match event {
        CTRL_C_EVENT => (int, false),
        CTRL_BREAK_EVENT => return TRUE,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => (term, true),
        _ => return FALSE,
    };
    flag.store(true, Ordering::Relaxed);
    if let Some(h) = wake_handle() {
        // SAFETY: h is the event register_signals created; it is never closed.
        unsafe { SetEvent(h) };
    }
    if closing {
        std::thread::sleep(CLOSE_GRACE);
    }
    TRUE
}

pub fn register_signals(
    int: Arc<AtomicBool>,
    term: Arc<AtomicBool>,
) -> Result<(), (SignalStep, io::Error)> {
    if WAKE.load(Ordering::Acquire) == 0 {
        // SAFETY: no attributes and no name; a manual-reset event, initially not set.
        let h = unsafe { CreateEventW(std::ptr::null(), TRUE, FALSE, std::ptr::null()) };
        if h.is_null() {
            return Err((SignalStep::Interrupt, io::Error::last_os_error()));
        }
        WAKE.store(h as usize, Ordering::Release);
    }
    let _ = CTRL_FLAGS.set((int, term.clone()));
    // SAFETY: a null handler with FALSE turns Ctrl-C handling back on (a parent may have
    // started nnn ignoring it); console_ctrl only touches statics and lives for the process.
    let ok = unsafe {
        SetConsoleCtrlHandler(None, FALSE) != 0
            && SetConsoleCtrlHandler(Some(console_ctrl), TRUE) != 0
    };
    if !ok {
        return Err((SignalStep::Interrupt, io::Error::last_os_error()));
    }
    // raise(SIGTERM) still exits cleanly.
    signal_hook::flag::register(signal_hook::consts::SIGTERM, term)
        .map_err(|e| (SignalStep::Terminate, e))?;
    Ok(())
}

const INPUT_MODES: CONSOLE_MODE =
    ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT | ENABLE_PROCESSED_INPUT;
const OUTPUT_MODES: CONSOLE_MODE = ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
const UTF8: u32 = 65001;

fn console_mode(h: HANDLE) -> Option<CONSOLE_MODE> {
    let mut mode: CONSOLE_MODE = 0;
    // SAFETY: GetConsoleMode only writes the mode; a handle that is not a console makes it fail.
    (unsafe { GetConsoleMode(h, &mut mode) } != 0).then_some(mode)
}

fn std_handle(which: STD_HANDLE) -> HANDLE {
    // SAFETY: GetStdHandle takes any STD_HANDLE value and only reads process state.
    unsafe { GetStdHandle(which) }
}

pub fn console_modes() -> Option<(u32, u32)> {
    Some((
        console_mode(std_handle(STD_INPUT_HANDLE))?,
        console_mode(std_handle(STD_OUTPUT_HANDLE))?,
    ))
}

/// The console: input records from the standard input, VT output on the standard output (the
/// standard error when the output is not a console).
pub struct Term {
    input: HANDLE,
    output: HANDLE,
    saved_in: CONSOLE_MODE,
    saved_out: CONSOLE_MODE,
    saved_cp: u32,
    saved_out_cp: u32,
    active: bool,
    resize_pending: bool,
    /// The visible window's size when last looked at: a buffer-size event only counts as a
    /// resize when this changed (conhost also sends them for alternate-screen switches and
    /// screen clears, which would otherwise make every redraw report a resize).
    window: Option<(i32, i32)>,
    /// The first half of a surrogate pair typed as two key events.
    high: Option<u16>,
}

impl Term {
    pub fn open() -> io::Result<Term> {
        let input = std_handle(STD_INPUT_HANDLE);
        let saved_in = console_mode(input).ok_or_else(io::Error::last_os_error)?;
        let (output, saved_out) = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .into_iter()
            .map(std_handle)
            .find_map(|h| console_mode(h).map(|m| (h, m)))
            .ok_or_else(io::Error::last_os_error)?;
        // SAFETY: these only read the console's code pages.
        let (saved_cp, saved_out_cp) = unsafe { (GetConsoleCP(), GetConsoleOutputCP()) };
        let mut t = Term {
            input,
            output,
            saved_in,
            saved_out,
            saved_cp,
            saved_out_cp,
            active: false,
            resize_pending: false,
            window: None,
            high: None,
        };
        t.window = t.window_size();
        t.apply()?;
        Ok(t)
    }

    /// Curses' modes and UTF-8 code pages.
    fn apply(&mut self) -> io::Result<()> {
        // SAFETY: the handles are the console's, valid for the life of the process; the calls
        // only change console state.
        let ok = unsafe {
            SetConsoleMode(self.input, INPUT_MODES) != 0
                && SetConsoleMode(self.output, self.saved_out | OUTPUT_MODES) != 0
                && SetConsoleCP(UTF8) != 0
                && SetConsoleOutputCP(UTF8) != 0
        };
        self.active = true;
        if ok {
            Ok(())
        } else {
            let e = io::Error::last_os_error();
            self.restore();
            Err(e)
        }
    }

    /// The visible window (`srWindow`, not the buffer): (lines, columns).
    fn window_size(&self) -> Option<(i32, i32)> {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        // SAFETY: info is a writable struct of the type the call fills.
        (unsafe { GetConsoleScreenBufferInfo(self.output, &mut info) } != 0).then(|| {
            let w = info.srWindow;
            (
                i32::from(w.Bottom) - i32::from(w.Top) + 1,
                i32::from(w.Right) - i32::from(w.Left) + 1,
            )
        })
    }

    pub fn size(&self) -> (i32, i32) {
        super::term_size(self.window_size())
    }

    /// Waits up to `wait_ms` (-1: no limit) for keys; typed characters come as UTF-8 (VT input
    /// sends keys as the escape sequences a terminal does). A console control event (Ctrl-C)
    /// ends the wait as a signal ends `poll` on Unix.
    pub fn read(&mut self, wait_ms: i32, out: &mut Vec<u8>) -> ReadResult {
        let deadline = u64::try_from(wait_ms)
            .ok()
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        // The wake event is reset only once a wait has seen it, so a Ctrl-C that comes before
        // the wait still ends it.
        let wake = wake_handle();
        let handles: Vec<HANDLE> = [Some(self.input), wake].into_iter().flatten().collect();
        loop {
            let wait = deadline.map_or(INFINITE, |d| {
                d.saturating_duration_since(Instant::now()).as_millis() as u32
            });
            // SAFETY: handles holds the console input and the wake event, both valid for the
            // life of the process; the count is the vector's length.
            let r = unsafe {
                WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), FALSE, wait)
            };
            if r != WAIT_OBJECT_0 {
                if r == WAIT_OBJECT_0 + 1
                    && let Some(h) = wake
                {
                    // SAFETY: h is the event register_signals created; it is never closed.
                    unsafe { ResetEvent(h) };
                }
                return ReadResult::Timeout;
            }
            let mut recs = [INPUT_RECORD::default(); 64];
            let mut n = 0u32;
            // SAFETY: recs has room for the 64 records asked for; n receives the count.
            let ok = unsafe { ReadConsoleInputW(self.input, recs.as_mut_ptr(), 64, &mut n) };
            if ok == 0 {
                return ReadResult::Timeout;
            }
            let start = out.len();
            let mut resized = false;
            for rec in &recs[..n as usize] {
                match u32::from(rec.EventType) {
                    KEY_EVENT => {
                        // SAFETY: EventType says the record holds a KEY_EVENT_RECORD.
                        let k = unsafe { rec.Event.KeyEvent };
                        // SAFETY: the W functions fill the UnicodeChar member.
                        let u = unsafe { k.uChar.UnicodeChar };
                        if k.bKeyDown != 0 && u != 0 {
                            for _ in 0..k.wRepeatCount.max(1) {
                                self.push_utf16(u, out);
                            }
                        }
                    }
                    WINDOW_BUFFER_SIZE_EVENT => {
                        let now = self.window_size();
                        if now != self.window {
                            self.window = now;
                            resized = true;
                        }
                    }
                    _ => {}
                }
            }
            if resized {
                self.resize_pending = true;
            }
            if out.len() > start {
                return ReadResult::Data;
            }
            if resized {
                return ReadResult::Resized;
            }
        }
    }

    fn push_utf16(&mut self, u: u16, out: &mut Vec<u8>) {
        let units: Vec<u16> = match self.high.take() {
            Some(h) if (0xDC00..0xE000).contains(&u) => vec![h, u],
            Some(h) => {
                push_char(char::decode_utf16([h]).next(), out);
                vec![u]
            }
            None if (0xD800..0xDC00).contains(&u) => {
                self.high = Some(u);
                return;
            }
            None => vec![u],
        };
        for c in char::decode_utf16(units) {
            push_char(Some(c), out);
        }
    }

    pub fn resize_pending(&self) -> bool {
        self.resize_pending
    }

    pub fn set_resize_pending(&mut self, on: bool) {
        self.resize_pending = on;
    }

    /// The saved modes and code pages back.
    pub fn restore(&mut self) {
        if self.active {
            // SAFETY: as in apply.
            unsafe {
                SetConsoleMode(self.input, self.saved_in);
                SetConsoleMode(self.output, self.saved_out);
                SetConsoleCP(self.saved_cp);
                SetConsoleOutputCP(self.saved_out_cp);
            }
            self.active = false;
        }
    }

    pub fn resume(&mut self) {
        if !self.active {
            let _ = self.apply();
        }
    }
}

/// A decoded UTF-16 unit as UTF-8; an unpaired surrogate as U+FFFD.
fn push_char(c: Option<Result<char, std::char::DecodeUtf16Error>>, out: &mut Vec<u8>) {
    if let Some(c) = c {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wtf8_round_trip_unpaired_surrogate() {
        let s = OsString::from_wide(&[0x61, 0xD800, 0x62]);
        let b = bytes(&s).into_owned();
        assert_eq!(b, b"a\xED\xA0\x80b");
        assert_eq!(os(&b).as_encoded_bytes(), s.as_encoded_bytes());
    }

    #[test]
    fn mode_is_synthesized_from_attributes() {
        assert_eq!(
            synth_mode(FILE_ATTRIBUTE_DIRECTORY, false, b"C:/d"),
            0o040755
        );
        assert_eq!(synth_mode(0x80, false, b"C:/f.txt"), 0o100644);
        assert_eq!(synth_mode(0x80, false, b"C:/Tool.EXE"), 0o100755);
        for ext in ["bat", "cmd", "com", "ps1"] {
            assert_eq!(
                synth_mode(0x80, false, format!("x.{ext}").as_bytes()),
                0o100755
            );
        }
        assert_eq!(synth_mode(0x80, false, b"C:/noext"), 0o100644);
        assert_eq!(synth_mode(FILE_ATTRIBUTE_READONLY, false, b"f"), 0o100444);
        assert_eq!(
            synth_mode(FILE_ATTRIBUTE_READONLY, false, b"f.exe"),
            0o100555
        );
        assert_eq!(synth_mode(FILE_ATTRIBUTE_DIRECTORY, true, b"l"), 0o120777);
    }

    #[test]
    fn times_convert_from_1601() {
        assert_eq!(unix_time(116_444_736_000_000_000), (0, 0));
        assert_eq!(
            unix_time(116_444_736_000_000_000 + 15_000_000),
            (1, 500_000_000)
        );
        assert_eq!(unix_time(0).0, -11_644_473_600);
    }

    #[test]
    fn extended_prefixes_are_dropped() {
        assert_eq!(strip_verbatim(b"//?/C:/a".to_vec()), b"C:/a");
        assert_eq!(
            strip_verbatim(b"//?/UNC/srv/share/x".to_vec()),
            b"//srv/share/x"
        );
        assert_eq!(strip_verbatim(b"rel/x".to_vec()), b"rel/x");
    }

    #[test]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "Windows: clearing READONLY so the temp dir can be removed"
    )]
    fn stat_reads_ids_sizes_and_attributes() {
        let dir = std::env::temp_dir().join(format!("nnn-winstat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("run.cmd");
        std::fs::write(&f, b"abcd").unwrap();
        let fb = bytes(f.as_os_str()).into_owned();
        let i = stat(&fb, true).unwrap();
        assert_eq!((i.mode, i.size, i.nlink), (0o100755, 4, 1));
        assert!(i.ino != 0 && i.dev != 0);
        assert_eq!((i.uid, i.gid), (0, 0));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((now - i.mtime.0).abs() < 120 && (now - i.ctime.0).abs() < 120);
        let mut perms = std::fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&f, perms).unwrap();
        assert_eq!(stat(&fb, true).unwrap().mode, 0o100555);
        let mut perms = std::fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&f, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cmd_arguments_keep_cmd_syntax() {
        assert_eq!(&*cmd_arg(b"/C"), b"/C");
        assert_eq!(&*cmd_arg(br#""""#), br#""""#);
        assert_eq!(&*cmd_arg(b""), br#""""#);
        assert_eq!(&*cmd_arg(b"C:/My Files/a.txt"), br#""C:/My Files/a.txt""#);
        assert_eq!(&*cmd_arg(br#"notepad "a b""#), br#"notepad "a b""#);
        for name in ["a&b.txt", "100%x%.txt", "x^(y).txt", "p|q<r>s!t,u;v=w"] {
            assert_eq!(&*cmd_arg(name.as_bytes()), format!("\"{name}\"").as_bytes());
        }
        // A command line after cmd /C is run as it is.
        let run = |argv: &[&str], words: usize| {
            let argv: Vec<Vec<u8>> = argv.iter().map(|a| a.as_bytes().to_vec()).collect();
            let out = super::super::command(&argv, words).output().unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(run(&["cmd", "/C", "echo a&echo b"], 2), "a\r\nb\r\n");
        // The prompt's shape: $SHELL is one word, /C and the line are added.
        assert_eq!(run(&["cmd", "/C", "echo a&echo b"], 1), "a\r\nb\r\n");
    }

    /// The `]` prompt with a cmd $SHELL: spawn(shell, "/C", line) runs the typed line.
    #[test]
    fn prompt_line_runs_under_cmd_shell() {
        let dir = std::env::temp_dir().join(format!("nnn-cmd-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ran");
        let shell = std::env::var_os("COMSPEC")
            .map_or_else(|| b"cmd.exe".to_vec(), |s| bytes(&s).into_owned());
        let flag = super::super::shell_command_flag(&shell).to_vec();
        let line = format!("echo x>{}&echo y>>{}", marker.display(), marker.display());
        let st = super::super::command(&[shell, flag, line.into_bytes()], 1)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(st.status.success(), "{st:?}");
        assert_eq!(std::fs::read(&marker).unwrap(), b"x\r\ny\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file name given to a cmd command (as to the default opener) is never parsed by cmd:
    /// no second command runs and no variable is expanded.
    #[test]
    fn file_names_reach_cmd_commands_literally() {
        let dir = std::env::temp_dir().join(format!("nnn-cmd-inject-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ran");
        let evil = format!("a&echo x>{}", marker.display());
        for name in [&evil[..], "100%PATH%.txt", "x^(y)!z!.txt", "p|q<r>s"] {
            let argv = [
                b"cmd".to_vec(),
                b"/C".to_vec(),
                b"echo".to_vec(),
                name.as_bytes().to_vec(),
            ];
            let out = super::super::command(&argv, 3).output().unwrap();
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                format!("\"{name}\"\r\n")
            );
        }
        assert!(!marker.exists(), "a file name ran a command");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_defaults() {
        let d = defaults();
        assert_eq!(d.opener, br#"cmd /C start """#);
        assert_eq!(
            (&d.editor[..], &d.pager[..]),
            (&b"notepad"[..], &b"more"[..])
        );
        assert!(!d.shell.is_empty());
    }

    #[test]
    fn shell_prefix_prefers_sh() {
        let dir = std::env::temp_dir().join(format!("nnn-sh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let git = dir.join("git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("sh.exe"), b"").unwrap();
        let path = std::env::join_paths([&empty, &git]).unwrap();
        assert_eq!(shell_exec_prefix_in(Some(&path)), b"sh -c");
        assert_eq!(shell_exec_prefix_in(Some(empty.as_os_str())), b"cmd /C");
        assert_eq!(shell_exec_prefix_in(None), b"cmd /C");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backslashes_become_slashes() {
        assert_eq!(&*bytes(OsStr::new("C:\\a\\b")), b"C:/a/b");
    }
}
