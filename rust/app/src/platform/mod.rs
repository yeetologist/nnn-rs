//! The OS-independent API (re-exports the active backend) and the path helpers.
//!
//! Internal paths are byte strings with `/` separators on every OS. On Unix the bytes are the
//! OS's own; on Windows they are WTF-8 (what `OsStr::as_encoded_bytes` holds), with `\` shown as `/`.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[cfg(any(windows, feature = "portable"))]
pub(crate) mod portable_locale;
#[cfg(unix)]
pub(crate) mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as backend;
#[cfg(windows)]
use windows as backend;

/// The path forms to interpret: both are testable on either host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Unix,
    Windows,
}

impl Os {
    /// The OS this build runs on.
    pub const CURRENT: Os = if cfg!(windows) { Os::Windows } else { Os::Unix };
}

/// The OS string for a byte path (Unix: zero-copy; Windows: WTF-8 decoded).
pub fn os(b: &[u8]) -> Cow<'_, OsStr> {
    backend::os(b)
}

/// The bytes of an OS string (Unix: zero-copy; Windows: WTF-8 with `\` turned into `/`).
pub fn bytes(s: &OsStr) -> Cow<'_, [u8]> {
    backend::bytes(s)
}

/// A byte path as a `Path`.
pub fn path(b: &[u8]) -> Cow<'_, Path> {
    match os(b) {
        Cow::Borrowed(s) => Cow::Borrowed(Path::new(s)),
        Cow::Owned(s) => Cow::Owned(PathBuf::from(s)),
    }
}

/// The bytes of an owned OS string.
pub fn into_bytes(s: std::ffi::OsString) -> Vec<u8> {
    bytes(&s).into_owned()
}

/// The length of the root of `path` in `os` form: 1 for `/...` on Unix; 3 for `C:/...` and the
/// length of `//server/share/` (without the trailing `/` if the path ends there) for UNC on
/// Windows; 0 for a relative path.
pub fn root_len_with(os: Os, path: &[u8]) -> usize {
    match os {
        Os::Unix => usize::from(path.first() == Some(&b'/')),
        Os::Windows => windows_root_len(path),
    }
}

fn windows_root_len(p: &[u8]) -> usize {
    if p.len() >= 3 && p[0].is_ascii_alphabetic() && p[1] == b':' && p[2] == b'/' {
        return 3;
    }
    if p.starts_with(b"//") && p.get(2).is_some_and(|&c| c != b'/') {
        let Some(server) = p[2..].iter().position(|&c| c == b'/') else {
            return 0;
        };
        let share_start = 2 + server + 1;
        if p.get(share_start).is_none_or(|&c| c == b'/') {
            return 0;
        }
        return match p[share_start..].iter().position(|&c| c == b'/') {
            Some(end) => share_start + end + 1,
            None => p.len(),
        };
    }
    0
}

/// `root_len_with` for this OS.
pub fn root_len(path: &[u8]) -> usize {
    root_len_with(Os::CURRENT, path)
}

/// `path` is a root: `/`, `C:/`, `//server/share/`.
pub fn is_root(path: &[u8]) -> bool {
    let n = root_len(path);
    n == path.len() && n > 0
}

/// `path` starts at a root.
pub fn is_absolute(path: &[u8]) -> bool {
    root_len(path) > 0
}

/// The directory part of `p`: everything before the last `/`, never cutting inside the root
/// (the root itself for a top-level entry). A path with no `/` is returned unchanged.
pub fn xdirname_with(os: Os, p: &[u8]) -> &[u8] {
    match p.iter().rposition(|&c| c == b'/') {
        Some(at) => &p[..at.max(root_len_with(os, p))],
        None => p,
    }
}

/// `xdirname_with` for this OS.
pub fn xdirname(p: &[u8]) -> &[u8] {
    xdirname_with(Os::CURRENT, p)
}

