//! Configuration: setup_config, key-value pairs (bookmarks, plugins, order), xmktree,
//! write_lastdir, and the path helpers of src/nnn.c (mkpath, xdirname, xbasename,
//! common_prefix, get_cwd_entry, is_suffix, is_prefix).

use crate::state::{Kv, Nnn, PATH_MAX};
use crate::sys;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// `CMD_LEN_MAX`: the size of `g_buf`.
pub const CMD_LEN_MAX: usize = PATH_MAX + ((crate::state::NAME_MAX + 1) << 1);

/// Indices of `env_cfg[]` used to tell key-value lists apart.
pub const NNN_BMS: u8 = 1;
pub const NNN_PLUG: u8 = 2;
pub const NNN_SEL: u8 = 9;
pub const NNN_ORDER: u8 = 11;

/// `toks[]`: the directories made under the config directory ("plugins" must be last).
pub const TOKS: [&[u8]; 4] = [b"bookmarks", b"sessions", b"mounts", b"plugins"];
pub const TOK_BM: usize = 0;
pub const TOK_SSN: usize = 1;
pub const TOK_MNT: usize = 2;
pub const TOK_PLG: usize = 3;

/// `messages[]` indices that handle_bookmark returns, and the texts this module shows.
pub const MSG_ACCESS: usize = 24;
pub const MSG_NOT_SET: usize = 27;
pub const MSG_INVALID_KEY: usize = 40;
const MSG_CANCEL: &[u8] = b"cancelled";
const MSG_KEYS: &[u8] = b"keys:";
const MSG_BM_NAME: &[u8] = b"name: ";

/// The maximum number of key-value pairs parsekvpair reads.
const KV_MAX: usize = 100;

use crate::platform::path;
/// The C string in `s`: the bytes before the first NUL.
pub fn cstr(s: &[u8]) -> &[u8] {
    s.iter().position(|&c| c == 0).map_or(s, |at| &s[..at])
}

/// `xstrsncpy`: copy the C string `src` into `dst`, at most `n` bytes including the
/// terminating NUL, which is always written when `n` > 0. Returns the bytes written.
pub fn xstrsncpy(dst: &mut [u8], src: &[u8], n: usize) -> usize {
    let src = cstr(src);
    let n = n.min(dst.len());
    if src.len() < n {
        dst[..src.len()].copy_from_slice(src);
        dst[src.len()] = 0;
        src.len() + 1
    } else {
        if n > 0 {
            dst[..n - 1].copy_from_slice(&src[..n - 1]);
            dst[n - 1] = 0;
        }
        n
    }
}

/// `is_suffix`.
pub fn is_suffix(s: &[u8], suffix: &[u8]) -> bool {
    let (s, suffix) = (cstr(s), cstr(suffix));
    suffix.len() <= s.len() && &s[s.len() - suffix.len()..] == suffix
}

/// `is_prefix`: `strncmp(s, prefix, len) == 0`.
pub fn is_prefix(s: &[u8], prefix: &[u8], len: usize) -> bool {
    for i in 0..len {
        let a = s.get(i).copied().unwrap_or(0);
        let b = prefix.get(i).copied().unwrap_or(0);
        if a != b {
            return false;
        }
        if a == 0 {
            return true;
        }
    }
    true
}

/// `is_bad_len_or_dir`.
pub fn is_bad_len_or_dir(p: &[u8]) -> bool {
    let p = cstr(p);
    p.len() >= PATH_MAX || p.last() == Some(&b'/')
}

/// `get_cwd_entry`: where the first component of `entrypath` below `cwdpath` starts, and its
/// length; `None` when `entrypath` is not under `cwdpath`.
pub fn get_cwd_entry(cwdpath: &[u8], entrypath: &[u8]) -> Option<(usize, usize)> {
    let len = cstr(cwdpath).len();
    if !is_prefix(entrypath, cwdpath, len) {
        return None;
    }
    let entrypath = cstr(entrypath);
    // The original steps over the '/' after cwdpath; an entrypath equal to cwdpath has none.
    let start = (len + 1).min(entrypath.len());
    let rest = &entrypath[start..];
    let tokenlen = rest.iter().position(|&c| c == b'/').unwrap_or(rest.len());
    Some((start, tokenlen))
}

