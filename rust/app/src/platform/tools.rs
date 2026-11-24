//! Finding external programs: a native `PATH`/`PATHEXT` search (Windows has no `which`).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

/// The tools nnn runs that Microsoft's Coreutils for Windows provides.
pub const COREUTILS_TOOLS: [&str; 5] = ["cp", "mv", "rm", "stat", "xargs"];

/// How a Coreutils install is laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// The winget install: one program per tool name (`cp.exe`, …) beside
    /// `coreutils-manager.exe` in this bin directory.
    Installed(PathBuf),
    /// The release zip: this `coreutils.exe` runs a tool named as its first argument.
    Multicall(PathBuf),
}

/// A detected Coreutils for Windows.
#[derive(Debug)]
pub struct Coreutils {
    pub layout: Layout,
    /// Multicall only: the tools `coreutils.exe --list` names, read on first use (any uutils
    /// build is called `coreutils.exe`, and not every one has `xargs` or `stat`).
    listed: OnceLock<Vec<String>>,
}

/// The names `coreutils --list` prints, one per line.
pub fn parse_list(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The tools the multicall `exe` provides; none when it does not run.
fn list_tools(exe: &Path) -> Vec<String> {
    use std::process::{Command, Stdio};
    match Command::new(exe)
        .arg("--list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => parse_list(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

impl Coreutils {
    /// The program and leading arguments that run `tool` (one of `COREUTILS_TOOLS`), or
    /// `None` for any other name or a tool the install lacks (`coreutils-manager disable`).
    pub fn argv0(&self, tool: &[u8]) -> Option<Vec<OsString>> {
        let tool = std::str::from_utf8(tool).ok()?;
        if !COREUTILS_TOOLS.contains(&tool) {
            return None;
        }
        match &self.layout {
            Layout::Installed(dir) => {
                let exe = dir.join(format!("{tool}.exe"));
                exe.is_file().then(|| vec![exe.into_os_string()])
            }
            Layout::Multicall(exe) => self
                .listed
                .get_or_init(|| list_tools(exe))
                .iter()
                .any(|t| t == tool)
                .then(|| vec![exe.clone().into_os_string(), tool.into()]),
        }
    }
}

/// Coreutils as found on `path_var` (each directory in order: `coreutils-manager.exe` there
/// means the installed layout, else `coreutils.exe` the multicall one), else in
/// `<program_files>/coreutils/bin` (the winget location). Only files are checked; nothing runs
/// (a multicall `coreutils.exe` is asked for its `--list` on the first `argv0`).
pub fn detect_in(path_var: Option<&OsStr>, program_files: Option<&OsStr>) -> Option<Coreutils> {
    let at = |dir: &Path| {
        if dir.join("coreutils-manager.exe").is_file() {
            Some(Layout::Installed(dir.to_path_buf()))
        } else {
            let exe = dir.join("coreutils.exe");
            exe.is_file().then_some(Layout::Multicall(exe))
        }
    };
    let found = path_dirs(path_var).iter().find_map(|d| at(d)).or_else(|| {
        let bin = Path::new(program_files?).join("coreutils").join("bin");
        bin.join("coreutils-manager.exe")
            .is_file()
            .then_some(Layout::Installed(bin))
    });
    found.map(|layout| Coreutils {
        layout,
        listed: OnceLock::new(),
    })
}

/// Detection is on unless `NNN_COREUTILS` is `0`.
pub fn enabled(var: Option<&OsStr>) -> bool {
    var != Some(OsStr::new("0"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn detect_installed_layout() {
        let t = tree(&["d/coreutils-manager.exe", "d/cp.exe"]);
        let c = detect_in(Some(&paths(&[t.join("d")])), None).unwrap();
        assert_eq!(c.layout, Layout::Installed(t.join("d")));
        assert_eq!(
            c.argv0(b"cp"),
            Some(vec![t.join("d").join("cp.exe").into_os_string()])
        );
    }

    #[test]
    fn detect_multicall_layout() {
        let t = tree(&["d/coreutils.exe"]);
        let c = detect_in(Some(&paths(&[t.join("d")])), None).unwrap();
        let exe = t.join("d").join("coreutils.exe");
        assert_eq!(c.layout, Layout::Multicall(exe.clone()));
        c.listed.set(vec!["cp".into(), "rm".into()]).unwrap();
        assert_eq!(
            c.argv0(b"rm"),
            Some(vec![exe.into_os_string(), OsString::from("rm")])
        );
    }

    #[test]
    fn multicall_without_listed_tool_is_none() {
        let t = tree(&["d/coreutils.exe"]);
        let c = detect_in(Some(&paths(&[t.join("d")])), None).unwrap();
        c.listed.set(vec!["cp".into(), "rm".into()]).unwrap();
        assert_eq!(c.argv0(b"xargs"), None);
        assert_eq!(c.argv0(b"stat"), None);
        assert!(c.argv0(b"cp").is_some());
    }

    #[test]
    fn multicall_unrunnable_offers_nothing() {
        let t = tree(&["d/coreutils.exe"]);
        let c = detect_in(Some(&paths(&[t.join("d")])), None).unwrap();
        assert_eq!(c.argv0(b"cp"), None);
    }

    #[test]
    fn parse_list_reads_names() {
        let l = parse_list("[\narch\ncoreutils-manager\ncp\r\nxargs\n");
        assert!(l.iter().any(|t| t == "cp"));
        assert!(l.iter().any(|t| t == "xargs"));
        assert!(!l.iter().any(|t| t.is_empty()));
    }

    #[test]
    fn detect_first_path_hit_wins() {
        let t = tree(&["d1/coreutils.exe", "d2/coreutils-manager.exe"]);
        let c = detect_in(Some(&paths(&[t.join("d1"), t.join("d2")])), None).unwrap();
        assert_eq!(
            c.layout,
            Layout::Multicall(t.join("d1").join("coreutils.exe"))
        );
    }

    #[test]
    fn detect_program_files_fallback() {
        let t = tree(&["pf/coreutils/bin/coreutils-manager.exe", "empty/"]);
        let pf = t.join("pf");
        let c = detect_in(Some(&paths(&[t.join("empty")])), Some(pf.as_os_str())).unwrap();
        assert_eq!(
            c.layout,
            Layout::Installed(pf.join("coreutils").join("bin"))
        );
    }

    #[test]
    fn detect_none() {
        let t = tree(&["d/cp.exe"]);
        assert!(detect_in(Some(&paths(&[t.join("d")])), Some(t.as_os_str())).is_none());
        assert!(detect_in(None, None).is_none());
    }

    #[test]
    fn argv0_installed_missing_tool_is_none() {
        let t = tree(&["d/coreutils-manager.exe", "d/cp.exe"]);
        let c = detect_in(Some(&paths(&[t.join("d")])), None).unwrap();
        assert_eq!(c.argv0(b"rm"), None);
        assert!(c.argv0(b"cp").is_some());
    }

    #[test]
    fn argv0_unknown_tool_is_none() {
        let t = tree(&[
            "i/coreutils-manager.exe",
            "i/sed.exe",
            "i/cp.exe.exe",
            "m/coreutils.exe",
        ]);
        for d in ["i", "m"] {
            let c = detect_in(Some(&paths(&[t.join(d)])), None).unwrap();
            assert_eq!(c.argv0(b"sed"), None);
            assert_eq!(c.argv0(b"cp.exe"), None);
        }
    }

    #[test]
    fn enabled_reads_env_value() {
        assert!(enabled(None));
        assert!(!enabled(Some(OsStr::new("0"))));
        assert!(enabled(Some(OsStr::new("1"))));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs Microsoft Coreutils"]
    fn real_cp_through_command() {
        let t = tree(&[]);
        let src = t.join("src");
        let dst = t.join("dst");
        std::fs::write(&src, b"hello").unwrap();
        let b = |p: &Path| crate::platform::into_bytes(p.as_os_str().to_owned());
        let argv = [
            b"cp".to_vec(),
            b"-rp".to_vec(),
            b"--".to_vec(),
            b(&src),
            b(&dst),
        ];
        let mut cmd = crate::platform::command(&argv, 1);
        assert!(Path::new(cmd.get_program()).is_absolute());
        assert!(cmd.status().unwrap().success());
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
    }
}
