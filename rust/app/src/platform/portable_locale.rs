//! The pure-Rust locale backend: what glibc does in the C.UTF-8 locale, without libc.
//!
//! Collation is byte order, case folding of bytes is ASCII only, wide characters are Unicode
//! scalar values decoded from strict UTF-8, widths come from the Unicode tables of
//! unicode-width, POSIX extended regexes are translated to the regex crate's syntax, and
//! local time comes from the OS time zone. Differences from glibc are in
//! rust/oracle/portable-known-diffs.md.

use crate::locale::{LocalTime, Wide};
use chrono::{DateTime, Datelike, FixedOffset, Local, Timelike};
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

/// The fixed zone a POSIX `TZ` without daylight-saving rules names (`UTC`, `UTC0`, `EST5`,
/// `<+0530>-5:30`), as seconds east of UTC; glibc reads a name without an offset as UTC.
/// None for anything else (unset, zoneinfo names, DST rules): the system's zone applies.
fn tz_fixed_offset(tz: &str) -> Option<i32> {
    let rest = match tz.strip_prefix('<') {
        Some(quoted) => &quoted[quoted.find('>')? + 1..],
        None => {
            let name = tz.bytes().take_while(u8::is_ascii_alphabetic).count();
            if name < 3 {
                return None;
            }
            &tz[name..]
        }
    };
    if rest.is_empty() {
        return Some(0);
    }
    let (west, hms) = match rest.as_bytes()[0] {
        b'-' => (false, &rest[1..]),
        b'+' => (true, &rest[1..]),
        _ => (true, rest),
    };
    let mut secs = 0;
    let mut parts = 0;
    for (part, unit) in hms.split(':').zip([3600, 60, 1]) {
        if part.is_empty() || part.len() > 2 || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        secs += part.parse::<i32>().ok()? * unit;
        parts += 1;
    }
    if parts != hms.split(':').count() || secs > 24 * 3600 {
        return None;
    }
    Some(if west { -secs } else { secs })
}

