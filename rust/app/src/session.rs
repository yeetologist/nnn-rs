//! Sessions: save and load (nnn.c's \`save_session\` and \`load_session\`), with the
//! original's on-disk format: a \`session_header_t\` (\`size_t ver\` and four \`size_t[CTX_MAX]\`
//! length arrays), \`cfg\` as its 32-bit \`settings\` bit-field word, then per context its
//! \`c_cfg\` word, \`color\` and the name, last dir, filter and path bytes (x86_64/aarch64
//! Linux: little-endian, 8-byte \`size_t\`, bit-fields from the low bit).

use crate::browse::{MESSAGES, MSG_FAILED};
use crate::config::{TOKS, cstr, mkpath};
use crate::prompt::XDELAY_INTERVAL_MS;
use crate::sort::TimeType;
use crate::state::{CTX_MAX, NAME_MAX, Nnn, PATH_MAX, REGEX_MAX, Settings};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;

/// \`SESSIONS_VERSION\`
pub const SESSIONS_VERSION: u64 = 1;
/// \`messages[MSG_SEL_MISSING]\` ("open failed")
const MSG_SEL_MISSING: usize = 23;
/// \`messages[MSG_SSN_NAME]\`
const MSG_SSN_NAME: usize = 6;
/// \`toks[TOK_SSN]\`
const TOK_SSN: usize = 1;
/// \`sizeof(session_header_t)\`: \`ver\` and \`pathln\`, \`lastln\`, \`nameln\`, \`fltrln\`.
pub const HEADER_LEN: usize = 8 * (1 + 4 * CTX_MAX);

/// The \`settings\` bit-field word as GCC lays it out (bit 0 first). The reserved bits are
/// always 0: nnn never sets them and \`Settings\` has no field for them.
pub fn settings_bits(s: &Settings) -> u32 {
    let b = |v: bool, n: u32| u32::from(v) << n;
    let tt: u32 = match s.sort.timetype {
        TimeType::Access => 0,
        TimeType::Change => 1,
        TimeType::Mod => 2,
    };
    b(s.filtermode, 0)
        | b(s.sort.timeorder, 1)
        | b(s.sort.sizeorder, 2)
        | b(s.sort.apparentsz, 3)
        | b(s.sort.blkorder, 4)
        | b(s.sort.extnorder, 5)
        | b(s.sort.showhidden, 6)
        | b(s.preview, 7)
        | b(s.sort.showdetail, 8)
        | b(s.ctxactive, 9)
        | b(s.sort.reverse, 10)
        | b(s.sort.version, 11)
        | (((s.curctx & 7) as u32) << 13)
        | b(s.prefersel, 16)
        | b(s.fileinfo, 17)
        | b(s.nonavopen, 18)
        | b(s.autoenter, 19)
        | b(s.useeditor, 21)
        | b(s.fuzzy, 24)
        | b(s.regex, 25)
        | b(s.x11, 26)
        | (tt << 27)
        | b(s.cliopener, 29)
        | b(s.waitedit, 30)
        | b(s.rollover, 31)
}

/// Sets the bit-field members of \`s\` from a \`settings\` word (the fields \`Settings\` keeps
/// beside them, such as the sort function flags, are left alone). A \`timetype\` of 3,
/// which nnn never writes, reads as modification time.
pub fn set_settings_bits(s: &mut Settings, w: u32) {
    let b = |n: u32| (w >> n) & 1 != 0;
    s.filtermode = b(0);
    s.sort.timeorder = b(1);
    s.sort.sizeorder = b(2);
    s.sort.apparentsz = b(3);
    s.sort.blkorder = b(4);
    s.sort.extnorder = b(5);
    s.sort.showhidden = b(6);
    s.preview = b(7);
    s.sort.showdetail = b(8);
    s.ctxactive = b(9);
    s.sort.reverse = b(10);
    s.sort.version = b(11);
    s.curctx = ((w >> 13) & 7) as usize;
    s.prefersel = b(16);
    s.fileinfo = b(17);
    s.nonavopen = b(18);
    s.autoenter = b(19);
    s.useeditor = b(21);
    s.fuzzy = b(24);
    s.regex = b(25);
    s.x11 = b(26);
    s.sort.timetype = match (w >> 27) & 3 {
        0 => TimeType::Access,
        1 => TimeType::Change,
        _ => TimeType::Mod,
    };
    s.cliopener = b(29);
    s.waitedit = b(30);
    s.rollover = b(31);
}

