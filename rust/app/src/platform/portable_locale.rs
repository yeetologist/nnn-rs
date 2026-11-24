//! The pure-Rust locale backend: what glibc does in the C.UTF-8 locale, without libc.
//!
//! Collation is byte order, case folding of bytes is ASCII only, wide characters are Unicode
//! scalar values decoded from strict UTF-8, widths come from the Unicode tables of
//! unicode-width, POSIX extended regexes are translated to the regex crate's syntax, and
//! local time comes from the OS time zone. Differences from glibc are in
//! rust/oracle/portable-known-diffs.md.

use crate::locale::{LocalTime, Wide};
use chrono::{DateTime, Datelike, Local, Timelike};
use std::collections::HashMap;
use std::sync::OnceLock;
use unicode_width::UnicodeWidthChar;

/// The bytes before the first NUL: what a C string function sees.
fn cstr(s: &[u8]) -> &[u8] {
    &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
}

/// No-op: the portable backend is always the C.UTF-8 locale.
pub fn init() {}

/// C.UTF-8 collates by code point, which for UTF-8 is byte order.
pub fn strcoll(a: &[u8], b: &[u8]) -> i32 {
    byte_diff(cstr(a).iter().copied(), cstr(b).iter().copied())
}

/// ASCII-only case-insensitive comparison; the result is the difference of the first
/// differing bytes, as glibc returns it.
pub fn strcasecmp(a: &[u8], b: &[u8]) -> i32 {
    byte_diff(
        cstr(a).iter().map(u8::to_ascii_lowercase),
        cstr(b).iter().map(u8::to_ascii_lowercase),
    )
}

fn byte_diff(a: impl Iterator<Item = u8>, b: impl Iterator<Item = u8>) -> i32 {
    let mut a = a.chain([0]);
    let mut b = b.chain([0]);
    loop {
        let (x, y) = (a.next().unwrap_or(0), b.next().unwrap_or(0));
        if x != y || x == 0 {
            return i32::from(x) - i32::from(y);
        }
    }
}

/// `tolower()` on one byte: ASCII only.
pub fn tolower(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

/// One UTF-8 character at the start of `s` (glibc's strict decoder: no overlong forms, no
/// surrogates, nothing above U+10FFFF): its value and length, `None` when invalid or cut off.
fn decode_one(s: &[u8]) -> Option<(char, usize)> {
    let len = match *s.first()? {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    let ch = std::str::from_utf8(s.get(..len)?).ok()?.chars().next()?;
    Some((ch, len))
}

/// `mbstowcs(dst, s, max)`: at most `max` wide characters, `None` for an invalid sequence
/// among them.
pub fn mbstowcs(s: &[u8], max: usize) -> Option<Wide> {
    let mut rest = cstr(s);
    let mut out = Wide::new();
    while !rest.is_empty() && out.len() < max {
        let (ch, len) = decode_one(rest)?;
        out.push(ch as i32);
        rest = &rest[len..];
    }
    Some(out)
}

/// `mblen(s, MB_LEN_MAX)` at the start of `s`: 0 at the end, -1 for an invalid sequence.
pub fn mblen_at(s: &[u8]) -> i32 {
    let s = cstr(s);
    if s.is_empty() {
        return 0;
    }
    decode_one(s).map_or(-1, |(_, len)| len as i32)
}

fn to_char(c: i32) -> Option<char> {
    u32::try_from(c).ok().and_then(char::from_u32)
}

/// The single-character mapping of a case conversion, or `c` when it has none or several.
fn simple_case(c: char, mut mapped: impl Iterator<Item = char>) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(m), None) => m,
        _ => c,
    }
}

fn simple_upper(c: char) -> char {
    simple_case(c, c.to_uppercase())
}

/// `towlower`: the single-character lowercase of `c`, or `c` when it has none or several.
/// U+0130 is the one character whose full lowercase is two characters but whose simple
/// lowercase (which glibc uses) is `i`.
pub fn to_lower_wide(c: i32) -> i32 {
    match to_char(c) {
        Some('\u{130}') => 'i' as i32,
        Some(ch) => simple_case(ch, ch.to_lowercase()) as i32,
        None => c,
    }
}

