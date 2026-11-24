# Portable locale backend: known differences from glibc C.UTF-8

`platform/portable_locale.rs` (Windows, and Unix with `--features portable`) is proven with
`LOCALES=C.UTF-8 rust/oracle/run.sh <oracle built with --features portable,oracles>`: 3/3 runs
identical for seeds 1, 7 and 100 (20000 calls each). The oracle's generator cannot reach the
classes below; they were measured by comparing every code point with glibc 2.43 (Unicode 17).

- **Unicode version.** The width, class and case tables come from Rust (unicode-width, regex-syntax,
  std), glibc 2.43's from its own Unicode release. About 1.2k code points that glibc already knows
  are unassigned for Rust (for example U+088F, U+16FF2): `wcswidth` gives -1 instead of 1 or 2 for
  them, `[[:alpha:]]` and `iswalnum` are false, and `towlower` differs for about 20 (U+A7DD, U+1DF40...).
- **`wcwidth` oddities.** glibc differs from unicode-width on about 100 more code points: U+0605
  and other Arabic number signs (glibc 1, Rust 0), U+2028 and U+2029 (glibc -1, Rust 1), U+1ACF..
  (glibc 0, Rust -1 as unassigned), U+302E and U+3248..U+324F (glibc 2, Rust 0 or 1), U+2D7F, U+17A4
  and U+17D8.
- **`[[:graph:]]`, `[[:print:]]`, `[[:punct:]]`.** Approximated with Unicode general categories
  (assigned and not control or separator; `\p{P}\p{S}`); glibc's tables differ by about 4.8k and
  140k code points (marks and format characters count as punctuation in glibc). `alpha`, `alnum`,
  `upper`, `lower` (apart from the Unicode version) and `space` match glibc; `digit` is 0-9.
- **Back-references** (`\1`..`\9`) in a pattern make `Regex::new` return `None`; glibc supports them.
- **Pattern bytes that are not UTF-8** match themselves, but a pattern that really contains
  U+10FF00..U+10FFFF is read as raw bytes, and a raw byte inside a bracket expression is an error.
- **Time zones** come from the OS through chrono (`TZ` is honoured on Unix); the oracle runs UTC,
  America/New_York and Asia/Kolkata identically, but glibc's handling of malformed `TZ` strings is
  not copied.
