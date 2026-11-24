//! `main()` of src/nnn.c: options, the startup checks, then list mode and the TUI.

use crate::opts::{self, Opts};
use crate::sys;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;

/// The `cleanup()` the original registers with `atexit` once the temp path is known.
fn cleanup(st: &Opts) {
    if st.x11 && !st.picker {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[23;0t");
        let _ = out.flush();
    }
}

/// `main()`: the exit code.
pub fn run(argv: Vec<OsString>) -> u8 {
    let argv: Vec<Vec<u8>> = argv.into_iter().map(OsStringExt::into_vec).collect();
    let mut st = Opts::default();

    if let Some(env) = std::env::var_os("NNN_OPTS")
        && let Some(code) = opts::parse_env_opts(&env.into_vec(), &mut st)
    {
        return code;
    }
    let (exit, operands) = opts::parse_opts(&argv, &mut st);
    if let Some(code) = exit {
        return code;
    }

    if let Err(e) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        sys::xerror(10726, &e);
        return 1;
    }
    if !sys::dir_access(b"/tmp") && std::env::var_os("TMPDIR").is_none() {
        eprintln!("set TMPDIR");
        return 1;
    }

    // From here on the original's atexit(cleanup) runs on every exit.
    rust_tui(st, &operands)
}

/// The rest of `main()` in Rust: list mode, then the TUI.
fn rust_tui(st: Opts, operands: &[Vec<u8>]) -> u8 {
    let mut nnn = crate::state::Nnn {
        opts: st.clone(),
        ..Default::default()
    };
    nnn.g_tmpfpath = if sys::dir_access(b"/tmp") {
        b"/tmp".to_vec()
    } else {
        std::env::var_os("TMPDIR")
            .map(OsStringExt::into_vec)
            .unwrap_or_default()
    };
    if !nnn.list_mode_main() {
        cleanup(&st);
        return 1;
    }
    let initpath = (!nnn.initpath.is_empty()).then(|| nnn.initpath.clone());
    crate::browse::tui_main(
        &mut nnn,
        crate::browse::TuiArgs {
            opts: &st,
            operands,
            initpath,
        },
    )
}
