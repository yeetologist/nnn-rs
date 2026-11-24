//! Disk-usage mode: dirwalk, the du worker threads, and dentfill's blkorder path.
//!
//! The C original shares globals between the main thread and its worker pool, guarded by
//! `running_mutex` (the task queue) and `du_count_mutex` (group and per-core counts). Here
//! those are `Shared::queue` and `Shared::counts`; each `du_group` has its own lock, taken
//! where the original holds `du_count_mutex`. Worker results for an entry (the original's
//! `pdents[entnum].blocks` write) are collected and applied when dentfill waits for the pool.

use crate::config::mkpath;
use crate::dir::DuHook;
use crate::platform::{self, DType, Dir, FileInfo};
use crate::sort::Entry;
use crate::state::Nnn;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// `HASH_BITS`
const HASH_BITS: u32 = 0xFF_FFFF;
/// `HASH_OCTETS`; the original allocates this many 64-bit words but indexes one past them
/// (`HASH_BITS >> 6 == HASH_OCTETS`), so the bitmaps here have one word more.
const HASH_OCTETS: usize = (HASH_BITS >> 6) as usize;
/// `NUM_DU_THREADS_MAX`
const NUM_DU_THREADS_MAX: usize = 32;
/// The original's `entry.blocks` is a 40-bit field: larger values wrap.
const BLOCKS_MASK: u64 = (1 << 40) - 1;
/// `TASK_CAP_DU`
const TASK_CAP_DU: usize = 256;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

fn is_dir(mode: u32) -> bool {
    mode & S_IFMT == S_IFDIR
}

fn is_reg(mode: u32) -> bool {
    mode & S_IFMT == S_IFREG
}

fn is_lnk(mode: u32) -> bool {
    mode & S_IFMT == S_IFLNK
}

/// A poisoned lock only means a worker panicked; the counts it guards stay usable.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `du_group`: one top-level directory being measured by one or more tasks.
#[derive(Debug, Default)]
struct DuGroup {
    pending: i64,
    blocks: i64,
    files: u64,
    entnum: i32,
    mntpoint: bool,
    no_aggregate: bool,
}

/// `du_task`
#[derive(Debug)]
struct DuTask {
    path: Vec<u8>,
    group: Arc<Mutex<DuGroup>>,
    count_root: bool,
}

/// What `running_mutex` guards: `du_tasks`/`du_task_len`, `du_tasks_pending`,
/// `active_threads`, `du_shutdown`.
#[derive(Debug, Default)]
struct Queue {
    tasks: Vec<DuTask>,
    pending: usize,
    active: usize,
    shutdown: bool,
}

/// What `du_count_mutex` guards besides the groups: `core_blocks`, `core_files`, and the
/// entries' block counts the workers produce (`pdents[entnum].blocks`).
#[derive(Debug, Default)]
struct Counts {
    core_blocks: Vec<i64>,
    core_files: Vec<u64>,
    entblocks: Vec<(usize, i64)>,
}

/// State shared with the worker threads.
struct Shared {
    queue: Mutex<Queue>,
    /// `work_cond`
    work_cond: Condvar,
    /// `du_cond`
    du_cond: Condvar,
    counts: Mutex<Counts>,
    /// `ihashbmp`
    ihashbmp: Vec<AtomicU64>,
    /// `g_state.interrupt` as SIGINT sets it.
    interrupt: Arc<AtomicBool>,
    /// `cfg.apparentsz`
    apparentsz: AtomicBool,
    /// `home`, for `mkpath`.
    home: Vec<u8>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("queue", &self.queue)
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

/// State this module keeps.
#[derive(Debug, Default)]
pub struct DuState {
    shared: Option<Arc<Shared>>,
    /// `dir_dispatched_bmp` (main thread only).
    dir_dispatched_bmp: Vec<u64>,
    /// `worker_tids`
    worker_tids: Vec<JoinHandle<()>>,
    /// `num_du_threads`
    num_du_threads: usize,
    /// `first_call`
    first_call: bool,
}

impl Drop for DuState {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl DuState {
    /// The thread shutdown of `dentfree`: stop the workers, drop queued tasks, join.
    pub fn shutdown(&mut self) {
        let Some(sh) = &self.shared else { return };
        {
            let mut q = lock(&sh.queue);
            q.shutdown = true;
            q.tasks.clear();
            q.pending = 0;
            sh.work_cond.notify_all();
        }
        for t in self.worker_tids.drain(..) {
            let _ = t.join();
        }
    }

