//! Archives (create, list, extract, mount) and remote mounts (src/nnn.c: get_archive_cmd,
//! sel_paths_in_dir, archive_selection, handle_archive, archive_mount, remote_mount, unmount).

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::browse::{MESSAGES, MSG_FAILED, MSG_NOT_SUPPORTED, xdiraccess};
use crate::config::{
    TOK_MNT, TOKS, cstr, is_prefix, is_suffix, mkpath, xbasename, xdirname, xmktree_io,
};
use crate::platform::{self, path};
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

/// `archive_cmd[]`
const ARCHIVE_CMD: [&[u8]; 4] = [b"atool -a", b"bsdtar -acvf", b"zip -r", b"tar -acvf"];

/// The static `found` of `unmount`: "fusermount3" was not found, "fusermount" is used.
static FUSERMOUNT_OLD: AtomicBool = AtomicBool::new(false);

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

/// `<system_root>\System32\tar.exe -acvf` when that file exists, else `tar -acvf`.
#[cfg(windows)]
fn windows_tar_cmd(system_root: Option<&std::ffi::OsStr>) -> Vec<u8> {
    let tar = system_root.map(|r| std::path::Path::new(r).join("System32").join("tar.exe"));
    match tar.filter(|t| t.is_file()) {
        Some(t) => [
            crate::platform::into_bytes(t.into_os_string()),
            b" -acvf".to_vec(),
        ]
        .concat(),
        None => ARCHIVE_CMD[3].to_vec(),
    }
}

/// If `cmd` invokes `tar` or `bsdtar`, builds `<cmd...> <archive> --null -T <list>` so tar
/// reads the NUL-separated selection list directly without `xargs` or a shell.
pub(crate) fn tar_list_argv(cmd: &[u8], archive: &[u8], list: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut argv = tar_cmd_prefix(cmd)?;
    argv.push(cstr(archive).to_vec());
    argv.push(b"--null".to_vec());
    argv.push(b"-T".to_vec());
    argv.push(cstr(list).to_vec());
    Some(argv)
}

fn tar_cmd_prefix(cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
    let argv = crate::popup::parseargs(cstr(cmd))?;
    let first = argv.first()?;
    let base = first
        .rsplit(|&c| c == b'/' || c == b'\\')
        .next()
        .unwrap_or(first);
    let lower: Vec<u8> = base.iter().map(u8::to_ascii_lowercase).collect();
    let stem = lower.strip_suffix(b".exe").unwrap_or(&lower);
    if stem != b"tar" && stem != b"bsdtar" {
        return None;
    }
    Some(argv)
}

/// Reads the NUL-separated selection entries from `pselbuf`, falling back to `selpath`
/// when `pselbuf` is empty (external selection).
fn selection_entries(pselbuf: &[u8], selpath: &[u8], relative: bool, path: &[u8]) -> Vec<Vec<u8>> {
    let owned;
    let buf = if pselbuf.is_empty() && !selpath.is_empty() {
        owned = std::fs::read(crate::platform::path(selpath)).unwrap_or_default();
        owned.as_slice()
    } else {
        pselbuf
    };
    let path_bytes = cstr(path);
    let dirlen = path_bytes.len() + usize::from(path_bytes != b"/");
    buf.split(|&c| c == 0)
        .filter(|e| !e.is_empty())
        .map(|e| {
            if relative {
                e.get(dirlen..).unwrap_or_default().to_vec()
            } else {
                e.to_vec()
            }
        })
        .collect()
}

