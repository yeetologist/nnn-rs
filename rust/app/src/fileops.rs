//! Copy, move and remove of the selection, batch rename, editing the selection list
//! (src/nnn.c: entries_in_file, rmtmpfile, editselection, spawn_sh_exec, opstr, rmmulstr,
//! cpmv_rename, cpmvrm_selection, batch_rename).
//!
//! The temp file `g_tmpfpath` names in the original is the path `create_tmp_file` returns
//! here (`self.g_tmpfpath` stays the temp directory, see popup.rs).

use crate::browse::{MESSAGES, MSG_FAILED};
use crate::config::{CMD_LEN_MAX, cstr};
use crate::keys::Action;
use crate::platform::path;
use crate::popup::{F_CHKRTN, F_CLI};
use crate::state::{NAME_MAX, Nnn, PATH_MAX};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;

// messages[] indexes this module uses.
const MSG_0_SELECTED: usize = 3;
const MSG_CANCEL: usize = 4;
const MSG_CP_MV_AS: usize = 7;
const MSG_INVALID_KEY: usize = 40;

/// `utils[UTIL_SH_EXEC]`
const UTIL_SH_EXEC: &[u8] = b"sh -c";
/// `SED` (Linux).
const SED: &str = "sed";
/// `cp`, `mv` and their `-r` (advcpmv) variants `PROGRESS_CP`, `PROGRESS_MV`.
const CP: &[u8] = b"cp -iRp --";
const MV: &[u8] = b"mv -i --";
const PROGRESS_CP: &[u8] = b"cpg -giRp --";
const PROGRESS_MV: &[u8] = b"mvg -gi --";

/// `snprintf(buf, CMD_LEN_MAX, ...)`: the text cut to what fits with its NUL.
fn cmdbuf(mut v: Vec<u8>) -> Vec<u8> {
    v.truncate(CMD_LEN_MAX - 1);
    v
}

/// `entries_in_file`: the count of `delim` in what is left to read of `f`, read
/// `buflen` bytes at a time; one more for NUL (the entries between the separators).
/// 0 on a read error.
pub fn entries_in_file(f: &mut impl Read, buflen: usize, delim: u8) -> u32 {
    let mut buf = vec![0u8; buflen.max(1)];
    let mut count: u32 = 0;
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(len) => {
                count += buf[..len].iter().filter(|&&c| c == delim).count() as u32;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // For all use cases 0 count is considered as error
            Err(_) => return 0,
        }
    }
    if delim != 0 { count } else { count + 1 }
}

/// `shell_escape` into a buffer of `buflen` bytes: `None` where the original fails.
fn shell_escape(inbuf: &[u8], buflen: usize) -> Option<Vec<u8>> {
    let inbuf = cstr(inbuf);
    if buflen < 3 {
        return None;
    }
    let n = inbuf.len();
    let mut buf = Vec::with_capacity(n + 2);
    for r in 0..n + 2 {
        if buf.len() + 5 >= buflen {
            return None;
        }
        if r == 0 || r == n + 1 {
            buf.push(b'\'');
        } else if inbuf[r - 1] == b'\'' {
            buf.extend_from_slice(b"'\\''");
        } else {
            buf.push(inbuf[r - 1]);
        }
    }
    Some(buf)
}

/// The selection list an editor left (the part of `editselection` after the read): the
/// last byte (the closing '\n') becomes the NUL, every '\n' before a '/' separates two
/// paths. Returns the buffer and the number of paths.
fn parse_edited_sel(mut buf: Vec<u8>) -> (Vec<u8>, i32) {
    let mut lines = 0;
    let Some(last) = buf.len().checked_sub(1) else {
        return (buf, 0);
    };
    // The last character should be '\n'
    buf[last] = 0;
    for count in (1..last).rev() {
        // Replace every '\n' that separates two paths
        if buf[count] == b'\n' && buf[count + 1] == b'/' {
            lines += 1;
            buf[count] = 0;
        }
    }
    // Add a line for the last file
    lines += 1;
    (buf, lines)
}

