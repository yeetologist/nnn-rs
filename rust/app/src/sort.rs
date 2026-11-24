//! Sorting and filtering (src/nnn.c: xstricmp, xstrverscasecmp, entrycmp, the fuzzy
//! matchers, visible_*, set_sort_flags, getorderstr).

use crate::locale::{self, Wide};
use std::cmp::Ordering;

const NAME_MAX: usize = 255;

pub const DIR_OR_DIRLNK: u8 = 0x01;
pub const HARD_LINK: u8 = 0x02;
pub const SYM_ORPHAN: u8 = 0x04;
pub const FILE_MISSING: u8 = 0x08;
pub const FILE_SELECTED: u8 = 0x10;
pub const FILE_SCANNED: u8 = 0x20;
pub const FILE_YOUNG: u8 = 0x40;

/// A directory entry (`struct entry`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub sec: i64,
    pub nsec: u32,
    pub mode: u32,
    pub size: i64,
    pub blocks: u64,
    pub flags: u8,
    pub uid: u32,
    pub gid: u32,
}

impl Entry {
    pub fn is_dir_or_dirlnk(&self) -> bool {
        self.flags & DIR_OR_DIRLNK != 0
    }
}

/// When times are compared and shown (`cfg.timetype`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeType {
    Access,
    Change,
    #[default]
    Mod,
}

/// The sort part of `cfg` plus the two comparator pointers it keeps in sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortCfg {
    pub timeorder: bool,
    pub sizeorder: bool,
    pub apparentsz: bool,
    pub blkorder: bool,
    pub extnorder: bool,
    pub reverse: bool,
    pub version: bool,
    pub showhidden: bool,
    pub showdetail: bool,
    pub timetype: TimeType,
    /// `entrycmpfn == &reventrycmp`
    pub rev_fn: bool,
    /// `namecmpfn == &xstrverscasecmp`
    pub vers_fn: bool,
    pub blk_shift: u32,
}

fn sign(v: i32) -> Ordering {
    v.cmp(&0)
}

/// glibc `strtoll(s, &end, 10)`: the value and whether any digit was read.
fn strtoll(s: &[u8]) -> (i64, bool) {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || (b'\t'..=b'\r').contains(&s[i])) {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let start = i;
    let mut v: i128 = 0;
    while let Some(d) = s.get(i).filter(|d| d.is_ascii_digit()) {
        v = (v * 10 + i128::from(d - b'0')).min(i128::from(i64::MAX) + 1);
        i += 1;
    }
    if i == start {
        return (0, false);
    }
    let v = if neg { -v } else { v };
    (
        v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
        true,
    )
}

/// `xstricmp`: numbers first, by value, then `strcoll`.
pub fn xstricmp(s1: &[u8], s2: &[u8]) -> Ordering {
    let (v1, n1) = strtoll(s1);
    let (v2, n2) = strtoll(s2);
    if n1 || n2 {
        if n1 && n2 {
            if v2 > v1 {
                return Ordering::Less;
            }
            if v1 > v2 {
                return Ordering::Greater;
            }
        }
        if !n1 {
            return Ordering::Greater;
        }
        if !n2 {
            return Ordering::Less;
        }
    }
    sign(locale::strcoll(s1, s2))
}

const S_N: usize = 0x0;
const S_I: usize = 0x3;
const S_F: usize = 0x6;
const S_Z: usize = 0x9;
const VCMP: i8 = 2;
const VLEN: i8 = 3;
const NEXT_STATE: [usize; 12] = [S_N, S_I, S_Z, S_N, S_I, S_I, S_N, S_F, S_F, S_N, S_F, S_Z];
#[rustfmt::skip]
const RESULT_TYPE: [i8; 36] = [
    VCMP, VCMP, VCMP, VCMP, VLEN, VCMP, VCMP, VCMP, VCMP,
    VCMP,   -1,   -1,    1, VLEN, VLEN,    1, VLEN, VLEN,
    VCMP, VCMP, VCMP, VCMP, VCMP, VCMP, VCMP, VCMP, VCMP,
    VCMP,    1,    1,   -1, VCMP, VCMP,   -1, VCMP, VCMP,
];

