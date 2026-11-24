//! The Rust side of the differential oracle (rust/oracle/oracle.c speaks the same protocol).

use nnn::dir::FillCfg;
use nnn::fmt;
use nnn::locale;
use nnn::sort::{self, FILE_YOUNG, SortCfg, TimeType};
use nnn::state::Nnn;
use std::cmp::Ordering;
use std::io::{BufRead, Write};

fn unhex(h: &str) -> Vec<u8> {
    h.as_bytes()
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap_or("00"), 16).unwrap_or(0))
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn ord(o: Ordering) -> i32 {
    o as i32
}

fn positions(p: &[usize]) -> String {
    p.iter().map(usize::to_string).collect::<Vec<_>>().join(",")
}

fn list(path: &[u8], flags: &str, nnn: &mut Nnn) -> String {
    let mut c = SortCfg::default();
    let mut fill = FillCfg::default();
    for f in flags.chars() {
        match f {
            'H' => fill.showhidden = true,
            't' => c.timeorder = true,
            's' => c.sizeorder = true,
            'e' => c.extnorder = true,
            'r' => c.reverse = true,
            'v' => c.version = true,
            'A' => fill.timetype = TimeType::Access,
            'C' => fill.timetype = TimeType::Change,
            'd' => c.blkorder = true,
            'a' => {
                c.blkorder = true;
                c.apparentsz = true;
            }
            _ => {}
        }
    }
    c.rev_fn = c.reverse;
    c.vers_fn = c.version;
    nnn.cfg.sort.blkorder = c.blkorder;
    nnn.cfg.sort.apparentsz = c.apparentsz;
    let mut ents = nnn.dentfill(path, fill);
    ents.sort_by(|a, b| c.cmp(a, b));
    let mut out = ents.len().to_string();
    for e in &ents {
        out += &format!(
            " {}:{:o}:{}:{}:{}:{:x}:{}:{}",
            hex(&e.name),
            e.mode,
            e.size,
            e.sec,
            e.nsec,
            e.flags & !FILE_YOUNG,
            e.uid,
            e.gid
        );
        if c.blkorder {
            out += &format!(":{}", e.blocks);
        }
    }
    if c.blkorder {
        out += &format!(" |{}:{}", nnn.num_files, nnn.dir_blocks);
    }
    out
}

fn call(f: &[&str], nnn: &mut Nnn) -> String {
    let arg = |i: usize| f.get(i).copied().unwrap_or("");
    let ic = arg(1) == "1";
    match f[0] {
        "stricmp" => ord(sort::xstricmp(&unhex(arg(1)), &unhex(arg(2)))).to_string(),
        "verscmp" => ord(sort::xstrverscasecmp(&unhex(arg(1)), &unhex(arg(2)))).to_string(),
        "fuzzy" => u8::from(sort::fuzzy_match(&unhex(arg(2)), &unhex(arg(3)), ic)).to_string(),
        "fscore" => sort::fuzzy_match_score(&unhex(arg(2)), &unhex(arg(3)), ic).to_string(),
        "strstr" => u8::from(sort::visible_str(&unhex(arg(2)), &unhex(arg(3)), ic)).to_string(),
        "fpos" => positions(&sort::fuzzy_match_positions(
            &unhex(arg(2)),
            &unhex(arg(3)),
            ic,
        )),
        "spos" => positions(&sort::string_match_positions(
            &unhex(arg(2)),
            &unhex(arg(3)),
            ic,
        )),
        "regex" => match locale::Regex::new(&unhex(arg(2)), ic) {
            None => "bad".to_string(),
            Some(re) => u8::from(re.is_match(&unhex(arg(3)))).to_string(),
        },
        "coolsize" => fmt::coolsize(arg(1).parse().unwrap_or(0)),
        "perms" => {
            let mode = arg(1).parse().unwrap_or(0);
            format!("{} {}", fmt::get_lsperms(mode), fmt::get_detail_ind(mode))
        }
        "unescape" => {
            let w = fmt::unescape(&unhex(arg(1)), arg(2).parse().unwrap_or(0));
            locale::wcstombs(&w).map_or_else(|| "bad".to_string(), |b| hex(&b))
        }
        "time" => fmt::print_time(arg(1).parse().unwrap_or(0)),
        "sortflags" => {
            let mut c = SortCfg::default();
            let mut out = String::new();
            if arg(1) != "-" {
                for k in arg(1).bytes() {
                    let (r, _) = c.set_sort_flags(if k == b'^' { 0x14 } else { k });
                    out += &format!("{r},");
                }
            }
            let b = |v: bool| u8::from(v);
            format!(
                "{out}[{}] {}{}{}{}{}",
                c.getorderstr(),
                b(c.timeorder),
                b(c.sizeorder),
                b(c.extnorder),
                b(c.reverse),
                b(c.version)
            )
        }
        "list" => list(&unhex(arg(1)), arg(2), nnn),
        _ => "?".to_string(),
    }
}

fn main() {
    locale::init();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut nnn = Nnn::default();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let f: Vec<&str> = line.split('\t').filter(|s| !s.is_empty()).collect();
        if f.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{}", call(&f, &mut nnn));
    }
}
