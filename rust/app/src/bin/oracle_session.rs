//! The Rust side of rust/oracle/session_oracle.c (run by rust/oracle/session_run.sh): the
//! same commands answered by rust/app/src/session.rs. Commands come from the file
//! argv[1], answers go to argv[2]; stdin must be a terminal (curses).

use nnn::session::{set_settings_bits, settings_bits};
use nnn::sort::Entry;
use nnn::state::{CTX_MAX, Nnn, REGEX_MAX};
use std::io::{BufRead, Write};

fn unhex(h: &str) -> Vec<u8> {
    h.as_bytes()
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap_or("00"), 16).unwrap_or(0))
        .collect()
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".into();
    }
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn cs(h: &str) -> Vec<u8> {
    let v = unhex(h);
    let end = v.iter().position(|&c| c == 0).unwrap_or(v.len());
    v[..end].to_vec()
}

/// The C side's `reset()`.
fn reset(nnn: &mut Nnn, cfgw: u32) {
    for (i, c) in nnn.g_ctx.iter_mut().enumerate() {
        *c = Default::default();
        c.c_path = format!("/old/path{i}").into_bytes();
        c.c_fltr = vec![0; REGEX_MAX];
        c.c_fltr[0] = b'/';
        let f = format!("oldf{i}");
        c.c_fltr[1..1 + f.len()].copy_from_slice(f.as_bytes());
        c.color = 100 + i as u32;
        set_settings_bits(&mut c.c_cfg, (0x200 * (i as u32 & 1)) | i as u32);
        c.c_cfg.sort.rev_fn = false;
        c.c_cfg.sort.vers_fn = false;
    }
    set_settings_bits(&mut nnn.cfg, cfgw);
    nnn.cfg.sort.rev_fn = false;
    nnn.cfg.sort.vers_fn = false;
    nnn.cfg.sort.blk_shift = 9;
    nnn.blk_shift = 9;
    nnn.cfgsort = [0; CTX_MAX + 1];
    nnn.popup.curssn.clear();
    nnn.ndents = 0;
    nnn.cur = 0;
}

fn dump(nnn: &Nnn, st: bool, spath: &str) -> String {
    let mut s = format!(
        "{} {:08x} {} {} {} {} {}",
        u8::from(st),
        settings_bits(&nnn.cfg),
        u8::from(nnn.cfg.sort.rev_fn),
        u8::from(nnn.cfg.sort.vers_fn),
        nnn.blk_shift,
        hex(&nnn.cfgsort),
        hex(&nnn.popup.curssn)
    );
    for c in &nnn.g_ctx {
        let mut f = c.c_fltr.clone();
        f.resize(REGEX_MAX, 0);
        s += &format!(
            " {:08x} {} {},{},{},{}",
            settings_bits(&c.c_cfg),
            c.color,
            hex(&c.c_name),
            hex(&c.c_last),
            hex(&f),
            hex(&c.c_path)
        );
    }
    s += &format!(" {}", u8::from(std::path::Path::new(spath).exists()));
    s
}

fn call(nnn: &mut Nnn, sdir: &str, f: &[&str]) -> String {
    let arg = |i: usize| match f.get(i).copied() {
        Some("-") | None => "",
        Some(s) => s,
    };
    let bits = |i: usize| u32::from_str_radix(arg(i), 16).unwrap_or(0);
    match f[0] {
        "save" if f.len() >= 5 + 6 * CTX_MAX => {
            reset(nnn, bits(1));
            nnn.ndents = arg(2).parse().unwrap_or(0);
            nnn.pdents = vec![Entry {
                name: cs(arg(3)),
                ..Entry::default()
            }];
            let sname = cs(arg(4));
            for (i, c) in nnn.g_ctx.iter_mut().enumerate() {
                let g = 5 + 6 * i;
                set_settings_bits(&mut c.c_cfg, bits(g));
                c.color = arg(g + 1).parse().unwrap_or(0);
                c.c_name = cs(arg(g + 2));
                c.c_last = cs(arg(g + 3));
                let mut fl = unhex(arg(g + 4));
                fl.resize(REGEX_MAX, 0);
                c.c_fltr = fl;
                c.c_path = cs(arg(g + 5));
            }
            let spath = format!("{sdir}/{}", String::from_utf8_lossy(&sname));
            let _ = std::fs::remove_file(&spath);
            let mut presel = 0;
            nnn.save_session(&sname, Some(&mut presel));
            match std::fs::read(&spath) {
                Ok(b) => {
                    let _ = std::fs::remove_file(&spath);
                    format!("{} {presel}", hex(&b))
                }
                Err(_) => format!("NOFILE {presel}"),
            }
        }
        "load" if f.len() >= 6 => {
            reset(nnn, bits(1));
            let null = arg(2) == "~";
            let sname = cs(arg(2));
            let restore = arg(3) == "1";
            nnn.g_state.picker = arg(4) == "1";
            let file = if null || restore {
                "@".to_string()
            } else {
                String::from_utf8_lossy(&sname).into_owned()
            };
            let spath = format!("{sdir}/{file}");
            if arg(5) == "NOFILE" {
                let _ = std::fs::remove_file(&spath);
            } else if std::fs::write(&spath, unhex(arg(5))).is_err() {
                std::process::exit(3);
            }
            let st = nnn.load_session((!null).then_some(&sname[..]), restore);
            let r = dump(nnn, st, &spath);
            let _ = std::fs::remove_file(&spath);
            r
        }
        _ => String::new(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        std::process::exit(2);
    }
    nnn::locale::init();
    let dir = std::env::temp_dir().join(format!("nnn-ssnrs.{}", std::process::id()));
    let sdir = dir.join("sessions");
    if std::fs::create_dir_all(&sdir).is_err() {
        std::process::exit(2);
    }
    let mut nnn = Nnn {
        home: b"/home/u".to_vec(),
        cfgpath: dir.to_string_lossy().as_bytes().to_vec(),
        curses: nnn::curses::Curses::init(true),
        xlines: 24,
        xcols: 80,
        ..Nnn::default()
    };
    if nnn.curses.is_none() {
        eprintln!("stdin must be a terminal");
        std::process::exit(2);
    }
    let (Ok(inf), Ok(mut out)) = (
        std::fs::File::open(&args[1]),
        std::fs::File::create(&args[2]),
    ) else {
        std::process::exit(2);
    };
    let sdir = sdir.to_string_lossy().into_owned();
    for line in std::io::BufReader::new(inf).lines() {
        let Ok(line) = line else { break };
        let f: Vec<&str> = line.split('\t').filter(|s| !s.is_empty()).collect();
        if f.is_empty() {
            continue;
        }
        let r = call(&mut nnn, &sdir, &f);
        let _ = writeln!(out, "{r}");
    }
    if let Some(c) = nnn.curses.as_mut() {
        c.endwin();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