    /// Release the worker pool and the 4 MiB inode/dir bitmaps when leaving DU mode.
    pub fn release(&mut self) {
        self.shutdown();
        self.shared = None;
        self.dir_dispatched_bmp = Vec::new();
        self.num_du_threads = 0;
    }
}

/// `test_set_bit`: atomically mark inode `nr`; true if it was not marked yet.
fn test_set_bit(bmp: &[AtomicU64], nr: u32) -> bool {
    let nr = nr & HASH_BITS;
    let bit = 1u64 << (nr & 63);
    let old = bmp[(nr >> 6) as usize].fetch_or(bit, Ordering::Relaxed);
    old & bit == 0
}

/// `test_set_bit_dir`: mark directory (dev, ino) as dispatched; true the first time.
fn test_set_bit_dir(bmp: &mut [u64], dev: u64, ino: u64) -> bool {
    let nr = ((dev ^ ino) as u32) & HASH_BITS;
    let m = &mut bmp[(nr >> 6) as usize];
    let bit = 1u64 << (nr & 63);
    if *m & bit != 0 {
        return false;
    }
    *m |= bit;
    true
}

/// `du_pending_inc`
fn du_pending_inc(group: &Mutex<DuGroup>, inc_pending: bool) {
    if inc_pending {
        lock(group).pending += 1;
    }
}

/// `du_pending_dec`
fn du_pending_dec(group: &Mutex<DuGroup>, inc_pending: bool) {
    if inc_pending {
        lock(group).pending -= 1;
    }
}

/// `du_queue_task`
fn du_queue_task(
    sh: &Shared,
    path: &[u8],
    group: &Arc<Mutex<DuGroup>>,
    count_root: bool,
    inc_pending: bool,
) -> bool {
    du_pending_inc(group, inc_pending);
    let mut q = lock(&sh.queue);
    if sh.interrupt.load(Ordering::Relaxed) {
        drop(q);
        du_pending_dec(group, inc_pending);
        return false;
    }
    q.tasks.push(DuTask {
        path: path.to_vec(),
        group: Arc::clone(group),
        count_root,
    });
    q.pending += 1;
    sh.work_cond.notify_one();
    true
}

/// `add_blocks`
fn add_blocks(sh: &Shared, tblocks: &mut i64, sb: &FileInfo) {
    if sh.apparentsz.load(Ordering::Relaxed) {
        *tblocks += sb.size;
    } else if sb.blocks512 != 0 {
        *tblocks += sb.blocks512 as i64;
    }
}

/// `lazy_stat`
fn lazy_stat(dir: &Dir, name: &[u8], sb: &mut Option<FileInfo>) -> bool {
    if sb.is_none() {
        match dir.stat_at(name, false) {
            Ok(s) => *sb = Some(s),
            Err(_) => return false,
        }
    }
    true
}

/// `du_walk_dir`
fn du_walk_dir(
    sh: &Shared,
    root: &[u8],
    group: &Arc<Mutex<DuGroup>>,
    count_root: bool,
    tfiles: &mut u64,
    tblocks: &mut i64,
) {
    if sh.interrupt.load(Ordering::Relaxed) {
        return;
    }
    let Ok(sb_root) = platform::stat(root, false) else {
        return;
    };
    if !is_dir(sb_root.mode) {
        return;
    }
    let root_dev = sb_root.dev;
    if count_root {
        add_blocks(sh, tblocks, &sb_root);
        *tfiles += 1;
    }
    if sh.interrupt.load(Ordering::Relaxed) {
        return;
    }
    let Ok(dfd) = Dir::open(root) else {
        return;
    };
    if sh.interrupt.load(Ordering::Relaxed) {
        return;
    }
    let Ok(entries) = dfd.entries() else {
        return;
    };
    for dp in entries {
        let Ok((name, dtype)) = dp else { break };
        if sh.interrupt.load(Ordering::Relaxed) {
            break;
        }
        let name = name.as_slice();
        if crate::dir::selforparent(name) {
            continue;
        }
        let mut isdir = dtype == DType::Dir;
        let mut isreg = dtype == DType::Regular;
        let mut sb: Option<FileInfo> = None;
        if dtype == DType::Unknown {
            let Ok(s) = dfd.stat_at(name, false) else {
                continue;
            };
            isdir = is_dir(s.mode);
            isreg = is_reg(s.mode);
            sb = Some(s);
        }
        if isdir {
            if !lazy_stat(&dfd, name, &mut sb) {
                continue;
            }
            if let Some(s) = &sb {
                add_blocks(sh, tblocks, s);
            }
        } else if isreg {
            if !lazy_stat(&dfd, name, &mut sb) {
                continue;
            }
            if let Some(s) = &sb {
                // Do not recount hard links
                if s.size != 0 && (s.nlink <= 1 || test_set_bit(&sh.ihashbmp, s.ino as u32)) {
                    add_blocks(sh, tblocks, s);
                }
            }
        }
        *tfiles += 1;
        if isdir {
            if !lazy_stat(&dfd, name, &mut sb) {
                continue;
            }
            if sb.as_ref().is_some_and(|s| s.dev == root_dev) {
                let (child, _) = mkpath(root, name, &sh.home);
                du_queue_task(sh, &child, group, false, true);
            }
        }
    }
}

/// `du_worker_loop` (without pinning the thread to a CPU core).
fn du_worker_loop(sh: Arc<Shared>, core: usize) {
    loop {
        let task = {
            let mut q = lock(&sh.queue);
            while q.tasks.is_empty() && !q.shutdown {
                q = sh.work_cond.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
            if q.shutdown {
                return;
            }
            let Some(task) = q.tasks.pop() else { continue };
            q.active += 1;
            task
        };

        let mut tfiles = 0u64;
        let mut tblocks = 0i64;
        if !sh.interrupt.load(Ordering::Relaxed) {
            du_walk_dir(
                &sh,
                &task.path,
                &task.group,
                task.count_root,
                &mut tfiles,
                &mut tblocks,
            );
        }

        // Aggregate into the shared group and finalize when done
        {
            let mut g = lock(&task.group);
            g.blocks += tblocks;
            g.files += tfiles;
            if g.pending > 0 {
                g.pending -= 1;
            }
            if g.pending == 0 {
                let mut c = lock(&sh.counts);
                if g.entnum >= 0 {
                    c.entblocks.push((g.entnum as usize, g.blocks));
                }
                if !g.no_aggregate {
                    if !g.mntpoint {
                        c.core_blocks[core] += g.blocks;
                        c.core_files[core] += g.files;
                    } else {
                        c.core_files[core] += 1;
                    }
                }
            }
        }

        let mut q = lock(&sh.queue);
        q.active -= 1;
        if q.pending > 0 {
            q.pending -= 1;
        }
        sh.du_cond.notify_one();
    }
}

/// The disk-usage half of `dentfill`, called by dir.rs at the points the original's
/// `cfg.blkorder` branches sit.
struct DuFill<'a> {
    nnn: &'a mut Nnn,
    /// `sb_path.st_dev`
    path_dev: u64,
}

impl Nnn {
    /// `g_state.interrupt`, which SIGINT sets.
    fn du_interrupted(&self) -> bool {
        self.g_state.interrupt || self.browse.sigint.load(Ordering::Relaxed)
    }

