//! tui-check: the screen proof of rust/tui-cases (ADR-004 b) without tmux, so it runs on
//! Windows too. Each case runs the build in an 80x24 pty (ConPTY on Windows) with a fresh
//! fixture tree; a vt100 emulator stands where tmux stood. Everything else is harness.py's:
//! the same env, key bytes, settle rule, normalization, work-tree hashes and snapshot JSON,
//! compared with the same expected/*.json.
//!
//!   tui-check check <build...> [--case NAME]... [--verbose] [--coreutils]
//!   tui-check record <build...> [--case NAME]...   write expected/<case>.json from the build
//!
//! A key token "<Resize COLSxROWS>" resizes the pty instead of typing.

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use regex::{Captures, Regex};
use serde_json::{Map, Value, json};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, SystemTime};

type Res<T> = Result<T, Box<dyn Error>>;

const COLS: u16 = 80;
const ROWS: u16 = 24;
/// 2023-11-14 22:13:20 UTC, the time of every fixture entry.
const FIXED_MTIME: u64 = 1_700_000_000;
/// The fixture's time as nnn shows it; any other time is "now" and wildcarded.
const FIXED_TIME: &str = "2023-11-14 22:13";
/// The name a case's optional "platforms" list must hold for it to run here.
const PLATFORM: &str = if cfg!(windows) { "windows" } else { "unix" };
/// Windows only: details that differ by file system, wildcarded in expected and actual alike.
const WINDOWS_NORMALIZE: bool = cfg!(windows);

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui-cases")
}

/// The bytes tmux sends for a key token: named keys as tmux encodes them (cursor keys follow
/// the application-cursor mode), "<C-x>" as the control byte, anything else literally.
fn key_bytes(tok: &str, app_cursor: bool) -> Vec<u8> {
    let cursor = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let named = match tok {
        "<Enter>" => "\r".to_owned(),
        "<Esc>" => "\x1b".to_owned(),
        "<Tab>" => "\t".to_owned(),
        "<BS>" => "\x7f".to_owned(),
        "<Space>" => " ".to_owned(),
        "<Up>" => cursor('A'),
        "<Down>" => cursor('B'),
        "<Right>" => cursor('C'),
        "<Left>" => cursor('D'),
        "<Home>" => "\x1b[1~".to_owned(),
        "<End>" => "\x1b[4~".to_owned(),
        "<PgUp>" => "\x1b[5~".to_owned(),
        "<PgDn>" => "\x1b[6~".to_owned(),
        _ => {
            let b = tok.as_bytes();
            if b.len() == 5 && tok.starts_with("<C-") && tok.ends_with('>') {
                return vec![b[3].to_ascii_uppercase() & 0x1f];
            }
            tok.to_owned()
        }
    };
    named.into_bytes()
}

/// The size a "<Resize COLSxROWS>" key token asks the pty to take.
fn resize_token(tok: &str) -> Option<(u16, u16)> {
    let size = tok.strip_prefix("<Resize ")?.strip_suffix('>')?;
    let (cols, rows) = size.split_once('x')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

/// GLOBAL_NORMALIZE: every time but the fixture's becomes "<NOW>", scanning as Python's
/// re.sub does with the pattern "(?!2023-11-14 22:13)\d{4}-\d\d-\d\d \d\d:\d\d".
fn wildcard_now(line: &str) -> String {
    static TIME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\d{4}-\d\d-\d\d \d\d:\d\d").expect("regex"));
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if !rest.starts_with(FIXED_TIME)
            && let Some(m) = TIME.find(rest)
        {
            out.push_str("<NOW>");
            rest = &rest[m.end()..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// A Python re.sub replacement ("\1", "\g<1>", "\\") in the regex crate's syntax.
fn python_replacement(rep: &str) -> String {
    let mut out = String::new();
    let mut chars = rep.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' => out.push_str("$$"),
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => {
                    let mut n = d.to_string();
                    if let Some(&e) = chars.peek()
                        && e.is_ascii_digit()
                    {
                        n.push(e);
                        chars.next();
                    }
                    out.push_str(&format!("${{{n}}}"));
                }
                Some('g') if chars.peek() == Some(&'<') => {
                    chars.next();
                    let name: String = chars.by_ref().take_while(|&c| c != '>').collect();
                    out.push_str(&format!("${{{name}}}"));
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            },
            _ => out.push(c),
        }
    }
    out
}

/// The Windows-only wildcards for one screen line: the detail-mode octal column and directory
/// size, the status bar's permission string, a directory's size and the N-INODE suffix.
fn windows_normalize(line: &str) -> String {
    static DETAIL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(.(?:\d{4}-\d\d-\d\d \d\d:\d\d|<NOW>))  [0-7]{3} +(\S+) ([ +].*)$")
            .expect("regex")
    });
    // The status bar: "N/M <flags> <time> <perm> <size>[ <type>][ <links>-<inode>]".
    static STATUS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(\d+/\d+ .*?(?:\d{4}-\d\d-\d\d \d\d:\d\d|<NOW>) )([-dlcbsp?])[-rwxsStTl]{9} (\S+)(.*?)(?: \d+-\d+)?$",
        )
        .expect("regex")
    });
    let line = DETAIL.replace(line, |c: &Captures<'_>| {
        let size = if c[3].ends_with('/') {
            "<DSIZE>"
        } else {
            &c[2]
        };
        format!("{}  <OCT> {size} {}", &c[1], &c[3])
    });
    STATUS
        .replace(&line, |c: &Captures<'_>| {
            let size = if &c[2] == "d" { "<DSIZE>" } else { &c[3] };
            format!("{}<PERM> {size}{}", &c[1], &c[4])
        })
        .into_owned()
}

