//! The Rust side of the prompt-history oracle (rust/oracle/prompt_oracle.c speaks the same
//! protocol; rust/oracle/prompt_run.sh compares the two).

use nnn::prompt::{INVALID_POS, PromptState, xconfirm};
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

/// The C string in `b`: up to its first NUL, as the C side passes it.
fn cstr(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

fn state(p: &PromptState) -> String {
    let e: Vec<String> = p.cmd_hist.iter().map(|c| hex(c)).collect();
    format!(" {} {} {}", p.lastcmdpos(), p.selcmdpos, e.join(","))
}

fn main() {
    nnn::locale::init();
    let mut p = PromptState::default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().split(b'\n') {
        let Ok(line) = line else { break };
        let line = String::from_utf8_lossy(&line).into_owned();
        let op = line.chars().next().unwrap_or(' ');
        let a = line.get(2..).unwrap_or("");
        let arg = unhex(a);
        let mut s = op.to_string();
        match op {
            'A' => p.addcmdtohist(&arg),
            'G' => {
                let c = p.getcmdfromhist(a.starts_with('1'));
                s += &format!(" {}", c.map_or("-".to_string(), |c| hex(&c)));
            }
            'S' => p.selcmdpos = INVALID_POS,
            'X' => p = PromptState::default(),
            'R' => p.xread_history(cstr(&arg)),
            'W' => {
                let _ = write!(out, "{s}");
                let _ = out.flush();
                s.clear();
                p.xwrite_history(cstr(&arg));
                let path = nnn::platform::path(cstr(&arg));
                s += &format!(
                    " {}",
                    std::fs::read(&path).map_or("-".to_string(), |b| hex(&b))
                );
            }
            'C' => {
                let n: i32 = a.trim().parse().unwrap_or(0);
                let _ = writeln!(out, "C {}", i32::from(xconfirm(n)));
                let _ = out.flush();
                continue;
            }
            _ => {}
        }
        s += &state(&p);
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}
