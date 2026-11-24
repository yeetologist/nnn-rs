//! Rust side of rust/oracle/browse_oracle.c: browse.rs's pure functions on the same
//! line protocol ("<fn>\t<arg in hex>" in, one line out). Run by rust/oracle/browse_run.py.

use nnn::browse::{C_BLK, C_UND, is_text_file, parse_fcolors, xextension};
use std::io::{BufRead, Write};

fn unhex(h: &[u8]) -> Vec<u8> {
    let v = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    let mut out: Vec<u8> = h
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| (v(p[0]) << 4) | v(p[1]))
        .collect();
    // The C side reads the argument as a C string.
    if let Some(at) = out.iter().position(|&c| c == 0) {
        out.truncate(at);
    }
    out
}

fn hex(s: &[u8]) -> String {
    s.iter().map(|c| format!("{c:02x}")).collect()
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().split(b'\n') {
        let Ok(line) = line else {
            break;
        };
        let Some(tab) = line.iter().position(|&c| c == b'\t') else {
            continue;
        };
        let arg = unhex(&line[tab + 1..]);
        let reply = match &line[..tab] {
            b"text" => format!("{}", i32::from(is_text_file(&arg))),
            b"ext" => xextension(&arg).map_or_else(|| "NULL".to_string(), hex),
            b"fcolors" => {
                let mut f = [0u32; C_UND + 1];
                let ok = parse_fcolors(&arg, &mut f);
                let mut s = format!("{}", i32::from(ok));
                for c in &f[C_BLK..=C_UND] {
                    s.push_str(&format!(" {c}"));
                }
                s
            }
            _ => continue,
        };
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
}
