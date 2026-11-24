//! In-process async preview engine (Yazi-style single worker + cancellation).
//!
//! Spawns one background worker lazily on the first [`PreviewEngine::request`],
//! cancels stale work via an atomic generation counter, and kills any active
//! external previewer child immediately on cancellation or new requests.

use crate::browse::is_text_file;
use crate::config::cstr;
use crate::fmt::coolsize;
use crate::platform::{self, into_bytes, os, path as os_path};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::UNIX_EPOCH;

const MAX_DIR_ENTRIES: usize = 2000;
const MAX_LINE_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreviewKey {
    path: Vec<u8>,
    width: usize,
    height: usize,
    meta_sig: (u64, u64),
}

struct Job {
    generation: u64,
    key: PreviewKey,
    previewer: Option<Vec<u8>>,
}

struct PreviewResult {
    generation: u64,
    key: PreviewKey,
    lines: Vec<Vec<u8>>,
}

type ActiveChild = Arc<Mutex<Option<Child>>>;

/// Asynchronous, single-worker file/directory preview engine with caching and
/// immediate subprocess cancellation.
pub struct PreviewEngine {
    generation: Arc<AtomicU64>,
    active_child: ActiveChild,
    tx: Option<mpsc::Sender<Job>>,
    rx: Option<mpsc::Receiver<PreviewResult>>,
    cached_key: Option<PreviewKey>,
    cached_lines: Vec<Vec<u8>>,
    in_flight: Option<PreviewKey>,
}

impl Default for PreviewEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewEngine {
    /// Creates an idle engine; no background thread is spawned until [`Self::request`].
    pub fn new() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            active_child: Arc::new(Mutex::new(None)),
            tx: None,
            rx: None,
            cached_key: None,
            cached_lines: Vec::new(),
            in_flight: None,
        }
    }

    /// Requests a preview for `path` fitted to `width`x`height`.
    ///
    /// Returns `Some(lines)` immediately on a cache hit (after draining completed
    /// results via [`Self::poll`]), or `None` while the background worker computes it.
    pub fn request(
        &mut self,
        path: &[u8],
        previewer: Option<&[u8]>,
        width: usize,
        height: usize,
    ) -> Option<&[Vec<u8>]> {
        self.poll();

        if width == 0 || height == 0 {
            return Some(&[]);
        }

        let path = cstr(path);
        let previewer = previewer.map(cstr).filter(|p| !p.is_empty());
        let key = PreviewKey {
            path: path.to_vec(),
            width,
            height,
            meta_sig: meta_sig(path),
        };

        if self.cached_key.as_ref() == Some(&key) {
            return Some(&self.cached_lines);
        }
        if self.in_flight.as_ref() == Some(&key) {
            return None;
        }

        let job_gen = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        kill_active(&self.active_child);
        self.ensure_worker();

        self.in_flight = Some(key.clone());
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job {
                generation: job_gen,
                key,
                previewer: previewer.map(|p| p.to_vec()),
            });
        }
        None
    }

    /// Drains completed results without blocking. Returns `true` if the cache was updated.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut updated = false;
        let current_gen = self.generation.load(Ordering::Acquire);
        while let Ok(res) = rx.try_recv() {
            if res.generation == current_gen {
                self.cached_key = Some(res.key);
                self.cached_lines = res.lines;
                self.in_flight = None;
                updated = true;
            }
        }
        updated
    }

    /// Returns `true` if a preview job is currently in flight.
    pub fn is_pending(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Cancels any in-flight preview and kills any running external previewer child.
    pub fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        kill_active(&self.active_child);
        self.in_flight = None;
    }

    /// Clears the cached preview and cancels any in-flight job.
    pub fn invalidate(&mut self) {
        self.cached_key = None;
        self.cached_lines.clear();
        self.cancel();
    }

    fn ensure_worker(&mut self) {
        if self.tx.is_some() {
            return;
        }
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, res_rx) = mpsc::channel::<PreviewResult>();
        let generation = Arc::clone(&self.generation);
        let active_child = Arc::clone(&self.active_child);

        let _ = thread::Builder::new()
            .name("nnn-preview".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    if job.generation != generation.load(Ordering::Acquire) {
                        continue;
                    }
                    let lines = generate_preview(&job, &generation, &active_child);
                    if job.generation == generation.load(Ordering::Acquire) {
                        let _ = res_tx.send(PreviewResult {
                            generation: job.generation,
                            key: job.key,
                            lines,
                        });
                    }
                }
            });

        self.tx = Some(job_tx);
        self.rx = Some(res_rx);
    }
}