fn upper(c: u8) -> u8 {
    c.to_ascii_uppercase()
}

fn class(c: u8) -> usize {
    usize::from(c == b'0') + usize::from(c.is_ascii_digit())
}

/// `xstrverscasecmp`: glibc `strverscmp`, ASCII case folded.
pub fn xstrverscasecmp(s1: &[u8], s2: &[u8]) -> Ordering {
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let (mut p1, mut p2) = (0, 0);
    let mut c1 = upper(at(s1, p1));
    p1 += 1;
    let mut c2 = upper(at(s2, p2));
    p2 += 1;
    let mut state = S_N + class(c1);
    let mut diff;
    loop {
        diff = i32::from(c1) - i32::from(c2);
        if diff != 0 {
            break;
        }
        if c1 == 0 {
            return Ordering::Equal;
        }
        state = NEXT_STATE[state];
        c1 = upper(at(s1, p1));
        p1 += 1;
        c2 = upper(at(s2, p2));
        p2 += 1;
        state += class(c1);
    }
    match RESULT_TYPE[state * 3 + class(c2)] {
        VCMP => sign(diff),
        VLEN => {
            loop {
                let d1 = at(s1, p1);
                p1 += 1;
                if !d1.is_ascii_digit() {
                    break;
                }
                let d2 = at(s2, p2);
                p2 += 1;
                if !d2.is_ascii_digit() {
                    return Ordering::Greater;
                }
            }
            if at(s2, p2).is_ascii_digit() {
                Ordering::Less
            } else {
                sign(diff)
            }
        }
        r => sign(i32::from(r)),
    }
}

impl SortCfg {
    pub fn namecmp(&self, a: &[u8], b: &[u8]) -> Ordering {
        if self.vers_fn {
            xstrverscasecmp(a, b)
        } else {
            xstricmp(a, b)
        }
    }

