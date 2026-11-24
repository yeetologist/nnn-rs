//! The selection buffer and its operations (src/nnn.c: writesel ... get_cur_or_sel).
//!
//! `pselbuf` holds the selected paths, each followed by a NUL; its length is the original's
//! `selbufpos` (`selbuflen`, the allocation, is the Vec's capacity). `findselpos` is an
//! offset into it. The original's scratch buffer `g_sel` ("<dir>/" then a name) is passed
//! explicitly: `scanselforpath` returns the "<dir>/" prefix that `addtoselbuf` and
//! `invertselbuf` take, and `findinsel`/`rmfromselbuf` take the bytes to look for.

use crate::config::{cstr, is_prefix, xstrsncpy};
use crate::platform::{self, path};
use crate::popup::{F_CLI, F_CONFIRM};
use crate::sort::{FILE_SCANNED, FILE_SELECTED};
use crate::state::{NAME_MAX, Nnn, PATH_MAX};
use crate::sys;
use std::cmp::Ordering;
use std::fs::OpenOptions;
use std::io::{self, Write};

const MSG_0_SELECTED: &[u8] = b"0 selected";
const MSG_CUR_SEL_OPTS: &[u8] = b"'c'urrent/'s'el?";
const MSG_SEL_MISSING: &[u8] = b"open failed";
const MSG_RM_TMP: &[u8] = b"remove tmp file?";

/// One write(2), true when all of `buf` went out (as `write(fd, buf, n) == n`).
fn write_once(f: &mut dyn Write, buf: &[u8]) -> bool {
    matches!(f.write(buf), Ok(n) if n == buf.len())
}

