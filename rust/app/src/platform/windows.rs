//! The Windows backend: byte paths are WTF-8 (`OsStr::as_encoded_bytes`), shown with `/`.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;

/// Decodes WTF-8 (UTF-8 that may hold unpaired surrogates) to UTF-16; a byte that is not part of
/// a sequence becomes U+FFFD.
fn wtf8_to_wide(b: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let (len, init) = match c {
            0x00..=0x7F => (1, u32::from(c)),
            0xC2..=0xDF => (2, u32::from(c & 0x1F)),
            0xE0..=0xEF => (3, u32::from(c & 0x0F)),
            0xF0..=0xF4 => (4, u32::from(c & 0x07)),
            _ => (0, 0),
        };
        let cont = b
            .get(i + 1..i + len)
            .filter(|t| t.iter().all(|&x| x & 0xC0 == 0x80));
        let cp = match (len, cont) {
            (0, _) | (_, None) => None,
            (_, Some(tail)) => {
                let cp = tail.iter().fold(init, |a, &x| a << 6 | u32::from(x & 0x3F));
                let min = [0, 0, 0x80, 0x800, 0x10000][len];
                (cp >= min && cp <= 0x10FFFF).then_some(cp)
            }
        };
        match cp {
            Some(cp) => {
                i += len;
                if cp >= 0x10000 {
                    let v = cp - 0x10000;
                    out.push(0xD800 | (v >> 10) as u16);
                    out.push(0xDC00 | (v & 0x3FF) as u16);
                } else {
                    out.push(cp as u16);
                }
            }
            None => {
                i += 1;
                out.push(0xFFFD);
            }
        }
    }
    out
}

pub fn os(b: &[u8]) -> Cow<'_, OsStr> {
    match std::str::from_utf8(b) {
        Ok(s) => Cow::Borrowed(OsStr::new(s)),
        Err(_) => Cow::Owned(OsString::from_wide(&wtf8_to_wide(b))),
    }
}

pub fn bytes(s: &OsStr) -> Cow<'_, [u8]> {
    let b = s.as_encoded_bytes();
    if b.contains(&b'\\') {
        Cow::Owned(
            b.iter()
                .map(|&c| if c == b'\\' { b'/' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wtf8_round_trip_unpaired_surrogate() {
        let s = OsString::from_wide(&[0x61, 0xD800, 0x62]);
        let b = bytes(&s).into_owned();
        assert_eq!(b, b"a\xED\xA0\x80b");
        assert_eq!(os(&b).as_encoded_bytes(), s.as_encoded_bytes());
    }

    #[test]
    fn backslashes_become_slashes() {
        assert_eq!(&*bytes(OsStr::new("C:\\a\\b")), b"C:/a/b");
    }
}
