//! The console's input and output modes are back after `Term` is dropped, also when a panic
//! unwinds through it. Windows only: the test binary runs itself again under a ConPTY (so it
//! has a console of its own) with `NNN_PROBE_MODES` naming the file for the modes it sees.
#![cfg(windows)]

use std::io::{Read, Write};

const PROBE: &str = "NNN_PROBE_MODES";

/// The child half; a no-op unless `NNN_PROBE_MODES` is set.
#[test]
fn probe_modes() {
    let Some(out) = std::env::var_os(PROBE) else {
        return;
    };
    let modes = || format!("{:?}", nnn::platform::console_modes());
    let mut log = format!("before {}\n", modes());
    let term = nnn::platform::Term::open().expect("Term::open");
    log += &format!("open {}\n", modes());
    drop(term);
    log += &format!("after {}\n", modes());
    let r = std::panic::catch_unwind(|| {
        let _term = nnn::platform::Term::open().expect("Term::open");
        panic!("probe: unwinding through Term");
    });
    assert!(r.is_err());
    log += &format!("panic {}\n", modes());
    std::fs::write(out, log).expect("write probe file");
}

#[test]
fn console_mode_restored_on_drop() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let file = std::env::temp_dir().join(format!("nnn-probe-modes-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current_exe"));
    cmd.args(["--exact", "probe_modes", "--nocapture", "--test-threads=1"]);
    cmd.env(PROBE, &file);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn under ConPTY");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    // Drain the screen; answer ConPTY's cursor position request so it does not wait for one.
    let drain = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                let _ = writer.write_all(b"\x1b[1;1R");
            }
        }
    });
    let status = child.wait().expect("wait");
    drop(pair.master);
    let _ = drain.join();
    assert!(status.success(), "probe child failed: {status:?}");

    let log = std::fs::read_to_string(&file).expect("probe file");
    let _ = std::fs::remove_file(&file);
    let mode = |tag: &str| {
        log.lines()
            .find_map(|l| l.strip_prefix(tag))
            .unwrap_or_else(|| panic!("no {tag:?} in {log:?}"))
            .to_string()
    };
    let before = mode("before ");
    assert!(
        before.starts_with("Some("),
        "no console in the child: {log}"
    );
    assert_ne!(mode("open "), before, "Term::open changed nothing: {log}");
    assert_eq!(mode("after "), before, "{log}");
    assert_eq!(mode("panic "), before, "{log}");
}
