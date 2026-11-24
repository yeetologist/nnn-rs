//! The glibc functions whose results depend on the locale, the time zone or POSIX regex
//! syntax: the original calls them, so the port calls the same ones (ADR-014). This is
//! the only module with `unsafe`.

use std::ffi::{CString, c_char, c_int};

pub type Wide = Vec<i32>;

unsafe extern "C" {
    fn towlower(wc: u32) -> u32;
    fn iswalnum(wc: u32) -> c_int;
    fn wcswidth(s: *const libc::wchar_t, n: libc::size_t) -> c_int;
    #[link_name = "mbstowcs"]
    fn c_mbstowcs(dest: *mut libc::wchar_t, src: *const c_char, n: libc::size_t) -> libc::size_t;
    #[link_name = "mblen"]
    fn c_mblen(s: *const c_char, n: libc::size_t) -> c_int;
}

fn cstr(s: &[u8]) -> CString {
    // INFALLIBLE: names and filters come from C strings and hold no NUL; cut at one if they do.
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
}

/// `setlocale(LC_COLLATE, ""); setlocale(LC_CTYPE, "")`, as `main()` does.
pub fn init() {
    // SAFETY: called once from main before any other thread starts; the argument is a
    // valid NUL-terminated string.
    unsafe {
        libc::setlocale(libc::LC_COLLATE, c"".as_ptr());
        libc::setlocale(libc::LC_CTYPE, c"".as_ptr());
    }
}

pub fn strcoll(a: &[u8], b: &[u8]) -> i32 {
    let (a, b) = (cstr(a), cstr(b));
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    unsafe { libc::strcoll(a.as_ptr(), b.as_ptr()) }
}

pub fn strcasecmp(a: &[u8], b: &[u8]) -> i32 {
    let (a, b) = (cstr(a), cstr(b));
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    unsafe { libc::strcasecmp(a.as_ptr(), b.as_ptr()) }
}

/// `tolower()` on one byte, as glibc's `strcasestr` folds.
pub fn tolower(c: u8) -> u8 {
    // SAFETY: tolower takes any unsigned char value.
    (unsafe { libc::tolower(c_int::from(c)) }) as u8
}

/// `mbstowcs(dst, s, max)`: at most `max` wide characters, `None` for `(size_t)-1`.
pub fn mbstowcs(s: &[u8], max: usize) -> Option<Wide> {
    let src = cstr(s);
    let mut buf: Vec<libc::wchar_t> = vec![0; max + 1];
    // SAFETY: buf holds max + 1 wide chars and mbstowcs writes at most max of them;
    // src is NUL-terminated.
    let n = unsafe { c_mbstowcs(buf.as_mut_ptr(), src.as_ptr(), max) };
    if n == usize::MAX {
        return None;
    }
    buf.truncate(n);
    Some(buf)
}

/// `mblen(s, MB_LEN_MAX)` at the start of `s`.
pub fn mblen_at(s: &[u8]) -> i32 {
    let src = cstr(s);
    // SAFETY: src is NUL-terminated, so mblen never reads past it.
    unsafe { c_mblen(src.as_ptr(), 16) }
}

pub fn to_lower_wide(c: i32) -> i32 {
    // SAFETY: towlower takes any wint_t.
    (unsafe { towlower(c as u32) }) as i32
}

/// `wcswidth(s, n)`: -1 when a character in the first `n` is not printable.
pub fn wcswidth_n(s: &[i32], n: usize) -> i32 {
    let mut buf: Vec<libc::wchar_t> = s.to_vec();
    buf.push(0);
    // SAFETY: buf is NUL-terminated and wcswidth stops at the NUL or after n chars.
    unsafe { wcswidth(buf.as_ptr(), n) }
}

/// `wcstombs`: the wide string back to the locale's bytes, `None` for `(size_t)-1`.
pub fn wcstombs(s: &[i32]) -> Option<Vec<u8>> {
    let mut buf: Vec<libc::wchar_t> = s.to_vec();
    buf.push(0);
    let mut out = vec![0u8; s.len() * 16 + 1];
    // SAFETY: out has room for every char at the maximum multibyte length plus the NUL.
    let n = unsafe { libc::wcstombs(out.as_mut_ptr().cast(), buf.as_ptr(), out.len()) };
    if n == usize::MAX {
        return None;
    }
    out.truncate(n);
    Some(out)
}

/// The fields of `localtime_r` the listing prints.
pub struct LocalTime {
    pub year: i32,
    pub mon: i32,
    pub mday: i32,
    pub hour: i32,
    pub min: i32,
}

pub fn localtime(secs: i64) -> LocalTime {
    // SAFETY: tm is plain data; zeroed is a valid value for every field.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t: libc::time_t = secs;
    // SAFETY: both pointers are valid for the call.
    unsafe { libc::localtime_r(&t, &mut tm) };
    LocalTime {
        year: tm.tm_year + 1900,
        mon: tm.tm_mon + 1,
        mday: tm.tm_mday,
        hour: tm.tm_hour,
        min: tm.tm_min,
    }
}

/// A compiled POSIX regex (`regcomp` with `REG_NOSUB | REG_EXTENDED [| REG_ICASE]`).
pub struct Regex(Box<libc::regex_t>);

impl Regex {
    pub fn new(pattern: &[u8], icase: bool) -> Option<Regex> {
        let p = cstr(pattern);
        // SAFETY: regex_t is plain data that regcomp initialises.
        let mut re: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
        let flags = libc::REG_NOSUB | libc::REG_EXTENDED | if icase { libc::REG_ICASE } else { 0 };
        // SAFETY: re points to writable storage for a regex_t, p is NUL-terminated.
        let rc = unsafe { libc::regcomp(&mut *re, p.as_ptr(), flags) };
        (rc == 0).then_some(Regex(re))
    }

    pub fn is_match(&self, s: &[u8]) -> bool {
        let s = cstr(s);
        // SAFETY: self.0 was compiled by regcomp; REG_NOSUB needs no match array.
        unsafe { libc::regexec(&*self.0, s.as_ptr(), 0, std::ptr::null_mut(), 0) == 0 }
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: self.0 was compiled by regcomp and is freed once.
        unsafe { libc::regfree(&mut *self.0) };
    }
}

/// glibc `iswalnum` in the current LC_CTYPE.
pub fn is_walnum(c: i32) -> bool {
    // SAFETY: iswalnum takes any wint_t.
    (unsafe { iswalnum(c as u32) }) != 0
}

/// `setenv(name, value, 1)`: variables nnn exports to the programs it starts.
pub fn setenv(name: &str, value: &[u8]) {
    let v = &*crate::platform::os(value);
    // SAFETY: nnn sets these from its main thread at points where no other thread reads or
    // writes the environment (the du workers never touch it), as the C original does.
    unsafe { std::env::set_var(name, v) };
}

/// `unsetenv(name)`
pub fn unsetenv(name: &str) {
    // SAFETY: as for setenv.
    unsafe { std::env::remove_var(name) };
}
