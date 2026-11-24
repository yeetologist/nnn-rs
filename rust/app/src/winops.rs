//! Native copy, move and remove of the selection, the file-info panel and directory-change
//! polling for Windows, which has no cp, mv, rm, stat or inotify. Compiled on every OS so its
//! tests run on Linux too; nnn calls it only on Windows.

use crate::platform::{self, Errno, FileInfo, path};
use std::fs;
use std::io;
use std::time::SystemTime;

/// The last component of `p` (trailing `/` ignored).
fn base(p: &[u8]) -> &[u8] {
    let p = p.strip_suffix(b"/").unwrap_or(p);
    p.rsplit(|&c| c == b'/').next().unwrap_or(p)
}

/// `dir/name`.
fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut v = dir.to_vec();
    if v.last() != Some(&b'/') {
        v.push(b'/');
    }
    v.extend_from_slice(name);
    v
}

/// `EINVAL` when `dest_dir/<name of src>` is `src` itself, or (for a real directory) inside
/// it. A link at `src` is not followed: the link itself is what is copied or moved.
fn check_target(src: &[u8], dest_dir: &[u8]) -> io::Result<()> {
    let parent = fs::canonicalize(path(platform::xdirname(src)))?;
    let d = fs::canonicalize(path(dest_dir))?;
    if parent == d {
        return Err(platform::error(Errno::Inval));
    }
    let real_dir = fs::symlink_metadata(path(src))?.is_dir();
    if real_dir && d.starts_with(parent.join(&*platform::os(base(src)))) {
        return Err(platform::error(Errno::Inval));
    }
    Ok(())
}

/// `cp -iRp src dest_dir`: copies `src` (a directory with all it holds) into `dest_dir`,
/// keeping modification times; links are copied as links. On an existing file `ask(name)`
/// decides whether to overwrite it (`false` skips it); directories merge.
pub fn copy_into(
    src: &[u8],
    dest_dir: &[u8],
    ask: &mut dyn FnMut(&[u8]) -> bool,
) -> io::Result<()> {
    check_target(src, dest_dir)?;
    copy_entry(src, &join(dest_dir, base(src)), ask)
}

fn copy_entry(src: &[u8], dst: &[u8], ask: &mut dyn FnMut(&[u8]) -> bool) -> io::Result<()> {
    let meta = fs::symlink_metadata(path(src))?;
    let existing = fs::symlink_metadata(path(dst)).ok();
    if meta.is_dir() {
        match existing {
            Some(m) if m.is_dir() => {}
            Some(_) => return Err(platform::error(Errno::Exist)),
            None => fs::create_dir(path(dst))?,
        }
        for e in fs::read_dir(path(src))? {
            let name = platform::into_bytes(e?.file_name());
            copy_entry(&join(src, &name), &join(dst, &name), ask)?;
        }
        return set_mtime(dst, meta.modified()?);
    }
    if let Some(m) = existing {
        // cp: cannot overwrite directory with non-directory.
        if m.is_dir() {
            return Err(platform::error(Errno::Exist));
        }
        if !ask(base(dst)) {
            return Ok(());
        }
        // A link replaces the file or link there (never a directory, refused above).
        if meta.file_type().is_symlink() {
            remove_all(dst)?;
        }
    }
    if meta.file_type().is_symlink() {
        let target = platform::readlink(src)?;
        return platform::symlink(
            &target,
            dst,
            fs::metadata(path(src)).is_ok_and(|m| m.is_dir()),
        );
    }
    fs::copy(path(src), path(dst))?;
    set_mtime(dst, meta.modified()?)
}

fn set_mtime(p: &[u8], t: SystemTime) -> io::Result<()> {
    open_for_times(p)?.set_modified(t)
}

/// A handle that may set the times of a file or directory (even a read-only one).
#[cfg(windows)]
fn open_for_times(p: &[u8]) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
    };
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path(p))
}

/// A handle that may set the times of a file or directory (the owner's `futimens`).
#[cfg(not(windows))]
fn open_for_times(p: &[u8]) -> io::Result<fs::File> {
    fs::File::open(path(p))
}

/// `mv -i src dest_dir`: renames `src` into `dest_dir` (`ask(name)` first when the target
/// exists, `false` skips it); across volumes it is copied, then removed.
pub fn move_into(
    src: &[u8],
    dest_dir: &[u8],
    ask: &mut dyn FnMut(&[u8]) -> bool,
) -> io::Result<()> {
    check_target(src, dest_dir)?;
    let target = join(dest_dir, base(src));
    if fs::symlink_metadata(path(&target)).is_ok() && !ask(base(src)) {
        return Ok(());
    }
    match fs::rename(path(src), path(&target)) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            copy_entry(src, &target, &mut |_| true)?;
            remove_all(src)
        }
        r => r,
    }
}

