//! The Rust side of the config/selection differential oracle
//! (rust/oracle/config_oracle.c speaks the same protocol; rust/oracle/run_config.sh compares).

use nnn::config::{self, NNN_BMS, NNN_ORDER, NNN_PLUG};
use std::io::{BufRead, Write};

fn unhex(h: &str) -> Vec<u8> {
    if h == "-" {
        return Vec::new();
    }
    h.as_bytes()
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap_or("00"), 16).unwrap_or(0))
        .collect()
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".to_string();
    }
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn rooted(root: &[u8], rel: &[u8]) -> Vec<u8> {
    let mut p = root.to_vec();
    p.push(b'/');
    p.extend_from_slice(rel);
    p
}

fn putfile(p: &[u8]) -> String {
    match std::fs::read(&*nnn::platform::os(p)) {
        Ok(b) => hex(&b[..b.len().min(1 << 14)]),
        // open(2) of a directory succeeds and its read(2) fails, as the C side prints.
        Err(_) if std::fs::File::open(&*nnn::platform::os(p)).is_ok() => "-".to_string(),
        Err(_) => "nofile".to_string(),
    }
}

fn call(f: &[&str], root: &[u8]) -> String {
    let arg = |i: usize| f.get(i).copied().unwrap_or("-");
    match f[0] {
        op @ ("kv" | "kvval" | "kvkey") => {
            let id = match op {
                "kvkey" => NNN_ORDER,
                "kvval" if arg(3).starts_with('P') => NNN_PLUG,
                _ => NNN_BMS,
            };
            let (ok, kvs) = config::parsekvpair(Some(&unhex(arg(1))));
            if !ok {
                return "bad".to_string();
            }
            match op {
                "kv" => {
                    let mut out = kvs.len().to_string();
                    for kv in &kvs {
                        out += &format!(" {}:{}", kv.key, hex(&kv.val));
                    }
                    out
                }
                "kvval" => {
                    let key = nnn::opts::atoi(arg(2).as_bytes());
                    config::get_kv_val(&kvs, key, id, &unhex(arg(4)))
                        .map_or_else(|| "null".to_string(), |v| hex(&v))
                }
                _ => config::get_kv_key(&kvs, &unhex(arg(2)), id).to_string(),
            }
        }
        "mkpath" => {
            let (p, n) = config::mkpath(&unhex(arg(1)), &unhex(arg(2)), &unhex(arg(3)));
            format!("{}:{n}", hex(&p))
        }
        "dirname" => hex(&config::xdirname(&unhex(arg(1)))),
        "basename" => hex(config::xbasename(&unhex(arg(1)))),
        "cprefix" => {
            let mut pre = unhex(arg(2));
            if config::common_prefix(&unhex(arg(1)), &mut pre) {
                hex(&pre)
            } else {
                "null".to_string()
            }
        }
        "mktree" => {
            let p = rooted(root, &unhex(arg(1)));
            u8::from(config::xmktree(&p, arg(2) == "1", arg(3) == "1")).to_string()
        }
        "lastdir" => {
            let cur = unhex(arg(1));
            let out = (arg(2) != "-").then(|| {
                let o = unhex(arg(2));
                if config::tilde_is_home(&o) {
                    o
                } else {
                    rooted(root, &o)
                }
            });
            let cfg = rooted(root, &unhex(arg(3)));
            let home = rooted(root, &unhex(arg(4)));
            let ok = config::write_lastdir_file(&cur, out.as_deref(), &cfg, &home).is_ok();
            let target = match &out {
                None => config::mkpath(&cfg, b".lastd", &home).0,
                Some(o) if config::tilde_is_home(o) => {
                    let mut t = home.clone();
                    t.extend_from_slice(&o[1..]);
                    t
                }
                Some(o) => o.clone(),
            };
            format!("{} {}", if ok { "ok" } else { "err" }, putfile(&target))
        }
        _ => "?".to_string(),
    }
}

fn main() {
    let root =
        std::env::var_os("ORACLE_ROOT").map_or_else(|| b".".to_vec(), nnn::platform::into_bytes);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let f: Vec<&str> = line.split('\t').filter(|s| !s.is_empty()).collect();
        if f.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{}", call(&f, &root));
    }
}
