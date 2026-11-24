//! Term under a ConPTY (Windows only): the test binary runs itself again under a pseudo
//! console of a chosen size, with `NNN_PROBE_MODES` naming the file for what the child sees.
//! The console's modes must be back after `Term` is dropped (also when a panic unwinds
//! through it), and `Term::size` must be the ConPTY's size; nnn itself must draw inside it.
#![cfg(windows)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

const PROBE: &str = "NNN_PROBE_MODES";
const PROBE_CTRL_C: &str = "NNN_PROBE_CTRL_C";
const PRINT_MODES: &str = "NNN_PRINT_MODES";

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

/// A ConPTY of a chosen size: programs run in it one after another, as in one console window
/// (console modes one leaves behind are what the next one finds).
struct Conpty {
    pair: portable_pty::PtyPair,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
}

/// One input step: once `after` shows (in what was drawn since the step before) and `delay`
/// has passed, `keys` are sent.
struct Step<'a> {
    after: &'a [u8],
    delay: Duration,
    keys: &'a [u8],
}

const fn step<'a>(after: &'a [u8], delay_ms: u64, keys: &'a [u8]) -> Step<'a> {
    Step {
        after,
        delay: Duration::from_millis(delay_ms),
        keys,
    }
}

impl Conpty {
    fn new(rows: u16, cols: u16) -> Conpty {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Conpty { pair, rx, writer }
    }