impl Drop for PreviewEngine {
    fn drop(&mut self) {
        self.cancel();
        self.tx = None;
    }
}

fn meta_sig(path: &[u8]) -> (u64, u64) {
    let Ok(meta) = fs::symlink_metadata(os_path(path)) else {
        return (0, 0);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    (meta.len(), mtime)
}

fn kill_active(active_child: &ActiveChild) {
    let child = active_child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn is_stale(generation: &AtomicU64, job_gen: u64) -> bool {
    generation.load(Ordering::Acquire) != job_gen
}

fn dim_line(text: &str) -> Vec<u8> {
    format!("\x1b[2m{text}\x1b[0m").into_bytes()
}

/// Runs `cmd`, merging stdout and stderr via `std::io::pipe()`, collecting up to
/// `max_lines` lines. Kills the child immediately on cancellation or once `max_lines`
/// are collected. Returns `Some((lines, exited_ok))` if the process spawned.
fn run_command_lines(
    mut cmd: Command,
    max_lines: usize,
    job_gen: u64,
    generation: &AtomicU64,
    active_child: &ActiveChild,
) -> Option<(Vec<Vec<u8>>, bool)> {
    if max_lines == 0 || is_stale(generation, job_gen) {
        return None;
    }

    let (reader, writer) = io::pipe().ok()?;
    let writer_err = writer.try_clone().ok()?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(writer_err));

    let child = cmd.spawn().ok()?;
    // Drop `cmd` immediately so our write ends of the pipe close before reading.
    drop(cmd);

    if is_stale(generation, job_gen) {
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    {
        let mut slot = active_child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut old) = slot.replace(child) {
            let _ = old.kill();
            let _ = old.wait();
        }
    }
    if is_stale(generation, job_gen) {
        kill_active(active_child);
        return None;
    }

    let mut br = BufReader::new(reader);
    let mut lines = Vec::new();
    let mut early_stop = false;

    while lines.len() < max_lines {
        if is_stale(generation, job_gen) {
            early_stop = true;
            break;
        }
        match read_line_capped(&mut br, MAX_LINE_BYTES) {
            Ok(Some(line)) => lines.push(line),
            Ok(None) | Err(_) => break,
        }
    }
    if lines.len() >= max_lines {
        early_stop = true;
    }
    drop(br);

    let child = active_child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let mut ok = early_stop && !is_stale(generation, job_gen);
    if let Some(mut child) = child {
        if early_stop {
            let _ = child.kill();
        }
        if let Ok(status) = child.wait()
            && !early_stop
        {
            ok = status.success();
        }
    }
    if is_stale(generation, job_gen) {
        return None;
    }
    Some((lines, ok))
}

/// Reads a single line up to `cap` bytes, discarding any remainder up to `\n`.
/// Strips trailing `\r` and `\n`. Returns `Ok(None)` at EOF.
fn read_line_capped<R: BufRead>(r: &mut R, cap: usize) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut got_any = false;
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            break;
        }
        got_any = true;
        if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let take = nl.min(cap.saturating_sub(out.len()));
            out.extend_from_slice(&buf[..take]);
            r.consume(nl + 1);
            break;
        }
        let take = buf.len().min(cap.saturating_sub(out.len()));
        out.extend_from_slice(&buf[..take]);
        let n = buf.len();
        r.consume(n);
    }
    if !got_any {
        return Ok(None);
    }
    if out.last() == Some(&b'\r') {
        out.pop();
    }
    Ok(Some(out))
}

