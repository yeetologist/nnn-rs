//! Archives (create, list, extract, mount) and remote mounts (src/nnn.c: get_archive_cmd,
//! sel_paths_in_dir, archive_selection, handle_archive, archive_mount, remote_mount, unmount).

use std::ffi::OsStr;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::browse::{MESSAGES, MSG_FAILED, xdiraccess};
use crate::config::{
    TOK_MNT, TOKS, cstr, is_prefix, is_suffix, mkpath, xbasename, xdirname, xmktree_io,
};
use crate::popup::{F_CLI, F_CONFIRM, F_MULTI, F_NORMAL, F_NOTRACE, F_NOWAIT, xgetenv};
use crate::prompt::{MSGWAIT, XDELAY_INTERVAL_MS, xconfirm};
use crate::state::{Nnn, PATH_MAX};
use crate::sys;

// messages[] indexes this module uses.
const MSG_CANCEL: usize = 4;
const MSG_HOSTNAME: usize = 16;
const MSG_NEW_PATH: usize = 19;
const MSG_REMOTE_OPTS: usize = 30;
const MSG_RCLONE_DELAY: usize = 31;
const MSG_INVALID_KEY: usize = 40;
const MSG_LAZY: usize = 37;
const MSG_ARCHIVE_RLTV: usize = 44;

// utils[] entries this module uses.
const UTIL_ATOOL: &[u8] = b"atool";
const UTIL_BSDTAR: &[u8] = b"bsdtar";
const UTIL_UNZIP: &[u8] = b"unzip";
const UTIL_TAR: &[u8] = b"tar";
const UTIL_SSHFS: &[u8] = b"sshfs";
const UTIL_RCLONE: &[u8] = b"rclone";
const UTIL_ARCHMNT: &[u8] = b"archivemount";
const UTIL_SH_EXEC: &[u8] = b"sh -c";

/// `archive_cmd[]`
const ARCHIVE_CMD: [&[u8]; 4] = [b"atool -a", b"bsdtar -acvf", b"zip -r", b"tar -acvf"];

/// The static `found` of `unmount`: "fusermount3" was not found, "fusermount" is used.
static FUSERMOUNT_OLD: AtomicBool = AtomicBool::new(false);