/// The `struct stat` fields nnn uses. On Windows `mode` is synthesized, `uid` and `gid` are 0,
/// `dev` is the volume serial number and `ino` the file index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileInfo {
    pub mode: u32,
    pub size: i64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
    pub ino: u64,
    pub dev: u64,
    pub blocks512: u64,
    pub atime: (i64, u32),
    pub ctime: (i64, u32),
    pub mtime: (i64, u32),
}

/// A directory entry's type as the directory listing gives it (`d_type`); `Unknown` means the
/// type needs a `stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    Dir,
    Symlink,
    Regular,
    Unknown,
    Other,
}

pub use backend::{Dir, Entries};

/// `stat` (`follow`) or `lstat` of a byte path.
pub fn stat(path: &[u8], follow: bool) -> io::Result<FileInfo> {
    backend::stat(path, follow)
}

/// The entries of a directory, without `.` and `..`.
pub fn read_dir(path: &[u8]) -> io::Result<Entries> {
    Dir::open(path)?.entries()
}

/// The target a symbolic link holds.
pub fn readlink(path: &[u8]) -> io::Result<Vec<u8>> {
    backend::readlink(path)
}

/// Makes `link` a symbolic link to `target` (`is_dir`: the target is a directory; only Windows
/// needs to know).
pub fn symlink(target: &[u8], link: &[u8], is_dir: bool) -> io::Result<()> {
    backend::symlink(target, link, is_dir)
}

/// The null device: `/dev/null` or `NUL`.
pub fn null_device() -> &'static [u8] {
    backend::NULL_DEVICE
}

/// The controlling terminal: `/dev/tty` or `CONIN$`.
pub fn tty_device() -> &'static [u8] {
    backend::TTY_DEVICE
}

/// The directory for temporary files, if there is one.
pub fn temp_dir() -> Option<Vec<u8>> {
    backend::temp_dir()
}

/// The user's home directory.
pub fn home() -> Option<Vec<u8>> {
    backend::home()
}

/// The name of user `uid` (none on Windows).
pub fn user_name(uid: u32) -> Option<Vec<u8>> {
    backend::user_name(uid)
}

/// The name of group `gid` (none on Windows).
pub fn group_name(gid: u32) -> Option<Vec<u8>> {
    backend::group_name(gid)
}

/// Opens files created by `create_file` private to the user where the OS has such modes.
pub fn private_mode(o: &mut OpenOptions) -> &mut OpenOptions {
    backend::private_mode(o)
}

/// Raises the soft limit on open files to the hard limit (a no-op on Windows).
pub fn raise_open_files_limit() {
    backend::raise_open_files_limit();
}

/// `setenv(name, value, 1)`.
pub fn setenv(name: &str, value: &[u8]) {
    backend::setenv(name, value);
}

/// `unsetenv(name)`.
pub fn unsetenv(name: &str) {
    backend::unsetenv(name);
}

/// `S_IFMT`, `S_IFIFO` and `S_IFDIR` of `FileInfo::mode`.
pub const S_IFMT: u32 = 0o170000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFDIR: u32 = 0o040000;

/// The `errno` values nnn reports itself (not as a system call's result).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Errno {
    NoEnt,
    Exist,
    Inval,
    NameTooLong,
    Io,
    Fault,
    NxIo,
}

/// The error for `errno` (Unix: the OS's own; Windows: glibc's words, so `strerror` matches).
pub fn error(errno: Errno) -> io::Error {
    backend::error(errno)
}

/// `errno == n` for an error a system call returned.
pub fn is_errno(e: &io::Error, n: Errno) -> bool {
    backend::is_errno(e, n)
}

/// The checks of `access(2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Exists,
    Write,
    ReadWrite,
    Exec,
}

/// `access(path, mode)`.
pub fn access(path: &[u8], mode: Access) -> io::Result<()> {
    backend::access(path, mode)
}

/// `mkdir(path, mode)` (the mode is Unix-only).
pub fn mkdir(path: &[u8], mode: u32) -> io::Result<()> {
    backend::mkdir(path, mode)
}