/// `wcswidth(s, n)`: -1 when a character among the first `n` (up to a NUL) is not printable.
pub fn wcswidth_n(s: &[i32], n: usize) -> i32 {
    let mut total = 0;
    for &c in s.iter().take(n).take_while(|&&c| c != 0) {
        match to_char(c).and_then(char_width) {
            Some(w) => total += w as i32,
            None => return -1,
        }
    }
    total
}

/// `wcstombs`: the wide string (up to a NUL) as UTF-8, `None` for a value that is no scalar.
pub fn wcstombs(s: &[i32]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for &c in s.iter().take_while(|&&c| c != 0) {
        out.extend_from_slice(to_char(c)?.encode_utf8(&mut [0; 4]).as_bytes());
    }
    Some(out)
}

pub fn localtime(secs: i64) -> LocalTime {
    let t = DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .with_timezone(&Local);
    LocalTime {
        year: t.year(),
        mon: t.month() as i32,
        mday: t.day() as i32,
        hour: t.hour() as i32,
        min: t.minute() as i32,
        sec: t.second() as i32,
    }
}

/// Whether `c` matches a Unicode property regex (a one-character `^[...]$` class), built once.
fn has_property(cell: &'static OnceLock<regex::Regex>, class: &str, c: char) -> bool {
    cell.get_or_init(|| {
        // INFALLIBLE: the class is a fixed, valid Unicode property expression.
        regex::Regex::new(&format!("^{class}$")).unwrap_or_else(|e| panic!("{class}: {e}"))
    })
    .is_match(c.encode_utf8(&mut [0; 4]))
}

/// `iswalnum` in C.UTF-8: letters and decimal digits, not the other numbers (`²`, `Ⅷ`).
pub fn is_walnum(c: i32) -> bool {
    static ALNUM: OnceLock<regex::Regex> = OnceLock::new();
    to_char(c).is_some_and(|c| {
        c.is_ascii_alphanumeric() || has_property(&ALNUM, "[\\p{Alphabetic}\\p{Nd}]", c)
    })
}

/// `wcwidth`: glibc gives -1 to every unassigned code point, and 1 to the soft hyphen.
fn char_width(c: char) -> Option<usize> {
    static ASSIGNED: OnceLock<regex::Regex> = OnceLock::new();
    if c == '\u{ad}' {
        return Some(1);
    }
    if !c.is_ascii() && !has_property(&ASSIGNED, "\\p{Assigned}", c) {
        return None;
    }
    UnicodeWidthChar::width(c)
}

/// A compiled POSIX extended regex (`REG_NOSUB | REG_EXTENDED [| REG_ICASE]`), translated to
/// the regex crate's syntax.
pub struct Regex(regex::bytes::Regex);

impl Regex {
    pub fn new(pattern: &[u8], icase: bool) -> Option<Regex> {
        regex::bytes::RegexBuilder::new(&ere_to_rust(cstr(pattern), icase)?)
            .dot_matches_new_line(true)
            .build()
            .ok()
            .map(Regex)
    }

    pub fn is_match(&self, s: &[u8]) -> bool {
        self.0.is_match(cstr(s))
    }
}

/// The largest repeat count in an interval (glibc's RE_DUP_MAX).
const DUP_MAX: u32 = 0x7fff;

/// One level of parentheses while translating: the finished alternatives and the pieces of
/// the current one (each with whether it is an anchor, which no repeat operator may follow).
#[derive(Default)]
struct Frame {
    alts: Vec<String>,
    seq: Vec<(String, bool)>,
}

impl Frame {
    fn finish(mut self) -> String {
        self.alts
            .push(self.seq.into_iter().map(|(p, _)| p).collect());
        self.alts.join("|")
    }

    fn push(&mut self, piece: impl Into<String>, anchor: bool) {
        self.seq.push((piece.into(), anchor));
    }

    /// Applies a repeat operator to the last piece; like glibc it may follow another one
    /// (`a+?` is `(a+)?`), but not an anchor, a `(` or a `|`.
    fn repeat(&mut self, op: &str) -> Option<()> {
        let (piece, anchor) = self.seq.pop()?;
        if anchor {
            return None;
        }
        self.seq.push((format!("(?:{piece}){op}"), false));
        Some(())
    }
}

/// A byte of the pattern that is not valid UTF-8 (glibc takes it as itself) is carried as a
/// character in the last private-use block, U+10FF00 + byte.
const RAW_BASE: u32 = 0x10_ff00;

