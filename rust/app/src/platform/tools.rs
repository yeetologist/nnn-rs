//! Finding external programs: a native `PATH`/`PATHEXT` search (Windows has no `which`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// `PATHEXT` when unset or empty.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The `PATHEXT` entries, each starting with `.`.
fn pathext_list(pathext: Option<&OsStr>) -> Vec<String> {
    let s = pathext
        .and_then(OsStr::to_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_PATHEXT);
    s.split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            if e.starts_with('.') {
                e.to_owned()
            } else {
                format!(".{e}")
            }
        })
        .collect()
}

/// The directories of a `PATH` value in order, without empty entries and without one pair of
/// surrounding quotes.
pub(crate) fn path_dirs(path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(p) = path_var else {
        return Vec::new();
    };
    std::env::split_paths(p)
        .filter_map(|d| {
            let d = match d.to_str() {
                Some(s) if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') => {
                    PathBuf::from(&s[1..s.len() - 1])
                }
                _ => d,
            };
            (!d.as_os_str().is_empty()).then_some(d)
        })
        .collect()
}

/// `base` as is when its extension is one of `exts`, then `base` with each of `exts` added.
fn candidates(base: &Path, exts: &[String]) -> Vec<PathBuf> {
    let known = base
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| exts.iter().any(|x| x[1..].eq_ignore_ascii_case(e)));
    let mut v = Vec::with_capacity(exts.len() + 1);
    if known {
        v.push(base.to_path_buf());
    }
    for x in exts {
        let mut s = base.as_os_str().to_owned();
        s.push(x);
        v.push(PathBuf::from(s));
    }
    v
}

/// The program `name` as `cmd` would find it: a name with a `/` or `\` is a path checked
/// on its own; any other is looked for in each `PATH` directory in order, as is when its
/// extension is in `PATHEXT`, then with each `PATHEXT` extension added. Only files count; the
/// current directory is never searched.
pub fn which_in(name: &[u8], path_var: Option<&OsStr>, pathext: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let exts = pathext_list(pathext);
    let name_path = super::path(name);
    let first_file = |base: &Path| candidates(base, &exts).into_iter().find(|p| p.is_file());
    if name.iter().any(|&c| c == b'/' || c == b'\\') {
        return first_file(&name_path);
    }
    path_dirs(path_var)
        .iter()
        .find_map(|d| first_file(&d.join(&name_path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fresh temp dir holding `files` (empty files; a trailing `/` makes a directory).
    pub(super) fn tree(files: &[&str]) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "nnn-tools-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for f in files {
            let p = root.join(f.trim_end_matches('/'));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            if f.ends_with('/') {
                std::fs::create_dir_all(&p).unwrap();
            } else {
                std::fs::write(&p, b"").unwrap();
            }
        }
        root
    }

    pub(super) fn paths(dirs: &[PathBuf]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    fn ext(s: &str) -> Option<&OsStr> {
        Some(OsStr::new(s))
    }

    #[test]
    fn which_finds_exe_via_pathext() {
        let t = tree(&["d1/foo.EXE"]);
        let p = paths(&[t.join("d1")]);
        assert_eq!(
            which_in(b"foo", Some(&p), ext(".COM;.EXE")),
            Some(t.join("d1").join("foo.EXE"))
        );
    }

    #[test]
    fn which_respects_path_order() {
        let t = tree(&["d1/foo.exe", "d2/foo.exe"]);
        let p = paths(&[t.join("d2"), t.join("d1")]);
        assert_eq!(
            which_in(b"foo", Some(&p), ext(".exe")),
            Some(t.join("d2").join("foo.exe"))
        );
    }

    #[test]
    fn which_pathext_order_wins_within_dir() {
        let t = tree(&["d/foo.bat", "d/foo.exe"]);
        let p = paths(&[t.join("d")]);
        assert_eq!(
            which_in(b"foo", Some(&p), ext(".exe;.bat")),
            Some(t.join("d").join("foo.exe"))
        );
    }

    #[test]
    fn which_name_with_known_extension() {
        let t = tree(&["d/foo.exe", "d/foo.exe.exe"]);
        let p = paths(&[t.join("d")]);
        assert_eq!(
            which_in(b"foo.exe", Some(&p), ext(".EXE")),
            Some(t.join("d").join("foo.exe"))
        );
    }

    #[test]
    fn which_skips_empty_and_quoted_dirs() {
        let t = tree(&["d/foo.exe"]);
        let sep = if cfg!(windows) { ";" } else { ":" };
        let quoted = format!("{sep}\"{}\"", t.join("d").display());
        assert_eq!(
            which_in(b"foo", Some(OsStr::new(&quoted)), ext(".exe")),
            Some(t.join("d").join("foo.exe"))
        );
    }

    #[test]
    fn which_missing_is_none() {
        let t = tree(&["d/bar.exe"]);
        let p = paths(&[t.join("d")]);
        assert_eq!(which_in(b"foo", Some(&p), ext(".EXE")), None);
    }

    #[test]
    fn which_unset_path_is_none() {
        assert_eq!(which_in(b"foo", None, ext(".EXE")), None);
    }

    #[test]
    fn which_directory_named_like_tool_is_not_a_hit() {
        let t = tree(&["d/foo.exe/"]);
        let p = paths(&[t.join("d")]);
        assert_eq!(which_in(b"foo", Some(&p), ext(".EXE")), None);
    }

    #[test]
    fn which_default_pathext() {
        let t = tree(&["d/foo.CMD"]);
        let p = paths(&[t.join("d")]);
        assert_eq!(
            which_in(b"foo", Some(&p), None),
            Some(t.join("d").join("foo.CMD"))
        );
    }
}