/// A C string buffer of at least \`cap\` bytes holding \`s\`, NUL-padded.
fn cbuf(s: &[u8], cap: usize) -> Vec<u8> {
    let mut v = s.to_vec();
    v.resize(cap.max(s.len() + 1), 0);
    v
}

/// \`read(fd, buf, n)\` from a regular file: copies what is left (at most \`n\` bytes) over the
/// start of \`buf\` and says whether all \`n\` were there.
fn read_into(data: &[u8], pos: &mut usize, buf: &mut [u8], n: usize) -> bool {
    let k = n.min(data.len() - *pos);
    buf[..k].copy_from_slice(&data[*pos..*pos + k]);
    *pos += k;
    k == n
}

/// Reads \`n\` bytes into the C string \`s\` (a \`cap\`-byte buffer): the bytes overwrite its
/// start and the string is what is then up to the first NUL.
fn read_cstr(data: &[u8], pos: &mut usize, s: &mut Vec<u8>, cap: usize, n: usize) -> bool {
    let mut buf = cbuf(s, cap.max(n));
    let ok = read_into(data, pos, &mut buf, n);
    *s = cstr(&buf).to_vec();
    ok
}

/// Reads a \`settings\` word over \`s\` (a short read changes only the bytes it got).
fn read_settings(data: &[u8], pos: &mut usize, s: &mut Settings) -> bool {
    let mut w = settings_bits(s).to_le_bytes();
    let ok = read_into(data, pos, &mut w, 4);
    set_settings_bits(s, u32::from_le_bytes(w));
    ok
}

impl Nnn {
    /// The path of session \`sname\`: \`mkpath(cfgpath, toks[TOK_SSN])\`, then \`sname\` in it.
    fn ssn_path(&self, sname: &[u8]) -> Vec<u8> {
        let (ssnpath, _) = mkpath(&self.cfgpath, TOKS[TOK_SSN], &self.home);
        mkpath(&ssnpath, sname, &self.home).0
    }