/// How a case's screen is turned into the text that is compared.
struct Normalizer {
    /// The case's temp dir as it may appear on screen, each shown as "<TMP>".
    tmp: Vec<String>,
    rules: Vec<(Regex, String)>,
}

impl Normalizer {
    fn new(tmp: &Path, case: &Value) -> Res<Normalizer> {
        let tmp = tmp.to_string_lossy().into_owned();
        let mut tmps = vec![tmp.clone()];
        if WINDOWS_NORMALIZE {
            tmps.push(tmp.replace('\\', "/"));
        }
        let mut rules = Vec::new();
        for rule in arr(case, "normalize") {
            let (Some(pat), Some(rep)) = (rule[0].as_str(), rule[1].as_str()) else {
                return Err(format!("bad normalize rule {rule}").into());
            };
            rules.push((Regex::new(pat)?, python_replacement(rep)));
        }
        Ok(Normalizer { tmp: tmps, rules })
    }

    fn untmp(&self, s: &str) -> String {
        self.tmp
            .iter()
            .fold(s.to_owned(), |s, t| s.replace(t.as_str(), "<TMP>"))
    }

    fn line(&self, raw: &str) -> String {
        let mut line = wildcard_now(&self.untmp(raw.trim_end()));
        for (re, rep) in &self.rules {
            line = re.replace_all(&line, rep.as_str()).into_owned();
        }
        line
    }
}

/// A case's list field (absent = empty).
fn arr<'a>(case: &'a Value, key: &str) -> &'a [Value] {
    case.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn strs(case: &Value, key: &str) -> Vec<String> {
    arr(case, key)
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Whether the case lists "coreutils" in "requires": it runs only with --coreutils, against
/// Microsoft's Coreutils for Windows found on PATH.
fn needs_coreutils(case: &Value) -> bool {
    strs(case, "requires").iter().any(|r| r == "coreutils")
}

/// Whether the case's "platforms" (all platforms when absent) includes this one.
fn runs_here(case: &Value) -> bool {
    case.get("platforms").is_none() || strs(case, "platforms").iter().any(|p| p == PLATFORM)
}

#[cfg(windows)]
fn open_for_times(p: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(p)
}

#[cfg(not(windows))]
fn open_for_times(p: &Path) -> std::io::Result<fs::File> {
    fs::File::open(p)
}

fn set_fixed_times(p: &Path) -> Res<()> {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(FIXED_MTIME);
    let times = fs::FileTimes::new().set_accessed(t).set_modified(t);
    open_for_times(p)?.set_times(times)?;
    Ok(())
}

/// Every entry under `dir`, children before their directory.
fn entries_bottom_up(dir: &Path, out: &mut Vec<PathBuf>) -> Res<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            entries_bottom_up(&e.path(), out)?;
        }
        out.push(e.path());
    }
    Ok(())
}

