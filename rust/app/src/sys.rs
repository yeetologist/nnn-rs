//! Thin wrappers over the system calls the original makes, with its error output.

use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn path(p: &[u8]) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(p))
}

/// `strerror(errno)`: Rust's message without its " (os error N)" suffix.
pub fn strerror(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}

/// `xerror()`: `perror` with the original's C source line as the prefix (ADR-011).
pub fn xerror(line: u32, e: &io::Error) {
    let _ = writeln!(io::stderr(), "{line}: {}", strerror(e));
}

/// `open(p, O_WRONLY | O_CREAT, 0600)` then `close`.
pub fn create_file(p: &[u8]) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path(p))
        .map(drop)
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
    let (base, rel): (Option<Vec<u8>>, &[u8]) = if p.starts_with(b"~/") {
        (home.map(<[u8]>::to_vec), &p[2..])
    } else if p.first() != Some(&b'/') && cwd.is_none() {
        let wd = std::env::current_dir().ok()?;
        (Some(wd.as_os_str().as_bytes().to_vec()), p)
    } else {
        (cwd.map(<[u8]>::to_vec), p)
    };
    let mut out = if rel.first() != Some(&b'/') {
        base?
    } else {
        Vec::new()
    };
    for part in rel.split(|&c| c == b'/') {
        match part {
            b".." => {
                if let Some(at) = out.iter().rposition(|&c| c == b'/') {
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
        assert_eq!(
            abspath_in(b"a/./b/../c", Some(b"/x"), None).unwrap(),
            b"/x/a/c"
        );
        assert_eq!(abspath_in(b"/..", None, None).unwrap(), b"/");
        assert_eq!(abspath_in(b"~/d", None, Some(b"/h")).unwrap(), b"/h/d");
        assert_eq!(abspath_in(b"~/d", Some(b"/c"), None), None);
    }
}