/// `mkfifo(path, 0600)`.
pub fn mkfifo(path: &[u8]) -> io::Result<()> {
    backend::mkfifo(path)
}

/// The FIFO at `path` opened to write without blocking and without following a link; it
/// must be a FIFO (`EINVAL` otherwise).
pub fn open_fifo_write(path: &[u8]) -> io::Result<std::fs::File> {
    backend::open_fifo_write(path)
}

/// Both ends of the FIFO at `path`, opened as `run_plugin` does: the read end first without
/// blocking (so opening the write end succeeds), the write end only if that worked; both block
/// afterwards.
pub fn open_fifo_pair(path: &[u8]) -> (Option<std::fs::File>, Option<std::fs::File>) {
    backend::open_fifo_pair(path)
}

/// Makes the controlling terminal stdin again after a list was read from it (list mode).
pub fn stdin_to_tty() {
    backend::stdin_to_tty();
}

/// `read(STDIN_FILENO, buf, len)`: one unbuffered read.
pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
    backend::read_stdin(buf)
}

/// `getppid() == 1`: the parent has exited.
pub fn parent_is_init() -> bool {
    backend::parent_is_init()
}

/// Starts `cmd` in its own process group (`setsid` in the original's grandchild).
pub fn detach(cmd: &mut std::process::Command) {
    backend::detach(cmd);
}

/// The exit status of a child, or the raw wait status when it did not exit.
pub fn wait_status(st: std::process::ExitStatus) -> i32 {
    backend::wait_status(st)
}

/// The programs nnn runs when `NNN_OPENER`, `VISUAL`/`EDITOR`, `PAGER` and `SHELL` name none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defaults {
    /// Holds its own arguments when it contains a blank (`spawn` with `F_MULTI`).
    pub opener: Vec<u8>,
    pub editor: Vec<u8>,
    pub pager: Vec<u8>,
    pub shell: Vec<u8>,
}

/// Unix: `xdg-open`, `vi`, `less`, `sh`; Windows: `cmd /C start ""`, `notepad`, `more`,
/// `%COMSPEC%`.
pub fn defaults() -> Defaults {
    backend::defaults()
}

/// `utils[UTIL_SH_EXEC]`, the command that runs a command line: `sh -c` (on Windows only
/// when `sh.exe` is on `PATH`, as Git for Windows puts it; else `cmd /C`).
pub fn shell_exec_prefix() -> &'static [u8] {
    backend::shell_exec_prefix()
}

/// `sh` is there to run script plugins (always on Unix).
pub fn has_sh() -> bool {
    backend::has_sh()
}

/// Named pipes for `NNN_FIFO` and the plugin control pipe.
pub const SUPPORTS_FIFO: bool = backend::SUPPORTS_FIFO;
/// FUSE mounts (archivemount, sshfs, rclone) and unmounting them.
pub const SUPPORTS_MOUNTS: bool = backend::SUPPORTS_MOUNTS;
/// Execute bits to toggle.
pub const SUPPORTS_CHMOD: bool = backend::SUPPORTS_CHMOD;

/// `cmd` or `cmd.exe` (any case, any directory) on Windows: a program that takes its
/// command line as it is instead of as C-style arguments.
fn is_cmd(prog: &[u8]) -> bool {
    let name = prog
        .rsplit(|&c| c == b'/' || c == b'\\')
        .next()
        .unwrap_or(prog)
        .to_ascii_lowercase();
    Os::CURRENT == Os::Windows && (name == b"cmd" || name == b"cmd.exe")
}

/// The option that makes `shell` run one command line: `/C` for `cmd`, else `-c`.
pub fn shell_command_flag(shell: &[u8]) -> &'static [u8] {
    if is_cmd(shell) { b"/C" } else { b"-c" }
}