/// `opstr`: the command that runs `op` on the selection file's paths.
pub fn opstr(op: &[u8], selpath: &[u8]) -> Vec<u8> {
    let mut v = b"xargs -0 sh -c '".to_vec();
    v.extend_from_slice(op);
    v.extend_from_slice(b" \"$0\" \"$@\" . < /dev/tty' < '");
    v.extend_from_slice(cstr(selpath));
    v.push(b'\'');
    cmdbuf(v)
}

/// The `rmmulstr` command for a `confirm_force` answer `r`.
fn rmmulcmd(r: u8, trashcmd: Option<&[u8]>, selpath: &[u8]) -> Vec<u8> {
    let mut v;
    if let Some(t) = trashcmd {
        v = b"xargs -0 ".to_vec();
        v.extend_from_slice(cstr(t));
        v.extend_from_slice(b" < '");
    } else {
        v = b"xargs -0 sh -c 'rm -".to_vec();
        v.push(r);
        v.extend_from_slice(b"vr -- \"$0\" \"$@\" < /dev/tty' < '");
    }
    v.extend_from_slice(cstr(selpath));
    v.push(b'\'');
    cmdbuf(v)
}

/// `patterns[P_CPMVFMT]` with the temp file.
pub fn p_cpmvfmt(tmp: &[u8]) -> Vec<u8> {
    let mut v = format!("{SED} -i 's|^\\(\\(.*/\\)\\(.*\\)$\\)|#\\1\\n\\3|' '").into_bytes();
    v.extend_from_slice(tmp);
    v.push(b'\'');
    v
}

/// `patterns[P_CPMVRNM]` with the escaped target dir, the temp file and the command.
pub fn p_cpmvrnm(path_esc: &[u8], tmp: &[u8], cmd: &[u8]) -> Vec<u8> {
    let mut v = format!("{SED} 's|^\\([^#/][^/]\\?.*\\)$|").into_bytes();
    v.extend_from_slice(path_esc);
    v.extend_from_slice(b"/\\1|;s|^#\\(/.*\\)$|\\1|' '");
    v.extend_from_slice(tmp);
    v.extend_from_slice(b"' | tr '\\n' '\\0' | xargs -0 -n2 sh -c '");
    v.extend_from_slice(cmd);
    v.extend_from_slice(b" \"$0\" \"$@\" < /dev/tty'");
    v
}

/// `batch_rename`'s `batchrenamecmd` with the two temp files.
fn batchrenamecmd(foriginal: &[u8], tmp: &[u8]) -> Vec<u8> {
    let mut v = b"paste -d'\n' '".to_vec();
    v.extend_from_slice(foriginal);
    v.extend_from_slice(b"' '");
    v.extend_from_slice(tmp);
    v.extend_from_slice(b"' | ");
    v.extend_from_slice(SED.as_bytes());
    v.extend_from_slice(
        b" 'N; /^\\(.*\\)\\n\\1$/!p;d' | tr '\n' '\\0' | xargs -0 -n2 sh -c 'mv -i -- \"$0\" \"$@\" < /dev/tty'",
    );
    v
}