fn generate_preview(job: &Job, generation: &AtomicU64, active_child: &ActiveChild) -> Vec<Vec<u8>> {
    let path = &job.key.path;
    let width = job.key.width;
    let height = job.key.height;

    // 1. Custom .npreview plugin.
    if let Some(previewer) = job.previewer.as_deref()
        && let Some(cmd) = build_previewer_cmd(previewer, path, width, height)
    {
        if let Some((lines, _)) =
            run_command_lines(cmd, height, job.generation, generation, active_child)
        {
            return lines;
        }
        if is_stale(generation, job.generation) {
            return Vec::new();
        }
    }

    let Ok(sym_meta) = fs::symlink_metadata(os_path(path)) else {
        return vec![dim_line("[unreadable]")];
    };

    // 2. Symlink: dim header + preview resolved target in remaining lines.
    if sym_meta.file_type().is_symlink() {
        let target = platform::readlink(path)
            .map(|t| String::from_utf8_lossy(&t).into_owned())
            .unwrap_or_else(|_| "?".into());
        let mut lines = vec![dim_line(&format!("-> {target}"))];
        if height > 1 {
            match fs::metadata(os_path(path)) {
                Ok(target_meta) => {
                    lines.extend(preview_entry(
                        path,
                        &target_meta,
                        width,
                        height - 1,
                        job.generation,
                        generation,
                        active_child,
                    ));
                }
                Err(_) => lines.push(dim_line("[broken symlink]")),
            }
        }
        return lines;
    }

    preview_entry(
        path,
        &sym_meta,
        width,
        height,
        job.generation,
        generation,
        active_child,
    )
}

fn preview_entry(
    path: &[u8],
    meta: &fs::Metadata,
    width: usize,
    height: usize,
    job_gen: u64,
    generation: &AtomicU64,
    active_child: &ActiveChild,
) -> Vec<Vec<u8>> {
    if height == 0 || is_stale(generation, job_gen) {
        return Vec::new();
    }

    // 3. Directory.
    if meta.is_dir() {
        return preview_dir(path, height, job_gen, generation);
    }

    // Never open FIFOs, sockets, or device nodes (blocking File::open would wedge the worker).
    if !meta.is_file() {
        return vec![dim_line(special_file_label(meta))];
    }

    let size = meta.len();
    let header = read_header(path);

    // 4. Image (by magic bytes).
    if is_image_magic(&header) {
        return preview_image(path, size, width, height, job_gen, generation, active_child);
    }

    // 5. Archive (by extension or magic bytes).
    if is_archive(path, &header) {
        return preview_archive(
            path,
            &header,
            size,
            height,
            job_gen,
            generation,
            active_child,
        );
    }

    // 6. Empty file.
    if size == 0 {
        return vec![dim_line("[empty file]")];
    }

    // 7. Text file.
    if is_text_file(path) {
        return preview_text(path, width, height, job_gen, generation, active_child);
    }

    // 8. Other binary.
    let kind = binary_kind(&header);
    vec![dim_line(&format!("[{kind} {}]", coolsize(size as i64)))]
}

fn build_previewer_cmd(
    previewer: &[u8],
    path: &[u8],
    width: usize,
    height: usize,
) -> Option<Command> {
    let w = width.to_string();
    let h = height.to_string();

    #[cfg(unix)]
    {
        if platform::access(previewer, platform::Access::Exec).is_err() {
            return None;
        }
        let mut cmd = Command::new(&*os(previewer));
        cmd.args([os(path).as_ref(), OsStr::new(&w), OsStr::new(&h)]);
        Some(cmd)
    }

    #[cfg(windows)]
    {
        let p = os_path(previewer);
        if !p.is_file() {
            return None;
        }
        let direct = p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            e.eq_ignore_ascii_case("exe")
                || e.eq_ignore_ascii_case("cmd")
                || e.eq_ignore_ascii_case("bat")
        });
        if direct {
            let mut cmd = Command::new(&*os(previewer));
            cmd.args([os(path).as_ref(), OsStr::new(&w), OsStr::new(&h)]);
            Some(cmd)
        } else if platform::has_sh() {
            let mut cmd = Command::new("sh");
            cmd.args([
                os(previewer).as_ref(),
                os(path).as_ref(),
                OsStr::new(&w),
                OsStr::new(&h),
            ]);
            Some(cmd)
        } else {
            None
        }
    }
}

