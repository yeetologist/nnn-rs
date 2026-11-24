//! The Rust side of rust/oracle/popup_oracle.c (run by rust/oracle/popup_run.sh): the same
//! calls answered by rust/app/src/popup.rs.

use nnn::popup;
use nnn::sort::Entry;
use nnn::state::Nnn;
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

/// A C string read back from a hex field: up to the first NUL.
fn cs(h: &str) -> Vec<u8> {
    let v = unhex(h);
    let end = v.iter().position(|&c| c == 0).unwrap_or(v.len());
    v[..end].to_vec()
}

fn opt(v: &[u8]) -> Option<&[u8]> {
    (!v.is_empty()).then_some(v)
}

fn call(nnn: &mut Nnn, f: &[&str]) -> Vec<u8> {
    let arg = |i: usize| match f.get(i).copied() {
        Some("-") | None => "",
        Some(s) => s,
    };
    let num = |i: usize| arg(i).parse::<i32>().unwrap_or(0);
    match f[0] {
        "parseargs" => match popup::parseargs(&cs(arg(1))) {
            None => b"NULL".to_vec(),
            Some(v) => {
                let mut s = v.len().to_string();
                for a in &v {
                    s.push(' ');
                    s += &hex(a);
                }
                s.into_bytes()
            }
        },
        "mkpath" => hex(&popup::mkpath(b"/home/u", &cs(arg(1)), &cs(arg(2)))).into_bytes(),
        "bufout" => {
            let (a, b, c) = (cs(arg(1)), cs(arg(2)), cs(arg(3)));
            match nnn.buffer_command_output(&[&a, &b], opt(&c), None) {
                Some(v) => hex(&v).into_bytes(),
                None => b"NULL".to_vec(),
            }
        }
        "getout" => {
            let (a, b) = (cs(arg(1)), cs(arg(2)));
            match nnn.get_output_buf(&a, opt(&b), None) {
                Some(v) => {
                    let end = v.iter().position(|&c| c == 0).unwrap_or(v.len());
                    hex(&v[..end]).into_bytes()
                }
                None => b"NULL".to_vec(),
            }
        }
        "fsinfo" => popup::get_fs_info(&cs(arg(1)), num(2) as u8)
            .to_string()
            .into_bytes(),
        "help" => {
            nnn.cfg.curctx = num(1) as usize;
            let mask = num(2);
            let paths: Vec<&str> = arg(4).split(',').collect();
            for (i, ctx) in nnn.g_ctx.iter_mut().enumerate() {
                ctx.c_cfg.ctxactive = (mask >> i) & 1 != 0;
                ctx.c_path = cs(paths.get(i).copied().unwrap_or(""));
            }
            nnn.popup.curssn = cs(arg(5));
            hex(&nnn.help_text(&cs(arg(3)))).into_bytes()
        }
        "float" => {
            let (lines, cols, perfile) = (num(1), num(2), num(3) != 0);
            let (ndents, cur) = (num(4), num(5));
            let name = cs(arg(6));
            let content = unhex(arg(7));
            let keys: Vec<i32> = arg(8).split(',').filter_map(|k| k.parse().ok()).collect();
            nnn.pdents = vec![
                Entry {
                    name: name.clone(),
                    ..Entry::default()
                };
                ndents.max(0) as usize
            ];
            let title = (ndents != 0).then(|| ((cur + 1) as u32, ndents as u32, name.as_slice()));
            match popup::floating_rows(lines, cols, &content, title, perfile, &keys) {
                None => b"NULL".to_vec(),
                Some(rows) => rows
                    .iter()
                    .map(|r| hex(r.as_bytes()))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into_bytes(),
            }
        }
        _ => Vec::new(),
    }
}

fn main() {
    nnn::locale::init();
    let mut nnn = Nnn {
        home: b"/home/u".to_vec(),
        pager: b"cat".to_vec(),
        g_tmpfpath: b"/tmp".to_vec(),
        ..Nnn::default()
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let f: Vec<&str> = line.split('\t').filter(|s| !s.is_empty()).collect();
        if f.is_empty() {
            continue;
        }
        let r = call(&mut nnn, &f);
        let _ = out.write_all(&r);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}