/// `execvp(argv[0], argv)` as a `Command` (looked up in `PATH`; each string ends at its
/// first NUL, as a C string does); on Windows the arguments of `cmd` go on its command line
/// as they are (quoted only when they hold a blank).
pub fn command(argv: &[Vec<u8>]) -> std::process::Command {
    let c = |a: &[u8]| {
        a.iter()
            .position(|&b| b == 0)
            .map_or(a, |n| &a[..n])
            .to_vec()
    };
    let argv: Vec<Vec<u8>> = argv.iter().map(|a| c(a)).collect();
    let mut cmd = std::process::Command::new(&*os(&argv[0]));
    if is_cmd(&argv[0]) {
        backend::cmd_args(&mut cmd, &argv[1..]);
    } else {
        cmd.args(argv[1..].iter().map(|a| os(a)));
    }
    cmd
}

/// `gethostname()` (Windows: `%COMPUTERNAME%`); empty when unknown.
pub fn hostname() -> Vec<u8> {
    backend::hostname()
}

/// `rm -rf path` with its output suppressed (Windows removes the tree itself; links in it
/// are removed, not followed).
pub fn remove_tree(path: &[u8]) {
    backend::remove_tree(path);
}

/// The `statvfs` fields `get_fs_info` uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FsBlocks {
    pub frsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
}

/// `statvfs(path)`.
pub fn fs_blocks(path: &[u8]) -> Option<FsBlocks> {
    backend::fs_blocks(path)
}

/// `xchmod`'s system calls: toggles the execute bits of `path` (not following a link):
/// all three off if the owner's is set, else all three on. The new mode, if it changed.
pub fn toggle_exec(path: &[u8]) -> Option<u32> {
    backend::toggle_exec(path)
}

pub use backend::DirWatch;

/// Which registration of `register_signals` failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalStep {
    Interrupt,
    Terminate,
    Quit,
}

/// The signals nnn handles: `int` is set on SIGINT, `term` on SIGTERM (and SIGHUP); SIGQUIT is
/// ignored. SIGWINCH belongs to `Term`.
pub fn register_signals(
    int: Arc<AtomicBool>,
    term: Arc<AtomicBool>,
) -> Result<(), (SignalStep, io::Error)> {
    backend::register_signals(int, term)
}

/// What `Term::read` got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadResult {
    /// Bytes were appended to the buffer.
    Data,
    /// The terminal changed size (and nothing was read).
    Resized,
    /// Nothing arrived in time.
    Timeout,
}

/// The terminal in curses' modes (Unix: cbreak, noecho, nonl on stdin; Windows: the console in
/// VT input and output modes, UTF-8 code pages). Dropping it restores the saved modes.
pub use backend::Term;

/// The screen size: `LINES`/`COLUMNS` win over what the terminal reports (`ws`, lines and
/// columns, 0 when unknown); 24x80 when neither is there.
fn term_size(ws: Option<(i32, i32)>) -> (i32, i32) {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|&v| v > 0)
    };
    let lines = env("LINES")
        .or(ws.map(|w| w.0).filter(|&v| v > 0))
        .unwrap_or(24);
    let cols = env("COLUMNS")
        .or(ws.map(|w| w.1).filter(|&v| v > 0))
        .unwrap_or(80);
    (lines, cols)
}