/// `xmemrchr`: the last `ch` in the first `n` bytes of `s`.
fn xmemrchr(s: &[u8], ch: u8, n: usize) -> Option<usize> {
    s[..n.min(s.len())].iter().rposition(|&c| c == ch)
}

/// `xdirname` (a very simplified dirname). The original dereferences NULL for a path with no
/// '/'; this returns such a path unchanged.
pub fn xdirname(p: &[u8]) -> Vec<u8> {
    let p = cstr(p);
    crate::platform::xdirname(p).to_vec()
}

/// `xbasename`.
pub fn xbasename(p: &[u8]) -> &[u8] {
    let p = cstr(p);
    match xmemrchr(p, b'/', p.len()) {
        Some(at) => &p[at + 1..],
        None => p,
    }
}

/// `tilde_is_home`.
pub fn tilde_is_home(s: &[u8]) -> bool {
    s.first() == Some(&b'~') && matches!(s.get(1), None | Some(0 | b'/'))
}

/// `tilde_is_home_strict`.
pub fn tilde_is_home_strict(s: &[u8]) -> bool {
    s.starts_with(b"~/")
}

/// `istopdir`.
pub fn istopdir(p: &[u8]) -> bool {
    crate::platform::is_root(cstr(p))
}

/// `convert_tilde`: `~` or `~/...` with `home` put in place of the `~`.
fn convert_tilde(p: &[u8], home: &[u8]) -> Option<Vec<u8>> {
    tilde_is_home(p).then(|| {
        let mut buf = cstr(home).to_vec();
        buf.extend_from_slice(&cstr(p)[1..]);
        buf
    })
}

/// `mkpath`: "dir/name", "/name" or, for "~/name", "$HOME/name", cut to `PATH_MAX` as the
/// original's buffer is. Returns the path and the original's return value (the bytes
/// written including the NUL).
pub fn mkpath(dir: &[u8], name: &[u8], home: &[u8]) -> (Vec<u8>, usize) {
    let mut out = vec![0u8; PATH_MAX];
    let mut len = 0;
    let mut name = cstr(name);
    if tilde_is_home_strict(name) {
        len = xstrsncpy(&mut out, home, PATH_MAX) - 1;
        name = &name[1..];
    } else if name.first() != Some(&b'/') {
        len = if istopdir(dir) {
            1
        } else {
            xstrsncpy(&mut out, dir, PATH_MAX)
        };
        out[len - 1] = b'/';
    }
    let r = xstrsncpy(&mut out[len..], name, PATH_MAX - len) + len;
    (cstr(&out).to_vec(), r)
}

/// `common_prefix`: shortens `prefix` to the directory both paths are under; `false` (NULL)
/// for an empty `path`.
///
/// Where `path` is a shorter match, the original copies `y - path` bytes, a difference of
/// pointers into two buffers; that is always more than `path` holds in practice, so the
/// whole `path` is copied here. A prefix with no '/' before the mismatch makes the original
/// dereference NULL; here it becomes empty.
pub fn common_prefix(p: &[u8], prefix: &mut Vec<u8>) -> bool {
    let p = cstr(p);
    if p.is_empty() {
        return false;
    }
    let pre = cstr(prefix).to_vec();
    if pre.is_empty() {
        *prefix = p[..p.len().min(PATH_MAX - 1)].to_vec();
        return true;
    }
    let mut i = 0;
    while i < p.len() && i < pre.len() && p[i] == pre[i] {
        i += 1;
    }
    let x = p.get(i).copied().unwrap_or(0);
    let y = pre.get(i).copied().unwrap_or(0);
    if x == 0 && y == 0 {
        *prefix = pre;
        return true;
    }
    if x == 0 && y == b'/' {
        *prefix = p.to_vec();
        return true;
    }
    if y == 0 && x == b'/' {
        *prefix = pre;
        return true;
    }
    let mut pre = pre;
    pre.truncate(i);
    let root = crate::platform::root_len(p);
    match xmemrchr(&pre, b'/', i) {
        Some(at) => pre.truncate(at.max(root)),
        None => pre.clear(),
    }
    *prefix = pre;
    true
}