/// `rm -rf path`: a directory with all it holds; a link itself, never what it points to.
pub fn remove_all(p: &[u8]) -> io::Result<()> {
    let meta = fs::symlink_metadata(path(p))?;
    if meta.is_dir() {
        fs::remove_dir_all(path(p))
    } else if is_dir_link(&meta) {
        fs::remove_dir(path(p))
    } else {
        fs::remove_file(path(p))
    }
}

/// `rm -ri`'s answer for one entry: unless `force`, `confirm(path)` decides first. True
/// when `path` was removed, false when declined.
pub fn remove_confirmed(
    p: &[u8],
    force: bool,
    confirm: &mut dyn FnMut(&[u8]) -> bool,
) -> io::Result<bool> {
    if !force && !confirm(p) {
        return Ok(false);
    }
    remove_all(p).map(|()| true)
}

/// A link to a directory (Windows removes it as a directory).
#[cfg(windows)]
fn is_dir_link(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    m.file_type().is_symlink_dir()
}

#[cfg(not(windows))]
fn is_dir_link(_m: &fs::Metadata) -> bool {
    false
}

/// The file attributes `Attrib:` names, in this order.
const ATTRIBS: [(u32, &str); 13] = [
    (0x1, "READONLY"),
    (0x2, "HIDDEN"),
    (0x4, "SYSTEM"),
    (0x10, "DIRECTORY"),
    (0x20, "ARCHIVE"),
    (0x80, "NORMAL"),
    (0x100, "TEMPORARY"),
    (0x200, "SPARSE_FILE"),
    (0x400, "REPARSE_POINT"),
    (0x800, "COMPRESSED"),
    (0x1000, "OFFLINE"),
    (0x2000, "NOT_CONTENT_INDEXED"),
    (0x4000, "ENCRYPTED"),
];

fn attrib_names(a: u32) -> String {
    ATTRIBS
        .iter()
        .filter(|&&(bit, _)| a & bit != 0)
        .map(|&(_, name)| name)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn attributes(p: &[u8]) -> u32 {
    use std::os::windows::fs::MetadataExt;
    fs::symlink_metadata(path(p)).map_or(0, |m| m.file_attributes())
}

#[cfg(not(windows))]
fn attributes(_p: &[u8]) -> u32 {
    0
}

/// "YYYY-MM-DD HH:MM:SS" in local time.
fn full_time(secs: i64) -> String {
    let t = crate::locale::localtime(secs);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year, t.mon, t.mday, t.hour, t.min, t.sec
    )
}

/// What `stat` shows of `path` (not following a link), for the file-info window.
pub fn file_info_text(p: &[u8]) -> Vec<u8> {
    match platform::stat(p, false) {
        Ok(st) => format_info(p, &st, attributes(p)),
        Err(e) => {
            let mut v = b"stat: cannot stat '".to_vec();
            v.extend_from_slice(p);
            v.extend_from_slice(format!("': {}\n", crate::sys::strerror(&e)).as_bytes());
            v
        }
    }
}

fn format_info(p: &[u8], st: &FileInfo, attrs: u32) -> Vec<u8> {
    let mut v = b"  File: ".to_vec();
    v.extend_from_slice(p);
    if st.mode & 0o170000 == 0o120000
        && let Ok(target) = platform::readlink(p)
    {
        v.extend_from_slice(b" -> ");
        v.extend_from_slice(&target);
    }
    v.push(b'\n');
    v.extend_from_slice(
        format!(
            "  Size: {}  Blocks: {}\nAccess: ({:04o}/{})\nModify: {}\nChange: {}\nAttrib: {}\n",
            st.size,
            st.blocks512,
            st.mode & 0o7777,
            crate::fmt::get_lsperms(st.mode),
            full_time(st.mtime.0),
            full_time(st.ctime.0),
            attrib_names(attrs)
        )
        .as_bytes(),
    );
    v
}

/// A directory's modification time and entry count (`None` when it cannot be read).
fn snapshot(p: &[u8]) -> Option<(SystemTime, usize)> {
    let m = fs::metadata(path(p)).ok()?.modified().ok()?;
    Some((m, fs::read_dir(path(p)).ok()?.count()))
}

/// Polls a directory for changes: its modification time and entry count against the last
/// look.
pub struct DirWatch {
    path: Vec<u8>,
    last: Option<(SystemTime, usize)>,
}