fn preview_dir(path: &[u8], height: usize, job_gen: u64, generation: &AtomicU64) -> Vec<Vec<u8>> {
    let Ok(rd) = fs::read_dir(os_path(path)) else {
        return vec![dim_line("[unreadable directory]")];
    };

    let mut entries: Vec<(bool, Vec<u8>)> = Vec::new();
    let mut truncated = false;
    for item in rd {
        if is_stale(generation, job_gen) {
            return Vec::new();
        }
        let Ok(entry) = item else {
            continue;
        };
        if entries.len() >= MAX_DIR_ENTRIES {
            truncated = true;
            break;
        }
        let is_dir = entry.file_type().ok().is_some_and(|ft| ft.is_dir());
        let name = into_bytes(entry.file_name());
        entries.push((is_dir, name));
    }

    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()))
            .then_with(|| a.1.cmp(&b.1))
    });

    let count_str = if truncated {
        format!("{MAX_DIR_ENTRIES}+ items")
    } else if entries.len() == 1 {
        "1 item".to_string()
    } else {
        format!("{} items", entries.len())
    };
    let mut lines = vec![dim_line(&format!("[directory ({count_str})]"))];

    for (is_dir, mut name) in entries.into_iter().take(height.saturating_sub(1)) {
        if is_dir {
            name.push(b'/');
        }
        lines.push(name);
    }
    lines
}

fn read_header(path: &[u8]) -> Vec<u8> {
    let Ok(mut f) = File::open(os_path(path)) else {
        return Vec::new();
    };
    let mut buf = [0u8; 64];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].to_vec()
}

fn is_image_magic(h: &[u8]) -> bool {
    h.starts_with(b"\x89PNG")
        || h.starts_with(b"\xff\xd8\xff")
        || h.starts_with(b"GIF8")
        || h.starts_with(b"BM")
        || (h.len() >= 12 && &h[0..4] == b"RIFF" && &h[8..12] == b"WEBP")
}

/// Pure-Rust image header dimension parser for PNG, JPEG (SOF0/1/2), GIF, BMP, and WebP.
pub fn image_dimensions(path: &[u8]) -> Option<(&'static str, u32, u32)> {
    let mut f = File::open(os_path(cstr(path))).ok()?;
    let mut buf = [0u8; 32];
    let n = f.read(&mut buf).ok()?;
    let h = &buf[..n];

    if h.len() >= 24 && h.starts_with(b"\x89PNG\r\n\x1a\n") && &h[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([h[16], h[17], h[18], h[19]]);
        let ht = u32::from_be_bytes([h[20], h[21], h[22], h[23]]);
        return Some(("PNG", w, ht));
    }
    if h.len() >= 10 && h.starts_with(b"GIF8") {
        let w = u16::from_le_bytes([h[6], h[7]]) as u32;
        let ht = u16::from_le_bytes([h[8], h[9]]) as u32;
        return Some(("GIF", w, ht));
    }
    if h.len() >= 26 && h.starts_with(b"BM") {
        let w = i32::from_le_bytes([h[18], h[19], h[20], h[21]]).unsigned_abs();
        let ht = i32::from_le_bytes([h[22], h[23], h[24], h[25]]).unsigned_abs();
        return Some(("BMP", w, ht));
    }
    if h.len() >= 25 && &h[0..4] == b"RIFF" && &h[8..12] == b"WEBP" {
        return parse_webp_dims(h);
    }
    if h.starts_with(b"\xff\xd8\xff") {
        // Read up to 64 KiB to scan past EXIF/APP segments to SOF0/SOF1/SOF2.
        let mut data = h.to_vec();
        let mut rest = vec![0u8; 65536];
        if let Ok(m) = f.read(&mut rest) {
            data.extend_from_slice(&rest[..m]);
        }
        return parse_jpeg_dims(&data);
    }
    None
}

