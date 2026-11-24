//! nnn's command line: `parse_opts`, `parse_env_opts` and `usage` (src/nnn.c).

use crate::getopt::{Getopt, Opt};
use crate::keys;
use crate::sys;
use std::io::Write;

pub const VERSION: &str = "5.3";
const OPTSTRING: &[u8] = b"aAb:BcCdDeEfF:gHiJKl:nNop:P:QrRs:St:T:uUVxz0h";
const NAME_MAX: usize = 255;

/// Everything the options set (`cfg`, `g_state` and `struct optstate` in the original).
#[derive(Debug, Default, Clone)]
pub struct Opts {
    pub autofifo: bool,
    pub no_autoenter: bool,
    pub bookmark: Option<Vec<u8>>,
    pub usebsdtar: bool,
    pub cliopener: bool,
    pub oldcolor: bool,
    pub showdetail: bool,
    pub dirctx: bool,
    pub useeditor: bool,
    pub waitedit: bool,
    pub hist_file: bool,
    pub fifomode: i32,
    pub regex: bool,
    pub fuzzy: bool,
    pub showhidden: bool,
    pub fileinfo: bool,
    pub stayonsel: bool,
    pub scroll_lines: Option<i32>,
    pub filtermode: bool,
    pub nonavopen: bool,
    pub picker: bool,
    /// Absolute path of the picker's selection file; `None` with `picker` means stdout.
    pub selpath: Option<Vec<u8>>,
    pub pkey: Option<u8>,
    pub forcequit: bool,
    pub progress_cpmv: bool,
    pub no_rollover: bool,
    pub session: Vec<u8>,
    pub persistent_session: bool,
    pub idletimeout: Option<i32>,
    pub sort: Option<u8>,
    pub prefersel: bool,
    pub uidgid: bool,
    pub x11: bool,
    pub sepnul: bool,
}

/// Why parsing stopped early: the process exits with this code.
pub type Exit = u8;

/// glibc `atoi`: `(int) strtol(s, NULL, 10)`, saturating at the `long` range first.
pub fn atoi(s: &[u8]) -> i32 {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || (b'\t'..=b'\r').contains(&s[i])) {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut v: i128 = 0;
    while let Some(d) = s.get(i).filter(|d| d.is_ascii_digit()) {
        v = (v * 10 + i128::from(d - b'0')).min(i128::from(i64::MAX) + 1);
        i += 1;
    }
    let v = if neg { -v } else { v };
    let long = v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    long as i32
}

pub fn usage() {
    let mut out = std::io::stdout();
    let _ = out.write_all(USAGE);
    let _ = out.flush();
}

