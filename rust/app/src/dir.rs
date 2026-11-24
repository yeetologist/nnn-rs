//! Reading a directory into entries (`dentfill`); disk-usage mode plugs in through
//! [`DuHook`] (du.rs).

use crate::platform::{DType, Dir, FileInfo};
use crate::sort::{
    DIR_OR_DIRLNK, Entry, FILE_MISSING, FILE_YOUNG, HARD_LINK, SYM_ORPHAN, TimeType,
};
use std::io;
use std::iter::Peekable;

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

impl FileInfo {
    fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
}

/// dentfill's `cfg.blkorder` branches (implemented in du.rs).
pub trait DuHook {
    /// Setup after the directory is opened; false goes straight to `exit:`.
    fn begin(&mut self, dir: &Dir, path: &[u8]) -> bool;
    /// A hidden entry while hidden files are not shown; false stops reading.
    fn hidden(&mut self, dir: &Dir, path: &[u8], name: &[u8]) -> bool;
    /// An entry about to be added at `idx` (`sb` None: missing file); sets its `blocks`;
    /// false stops reading without adding it.
    fn entry(
        &mut self,
        dir: &Dir,
        path: &[u8],
        name: &[u8],
        sb: Option<&FileInfo>,
        idx: usize,
        blocks: &mut u64,
    ) -> bool;
    /// `exit:`, with the entries read.
    fn finish(&mut self, ents: &mut [Entry]);
}

/// `AT_SYMLINK_NOFOLLOW` for the whole listing: with disk usage, or when the first entry
/// `readdir` gives (usually `.`) has no `d_type`, as the original decides it.
fn nofollow_for<I>(entries: &mut Peekable<I>, du: bool) -> bool
where
    I: Iterator<Item = io::Result<(Vec<u8>, DType)>>,
{
    du || matches!(entries.peek(), Some(Ok((_, DType::Unknown))))
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
    let Ok(dir) = Dir::open(path) else {
        return Vec::new();
    };
    let Ok(entries) = dir.raw_entries() else {
        return Vec::new();
    };
    let mut entries = entries.peekable();
    let mut out = Vec::new();
    if let Some(h) = du.as_deref_mut()
        && !h.begin(&dir, path)
    {
        h.finish(&mut out);
        return out;
    }
    let nofollow = nofollow_for(&mut entries, du.is_some());
    for dp in entries {
        let Ok((name, d_type)) = dp else { break };
        let name = name.as_slice();
        if selforparent(name) {
            continue;
        }
        if !cfg.showhidden && name.first() == Some(&b'.') {
            match du.as_deref_mut() {
                None => continue,
                Some(h) => {
                    if h.hidden(&dir, path, name) {
                        continue;
                    }
                    break;
                }
            }
        }
        let (raw, mut entflags) = match dir.stat_at(name, !nofollow) {
            Ok(sb) => (Some(sb), 0),
            Err(_) if nofollow => (None, FILE_MISSING),
            Err(_) => match dir.stat_at(name, false) {
                Ok(sb) => (Some(sb), SYM_ORPHAN),
                Err(_) => (None, FILE_MISSING),
            },
        };
        let sb = raw.unwrap_or_default();
        let (sec, nsec) = match cfg.timetype {
            TimeType::Access => sb.atime,
            TimeType::Change => sb.ctime,
            TimeType::Mod => sb.mtime,
        };
        if now - sb.mtime.0 <= 300 || now - sb.ctime.0 <= 300 {
            entflags |= FILE_YOUNG;
        }
        let (mode, size) = if !nofollow && d_type == DType::Symlink {
            (
                (sb.mode & !S_IFMT) | S_IFLNK,
                if cfg.listpath { sb.size } else { 0 },
            )
        } else {
            (sb.mode, sb.size)
        };
        let mut blocks = 0;
        if let Some(h) = du.as_deref_mut()
            && !h.entry(&dir, path, name, raw.as_ref(), out.len(), &mut blocks)
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
                dir.stat_at(name, true).is_ok_and(|t| t.is_dir())
            } else {
                sb.is_dir()
            }
        } else {
            d_type == DType::Dir
                || ((d_type == DType::Symlink || d_type == DType::Unknown) && sb.is_dir())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(types: &[(&str, DType)]) -> std::vec::IntoIter<io::Result<(Vec<u8>, DType)>> {
        let v: Vec<_> = types
            .iter()
            .map(|&(n, t)| Ok((n.as_bytes().to_vec(), t)))
            .collect();
        v.into_iter()
    }

    #[test]
    fn nofollow_is_decided_by_the_first_raw_entry() {
        // "." without d_type decides it, though the real entries have one.
        let mut it = listing(&[
            (".", DType::Unknown),
            ("..", DType::Dir),
            ("a", DType::Regular),
        ])
        .peekable();
        assert!(nofollow_for(&mut it, false));
        assert_eq!(it.next().unwrap().unwrap().0, b".");
        // "." with d_type: a later entry without one does not change it.
        let mut it =
            listing(&[(".", DType::Dir), ("..", DType::Dir), ("b", DType::Unknown)]).peekable();
        assert!(!nofollow_for(&mut it, false));
        // Disk usage always lstats.
        let mut it = listing(&[(".", DType::Dir)]).peekable();
        assert!(nofollow_for(&mut it, true));
        assert!(!nofollow_for(&mut listing(&[]).peekable(), false));
    }
}