/// Windows tar-family argv for a selection: passes entries directly as UTF-16 argv when they
/// fit under `CreateProcessW`'s limit (so bsdtar receives Unicode names without `-T`'s
/// ANSI codepage decoding), and falls back to `--null -T <list>` for oversized selections.
fn tar_selection_argv(
    cmd: &[u8],
    archive: &[u8],
    entries: &[Vec<u8>],
    list: &[u8],
) -> Option<Vec<Vec<u8>>> {
    let mut argv = tar_cmd_prefix(cmd)?;
    let approx_len: usize =
        cmd.len() + archive.len() + 8 + entries.iter().map(|e| e.len() + 3).sum::<usize>();
    if !entries.is_empty() && approx_len <= 28_000 {
        argv.push(cstr(archive).to_vec());
        argv.push(b"--".to_vec());
        argv.extend(entries.iter().cloned());
        return Some(argv);
    }
    tar_list_argv(cmd, archive, list)
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
        // Windows: its own tar (bsdtar) takes drive paths; Git for Windows' GNU tar, often
        // first on PATH, reads "C:/x.tar" as a remote host.
        #[cfg(windows)]
        if i == 3 {
            return windows_tar_cmd(std::env::var_os("SystemRoot").as_deref());
        }
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

        let selpath = match &tmpfile {
            Some(t) => t.clone(),
            None => self.selpath.clone().unwrap_or_default(),
        };

        let win_tar_argv = if cfg!(windows) {
            let entries = selection_entries(&self.pselbuf, &selpath, relative, path);
            tar_selection_argv(cmd, archive, &entries, &selpath)
        } else {
            None
        };

        if let Some(argv) = win_tar_argv {
            let cwd = relative.then(|| cstr(path));
            self.spawn_argv(&argv, 1, cwd, F_CLI | F_CONFIRM);
        } else if cfg!(windows) && crate::platform::has_coreutil(b"xargs") {
            // Coreutils' xargs reads the list itself (-a): no shell, no quoting.
            let mut argv = vec![b"xargs".to_vec(), b"-0".to_vec(), b"-a".to_vec(), selpath];
            argv.extend(crate::popup::parseargs(cstr(cmd)).unwrap_or_default());
            argv.push(cstr(archive).to_vec());
            let cwd = relative.then(|| cstr(path));
            self.spawn_argv(&argv, 1, cwd, F_CLI | F_CONFIRM);
        } else {
            let archive_esc = shell_escape(archive);
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

            self.spawn(
                crate::platform::shell_exec_prefix(),
                Some(&buf),
                None,
                None,
                F_CLI | F_CONFIRM,
            );
        }

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
                    Ok(d) => outdir = crate::platform::bytes(d.as_os_str()).to_vec(),
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
        if !platform::SUPPORTS_MOUNTS {
            self.printwait(MESSAGES[MSG_NOT_SUPPORTED], None);
            return false;
        }
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
        if !platform::SUPPORTS_MOUNTS {
            self.printwait(MESSAGES[MSG_NOT_SUPPORTED], None);
            return false;
        }
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
        if !platform::SUPPORTS_MOUNTS {
            self.printwait(MESSAGES[MSG_NOT_SUPPORTED], Some(presel));
            return false;
        }
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
        let mut sb: Option<platform::FileInfo> = None;
        let mut psb: Option<platform::FileInfo> = None;
        let mut hovered = false;

        let mntpath = self.mntpath();

        if let Some(t) = &tmp
            && mntpath == cstr(currentpath)
        {
            *newpath = mkpath(&mntpath, t, &self.home).0;
            sb = platform::stat(newpath, false).ok();
            psb = platform::stat(&xdirname(newpath), false).ok();
            if sb.is_none() && psb.is_none() {
                *presel = MSGWAIT;
                return false;
            }
        }

        let needs_host = match (&tmp, &sb) {
            (None, _) | (_, None) => true,
            (Some(_), Some(s)) => {
                s.mode & platform::S_IFMT != platform::S_IFDIR
                    || psb.as_ref().is_some_and(|p| s.dev == p.dev)
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

    #[cfg(windows)]
    #[test]
    fn windows_tar_prefers_system32() {
        let root = std::env::temp_dir().join(format!("nnn-sysroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("System32")).unwrap();
        assert_eq!(windows_tar_cmd(Some(root.as_os_str())), b"tar -acvf");
        std::fs::write(root.join("System32").join("tar.exe"), b"").unwrap();
        let tar = root.join("System32").join("tar.exe").into_os_string();
        let want = [crate::platform::into_bytes(tar), b" -acvf".to_vec()].concat();
        assert_eq!(windows_tar_cmd(Some(root.as_os_str())), want);
        assert_eq!(windows_tar_cmd(None), b"tar -acvf");
    }

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

    #[test]
    fn tar_list_argv_forms() {
        assert_eq!(
            tar_list_argv(b"tar -acvf", b"a.tar", b"l"),
            Some(vec![
                b"tar".to_vec(),
                b"-acvf".to_vec(),
                b"a.tar".to_vec(),
                b"--null".to_vec(),
                b"-T".to_vec(),
                b"l".to_vec(),
            ])
        );
        let full = tar_list_argv(b"C:/WINDOWS/System32/tar.exe -acvf", b"a.tar", b"l").unwrap();
        assert_eq!(full[0], b"C:/WINDOWS/System32/tar.exe");
        assert!(tar_list_argv(b"bsdtar -acvf", b"a.tar", b"l").is_some());
        assert!(tar_list_argv(b"BSDTAR.EXE -acvf", b"a.tar", b"l").is_some());
        assert_eq!(tar_list_argv(b"zip -r", b"a.zip", b"l"), None);
        assert_eq!(tar_list_argv(b"atool -a", b"a.tar", b"l"), None);
    }

    #[test]
    fn selection_entries_falls_back_to_selpath() {
        let dir = std::env::temp_dir().join(format!("nnn-selpath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sel = dir.join(".selection");
        std::fs::write(&sel, b"/w/a\0/w/b c\0").unwrap();
        let sel_b = crate::platform::bytes(sel.as_os_str()).into_owned();
        assert_eq!(
            selection_entries(b"", &sel_b, true, b"/w"),
            vec![b"a".to_vec(), b"b c".to_vec()]
        );
        assert_eq!(
            selection_entries(b"", &sel_b, false, b"/w"),
            vec![b"/w/a".to_vec(), b"/w/b c".to_vec()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn tar_list_argv_archives_odd_names() {
        let dir = std::env::temp_dir().join(format!("nnn-tar-odd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["x y", "a&b", "ñandú.txt"];
        for n in names {
            std::fs::write(dir.join(n), b"data").unwrap();
        }
        let list_path = dir.join("list");
        let entries: Vec<Vec<u8>> = names.iter().map(|n| n.as_bytes().to_vec()).collect();
        let list_arg = crate::platform::bytes(list_path.as_os_str()).into_owned();
        let cmd = windows_tar_cmd(std::env::var_os("SystemRoot").as_deref());
        let argv = tar_selection_argv(&cmd, b"g.tar", &entries, &list_arg).unwrap();
        let status = crate::platform::command(&argv, 1)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
        let out_dir = dir.join("out");
        std::fs::create_dir(&out_dir).unwrap();
        let extract = crate::platform::command(
            &[
                argv[0].clone(),
                b"-xf".to_vec(),
                b"g.tar".to_vec(),
                b"-C".to_vec(),
                b"out".to_vec(),
            ],
            1,
        )
        .current_dir(&dir)
        .status()
        .unwrap();
        assert!(extract.success(), "{extract}");
        for n in names {
            assert_eq!(std::fs::read(out_dir.join(n)).unwrap(), b"data");
        }
        // `--null -T <list>` fallback also archives ASCII odd names without xargs/shell.
        let ascii = ["x y", "a&b"];
        std::fs::write(&list_path, ascii.join("\0").as_bytes()).unwrap();
        let list_argv = tar_list_argv(&cmd, b"g2.tar", &list_arg).unwrap();
        let status2 = crate::platform::command(&list_argv, 1)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status2.success(), "{status2}");
        let out2 = crate::platform::command(
            &[list_argv[0].clone(), b"-tf".to_vec(), b"g2.tar".to_vec()],
            1,
        )
        .current_dir(&dir)
        .output()
        .unwrap();
        assert!(out2.status.success());
        let listed = String::from_utf8_lossy(&out2.stdout);
        for n in ascii {
            assert!(listed.lines().any(|l| l == n), "missing {n} in:\n{listed}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
