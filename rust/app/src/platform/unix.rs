//! The Unix backend: byte paths are the OS's own bytes.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

pub fn os(b: &[u8]) -> Cow<'_, OsStr> {
    Cow::Borrowed(OsStr::from_bytes(b))
}

pub fn bytes(s: &OsStr) -> Cow<'_, [u8]> {
    Cow::Borrowed(s.as_bytes())
}