    /// `dentfill`: the entries of `path`; in du mode also `dir_blocks`, `num_files` and each
    /// entry's `blocks`.
    pub fn dentfill(&mut self, path: &[u8], fill: crate::dir::FillCfg) -> Vec<Entry> {
        if !self.cfg.sort.blkorder {
            if self.g_state.duinit
                || self.du.shared.is_some()
                || !self.du.dir_dispatched_bmp.is_empty()
            {
                self.release_du();
            }
            return crate::dir::dentfill(path, fill);
        }
        let mut hook = DuFill {
            nnn: self,
            path_dev: 0,
        };
        crate::dir::dentfill_with(path, fill, Some(&mut hook))
    }

    /// Release DU workers and bitmaps when leaving disk-usage mode.
    pub fn release_du(&mut self) {
        self.du.release();
        self.g_state.duinit = false;
    }

    /// `dirwalk`: queue `path` for the workers as a new group.
    fn dirwalk(&mut self, path: &[u8], entnum: i32, mountpoint: bool, no_aggregate: bool) {
        if self.du_interrupted() {
            return;
        }
        let Some(sh) = self.du.shared.clone() else {
            return;
        };
        let group = Arc::new(Mutex::new(DuGroup {
            pending: 1,
            entnum,
            mntpoint: mountpoint,
            no_aggregate,
            ..DuGroup::default()
        }));
        if !du_queue_task(&sh, path, &group, true, false) {
            return;
        }
        if self.du.first_call {
            if self.curses.is_some() {
                self.tolastln();
                let cs = self.cs();
                cs.stdscr.addstr(b"[^C aborts]\n");
                cs.refresh();
            }
            self.du.first_call = false;
        }
    }