/// `bmtarget`: the absolute path of the link `filepath` points to, taking `cwd` into account.
pub fn bmtarget(filepath: &[u8], cwd: &[u8], home: &[u8]) -> Option<Vec<u8>> {
    let target = std::fs::read_link(path(filepath)).ok()?;
    let target = crate::platform::bytes(target.as_os_str());
    let target = &target[..target.len().min(PATH_MAX)];
    sys::abspath_in(cstr(target), Some(cwd), Some(home))
}

fn mkdir(p: &[u8]) -> io::Result<()> {
    match DirBuilder::new().mode(0o777).create(path(p)) {
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Ok(()),
        r => r,
    }
}

/// `xmktree` with the error the original leaves in `errno`.
pub fn xmktree_io(p: &[u8], dir: bool, tree: bool) -> io::Result<()> {
    let p = cstr(p);
    if p.is_empty() {
        return Err(io::Error::from_raw_os_error(libc::ENOENT));
    }
    if tree {
        // Skip the first '/'
        for (at, _) in p.iter().enumerate().skip(1).filter(|&(_, &c)| c == b'/') {
            mkdir(&p[..at])?;
        }
    }
    if dir {
        mkdir(p)
    } else {
        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path(p))
        {
            Err(e) if e.raw_os_error() != Some(libc::EEXIST) => Err(e),
            _ => Ok(()),
        }
    }
}

/// `xmktree`: create the non-existent parents of `p` (with `tree`), then `p` as a
/// directory or an empty file.
pub fn xmktree(p: &[u8], dir: bool, tree: bool) -> bool {
    xmktree_io(p, dir, tree).is_ok()
}

/// `shell_escape` into a fixed buffer of `buflen` bytes: `None` where the original fails.
fn shell_escape(inbuf: &[u8], buflen: usize) -> Option<Vec<u8>> {
    if buflen < 3 {
        return None;
    }
    let inbuf = cstr(inbuf);
    let n = inbuf.len();
    let mut out = Vec::with_capacity(n + 2);
    for r in 0..n + 2 {
        // 4 for the single quote case, 1 from NULL terminator
        if out.len() + 5 >= buflen {
            return None;
        }
        if r == 0 || r == n + 1 {
            out.push(b'\'');
        } else if inbuf[r - 1] == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(inbuf[r - 1]);
        }
    }
    Some(out)
}

/// The file work of `write_lastdir`: "cd '<curpath>'" into `outfile` (`~` expanded) or
/// `<cfgpath>/.lastd`.
pub fn write_lastdir_file(
    curpath: &[u8],
    outfile: Option<&[u8]>,
    cfgpath: &[u8],
    home: &[u8],
) -> io::Result<()> {
    let target = match outfile {
        None => mkpath(cfgpath, b".lastd", home).0,
        Some(o) => convert_tilde(o, home).unwrap_or_else(|| cstr(o).to_vec()),
    };
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path(&target))?;
    let Some(esc) = shell_escape(curpath, CMD_LEN_MAX - 3) else {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    };
    let mut buf = b"cd ".to_vec();
    buf.extend_from_slice(&esc);
    match f.write(&buf) {
        Ok(n) if n == buf.len() => Ok(()),
        Ok(_) => Err(io::Error::from_raw_os_error(libc::EIO)),
        Err(e) => Err(e),
    }
}