    /// `entrycmp`
    pub fn entrycmp(&self, pa: &Entry, pb: &Entry) -> Ordering {
        if pb.is_dir_or_dirlnk() != pa.is_dir_or_dirlnk() {
            return if pb.is_dir_or_dirlnk() {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        if self.timeorder {
            let o = pb.sec.cmp(&pa.sec).then(pb.nsec.cmp(&pa.nsec));
            if o != Ordering::Equal {
                return o;
            }
        } else if self.sizeorder {
            let o = pb.size.cmp(&pa.size);
            if o != Ordering::Equal {
                return o;
            }
        } else if self.blkorder {
            let o = pb.blocks.cmp(&pa.blocks);
            if o != Ordering::Equal {
                return o;
            }
        } else if self.extnorder && !pb.is_dir_or_dirlnk() {
            fn ext(n: &[u8]) -> Option<&[u8]> {
                n.iter().rposition(|&c| c == b'.').map(|i| &n[i..])
            }
            match (ext(&pa.name), ext(&pb.name)) {
                (None, None) => {}
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(a), Some(b)) => {
                    let r = locale::strcasecmp(a, b);
                    if r != 0 {
                        return sign(r);
                    }
                }
            }
        }
        self.namecmp(&pa.name, &pb.name)
    }

    /// `entrycmpfn`: `entrycmp` or `reventrycmp`.
    pub fn cmp(&self, pa: &Entry, pb: &Entry) -> Ordering {
        if !self.rev_fn {
            return self.entrycmp(pa, pb);
        }
        if pb.is_dir_or_dirlnk() != pa.is_dir_or_dirlnk() {
            return if pb.is_dir_or_dirlnk() {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        self.entrycmp(pa, pb).reverse()
    }

    /// `fuzzyentrycmp`, while a fuzzy filter is typed.
    pub fn fuzzyentrycmp(&self, filter: &[u8], ic: bool, pa: &Entry, pb: &Entry) -> Ordering {
        if pb.is_dir_or_dirlnk() != pa.is_dir_or_dirlnk() {
            return if pb.is_dir_or_dirlnk() {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        let sa = fuzzy_match_score(filter, &pa.name, ic);
        let sb = fuzzy_match_score(filter, &pb.name, ic);
        if sa != sb {
            let o = if sa < sb {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            return if self.reverse { o.reverse() } else { o };
        }
        let o = self.namecmp(&pa.name, &pb.name);
        if self.reverse { o.reverse() } else { o }
    }

    /// Sort `entries` by `fuzzyentrycmp`, computing each entry's fuzzy score once in O(N).
    pub fn sort_fuzzy(&self, entries: &mut [Entry], filter: &[u8], ic: bool) {
        let mut order: Vec<(usize, i32)> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (i, fuzzy_match_score(filter, &e.name, ic)))
            .collect();
        order.sort_by(|&(ia, sa), &(ib, sb)| {
            let (pa, pb) = (&entries[ia], &entries[ib]);
            if pb.is_dir_or_dirlnk() != pa.is_dir_or_dirlnk() {
                return if pb.is_dir_or_dirlnk() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            if sa != sb {
                let o = sa.cmp(&sb);
                return if self.reverse { o.reverse() } else { o };
            }
            let o = self.namecmp(&pa.name, &pb.name);
            if self.reverse { o.reverse() } else { o }
        });
        for i in 0..order.len() {
            let mut cur = i;
            while order[cur].0 != i {
                let next = order[cur].0;
                entries.swap(cur, next);
                order[cur].0 = cur;
                cur = next;
            }
            order[cur].0 = cur;
        }
    }

    fn reset_sort_flags(&mut self) {
        self.timeorder = false;
        self.sizeorder = false;
        self.apparentsz = false;
        self.blkorder = false;
        self.extnorder = false;
    }

    /// `set_sort_flags`: the key's code (0 when it is not a sort key), and whether the
    /// selection must end because the directory is reloaded in disk-usage mode.
    pub fn set_sort_flags(&mut self, key: u8) -> (u8, bool) {
        let mut r = key;
        let session = r == 0;
        let mut reverse = false;
        let mut reload = false;
        if r.is_ascii_uppercase() && r != b'R' && r != b'C' {
            reverse = true;
            r = r.to_ascii_lowercase();
        }
        if session {
            if self.apparentsz {
                self.apparentsz = false;
                r = b'a';
            } else if self.blkorder {
                self.blkorder = false;
                r = b'd';
            }
            self.rev_fn = self.reverse;
            self.vers_fn = self.version;
        } else if r == 0x14 {
            r = if self.timeorder {
                b's'
            } else if self.sizeorder {
                b'c'
            } else {
                b't'
            };
        }
        match r {
            b'a' | b'd' => {
                if r == b'a' {
                    self.apparentsz = !self.apparentsz;
                    if self.apparentsz {
                        self.blkorder = true;
                        self.blk_shift = 0;
                    } else {
                        self.blkorder = false;
                    }
                } else {
                    if !self.apparentsz {
                        self.blkorder = !self.blkorder;
                    }
                    self.apparentsz = false;
                    self.blk_shift = 512u32.trailing_zeros();
                }
                if self.blkorder {
                    self.showdetail = true;
                }
                self.timeorder = false;
                self.sizeorder = false;
                self.extnorder = false;
                if !session {
                    self.reverse = false;
                    self.rev_fn = false;
                }
                reload = true;
            }
            b'c' => {
                self.reset_sort_flags();
                self.reverse = false;
                self.version = false;
                self.rev_fn = false;
                self.vers_fn = false;
            }
            b'e' | b's' | b't' => {
                let val = !match r {
                    b'e' => self.extnorder,
                    b's' => self.sizeorder,
                    _ => self.timeorder,
                };
                self.reset_sort_flags();
                match r {
                    b'e' => self.extnorder = val,
                    b's' => self.sizeorder = val,
                    _ => self.timeorder = val,
                }
                self.reverse = false;
                self.rev_fn = false;
            }
            b'r' => {
                self.reverse = !self.reverse;
                self.rev_fn = self.reverse;
            }
            b'v' => {
                self.version = !self.version;
                self.vers_fn = self.version;
                self.reset_sort_flags();
            }
            _ => return (0, false),
        }
        if reverse {
            self.reverse = true;
            self.rev_fn = true;
        }
        (r, reload)
    }

    /// `getorderstr`: the sort letters shown in the status bar, with a trailing space.
    pub fn getorderstr(&self) -> String {
        let mut s = String::new();
        if self.showhidden {
            s.push('H');
        }
        if self.timeorder {
            s.push(match self.timetype {
                TimeType::Mod => 'M',
                TimeType::Access => 'A',
                TimeType::Change => 'C',
            });
        } else if self.sizeorder {
            s.push('S');
        } else if self.extnorder {
            s.push('E');
        }
        if self.rev_fn {
            s.push('R');
        }
        if self.vers_fn {
            s.push('V');
        }
        if !s.is_empty() {
            s.push(' ');
        }
        s
    }
}

fn normalize_char(c: i32) -> i32 {
    if c == b' ' as i32 || c == b'_' as i32 || c == b'-' as i32 {
        b' ' as i32
    } else {
        c
    }
}

enum Prep {
    Error,
    Empty,
    Ok(Wide, Wide),
}

/// `wcs_prep_filter`
fn wcs_prep(filter: &[u8], fname: &[u8], ic: bool) -> Prep {
    let Some(mut f) = locale::mbstowcs(filter, NAME_MAX - 1) else {
        return Prep::Error;
    };
    if f.is_empty() {
        return Prep::Empty;
    }
    let Some(mut n) = locale::mbstowcs(fname, NAME_MAX - 1) else {
        return Prep::Error;
    };
    if ic {
        f.iter_mut().for_each(|c| *c = locale::to_lower_wide(*c));
        n.iter_mut().for_each(|c| *c = locale::to_lower_wide(*c));
    }
    Prep::Ok(f, n)
}

/// Where each filter character matched, in order (`None`: not all of them did).
fn fuzzy_positions(f: &[i32], n: &[i32]) -> Option<Vec<usize>> {
    let mut pos = Vec::with_capacity(f.len());
    let mut fi = 0;
    for (ni, &c) in n.iter().enumerate() {
        if fi == f.len() || f[fi] == 0 {
            break;
        }
        if normalize_char(f[fi]) == normalize_char(c) {
            pos.push(ni);
            fi += 1;
        }
    }
    (fi == f.len()).then_some(pos)
}

/// `fuzzy_match`
pub fn fuzzy_match(filter: &[u8], fname: &[u8], ic: bool) -> bool {
    match wcs_prep(filter, fname, ic) {
        Prep::Error => false,
        Prep::Empty => true,
        Prep::Ok(f, n) => fuzzy_positions(&f, &n).is_some(),
    }
}

/// `fuzzy_match_score`: lower is better, `i32::MAX` for no match.
pub fn fuzzy_match_score(filter: &[u8], fname: &[u8], ic: bool) -> i32 {
    let (f, n) = match wcs_prep(filter, fname, ic) {
        Prep::Error => return i32::MAX,
        Prep::Empty => return 0,
        Prep::Ok(f, n) => (f, n),
    };
    let Some(pos) = fuzzy_positions(&f, &n) else {
        return i32::MAX;
    };
    let (first, endpos) = (pos[0], pos[pos.len() - 1]);
    let span = endpos - first + 1;
    let gaps = span - f.len();
    let consec = pos.windows(2).filter(|w| w[1] == w[0] + 1).count();
    let word_starts = pos
        .iter()
        .filter(|&&p| p == 0 || normalize_char(n[p - 1]) == b' ' as i32)
        .count();
    (gaps * 1000 + first * 100 + (span - consec) * 10).wrapping_sub(word_starts * 5) as i32
}

/// `fuzzy_match_positions`: wide-character indexes of the name that are highlighted.
pub fn fuzzy_match_positions(filter: &[u8], fname: &[u8], ic: bool) -> Vec<usize> {
    let Prep::Ok(f, n) = wcs_prep(filter, fname, ic) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut fi = 0;
    for (ni, &c) in n.iter().enumerate() {
        if fi >= f.len() {
            break;
        }
        if normalize_char(f[fi]) == normalize_char(c) {
            out.push(ni);
            fi += 1;
        }
    }
    out
}

/// glibc `strcasestr` (`ic`) or `strstr`: the byte offset of the first match.
pub fn find(hay: &[u8], needle: &[u8], ic: bool) -> Option<usize> {
    let hay = &hay[..hay.iter().position(|&c| c == 0).unwrap_or(hay.len())];
    let needle = &needle[..needle.iter().position(|&c| c == 0).unwrap_or(needle.len())];
    if needle.is_empty() {
        return Some(0);
    }
    let eq = |a: u8, b: u8| {
        if ic {
            locale::tolower(a) == locale::tolower(b)
        } else {
            a == b
        }
    };
    (0..hay.len().saturating_sub(needle.len() - 1))
        .find(|&i| needle.iter().zip(&hay[i..]).all(|(&n, &h)| eq(n, h)))
}

/// `visible_str`
pub fn visible_str(filter: &[u8], fname: &[u8], ic: bool) -> bool {
    find(fname, filter, ic).is_some()
}

/// `string_match_positions`
pub fn string_match_positions(filter: &[u8], fname: &[u8], ic: bool) -> Vec<usize> {
    if filter.is_empty() {
        return Vec::new();
    }
    let Some(pos) = find(fname, filter, ic) else {
        return Vec::new();
    };
    let Some(wname) = locale::mbstowcs(fname, NAME_MAX - 1) else {
        return Vec::new();
    };
    let mut matched_pos = 0;
    let mut at = 0;
    while at < pos && matched_pos < wname.len() {
        let len = locale::mblen_at(&fname[at..]);
        if len <= 0 {
            break;
        }
        at += len as usize;
        matched_pos += 1;
    }
    let Some(wfilter) = locale::mbstowcs(filter, NAME_MAX - 1) else {
        return Vec::new();
    };
    (0..wfilter.len())
        .map(|i| matched_pos + i)
        .take_while(|&p| p < NAME_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order() {
        assert_eq!(xstrverscasecmp(b"file2", b"file10"), Ordering::Less);
        assert_eq!(xstrverscasecmp(b"a01", b"a1"), Ordering::Less);
        assert_eq!(xstrverscasecmp(b"ABC", b"abc"), Ordering::Equal);
    }

    #[test]
    fn numeric_names_first() {
        assert_eq!(strtoll(b" -12x"), (-12, true));
        assert_eq!(strtoll(b"+"), (0, false));
        assert_eq!(xstricmp(b"10", b"9"), Ordering::Greater);
        assert_eq!(xstricmp(b"abc", b"9"), Ordering::Greater);
    }

    #[test]
    fn sort_keys() {
        let mut c = SortCfg::default();
        assert_eq!(c.set_sort_flags(b'S'), (b's', false));
        assert_eq!(c.getorderstr(), "SR ");
        assert_eq!(c.set_sort_flags(b'x'), (0, false));
    }

    #[test]
    fn sort_fuzzy_matches_fuzzyentrycmp_order() {
        let names: &[(&[u8], bool)] = &[
            (b"alpha", true),
            (b"Beta", true),
            (b"foo_bar", true),
            (b"f_b", true),
            (b"zzz", true),
            (b"fb", false),
            (b"f-b", false),
            (b"foo_bar.rs", false),
            (b"FooBar.txt", false),
            (b"a_foo_b", false),
            (b"bar_foo", false),
            (b"f123b", false),
            (b"FB", false),
            (b"f_b_c", false),
            (b"x_f_b", false),
            (b"foo_baz", false),
            (b"file_b", false),
            (b"f.b", false),
            (b"other", false),
            (b"my_fb_file", false),
        ];
        let base: Vec<Entry> = names
            .iter()
            .map(|&(name, is_dir)| Entry {
                name: name.to_vec(),
                flags: if is_dir { DIR_OR_DIRLNK } else { 0 },
                ..Default::default()
            })
            .collect();
        for reverse in [false, true] {
            for ic in [false, true] {
                let sort = SortCfg {
                    reverse,
                    rev_fn: reverse,
                    ..Default::default()
                };
                let mut expected = base.clone();
                expected.sort_by(|a, b| sort.fuzzyentrycmp(b"fb", ic, a, b));
                let mut actual = base.clone();
                sort.sort_fuzzy(&mut actual, b"fb", ic);
                assert_eq!(actual, expected, "reverse={reverse} ic={ic}");
            }
        }
    }
}