fn parse_webp_dims(h: &[u8]) -> Option<(&'static str, u32, u32)> {
    match &h[12..16] {
        b"VP8 " if h.len() >= 30 && h[23..26] == [0x9d, 0x01, 0x2a] => {
            let w = (u16::from_le_bytes([h[26], h[27]]) & 0x3FFF) as u32;
            let ht = (u16::from_le_bytes([h[28], h[29]]) & 0x3FFF) as u32;
            Some(("WebP", w, ht))
        }
        b"VP8L" if h.len() >= 25 && h[20] == 0x2f => {
            let bits = u32::from_le_bytes([h[21], h[22], h[23], h[24]]);
            let w = (bits & 0x3FFF) + 1;
            let ht = ((bits >> 14) & 0x3FFF) + 1;
            Some(("WebP", w, ht))
        }
        b"VP8X" if h.len() >= 30 => {
            let w = 1 + (h[24] as u32 | ((h[25] as u32) << 8) | ((h[26] as u32) << 16));
            let ht = 1 + (h[27] as u32 | ((h[28] as u32) << 8) | ((h[29] as u32) << 16));
            Some(("WebP", w, ht))
        }
        _ => None,
    }
}

fn parse_jpeg_dims(data: &[u8]) -> Option<(&'static str, u32, u32)> {
    let mut i = 2;
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            return None;
        }
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let marker = data[i];
        i += 1;
        if matches!(marker, 0x00 | 0xD9 | 0xDA) {
            return None;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD8) {
            continue;
        }
        if i + 2 > data.len() {
            return None;
        }
        let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if seg_len < 2 {
            return None;
        }
        if matches!(marker, 0xC0..=0xC2) {
            if i + 7 > data.len() {
                return None;
            }
            let ht = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            let w = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            return Some(("JPEG", w, ht));
        }
        i = i.saturating_add(seg_len);
    }
    None
}

fn preview_image(
    path: &[u8],
    size: u64,
    width: usize,
    height: usize,
    job_gen: u64,
    generation: &AtomicU64,
    active_child: &ActiveChild,
) -> Vec<Vec<u8>> {
    let sz = coolsize(size as i64);
    let header = match image_dimensions(path) {
        Some((fmt, w, h)) => dim_line(&format!("[image {fmt} {w}x{h}, {sz}]")),
        None => dim_line(&format!("[image {sz}]")),
    };
    let mut lines = vec![header];
    if height <= 1 {
        return lines;
    }

    let body_h = height - 1;
    let cmd = if let Some(chafa) = chafa_bin() {
        let mut c = Command::new(chafa);
        c.args([
            "--format=symbols",
            "--color-space=rgb",
            "--size",
            &format!("{width}x{body_h}"),
        ])
        .arg(&*os(path));
        Some(c)
    } else if let Some(viu) = viu_bin() {
        let mut c = Command::new(viu);
        c.args(["-w", &width.to_string(), "-h", &body_h.to_string()])
            .arg(&*os(path));
        Some(c)
    } else {
        None
    };

    if let Some(cmd) = cmd
        && let Some((rendered, _)) =
            run_command_lines(cmd, body_h, job_gen, generation, active_child)
    {
        lines.extend(rendered);
    }
    lines
}

fn special_file_label(meta: &fs::Metadata) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let ft = meta.file_type();
        if ft.is_fifo() {
            return "[fifo]";
        }
        if ft.is_socket() {
            return "[socket]";
        }
        if ft.is_block_device() {
            return "[block device]";
        }
        if ft.is_char_device() {
            return "[char device]";
        }
    }
    let _ = meta;
    "[special file]"
}

