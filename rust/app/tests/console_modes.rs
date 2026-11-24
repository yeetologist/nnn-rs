//! Term under a ConPTY (Windows only): the test binary runs itself again under a pseudo
//! console of a chosen size, with `NNN_PROBE_MODES` naming the file for what the child sees.
//! The console's modes must be back after `Term` is dropped (also when a panic unwinds
//! through it), and `Term::size` must be the ConPTY's size; nnn itself must draw inside it.
#![cfg(windows)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

const PROBE: &str = "NNN_PROBE_MODES";

static PROBES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
    log += &format!("size {:?}\n", term.size());
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

/// Runs `cmd` under a `rows` x `cols` ConPTY without LINES/COLUMNS, sending `keys` after
/// `delay`; the exit success and everything it drew. A child still running after 10 s is
/// killed (and fails).
fn run_in_conpty(
    mut cmd: CommandBuilder,
    rows: u16,
    cols: u16,
    delay: Duration,
    keys: &[u8],
) -> (bool, Vec<u8>) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    cmd.env_remove("LINES");
    cmd.env_remove("COLUMNS");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn under ConPTY");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut screen = Vec::new();
    let take = |screen: &mut Vec<u8>, writer: &mut Box<dyn Write + Send>| {
        while let Ok(chunk) = rx.try_recv() {
            // Answer ConPTY's cursor position request so it does not wait for one.
            if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                let _ = writer.write_all(b"\x1b[1;1R");
            }
            screen.extend(chunk);
        }
    };
    let start = Instant::now();
    let mut sent = keys.is_empty();
    let status = loop {
        take(&mut screen, &mut writer);
        if let Some(s) = child.try_wait().expect("try_wait") {
            break Some(s);
        }
        if !sent && start.elapsed() >= delay {
            writer.write_all(keys).expect("send keys");
            sent = true;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    std::thread::sleep(Duration::from_millis(100));
    take(&mut screen, &mut writer);
    drop(pair.master);
    (status.is_some_and(|s| s.success()), screen)
}

/// The probe child under a `rows` x `cols` ConPTY: its log.
fn probe(rows: u16, cols: u16) -> String {
    let file = std::env::temp_dir().join(format!(
        "nnn-probe-modes-{}-{}",
        std::process::id(),
        PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&file);
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current_exe"));
    cmd.args(["--exact", "probe_modes", "--nocapture", "--test-threads=1"]);
    cmd.env(PROBE, &file);
    let (ok, _) = run_in_conpty(cmd, rows, cols, Duration::ZERO, b"");
    assert!(ok, "probe child failed");
    let log = std::fs::read_to_string(&file).expect("probe file");
    let _ = std::fs::remove_file(&file);
    log
}

fn field<'a>(log: &'a str, tag: &str) -> &'a str {
    log.lines()
        .find_map(|l| l.strip_prefix(tag))
        .unwrap_or_else(|| panic!("no {tag:?} in {log:?}"))
}

#[test]
fn console_mode_restored_on_drop() {
    let log = probe(24, 80);
    let before = field(&log, "before ");
    assert!(
        before.starts_with("Some("),
        "no console in the child: {log}"
    );
    assert_ne!(
        field(&log, "open "),
        before,
        "Term::open changed nothing: {log}"
    );
    assert_eq!(field(&log, "after "), before, "{log}");
    assert_eq!(field(&log, "panic "), before, "{log}");
}

#[test]
fn term_size_is_the_visible_window() {
    assert_eq!(field(&probe(24, 80), "size "), "(24, 80)");
    assert_eq!(field(&probe(30, 100), "size "), "(30, 100)");
}

/// The largest row and column of the cursor moves (`ESC[row;colH`) in `screen`. A move right
/// after `ESC[?25l` is left out: with the cursor hidden ConPTY parks it past the last column
/// after a line ending in blanks (seen with any program, not only nnn).
fn max_cursor(screen: &[u8]) -> (u32, u32) {
    let s = String::from_utf8_lossy(screen);
    let mut max = (0, 0);
    let mut after_hide = false;
    for part in s.split("\x1b[").skip(1) {
        let hidden = std::mem::replace(&mut after_hide, part == "?25l");
        if hidden {
            continue;
        }
        let Some(end) = part.find(|c: char| !(c.is_ascii_digit() || c == ';')) else {
            continue;
        };
        if part[end..].starts_with('H')
            && let Some((r, c)) = part[..end].split_once(';')
            && let (Ok(r), Ok(c)) = (r.parse::<u32>(), c.parse::<u32>())
        {
            max = (max.0.max(r), max.1.max(c));
        }
    }
    max
}

#[test]
fn nnn_draws_inside_the_console() {
    for (rows, cols) in [(24u16, 80u16), (30, 100)] {
        let home = std::env::temp_dir().join(format!(
            "nnn-conpty-home-{}-{rows}x{cols}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("home");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_nnn"));
        cmd.env("HOME", &home);
        cmd.env(
            "NNN_DBG",
            std::env::temp_dir().join(format!("nnn-dbg-{rows}x{cols}.txt")),
        );
        cmd.cwd(&home);
        let (ok, screen) = run_in_conpty(cmd, rows, cols, Duration::from_secs(2), b"q");
        let _ = std::fs::remove_dir_all(&home);
        assert!(ok, "nnn did not quit with 0 at {rows}x{cols}");
        assert!(
            screen.windows(8).any(|w| w == b"\x1b[?1049h"),
            "{rows}x{cols}: nnn never entered curses mode"
        );
        // A resize storm (a buffer-size event for every redraw) would repaint without end.
        assert!(
            screen.len() < 64 * 1024,
            "{rows}x{cols}: {} bytes drawn",
            screen.len()
        );
        let (r, c) = max_cursor(&screen);
        assert!(
            r <= u32::from(rows) && c <= u32::from(cols),
            "{rows}x{cols}: cursor went to row {r}, column {c}"
        );
    }
}
