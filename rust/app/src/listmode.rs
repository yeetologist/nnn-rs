//! List mode: paths read from stdin (or a plugin's FIFO) into a temporary tree of symlinks
//! (\`load_input\`, \`make_tmp_tree\`, \`rmlistpath\` and the list-mode part of \`main()\` in
//! src/nnn.c).

use crate::curses::color_pair;
use crate::dir::selforparent;
use crate::platform::{self, Os};
use crate::platform::{Access, Errno};
use crate::state::{Nnn, PATH_MAX};
use crate::sys;
use std::hash::{BuildHasher, Hasher};
use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

/// \`LIST_FILES_MAX\`: list mode shows at most 16K files.
pub const LIST_FILES_MAX: usize = 1 << 14;
/// \`LIST_INPUT_MAX\`
pub const LIST_INPUT_MAX: usize = LIST_FILES_MAX * PATH_MAX;
/// The chunk \`load_input\` reads and grows its buffer by (512 KiB).
const CHUNK: usize = 512 * 1024;
/// \`XDELAY_INTERVAL_MS\` (in microseconds, as the original uses it).
const XDELAY_INTERVAL_MS: u64 = 350_000;
/// \`utils[UTIL_RM_RF]\`
const UTIL_RM_RF: [&str; 2] = ["rm", "-rf"];

/// \`messages[MSG_0_ENTRIES]\`
pub const MSG_0_ENTRIES: &[u8] = b"0 entries";
/// \`messages[MSG_FILE_LIMIT]\`
pub const MSG_FILE_LIMIT: &[u8] = b"file limit exceeded";
/// \`messages[MSG_SIZE_LIMIT]\`
pub const MSG_SIZE_LIMIT: &[u8] = b"size limit exceeded";

/// State this module keeps: none beyond nnn.c's globals \`listpath\`/\`listroot\` (in \`Nnn\`).
#[derive(Debug, Default)]
pub struct ListState {}

/// \`xmemrchr\`: index of the last \`ch\` in \`s\`.
fn xmemrchr(s: &[u8], ch: u8) -> Option<usize> {
    s.iter().rposition(|&c| c == ch)
}

/// \`xdirname\` on an absolute path: the directory part ("/" for a top-level entry).
fn xdirname(p: &[u8]) -> &[u8] {
    if p.contains(&b'/') {
        crate::platform::xdirname(p)
    } else {
        b"/"
    }
}

/// \`common_prefix\`: shortens \`prefix\` to the directory prefix it shares with \`path\` (sets it
/// to \`path\` when empty). False where the original returns NULL (empty \`path\`).
fn common_prefix(path: &[u8], prefix: &mut Vec<u8>) -> bool {
    common_prefix_with(Os::CURRENT, path, prefix)
}