pub fn localtime(secs: i64) -> LocalTime {
    static ZONE: OnceLock<Option<FixedOffset>> = OnceLock::new();
    let zone = ZONE.get_or_init(|| {
        let tz = std::env::var("TZ").ok()?;
        FixedOffset::east_opt(tz_fixed_offset(&tz)?)
    });
    let utc = DateTime::from_timestamp(secs, 0).unwrap_or_default();
    let t = match zone {
        Some(z) => utc.with_timezone(z).naive_local(),
        None => utc.with_timezone(&Local).naive_local(),
    };
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

/// Whether `c` is an unassigned (`General_Category=Cn`) code point in Unicode 16.0.
fn is_unassigned(c: char) -> bool {
    let cp = c as u32;
    UNASSIGNED
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `wcwidth`: glibc gives -1 to every unassigned code point, and 1 to the soft hyphen.
fn char_width(c: char) -> Option<usize> {
    if c == '\u{ad}' {
        return Some(1);
    }
    if c.is_ascii() || c.is_alphanumeric() || c.is_whitespace() {
        return UnicodeWidthChar::width(c);
    }
    if is_unassigned(c) {
        return None;
    }
    UnicodeWidthChar::width(c)
}

/// Inclusive code-point ranges with General_Category=Cn (Unassigned) in Unicode 16.0.
const UNASSIGNED: &[(u32, u32)] = &[
    (0x0378, 0x0379),
    (0x0380, 0x0383),
    (0x038b, 0x038b),
    (0x038d, 0x038d),
    (0x03a2, 0x03a2),
    (0x0530, 0x0530),
    (0x0557, 0x0558),
    (0x058b, 0x058c),
    (0x0590, 0x0590),
    (0x05c8, 0x05cf),
    (0x05eb, 0x05ee),
    (0x05f5, 0x05ff),
    (0x070e, 0x070e),
    (0x074b, 0x074c),
    (0x07b2, 0x07bf),
    (0x07fb, 0x07fc),
    (0x082e, 0x082f),
    (0x083f, 0x083f),
    (0x085c, 0x085d),
    (0x085f, 0x085f),
    (0x086b, 0x086f),
    (0x088f, 0x088f),
    (0x0892, 0x0896),
    (0x0984, 0x0984),
    (0x098d, 0x098e),
    (0x0991, 0x0992),
    (0x09a9, 0x09a9),
    (0x09b1, 0x09b1),
    (0x09b3, 0x09b5),
    (0x09ba, 0x09bb),
    (0x09c5, 0x09c6),
    (0x09c9, 0x09ca),
    (0x09cf, 0x09d6),
    (0x09d8, 0x09db),
    (0x09de, 0x09de),
    (0x09e4, 0x09e5),
    (0x09ff, 0x0a00),
    (0x0a04, 0x0a04),
    (0x0a0b, 0x0a0e),
    (0x0a11, 0x0a12),
    (0x0a29, 0x0a29),
    (0x0a31, 0x0a31),
    (0x0a34, 0x0a34),
    (0x0a37, 0x0a37),
    (0x0a3a, 0x0a3b),
    (0x0a3d, 0x0a3d),
    (0x0a43, 0x0a46),
    (0x0a49, 0x0a4a),
    (0x0a4e, 0x0a50),
    (0x0a52, 0x0a58),
    (0x0a5d, 0x0a5d),
    (0x0a5f, 0x0a65),
    (0x0a77, 0x0a80),
    (0x0a84, 0x0a84),
    (0x0a8e, 0x0a8e),
    (0x0a92, 0x0a92),
    (0x0aa9, 0x0aa9),
    (0x0ab1, 0x0ab1),
    (0x0ab4, 0x0ab4),
    (0x0aba, 0x0abb),
    (0x0ac6, 0x0ac6),
    (0x0aca, 0x0aca),
    (0x0ace, 0x0acf),
    (0x0ad1, 0x0adf),
    (0x0ae4, 0x0ae5),
    (0x0af2, 0x0af8),
    (0x0b00, 0x0b00),
    (0x0b04, 0x0b04),
    (0x0b0d, 0x0b0e),
    (0x0b11, 0x0b12),
    (0x0b29, 0x0b29),
    (0x0b31, 0x0b31),
    (0x0b34, 0x0b34),
    (0x0b3a, 0x0b3b),
    (0x0b45, 0x0b46),
    (0x0b49, 0x0b4a),
    (0x0b4e, 0x0b54),
    (0x0b58, 0x0b5b),
    (0x0b5e, 0x0b5e),
    (0x0b64, 0x0b65),
    (0x0b78, 0x0b81),
    (0x0b84, 0x0b84),
    (0x0b8b, 0x0b8d),
    (0x0b91, 0x0b91),
    (0x0b96, 0x0b98),
    (0x0b9b, 0x0b9b),
    (0x0b9d, 0x0b9d),
    (0x0ba0, 0x0ba2),
    (0x0ba5, 0x0ba7),
    (0x0bab, 0x0bad),
    (0x0bba, 0x0bbd),
    (0x0bc3, 0x0bc5),
    (0x0bc9, 0x0bc9),
    (0x0bce, 0x0bcf),
    (0x0bd1, 0x0bd6),
    (0x0bd8, 0x0be5),
    (0x0bfb, 0x0bff),
    (0x0c0d, 0x0c0d),
    (0x0c11, 0x0c11),
    (0x0c29, 0x0c29),
    (0x0c3a, 0x0c3b),
    (0x0c45, 0x0c45),
    (0x0c49, 0x0c49),
    (0x0c4e, 0x0c54),
    (0x0c57, 0x0c57),
    (0x0c5b, 0x0c5c),
    (0x0c5e, 0x0c5f),
    (0x0c64, 0x0c65),
    (0x0c70, 0x0c76),
    (0x0c8d, 0x0c8d),
    (0x0c91, 0x0c91),
    (0x0ca9, 0x0ca9),
    (0x0cb4, 0x0cb4),
    (0x0cba, 0x0cbb),
    (0x0cc5, 0x0cc5),
    (0x0cc9, 0x0cc9),
    (0x0cce, 0x0cd4),
    (0x0cd7, 0x0cdc),
    (0x0cdf, 0x0cdf),
    (0x0ce4, 0x0ce5),
    (0x0cf0, 0x0cf0),
    (0x0cf4, 0x0cff),
    (0x0d0d, 0x0d0d),
    (0x0d11, 0x0d11),
    (0x0d45, 0x0d45),
    (0x0d49, 0x0d49),
    (0x0d50, 0x0d53),
    (0x0d64, 0x0d65),
    (0x0d80, 0x0d80),
    (0x0d84, 0x0d84),
    (0x0d97, 0x0d99),
    (0x0db2, 0x0db2),
    (0x0dbc, 0x0dbc),
    (0x0dbe, 0x0dbf),
    (0x0dc7, 0x0dc9),
    (0x0dcb, 0x0dce),
    (0x0dd5, 0x0dd5),
    (0x0dd7, 0x0dd7),
    (0x0de0, 0x0de5),
    (0x0df0, 0x0df1),
    (0x0df5, 0x0e00),
    (0x0e3b, 0x0e3e),
    (0x0e5c, 0x0e80),
    (0x0e83, 0x0e83),
    (0x0e85, 0x0e85),
    (0x0e8b, 0x0e8b),
    (0x0ea4, 0x0ea4),
    (0x0ea6, 0x0ea6),
    (0x0ebe, 0x0ebf),
    (0x0ec5, 0x0ec5),
    (0x0ec7, 0x0ec7),
    (0x0ecf, 0x0ecf),
    (0x0eda, 0x0edb),
    (0x0ee0, 0x0eff),
    (0x0f48, 0x0f48),
    (0x0f6d, 0x0f70),
    (0x0f98, 0x0f98),
    (0x0fbd, 0x0fbd),
    (0x0fcd, 0x0fcd),
    (0x0fdb, 0x0fff),
    (0x10c6, 0x10c6),
    (0x10c8, 0x10cc),
    (0x10ce, 0x10cf),
    (0x1249, 0x1249),
    (0x124e, 0x124f),
    (0x1257, 0x1257),
    (0x1259, 0x1259),
    (0x125e, 0x125f),
    (0x1289, 0x1289),
    (0x128e, 0x128f),
    (0x12b1, 0x12b1),
    (0x12b6, 0x12b7),
    (0x12bf, 0x12bf),
    (0x12c1, 0x12c1),
    (0x12c6, 0x12c7),
    (0x12d7, 0x12d7),
    (0x1311, 0x1311),
    (0x1316, 0x1317),
    (0x135b, 0x135c),
    (0x137d, 0x137f),
    (0x139a, 0x139f),
    (0x13f6, 0x13f7),
    (0x13fe, 0x13ff),
    (0x169d, 0x169f),
    (0x16f9, 0x16ff),
    (0x1716, 0x171e),
    (0x1737, 0x173f),
    (0x1754, 0x175f),
    (0x176d, 0x176d),
    (0x1771, 0x1771),
    (0x1774, 0x177f),
    (0x17de, 0x17df),
    (0x17ea, 0x17ef),
    (0x17fa, 0x17ff),
    (0x181a, 0x181f),
    (0x1879, 0x187f),
    (0x18ab, 0x18af),
    (0x18f6, 0x18ff),
    (0x191f, 0x191f),
    (0x192c, 0x192f),
    (0x193c, 0x193f),
    (0x1941, 0x1943),
    (0x196e, 0x196f),
    (0x1975, 0x197f),
    (0x19ac, 0x19af),
    (0x19ca, 0x19cf),
    (0x19db, 0x19dd),
    (0x1a1c, 0x1a1d),
    (0x1a5f, 0x1a5f),
    (0x1a7d, 0x1a7e),
    (0x1a8a, 0x1a8f),
    (0x1a9a, 0x1a9f),
    (0x1aae, 0x1aaf),
    (0x1acf, 0x1aff),
    (0x1b4d, 0x1b4d),
    (0x1bf4, 0x1bfb),
    (0x1c38, 0x1c3a),
    (0x1c4a, 0x1c4c),
    (0x1c8b, 0x1c8f),
    (0x1cbb, 0x1cbc),
    (0x1cc8, 0x1ccf),
    (0x1cfb, 0x1cff),
    (0x1f16, 0x1f17),
    (0x1f1e, 0x1f1f),
    (0x1f46, 0x1f47),
    (0x1f4e, 0x1f4f),
    (0x1f58, 0x1f58),
    (0x1f5a, 0x1f5a),
    (0x1f5c, 0x1f5c),
    (0x1f5e, 0x1f5e),
    (0x1f7e, 0x1f7f),
    (0x1fb5, 0x1fb5),
    (0x1fc5, 0x1fc5),
    (0x1fd4, 0x1fd5),
    (0x1fdc, 0x1fdc),
    (0x1ff0, 0x1ff1),
    (0x1ff5, 0x1ff5),
    (0x1fff, 0x1fff),
    (0x2065, 0x2065),
    (0x2072, 0x2073),
    (0x208f, 0x208f),
    (0x209d, 0x209f),
    (0x20c1, 0x20cf),
    (0x20f1, 0x20ff),
    (0x218c, 0x218f),
    (0x242a, 0x243f),
    (0x244b, 0x245f),
    (0x2b74, 0x2b75),
    (0x2b96, 0x2b96),
    (0x2cf4, 0x2cf8),
    (0x2d26, 0x2d26),
    (0x2d28, 0x2d2c),
    (0x2d2e, 0x2d2f),
    (0x2d68, 0x2d6e),
    (0x2d71, 0x2d7e),
    (0x2d97, 0x2d9f),
    (0x2da7, 0x2da7),
    (0x2daf, 0x2daf),
    (0x2db7, 0x2db7),
    (0x2dbf, 0x2dbf),
    (0x2dc7, 0x2dc7),
    (0x2dcf, 0x2dcf),
    (0x2dd7, 0x2dd7),
    (0x2ddf, 0x2ddf),
    (0x2e5e, 0x2e7f),
    (0x2e9a, 0x2e9a),
    (0x2ef4, 0x2eff),
    (0x2fd6, 0x2fef),
    (0x3040, 0x3040),
    (0x3097, 0x3098),
    (0x3100, 0x3104),
    (0x3130, 0x3130),
    (0x318f, 0x318f),
    (0x31e6, 0x31ee),
    (0x321f, 0x321f),
    (0xa48d, 0xa48f),
    (0xa4c7, 0xa4cf),
    (0xa62c, 0xa63f),
    (0xa6f8, 0xa6ff),
    (0xa7ce, 0xa7cf),
    (0xa7d2, 0xa7d2),
    (0xa7d4, 0xa7d4),
    (0xa7dd, 0xa7f1),
    (0xa82d, 0xa82f),
    (0xa83a, 0xa83f),
    (0xa878, 0xa87f),
    (0xa8c6, 0xa8cd),
    (0xa8da, 0xa8df),
    (0xa954, 0xa95e),
    (0xa97d, 0xa97f),
    (0xa9ce, 0xa9ce),
    (0xa9da, 0xa9dd),
    (0xa9ff, 0xa9ff),
    (0xaa37, 0xaa3f),
    (0xaa4e, 0xaa4f),
    (0xaa5a, 0xaa5b),
    (0xaac3, 0xaada),
    (0xaaf7, 0xab00),
    (0xab07, 0xab08),
    (0xab0f, 0xab10),
    (0xab17, 0xab1f),
    (0xab27, 0xab27),
    (0xab2f, 0xab2f),
    (0xab6c, 0xab6f),
    (0xabee, 0xabef),
    (0xabfa, 0xabff),
    (0xd7a4, 0xd7af),
    (0xd7c7, 0xd7ca),
    (0xd7fc, 0xdfff),
    (0xfa6e, 0xfa6f),
    (0xfada, 0xfaff),
    (0xfb07, 0xfb12),
    (0xfb18, 0xfb1c),
    (0xfb37, 0xfb37),
    (0xfb3d, 0xfb3d),
    (0xfb3f, 0xfb3f),
    (0xfb42, 0xfb42),
    (0xfb45, 0xfb45),
    (0xfbc3, 0xfbd2),
    (0xfd90, 0xfd91),
    (0xfdc8, 0xfdce),
    (0xfdd0, 0xfdef),
    (0xfe1a, 0xfe1f),
    (0xfe53, 0xfe53),
    (0xfe67, 0xfe67),
    (0xfe6c, 0xfe6f),
    (0xfe75, 0xfe75),
    (0xfefd, 0xfefe),
    (0xff00, 0xff00),
    (0xffbf, 0xffc1),
    (0xffc8, 0xffc9),
    (0xffd0, 0xffd1),
    (0xffd8, 0xffd9),
    (0xffdd, 0xffdf),
    (0xffe7, 0xffe7),
    (0xffef, 0xfff8),
    (0xfffe, 0xffff),
    (0x1000c, 0x1000c),
    (0x10027, 0x10027),
    (0x1003b, 0x1003b),
    (0x1003e, 0x1003e),
    (0x1004e, 0x1004f),
    (0x1005e, 0x1007f),
    (0x100fb, 0x100ff),
    (0x10103, 0x10106),
    (0x10134, 0x10136),
    (0x1018f, 0x1018f),
    (0x1019d, 0x1019f),
    (0x101a1, 0x101cf),
    (0x101fe, 0x1027f),
    (0x1029d, 0x1029f),
    (0x102d1, 0x102df),
    (0x102fc, 0x102ff),
    (0x10324, 0x1032c),
    (0x1034b, 0x1034f),
    (0x1037b, 0x1037f),
    (0x1039e, 0x1039e),
    (0x103c4, 0x103c7),
    (0x103d6, 0x103ff),
    (0x1049e, 0x1049f),
    (0x104aa, 0x104af),
    (0x104d4, 0x104d7),
    (0x104fc, 0x104ff),
    (0x10528, 0x1052f),
    (0x10564, 0x1056e),
    (0x1057b, 0x1057b),
    (0x1058b, 0x1058b),
    (0x10593, 0x10593),
    (0x10596, 0x10596),
    (0x105a2, 0x105a2),
    (0x105b2, 0x105b2),
    (0x105ba, 0x105ba),
    (0x105bd, 0x105bf),
    (0x105f4, 0x105ff),
    (0x10737, 0x1073f),
    (0x10756, 0x1075f),
    (0x10768, 0x1077f),
    (0x10786, 0x10786),
    (0x107b1, 0x107b1),
    (0x107bb, 0x107ff),
    (0x10806, 0x10807),
    (0x10809, 0x10809),
    (0x10836, 0x10836),
    (0x10839, 0x1083b),
    (0x1083d, 0x1083e),
    (0x10856, 0x10856),
    (0x1089f, 0x108a6),
    (0x108b0, 0x108df),
    (0x108f3, 0x108f3),
    (0x108f6, 0x108fa),
    (0x1091c, 0x1091e),
    (0x1093a, 0x1093e),
    (0x10940, 0x1097f),
    (0x109b8, 0x109bb),
    (0x109d0, 0x109d1),
    (0x10a04, 0x10a04),
    (0x10a07, 0x10a0b),
    (0x10a14, 0x10a14),
    (0x10a18, 0x10a18),
    (0x10a36, 0x10a37),
    (0x10a3b, 0x10a3e),
    (0x10a49, 0x10a4f),
    (0x10a59, 0x10a5f),
    (0x10aa0, 0x10abf),
    (0x10ae7, 0x10aea),
    (0x10af7, 0x10aff),
    (0x10b36, 0x10b38),
    (0x10b56, 0x10b57),
    (0x10b73, 0x10b77),
    (0x10b92, 0x10b98),
    (0x10b9d, 0x10ba8),
    (0x10bb0, 0x10bff),
    (0x10c49, 0x10c7f),
    (0x10cb3, 0x10cbf),
    (0x10cf3, 0x10cf9),
    (0x10d28, 0x10d2f),
    (0x10d3a, 0x10d3f),
    (0x10d66, 0x10d68),
    (0x10d86, 0x10d8d),
    (0x10d90, 0x10e5f),
    (0x10e7f, 0x10e7f),
    (0x10eaa, 0x10eaa),
    (0x10eae, 0x10eaf),
    (0x10eb2, 0x10ec1),
    (0x10ec5, 0x10efb),
    (0x10f28, 0x10f2f),
    (0x10f5a, 0x10f6f),
    (0x10f8a, 0x10faf),
    (0x10fcc, 0x10fdf),
    (0x10ff7, 0x10fff),
    (0x1104e, 0x11051),
    (0x11076, 0x1107e),
    (0x110c3, 0x110cc),
    (0x110ce, 0x110cf),
    (0x110e9, 0x110ef),
    (0x110fa, 0x110ff),
    (0x11135, 0x11135),
    (0x11148, 0x1114f),
    (0x11177, 0x1117f),
    (0x111e0, 0x111e0),
    (0x111f5, 0x111ff),
    (0x11212, 0x11212),
    (0x11242, 0x1127f),
    (0x11287, 0x11287),
    (0x11289, 0x11289),
    (0x1128e, 0x1128e),
    (0x1129e, 0x1129e),
    (0x112aa, 0x112af),
    (0x112eb, 0x112ef),
    (0x112fa, 0x112ff),
    (0x11304, 0x11304),
    (0x1130d, 0x1130e),
    (0x11311, 0x11312),
    (0x11329, 0x11329),
    (0x11331, 0x11331),
    (0x11334, 0x11334),
    (0x1133a, 0x1133a),
    (0x11345, 0x11346),
    (0x11349, 0x1134a),
    (0x1134e, 0x1134f),
    (0x11351, 0x11356),
    (0x11358, 0x1135c),
    (0x11364, 0x11365),
    (0x1136d, 0x1136f),
    (0x11375, 0x1137f),
    (0x1138a, 0x1138a),
    (0x1138c, 0x1138d),
    (0x1138f, 0x1138f),
    (0x113b6, 0x113b6),
    (0x113c1, 0x113c1),
    (0x113c3, 0x113c4),
    (0x113c6, 0x113c6),
    (0x113cb, 0x113cb),
    (0x113d6, 0x113d6),
    (0x113d9, 0x113e0),
    (0x113e3, 0x113ff),
    (0x1145c, 0x1145c),
    (0x11462, 0x1147f),
    (0x114c8, 0x114cf),
    (0x114da, 0x1157f),
    (0x115b6, 0x115b7),
    (0x115de, 0x115ff),
    (0x11645, 0x1164f),
    (0x1165a, 0x1165f),
    (0x1166d, 0x1167f),
    (0x116ba, 0x116bf),
    (0x116ca, 0x116cf),
    (0x116e4, 0x116ff),
    (0x1171b, 0x1171c),
    (0x1172c, 0x1172f),
    (0x11747, 0x117ff),
    (0x1183c, 0x1189f),
    (0x118f3, 0x118fe),
    (0x11907, 0x11908),
    (0x1190a, 0x1190b),
    (0x11914, 0x11914),
    (0x11917, 0x11917),
    (0x11936, 0x11936),
    (0x11939, 0x1193a),
    (0x11947, 0x1194f),
    (0x1195a, 0x1199f),
    (0x119a8, 0x119a9),
    (0x119d8, 0x119d9),
    (0x119e5, 0x119ff),
    (0x11a48, 0x11a4f),
    (0x11aa3, 0x11aaf),
    (0x11af9, 0x11aff),
    (0x11b0a, 0x11bbf),
    (0x11be2, 0x11bef),
    (0x11bfa, 0x11bff),
    (0x11c09, 0x11c09),
    (0x11c37, 0x11c37),
    (0x11c46, 0x11c4f),
    (0x11c6d, 0x11c6f),
    (0x11c90, 0x11c91),
    (0x11ca8, 0x11ca8),
    (0x11cb7, 0x11cff),
    (0x11d07, 0x11d07),
    (0x11d0a, 0x11d0a),
    (0x11d37, 0x11d39),
    (0x11d3b, 0x11d3b),
    (0x11d3e, 0x11d3e),
    (0x11d48, 0x11d4f),
    (0x11d5a, 0x11d5f),
    (0x11d66, 0x11d66),
    (0x11d69, 0x11d69),
    (0x11d8f, 0x11d8f),
    (0x11d92, 0x11d92),
    (0x11d99, 0x11d9f),
    (0x11daa, 0x11edf),
    (0x11ef9, 0x11eff),
    (0x11f11, 0x11f11),
    (0x11f3b, 0x11f3d),
    (0x11f5b, 0x11faf),
    (0x11fb1, 0x11fbf),
    (0x11ff2, 0x11ffe),
    (0x1239a, 0x123ff),
    (0x1246f, 0x1246f),
    (0x12475, 0x1247f),
    (0x12544, 0x12f8f),
    (0x12ff3, 0x12fff),
    (0x13456, 0x1345f),
    (0x143fb, 0x143ff),
    (0x14647, 0x160ff),
    (0x1613a, 0x167ff),
    (0x16a39, 0x16a3f),
    (0x16a5f, 0x16a5f),
    (0x16a6a, 0x16a6d),
    (0x16abf, 0x16abf),
    (0x16aca, 0x16acf),
    (0x16aee, 0x16aef),
    (0x16af6, 0x16aff),
    (0x16b46, 0x16b4f),
    (0x16b5a, 0x16b5a),
    (0x16b62, 0x16b62),
    (0x16b78, 0x16b7c),
    (0x16b90, 0x16d3f),
    (0x16d7a, 0x16e3f),
    (0x16e9b, 0x16eff),
    (0x16f4b, 0x16f4e),
    (0x16f88, 0x16f8e),
    (0x16fa0, 0x16fdf),
    (0x16fe5, 0x16fef),
    (0x16ff2, 0x16fff),
    (0x187f8, 0x187ff),
    (0x18cd6, 0x18cfe),
    (0x18d09, 0x1afef),
    (0x1aff4, 0x1aff4),
    (0x1affc, 0x1affc),
    (0x1afff, 0x1afff),
    (0x1b123, 0x1b131),
    (0x1b133, 0x1b14f),
    (0x1b153, 0x1b154),
    (0x1b156, 0x1b163),
    (0x1b168, 0x1b16f),
    (0x1b2fc, 0x1bbff),
    (0x1bc6b, 0x1bc6f),
    (0x1bc7d, 0x1bc7f),
    (0x1bc89, 0x1bc8f),
    (0x1bc9a, 0x1bc9b),
    (0x1bca4, 0x1cbff),
    (0x1ccfa, 0x1ccff),
    (0x1ceb4, 0x1ceff),
    (0x1cf2e, 0x1cf2f),
    (0x1cf47, 0x1cf4f),
    (0x1cfc4, 0x1cfff),
    (0x1d0f6, 0x1d0ff),
    (0x1d127, 0x1d128),
    (0x1d1eb, 0x1d1ff),
    (0x1d246, 0x1d2bf),
    (0x1d2d4, 0x1d2df),
    (0x1d2f4, 0x1d2ff),
    (0x1d357, 0x1d35f),
    (0x1d379, 0x1d3ff),
    (0x1d455, 0x1d455),
    (0x1d49d, 0x1d49d),
    (0x1d4a0, 0x1d4a1),
    (0x1d4a3, 0x1d4a4),
    (0x1d4a7, 0x1d4a8),
    (0x1d4ad, 0x1d4ad),
    (0x1d4ba, 0x1d4ba),
    (0x1d4bc, 0x1d4bc),
    (0x1d4c4, 0x1d4c4),
    (0x1d506, 0x1d506),
    (0x1d50b, 0x1d50c),
    (0x1d515, 0x1d515),
    (0x1d51d, 0x1d51d),
    (0x1d53a, 0x1d53a),
    (0x1d53f, 0x1d53f),
    (0x1d545, 0x1d545),
    (0x1d547, 0x1d549),
    (0x1d551, 0x1d551),
    (0x1d6a6, 0x1d6a7),
    (0x1d7cc, 0x1d7cd),
    (0x1da8c, 0x1da9a),
    (0x1daa0, 0x1daa0),
    (0x1dab0, 0x1deff),
    (0x1df1f, 0x1df24),
    (0x1df2b, 0x1dfff),
    (0x1e007, 0x1e007),
    (0x1e019, 0x1e01a),
    (0x1e022, 0x1e022),
    (0x1e025, 0x1e025),
    (0x1e02b, 0x1e02f),
    (0x1e06e, 0x1e08e),
    (0x1e090, 0x1e0ff),
    (0x1e12d, 0x1e12f),
    (0x1e13e, 0x1e13f),
    (0x1e14a, 0x1e14d),
    (0x1e150, 0x1e28f),
    (0x1e2af, 0x1e2bf),
    (0x1e2fa, 0x1e2fe),
    (0x1e300, 0x1e4cf),
    (0x1e4fa, 0x1e5cf),
    (0x1e5fb, 0x1e5fe),
    (0x1e600, 0x1e7df),
    (0x1e7e7, 0x1e7e7),
    (0x1e7ec, 0x1e7ec),
    (0x1e7ef, 0x1e7ef),
    (0x1e7ff, 0x1e7ff),
    (0x1e8c5, 0x1e8c6),
    (0x1e8d7, 0x1e8ff),
    (0x1e94c, 0x1e94f),
    (0x1e95a, 0x1e95d),
    (0x1e960, 0x1ec70),
    (0x1ecb5, 0x1ed00),
    (0x1ed3e, 0x1edff),
    (0x1ee04, 0x1ee04),
    (0x1ee20, 0x1ee20),
    (0x1ee23, 0x1ee23),
    (0x1ee25, 0x1ee26),
    (0x1ee28, 0x1ee28),
    (0x1ee33, 0x1ee33),
    (0x1ee38, 0x1ee38),
    (0x1ee3a, 0x1ee3a),
    (0x1ee3c, 0x1ee41),
    (0x1ee43, 0x1ee46),
    (0x1ee48, 0x1ee48),
    (0x1ee4a, 0x1ee4a),
    (0x1ee4c, 0x1ee4c),
    (0x1ee50, 0x1ee50),
    (0x1ee53, 0x1ee53),
    (0x1ee55, 0x1ee56),
    (0x1ee58, 0x1ee58),
    (0x1ee5a, 0x1ee5a),
    (0x1ee5c, 0x1ee5c),
    (0x1ee5e, 0x1ee5e),
    (0x1ee60, 0x1ee60),
    (0x1ee63, 0x1ee63),
    (0x1ee65, 0x1ee66),
    (0x1ee6b, 0x1ee6b),
    (0x1ee73, 0x1ee73),
    (0x1ee78, 0x1ee78),
    (0x1ee7d, 0x1ee7d),
    (0x1ee7f, 0x1ee7f),
    (0x1ee8a, 0x1ee8a),
    (0x1ee9c, 0x1eea0),
    (0x1eea4, 0x1eea4),
    (0x1eeaa, 0x1eeaa),
    (0x1eebc, 0x1eeef),
    (0x1eef2, 0x1efff),
    (0x1f02c, 0x1f02f),
    (0x1f094, 0x1f09f),
    (0x1f0af, 0x1f0b0),
    (0x1f0c0, 0x1f0c0),
    (0x1f0d0, 0x1f0d0),
    (0x1f0f6, 0x1f0ff),
    (0x1f1ae, 0x1f1e5),
    (0x1f203, 0x1f20f),
    (0x1f23c, 0x1f23f),
    (0x1f249, 0x1f24f),
    (0x1f252, 0x1f25f),
    (0x1f266, 0x1f2ff),
    (0x1f6d8, 0x1f6db),
    (0x1f6ed, 0x1f6ef),
    (0x1f6fd, 0x1f6ff),
    (0x1f777, 0x1f77a),
    (0x1f7da, 0x1f7df),
    (0x1f7ec, 0x1f7ef),
    (0x1f7f1, 0x1f7ff),
    (0x1f80c, 0x1f80f),
    (0x1f848, 0x1f84f),
    (0x1f85a, 0x1f85f),
    (0x1f888, 0x1f88f),
    (0x1f8ae, 0x1f8af),
    (0x1f8bc, 0x1f8bf),
    (0x1f8c2, 0x1f8ff),
    (0x1fa54, 0x1fa5f),
    (0x1fa6e, 0x1fa6f),
    (0x1fa7d, 0x1fa7f),
    (0x1fa8a, 0x1fa8e),
    (0x1fac7, 0x1facd),
    (0x1fadd, 0x1fade),
    (0x1faea, 0x1faef),
    (0x1faf9, 0x1faff),
    (0x1fb93, 0x1fb93),
    (0x1fbfa, 0x1ffff),
    (0x2a6e0, 0x2a6ff),
    (0x2b73a, 0x2b73f),
    (0x2b81e, 0x2b81f),
    (0x2cea2, 0x2ceaf),
    (0x2ebe1, 0x2ebef),
    (0x2ee5e, 0x2f7ff),
    (0x2fa1e, 0x2ffff),
    (0x3134b, 0x3134f),
    (0x323b0, 0xe0000),
    (0xe0002, 0xe001f),
    (0xe0080, 0xe00ff),
    (0xe01f0, 0xeffff),
    (0xffffe, 0xfffff),
    (0x10fffe, 0x10ffff),
];

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

    #[test]
    fn tz_names_a_fixed_zone_as_glibc_reads_it() {
        assert_eq!(tz_fixed_offset("UTC"), Some(0));
        assert_eq!(tz_fixed_offset("UTC0"), Some(0));
        assert_eq!(tz_fixed_offset("GMT"), Some(0));
        assert_eq!(tz_fixed_offset("EST5"), Some(-5 * 3600));
        assert_eq!(tz_fixed_offset("JST-9"), Some(9 * 3600));
        assert_eq!(tz_fixed_offset("<+0530>-5:30"), Some(5 * 3600 + 30 * 60));
        assert_eq!(tz_fixed_offset("Europe/Paris"), None);
        assert_eq!(tz_fixed_offset("CET-1CEST,M3.5.0,M10.5.0/3"), None);
        assert_eq!(tz_fixed_offset(""), None);
    }

    #[test]
    fn char_width_matches_glibc_c_utf8() {
        let wcwidth = |c: i32| wcswidth_n(&[c], 1);
        assert_eq!(wcwidth('a' as i32), 1);
        assert_eq!(wcwidth('中' as i32), 2);
        assert_eq!(wcwidth('\u{ad}' as i32), 1);
        assert_eq!(wcwidth('\u{0301}' as i32), 0);
        assert_eq!(wcwidth('\u{0378}' as i32), -1);
    }
}
