//! The locale, time zone and regex functions nnn calls, as glibc defines them (ADR-014).
//!
//! Two backends give the same API: glibc through FFI (`platform::unix::glibc`, the default on
//! Unix) and a pure-Rust one that reproduces glibc's `C.UTF-8` (`platform::portable_locale`,
//! always on Windows, and on Unix with the `portable` feature). This module has no `unsafe`.

#[cfg(any(windows, feature = "portable"))]
use crate::platform::portable_locale as imp;
#[cfg(not(any(windows, feature = "portable")))]
use crate::platform::unix::glibc as imp;

pub use crate::platform::{setenv, unsetenv};
pub use imp::{
    Regex, init, is_walnum, localtime, mblen_at, mbstowcs, strcasecmp, strcoll, to_lower_wide,
    tolower, wcstombs, wcswidth_n,
};

pub type Wide = Vec<i32>;

/// The fields of `localtime_r` the listing prints.
pub struct LocalTime {
    pub year: i32,
    pub mon: i32,
    pub mday: i32,
    pub hour: i32,
    pub min: i32,
    pub sec: i32,
}