/// The console's input and output modes (for checking that `Term` restores them).
#[cfg(windows)]
pub fn console_modes() -> Option<(u32, u32)> {
    backend::console_modes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_len_unix_and_windows_forms() {
        assert_eq!(root_len_with(Os::Unix, b"/a/b"), 1);
        assert_eq!(root_len_with(Os::Unix, b"a/b"), 0);
        assert_eq!(root_len_with(Os::Windows, b"C:/a"), 3);
        assert_eq!(root_len_with(Os::Windows, b"C:/"), 3);
        assert_eq!(root_len_with(Os::Windows, b"//srv/share/x"), 12);
        assert_eq!(root_len_with(Os::Windows, b"//srv/share"), 11);
        assert_eq!(root_len_with(Os::Windows, b"a/b"), 0);
    }

    #[test]
    fn malformed_unc_has_no_root() {
        assert_eq!(root_len_with(Os::Windows, b"//srv"), 0);
        assert_eq!(root_len_with(Os::Windows, b"//srv/"), 0);
        assert_eq!(root_len_with(Os::Windows, b"//srv//x"), 0);
        assert_eq!(root_len_with(Os::Windows, b"///x"), 0);
        assert_eq!(root_len_with(Os::Windows, b"C:"), 0);
    }

    #[test]
    fn xdirname_stops_at_root() {
        assert_eq!(xdirname_with(Os::Windows, b"C:/a"), b"C:/");
        assert_eq!(xdirname_with(Os::Windows, b"C:/"), b"C:/");
        assert_eq!(xdirname_with(Os::Windows, b"C:/a/b"), b"C:/a");
        assert_eq!(
            xdirname_with(Os::Windows, b"//srv/share/x"),
            b"//srv/share/"
        );
        assert_eq!(xdirname_with(Os::Windows, b"//srv/share/"), b"//srv/share/");
        assert_eq!(xdirname_with(Os::Unix, b"/a"), b"/");
        assert_eq!(xdirname_with(Os::Unix, b"/a/b"), b"/a");
        assert_eq!(xdirname_with(Os::Unix, b"rel"), b"rel");
    }

    #[cfg(unix)]
    #[test]
    fn unix_defaults() {
        let d = defaults();
        let all = [&d.opener[..], &d.editor[..], &d.pager[..], &d.shell[..]];
        assert_eq!(all, [&b"xdg-open"[..], b"vi", b"less", b"sh"]);
        assert_eq!(shell_exec_prefix(), b"sh -c");
    }

    #[test]
    fn cmd_is_known_only_on_windows() {
        let win = Os::CURRENT == Os::Windows;
        assert_eq!(is_cmd(b"cmd"), win);
        assert_eq!(is_cmd(b"C:\\WINDOWS\\system32\\CMD.EXE"), win);
        assert_eq!(is_cmd(b"C:/Windows/cmd.exe"), win);
        assert!(!is_cmd(b"sh"));
        assert!(!is_cmd(b"/usr/bin/cmdx"));
        assert_eq!(shell_command_flag(b"sh"), b"-c");
        assert_eq!(
            shell_command_flag(b"cmd.exe"),
            if win { &b"/C"[..] } else { b"-c" }
        );
    }

    #[test]
    fn current_os_helpers() {
        assert!(is_root(b"/") == (Os::CURRENT == Os::Unix));
        assert!(!is_root(b""));
        assert!(!is_root(b"rel"));
        assert_eq!(is_absolute(b"/x"), Os::CURRENT == Os::Unix);
    }

    #[test]
    fn os_and_bytes_round_trip() {
        let p = b"dir/na\xC3\xA9me";
        assert_eq!(&*bytes(&os(p)), p);
    }

    /// A fresh temp dir holding the directory `d` and the 3-byte file `f`.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nnn-platform-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("f"), b"abc").unwrap();
        dir
    }

    fn join(dir: &Path, name: &str) -> Vec<u8> {
        let mut b = bytes(dir.as_os_str()).into_owned();
        b.push(b'/');
        b.extend_from_slice(name.as_bytes());
        b
    }

    #[test]
    fn stat_dir_and_file_modes() {
        let dir = scratch("modes");
        let d = stat(&join(&dir, "d"), true).unwrap();
        assert_eq!(d.mode & 0o170000, 0o040000);
        let f = stat(&join(&dir, "f"), true).unwrap();
        assert_eq!(f.mode & 0o170000, 0o100000);
        assert_eq!(f.size, 3);
        assert!(f.nlink >= 1);
        assert!(stat(&join(&dir, "missing"), true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_dir_reports_types() {
        let dir = scratch("readdir");
        let mut names = Vec::new();
        for e in read_dir(&bytes(dir.as_os_str())).unwrap() {
            let (name, ty) = e.unwrap();
            match name.as_slice() {
                b"d" => assert_eq!(ty, DType::Dir),
                b"f" => assert!(matches!(ty, DType::Regular | DType::Unknown)),
                other => panic!("unexpected entry {other:?}"),
            }
            names.push(name);
        }
        names.sort();
        assert_eq!(names, [b"d".to_vec(), b"f".to_vec()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_handle_stats_names_inside() {
        let dir = scratch("dirat");
        let h = Dir::open(&bytes(dir.as_os_str())).unwrap();
        assert_eq!(h.stat_at(b"f", true).unwrap().size, 3);
        assert!(h.stat_at(b"nope", false).is_err());
        let names: Vec<_> = h.entries().unwrap().map(|e| e.unwrap().0).collect();
        assert_eq!(names.len(), 2);
        assert!(Dir::open(&join(&dir, "f")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devices_home_and_temp() {
        assert_eq!(
            null_device(),
            if cfg!(windows) {
                &b"NUL"[..]
            } else {
                b"/dev/null"
            }
        );
        assert_eq!(
            tty_device(),
            if cfg!(windows) {
                &b"CONIN$"[..]
            } else {
                b"/dev/tty"
            }
        );
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(os(null_device()))
                .is_ok()
        );
        assert!(temp_dir().is_some_and(|t| !t.is_empty()));
        if std::env::var_os("HOME").is_some() {
            assert_eq!(home(), std::env::var_os("HOME").map(into_bytes));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_round_trip_and_nofollow() {
        let dir = scratch("link");
        let link = join(&dir, "l");
        symlink(b"f", &link, false).unwrap();
        assert_eq!(readlink(&link).unwrap(), b"f");
        assert_eq!(stat(&link, false).unwrap().mode & 0o170000, 0o120000);
        assert_eq!(stat(&link, true).unwrap().mode & 0o170000, 0o100000);
        let names: Vec<_> = read_dir(&bytes(dir.as_os_str()))
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert!(names.contains(&(b"l".to_vec(), DType::Symlink)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn symlink_round_trip_when_allowed() {
        let dir = scratch("link");
        let link = join(&dir, "l");
        if let Err(e) = symlink(b"f", &link, false) {
            // Creating links needs a privilege (or Developer Mode) that a plain account lacks.
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
            return;
        }
        assert_eq!(readlink(&link).unwrap(), b"f");
        assert_eq!(stat(&link, false).unwrap().mode, 0o120777);
        assert_eq!(stat(&link, true).unwrap().mode & 0o170000, 0o100000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn user_and_group_names() {
        assert_eq!(user_name(0), Some(b"root".to_vec()));
        assert!(group_name(0).is_some());
        assert_eq!(user_name(4_000_000_000), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_has_no_user_names() {
        assert_eq!(user_name(0), None);
        assert_eq!(group_name(0), None);
    }

    #[cfg(windows)]
    #[test]
    fn wtf8_round_trip_unpaired_surrogate() {
        use std::os::windows::ffi::OsStringExt;
        let dir = scratch("wtf8");
        let name = std::ffi::OsString::from_wide(&[0x61, 0xD800]);
        std::fs::write(dir.join(&name), b"x").unwrap();
        let listed = read_dir(&bytes(dir.as_os_str()))
            .unwrap()
            .map(|e| e.unwrap().0)
            .find(|n| n.starts_with(b"a"))
            .unwrap();
        let mut from = bytes(dir.as_os_str()).into_owned();
        from.push(b'/');
        from.extend_from_slice(&listed);
        let to = join(&dir, "renamed");
        std::fs::rename(&*os(&from), &*os(&to)).unwrap();
        assert!(dir.join("renamed").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn strerror_uses_glibc_words() {
        let e = std::fs::File::open("C:/no/such/file").unwrap_err();
        assert_eq!(crate::sys::strerror(&e), "No such file or directory");
    }
}