    fn take(&mut self, screen: &mut Vec<u8>) {
        while let Ok(chunk) = self.rx.try_recv() {
            // Answer ConPTY's cursor position request so it does not wait for one.
            if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                let _ = self.writer.write_all(b"\x1b[1;1R");
            }
            screen.extend(chunk);
        }
    }

    /// Runs `cmd` without LINES/COLUMNS, playing `steps`: the exit success, everything it
    /// drew and where in that each step's keys were sent. A child still running after 15 s
    /// is killed (and fails).
    fn run(&mut self, mut cmd: CommandBuilder, steps: &[Step<'_>]) -> (bool, Vec<u8>, Vec<usize>) {
        cmd.env_remove("LINES");
        cmd.env_remove("COLUMNS");
        let mut child = self
            .pair
            .slave
            .spawn_command(cmd)
            .expect("spawn under ConPTY");
        let mut screen = Vec::new();
        let mut sent = Vec::new();
        let mut seen: Option<Instant> = None;
        let start = Instant::now();
        let status = loop {
            self.take(&mut screen);
            if let Some(s) = child.try_wait().expect("try_wait") {
                break Some(s);
            }
            if let Some(st) = steps.get(sent.len()) {
                let from = sent.last().copied().unwrap_or(0);
                if seen.is_none()
                    && (st.after.is_empty()
                        || screen[from..]
                            .windows(st.after.len())
                            .any(|w| w == st.after))
                {
                    seen = Some(Instant::now());
                }
                if seen.is_some_and(|t| t.elapsed() >= st.delay) {
                    self.writer.write_all(st.keys).expect("send keys");
                    sent.push(screen.len());
                    seen = None;
                }
            }
            if start.elapsed() > Duration::from_secs(15) {
                let _ = child.kill();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        std::thread::sleep(Duration::from_millis(100));
        self.take(&mut screen);
        (status.is_some_and(|s| s.success()), screen, sent)
    }
}

/// Runs `cmd` under a fresh `rows` x `cols` ConPTY, sending `keys` after `delay`.
fn run_in_conpty(
    cmd: CommandBuilder,
    rows: u16,
    cols: u16,
    delay: Duration,
    keys: &[u8],
) -> (bool, Vec<u8>) {
    let steps = [Step {
        after: b"",
        delay,
        keys,
    }];
    let steps = if keys.is_empty() {
        &steps[..0]
    } else {
        &steps[..]
    };
    let (ok, screen, _) = Conpty::new(rows, cols).run(cmd, steps);
    (ok, screen)
}

/// A temp file name for a child's log.
fn probe_file(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "nnn-probe-{tag}-{}-{}",
        std::process::id(),
        PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// This test binary running only the test `name`, with `env` naming `file`.
fn probe_cmd(name: &str, env: &str, file: &std::path::Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current_exe"));
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"]);
    cmd.env(env, file);
    cmd
}

fn read_log(file: &std::path::Path) -> String {
    let log = std::fs::read_to_string(file).expect("probe file");
    let _ = std::fs::remove_file(file);
    log
}

/// The probe child under a `rows` x `cols` ConPTY: its log.
fn probe(rows: u16, cols: u16) -> String {
    let file = probe_file("modes");
    let (ok, _) = run_in_conpty(
        probe_cmd("probe_modes", PROBE, &file),
        rows,
        cols,
        Duration::ZERO,
        b"",
    );
    assert!(ok, "probe child failed");
    read_log(&file)
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

/// The child half of `ctrl_c_wakes_read`: waits for keys with no time limit after "READY".
#[test]
fn probe_ctrl_c() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let Some(out) = std::env::var_os(PROBE_CTRL_C) else {
        return;
    };
    let modes = || format!("{:?}", nnn::platform::console_modes());
    let (int, term) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    nnn::platform::register_signals(int.clone(), term).expect("register_signals");
    let mut log = format!("before {}\n", modes());
    let mut t = nnn::platform::Term::open().expect("Term::open");
    print!("READY");
    std::io::stdout().flush().expect("flush");
    let mut buf = Vec::new();
    let r = t.read(-1, &mut buf);
    log += &format!("read {r:?} {buf:?}\n");
    log += &format!("int {}\n", int.load(Ordering::Relaxed));
    drop(t);
    log += &format!("after {}\n", modes());
    std::fs::write(out, log).expect("write probe file");
}

/// Writes the console's modes to the file `NNN_PRINT_MODES` names.
#[test]
fn print_modes() {
    if let Some(out) = std::env::var_os(PRINT_MODES) {
        let modes = format!("{:?}", nnn::platform::console_modes());
        std::fs::write(out, modes).expect("write probe file");
    }
}

fn console_modes_in(pty: &mut Conpty) -> String {
    let file = probe_file("print");
    let (ok, _, _) = pty.run(probe_cmd("print_modes", PRINT_MODES, &file), &[]);
    assert!(ok, "print_modes child failed");
    read_log(&file)
}

/// Ctrl-C (0x03 through ConPTY is a CTRL_C_EVENT) ends a wait for keys with no time limit,
/// as SIGINT ends poll() on Linux: nothing is read, the interrupt flag is set.
#[test]
fn ctrl_c_wakes_read() {
    let file = probe_file("ctrlc");
    let mut pty = Conpty::new(24, 80);
    let steps = [step(b"READY", 300, b"\x03")];
    let (ok, _, sent) = pty.run(probe_cmd("probe_ctrl_c", PROBE_CTRL_C, &file), &steps);
    assert_eq!(sent.len(), 1, "the child never got ready");
    assert!(
        ok,
        "probe child failed (killed by Ctrl-C, or still waiting)"
    );
    let log = read_log(&file);
    assert_eq!(field(&log, "read "), "Timeout []", "{log}");
    assert_eq!(field(&log, "int "), "true", "{log}");
    assert_eq!(field(&log, "after "), field(&log, "before "), "{log}");
}

/// Ctrl-C at the main loop and inside the command prompt: as on Linux nnn keeps running, the
/// prompt stays open and takes the next key; after `q` the console has its modes back.
#[test]
fn ctrl_c_in_nnn_and_its_prompt() {
    let home = std::env::temp_dir().join(format!("nnn-conpty-ctrlc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("home");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_nnn"));
    cmd.env("HOME", &home);
    cmd.arg(&home);
    let mut pty = Conpty::new(24, 80);
    let before = console_modes_in(&mut pty);
    let steps = [
        step(b"\x1b[?1049h", 1000, b"\x03"),
        step(b"", 500, b"]"),
        step(b">>>", 300, b"\x03"),
        step(b"", 300, b"x"),
        step(b"", 500, b"\x1b"),
        step(b"", 500, b"q"),
    ];
    let (ok, screen, sent) = pty.run(cmd, &steps);
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(
        sent.len(),
        steps.len(),
        "nnn stopped early: {}",
        String::from_utf8_lossy(&screen)
    );
    assert!(ok, "nnn did not quit with 0");
    assert!(
        screen[sent[3]..sent[4]].contains(&b'x'),
        "the prompt did not take a key after Ctrl-C"
    );
    assert_eq!(console_modes_in(&mut pty), before);
}
