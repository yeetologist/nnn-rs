//! The Rust side of the list-mode oracle (rust/oracle/listmode_oracle.c speaks the same
//! protocol): runs \`load_input\` on stdin and prints the result, \`listroot\` and \`listpath\`.
//!
//!   oracle_listmode <tmp-dir> [<cwd-for-relative-paths>]

use nnn::state::Nnn;
use std::process::ExitCode;

fn put(key: &str, v: Option<&[u8]>) {
    match v {
        None => println!("{key} NULL"),
        Some(s) => println!(
            "{key} {}",
            s.iter().map(|c| format!("{c:02x}")).collect::<String>()
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(nnn::platform::into_bytes)
        .collect();
    let Some(tmp) = args.first() else {
        return ExitCode::from(2);
    };
    let mut n = Nnn {
        g_tmpfpath: tmp.clone(),
        ..Nnn::default()
    };
    let cwd = args.get(1).filter(|c| !c.is_empty()).cloned();
    let ret = n.load_input(&mut std::io::stdin(), cwd.as_deref());
    put("ret", ret.as_deref());
    put("listroot", n.listroot.as_deref());
    put("listpath", n.listpath.as_deref());
    ExitCode::SUCCESS
}