/// \`common_prefix\` for the path forms of \`os\`.
fn common_prefix_with(os: Os, path: &[u8], prefix: &mut Vec<u8>) -> bool {
    if path.is_empty() {
        return false;
    }
    if prefix.is_empty() {
        // xstrsncpy(prefix, path, PATH_MAX)
        prefix.extend_from_slice(&path[..path.len().min(PATH_MAX - 1)]);
        return true;
    }
    let n = path
        .iter()
        .zip(prefix.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let (x, y) = (path.get(n), prefix.get(n));
    // Strings are same
    if x.is_none() && y.is_none() {
        return true;
    }
    // Path is shorter: the original copies with a size computed from two unrelated
    // buffers, which in practice copies all of path (path is a prefix of prefix here).
    if x.is_none() && y == Some(&b'/') {
        prefix.truncate(n);
        return true;
    }
    // Prefix is shorter
    if y.is_none() && x == Some(&b'/') {
        return true;
    }
    // Shorten prefix
    prefix.truncate(n);
    let root = platform::root_len_with(os, path).max(1);
    match xmemrchr(prefix, b'/') {
        Some(sep) => prefix.truncate(sep.max(root)),
        None => prefix.truncate(root), // Just the root
    }
    true
}

/// \`xmktree(path, TRUE, TRUE)\`: \`mkdir -p\` with mode 0777 (umask applies).
fn xmktree(p: &[u8]) -> bool {
    let mkdir = |d: &[u8]| match platform::mkdir(d, 0o777) {
        Ok(()) => true,
        Err(e) => platform::is_errno(&e, Errno::Exist),
    };
    if p.is_empty() {
        return false;
    }
    // Skip the first '/'
    for (i, &c) in p.iter().enumerate().skip(1) {
        if c == b'/' && !mkdir(&p[..i]) {
            return false;
        }
    }
    mkdir(p)
}

/// \`mkdtemp\`: replaces the trailing "XXXXXX" of \`template\` and creates the directory 0700.
fn mkdtemp(template: &mut [u8]) -> bool {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let Some(start) = template.len().checked_sub(6) else {
        return false;
    };
    if &template[start..] != b"XXXXXX" {
        return false;
    }
    let seed = std::collections::hash_map::RandomState::new();
    // TMP_MAX attempts, as glibc's __gen_tempname
    for attempt in 0u32..238_328 {
        let mut h = seed.build_hasher();
        h.write_u32(attempt);
        h.write_u32(std::process::id());
        let mut v = h.finish();
        for c in &mut template[start..] {
            *c = LETTERS[(v % 62) as usize];
            v /= 62;
        }
        match platform::mkdir(template, 0o700) {
            Ok(()) => return true,
            Err(e) if platform::is_errno(&e, Errno::Exist) => {}
            Err(_) => return false,
        }
    }
    false
}

/// \`msg()\`: a line on stderr.
fn msg(m: &[u8]) {
    let mut e = std::io::stderr();
    let _ = e.write_all(m);
    let _ = e.write_all(b"\n");
}

impl Nnn {
    /// \`printmsg()\` then \`xdelay()\` (nnn.c 1622, 1724), kept here until the module that ports
    /// the status line owns them.
    fn listmode_printmsg_delay(&mut self, m: &[u8], delay_us: u64) {
        let pair = color_pair(self.cfg.curctx as u32 + 1);
        let last = self.xlines - 1;
        let cs = self.cs();
        cs.stdscr.attron(pair);
        cs.stdscr.mvaddstr(last, 0, m);
        cs.stdscr.clrtoeol();
        cs.stdscr.attroff(pair);
        cs.refresh();
        std::thread::sleep(Duration::from_micros(delay_us));
    }

    /// \`rmlistpath()\`: removes the list-mode temp tree (\`rm -rf\`, output suppressed).
    pub fn rmlistpath(&mut self) {
        if let Some(lp) = self.listpath.take() {
            // spawn(utils[UTIL_RM_RF], listpath, NULL, NULL, F_NOTRACE | F_MULTI)
            let _ = std::process::Command::new(UTIL_RM_RF[0])
                .arg(UTIL_RM_RF[1])
                .arg(&*crate::platform::os(&lp))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    /// \`make_tmp_tree()\`: creates \`<tmp>/nnnXXXXXX\` (which becomes \`listpath\`) and in it a
    /// symlink to each existing path, at the path relative to \`prefix\`. Returns the
    /// directory in which to start.
    pub fn make_tmp_tree(&mut self, paths: &[Option<Vec<u8>>], prefix: &[u8]) -> Option<Vec<u8>> {
        // tmpdir holds the full path; tmpdir[tmp..] the path without the tmp dir prefix
        let mut tmpdir = self.g_tmpfpath.clone();
        tmpdir.extend_from_slice(b"/nnnXXXXXX");
        let tmp = tmpdir.len();

        // handle the case where files are directly under /
        let len = if prefix == b"/" { 0 } else { prefix.len() };

        if !mkdtemp(&mut tmpdir) {
            return None;
        }

        self.listpath = Some(tmpdir.clone());

        for p in paths.iter().flatten() {
            let info = match platform::stat(p, true) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                other => other.ok(),
            };

            // Don't copy the common prefix
            tmpdir.truncate(tmp);
            tmpdir.extend_from_slice(p.get(len..).unwrap_or_default());

            // Get the dir containing the path
            let dir_end = xmemrchr(&tmpdir[tmp..], b'/').map_or(tmpdir.len(), |s| tmp + s);
            let dir = &tmpdir[..dir_end];
            if platform::access(dir, Access::Exists).is_err() {
                // Create directory if it doesn't exist
                xmktree(dir);
            }

            let is_dir = info.is_some_and(|i| i.mode & 0o170000 == 0o040000);
            let _ = platform::symlink(p, &tmpdir, is_dir);
        }

        // Get the dir in which to start
        tmpdir.truncate(tmp);
        Some(tmpdir)
    }

    /// \`load_input()\`: reads NUL-separated paths from \`fd\` (relative ones resolved against
    /// \`cwd\`, or the working directory when \`None\`), sets \`listroot\` to their common directory
    /// and builds the temp tree. Shows "0 entries" / "file limit exceeded" on the status line
    /// once curses runs, on stderr before.
    pub fn load_input(&mut self, fd: &mut dyn Read, cwd: Option<&[u8]>) -> Option<Vec<u8>> {
        let chunk = CHUNK;
        let mut chunk_count = 1usize;
        let mut input = vec![0u8; chunk];
        let mut offsets: Vec<usize> = Vec::new();
        let (mut total_read, mut off) = (0usize, 0usize);
        let mut msgnum: Option<&'static [u8]> = None;
        let mut tmpdir = None;

        let cwd: Vec<u8> = match cwd {
            None => match std::env::current_dir() {
                Ok(d) => crate::platform::into_bytes(d.into_os_string()),
                Err(_) => return None,
            },
            Some(p) => p[..p.len().min(PATH_MAX - 1)].to_vec(),
        };

        'read: {
            while chunk_count < LIST_INPUT_MAX / chunk && msgnum.is_none() {
                let input_read = match fd.read(&mut input[total_read..total_read + chunk]) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break 'read,
                };
                total_read += input_read;

                while off < total_read {
                    let Some(nul) = input[off..total_read].iter().position(|&c| c == 0) else {
                        break;
                    };
                    let next = off + nul + 1;
                    if next == off + 1 {
                        off = next;
                        continue;
                    }
                    if offsets.len() == LIST_FILES_MAX {
                        msgnum = Some(MSG_FILE_LIMIT);
                        break;
                    }
                    offsets.push(off);
                    off = next;
                }

                // We don't need to allocate another chunk
                if chunk_count > total_read.div_ceil(chunk) {
                    continue;
                }

                // We can never need more than one additional chunk
                chunk_count += 1;
                if chunk_count > LIST_INPUT_MAX / chunk {
                    msgnum = Some(MSG_SIZE_LIMIT);
                    break;
                }
                input.resize(chunk_count * chunk, 0);
            }

            // Any extra data is left to the kernel to handle
            if off != total_read {
                if offsets.len() == LIST_FILES_MAX {
                    msgnum = Some(MSG_FILE_LIMIT);
                } else {
                    offsets.push(off);
                }
            }

            if offsets.is_empty() {
                msgnum = Some(MSG_0_ENTRIES);
                break 'read;
            }

            input.truncate(total_read);
            let entry = |o: usize| {
                let rest = &input[o..];
                &rest[..rest.iter().position(|&c| c == 0).unwrap_or(rest.len())]
            };

            let mut listroot = Vec::new();
            let mut paths: Vec<Option<Vec<u8>>> = Vec::with_capacity(offsets.len());
            let mut complete = true;
            for &o in &offsets {
                let raw = entry(o);
                if raw.first() == Some(&b'\n') || selforparent(raw) {
                    paths.push(None);
                    continue;
                }
                // abspath(paths[i], cwd, NULL): "~/" uses the global home (unset at startup)
                let home = (!self.home.is_empty()).then_some(self.home.as_slice());
                let Some(abs) = sys::abspath_in(raw, Some(&cwd), home) else {
                    complete = false;
                    break;
                };
                let ok = common_prefix(xdirname(&abs), &mut listroot);
                paths.push(Some(abs));
                if !ok {
                    complete = false;
                    break;
                }
            }
            self.listroot = Some(listroot);

            if complete
                && let Some(root) = self.listroot.clone()
                && !root.is_empty()
            {
                tmpdir = self.make_tmp_tree(&paths, &root);
            }
        }

        // Check if we are past init stage and show msg
        if let Some(m) = msgnum {
            if self.curses.is_some() {
                self.listmode_printmsg_delay(m, XDELAY_INTERVAL_MS << 2);
            } else {
                msg(m);
                std::thread::sleep(Duration::from_micros(XDELAY_INTERVAL_MS << 2));
            }
        }

        tmpdir
    }

    /// The list-mode part of \`main()\` (nnn.c 10736-10757): when stdin is not a terminal, read
    /// the paths from it into \`initpath\`, then make stdin the terminal again. False where
    /// \`main()\` returns \`EXIT_FAILURE\`. The caller also clears \`curssn\` (sessions) when this
    /// read a list.
    pub fn list_mode_main(&mut self) -> bool {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return true;
        }
        // This is the same as listpath
        let Some(initpath) = self.load_input(&mut stdin, None) else {
            return false;
        };
        self.initpath = initpath;

        // We return to tty
        platform::stdin_to_tty();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn dirname_and_prefix() {
        assert_eq!(xdirname(b"/a/b"), b"/a");
        assert_eq!(xdirname(b"/a"), b"/");
        assert_eq!(xdirname(b"/"), b"/");
        let mut p = Vec::new();
        for (path, want) in [
            (&b"/a/b/c"[..], &b"/a/b/c"[..]),
            (b"/a/b/c", b"/a/b/c"),
            (b"/a/b/c/d", b"/a/b/c"),
            (b"/a/b", b"/a/b"),
            (b"/a/bc", b"/a"),
            (b"/x", b"/"),
        ] {
            assert!(common_prefix(path, &mut p));
            assert_eq!(p, want, "{:?}", String::from_utf8_lossy(path));
        }
        let mut p = b"/ab".to_vec();
        common_prefix(b"/ac", &mut p);
        assert_eq!(p, b"/");
        assert!(!common_prefix(b"", &mut p));
    }

    #[test]
    fn prefix_windows_roots() {
        let mut p = b"C:/a/b".to_vec();
        assert!(common_prefix_with(Os::Windows, b"C:/a/c", &mut p));
        assert_eq!(p, b"C:/a");
        assert!(common_prefix_with(Os::Windows, b"C:/b", &mut p));
        assert_eq!(p, b"C:/");
        let mut p = b"//srv/share/a".to_vec();
        assert!(common_prefix_with(Os::Windows, b"//srv/share/b", &mut p));
        assert_eq!(p, b"//srv/share/");
    }

    // W7 Task 6: list mode's temp tree needs symlinks and rm -rf (Unix only so far).
    #[cfg(unix)]
    #[test]
    fn tmp_tree_from_pipe() {
        let base = std::env::temp_dir().join(format!("nnn-listmode-{}", std::process::id()));
        let fx = base.join("fx");
        std::fs::create_dir_all(fx.join("d/e")).unwrap();
        std::fs::write(fx.join("d/e/f"), b"").unwrap();
        std::fs::write(fx.join("d/g"), b"").unwrap();
        let fxb = crate::platform::bytes(fx.as_os_str()).to_vec();
        let mut n = Nnn {
            g_tmpfpath: crate::platform::bytes(base.as_os_str()).to_vec(),
            ..Nnn::default()
        };
        let (mut r, mut w) = std::io::pipe().unwrap();
        w.write_all(b"d/e/f\0\0./d/g\0.\0missing/x\0\nnl\0d/e")
            .unwrap();
        drop(w);
        let root = n.load_input(&mut r, Some(&fxb)).unwrap();
        assert_eq!(n.listpath.as_deref(), Some(root.as_slice()));
        // "missing/x" still counts towards the common directory.
        assert_eq!(n.listroot.as_deref(), Some(fxb.as_slice()));
        let root = std::path::PathBuf::from(&*crate::platform::os(&root));
        assert_eq!(
            std::fs::read_link(root.join("d/g")).unwrap(),
            fx.join("d/g")
        );
        assert_eq!(
            std::fs::read_link(root.join("d/e/f")).unwrap(),
            fx.join("d/e/f")
        );
        // "d/e" came after "d/e/f" made the directory: no link.
        assert!(std::fs::read_link(root.join("d/e")).is_err());
        assert!(!root.join("missing").exists());
        n.rmlistpath();
        assert!(!root.exists());
        assert!(n.listpath.is_none());

        // Only separators: 0 entries, nothing created.
        let (mut r, mut w) = std::io::pipe().unwrap();
        w.write_all(b"\0\0\0").unwrap();
        drop(w);
        assert_eq!(n.load_input(&mut r, Some(&fxb)), None);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