impl DirWatch {
    pub fn new(p: &[u8]) -> DirWatch {
        DirWatch {
            path: p.to_vec(),
            last: snapshot(p),
        }
    }

    /// The directory changed since `new` or the last call.
    pub fn changed(&mut self) -> bool {
        let now = snapshot(&self.path);
        let changed = now != self.last;
        self.last = now;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    /// A fresh, empty temp dir for one test.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nnn-winops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn b(p: &Path) -> Vec<u8> {
        platform::bytes(p.as_os_str()).into_owned()
    }

    fn never(_: &[u8]) -> bool {
        panic!("no conflict expected")
    }

    #[test]
    fn copy_into_copies_tree_and_mtime() {
        let dir = scratch("cp");
        let src = dir.join("s");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"abc").unwrap();
        std::fs::write(src.join("sub").join("b"), b"").unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        std::fs::File::options()
            .write(true)
            .open(src.join("a.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        copy_into(&b(&src), &b(&dir.join("d")), &mut never).unwrap();
        let out = dir.join("d").join("s");
        assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"abc");
        assert!(out.join("sub").join("b").is_file());
        let m = std::fs::metadata(out.join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(m, old);
        assert!(src.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_into_asks_on_conflict() {
        let dir = scratch("ask");
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("f"), b"new").unwrap();
        std::fs::write(dir.join("d").join("f"), b"old").unwrap();
        let mut asked = Vec::new();
        let mut ask = |n: &[u8]| {
            asked.push(n.to_vec());
            false
        };
        copy_into(&b(&dir.join("f")), &b(&dir.join("d")), &mut ask).unwrap();
        assert_eq!(asked, [b"f".to_vec()]);
        assert_eq!(std::fs::read(dir.join("d").join("f")).unwrap(), b"old");
        copy_into(&b(&dir.join("f")), &b(&dir.join("d")), &mut |_| true).unwrap();
        assert_eq!(std::fs::read(dir.join("d").join("f")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_into_itself_is_refused() {
        let dir = scratch("self");
        std::fs::create_dir_all(dir.join("s").join("in")).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
        let s = b(&dir.join("s"));
        assert!(copy_into(&s, &b(&dir.join("s").join("in")), &mut never).is_err());
        assert!(copy_into(&b(&dir.join("f")), &b(&dir), &mut |_| true).is_err());
        assert_eq!(std::fs::read(dir.join("f")).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_into_renames() {
        let dir = scratch("mv");
        std::fs::create_dir_all(dir.join("m").join("x")).unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        move_into(&b(&dir.join("m")), &b(&dir.join("d")), &mut never).unwrap();
        assert!(!dir.join("m").exists());
        assert!(dir.join("d").join("m").join("x").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_all_removes_tree() {
        let dir = scratch("rm");
        std::fs::create_dir_all(dir.join("t").join("u")).unwrap();
        std::fs::write(dir.join("t").join("u").join("f"), b"x").unwrap();
        std::fs::write(dir.join("g"), b"x").unwrap();
        remove_all(&b(&dir.join("t"))).unwrap();
        remove_all(&b(&dir.join("g"))).unwrap();
        assert!(!dir.join("t").exists());
        assert!(!dir.join("g").exists());
        assert!(remove_all(&b(&dir.join("g"))).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_watch_sees_new_file() {
        let dir = scratch("watch");
        let mut w = DirWatch::new(&b(&dir));
        assert!(!w.changed());
        std::fs::write(dir.join("new"), b"").unwrap();
        assert!(w.changed());
        assert!(!w.changed());
        std::fs::remove_file(dir.join("new")).unwrap();
        assert!(w.changed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_long_path() {
        let dir = scratch("long");
        let mut deep = dir.join("src");
        while b(&deep).len() < 300 {
            deep.push("abcdefghij");
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("f"), b"long").unwrap();
        let src = b(&deep.join("f"));
        assert!(src.len() >= 300);
        std::fs::create_dir(dir.join("d")).unwrap();
        copy_into(&src, &b(&dir.join("d")), &mut never).unwrap();
        assert_eq!(std::fs::read(dir.join("d").join("f")).unwrap(), b"long");
        // The long copy back, then removed through its long path.
        copy_into(&b(&dir.join("d").join("f")), &b(&deep), &mut |_| true).unwrap();
        remove_all(&b(&dir.join("src"))).unwrap();
        assert!(!dir.join("src").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn delete_locked_file_reports_error() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("locked");
        let f = dir.join("held");
        std::fs::write(&f, b"x").unwrap();
        // FILE_SHARE_READ | FILE_SHARE_WRITE, no FILE_SHARE_DELETE.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&f)
            .unwrap();
        let e = remove_all(&b(&f)).unwrap_err();
        let text = crate::sys::strerror(&e);
        assert!(
            text == "Permission denied" || text == "Device or resource busy",
            "{text}"
        );
        assert!(f.exists());
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_info_lines() {
        let dir = scratch("info");
        std::fs::write(dir.join("f"), b"abc").unwrap();
        let p = b(&dir.join("f"));
        let text = String::from_utf8(file_info_text(&p)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("  File: {}", String::from_utf8_lossy(&p)));
        assert!(lines[1].starts_with("  Size: 3  Blocks: "), "{}", lines[1]);
        assert!(lines[2].starts_with("Access: (0"), "{}", lines[2]);
        assert!(lines[2].ends_with("/-rw-r--r--)"), "{}", lines[2]);
        for (line, key) in [(lines[3], "Modify: "), (lines[4], "Change: ")] {
            assert!(line.starts_with(key), "{line}");
            assert_eq!(
                line.len(),
                key.len() + "YYYY-MM-DD HH:MM:SS".len(),
                "{line}"
            );
        }
        assert!(lines[5].starts_with("Attrib:"), "{}", lines[5]);
        assert!(
            String::from_utf8(file_info_text(&b(&dir.join("none"))))
                .unwrap()
                .contains("No such file or directory")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_confirmed_asks_unless_forced() {
        let dir = scratch("rmask");
        std::fs::create_dir_all(dir.join("t").join("u")).unwrap();
        let t = b(&dir.join("t"));
        let mut asked = Vec::new();
        let mut no = |p: &[u8]| {
            asked.push(p.to_vec());
            false
        };
        assert!(!remove_confirmed(&t, false, &mut no).unwrap());
        assert_eq!(asked, std::slice::from_ref(&t));
        assert!(dir.join("t").join("u").is_dir());
        assert!(remove_confirmed(&t, true, &mut never).unwrap());
        assert!(!dir.join("t").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_and_dir_never_replace_each_other() {
        let dir = scratch("kinds");
        std::fs::create_dir_all(dir.join("d").join("x").join("keep")).unwrap();
        std::fs::write(dir.join("x"), b"file").unwrap();
        // A file onto a directory: refused before asking, the directory stays.
        assert!(copy_into(&b(&dir.join("x")), &b(&dir.join("d")), &mut never).is_err());
        assert!(dir.join("d").join("x").join("keep").is_dir());
        // A directory onto a file: refused, the file stays.
        std::fs::create_dir_all(dir.join("s").join("x")).unwrap();
        std::fs::write(dir.join("e"), b"").unwrap();
        std::fs::create_dir(dir.join("d2")).unwrap();
        std::fs::write(dir.join("d2").join("x"), b"file").unwrap();
        assert!(
            copy_into(
                &b(&dir.join("s").join("x")),
                &b(&dir.join("d2")),
                &mut never
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dir.join("d2").join("x")).unwrap(), b"file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Makes the link `at` to `target`; false (with a note) where links need a privilege.
    fn link(target: &[u8], at: &Path, is_dir: bool) -> bool {
        match platform::symlink(target, &b(at), is_dir) {
            Ok(()) => true,
            Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied => {
                println!("note: no symlink privilege, link case skipped");
                false
            }
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn links_are_copied_not_followed() {
        let dir = scratch("links");
        std::fs::create_dir_all(dir.join("x").join("sub")).unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        let target = b(&dir.join("x"));
        if !link(&target, &dir.join("d").join("l"), true) {
            return;
        }
        // Into its own directory: refused, the link stays.
        let l = b(&dir.join("d").join("l"));
        assert!(copy_into(&l, &b(&dir.join("d")), &mut |_| true).is_err());
        assert_eq!(platform::readlink(&l).unwrap(), target);
        // A link to a directory moves into that directory's subtree (only the link moves).
        move_into(&l, &b(&dir.join("x").join("sub")), &mut never).unwrap();
        let moved = b(&dir.join("x").join("sub").join("l"));
        assert_eq!(platform::readlink(&moved).unwrap(), target);
        // A dangling link copies as it is.
        assert!(link(b"nowhere", &dir.join("dang"), false));
        copy_into(&b(&dir.join("dang")), &b(&dir.join("d")), &mut never).unwrap();
        assert_eq!(
            platform::readlink(&b(&dir.join("d").join("dang"))).unwrap(),
            b"nowhere"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attribute_names() {
        assert_eq!(attrib_names(0x1 | 0x2 | 0x20), "READONLY HIDDEN ARCHIVE");
        assert_eq!(attrib_names(0), "");
    }
}