fn raw_byte(c: char) -> Option<u8> {
    u8::try_from((c as u32).checked_sub(RAW_BASE)?).ok()
}

fn literal(c: char) -> String {
    match raw_byte(c) {
        Some(b) => format!("(?-u:\\x{b:02X})"),
        None => format!("\\x{{{:X}}}", c as u32),
    }
}

/// Every character that has an uppercase other than itself, by that uppercase.
fn case_variants() -> &'static HashMap<char, Vec<char>> {
    static MAP: OnceLock<HashMap<char, Vec<char>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map: HashMap<char, Vec<char>> = HashMap::new();
        for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
            let up = simple_upper(c);
            if up != c {
                map.entry(up).or_default().push(c);
            }
        }
        map
    })
}

/// A literal under `REG_ICASE`: glibc compares `towupper` of both sides, so it matches every
/// character with the same simple uppercase (`i`, `I` and `ı`, but not U+212A for `k`).
fn literal_icase(c: char) -> String {
    if raw_byte(c).is_some() {
        return literal(c);
    }
    let up = simple_upper(c);
    let variants = case_variants().get(&up).map_or(&[][..], Vec::as_slice);
    if variants.is_empty() {
        return literal(c);
    }
    let set: String = std::iter::once(up)
        .chain(variants.iter().copied())
        .map(literal)
        .collect();
    format!("[{set}]")
}

/// A number in `{n,m}` at `*i`, `None` when there are no digits.
fn number(c: &[char], i: &mut usize) -> Option<u32> {
    let start = *i;
    let mut n: u32 = 0;
    while let Some(d) = c.get(*i).and_then(|c| c.to_digit(10)) {
        n = n.saturating_mul(10).saturating_add(d);
        *i += 1;
    }
    (*i > start).then_some(n)
}

/// `{n}`, `{n,}`, `{,m}` or `{n,m}` after the `{` (glibc rejects anything else in an ERE).
fn interval(c: &[char], i: &mut usize) -> Option<String> {
    let lo = number(c, i);
    let (lo, hi) = if c.get(*i) == Some(&',') {
        *i += 1;
        (lo.unwrap_or(0), number(c, i))
    } else {
        let n = lo?;
        (n, Some(n))
    };
    if c.get(*i) != Some(&'}') {
        return None;
    }
    *i += 1;
    if lo > DUP_MAX || hi.is_some_and(|h| h > DUP_MAX || h < lo) {
        return None;
    }
    Some(match hi {
        Some(h) => format!("{{{lo},{h}}}"),
        None => format!("{{{lo},}}"),
    })
}

/// A bracket expression after the `[`, as a regex-crate class. Classes are the ASCII POSIX
/// ones, equivalence classes and collating symbols are the character itself.
fn bracket(c: &[char], i: &mut usize, icase: bool) -> Option<String> {
    let mut out = String::from("[");
    if c.get(*i) == Some(&'^') {
        out.push('^');
        *i += 1;
    }
    let mut first = true;
    loop {
        let ch = *c.get(*i)?;
        if ch == ']' && !first {
            *i += 1;
            break;
        }
        first = false;
        let lo = bracket_atom(c, i, &mut out, icase)?;
        // A range `lo-hi`; a `-` just before the closing `]` is a literal.
        if let (Some(lo), Some(&'-'), Some(&next)) = (lo, c.get(*i), c.get(*i + 1))
            && next != ']'
        {
            *i += 1;
            let hi = bracket_atom(c, i, &mut String::new(), icase)??;
            if hi < lo {
                return None;
            }
            out.push_str(&format!("-{}", literal(hi)));
        }
    }
    out.push(']');
    Some(out)
}