/// `parsekvpair`: "k:val;k:val..." as key-value pairs. `env` is the variable's value
/// (`getenv(env_cfg[id])`). Returns the original's result and the pairs read (none on
/// failure: the original leaves its count at 0 then).
pub fn parsekvpair(env: Option<&[u8]>) -> (bool, Vec<Kv>) {
    let Some(env) = env.map(cstr).filter(|e| !e.is_empty()) else {
        return (true, Vec::new());
    };
    let mut buf = env.to_vec();
    buf.push(0);
    let mut offs: Vec<(i32, usize)> = Vec::new();
    let mut new = true;
    let mut ptr = 0;
    while buf[ptr] != 0 && offs.len() < KV_MAX {
        if new {
            let key = i32::from(buf[ptr]);
            ptr += 1;
            if buf[ptr] != b':' {
                // The caller never reads past the count, which a failure leaves at 0.
                return (false, Vec::new());
            }
            ptr += 1;
            if buf[ptr] == 0 || buf[ptr] == b';' {
                return (false, Vec::new());
            }
            offs.push((key, ptr));
            new = false;
        }
        if buf[ptr] == b';' {
            buf[ptr] = 0;
            new = true;
        }
        ptr += 1;
    }
    let kvs: Vec<Kv> = offs
        .into_iter()
        .map(|(key, off)| Kv {
            key,
            val: cstr(&buf[off..]).to_vec(),
        })
        .collect();
    let n = kvs.len();
    (n != 0 && n <= KV_MAX, kvs)
}

/// `get_kv_val`: the value for `key`; for bookmarks (`id` != `NNN_PLUG`) the absolute path
/// with `~` expanded. `None` for no match or a path that does not resolve.
pub fn get_kv_val(kvarr: &[Kv], key: i32, id: u8, home: &[u8]) -> Option<Vec<u8>> {
    for kv in kvarr.iter().take_while(|kv| kv.key != 0) {
        if kv.key == key {
            // Do not allocate new memory for plugin
            if id == NNN_PLUG {
                return Some(kv.val.clone());
            }
            let val = convert_tilde(&kv.val, home).unwrap_or_else(|| kv.val.clone());
            return sys::abspath_in(&val, None, Some(home));
        }
    }
    None
}

/// `get_kv_key`: the key whose value is `val` (order strings only), or -1.
pub fn get_kv_key(kvarr: &[Kv], val: &[u8], id: u8) -> i32 {
    if id != NNN_ORDER {
        return -1;
    }
    kvarr
        .iter()
        .take_while(|kv| kv.key != 0)
        .find(|kv| kv.val == cstr(val))
        .map_or(-1, |kv| kv.key)
}

/// `printkv`: " k: value" lines.
pub fn printkv(kvarr: &[Kv], f: &mut dyn Write) -> io::Result<()> {
    for kv in kvarr.iter().take_while(|kv| kv.key != 0) {
        // The key is an unsigned char in the original.
        f.write_all(&[b' ', kv.key as u8, b':', b' '])?;
        f.write_all(cstr(&kv.val))?;
        f.write_all(b"\n")?;
    }
    Ok(())
}

/// `printkeys`: " a b c" for the keys.
pub fn printkeys(kvarr: &[Kv]) -> Vec<u8> {
    let mut buf = Vec::new();
    for kv in kvarr.iter().take_while(|kv| kv.key != 0) {
        buf.push(b' ');
        // The key is an unsigned char in the original.
        buf.push(kv.key as u8);
    }
    buf
}

impl Nnn {
    /// `lazy_parse_plug`: parse `NNN_PLUG` into `plug` the first time plugins are needed.
    pub fn lazy_parse_plug(&mut self) -> bool {
        if !self.g_state.plugparsed {
            self.g_state.plugparsed = true;
            let env = std::env::var_os("NNN_PLUG").map(crate::platform::into_bytes);
            let (ok, plug) = parsekvpair(env.as_deref());
            self.plug = plug;
            if !ok {
                self.printwait(b"NNN_PLUG", None);
                return false;
            }
        }
        true
    }

    /// `get_kv_input`: show the keys and read the user's choice.
    pub fn get_kv_input(&mut self, kvarr: &[Kv], pref: &[u8]) -> i32 {
        let mut buf = MSG_KEYS.to_vec();
        buf.extend_from_slice(pref);
        buf.extend_from_slice(&printkeys(kvarr));
        buf.truncate(CMD_LEN_MAX - 1);
        self.get_input(Some(&buf))
    }