/// `memmem`.
fn memmem(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `selmark`: a run of the selection buffer to drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelMark {
    pub startpos: usize,
    pub len: usize,
}

/// `markcmp`: order marks by where they start in the buffer.
pub fn markcmp(ma: &SelMark, mb: &SelMark) -> Ordering {
    ma.startpos.cmp(&mb.startpos)
}

/// `g_sel + pathlen` filled with `name` as `xstrsncpy(pbuf, name, n)` does: the prefix, the
/// name cut to `n - 1` bytes, a NUL.
fn with_name(prefix: &[u8], name: &[u8], n: usize) -> Vec<u8> {
    let mut buf = prefix.to_vec();
    let mut tail = vec![0u8; n];
    let w = xstrsncpy(&mut tail, name, n);
    buf.extend_from_slice(&tail[..w]);
    buf
}

impl Nnn {
    /// `isselfileempty`.
    pub fn isselfileempty(&self) -> bool {
        self.selpath
            .as_ref()
            .and_then(|p| std::fs::metadata(path(p)).ok())
            .is_none_or(|m| m.len() == 0)
    }

    /// `get_cur_or_sel`: 'c' (current file), 's' (selection) or 0.
    pub fn get_cur_or_sel(&mut self) -> i32 {
        let sel = !self.pselbuf.is_empty() || !self.isselfileempty();

        // Check both local buffer and selection file for external selection
        if sel && self.ndents != 0 {
            // If selection is preferred and we have a local selection, return selection.
            // Always show the prompt in case of an external selection.
            if self.cfg.prefersel && !self.pselbuf.is_empty() {
                return i32::from(b's');
            }
            let choice = self.get_input(Some(MSG_CUR_SEL_OPTS));
            return if choice == i32::from(b'c') || choice == i32::from(b's') {
                choice
            } else {
                0
            };
        }
        if sel {
            return i32::from(b's');
        }
        if self.ndents != 0 {
            return i32::from(b'c');
        }
        0
    }

    /// `writesel`: the first `buflen` bytes of the selection buffer to the selection file
    /// (`writesel(NULL, 0)` is `writesel(0)`). Callers pass `selbufpos - 1` as
    /// `pselbuf.len().wrapping_sub(1)`: past the buffer, write(2) fails with EFAULT as the
    /// original's does.
    pub fn writesel(&mut self, buflen: usize) {
        let Some(selpath) = self.selpath.clone() else {
            return;
        };
        let f = platform::private_mode(OpenOptions::new().write(true).create(true).truncate(true))
            .open(path(&selpath));
        let err = match f {
            Ok(mut f) => match self.pselbuf.get(..buflen) {
                None => Some(platform::error(platform::Errno::Fault)),
                Some(buf) => match f.write(buf) {
                    Ok(n) if n == buflen => None,
                    Ok(_) => Some(io::Error::last_os_error()),
                    Err(e) => Some(e),
                },
            },
            Err(e) => Some(e),
        };
        if let Some(e) = err {
            self.printwait(sys::strerror(&e).as_bytes(), None);
        }
    }

    /// `writesel(pselbuf, selbufpos - 1)`: the buffer without its last NUL.
    fn writeselbuf(&mut self) {
        self.writesel(self.pselbuf.len().wrapping_sub(1));
    }

    /// `appendfpath`: append `path` (cut to `len - 1` bytes) and a NUL.
    pub fn appendfpath(&mut self, p: &[u8], len: usize) {
        let p = cstr(p);
        if len == 0 {
            return;
        }
        let n = p.len().min(len - 1);
        self.pselbuf.extend_from_slice(&p[..n]);
        self.pselbuf.push(0);
    }

    /// `selbufrealloc`: room for `alloclen` more bytes.
    pub fn selbufrealloc(&mut self, alloclen: usize) {
        self.pselbuf.reserve(alloclen);
    }

    /// `seltofile`: the selected paths to `f`, separated by `separator`, list mode paths
    /// mapped back to the list root. Returns the bytes of the buffer written (as positions)
    /// and the count of paths (`*pcount`, 0 when a write fails).
    pub fn seltofile(&self, f: &mut dyn Write, separator: u8) -> (usize, u32) {
        let buf = &self.pselbuf;
        if buf.is_empty() {
            return (0, 0);
        }
        let lastpos = buf.len() - 1;
        let (listroot, listpath) = match &self.listpath {
            Some(lp) => (
                cstr(self.listroot.as_deref().unwrap_or_default()),
                Some(cstr(lp)),
            ),
            None => (&b""[..], None),
        };
        let initlen = listpath.map_or(0, <[u8]>::len);
        let (mut pos, mut pbuf, mut count) = (0, 0, 0);

        while pos <= lastpos {
            let entry = cstr(&buf[pbuf..]);
            let len = entry.len();
            match listpath {
                Some(lp) if is_prefix(entry, lp, initlen) => {
                    if !write_once(f, listroot) || !write_once(f, &entry[initlen..]) {
                        return (pos, 0);
                    }
                }
                _ => {
                    if !write_once(f, entry) {
                        return (pos, 0);
                    }
                }
            }
            pos += len;
            if pos <= lastpos {
                if !write_once(f, &[separator]) {
                    return (pos, 0);
                }
                pbuf += len + 1;
            }
            pos += 1;
            count += 1;
        }
        (pos, count)
    }

    /// `listselfile`: show the selection file of another instance.
    pub fn listselfile(&mut self) -> bool {
        if self.isselfileempty() {
            return false;
        }
        let mut cmd = b"tr '\\0' '\\n' < '".to_vec();
        cmd.extend_from_slice(cstr(self.selpath.as_deref().unwrap_or_default()));
        cmd.push(b'\'');
        cmd.truncate(crate::config::CMD_LEN_MAX - 1);
        self.spawn(
            crate::platform::shell_exec_prefix(),
            Some(&cmd),
            None,
            None,
            F_CLI | F_CONFIRM,
        );
        true
    }

    /// `resetselind`: clear the selection marks of the listed entries.
    pub fn resetselind(&mut self) {
        let n = usize::try_from(self.ndents).unwrap_or(0);
        for e in self.pdents.iter_mut().take(n) {
            e.flags &= !FILE_SELECTED;
        }
    }

    /// `startselection`.
    pub fn startselection(&mut self) {
        if !self.g_state.selmode {
            self.g_state.selmode = true;
            self.nselected = 0;
            if !self.pselbuf.is_empty() {
                self.resetselind();
                self.writesel(0);
                self.pselbuf.clear();
            }
        }
    }

    /// `clearselection`.
    pub fn clearselection(&mut self) {
        self.nselected = 0;
        self.pselbuf.clear();
        self.g_state.selmode = false;
        self.writesel(0);
    }

    /// `findinsel`: where `sel` (the original's `g_sel`, `len` bytes) starts a path in the
    /// buffer at or after `startpos` (the start for `None`).
    pub fn findinsel(&self, startpos: Option<usize>, sel: &[u8]) -> Option<usize> {
        if self.pselbuf.is_empty() {
            return None;
        }
        let start = startpos.unwrap_or(0);
        let end = self.pselbuf.len();
        let mut found = start;
        loop {
            found += memmem(self.pselbuf.get(found..end)?, sel)?;
            if found == start || self.pselbuf[found - 1] == 0 {
                return Some(found);
            }
            // We found g_sel as a substring of a path, move forward
            found += sel.len();
            if found >= end {
                return None;
            }
        }
    }

    /// `findmarkentry`: mark entry `i` selected when "<prefix><name>" is in the buffer.
    /// scanselforpath() must be called before calling this.
    pub fn findmarkentry(&mut self, prefix: &[u8], i: usize) {
        if self.pdents[i].flags & FILE_SCANNED == 0 {
            let name = &self.pdents[i].name;
            let sel = with_name(prefix, name, name.len() + 1);
            if self.findinsel(self.findselpos, &sel).is_some() {
                self.pdents[i].flags |= FILE_SELECTED;
            }
            self.pdents[i].flags |= FILE_SCANNED;
        }
    }

    /// `invertselbuf`: invert the selection of the listed entries; `prefix` is "<dir>/"
    /// from scanselforpath() (its length is the original's `pathlen`).
    pub fn invertselbuf(&mut self, prefix: &[u8]) {
        let pathlen = prefix.len();
        let (mut shrinklen, mut alloclen) = (0, 0);
        let mut marked: Vec<SelMark> = Vec::new();
        let ndents = usize::try_from(self.ndents).unwrap_or(0);

        // First pass: inversion
        for i in 0..ndents {
            // Ensure off-screen entries are reconciled with selbuf before inversion.
            self.findmarkentry(prefix, i);

            let nlen = self.pdents[i].name.len() + 1;
            if self.pdents[i].flags & FILE_SELECTED != 0 {
                self.pdents[i].flags ^= FILE_SELECTED; // Clear selection status
                let sel = with_name(prefix, &self.pdents[i].name, NAME_MAX);
                let len = sel.len();
                if let Some(found) = self.findinsel(self.findselpos, &sel) {
                    if self.findselpos == Some(found) {
                        self.findselpos = Some(found + len);
                    }
                    match marked.last_mut() {
                        Some(last) if found == last.startpos + last.len => last.len += len,
                        _ => marked.push(SelMark {
                            startpos: found,
                            len,
                        }),
                    }
                    self.nselected -= 1;
                    shrinklen += len; // buffer size adjustment
                } else {
                    self.pdents[i].flags |= FILE_SELECTED;
                    alloclen += pathlen + nlen;
                }
            } else {
                self.pdents[i].flags |= FILE_SELECTED;
                alloclen += pathlen + nlen;
            }
        }

        // Files marked for deselection could be found in arbitrary order.
        // Sort by appearance in selection buffer.
        marked.sort_by(markcmp);

        // Some files might be adjacent. Merge them into a single entry
        let mut prev = 0;
        for i in 1..marked.len() {
            if marked[i].startpos == marked[prev].startpos + marked[prev].len {
                marked[prev].len += marked[i].len;
            } else {
                prev += 1;
                marked[prev] = marked[i];
            }
        }
        let nmarked = if marked.is_empty() { 0 } else { prev + 1 };

        // Using merged entries remove unselected chunks from selection buffer. As in the
        // original, each run is moved back by its own length only.
        let selbufpos = self.pselbuf.len();
        for i in 0..nmarked {
            let found = marked[i].startpos;
            let endpos = if i + 1 == nmarked {
                selbufpos
            } else {
                marked[i + 1].startpos
            };
            let len = marked[i].len;
            // Move back only selected entries. No selected memory is moved twice
            self.pselbuf.copy_within(found + len..endpos, found);
        }

        // Buffer size adjustment
        self.pselbuf.truncate(selbufpos - shrinklen);
        self.selbufrealloc(alloclen);

        // Second pass: append newly selected to buffer
        for i in 0..ndents {
            if self.pdents[i].flags & FILE_SELECTED != 0 {
                let sel = with_name(prefix, &self.pdents[i].name, NAME_MAX);
                self.appendfpath(&sel, sel.len());
                self.nselected += 1;
            }
        }

        if self.nselected != 0 {
            self.writeselbuf();
        } else {
            self.clearselection();
        }
    }

    /// `addtoselbuf`: select entries `startid..=endid`; `prefix` is "<dir>/" from
    /// scanselforpath().
    pub fn addtoselbuf(&mut self, prefix: &[u8], startid: i32, endid: i32) {
        let pathlen = prefix.len();
        let mut alloclen = 0;
        let range = || {
            usize::try_from(startid).unwrap_or(0)
                ..usize::try_from(endid.saturating_add(1)).unwrap_or(0)
        };

        // Remember current selection buffer position
        for i in range() {
            let nlen = self.pdents[i].name.len() + 1;
            if let Some(fsp) = self.findselpos {
                let sel = with_name(prefix, &self.pdents[i].name, NAME_MAX);
                let len = sel.len();
                if let Some(found) = self.findinsel(Some(fsp), &sel) {
                    self.pdents[i].flags |= FILE_SCANNED | FILE_SELECTED;
                    if found == fsp {
                        let next = fsp + len;
                        self.findselpos = (next != self.pselbuf.len()).then_some(next);
                    }
                } else {
                    alloclen += pathlen + nlen;
                }
            } else {
                alloclen += pathlen + nlen;
            }
        }

        self.selbufrealloc(alloclen);

        for i in range() {
            if self.pdents[i].flags & FILE_SELECTED == 0 {
                let sel = with_name(prefix, &self.pdents[i].name, NAME_MAX);
                self.appendfpath(&sel, sel.len());
                self.nselected += 1;
                self.pdents[i].flags |= FILE_SCANNED | FILE_SELECTED;
            }
        }

        self.writeselbuf(); // Truncate NULL from end
    }

    /// `rmfromselbuf`: remove `sel` ("<path>" and its NUL, the original's `g_sel` and `len`).
    pub fn rmfromselbuf(&mut self, sel: &[u8]) {
        let Some(found) = self.findinsel(self.findselpos, sel) else {
            return;
        };
        let len = sel.len();
        let end = self.pselbuf.len();
        self.pselbuf.copy_within(found + len..end, found);
        self.pselbuf.truncate(end - len);

        if self.nselected != 0 {
            self.writeselbuf();
        } else {
            self.clearselection();
        }
    }

    /// `scanselforpath`: "<path>/" (the original's `g_sel`) and its length; sets
    /// `findselpos` to where paths under `path` start in the buffer. Without `getsize` the
    /// length is 0 when none is selected.
    pub fn scanselforpath(&mut self, p: &[u8], getsize: bool) -> (i32, Vec<u8>) {
        let p = cstr(p);
        if p.len() < 2 {
            // path should always be at least two bytes (including NULL)
            self.findselpos = Some(0);
            return (1, b"/".to_vec()); // Length of '/' is 1
        }
        let mut g_sel = vec![0u8; PATH_MAX];
        let off = xstrsncpy(&mut g_sel, p, PATH_MAX);
        g_sel[off - 1] = b'/';
        g_sel.truncate(off);
        // We set findselpos only here. Directories can be listed in arbitrary order.
        // This is the best best we can do for remembering position.
        self.findselpos = self.findinsel(None, &g_sel);

        // INFALLIBLE: off is at most PATH_MAX.
        let off_i = i32::try_from(off).unwrap_or(i32::MAX);
        if getsize || self.findselpos.is_some() {
            (off_i, g_sel)
        } else {
            (0, g_sel)
        }
    }

    /// `endselection`: leave selection mode; in list mode, map the selected paths back to
    /// the list root.
    pub fn endselection(&mut self, endselmode: bool) {
        if endselmode && self.g_state.selmode {
            self.g_state.selmode = false;
        }

        // The code below is only for listing mode
        let Some(listpath) = self.listpath.clone() else {
            return;
        };
        if self.pselbuf.is_empty() {
            return;
        }
        let listpath = cstr(&listpath).to_vec();
        let listroot = cstr(self.listroot.as_deref().unwrap_or_default()).to_vec();
        let (prefixlen, initlen) = (listroot.len(), listpath.len());
        let inlist = |e: &[u8]| {
            is_prefix(e, &listpath, initlen) && matches!(e.get(initlen), None | Some(b'/'))
        };

        // Size the translated selection before replacing any paths.
        let end = self.pselbuf.len();
        let mut alloclen: usize = 0;
        let mut pbuf = 0;
        while pbuf < end {
            let e = cstr(&self.pselbuf[pbuf..]);
            let mut len = e.len() + 1;
            if inlist(e) {
                len = len - initlen + prefixlen;
            }
            if len > u32::MAX as usize - alloclen {
                self.printerr(2184);
            }
            alloclen += len;
            pbuf += e.len() + 1;
        }
        if alloclen == 0 {
            return;
        }

        // Keep filesystem names literal and preserve the NUL separators.
        let mut buf = Vec::with_capacity(alloclen);
        let mut pbuf = 0;
        while pbuf < end {
            let e = cstr(&self.pselbuf[pbuf..]);
            if inlist(e) {
                buf.extend_from_slice(&listroot);
                buf.extend_from_slice(&e[initlen..]);
            } else {
                buf.extend_from_slice(e);
            }
            buf.push(0);
            pbuf += e.len() + 1;
        }

        self.pselbuf = buf;
        self.findselpos = None;
        self.writeselbuf();
    }

    /// `selsafe`: the selection file path is set and readable and writable.
    pub fn selsafe(&mut self) -> bool {
        // Fail if selection file path not generated
        let Some(selpath) = self.selpath.clone() else {
            self.printmsg(MSG_SEL_MISSING);
            return false;
        };

        // Fail if selection file path isn't accessible
        if let Err(e) = platform::access(&selpath, platform::Access::ReadWrite) {
            if platform::is_errno(&e, platform::Errno::NoEnt) {
                self.printmsg(MSG_0_SELECTED);
            } else {
                self.printwait(sys::strerror(&e).as_bytes(), None);
            }
            return false;
        }
        true
    }

    /// `export_file_list`: the listed names to a temporary file opened in the editor.
    pub fn export_file_list(&mut self) {
        if self.ndents == 0 {
            return;
        }
        let Some((mut f, tmp)) = self.create_tmp_file() else {
            return;
        };
        let n = usize::try_from(self.ndents).unwrap_or(0);
        for (r, e) in self.pdents.iter().take(n).enumerate() {
            if !write_once(&mut f, &e.name) {
                break;
            }
            if r != n - 1 && !write_once(&mut f, b"\n") {
                break;
            }
        }
        drop(f);

        let editor = self.editor.clone();
        self.spawn(&editor, Some(&tmp), None, None, F_CLI);

        let c = self.get_input(Some(MSG_RM_TMP));
        if c == i32::from(b'y') || c == i32::from(b'Y') {
            sys::unlink(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::Entry;

    fn nnn(names: &[&[u8]], sel: &[u8]) -> Nnn {
        Nnn {
            pdents: names
                .iter()
                .map(|&name| Entry {
                    name: name.to_vec(),
                    ..Entry::default()
                })
                .collect(),
            ndents: i32::try_from(names.len()).unwrap(),
            pselbuf: sel.to_vec(),
            ..Nnn::default()
        }
    }

    #[test]
    fn find_and_scan() {
        let mut n = nnn(&[], b"/a/bc\0/a/b\0/x\0");
        assert_eq!(n.findinsel(None, b"/a/b\0"), Some(6));
        assert_eq!(n.findinsel(None, b"b\0"), None);
        assert_eq!(n.scanselforpath(b"/a", false), (3, b"/a/".to_vec()));
        assert_eq!(n.findselpos, Some(0));
        assert_eq!(n.scanselforpath(b"/q", false).0, 0);
        assert_eq!(n.scanselforpath(b"/q", true).0, 3);
        assert_eq!(n.scanselforpath(b"/", false), (1, b"/".to_vec()));
    }

    #[test]
    fn add_rm_invert() {
        let mut n = nnn(&[b"a", b"b", b"c"], b"");
        let (_, pre) = n.scanselforpath(b"/d", true);
        n.addtoselbuf(&pre, 0, 1);
        assert_eq!(n.pselbuf, b"/d/a\0/d/b\0");
        assert_eq!(n.nselected, 2);
        n.rmfromselbuf(b"/d/a\0");
        assert_eq!(n.pselbuf, b"/d/b\0");
        n.nselected = 1;
        for e in &mut n.pdents {
            e.flags = 0;
        }
        let (_, pre) = n.scanselforpath(b"/d", true);
        n.invertselbuf(&pre);
        assert_eq!(n.pselbuf, b"/d/a\0/d/c\0");
        assert_eq!(n.nselected, 2);
    }

    #[test]
    fn to_file_and_append() {
        let mut n = nnn(&[], b"");
        n.appendfpath(b"/x/y", 5);
        n.appendfpath(b"/zz", 3);
        assert_eq!(n.pselbuf, b"/x/y\0/z\0");
        let mut out = Vec::new();
        assert_eq!(n.seltofile(&mut out, b'\n'), (8, 2));
        assert_eq!(out, b"/x/y\n/z\n");
        n.listpath = Some(b"/tmp/l".to_vec());
        n.listroot = Some(b"/r".to_vec());
        n.pselbuf = b"/tmp/l/a\0/b\0".to_vec();
        out.clear();
        assert_eq!(n.seltofile(&mut out, 0), (12, 2));
        assert_eq!(out, b"/r/a\0/b\0");
        n.endselection(false);
        assert_eq!(n.pselbuf, b"/r/a\0/b\0");
    }
}
