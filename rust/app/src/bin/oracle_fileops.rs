//! Rust side of rust/oracle/fileops_oracle.c (same line protocol).

use nnn::fileops::{entries_in_file, opstr, p_cpmvfmt, p_cpmvrnm};
use std::io::{BufRead, Seek, SeekFrom, Write};

fn unhex(h: &str) -> Vec<u8> {
    h.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or("00"), 16).unwrap_or(0))
        .collect()
}

/// The C side's char buffers: a string ends at its first NUL.
fn cstr(v: Vec<u8>) -> Vec<u8> {
    let end = v.iter().position(|&c| c == 0).unwrap_or(v.len());
    v[..end].to_vec()
}

fn hex(s: &[u8]) -> String {
    s.iter().map(|c| format!("{c:02x}")).collect()
}

fn main() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let dir = std::env::temp_dir();
    for line in stdin.lock().lines() {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        let arg = |i: usize| f.get(i).copied().unwrap_or("");
        let op = arg(0);
        let res = match op {
            "E" => {
                let buflen: usize = arg(1).parse().unwrap_or(0);
                let delim = arg(2).parse::<i32>().unwrap_or(0) as u8;
                let p = dir.join(format!("nnn-fo-oracle-rs.{}", std::process::id()));
                let mut file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&p)?;
                file.write_all(&unhex(arg(3)))?;
                file.seek(SeekFrom::Start(0))?;
                let n = entries_in_file(&mut file, buflen, delim);
                std::fs::remove_file(&p)?;
                n.to_string()
            }
            "O" => hex(&opstr(&cstr(unhex(arg(1))), &cstr(unhex(arg(2))))),
            "F" => hex(&p_cpmvfmt(&cstr(unhex(arg(1))))),
            "N" => hex(&p_cpmvrnm(
                &cstr(unhex(arg(1))),
                &cstr(unhex(arg(2))),
                &cstr(unhex(arg(3))),
            )),
            _ => String::new(),
        };
        writeln!(out, "{op} {res}")?;
        out.flush()?;
    }
    Ok(())
}