    /// `handle_bookmark`: visit a bookmark, the mark (`,`) or the bookmarks directory
    /// (Enter). Returns 0 or the `messages[]` index to show; `newpath` gets the target.
    pub fn handle_bookmark(&mut self, bmark: Option<&[u8]>, newpath: &mut Vec<u8>) -> usize {
        let mut key = i32::from(b'\r');
        if !self.bookmark.is_empty() || bmark.is_some() {
            let bm = self.bookmark.clone();
            key = self.get_kv_input(&bm, if bmark.is_some() { b" ," } else { b"" });
        }

        let mut r = 0;
        if key == i32::from(b',') {
            // Visit marked directory
            match bmark {
                Some(b) => *newpath = cstr(b)[..cstr(b).len().min(PATH_MAX - 1)].to_vec(),
                None => r = MSG_NOT_SET,
            }
        } else if key == i32::from(b'\r') {
            // Visit bookmarks directory
            *newpath = mkpath(&self.cfgpath, TOKS[TOK_BM], &self.home).0;
            self.g_state.selbm = true;
        } else {
            match get_kv_val(&self.bookmark, key, NNN_BMS, &self.home) {
                Some(p) => *newpath = p,
                None => {
                    newpath.clear();
                    r = MSG_INVALID_KEY;
                }
            }
        }

        if r == 0 && std::env::set_current_dir(path(newpath)).is_err() {
            r = MSG_ACCESS;
            self.g_state.selbm = false;
        }
        r
    }

    /// `add_bookmark`: symlink `path` into the bookmarks directory under a name read from
    /// the user.
    pub fn add_bookmark(&mut self, p: &[u8], newpath: &mut Vec<u8>, presel: &mut i32) {
        let dir = xbasename(p).to_vec();
        let name = self.xreadline(if dir.is_empty() { None } else { Some(&dir) }, MSG_BM_NAME);
        match name {
            name if !name.is_empty() => {
                let (np, r) = mkpath(&self.cfgpath, TOKS[TOK_BM], &self.home);
                let mut buf = vec![0u8; PATH_MAX];
                buf[..np.len()].copy_from_slice(&np);
                buf[r - 1] = b'/';
                xstrsncpy(&mut buf[r..], &name, PATH_MAX - r);
                *newpath = cstr(&buf).to_vec();
                let msg = match std::os::unix::fs::symlink(path(p), path(newpath)) {
                    Err(e) => sys::strerror(&e).into_bytes(),
                    Ok(()) => newpath.clone(),
                };
                self.printwait(&msg, Some(presel));
            }
            _ => self.printwait(MSG_CANCEL, Some(presel)),
        }
    }

    /// `write_lastdir`: write "cd <curpath>" for the shell to source; exits on failure.
    pub fn write_lastdir(&mut self, curpath: &[u8], outfile: Option<&[u8]>) {
        if write_lastdir_file(curpath, outfile, &self.cfgpath, &self.home).is_err() {
            self.printerr(3207);
        }
    }

    /// `setup_config`: the config directory and its subdirectories, the plugin directory and
    /// the selection file path. Errors are printed with the original's line numbers.
    pub fn setup_config(&mut self) -> bool {
        let xdgcfg = std::env::var_os("XDG_CONFIG_HOME").map(crate::platform::into_bytes);
        let mut xdg = None;

        // Set up configuration file paths
        if let Some(x) = xdgcfg.filter(|x| !x.is_empty()) {
            let x = if tilde_is_home(&x) {
                let mut buf = vec![0u8; CMD_LEN_MAX];
                let r = xstrsncpy(&mut buf, &self.home, PATH_MAX);
                xstrsncpy(&mut buf[r - 1..], &x[1..], PATH_MAX - r);
                cstr(&buf).to_vec()
            } else {
                x
            };
            if let Err(e) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY)
                .open(path(&x))
            {
                sys::xerror(10336, &e);
                return false;
            }
            xdg = Some(x);
        }

        let mut cfgpath = match xdg {
            Some(x) => x,
            None => {
                // Create ~/.config
                let mut c = cstr(&self.home).to_vec();
                c.extend_from_slice(b"/.config");
                c
            }
        };
        // Create ~/.config/nnn
        cfgpath.extend_from_slice(b"/nnn");
        self.cfgpath = cfgpath;

        if let Err(e) = xmktree_io(&self.cfgpath, true, true) {
            sys::xerror(10372, &e);
            return false;
        }