/// A POSIX character class as C.UTF-8 defines it (Unicode aware), as regex-crate class items.
fn posix_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "[[\\p{Alphabetic}\\p{Nd}]--[0-9]]",
        "alnum" => "\\p{Alphabetic}\\p{Nd}",
        "digit" => "0-9",
        "xdigit" => "0-9A-Fa-f",
        "upper" => "\\p{Uppercase}",
        "lower" => "\\p{Lowercase}",
        "space" => {
            "\\x{9}-\\x{D}\\x{20}\\x{1680}\\x{2000}-\\x{2006}\\x{2008}-\\x{200A}\\x{2028}\\x{2029}\\x{205F}\\x{3000}"
        }
        "blank" => "\\x{9}\\x{20}\\x{1680}\\x{2000}-\\x{2006}\\x{2008}-\\x{200A}\\x{205F}\\x{3000}",
        "cntrl" => "\\p{Cc}\\x{2028}\\x{2029}",
        "punct" => "\\p{P}\\p{S}",
        "graph" => "[[\\p{Assigned}]--[\\p{Cc}\\p{Z}]]",
        "print" => "[[\\p{Assigned}]--[\\p{Cc}\\x{2028}\\x{2029}]]",
        _ => return None,
    })
}

/// One element of a bracket expression, appended to `out`; `Some(Some(c))` for a single
/// character that can start a range, `Some(None)` for a class, `None` for a syntax error.
fn bracket_atom(c: &[char], i: &mut usize, out: &mut String, icase: bool) -> Option<Option<char>> {
    let ch = *c.get(*i)?;
    *i += 1;
    if raw_byte(ch).is_some() {
        return None;
    }
    if ch == '[' && matches!(c.get(*i), Some(':' | '.' | '=')) {
        let kind = c[*i];
        let close =
            (*i + 1..c.len().saturating_sub(1)).find(|&k| c[k] == kind && c[k + 1] == ']')?;
        let name: String = c[*i + 1..close].iter().collect();
        *i = close + 2;
        if kind == ':' {
            // glibc reads upper and lower as alpha under REG_ICASE.
            let name = if icase && (name == "upper" || name == "lower") {
                "alpha"
            } else {
                &name
            };
            out.push_str(posix_class(name)?);
            return Some(None);
        }
        let mut chars = name.chars();
        let (Some(only), None) = (chars.next(), chars.next()) else {
            return None;
        };
        out.push_str(&literal(only));
        return Some(Some(only));
    }
    out.push_str(&literal(ch));
    Some(Some(ch))
}