/// `parse_opts`: `Some(code)` when nnn exits right away.
pub fn parse_opts(argv: &[Vec<u8>], st: &mut Opts) -> (Option<Exit>, Vec<Vec<u8>>) {
    let mut g = Getopt::new(argv, OPTSTRING);
    while let Some(opt) = g.next_opt() {
        let Opt::Flag(c, arg) = opt else {
            usage();
            return (Some(1), Vec::new());
        };
        let arg = arg.unwrap_or_default();
        match c {
            b'a' => st.autofifo = true,
            b'A' => st.no_autoenter = true,
            b'b' => st.bookmark = Some(arg),
            b'B' => st.usebsdtar = true,
            b'c' => st.cliopener = true,
            b'C' => st.oldcolor = true,
            b'd' => st.showdetail = true,
            b'D' => st.dirctx = true,
            b'e' => st.useeditor = true,
            b'E' => st.waitedit = true,
            b'f' => st.hist_file = true,
            b'F' => {
                let fd = atoi(&arg);
                if !(0..=1).contains(&fd) {
                    return (Some(1), Vec::new());
                }
                st.fifomode = fd;
            }
            b'g' => {
                if st.fuzzy {
                    return (Some(1), Vec::new());
                }
                st.regex = true;
            }
            b'H' => st.showhidden = true,
            b'i' => st.fileinfo = true,
            b'J' => st.stayonsel = true,
            b'K' => {
                keys::check_key_collision();
                return (Some(0), Vec::new());
            }
            b'l' => st.scroll_lines = Some(atoi(&arg)),
            b'n' => st.filtermode = true,
            b'o' => st.nonavopen = true,
            b'p' => {
                st.picker = true;
                if arg.as_slice() != b"-" {
                    if let Err(e) = sys::create_file(&arg) {
                        sys::xerror(10567, &e);
                        return (Some(1), Vec::new());
                    }
                    let Some(path) = sys::abspath_in(&arg, None, None) else {
                        sys::xerror(10575, &std::io::Error::last_os_error());
                        return (Some(1), Vec::new());
                    };
                    sys::unlink(&path);
                    st.selpath = Some(path);
                }
            }
            b'P' => {
                if arg.len() == 1 {
                    st.pkey = Some(arg[0]);
                }
            }
            b'Q' => st.forcequit = true,
            b'r' => st.progress_cpmv = true,
            b'R' => st.no_rollover = true,
            b's' => st.session = arg.iter().copied().take(NAME_MAX - 1).collect(),
            b'S' => {
                st.persistent_session = true;
                if st.session.is_empty() {
                    st.session = b"@".to_vec();
                }
            }
            b't' => st.idletimeout = Some(atoi(&arg)),
            b'T' => st.sort = Some(arg.first().copied().unwrap_or(0)),
            b'u' => st.prefersel = true,
            b'U' => st.uidgid = true,
            b'V' => {
                println!("{VERSION}");
                return (Some(0), Vec::new());
            }
            b'x' => st.x11 = true,
            b'z' => {
                if st.regex {
                    return (Some(1), Vec::new());
                }
                st.fuzzy = true;
            }
            b'0' => st.sepnul = true,
            b'h' => {
                usage();
                return (Some(0), Vec::new());
            }
            // In the option string but not built: `-N` (readline) in the default build.
            _ => {
                usage();
                return (Some(1), Vec::new());
            }
        }
    }
    (None, g.operands().into_iter().map(<[u8]>::to_vec).collect())
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// `parse_env_opts`: NNN_OPTS split on spaces, a `-` added to the first word if missing.
pub fn parse_env_opts(env: &[u8], st: &mut Opts) -> Option<Exit> {
    let mut av = vec![b"nnn".to_vec()];
    for word in env.split(|&c| is_space(c)).filter(|w| !w.is_empty()) {
        let mut w = Vec::with_capacity(word.len() + 1);
        if av.len() == 1 && word[0] != b'-' {
            w.push(b'-');
        }
        w.extend_from_slice(word);
        av.push(w);
    }
    if av.len() == 1 {
        return None;
    }
    let (exit, operands) = parse_opts(&av, st);
    if exit.is_some() {
        return exit;
    }
    if let Some(word) = operands.first() {
        let mut line = b"nnn: NNN_OPTS: unexpected '".to_vec();
        line.extend_from_slice(word);
        line.extend_from_slice(b"'\n");
        let _ = std::io::stderr().write_all(&line);
        return Some(1);
    }
    None
}

const USAGE: &[u8] = br#"usage: nnn [OPTIONS] [PATH]

The unorthodox terminal file manager.

positional args:
  PATH   start dir/file [default: .]

optional args:
 -a      auto NNN_FIFO
 -A      no dir auto-enter during filter
 -b key  open bookmark key (trumps -s/S)
 -B      use bsdtar for archives
 -c      cli-only NNN_OPENER (trumps -e)
 -C      8-color scheme
 -d      detail mode
 -D      dirs in context color
 -e      text in $VISUAL/$EDITOR/vi
 -E      internal edits in EDITOR
 -f      use history file
 -F val  fifo mode [0:preview 1:explore]
 -g      regex filters
 -H      show hidden files
 -i      show current file info
 -J      no auto-advance on selection
 -K      detect key collision and exit
 -l val  set scroll lines
 -n      type-to-nav mode
 -o      open files only on Enter
 -p file selection file [-:stdout]
 -P key  run plugin key
 -Q      no quit confirmation
 -r      use advcpmv patched cp, mv
 -R      no rollover at edges
 -s name load session by name
 -S      persistent session
 -t secs timeout to lock
 -T key  sort order [a/d/e/r/s/t/v]
 -u      use selection (no prompt)
 -U      show user and group
 -V      show version
 -x      notis, selection sync, xterm title
 -z      in order fuzzy filters
 -0      null separator in picker mode
 -h      show help

v5.3
BSD 2-Clause
https://github.com/jarun/nnn
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_matches_glibc() {
        assert_eq!(atoi(b"2147483648"), i32::MIN);
        assert_eq!(atoi(b"9007199254740993"), 1);
        assert_eq!(atoi(b"9223372036854775807"), -1);
        assert_eq!(atoi(b"-9223372036854775808"), 0);
        assert_eq!(atoi(b"18446744073709551616"), -1);
        assert_eq!(atoi(b" +1x"), 1);
        assert_eq!(atoi(b"abc"), 0);
    }
}