impl Nnn {
    /// `cp` (with `-r`, `PROGRESS_CP`).
    fn cpcmd(&self) -> &'static [u8] {
        if self.opts.progress_cpmv {
            PROGRESS_CP
        } else {
            CP
        }
    }

    /// `mv` (with `-r`, `PROGRESS_MV`).
    fn mvcmd(&self) -> &'static [u8] {
        if self.opts.progress_cpmv {
            PROGRESS_MV
        } else {
            MV
        }
    }

    /// The editor `cfg.waitedit` picks.
    fn sel_editor(&self) -> Vec<u8> {
        if self.cfg.waitedit {
            self.enveditor.clone()
        } else {
            self.editor.clone()
        }
    }

    /// `rmtmpfile`: remove the temp file, warn on failure.
    pub fn rmtmpfile(&mut self, tmp: &[u8]) {
        if std::fs::remove_file(path(tmp)).is_err() {
            self.printwarn(None);
        }
    }

    /// `spawn_sh_exec`
    pub fn spawn_sh_exec(&mut self, cmd: &[u8]) {
        self.spawn(UTIL_SH_EXEC, Some(cmd), None, None, F_CLI);
    }

    /// `editselection`: edit the selection list in the editor.
    /// Returns: 1 - success, 0 - none selected, -1 - other failure
    pub fn editselection(&mut self, allowemptysel: bool) -> i32 {
        let mut ret = -1;

        if !allowemptysel && self.pselbuf.is_empty() {
            // External selection is only editable at source
            return i32::from(self.listselfile());
        }

        let Some((mut f, tmp)) = self.create_tmp_file() else {
            return -1;
        };
        if !allowemptysel {
            self.seltofile(&mut f, b'\n');
        }
        drop(f);

        // Save the last modification time
        let mtime = match std::fs::metadata(path(&tmp)) {
            Ok(sb) => sb.mtime(),
            Err(_) => {
                self.rmtmpfile(&tmp);
                return -1;
            }
        };

        let editor = self.sel_editor();
        self.spawn(&editor, Some(&tmp), None, None, F_CLI);

        let mut f = match File::open(path(&tmp)) {
            Ok(f) => f,
            Err(_) => {
                self.rmtmpfile(&tmp);
                return -1;
            }
        };
        let (st_mtime, st_size) = f
            .metadata()
            .map_or((0, 0), |sb| (sb.mtime(), sb.size() as usize));

        'emptyedit: {
            if mtime == st_mtime {
                // selection is not modified
                if std::fs::remove_file(path(&tmp)).is_err() {
                    self.printwarn(None);
                    break 'emptyedit;
                }
                return 1;
            }

            if allowemptysel {
                self.selbufrealloc(st_size);
            } else if st_size > self.pselbuf.len() {
                // edited buffer larger than previous
                self.rmtmpfile(&tmp);
                break 'emptyedit;
            }

            // read(fd, pselbuf, selbuflen)
            let selbuflen = self.pselbuf.capacity().max(st_size);
            let mut buf = vec![0u8; selbuflen];
            let count = loop {
                match f.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    r => break r,
                }
            };
            let Ok(count) = count else {
                self.printwarn(None);
                drop(f);
                self.rmtmpfile(&tmp);
                break 'emptyedit;
            };
            buf.truncate(count);

            drop(f);
            if std::fs::remove_file(path(&tmp)).is_err() {
                self.printwarn(None);
                break 'emptyedit;
            }

            if count == 0 {
                ret = 1;
                break 'emptyedit;
            }

            self.resetselind();
            let (buf, lines) = parse_edited_sel(buf);
            self.pselbuf = buf;

            if !allowemptysel && lines > self.nselected {
                // files added to selection
                break 'emptyedit;
            }

            self.nselected = lines;
            self.writesel(self.pselbuf.len() - 1);

            return 1;
        }

        self.resetselind();
        self.clearselection();
        ret
    }

    /// `opstr` into `g_buf`.
    fn opstr(&self, op: &[u8]) -> Vec<u8> {
        opstr(op, self.selpath.as_deref().unwrap_or_default())
    }

    /// `rmmulstr`: confirm, then the remove (or trash) command for the selection;
    /// `None` when cancelled.
    fn rmmulstr(&mut self, use_trash: bool) -> Option<Vec<u8>> {
        let r = self.confirm_force(true, use_trash);
        if r == 0 {
            return None;
        }
        let trash = if use_trash {
            // use_trash implies trashcmd is set
            Some(self.trashcmd.clone().unwrap_or_default())
        } else {
            None
        };
        Some(rmmulcmd(
            r,
            trash.as_deref(),
            self.selpath.as_deref().unwrap_or_default(),
        ))
    }

    /// `cpmv_rename`: copy ('c') or move the selection into `path` under names edited
    /// in the editor.
    fn cpmv_rename(&mut self, choice: i32, dirpath: &[u8]) -> bool {
        let cmd = if choice == i32::from(b'c') {
            self.cpcmd()
        } else {
            self.mvcmd()
        };

        let Some((mut f, tmp)) = self.create_tmp_file() else {
            return false;
        };

        // selsafe() returned TRUE for this to be called
        let count = if self.pselbuf.is_empty() {
            let mut buf = b"tr '\\0' '\\n' < '".to_vec();
            buf.extend_from_slice(cstr(self.selpath.as_deref().unwrap_or_default()));
            buf.extend_from_slice(b"' > '");
            buf.extend_from_slice(&tmp);
            buf.push(b'\'');
            self.spawn_sh_exec(&buf);

            let mut n = entries_in_file(&mut f, PATH_MAX * 5, b'\n');
            // External selections may not end with a NUL, so tr leaves the last
            // path without a newline and entries_in_file() does not count it.
            if f.seek(SeekFrom::End(-1)).is_ok() {
                let mut c = [0u8; 1];
                if !matches!(f.read(&mut c), Ok(1)) {
                    return false;
                }
                if c[0] != b'\n' {
                    if !matches!(f.write(b"\n"), Ok(1)) {
                        return false;
                    }
                    n += 1;
                }
            }
            if n == 0 {
                return false;
            }
            n
        } else {
            self.seltofile(&mut f, b'\n').1
        };
        drop(f);

        self.spawn_sh_exec(&p_cpmvfmt(&tmp));

        let editor = self.sel_editor();
        self.spawn(&editor, Some(&tmp), None, None, F_CLI);

        let Ok(mut f) = File::open(path(&tmp)) else {
            return false;
        };

        let lines = entries_in_file(&mut f, PATH_MAX * 5, b'\n');
        if lines == 0 || 2 * count != lines {
            // num mismatch
            return false;
        }

        // Escape for insertion inside an existing single-quoted shell string.
        let Some(esc) = shell_escape(dirpath, (PATH_MAX << 2) + 3) else {
            return false;
        };
        if esc.len() < 2 {
            return false;
        }
        // Strip outer quotes from shell_escape(), keep interior quote breaks (e.g. '\'').
        let path_esc = &esc[1..esc.len() - 1];

        let buf = p_cpmvrnm(path_esc, &tmp, cmd);
        self.spawn(UTIL_SH_EXEC, Some(&buf), None, None, F_CLI | F_CHKRTN) == 0
    }

    /// `cpmvrm_selection`: copy, move, copy/move as, trash or remove the selection.
    pub fn cpmvrm_selection(&mut self, sel: Action, dirpath: &[u8]) -> bool {
        if (sel == Action::Cp || sel == Action::Mv) && self.isselfileempty() {
            self.editselection(true);
        }

        if self.isselfileempty() {
            if self.nselected != 0 {
                self.clearselection();
            }
            self.printmsg(MESSAGES[MSG_0_SELECTED]);
            return false;
        }

        if !self.selsafe() {
            return false;
        }

        let g_buf = match sel {
            Action::Cp => self.opstr(self.cpcmd()),
            Action::Mv => self.opstr(self.mvcmd()),
            Action::Cpmvas => {
                let r = self.get_input(Some(MESSAGES[MSG_CP_MV_AS]));
                if r != i32::from(b'c') && r != i32::from(b'm') {
                    self.printmsg(MESSAGES[MSG_INVALID_KEY]);
                    return false;
                }
                if !self.cpmv_rename(r, dirpath) {
                    self.printmsg(MESSAGES[MSG_FAILED]);
                    return false;
                }
                Vec::new()
            }
            // SEL_TRASH, SEL_RM_RF
            _ => match self.rmmulstr(self.trashcmd.is_some() && sel == Action::Trash) {
                Some(b) => b,
                None => {
                    self.printmsg(MESSAGES[MSG_CANCEL]);
                    return false;
                }
            },
        };

        if sel != Action::Cpmvas
            && self.spawn(UTIL_SH_EXEC, Some(&g_buf), None, None, F_CLI | F_CHKRTN) != 0
        {
            self.printmsg(MESSAGES[MSG_FAILED]);
            return false;
        }

        // Clear selection
        self.clearselection();
        true
    }

    /// `batch_rename`: rename the current dir's entries or the selection in the editor.
    pub fn batch_rename(&mut self) -> bool {
        let i = self.get_cur_or_sel();
        if i == 0 {
            return false;
        }

        let mut dir = false;
        if i == i32::from(b'c') {
            // Rename entries in current dir
            self.pselbuf.clear();
            dir = true;
        }

        let Some((mut f1, foriginal)) = self.create_tmp_file() else {
            return false;
        };
        let Some((mut f2, tmp)) = self.create_tmp_file() else {
            crate::sys::unlink(&foriginal);
            return false;
        };

        if dir {
            let n = usize::try_from(self.ndents).unwrap_or(0);
            let names: Vec<Vec<u8>> = self.pdents.iter().take(n).map(|e| e.name.clone()).collect();
            for name in names {
                self.appendfpath(&name, NAME_MAX);
            }
        }

        let count = self.seltofile(&mut f1, b'\n').1;
        self.seltofile(&mut f2, b'\n');
        drop(f2);

        if dir {
            // Don't retain dir entries in selection
            self.pselbuf.clear();
        }

        let editor = self.sel_editor();
        self.spawn(&editor, Some(&tmp), None, None, F_CLI);

        let mut ret = false;
        // Reopen file descriptor to get updated contents
        if let Ok(mut f2) = File::open(path(&tmp)) {
            let lines = entries_in_file(&mut f2, PATH_MAX * 5, b'\n');
            if lines != 0 && count == lines {
                self.spawn_sh_exec(&batchrenamecmd(&foriginal, &tmp));
                ret = true;
            }
        }

        drop(f1);
        crate::sys::unlink(&foriginal);
        crate::sys::unlink(&tmp);
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_entries() {
        assert_eq!(entries_in_file(&mut &b"a\nb\nc"[..], 2, b'\n'), 2);
        assert_eq!(entries_in_file(&mut &b"a\0b\0c"[..], 300, 0), 3);
        assert_eq!(entries_in_file(&mut &b""[..], 300, 0), 1);
        assert_eq!(entries_in_file(&mut &b""[..], 300, b'\n'), 0);
    }

    #[test]
    fn parses_edit() {
        let (b, n) = parse_edited_sel(b"/a\n/b c\nd\n/e\n".to_vec());
        assert_eq!(b, b"/a\0/b c\nd\0/e\0");
        assert_eq!(n, 3);
        assert_eq!(parse_edited_sel(b"x".to_vec()), (b"\0".to_vec(), 1));
    }

    #[test]
    fn commands() {
        assert_eq!(
            opstr(CP, b"/s"),
            b"xargs -0 sh -c 'cp -iRp -- \"$0\" \"$@\" . < /dev/tty' < '/s'"
        );
        assert_eq!(
            rmmulcmd(b'f', None, b"/s"),
            b"xargs -0 sh -c 'rm -fvr -- \"$0\" \"$@\" < /dev/tty' < '/s'"
        );
        assert_eq!(
            rmmulcmd(b'f', Some(b"gio trash"), b"/s"),
            b"xargs -0 gio trash < '/s'"
        );
        assert_eq!(shell_escape(b"a'b", 100).unwrap(), b"'a'\\''b'");
        assert!(batchrenamecmd(b"/o", b"/t").starts_with(b"paste -d'\n' '/o' '/t' | sed 'N;"));
    }
}