fn is_archive(path: &[u8], header: &[u8]) -> bool {
    if header.starts_with(b"PK\x03\x04") || header.starts_with(b"\x1f\x8b") {
        return true;
    }
    const EXTS: &[&[u8]] = &[
        b".tar.gz",
        b".tar.bz2",
        b".tar.xz",
        b".tar.zst",
        b".tar",
        b".tgz",
        b".tbz2",
        b".txz",
        b".zip",
    ];
    let lower = path.to_ascii_lowercase();
    EXTS.iter().any(|ext| lower.ends_with(ext))
}

fn is_zip(path: &[u8], header: &[u8]) -> bool {
    header.starts_with(b"PK\x03\x04") || path.to_ascii_lowercase().ends_with(b".zip")
}

fn preview_archive(
    path: &[u8],
    header: &[u8],
    size: u64,
    height: usize,
    job_gen: u64,
    generation: &AtomicU64,
    active_child: &ActiveChild,
) -> Vec<Vec<u8>> {
    let mut lines = vec![dim_line(&format!("[archive {}]", coolsize(size as i64)))];
    if height <= 1 {
        return lines;
    }
    let body_h = height - 1;

    if let Some(tar) = tar_bin() {
        let mut cmd = Command::new(tar);
        cmd.arg("-tf").arg(&*os(path));
        if let Some((out, ok)) = run_command_lines(cmd, body_h, job_gen, generation, active_child)
            && ok
            && !out.is_empty()
        {
            lines.extend(out);
            return lines;
        }
        if is_stale(generation, job_gen) {
            return Vec::new();
        }
    }

    if is_zip(path, header)
        && let Some(unzip) = unzip_bin()
    {
        let mut cmd = Command::new(unzip);
        cmd.arg("-Z1").arg(&*os(path));
        if let Some((out, ok)) = run_command_lines(cmd, body_h, job_gen, generation, active_child)
            && ok
            && !out.is_empty()
        {
            lines.extend(out);
            return lines;
        }
    }
    lines
}

fn preview_text(
    path: &[u8],
    width: usize,
    height: usize,
    job_gen: u64,
    generation: &AtomicU64,
    active_child: &ActiveChild,
) -> Vec<Vec<u8>> {
    if let Some(bat) = bat_bin() {
        let mut cmd = Command::new(bat);
        cmd.args([
            "--color=always",
            "--style=plain",
            "--paging=never",
            &format!("--line-range=:{height}"),
            &format!("--terminal-width={width}"),
            "--",
        ])
        .arg(&*os(path));
        if let Some((out, ok)) = run_command_lines(cmd, height, job_gen, generation, active_child)
            && ok
        {
            return out;
        }
        if is_stale(generation, job_gen) {
            return Vec::new();
        }
    }

    let Ok(f) = File::open(os_path(path)) else {
        return vec![dim_line("[unreadable]")];
    };
    let mut br = BufReader::new(f);
    let mut lines = Vec::new();
    while lines.len() < height {
        if is_stale(generation, job_gen) {
            return Vec::new();
        }
        let Ok(Some(raw)) = read_line_capped(&mut br, MAX_LINE_BYTES) else {
            break;
        };
        if raw.is_empty() {
            lines.push(Vec::new());
            continue;
        }
        // Soft-wrap long lines at UTF-8 char boundaries to `width` chars.
        let text = String::from_utf8_lossy(&raw);
        let mut start = 0;
        let mut count = 0;
        for (idx, _) in text.char_indices() {
            if count == width {
                lines.push(text[start..idx].as_bytes().to_vec());
                if lines.len() >= height {
                    break;
                }
                start = idx;
                count = 0;
            }
            count += 1;
        }
        if lines.len() < height && start < text.len() {
            lines.push(text[start..].as_bytes().to_vec());
        }
    }
    lines
}