        // Create bookmarks, sessions, mounts and plugins directories
        for tok in TOKS {
            self.plgpath = mkpath(&self.cfgpath, tok, &self.home).0;
            // The dirs are created on first run, check if they already exist
            if let Err(e) = xmktree_io(&self.plgpath, true, false) {
                sys::xerror(10382, &e);
                return false;
            }
        }

        // Set selection file path
        if !self.g_state.picker {
            let env_sel = std::env::var_os("NNN_SEL")
                .map(crate::platform::into_bytes)
                .filter(|v| !v.is_empty());
            self.selpath = Some(env_sel.unwrap_or_else(|| {
                let mut s = self.cfgpath.clone();
                s.extend_from_slice(b"/.selection");
                s
            }));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(k: u8, v: &[u8]) -> Kv {
        Kv {
            key: i32::from(k),
            val: v.to_vec(),
        }
    }

    #[test]
    fn kvpairs() {
        assert_eq!(parsekvpair(None), (true, vec![]));
        assert_eq!(
            parsekvpair(Some(b"a:x;b:/y;")),
            (true, vec![kv(b'a', b"x"), kv(b'b', b"/y")])
        );
        assert!(!parsekvpair(Some(b"a:x;;")).0);
        assert!(!parsekvpair(Some(b"a:")).0);
        assert!(!parsekvpair(Some(b"ab")).0);
        let kvs = parsekvpair(Some(b"a:/x;b:~/y;p:plug")).1;
        assert_eq!(
            get_kv_val(&kvs, i32::from(b'b'), NNN_BMS, b"/h").unwrap(),
            b"/h/y"
        );
        assert_eq!(
            get_kv_val(&kvs, i32::from(b'p'), NNN_PLUG, b"/h").unwrap(),
            b"plug"
        );
        assert_eq!(get_kv_val(&kvs, i32::from(b'z'), NNN_BMS, b"/h"), None);
        assert_eq!(get_kv_key(&kvs, b"/x", NNN_ORDER), i32::from(b'a'));
        assert_eq!(get_kv_key(&kvs, b"/x", NNN_BMS), -1);
        assert_eq!(printkeys(&kvs), b" a b p");
        let mut out = Vec::new();
        printkv(&kvs, &mut out).unwrap();
        assert_eq!(out, b" a: /x\n b: ~/y\n p: plug\n");
    }

    #[test]
    fn paths() {
        assert_eq!(mkpath(b"/", b"a", b"/h"), (b"/a".to_vec(), 3));
        assert_eq!(mkpath(b"/x", b"a", b"/h"), (b"/x/a".to_vec(), 5));
        assert_eq!(mkpath(b"/x", b"/a", b"/h"), (b"/a".to_vec(), 3));
        assert_eq!(mkpath(b"/x", b"~/a", b"/h"), (b"/h/a".to_vec(), 5));
        assert_eq!(xdirname(b"/a/b"), b"/a");
        assert_eq!(xdirname(b"/a"), b"/");
        assert_eq!(xbasename(b"/a/b"), b"b");
        assert_eq!(xbasename(b"b"), b"b");
        let mut p = b"/a/b/c".to_vec();
        assert!(common_prefix(b"/a/bd", &mut p));
        assert_eq!(p, b"/a");
        let mut p = Vec::new();
        assert!(common_prefix(b"/q", &mut p));
        assert_eq!(p, b"/q");
        assert!(!common_prefix(b"", &mut p));
        assert_eq!(get_cwd_entry(b"/a", b"/a/bc/d"), Some((3, 2)));
        assert_eq!(get_cwd_entry(b"/a", b"/b/c"), None);
        assert!(is_suffix(b"abc", b"bc"));
        assert!(!is_suffix(b"c", b"bc"));
        assert!(is_prefix(b"abc", b"ab", 2));
        assert!(is_bad_len_or_dir(b"/a/"));
    }

    #[test]
    fn escape() {
        assert_eq!(shell_escape(b"a'b", 100).unwrap(), b"'a'\\''b'");
        assert_eq!(shell_escape(b"ab", 6), None);
    }
}