    /// `prep_threads`
    fn prep_threads(&mut self) -> bool {
        let Some(sh) = self.du.shared.clone() else {
            return false;
        };
        if !self.g_state.duinit {
            let n = std::thread::available_parallelism().map_or(0, usize::from);
            let mut num = if n > 0 && n <= NUM_DU_THREADS_MAX {
                n
            } else {
                4
            };
            if num < 2 {
                num = 2;
            }
            self.du.num_du_threads = num;
            {
                let mut q = lock(&sh.queue);
                q.shutdown = false;
                q.active = 0;
                q.tasks.clear();
                q.tasks.reserve(TASK_CAP_DU);
                q.pending = 0;
            }
            {
                let mut c = lock(&sh.counts);
                c.core_blocks = vec![0; num];
                c.core_files = vec![0; num];
                c.entblocks.clear();
            }
            for i in 0..num {
                let s = Arc::clone(&sh);
                match std::thread::Builder::new().spawn(move || du_worker_loop(s, i)) {
                    Ok(t) => self.du.worker_tids.push(t),
                    Err(_) => {
                        self.du.shutdown();
                        if self.curses.is_some() {
                            self.printwarn(None);
                        }
                        return false;
                    }
                }
            }
            platform::raise_open_files_limit();
            self.g_state.duinit = true;
        } else {
            {
                let mut c = lock(&sh.counts);
                c.core_blocks.iter_mut().for_each(|b| *b = 0);
                c.core_files.iter_mut().for_each(|f| *f = 0);
                c.entblocks.clear();
            }
            let mut q = lock(&sh.queue);
            q.tasks.clear();
            q.pending = 0;
        }
        true
    }

    /// The thread shutdown at the start of `dentfree`.
    pub fn dentfree_du(&mut self) {
        if self.g_state.duinit {
            self.du.shutdown();
            self.g_state.duinit = false;
        }
    }
}

impl DuHook for DuFill<'_> {
    /// dentfill's setup when `cfg.blkorder` is set (7316-7343).
    fn begin(&mut self, dir: &Dir, path: &[u8]) -> bool {
        let n = &mut *self.nnn;
        n.du.first_call = true;
        n.num_files = 0;
        n.dir_blocks = 0;
        let Ok(sb_path) = dir.stat_at(path, true) else {
            return false;
        };
        self.path_dev = sb_path.dev;
        match &n.du.shared {
            Some(sh) => sh
                .ihashbmp
                .iter()
                .for_each(|w| w.store(0, Ordering::Relaxed)),
            None => {
                n.du.shared = Some(Arc::new(Shared {
                    queue: Mutex::new(Queue::default()),
                    work_cond: Condvar::new(),
                    du_cond: Condvar::new(),
                    counts: Mutex::new(Counts::default()),
                    ihashbmp: (0..=HASH_OCTETS).map(|_| AtomicU64::new(0)).collect(),
                    interrupt: Arc::clone(&n.browse.sigint),
                    apparentsz: AtomicBool::new(false),
                    home: n.home.clone(),
                }));
            }
        }
        if let Some(sh) = &n.du.shared {
            sh.apparentsz
                .store(n.cfg.sort.apparentsz, Ordering::Relaxed);
        }
        if n.du.dir_dispatched_bmp.is_empty() {
            n.du.dir_dispatched_bmp = vec![0; HASH_OCTETS + 1];
        } else {
            n.du.dir_dispatched_bmp.iter_mut().for_each(|w| *w = 0);
        }
        if !n.prep_threads() {
            return false;
        }
        if n.curses.is_some() {
            n.attron_curctx();
        }
        true
    }