fn binary_kind(h: &[u8]) -> &'static str {
    if h.starts_with(b"%PDF") {
        "PDF"
    } else if h.starts_with(b"\x7fELF") {
        "ELF"
    } else if h.starts_with(b"\xfe\xed\xfa\xce")
        || h.starts_with(b"\xfe\xed\xfa\xcf")
        || h.starts_with(b"\xce\xfa\xed\xfe")
        || h.starts_with(b"\xcf\xfa\xed\xfe")
        || h.starts_with(b"\xca\xfe\xba\xbe")
    {
        "Mach-O"
    } else if h.starts_with(b"SQLite format 3\0") {
        "SQLite"
    } else if h.starts_with(b"ID3")
        || h.starts_with(b"OggS")
        || h.starts_with(b"fLaC")
        || h.starts_with(b"MThd")
        || (h.len() >= 12 && &h[0..4] == b"RIFF" && &h[8..12] == b"WAVE")
    {
        "audio"
    } else if h.starts_with(b"7z\xbc\xaf\x27\x1c") {
        "7z"
    } else if h.starts_with(b"\xfd7zXZ\x00") {
        "xz"
    } else {
        "binary"
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        platform::which(name.as_bytes()).map(|b| os_path(&b).into_owned())
    }
    #[cfg(unix)]
    {
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(name);
            if cand.is_file()
                && platform::access(&platform::bytes(cand.as_os_str()), platform::Access::Exec)
                    .is_ok()
            {
                return Some(cand);
            }
        }
        None
    }
}

fn chafa_bin() -> Option<&'static PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| find_in_path("chafa")).as_ref()
}

fn viu_bin() -> Option<&'static PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| find_in_path("viu")).as_ref()
}

fn bat_bin() -> Option<&'static PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| find_in_path("bat")).as_ref()
}

fn unzip_bin() -> Option<&'static PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| find_in_path("unzip")).as_ref()
}

