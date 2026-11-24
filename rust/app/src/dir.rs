//! Reading a directory into entries (`dentfill`); disk-usage mode plugs in through
//! [`DuHook`] (du.rs).

use crate::sort::{
    DIR_OR_DIRLNK, Entry, FILE_MISSING, FILE_YOUNG, HARD_LINK, SYM_ORPHAN, TimeType,
};
use rustix::fd::{AsFd, BorrowedFd};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat};
use std::ffi::CStr;

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFDIR: u32 = 0o040000;

/// `selforparent`
pub fn selforparent(name: &[u8]) -> bool {
    name == b"." || name == b".."
}

/// What `dentfill` reads from `cfg` and the globals.
#[derive(Debug, Clone, Copy, Default)]
pub struct FillCfg {
    pub showhidden: bool,
    pub timetype: TimeType,
    /// `listpath` is set: symlink sizes are kept.
    pub listpath: bool,
}

/// The `struct stat` fields `dentfill` uses; all zero for a missing file (the original's
/// memset).
#[derive(Default, Clone, Copy)]
struct Info {
    mode: u32,
    size: i64,
    nlink: u64,
    uid: u32,
    gid: u32,
    times: [(i64, u32); 3],
}

impl Info {
    fn of(sb: &Stat) -> Info {
        Info {
            mode: sb.st_mode,
            size: sb.st_size,
            nlink: sb.st_nlink,
            uid: sb.st_uid,
            gid: sb.st_gid,
            times: [
                (sb.st_atime, sb.st_atime_nsec as u32),
                (sb.st_ctime, sb.st_ctime_nsec as u32),
                (sb.st_mtime, sb.st_mtime_nsec as u32),
            ],
        }
    }

    fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
}

fn raw_stat(dir: &impl AsFd, name: &CStr, follow: bool) -> Option<Stat> {
    let at = if follow {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    };
    rustix::fs::statat(dir, name, at).ok()
}

fn stat(dir: &impl AsFd, name: &CStr, follow: bool) -> Option<Info> {
    raw_stat(dir, name, follow).map(|sb| Info::of(&sb))
}

/// dentfill's `cfg.blkorder` branches (implemented in du.rs).
pub trait DuHook {
    /// Setup after the directory is opened; false goes straight to `exit:`.
    fn begin(&mut self, fd: BorrowedFd<'_>, path: &[u8]) -> bool;
    /// A hidden entry while hidden files are not shown; false stops reading.
    fn hidden(&mut self, fd: BorrowedFd<'_>, path: &[u8], name: &CStr) -> bool;
    /// An entry about to be added at `idx` (`sb` None: missing file); sets its `blocks`;
    /// false stops reading without adding it.
    fn entry(
        &mut self,
        fd: BorrowedFd<'_>,
        path: &[u8],
        name: &CStr,
        sb: Option<&Stat>,
        idx: usize,
        blocks: &mut u64,
    ) -> bool;
    /// `exit:`, with the entries read.
    fn finish(&mut self, ents: &mut [Entry]);
}

/// `dentfill`: the entries of `path` in directory order; empty if it cannot be read.
pub fn dentfill(path: &[u8], cfg: FillCfg) -> Vec<Entry> {
    dentfill_with(path, cfg, None)
}

/// `dentfill` with the disk-usage branches when `du` is given (`cfg.blkorder`).
pub fn dentfill_with(path: &[u8], cfg: FillCfg, mut du: Option<&mut dyn DuHook>) -> Vec<Entry> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let Ok(fd) = rustix::fs::open(&*crate::platform::os(path), flags, Mode::empty()) else {
        return Vec::new();
    };
    let Ok(dir) = Dir::read_from(&fd) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(h) = du.as_deref_mut()
        && !h.begin(fd.as_fd(), path)
    {
        h.finish(&mut out);
        return out;
    }
    let mut nofollow: Option<bool> = None;
    for dp in dir {
        let Ok(dp) = dp else { break };
        let cname = dp.file_name();
        let name = cname.to_bytes();
        let d_type = dp.file_type();
        // Decided on the first entry read, as the original does.
        let nofollow = *nofollow.get_or_insert(du.is_some() || d_type == FileType::Unknown);
        if selforparent(name) {
            continue;
        }
        if !cfg.showhidden && name.first() == Some(&b'.') {
            match du.as_deref_mut() {
                None => continue,
                Some(h) => {
                    if h.hidden(fd.as_fd(), path, cname) {
                        continue;
                    }
                    break;
                }
            }
        }
        let (raw, mut entflags) = match raw_stat(&fd, cname, !nofollow) {
            Some(sb) => (Some(sb), 0),
            None if nofollow => (None, FILE_MISSING),
            None => match raw_stat(&fd, cname, false) {
                Some(sb) => (Some(sb), SYM_ORPHAN),
                None => (None, FILE_MISSING),
            },
        };
        let sb = raw.as_ref().map(Info::of).unwrap_or_default();
        let (sec, nsec) = sb.times[match cfg.timetype {
            TimeType::Access => 0,
            TimeType::Change => 1,
            TimeType::Mod => 2,
        }];
        if now - sb.times[2].0 <= 300 || now - sb.times[1].0 <= 300 {
            entflags |= FILE_YOUNG;
        }
        let (mode, size) = if !nofollow && d_type == FileType::Symlink {
            (
                (sb.mode & !S_IFMT) | S_IFLNK,
                if cfg.listpath { sb.size } else { 0 },
            )
        } else {
            (sb.mode, sb.size)
        };
        let mut blocks = 0;
        if let Some(h) = du.as_deref_mut()
            && !h.entry(
                fd.as_fd(),
                path,
                cname,
                raw.as_ref(),
                out.len(),
                &mut blocks,
            )
        {
            break;
        }
        let mut flags = if sb.is_dir() || sb.nlink <= 1 {
            0
        } else {
            HARD_LINK
        };
        flags |= entflags;
        let target_is_dir = if nofollow {
            if sb.mode & S_IFMT == S_IFLNK {
                stat(&fd, cname, true).is_some_and(|t| t.is_dir())
            } else {
                sb.is_dir()
            }
        } else {
            d_type == FileType::Directory
                || ((d_type == FileType::Symlink || d_type == FileType::Unknown) && sb.is_dir())
        };
        if target_is_dir {
            flags |= DIR_OR_DIRLNK;
        }
        out.push(Entry {
            name: name.to_vec(),
            sec,
            nsec,
            mode,
            size,
            blocks,
            flags,
            uid: sb.uid,
            gid: sb.gid,
        });
    }
    if let Some(h) = du {
        h.finish(&mut out);
    }
    out
}