fn path(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

/// `shell_escape` with a dynamic buffer (it cannot fail but for memory): `'...'` with each
/// `'` written as `'\\''`.
fn shell_escape(inbuf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(inbuf.len() + 2);
    out.push(b'\'');
    for &c in cstr(inbuf) {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
    out
}

/// `sel_paths_in_dir`: every selected path (NUL-separated in `pselbuf`) is inside `dir`.
pub fn sel_paths_in_dir(pselbuf: &[u8], dir: &[u8]) -> bool {
    let dir = cstr(dir);
    let dirlen = dir.len();
    let isroot = dirlen == 1; // dir is "/"

    if pselbuf.is_empty() {
        return false;
    }

    let mut pos = 0;
    while pos < pselbuf.len() {
        let psel = cstr(&pselbuf[pos..]);
        let len = psel.len();
        if len <= dirlen || !is_prefix(psel, dir, dirlen) || (!isroot && psel[dirlen] != b'/') {
            return false;
        }
        pos += len + 1;
    }
    true
}

/// The name `archive_mount` mounts at: `name` cut at its last '.' past the first byte.
fn archive_mount_dir(name: &[u8]) -> &[u8] {
    match name.iter().skip(1).rposition(|&c| c == b'.') {
        Some(at) => &name[..at + 1],
        None => name,
    }
}

/// The directory `handle_archive` suggests for extracting `fpath`: its base name cut at
/// the first '.' unless the name starts with it.
fn extract_dir_suggestion(fpath: &[u8]) -> Vec<u8> {
    let name = xbasename(fpath);
    match name.iter().position(|&c| c == b'.') {
        Some(at) if at != 0 => name[..at].to_vec(),
        _ => name.to_vec(),
    }
}

impl Nnn {
    /// `printwarn(presel)` for an error Rust reports instead of leaving it in errno.
    fn printwarn_err(&mut self, e: &io::Error, presel: Option<&mut i32>) {
        let s = sys::strerror(e);
        self.printwait(s.as_bytes(), presel);
    }

    /// `mkpath(cfgpath, toks[TOK_MNT], mntpath)`
    fn mntpath(&self) -> Vec<u8> {
        mkpath(&self.cfgpath, TOKS[TOK_MNT], &self.home).0
    }

    /// `get_archive_cmd`: the command that creates `archive`.
    pub fn get_archive_cmd(&mut self, archive: &[u8]) -> Vec<u8> {
        let i = if !self.g_state.usebsdtar && self.getutil(UTIL_ATOOL) {
            0
        } else if self.getutil(UTIL_BSDTAR) {
            1
        } else if is_suffix(archive, b".zip") {
            2
        } else {
            3 // tar
        };
        ARCHIVE_CMD[i].to_vec()
    }

    /// `archive_selection`: archive the selection into `archive` with `cmd`, with paths
    /// relative to `path` if they all are in it and the user agrees.
    pub fn archive_selection(&mut self, cmd: &[u8], archive: &[u8], path: &[u8]) {
        let relative = sel_paths_in_dir(&self.pselbuf, path)
            && xconfirm(self.get_input(Some(MESSAGES[MSG_ARCHIVE_RLTV])));
        let mut tmpfile: Option<Vec<u8>> = None;

        if relative {
            let path = cstr(path);
            // Skip the trailing / too.
            let dirlen = path.len() + usize::from(path != b"/");
            let Some((mut f, tmp)) = self.create_tmp_file() else {
                self.printwarn(None);
                return;
            };

            // The selected paths without the directory, NUL-separated, no NUL at the end.
            let buf = self.pselbuf.clone();
            let mut entries: Vec<&[u8]> = buf.split(|&c| c == 0).collect();
            if buf.last() == Some(&0) {
                entries.pop();
            }
            let mut out: Vec<u8> = Vec::new();
            for (i, e) in entries.iter().enumerate() {
                if i != 0 {
                    out.push(0);
                }
                out.extend_from_slice(e.get(dirlen..).unwrap_or_default());
            }
            if let Err(e) = f.write_all(&out) {
                self.printwarn_err(&e, None);
                drop(f);
                sys::unlink(&tmp);
                return;
            }
            drop(f);
            tmpfile = Some(tmp);
        }

        let archive_esc = shell_escape(archive);
        let selpath = match &tmpfile {
            Some(t) => t.clone(),
            None => self.selpath.clone().unwrap_or_default(),
        };
        let selpath_esc = shell_escape(&selpath);

        let mut buf: Vec<u8> = Vec::new();
        if relative {
            let path_esc = shell_escape(path);
            buf.extend_from_slice(b"cd ");
            buf.extend_from_slice(&path_esc);
            buf.extend_from_slice(b" && xargs -0 ");
        } else {
            // patterns[P_ARCHIVE_CMD]: "xargs -0 %s %s < %s"
            buf.extend_from_slice(b"xargs -0 ");
        }
        buf.extend_from_slice(cstr(cmd));
        buf.push(b' ');
        buf.extend_from_slice(&archive_esc);
        buf.extend_from_slice(b" < ");
        buf.extend_from_slice(&selpath_esc);

        self.spawn(UTIL_SH_EXEC, Some(&buf), None, None, F_CLI | F_CONFIRM);

        if let Some(t) = tmpfile {
            sys::unlink(&t);
        }
    }

    /// `handle_archive`: list (`op` 'l') or extract ('x') the archive `fpath`. After an
    /// extraction `fpath` is the directory extracted to, or empty for the current one.
    pub fn handle_archive(&mut self, fpath: &mut Vec<u8>, op: u8) -> bool {
        let mut arg: Vec<u8> = b"-tvf".to_vec(); // options for tar/bsdtar to list files
        let mut outdir: Vec<u8> = Vec::new();
        let mut x_to = false;
        let is_atool = !self.g_state.usebsdtar && self.getutil(UTIL_ATOOL);

        if op == b'x' {
            let name = extract_dir_suggestion(fpath);
            let prefill: &[u8] = if is_atool { b"." } else { &name };
            outdir = self.xreadline(Some(prefill), MESSAGES[MSG_NEW_PATH]);

            if outdir.is_empty() {
                // Cancelled
                self.printwait(MESSAGES[MSG_CANCEL], None);
                return false;
            }
            // Do not create smart context for current dir
            if outdir != b"." {
                let r = xmktree_io(&outdir, true, true)
                    .and_then(|()| std::env::set_current_dir(path(&outdir)));
                if let Err(e) = r {
                    self.printwarn_err(&e, None);
                    return false;
                }
                // Copy the new dir path to open it in smart context
                match std::env::current_dir() {
                    Ok(d) => outdir = d.as_os_str().as_bytes().to_vec(),
                    Err(_) => return false,
                }
                x_to = true;
            }
        }

        let util: &[u8] = if is_atool {
            arg = vec![b'-', op];
            UTIL_ATOOL
        } else if self.getutil(UTIL_BSDTAR) {
            if op == b'x' {
                arg[1] = op;
            }
            UTIL_BSDTAR
        } else if is_suffix(fpath, b".zip") {
            // 'v': verbose listing
            arg = if op == b'l' {
                b"-v".to_vec()
            } else {
                b"-".to_vec()
            };
            UTIL_UNZIP
        } else {
            if op == b'x' {
                arg[1] = op;
            }
            UTIL_TAR
        };

        let fp = cstr(fpath).to_vec();
        if op == b'x' {
            // extract
            self.spawn(util, Some(&arg), Some(&fp), None, F_NORMAL | F_MULTI);
        } else {
            // list
            self.get_output(util, Some(&arg), Some(&fp), None, true);
        }

        if x_to {
            if let Err(e) = std::env::set_current_dir(path(&xdirname(&fp))) {
                self.printwarn_err(&e, None);
                return false;
            }
            outdir.truncate(PATH_MAX - 1);
            *fpath = outdir;
        } else if op == b'x' {
            fpath.clear();
        }

        true
    }

    /// `archive_mount`: mount the hovered archive under `<cfgpath>/mounts`; `newpath` is
    /// the mount point.
    pub fn archive_mount(&mut self, newpath: &mut Vec<u8>) -> bool {
        let cmd = xgetenv("NNN_ARCHMNT", Some(UTIL_ARCHMNT)).unwrap_or_default();
        let name = self
            .pdents
            .get(self.cur as usize)
            .map(|e| cstr(&e.name).to_vec())
            .unwrap_or_default();

        if !self.getutil(&cmd) {
            self.printmsg(b"install utility (archivemount)");
            return false;
        }

        let dir = archive_mount_dir(&name);

        // Create the mount point
        let mntpath = self.mntpath();
        *newpath = mkpath(&mntpath, dir, &self.home).0;

        if let Err(e) = xmktree_io(newpath, true, true) {
            self.printwarn_err(&e, None);
            return false;
        }

        // Mount archive
        let np = newpath.clone();
        if self.spawn(&cmd, Some(&name), Some(&np), None, F_NORMAL) != 0 {
            self.printmsg(MESSAGES[MSG_FAILED]);
            return false;
        }

        true
    }

    /// `remote_mount`: mount a remote with sshfs or rclone under `<cfgpath>/mounts`;
    /// `newpath` is the mount point.
    pub fn remote_mount(&mut self, newpath: &mut Vec<u8>) -> bool {
        let mut flag = F_CLI;
        let r = self.getutil(UTIL_RCLONE);
        let s = self.getutil(UTIL_SSHFS);

        if !(r || s) {
            self.printmsg(b"install sshfs/rclone");
            return false;
        }

        let opt = if r && s {
            self.get_input(Some(MESSAGES[MSG_REMOTE_OPTS]))
        } else if !s {
            i32::from(b'r')
        } else {
            i32::from(b's')
        };

        let env = if opt == i32::from(b's') {
            xgetenv("NNN_SSHFS", Some(UTIL_SSHFS)).unwrap_or_default()
        } else if opt == i32::from(b'r') {
            flag |= F_NOWAIT | F_NOTRACE;
            xgetenv("NNN_RCLONE", Some(b"rclone mount")).unwrap_or_default()
        } else {
            self.printmsg(MESSAGES[MSG_INVALID_KEY]);
            return false;
        };

        let mut tmp = self.xreadline(None, b"host[:dir] > ");
        if tmp.is_empty() {
            self.printmsg(MESSAGES[MSG_CANCEL]);
            return false;
        }

        let div = tmp.iter().position(|&c| c == b':');
        let host = &tmp[..div.unwrap_or(tmp.len())];

        // Create the mount point
        let mntpath = self.mntpath();
        *newpath = mkpath(&mntpath, host, &self.home).0;
        if let Err(e) = xmktree_io(newpath, true, true) {
            self.printwarn_err(&e, None);
            return false;
        }

        if div.is_none() {
            // Convert "host" to "host:"
            tmp.push(b':');
        }

        // Connect to remote
        let np = newpath.clone();
        if opt == i32::from(b's') {
            if self.spawn(&env, Some(&tmp), Some(&np), None, flag) != 0 {
                self.printmsg(MESSAGES[MSG_FAILED]);
                return false;
            }
        } else {
            self.spawn(&env, Some(&tmp), Some(&np), None, flag);
            self.printmsg(MESSAGES[MSG_RCLONE_DELAY]);
            self.xdelay(XDELAY_INTERVAL_MS << 2); // Set 4 times the usual delay
        }

        true
    }

    /// `unmount`: unmount the hovered directory if it is a mount point, else ask for the
    /// host. Returns true if the directory needs to be refreshed.
    pub fn unmount(
        &mut self,
        name: Option<&[u8]>,
        newpath: &mut Vec<u8>,
        presel: &mut i32,
        currentpath: &[u8],
    ) -> bool {
        // On Ubuntu it's fusermount; the original's check runs until it falls back.
        if !FUSERMOUNT_OLD.load(Ordering::Relaxed) && !self.getutil(b"fusermount3") {
            FUSERMOUNT_OLD.store(true, Ordering::Relaxed);
        }
        let cmd: &[u8] = if FUSERMOUNT_OLD.load(Ordering::Relaxed) {
            b"fusermount"
        } else {
            b"fusermount3"
        };

        let mut tmp: Option<Vec<u8>> = name.map(|n| cstr(n).to_vec());
        let mut sb: Option<std::fs::Metadata> = None;
        let mut psb: Option<std::fs::Metadata> = None;
        let mut hovered = false;

        let mntpath = self.mntpath();

        if let Some(t) = &tmp
            && mntpath == cstr(currentpath)
        {
            *newpath = mkpath(&mntpath, t, &self.home).0;
            sb = std::fs::symlink_metadata(path(newpath)).ok();
            psb = std::fs::symlink_metadata(path(&xdirname(newpath))).ok();
            if sb.is_none() && psb.is_none() {
                *presel = MSGWAIT;
                return false;
            }
        }

        let needs_host = match (&tmp, &sb) {
            (None, _) | (_, None) => true,
            (Some(_), Some(s)) => {
                !s.file_type().is_dir() || psb.as_ref().is_some_and(|p| s.dev() == p.dev())
            }
        };
        if needs_host {
            let t = self.xreadline(None, MESSAGES[MSG_HOSTNAME]);
            if t.is_empty() {
                return false;
            }
            if let Some(n) = name
                && t == b"-"
            {
                *newpath = mkpath(currentpath, n, &self.home).0;
                hovered = true;
            }
            tmp = Some(t);
        }

        if !hovered {
            // INFALLIBLE: tmp is the hovered name or the host just read
            let t = tmp.unwrap_or_default();
            *newpath = mkpath(&mntpath, &t, &self.home).0;
        }

        if !xdiraccess(newpath) {
            *presel = MSGWAIT;
            return false;
        }

        let np = newpath.clone();
        if self.spawn(cmd, Some(b"-qu"), Some(&np), None, F_NORMAL) != 0 {
            if !xconfirm(self.get_input(Some(MESSAGES[MSG_LAZY]))) {
                return false;
            }
            if self.spawn(cmd, Some(b"-quz"), Some(&np), None, F_NORMAL) != 0 {
                self.printwait(MESSAGES[MSG_FAILED], Some(presel));
                return false;
            }
        }

        if let Err(e) = std::fs::remove_dir(path(newpath)) {
            self.printwarn_err(&e, Some(presel));
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes() {
        assert_eq!(shell_escape(b"a'b"), b"'a'\\''b'");
        assert_eq!(shell_escape(b""), b"''");
    }

    #[test]
    fn sel_in_dir() {
        assert!(!sel_paths_in_dir(b"", b"/a"));
        assert!(sel_paths_in_dir(b"/a/x\0/a/y/z\0", b"/a"));
        assert!(!sel_paths_in_dir(b"/a/x\0/ab\0", b"/a"));
        assert!(!sel_paths_in_dir(b"/a\0", b"/a"));
        assert!(sel_paths_in_dir(b"/x\0", b"/"));
        assert!(!sel_paths_in_dir(b"/\0", b"/"));
    }

    #[test]
    fn mount_dir_names() {
        assert_eq!(archive_mount_dir(b"a.tar.gz"), b"a.tar");
        assert_eq!(archive_mount_dir(b".zip"), b".zip");
        assert_eq!(archive_mount_dir(b"x"), b"x");
        assert_eq!(archive_mount_dir(b"a."), b"a");
    }

    #[test]
    fn extract_names() {
        assert_eq!(extract_dir_suggestion(b"/d/a.tar.gz"), b"a");
        assert_eq!(extract_dir_suggestion(b"/d/.a.tar"), b".a.tar");
        assert_eq!(extract_dir_suggestion(b"/d/ab"), b"ab");
    }
}
