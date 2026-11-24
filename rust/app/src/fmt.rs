//! How entries are shown (src/nnn.c: coolsize, get_lsperms, get_detail_ind, unescape,
//! print_time).

use crate::locale;

const S_IFMT: u32 = 0o170000;
const S_IFSOCK: u32 = 0o140000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;
const S_IFBLK: u32 = 0o060000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;
const S_IFIFO: u32 = 0o010000;

/// `get_size`: round the remainder and carry into the integer part.
fn get_size(size: i64, rem: &mut i64, comp: i64) -> i64 {
    let quo = *rem / 10;
    if *rem - quo * 10 >= 5 {
        *rem = quo + 1;
        if *rem == comp {
            *rem = 0;
            return size + 1;
        }
    } else {
        *rem = quo;
    }
    size
}

/// `xitoa`: the decimal digits of an unsigned 32-bit value.
fn xitoa(v: i64) -> String {
    (v as u32).to_string()
}

/// `coolsize`: a size in 1024 units, e.g. "4.9K".
pub fn coolsize(mut size: i64) -> String {
    const U: &[u8] = b"BKMGTPEZY";
    let mut rem = 0;
    let mut i = 0;
    while size >= 1024 {
        rem = size & 0x3FF;
        size >>= 10;
        i += 1;
    }
    match i {
        1 => {
            rem = ((rem * 1000) >> 10) / 10;
            size = get_size(size, &mut rem, 10);
        }
        2 => {
            rem = (rem * 1000) >> 10;
            size = get_size(size, &mut rem, 100);
        }
        3.. => {
            rem = (rem * 10000) >> 10;
            size = get_size(size, &mut rem, 1000);
        }
        _ => {}
    }
    let mut out = if i > 0 && i < 6 && rem != 0 {
        let toprint = i.min(3);
        format!("{}.{:0>toprint$}", xitoa(size), xitoa(rem))
    } else if size != 0 {
        xitoa(size)
    } else {
        "0".to_string()
    };
    out.push(char::from(U[i]));
    out
}

/// `get_lsperms`: "ls -l" style permissions.
pub fn get_lsperms(mode: u32) -> String {
    const RWX: [&str; 8] = ["---", "--x", "-w-", "-wx", "r--", "r-x", "rw-", "rwx"];
    let kind = match mode & S_IFMT {
        S_IFREG => '-',
        S_IFDIR => 'd',
        S_IFLNK => 'l',
        S_IFSOCK => 's',
        S_IFIFO => 'p',
        S_IFBLK => 'b',
        S_IFCHR => 'c',
        _ => '?',
    };
    let mut b: Vec<char> = std::iter::once(kind)
        .chain(
            [(mode >> 6) & 7, (mode >> 3) & 7, mode & 7]
                .iter()
                .flat_map(|&p| RWX[p as usize].chars()),
        )
        .collect();
    if mode & 0o4000 != 0 {
        b[3] = if mode & 0o100 != 0 { 's' } else { 'S' };
    }
    if mode & 0o2000 != 0 {
        b[6] = if mode & 0o010 != 0 { 's' } else { 'l' };
    }
    if mode & 0o1000 != 0 {
        b[9] = if mode & 0o001 != 0 { 't' } else { 'T' };
    }
    b.into_iter().collect()
}

/// `get_detail_ind`: the type indicator in detail mode.
pub fn get_detail_ind(mode: u32) -> char {
    match mode & S_IFMT {
        S_IFDIR | S_IFREG => ' ',
        S_IFLNK => '@',
        S_IFSOCK => '=',
        S_IFIFO => '|',
        S_IFBLK => 'b',
        S_IFCHR => 'c',
        _ => '?',
    }
}

/// `unescape`: the name as wide characters, controls shown as '?', cut to `maxcols`
/// columns.
pub fn unescape(s: &[u8], maxcols: usize) -> Vec<i32> {
    let mut w = locale::mbstowcs(s, maxcols).unwrap_or_default();
    for c in &mut w {
        if *c <= 0x1f || *c == 0x7f {
            *c = b'?' as i32;
        }
    }
    let width = |n: usize| locale::wcswidth_n(&w, n) as isize as usize;
    let mut len = width(w.len());
    if (len as isize) < 0 {
        len = w.len();
    }
    if len >= maxcols {
        let mut lencount = maxcols;
        while len > maxcols {
            lencount -= 1;
            len = width(lencount);
        }
        w.truncate(lencount);
    }
    w
}

/// `print_time`: "YYYY-MM-DD HH:MM" in local time.
pub fn print_time(secs: i64) -> String {
    let t = locale::localtime(secs);
    format!(
        "{}-{:02}-{:02} {:02}:{:02}",
        xitoa(i64::from(t.year)),
        t.mon,
        t.mday,
        t.hour,
        t.min
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(coolsize(0), "0B");
        assert_eq!(coolsize(5000), "4.9K");
        assert_eq!(coolsize(1024), "1K");
        assert_eq!(coolsize(1_048_575), "1024K");
    }

    #[test]
    fn perms() {
        assert_eq!(get_lsperms(0o100644), "-rw-r--r--");
        assert_eq!(get_lsperms(0o42755), "drwxr-sr-x");
        assert_eq!(get_lsperms(0o41777), "drwxrwxrwt");
    }
}
