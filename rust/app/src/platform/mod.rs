//! The OS-independent API (re-exports the active backend) and the path helpers.
//!
//! Internal paths are byte strings with `/` separators on every OS. On Unix the bytes are the
//! OS's own; on Windows they are WTF-8 (what `OsStr::as_encoded_bytes` holds), with `\` shown as `/`.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
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
}
