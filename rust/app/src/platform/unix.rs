//! The Unix backend: byte paths are the OS's own bytes.

use super::{DType, FileInfo};
use rustix::fd::{AsFd, OwnedFd};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, Stat};
use std::borrow::Cow;
use std::ffi::{OsStr, c_char};
use std::fs::OpenOptions;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;

pub const NULL_DEVICE: &[u8] = b"/dev/null";
pub const TTY_DEVICE: &[u8] = b"/dev/tty";

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
        Ok(Entries(rustix::fs::Dir::read_from(&self.fd)?))
    }
}

pub struct Entries(rustix::fs::Dir);

impl Iterator for Entries {
    type Item = io::Result<(Vec<u8>, DType)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let e = match self.0.next()? {
                Ok(e) => e,
                Err(err) => return Some(Err(err.into())),
            };
            let name = e.file_name().to_bytes();
            if name == b"." || name == b".." {
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