    /// A hidden entry while hidden files are not shown (7374-7407); false to stop (interrupt).
    fn hidden(&mut self, dir: &Dir, path: &[u8], name: &[u8]) -> bool {
        let Ok(sb) = dir.stat_at(name, false) else {
            return true;
        };
        // Use resolved (dev,ino) for duplicate check (symlink-to-dir vs real dir)
        let resolved = if is_lnk(sb.mode) {
            dir.stat_at(name, true).ok()
        } else {
            Some(sb)
        };
        let n = &mut *self.nnn;
        match resolved.filter(|s| is_dir(s.mode)) {
            Some(sd) => {
                if self.path_dev == sd.dev {
                    let (buf, _) = mkpath(path, name, &n.home);
                    let first = test_set_bit_dir(&mut n.du.dir_dispatched_bmp, sd.dev, sd.ino);
                    n.dirwalk(&buf, -1, false, !first);
                    if n.du_interrupted() {
                        return false;
                    }
                }
                n.num_files += 1;
            }
            None => {
                if sb.size != 0 && is_reg(sb.mode) && (sb.nlink <= 1 || n.ihash_set(sb.ino)) {
                    n.dir_blocks += if n.cfg.sort.apparentsz {
                        sb.size
                    } else {
                        sb.blocks512 as i64
                    };
                }
                n.num_files += 1;
            }
        }
        true
    }

    /// An entry (7513-7539): `sb` is None for a missing file (the original's zeroed stat).
    /// Sets `blocks` for a non-directory; false to stop (interrupt), the entry not added.
    fn entry(
        &mut self,
        dir: &Dir,
        path: &[u8],
        name: &[u8],
        sb: Option<&FileInfo>,
        idx: usize,
        blocks: &mut u64,
    ) -> bool {
        let n = &mut *self.nnn;
        // Use resolved (dev,ino) for duplicate check so symlink-to-dir and real dir count once
        let resolved = match sb {
            Some(s) if is_lnk(s.mode) => dir.stat_at(name, true).ok(),
            Some(s) => Some(*s),
            None => None,
        };
        match resolved.filter(|s| is_dir(s.mode)) {
            Some(sd) => {
                let (buf, _) = mkpath(path, name, &n.home);
                let first = test_set_bit_dir(&mut n.du.dir_dispatched_bmp, sd.dev, sd.ino);
                n.dirwalk(&buf, idx as i32, self.path_dev != sd.dev, !first);
                if n.du_interrupted() {
                    return false;
                }
                n.num_files += 1;
            }
            None => {
                let (size, nblk, mode, nlink, ino) = sb.map_or((0, 0, 0, 0, 0), |s| {
                    (s.size, s.blocks512 as i64, s.mode, s.nlink, s.ino)
                });
                let b = if n.cfg.sort.apparentsz { size } else { nblk };
                // dir_blocks adds the stored (40-bit) value, as the original does.
                let b = (b as u64 & BLOCKS_MASK) as i64;
                *blocks = b as u64;
                // Do not recount hard links
                if size != 0 && is_reg(mode) && (nlink <= 1 || n.ihash_set(ino)) {
                    n.dir_blocks += b;
                }
                n.num_files += 1;
            }
        }
        true
    }