fn tar_bin() -> Option<&'static PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        #[cfg(windows)]
        if let Some(sysroot) = std::env::var_os("SystemRoot") {
            let win_tar = PathBuf::from(sysroot).join("System32").join("tar.exe");
            if win_tar.is_file() {
                return Some(win_tar);
            }
        }
        find_in_path("tar")
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nnn-preview-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn path_bytes(p: &std::path::Path) -> Vec<u8> {
        platform::bytes(p.as_os_str()).into_owned()
    }

    #[test]
    fn image_dimensions_parsers() {
        let dir = scratch("img");

        // PNG: 8-byte sig + 4-byte len + "IHDR" + 4-byte w (320) + 4-byte h (240)
        let png = dir.join("a.png");
        let mut png_bytes = Vec::new();
        png_bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png_bytes.extend_from_slice(&13u32.to_be_bytes());
        png_bytes.extend_from_slice(b"IHDR");
        png_bytes.extend_from_slice(&320u32.to_be_bytes());
        png_bytes.extend_from_slice(&240u32.to_be_bytes());
        fs::write(&png, &png_bytes).unwrap();
        assert_eq!(image_dimensions(&path_bytes(&png)), Some(("PNG", 320, 240)));

        // GIF89a: 640x480
        let gif = dir.join("a.gif");
        let mut gif_bytes = b"GIF89a".to_vec();
        gif_bytes.extend_from_slice(&640u16.to_le_bytes());
        gif_bytes.extend_from_slice(&480u16.to_le_bytes());
        fs::write(&gif, &gif_bytes).unwrap();
        assert_eq!(image_dimensions(&path_bytes(&gif)), Some(("GIF", 640, 480)));

        // BMP: 100x50 (top-down negative height -50)
        let bmp = dir.join("a.bmp");
        let mut bmp_bytes = vec![0u8; 26];
        bmp_bytes[0..2].copy_from_slice(b"BM");
        bmp_bytes[18..22].copy_from_slice(&100i32.to_le_bytes());
        bmp_bytes[22..26].copy_from_slice(&(-50i32).to_le_bytes());
        fs::write(&bmp, &bmp_bytes).unwrap();
        assert_eq!(image_dimensions(&path_bytes(&bmp)), Some(("BMP", 100, 50)));

        // JPEG: SOI + APP0 (length 4) + SOF0 (1920x1080)
        let jpg = dir.join("a.jpg");
        let mut jpg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpg_bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        jpg_bytes.extend_from_slice(&1080u16.to_be_bytes());
        jpg_bytes.extend_from_slice(&1920u16.to_be_bytes());
        jpg_bytes.extend_from_slice(&[0x03, 0x00, 0x00]);
        fs::write(&jpg, &jpg_bytes).unwrap();
        assert_eq!(
            image_dimensions(&path_bytes(&jpg)),
            Some(("JPEG", 1920, 1080))
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_preview_sorted_dirs_first() {
        let dir = scratch("dir");
        fs::create_dir(dir.join("zebra_dir")).unwrap();
        fs::create_dir(dir.join("Alpha_dir")).unwrap();
        fs::write(dir.join("beta.txt"), b"b").unwrap();
        fs::write(dir.join("aaa.txt"), b"a").unwrap();

        let generation = AtomicU64::new(1);
        let lines = preview_dir(&path_bytes(&dir), 10, 1, &generation);
        assert_eq!(lines[0], dim_line("[directory (4 items)]"));
        assert_eq!(lines[1], b"Alpha_dir/");
        assert_eq!(lines[2], b"zebra_dir/");
        assert_eq!(lines[3], b"aaa.txt");
        assert_eq!(lines[4], b"beta.txt");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_and_empty_file_preview() {
        let dir = scratch("text");
        let empty = dir.join("empty.txt");
        fs::write(&empty, b"").unwrap();
        let text = dir.join("hello.txt");
        fs::write(&text, b"first line\nsecond line\n").unwrap();

        let generation = AtomicU64::new(1);
        let active = Arc::new(Mutex::new(None));

        let empty_job = Job {
            generation: 1,
            key: PreviewKey {
                path: path_bytes(&empty),
                width: 40,
                height: 10,
                meta_sig: (0, 0),
            },
            previewer: None,
        };
        assert_eq!(
            generate_preview(&empty_job, &generation, &active),
            vec![dim_line("[empty file]")]
        );

        let text_job = Job {
            generation: 1,
            key: PreviewKey {
                path: path_bytes(&text),
                width: 40,
                height: 10,
                meta_sig: (23, 0),
            },
            previewer: None,
        };
        let lines = generate_preview(&text_job, &generation, &active);
        assert!(lines.len() >= 2);
        assert!(String::from_utf8_lossy(&lines[0]).contains("first line"));
        assert!(String::from_utf8_lossy(&lines[1]).contains("second line"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_async_request_poll_cache_and_cancel() {
        let dir = scratch("engine");
        let f = dir.join("note.txt");
        fs::write(&f, b"hello preview\n").unwrap();
        let p = path_bytes(&f);

        let mut engine = PreviewEngine::new();
        assert!(!engine.is_pending());

        // First request is async -> None, pending = true.
        assert!(engine.request(&p, None, 40, 10).is_none());
        assert!(engine.is_pending());

        // Wait for worker to finish.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !engine.poll() {
            assert!(Instant::now() < deadline, "preview worker timed out");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!engine.is_pending());

        // Cache hit returns Some immediately.
        let lines = engine.request(&p, None, 40, 10).expect("cache hit");
        assert!(!lines.is_empty());
        assert!(String::from_utf8_lossy(&lines[0]).contains("hello preview"));

        // Invalidate + cancel clears cache and pending state.
        engine.invalidate();
        assert!(!engine.is_pending());
        assert!(engine.request(&p, None, 40, 10).is_none());
        engine.cancel();
        assert!(!engine.is_pending());

        let _ = fs::remove_dir_all(&dir);
    }
}
