//! The Windows backend: byte paths are WTF-8 (`OsStr::as_encoded_bytes`), shown with `/`.

use super::{DType, FileInfo};
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_STANDARD_INFO, FileBasicInfo,
    FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
};

pub const NULL_DEVICE: &[u8] = b"NUL";
pub const TTY_DEVICE: &[u8] = b"CONIN$";

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

pub fn os(b: &[u8]) -> Cow<'_, OsStr> {
    match std::str::from_utf8(b) {
        Ok(s) => Cow::Borrowed(OsStr::new(s)),
        Err(_) => Cow::Owned(OsString::from_wide(&wtf8_to_wide(b))),
    }
}

pub fn bytes(s: &OsStr) -> Cow<'_, [u8]> {
    let b = s.as_encoded_bytes();
    if b.contains(&b'\\') {
        Cow::Owned(
            b.iter()
                .map(|&c| if c == b'\\' { b'/' } else { c })
                .collect(),
        )
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
    if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
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
    fn backslashes_become_slashes() {
        assert_eq!(&*bytes(OsStr::new("C:\\a\\b")), b"C:/a/b");
    }
}

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