    /// dentfill's `exit:` when `cfg.blkorder` is set (7560-7572): wait for the workers,
    /// add their totals and the directories' sizes.
    fn finish(&mut self, ents: &mut [Entry]) {
        let n = &mut *self.nnn;
        if !n.g_state.duinit {
            return;
        }
        let Some(sh) = n.du.shared.clone() else {
            return;
        };
        {
            let mut q = lock(&sh.queue);
            while q.pending != 0 {
                q = sh.du_cond.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        }
        if n.curses.is_some() {
            n.attroff_curctx();
        }
        let mut c = lock(&sh.counts);
        for i in 0..n.du.num_du_threads {
            n.num_files += c.core_files.get(i).copied().unwrap_or(0);
            n.dir_blocks += c.core_blocks.get(i).copied().unwrap_or(0);
        }
        for (i, b) in c.entblocks.drain(..) {
            if let Some(e) = ents.get_mut(i) {
                e.blocks = b as u64 & BLOCKS_MASK;
            }
        }
    }
}

impl Nnn {
    /// `test_set_bit` on `ihashbmp` from the main thread.
    fn ihash_set(&self, ino: u64) -> bool {
        self.du
            .shared
            .as_ref()
            .is_some_and(|sh| test_set_bit(&sh.ihashbmp, ino as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits() {
        let bmp: Vec<AtomicU64> = (0..=HASH_OCTETS).map(|_| AtomicU64::new(0)).collect();
        assert!(test_set_bit(&bmp, 5));
        assert!(!test_set_bit(&bmp, 5));
        assert!(!test_set_bit(&bmp, 5 | 0x100_0000));
        assert!(test_set_bit(&bmp, HASH_BITS));
        let mut d = vec![0u64; HASH_OCTETS + 1];
        assert!(test_set_bit_dir(&mut d, 1, 2));
        assert!(!test_set_bit_dir(&mut d, 2, 1));
        assert!(test_set_bit_dir(&mut d, 0, 0xFF_FFFF));
    }

    #[test]
    fn du_worker_stops_immediately_on_interrupt() {
        let dir = std::env::temp_dir().join(format!(
            "nnn_du_interrupt_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), b"hello world").unwrap();

        let sh = Shared {
            queue: Mutex::new(Queue::default()),
            work_cond: Condvar::new(),
            du_cond: Condvar::new(),
            counts: Mutex::new(Counts::default()),
            ihashbmp: (0..=HASH_OCTETS).map(|_| AtomicU64::new(0)).collect(),
            interrupt: Arc::new(AtomicBool::new(true)),
            apparentsz: AtomicBool::new(true),
            home: Vec::new(),
        };
        let group = Arc::new(Mutex::new(DuGroup::default()));
        let mut tfiles = 0u64;
        let mut tblocks = 0i64;
        let path_bytes = dir.to_string_lossy().into_owned().into_bytes();
        du_walk_dir(&sh, &path_bytes, &group, true, &mut tfiles, &mut tblocks);

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(tfiles, 0);
        assert_eq!(tblocks, 0);
    }

    #[test]
    fn du_state_default_does_not_allocate_bitmaps() {
        let mut du = DuState::default();
        assert!(du.shared.is_none());
        assert!(du.dir_dispatched_bmp.is_empty());
        assert_eq!(du.dir_dispatched_bmp.capacity(), 0);

        du.shared = Some(Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            work_cond: Condvar::new(),
            du_cond: Condvar::new(),
            counts: Mutex::new(Counts::default()),
            ihashbmp: (0..=HASH_OCTETS).map(|_| AtomicU64::new(0)).collect(),
            interrupt: Arc::new(AtomicBool::new(false)),
            apparentsz: AtomicBool::new(false),
            home: Vec::new(),
        }));
        du.dir_dispatched_bmp = vec![0; HASH_OCTETS + 1];
        assert!(du.shared.is_some());
        assert_eq!(du.dir_dispatched_bmp.len(), HASH_OCTETS + 1);

        du.release();
        assert!(du.shared.is_none());
        assert!(du.dir_dispatched_bmp.is_empty());
        assert_eq!(du.dir_dispatched_bmp.capacity(), 0);
    }
}