/// POSIX extended regex syntax as glibc's `regcomp` reads it, to the regex crate's syntax;
/// `None` where glibc reports an error (or, for back-references, where the regex crate cannot).
fn ere_to_rust(pattern: &[u8], icase: bool) -> Option<String> {
    let mut c: Vec<char> = Vec::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        let (ch, len) =
            decode_one(rest).unwrap_or((char::from_u32(RAW_BASE + u32::from(rest[0]))?, 1));
        c.push(ch);
        rest = &rest[len..];
    }
    let lit = |ch: char| {
        if icase {
            literal_icase(ch)
        } else {
            literal(ch)
        }
    };
    let mut i = 0;
    let mut stack = vec![Frame::default()];
    while i < c.len() {
        let ch = c[i];
        i += 1;
        match ch {
            '\\' => {
                let n = *c.get(i)?;
                i += 1;
                match n {
                    'w' => stack
                        .last_mut()?
                        .push(format!("[{}_]", posix_class("alnum")?), false),
                    'W' => stack
                        .last_mut()?
                        .push(format!("[^{}_]", posix_class("alnum")?), false),
                    's' => stack
                        .last_mut()?
                        .push(format!("[{}]", posix_class("space")?), false),
                    'S' => stack
                        .last_mut()?
                        .push(format!("[^{}]", posix_class("space")?), false),
                    'b' | 'B' => stack.last_mut()?.push(format!("\\{n}"), true),
                    '<' => stack.last_mut()?.push("\\b{start}", true),
                    '>' => stack.last_mut()?.push("\\b{end}", true),
                    '`' => stack.last_mut()?.push("\\A", true),
                    '\'' => stack.last_mut()?.push("\\z", true),
                    '1'..='9' => return None,
                    _ => stack.last_mut()?.push(lit(n), false),
                }
            }
            '(' => stack.push(Frame::default()),
            ')' if stack.len() > 1 => {
                let group = stack.pop()?.finish();
                stack.last_mut()?.push(format!("(?:{group})"), false);
            }
            '|' => {
                let top = stack.last_mut()?;
                let seq = std::mem::take(&mut top.seq);
                top.alts.push(seq.into_iter().map(|(p, _)| p).collect());
            }
            '*' => stack.last_mut()?.repeat("*")?,
            '+' => stack.last_mut()?.repeat("+")?,
            '?' => stack.last_mut()?.repeat("?")?,
            '{' => {
                let op = interval(&c, &mut i)?;
                stack.last_mut()?.repeat(&op)?;
            }
            '[' => {
                let class = bracket(&c, &mut i, icase)?;
                let class = if icase {
                    format!("(?i:{class})")
                } else {
                    class
                };
                stack.last_mut()?.push(class, false);
            }
            '.' => stack.last_mut()?.push(".", false),
            '^' => stack.last_mut()?.push("^", true),
            '$' => stack.last_mut()?.push("$", true),
            _ => stack.last_mut()?.push(lit(ch), false),
        }
    }
    if stack.len() != 1 {
        return None;
    }
    Some(stack.pop()?.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re(p: &str, icase: bool) -> Regex {
        Regex::new(p.as_bytes(), icase).unwrap_or_else(|| panic!("{p} should compile"))
    }

    fn matches(p: &str, s: &str) -> bool {
        re(p, false).is_match(s.as_bytes())
    }

    #[test]
    fn collation_and_case_are_bytewise_and_ascii() {
        assert!(strcoll(b"B", b"a") < 0);
        assert_eq!(strcoll(b"abc", b"abc\0def"), 0);
        assert_eq!(strcasecmp(b"ABC", b"abc"), 0);
        assert!(strcasecmp("É".as_bytes(), "é".as_bytes()) < 0);
        assert_eq!(tolower(b'Q'), b'q');
        assert_eq!(tolower(0xc9), 0xc9);
    }

    #[test]
    fn mbstowcs_decodes_strict_utf8() {
        assert_eq!(
            mbstowcs("aé€😀".as_bytes(), 10),
            Some(vec![97, 233, 0x20ac, 0x1f600])
        );
        assert_eq!(mbstowcs("aé€".as_bytes(), 2), Some(vec![97, 233]));
        assert_eq!(mbstowcs(b"a\xff", 10), None);
        assert_eq!(mbstowcs(b"\xc0\x80", 10), None);
        assert_eq!(mbstowcs(b"\xed\xa0\x80", 10), None);
        assert_eq!(mbstowcs(b"\xe2\x82", 10), None);
        assert_eq!(mblen_at("€x".as_bytes()), 3);
        assert_eq!(mblen_at(b""), 0);
        assert_eq!(mblen_at(b"\xe2\x28"), -1);
    }

    #[test]
    fn wide_functions_follow_unicode() {
        assert_eq!(to_lower_wide('Ä' as i32), 'ä' as i32);
        assert_eq!(to_lower_wide(-1), -1);
        assert_eq!(wcswidth_n(&['a' as i32, '日' as i32], 2), 3);
        assert_eq!(wcswidth_n(&['a' as i32, 7], 2), -1);
        assert_eq!(wcswidth_n(&['a' as i32, '日' as i32], 1), 1);
        assert_eq!(wcstombs(&[97, 0x20ac]), Some("a€".as_bytes().to_vec()));
        assert_eq!(wcstombs(&[0xd800]), None);
        assert!(is_walnum('é' as i32));
        assert!(!is_walnum('-' as i32));
    }

    #[test]
    fn ere_syntax_matches_posix() {
        assert!(matches("^a.c$", "a\nc"));
        assert!(matches("[]a]+", "]"));
        assert!(matches("[[:digit:]]{2,3}x", "a123x"));
        assert!(!matches("^[[:alpha:]]+$", "ab1"));
        assert!(matches("[\\d]", "\\"));
        assert!(matches("a+?", ""));
        assert!(matches("(ab|)c", "c"));
        assert!(matches("a)", "a)"));
        assert!(matches("\\.", "."));
        assert!(matches("\\d", "d"));
        assert!(matches("[a-]", "-"));
        assert!(re("ABC", true).is_match(b"xabcx"));
        assert!(!re("ABC", false).is_match(b"xabcx"));
    }

    #[test]
    fn ere_errors_match_regcomp() {
        for bad in [
            "*a",
            "a|*",
            "(a",
            "[a",
            "a{",
            "a{1",
            "a{2,1}",
            "{1}",
            "^*",
            "a{x}",
            "\\",
            "[[:foo:]]",
            "[b-a]",
        ] {
            assert!(
                Regex::new(bad.as_bytes(), false).is_none(),
                "{bad} should not compile"
            );
        }
    }
}
