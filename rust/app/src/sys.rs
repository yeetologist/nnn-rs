//! Thin wrappers over the system calls the original makes, with its error output.

use std::io::{self, Write};

use crate::platform::{self, Os, path};

/// Rust's message without its " (os error N)" suffix.
fn without_os_error(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}

/// `strerror` on Windows: glibc's words for the errors nnn reports, so the output matches
/// Linux; any other error keeps Rust's message.
fn strerror_windows(e: &io::Error) -> String {
    use io::ErrorKind::{
        AlreadyExists, DirectoryNotEmpty, InvalidFilename, NotFound, PermissionDenied, ResourceBusy,
    };
    let text = match e.kind() {
        NotFound => "No such file or directory",
        PermissionDenied => "Permission denied",
        AlreadyExists => "File exists",
        DirectoryNotEmpty => "Directory not empty",
        ResourceBusy => "Device or resource busy",
        InvalidFilename => "File name too long",
        _ => return without_os_error(e),
    };
    text.to_string()
}

/// `strerror(errno)`: Rust's message without its " (os error N)" suffix (glibc's words on
/// Windows).
pub fn strerror(e: &io::Error) -> String {
    if cfg!(windows) {
        strerror_windows(e)
    } else {
        without_os_error(e)
    }
}

/// `xerror()`: `perror` with the original's C source line as the prefix (ADR-011).
pub fn xerror(line: u32, e: &io::Error) {
    let _ = writeln!(io::stderr(), "{line}: {}", strerror(e));
}

/// `open(p, O_WRONLY | O_CREAT, 0600)` then `close`.
pub fn create_file(p: &[u8]) -> io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(false);
    platform::private_mode(&mut o).open(path(p)).map(drop)
}

pub fn unlink(p: &[u8]) {
    let _ = std::fs::remove_file(path(p));
}

/// `xdiraccess`: the path opens as a directory.
pub fn dir_access(p: &[u8]) -> bool {
    std::fs::read_dir(path(p)).is_ok()
}

/// `abspath(filepath, cwd, NULL)`; `home` is the global the original reads for `~/`.
pub fn abspath_in(p: &[u8], cwd: Option<&[u8]>, home: Option<&[u8]>) -> Option<Vec<u8>> {
    abspath_with(Os::CURRENT, p, cwd, home)
}

/// `abspath_in` for the path forms of `os`. On Windows a rooted path without a drive (`/x`)
/// is at the root of the drive (or share) of the directory it is resolved in.
pub fn abspath_with(os: Os, p: &[u8], cwd: Option<&[u8]>, home: Option<&[u8]>) -> Option<Vec<u8>> {
    let root_len = |b: &[u8]| platform::root_len_with(os, b);
    let (base, rel): (Option<Vec<u8>>, &[u8]) = if p.starts_with(b"~/") {
        (home.map(<[u8]>::to_vec), &p[2..])
    } else if root_len(p) == 0 && cwd.is_none() {
        let wd = std::env::current_dir().ok()?;
        (Some(platform::bytes(wd.as_os_str()).to_vec()), p)
    } else {
        (cwd.map(<[u8]>::to_vec), p)
    };
    // The root (`/`, `C:/`, `//server/share/`) of the result: never cut by `..`.
    let (mut out, rel, root) = if root_len(rel) > 0 {
        let n = root_len(rel);
        let root = rel[..n].to_vec();
        let kept = root.strip_suffix(b"/").unwrap_or(&root).to_vec();
        (kept, &rel[n..], root)
    } else if rel.first() == Some(&b'/') {
        // Windows only: `/x` is on the drive (share) of the base.
        let root = base
            .as_deref()
            .map_or_else(Vec::new, |b| b[..root_len(b)].to_vec());
        let kept = root.strip_suffix(b"/").unwrap_or(&root).to_vec();
        (kept, rel, root)
    } else {
        let base = base?;
        let root = base[..root_len(&base)].to_vec();
        (base, rel, root)
    };
    for part in rel.split(|&c| c == b'/') {
        match part {
            b".." => {
                if let Some(at) = out.iter().rposition(|&c| c == b'/')
                    && at + 1 >= root.len()
                {
                    out.truncate(at);
                }
            }
            b"." | b"" => {}
            _ => {
                out.push(b'/');
                out.extend_from_slice(part);
            }
        }
    }
    if out.len() < root.len() {
        out = root;
    }
    if out.is_empty() {
        out.push(b'/');
    }
    (out.len() < 4096).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abspath_is_lexical() {
        let u = Os::Unix;
        assert_eq!(
            abspath_with(u, b"a/./b/../c", Some(b"/x"), None).unwrap(),
            b"/x/a/c"
        );
        assert_eq!(abspath_with(u, b"/..", None, None).unwrap(), b"/");
        assert_eq!(abspath_with(u, b"~/d", None, Some(b"/h")).unwrap(), b"/h/d");
        assert_eq!(abspath_with(u, b"~/d", Some(b"/c"), None), None);
    }

    #[test]
    fn abspath_windows_forms() {
        let w = Os::Windows;
        let abs =
            |p: &[u8], cwd: &[u8]| abspath_with(w, p, Some(cwd), Some(b"C:/Users/u")).unwrap();
        assert_eq!(abs(b"a/../b", b"C:/x"), b"C:/x/b");
        assert_eq!(abs(b"C:/..", b"D:/"), b"C:/");
        assert_eq!(abs(b"../..", b"C:/x"), b"C:/");
        assert_eq!(abs(b"//srv/share/d/..", b"C:/"), b"//srv/share/");
        assert_eq!(abs(b"../../..", b"//srv/share/a/b"), b"//srv/share/");
        assert_eq!(abs(b"~/d", b"C:/"), b"C:/Users/u/d");
    }

    #[test]
    fn drive_less_rooted_path_uses_the_cwd_drive() {
        let w = Os::Windows;
        let abs = |p: &[u8], cwd: &[u8]| abspath_with(w, p, Some(cwd), None).unwrap();
        assert_eq!(abs(b"/x", b"D:/p/q"), b"D:/x");
        assert_eq!(abs(b"/x/../..", b"D:/p"), b"D:/");
        assert_eq!(abs(b"/x", b"//srv/share/a"), b"//srv/share/x");
    }

    #[test]
    fn strerror_text_uses_glibc_words() {
        let kind = |k: io::ErrorKind| strerror_windows(&io::Error::from(k));
        assert_eq!(kind(io::ErrorKind::NotFound), "No such file or directory");
        assert_eq!(kind(io::ErrorKind::PermissionDenied), "Permission denied");
        assert_eq!(kind(io::ErrorKind::AlreadyExists), "File exists");
        assert_eq!(
            kind(io::ErrorKind::DirectoryNotEmpty),
            "Directory not empty"
        );
        assert_eq!(kind(io::ErrorKind::ResourceBusy), "Device or resource busy");
        assert_eq!(kind(io::ErrorKind::InvalidFilename), "File name too long");
        let other = io::Error::other("custom text");
        assert_eq!(strerror_windows(&other), "custom text");
        let raw = io::Error::other("boom (os error 5)");
        assert_eq!(strerror_windows(&raw), "boom");
    }
}