    /// The bytes \`save_session\` writes for the current state (the hovered name already
    /// copied to the current context).
    pub fn session_bytes(&self) -> Vec<u8> {
        let mut pathln = [0u64; CTX_MAX];
        let mut lastln = [0u64; CTX_MAX];
        let mut nameln = [0u64; CTX_MAX];
        let mut fltrln = [0u64; CTX_MAX];
        for (i, c) in self.g_ctx.iter().enumerate() {
            if c.c_cfg.ctxactive {
                pathln[i] = (cstr(&c.c_path).len().min(PATH_MAX) + 1) as u64;
                lastln[i] = (cstr(&c.c_last).len().min(PATH_MAX) + 1) as u64;
                nameln[i] = (cstr(&c.c_name).len().min(NAME_MAX) + 1) as u64;
                fltrln[i] = REGEX_MAX as u64;
            }
        }
        let mut out = Vec::with_capacity(HEADER_LEN + 4);
        out.extend_from_slice(&SESSIONS_VERSION.to_le_bytes());
        for a in [&pathln, &lastln, &nameln, &fltrln] {
            for v in a {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.extend_from_slice(&settings_bits(&self.cfg).to_le_bytes());
        for (i, c) in self.g_ctx.iter().enumerate() {
            out.extend_from_slice(&settings_bits(&c.c_cfg).to_le_bytes());
            out.extend_from_slice(&c.color.to_le_bytes());
            for (s, n) in [
                (&c.c_name, nameln[i]),
                (&c.c_last, lastln[i]),
                (&c.c_fltr, fltrln[i]),
                (&c.c_path, pathln[i]),
            ] {
                let n = n as usize;
                out.extend_from_slice(&cbuf(s, n)[..n]);
            }
        }
        out
    }

    /// Ports \`save_session\`: writes every context to session \`sname\` (mode 0600); "open
    /// failed" or "failed!" on errors (kept up with \`presel\`).
    pub fn save_session(&mut self, sname: &[u8], presel: Option<&mut i32>) {
        let cc = self.cfg.curctx;
        if self.g_ctx[cc].c_cfg.ctxactive && self.ndents != 0 {
            // Update current file name, arrows don't update it
            if let Some(e) = self.pdents.get(self.cur as usize) {
                let n = cstr(&e.name);
                self.g_ctx[cc].c_name = n[..n.len().min(NAME_MAX)].to_vec();
            }
        }
        let bytes = self.session_bytes();
        let spath = self.ssn_path(sname);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(std::ffi::OsStr::from_bytes(&spath));
        let Ok(mut f) = f else {
            self.printwait(MESSAGES[MSG_SEL_MISSING], presel);
            return;
        };
        if f.write_all(&bytes).is_err() {
            self.printwait(MESSAGES[MSG_FAILED], presel);
        }
    }

    /// Ports \`load_session\`: loads session \`sname\` (asked for when \`None\` or empty), or
    /// the "@" session that \`restore\` deletes after loading. A load asked for from the
    /// session key first saves the current state as "@" (unless that is what is loaded).
    /// On success the current context is the session's and \`curssn\` is its name.
    pub fn load_session(&mut self, sname: Option<&[u8]>, restore: bool) -> bool {
        let mut has_loaded_dynamically = !(sname.is_some() || restore);
        // Picker mode with session program option
        let picker_ssn = sname.is_some() && self.g_state.picker;
        let name: Vec<u8>;
        if !restore {
            name = match sname {
                Some(s) if !cstr(s).is_empty() => cstr(s).to_vec(),
                _ => self.xreadline(None, MESSAGES[MSG_SSN_NAME]),
            };
            if name.is_empty() {
                return false;
            }
            // If user is explicitly loading the "last session", skip auto-save
            if name == b"@" {
                has_loaded_dynamically = false;
            }
        } else {
            name = b"@".to_vec();
        }
        let spath = self.ssn_path(&name);

        if has_loaded_dynamically {
            self.save_session(b"@", None);
        }

        let Ok(mut f) = std::fs::File::open(std::ffi::OsStr::from_bytes(&spath)) else {
            if !picker_ssn {
                self.printmsg(MESSAGES[MSG_SEL_MISSING]);
                self.xdelay(XDELAY_INTERVAL_MS);
            }
            return false;
        };
        // A read error (a directory, say) fails the first read, as in the original.
        let mut data = Vec::new();
        if f.read_to_end(&mut data).is_err() {
            data.clear();
        }

        let status = self.load_session_data(&data);
        if status {
            // Set correct sort and filter options
            self.set_sort_flags(0);
            let curssn = if restore {
                sname.map_or(&b"@"[..], cstr)
            } else {
                &name[..]
            };
            self.popup.curssn = curssn[..curssn.len().min(NAME_MAX - 1)].to_vec();
        }

        if !status {
            self.printmsg(MESSAGES[MSG_FAILED]);
            self.xdelay(XDELAY_INTERVAL_MS);
        } else if restore {
            crate::sys::unlink(&spath);
        }
        status
    }

    /// The reads of \`load_session\`, applied to the state as the original's \`read()\`s are,
    /// so a broken file leaves what was read before the error.
    fn load_session_data(&mut self, data: &[u8]) -> bool {
        let mut pos = 0;
        let mut hb = [0u8; HEADER_LEN];
        if !read_into(data, &mut pos, &mut hb, HEADER_LEN) {
            return false;
        }
        let h = |k: usize| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&hb[k * 8..k * 8 + 8]);
            u64::from_le_bytes(w)
        };
        if h(0) != SESSIONS_VERSION {
            return false;
        }
        let mut cfg = self.cfg.clone();
        let ok = read_settings(data, &mut pos, &mut cfg);
        self.cfg = cfg;
        if !ok {
            return false;
        }

        let c = &mut self.g_ctx[self.cfg.curctx];
        c.c_name.clear();
        c.c_last.clear();
        if c.c_fltr.len() < 2 {
            c.c_fltr.resize(REGEX_MAX, 0);
        }
        c.c_fltr[0] = 0;
        c.c_fltr[1] = 0;

        for (i, c) in self.g_ctx.iter_mut().enumerate() {
            let (pathln, lastln) = (h(1 + i), h(1 + CTX_MAX + i));
            let (nameln, fltrln) = (h(1 + 2 * CTX_MAX + i), h(1 + 3 * CTX_MAX + i));
            if nameln > (NAME_MAX + 1) as u64
                || lastln > PATH_MAX as u64
                || fltrln > REGEX_MAX as u64
                || pathln > PATH_MAX as u64
            {
                return false;
            }
            if !read_settings(data, &mut pos, &mut c.c_cfg) {
                return false;
            }
            let mut color = c.color.to_le_bytes();
            let ok = read_into(data, &mut pos, &mut color, 4);
            c.color = u32::from_le_bytes(color);
            if !ok
                || !read_cstr(data, &mut pos, &mut c.c_name, NAME_MAX + 1, nameln as usize)
                || !read_cstr(data, &mut pos, &mut c.c_last, PATH_MAX, lastln as usize)
            {
                return false;
            }
            if c.c_fltr.len() < REGEX_MAX {
                c.c_fltr.resize(REGEX_MAX, 0);
            }
            if !read_into(data, &mut pos, &mut c.c_fltr, fltrln as usize)
                || !read_cstr(data, &mut pos, &mut c.c_path, PATH_MAX, pathln as usize)
            {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_bits_round_trip() {
        for w in [
            0u32,
            !((1 << 12) | (1 << 20) | (3 << 22) | (1 << 28)),
            0x8000_0200,
            0x1000_e000,
        ] {
            let mut s = Settings::default();
            set_settings_bits(&mut s, w);
            assert_eq!(settings_bits(&s), w, "{w:08x}");
        }
        // nnn's initial cfg: ctxactive, autoenter, timetype 2 (modification), rollover.
        assert_eq!(
            settings_bits(&Settings::default()),
            (1 << 9) | (1 << 19) | (2 << 27) | (1 << 31)
        );
    }

    #[test]
    fn save_layout_and_load_back() {
        let mut n = Nnn::default();
        n.cfg.curctx = 1;
        n.g_ctx[1].c_cfg = n.cfg.clone();
        n.g_ctx[1].c_path = b"/tmp".to_vec();
        n.g_ctx[1].c_name = b"x".to_vec();
        n.g_ctx[1].color = 4;
        let b = n.session_bytes();
        // header, cfg, 8 x (c_cfg + color), then ctx 1's name, last, filter and path.
        assert_eq!(b.len(), HEADER_LEN + 4 + 8 * 8 + 2 + 1 + REGEX_MAX + 5);
        assert_eq!(&b[..8], &1u64.to_le_bytes());
        assert_eq!(&b[8 + 8..16 + 8], &5u64.to_le_bytes()); // pathln[1]
        let at = HEADER_LEN + 4 + 2 * 8 + 2 + 1 + REGEX_MAX;
        assert_eq!(&b[at..at + 5], b"/tmp\0");

        let mut m = Nnn::default();
        assert!(m.load_session_data(&b));
        assert_eq!(m.cfg.curctx, 1);
        assert!(m.g_ctx[1].c_cfg.ctxactive);
        assert_eq!(m.g_ctx[1].c_path, b"/tmp");
        assert_eq!(m.g_ctx[1].c_name, b"x");
        assert_eq!(m.g_ctx[1].color, 4);
        assert_eq!(m.session_bytes(), b);

        // A wrong version, a truncated file and an oversized length all fail.
        let mut bad = b.clone();
        bad[0] = 2;
        assert!(!Nnn::default().load_session_data(&bad));
        assert!(!Nnn::default().load_session_data(&b[..b.len() - 1]));
        let mut big = b.clone();
        big[8 + 64 * 2 + 8..8 + 64 * 2 + 16].copy_from_slice(&257u64.to_le_bytes());
        assert!(!Nnn::default().load_session_data(&big));
    }
}