/// The case's fixture: files and (null) directories, all with the fixed times.
fn build_fixture(root: &Path, spec: &Map<String, Value>) -> Res<()> {
    fs::create_dir_all(root)?;
    let mut rels: Vec<&String> = spec.keys().collect();
    rels.sort();
    for rel in rels {
        let p = root.join(rel);
        match &spec[rel.as_str()] {
            Value::Null => fs::create_dir_all(&p)?,
            content => {
                if let Some(parent) = p.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&p, content.as_str().ok_or("fixture content")?)?;
            }
        }
    }
    let mut all = Vec::new();
    entries_bottom_up(root, &mut all)?;
    all.push(root.to_path_buf());
    for p in all {
        set_fixed_times(&p)?;
    }
    Ok(())
}

/// The work tree as sorted lines: "dir/" for directories, "path sha1[:10]" for files.
fn tree(root: &Path) -> Res<Vec<String>> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> Res<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let path = e.path();
            if path.is_dir() {
                out.push(format!("{rel}/"));
                if !e.file_type()?.is_symlink() {
                    walk(&path, &rel, out)?;
                }
            } else {
                let hash = sha1_smol::Sha1::from(fs::read(&path)?).digest().to_string();
                out.push(format!("{rel} {}", &hash[..10]));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// Files whose bytes differ run to run (archives hold timestamps): compared by name only.
fn nohash(lines: Vec<String>, case: &Value) -> Res<Vec<String>> {
    let pats = strs(case, "nohash")
        .iter()
        .map(|p| Regex::new(&format!("^(?:{p})$")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(lines
        .into_iter()
        .map(|line| {
            if line.ends_with('/') {
                return line;
            }
            let name = line.rsplit_once(' ').map_or(line.as_str(), |(n, _)| n);
            if pats.iter().any(|p| p.is_match(name)) {
                format!("{name} <HASH>")
            } else {
                line
            }
        })
        .collect())
}

/// A file the case reads back, as Python's text mode reads it (universal newlines).
fn read_back(p: &Path, norm: &Normalizer) -> Value {
    match fs::read(p) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            Value::String(norm.untmp(&text))
        }
        Err(_) => Value::Null,
    }
}

/// The build running in a pty, its output fed to a terminal emulator.
struct Session {
    parser: vt100::Parser,
    output: Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    status: Option<ExitStatus>,
    /// The end of the output so far, to spot a cursor-position request split across reads.
    tail: Vec<u8>,
    master: Box<dyn MasterPty + Send>,
}

impl Session {
    fn spawn(argv: &[String], env: &[(String, String)], cwd: &Path) -> Res<Session> {
        let pair = native_pty_system().openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::from_argv(argv.iter().map(Into::into).collect());
        cmd.env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.cwd(cwd);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (tx, output) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Session {
            parser: vt100::Parser::new(ROWS, COLS, 0),
            output,
            writer,
            child,
            status: None,
            tail: Vec::new(),
            master: pair.master,
        })
    }

    /// Feeds what the build drew so far to the emulator, answering cursor-position requests
    /// as a terminal (tmux, or ConPTY's host) does.
    fn pump(&mut self) -> Res<()> {
        while let Ok(chunk) = self.output.try_recv() {
            self.parser.process(&chunk);
            self.tail.extend_from_slice(&chunk);
            let asks: Vec<usize> = self
                .tail
                .windows(4)
                .enumerate()
                .filter(|(_, w)| w == b"\x1b[6n")
                .map(|(at, _)| at + 4)
                .collect();
            for _ in &asks {
                let (row, col) = self.parser.screen().cursor_position();
                let answer = format!("\x1b[{};{}R", row + 1, col + 1);
                self.writer.write_all(answer.as_bytes())?;
                self.writer.flush()?;
            }
            // Keep what may start a request split across reads, never one already answered.
            let answered = asks.last().copied().unwrap_or(0);
            let cut = self.tail.len().saturating_sub(3).max(answered);
            self.tail.drain(..cut);
        }
        Ok(())
    }

    fn screen(&mut self, norm: &Normalizer) -> Res<Vec<String>> {
        self.pump()?;
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        Ok((0..rows)
            .map(|row| {
                let mut line = String::new();
                for col in 0..cols {
                    match screen.cell(row, col) {
                        Some(c) if c.is_wide_continuation() => {}
                        Some(c) if c.has_contents() => line.push_str(c.contents()),
                        _ => line.push(' '),
                    }
                }
                norm.line(&line)
            })
            .collect())
    }

    /// The screen once it has not changed for `quiet` seconds (at most 5 s).
    fn settle(&mut self, norm: &Normalizer, quiet: f64) -> Res<Vec<String>> {
        let quiet = Duration::from_secs_f64(quiet);
        let end = Instant::now() + Duration::from_secs(5);
        let (mut last, mut since) = (None, Instant::now());
        while Instant::now() < end {
            let cur = self.screen(norm)?;
            if last.as_ref() != Some(&cur) {
                (last, since) = (Some(cur), Instant::now());
            } else if since.elapsed() >= quiet {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(last.unwrap_or_default())
    }

    /// The exit as tmux reports a dead pane (`null` for a signal), or None while it runs.
    fn exit(&mut self) -> Res<Option<Value>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status.as_ref().map(|s| match s.signal() {
            Some(_) => Value::Null,
            None => json!(s.exit_code()),
        }))
    }

    /// Resizes the pty (the build sees a window change) and the emulator with it.
    fn resize(&mut self, cols: u16, rows: u16) -> Res<()> {
        self.pump()?;
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }

    fn send(&mut self, tok: &str) -> Res<()> {
        if let Some((cols, rows)) = resize_token(tok) {
            return self.resize(cols, rows);
        }
        self.pump()?;
        let bytes = key_bytes(tok, self.parser.screen().application_cursor());
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.status.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The variables the build gets: exactly harness.py's (plus, on Windows, the few a process
/// cannot start without), then the case's own.
fn case_env(home: &Path, work: &Path, case: &Value) -> Vec<(String, String)> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut env: Vec<(String, String)> = [
        ("HOME", path(home)),
        ("PWD", path(work)),
        ("PATH", std::env::var("PATH").unwrap_or_default()),
        ("TERM", "screen-256color".to_owned()),
        ("LANG", "C.UTF-8".to_owned()),
        ("LC_ALL", "C.UTF-8".to_owned()),
        ("TZ", "UTC".to_owned()),
        ("USER", "tester".to_owned()),
        ("SHELL", "/bin/sh".to_owned()),
        ("EDITOR", "true".to_owned()),
        ("VISUAL", "true".to_owned()),
        ("PAGER", "cat".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    if cfg!(windows) {
        for k in ["SystemRoot", "windir", "ComSpec", "PATHEXT", "TEMP", "TMP"] {
            if let Ok(v) = std::env::var(k) {
                env.push((k.to_owned(), v));
            }
        }
    }
    // Coreutils on PATH changes nnn's file operations on Windows: only the cases that require
    // it see it.
    if !needs_coreutils(case) {
        env.push(("NNN_COREUTILS".to_owned(), "0".to_owned()));
    }
    if let Some(own) = case.get("env").and_then(Value::as_object) {
        for (k, v) in own {
            let v = v.as_str().unwrap_or_default().to_owned();
            match env.iter_mut().find(|(e, _)| e == k) {
                Some(slot) => slot.1 = v,
                None => env.push((k.clone(), v)),
            }
        }
    }
    env
}

/// The case's temp dir, as long as harness.py's `/tmp/nnnh-XXXXXXXX` (18 bytes): screens wrap
/// and session files record lengths by it. On Windows it is `C:\nnnh-XXXXXXXXXX` on the
/// temp dir's drive, `%TEMP%` being far longer than `/tmp`.
fn case_tempdir() -> Res<tempfile::TempDir> {
    let mut b = tempfile::Builder::new();
    b.prefix("nnnh-");
    if cfg!(windows) {
        let temp = std::env::temp_dir();
        let root = temp.ancestors().last().unwrap_or(&temp).to_path_buf();
        return Ok(b.rand_bytes(10).tempdir_in(root)?);
    }
    Ok(b.rand_bytes(8).tempdir()?)
}

/// One run of a case: {exit, snaps, work, files}.
fn run_case(build: &[String], case: &Value) -> Res<Value> {
    let tmp = case_tempdir()?;
    let (home, work) = (tmp.path().join("home"), tmp.path().join("work"));
    fs::create_dir_all(&home)?;
    let empty = Map::new();
    build_fixture(
        &work,
        case.get("fixture")
            .and_then(Value::as_object)
            .unwrap_or(&empty),
    )?;
    let (h, w) = (home.to_string_lossy(), work.to_string_lossy());
    let mut argv = build.to_vec();
    argv.extend(
        strs(case, "args")
            .iter()
            .map(|a| a.replace("{work}", &w).replace("{home}", &h)),
    );
    let norm = Normalizer::new(tmp.path(), case)?;
    let quiet = case.get("quiet").and_then(Value::as_f64).unwrap_or(0.3);

    let mut s = Session::spawn(&argv, &case_env(&home, &work, case), &work)?;
    let mut snaps = vec![json!({"after": "start", "screen": s.settle(&norm, 0.4)?})];
    for tok in strs(case, "keys") {
        if s.exit()?.is_some() {
            break;
        }
        s.send(&tok)?;
        snaps.push(json!({"after": tok, "screen": s.settle(&norm, quiet)?}));
    }
    let mut exit = Value::from("killed");
    for _ in 0..100 {
        if let Some(code) = s.exit()? {
            exit = code;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(s);
    let mut files = Map::new();
    for rel in strs(case, "read") {
        let v = read_back(&tmp.path().join(&rel), &norm);
        files.insert(rel, v);
    }
    Ok(json!({
        "exit": exit,
        "snaps": snaps,
        "work": nohash(tree(&work)?, case)?,
        "files": files,
    }))
}

/// A result with the Windows-only wildcards applied to its screens.
fn for_windows(mut result: Value) -> Value {
    if let Some(snaps) = result.get_mut("snaps").and_then(Value::as_array_mut) {
        for line in snaps
            .iter_mut()
            .filter_map(|s| s.get_mut("screen").and_then(Value::as_array_mut))
            .flatten()
        {
            if let Some(l) = line.as_str() {
                *line = Value::String(windows_normalize(l));
            }
        }
    }
    result
}

/// JSON as Python's json.dumps(indent=1, ensure_ascii=False) writes it: serde_json's pretty
/// form indents by two spaces (and no string holds a raw newline), so halve the indents.
fn dumps(v: &Value) -> Res<Vec<String>> {
    Ok(serde_json::to_string_pretty(v)?
        .lines()
        .map(|l| {
            let body = l.trim_start_matches(' ');
            format!("{}{body}", " ".repeat((l.len() - body.len()) / 2))
        })
        .collect())
}

/// difflib.unified_diff(a, b, "expected", "actual", lineterm="", n=2) as lines.
fn unified_diff(a: &[&str], b: &[&str]) -> Vec<String> {
    const CONTEXT: usize = 2;
    // Longest common subsequence table, then the edit script: (tag, a index, b index).
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', i, j));
            (i, j) = (i + 1, j + 1);
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(('-', i, j));
            i += 1;
        } else {
            ops.push(('+', i, j));
            j += 1;
        }
    }
    let changes: Vec<usize> = (0..ops.len()).filter(|&k| ops[k].0 != ' ').collect();
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for k in changes {
        let (lo, hi) = (k.saturating_sub(CONTEXT), (k + CONTEXT).min(ops.len() - 1));
        match hunks.last_mut() {
            Some(h) if lo <= h.1 + 1 => h.1 = hi,
            _ => hunks.push((lo, hi)),
        }
    }
    let range = |start: usize, len: usize| match len {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{len}", start + 1),
    };
    let mut out = Vec::new();
    for (lo, hi) in hunks {
        if out.is_empty() {
            out.push("--- expected".to_owned());
            out.push("+++ actual".to_owned());
        }
        let hunk = &ops[lo..=hi];
        let a_len = hunk.iter().filter(|o| o.0 != '+').count();
        let b_len = hunk.iter().filter(|o| o.0 != '-').count();
        out.push(format!(
            "@@ -{} +{} @@",
            range(hunk[0].1, a_len),
            range(hunk[0].2, b_len)
        ));
        for &(tag, i, j) in hunk {
            let line = if tag == '+' { b[j] } else { a[i] };
            out.push(format!("{tag}{line}"));
        }
    }
    out
}

struct Options {
    only: Vec<String>,
    verbose: bool,
    /// Run the cases that require Microsoft's Coreutils (skipped otherwise).
    coreutils: bool,
    build: Vec<String>,
}

const USAGE: &str =
    "usage: tui-check (check|record) <build...> [--case NAME]... [--verbose] [--coreutils]";

fn parse(args: &[String]) -> Res<(String, Options)> {
    let (mode, rest) = args.split_first().ok_or(USAGE)?;
    let mut o = Options {
        only: Vec::new(),
        verbose: false,
        coreutils: false,
        build: Vec::new(),
    };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--case" => o.only.push(it.next().ok_or(USAGE)?.clone()),
            "--verbose" => o.verbose = true,
            "--coreutils" => o.coreutils = true,
            _ => o.build.push(a.clone()),
        }
    }
    let first = o.build.first_mut().ok_or(USAGE)?;
    if first.contains('/') || first.contains(std::path::MAIN_SEPARATOR) {
        *first = std::path::absolute(&*first)?.to_string_lossy().into_owned();
    }
    Ok((mode.clone(), o))
}

fn load_cases() -> Res<Vec<Value>> {
    Ok(serde_json::from_str(&fs::read_to_string(
        cases_dir().join("cases.json"),
    )?)?)
}

fn case_name(case: &Value) -> Res<&str> {
    Ok(case["name"].as_str().ok_or("case without a name")?)
}

/// A case that requires Coreutils, without --coreutils: reported and not counted.
fn skipped(o: &Options, case: &Value, name: &str) -> bool {
    let skip = needs_coreutils(case) && !o.coreutils;
    if skip {
        println!("SKIP {name} (requires coreutils)");
    }
    skip
}

fn named(o: &Options, name: &str) -> bool {
    o.only.iter().any(|n| n == name)
}

/// Writes expected/<case>.json from the build, as harness.py record does: two runs must agree.
/// The cases that run here, or the ones named with --case whatever their platforms (an
/// OS-independent case for another OS is recorded from this OS's build).
fn record(o: &Options) -> Res<bool> {
    let dir = cases_dir().join("expected");
    fs::create_dir_all(&dir)?;
    let (mut passed, mut total) = (0, 0);
    for case in &load_cases()? {
        let name = case_name(case)?;
        if !(named(o, name) || (o.only.is_empty() && runs_here(case))) {
            continue;
        }
        if skipped(o, case, name) {
            continue;
        }
        total += 1;
        let got = run_case(&o.build, case)?;
        if run_case(&o.build, case)? != got {
            println!("NOISE {name}: two runs of the build differ");
            continue;
        }
        fs::write(dir.join(format!("{name}.json")), dumps(&got)?.join("\n"))?;
        passed += 1;
    }
    println!("{passed}/{total} tui cases recorded");
    Ok(passed == total)
}

/// Compares the build against expected/: true when every case that runs here passes.
fn check(o: &Options) -> Res<bool> {
    let dir = cases_dir();
    let (mut passed, mut total) = (0, 0);
    for case in &load_cases()? {
        let name = case_name(case)?;
        if (!o.only.is_empty() && !named(o, name)) || !runs_here(case) {
            continue;
        }
        if skipped(o, case, name) {
            continue;
        }
        total += 1;
        let path = dir.join("expected").join(format!("{name}.json"));
        let want: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        let mut got = run_case(&o.build, case)?;
        let want = if WINDOWS_NORMALIZE {
            got = for_windows(got);
            for_windows(want)
        } else {
            want
        };
        if got == want {
            passed += 1;
            continue;
        }
        println!("FAIL {name}");
        let (a, b) = (dumps(&want)?, dumps(&got)?);
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        let b: Vec<&str> = b.iter().map(String::as_str).collect();
        let diff = unified_diff(&a, &b);
        let shown = if o.verbose { diff.len() } else { 60 };
        for line in diff.iter().take(shown) {
            println!("{line}");
        }
    }
    println!("{passed}/{total} tui cases passed");
    Ok(passed == total)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = parse(&args).and_then(|(mode, o)| match mode.as_str() {
        "check" => check(&o),
        "record" => record(&o),
        _ => Err(USAGE.into()),
    });
    match run {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("tui-check: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_as_tmux_sends_them() {
        assert_eq!(key_bytes("<Down>", true), b"\x1bOB");
        assert_eq!(key_bytes("<Down>", false), b"\x1b[B");
        assert_eq!(key_bytes("<Home>", true), b"\x1b[1~");
        assert_eq!(key_bytes("<C-R>", false), [0x12]);
        assert_eq!(key_bytes("<Enter>", false), b"\r");
        assert_eq!(key_bytes("touch x", false), b"touch x");
        assert_eq!(resize_token("<Resize 100x30>"), Some((100, 30)));
        assert_eq!(resize_token("<Resize 100>"), None);
        assert_eq!(resize_token("<Esc>"), None);
    }

    #[test]
    fn only_times_other_than_the_fixture_are_now() {
        assert_eq!(
            wildcard_now("1/11  2023-11-14 22:13 x 2026-11-24 05:46"),
            "1/11  2023-11-14 22:13 x <NOW>"
        );
        assert_eq!(wildcard_now("ñ 2026-01-02 03:04:05"), "ñ <NOW>:05");
    }

    #[test]
    fn python_replacements_convert() {
        assert_eq!(python_replacement(r"\1: <TS>"), "${1}: <TS>");
        assert_eq!(python_replacement(r"\g<2>$"), "${2}$$");
        assert_eq!(python_replacement("VOLUME: <FS>"), "VOLUME: <FS>");
    }

    #[test]
    fn windows_wildcards() {
        assert_eq!(
            windows_normalize(">2023-11-14 22:13  755      80B  Dir2/"),
            ">2023-11-14 22:13  <OCT> <DSIZE>  Dir2/"
        );
        assert_eq!(
            windows_normalize(" <NOW>  644     4.9K  big.bin"),
            " <NOW>  <OCT> 4.9K  big.bin"
        );
        assert_eq!(
            windows_normalize("1/11  2023-11-14 22:13 drwxr-xr-x 80B"),
            "1/11  2023-11-14 22:13 <PERM> <DSIZE>"
        );
        assert_eq!(
            windows_normalize("2/11  2023-11-14 22:13 -rw-r--r-- 6B txt 2-1234"),
            "2/11  2023-11-14 22:13 <PERM> 6B txt"
        );
        assert_eq!(windows_normalize(" drwxr-xr-x 80B"), " drwxr-xr-x 80B");
        assert_eq!(
            windows_normalize("1/11  +1   <NOW> -rw-r--r-- 0B"),
            "1/11  +1   <NOW> <PERM> 0B"
        );
    }

    #[test]
    fn tmp_in_both_spellings() {
        let n = Normalizer::new(Path::new(r"C:\T\nnnh-x"), &json!({})).expect("normalizer");
        let line = n.line(r"1 C:\T\nnnh-x/work   ");
        assert_eq!(line, "1 <TMP>/work");
        if WINDOWS_NORMALIZE {
            assert_eq!(n.line("C:/T/nnnh-x/work"), "<TMP>/work");
        }
    }

    #[test]
    fn diff_as_difflib() {
        let a = ["a", "b", "c", "d", "e", "f", "g"];
        let b = ["a", "b", "c", "X", "e", "f", "g"];
        assert_eq!(
            unified_diff(&a, &b),
            [
                "--- expected",
                "+++ actual",
                "@@ -2,5 +2,5 @@",
                " b",
                " c",
                "-d",
                "+X",
                " e",
                " f"
            ]
        );
        assert!(unified_diff(&a, &a).is_empty());
    }

    #[test]
    fn fixture_has_the_fixed_times() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        let spec = json!({"a": "x\n", "d": null, "d/b": "", "e/f/g": "y"});
        build_fixture(&root, spec.as_object().expect("object")).expect("fixture");
        let fixed = SystemTime::UNIX_EPOCH + Duration::from_secs(FIXED_MTIME);
        for rel in ["", "a", "d", "d/b", "e", "e/f", "e/f/g"] {
            let m = fs::metadata(root.join(rel)).expect("metadata");
            assert_eq!(m.modified().expect("mtime"), fixed, "{rel:?}");
        }
        assert_eq!(
            tree(&root).expect("tree"),
            [
                "a 6fcf9dfbd4",
                "d/",
                "d/b da39a3ee5e",
                "e/",
                "e/f/",
                "e/f/g 95cb0bfd29"
            ]
        );
    }

    #[test]
    fn coreutils_pinned_off_unless_required() {
        let (h, w) = (Path::new("h"), Path::new("w"));
        let pin = ("NNN_COREUTILS".to_owned(), "0".to_owned());
        assert!(case_env(h, w, &json!({"name": "x"})).contains(&pin));
        let env = case_env(h, w, &json!({"name": "x", "requires": ["coreutils"]}));
        assert!(!env.iter().any(|(k, _)| k == "NNN_COREUTILS"));
    }

    #[test]
    fn requires_coreutils_field() {
        assert!(needs_coreutils(&json!({"requires": ["coreutils"]})));
        assert!(!needs_coreutils(&json!({"name": "x"})));
    }

    #[test]
    fn platforms_filter() {
        assert!(runs_here(&json!({"name": "x"})));
        assert_eq!(
            runs_here(&json!({"platforms": ["unix"]})),
            PLATFORM == "unix"
        );
    }
}
