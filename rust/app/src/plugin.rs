//! Plugins, the command prompt, the shell and the app launcher: setexports,
//! run_cmd_as_plugin, plctrl_init, read_nointr, readpipe, run_plugin, launch_app,
//! prompt_run and handle_cmd of src/nnn.c.
//!
//! run_plugin() forks in the original; the child holds the write end of the plugin FIFO open
//! while it runs the plugin, the parent reads the FIFO. Here a thread plays the child (it
//! owns the write end and runs the plugin), so the main thread reads the FIFO exactly as the
//! original's parent does.

use crate::browse::MESSAGES;
use crate::config::{CMD_LEN_MAX, cstr, mkpath, xbasename, xdirname};
use crate::draw::strsncpy;
use crate::keys::Action;
use crate::locale::setenv;
use crate::popup::{F_CLI, F_CONFIRM, F_MULTI, F_NORMAL, F_NOTRACE, F_NOWAIT, F_PAGE, F_WINDOW};
use crate::prompt::{INVALID_POS, PROMPT};
use crate::state::{CTX_MAX, NAME_MAX, Nnn, PATH_MAX};
use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::Command;

/// `messages[MSG_ENTER]`
const MSG_ENTER: usize = 22;
/// `messages[MSG_APP_NAME]`
const MSG_APP_NAME: usize = 32;
/// `utils[UTIL_LAUNCH]`
const UTIL_LAUNCH: &[u8] = b"launch";
/// `utils[UTIL_SH_EXEC]`
const UTIL_SH_EXEC: &[u8] = b"sh -c";
/// `utils[UTIL_FZF]`
const UTIL_FZF: &[u8] = b"fzf";
/// `envs[ENV_NCUR]`
const ENV_NCUR: &str = "nnn";
/// `env_cfg[NNN_PIPE]`
const NNN_PIPE: &str = "NNN_PIPE";
/// `env_cfg[NNNLVL]`
const NNNLVL: &str = "NNNLVL";
/// `TMP_LEN_MAX`
const TMP_LEN_MAX: usize = 64;

fn os(b: &[u8]) -> Cow<'_, OsStr> {
    crate::platform::os(cstr(b))
}

/// `xitoa` of an unsigned value.
fn xitoa(v: u32) -> Vec<u8> {
    v.to_string().into_bytes()
}

/// glibc `atoi`: leading blanks, an optional sign, then digits (wrapping as the C int does).
fn atoi(s: &[u8]) -> i32 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut v: i32 = 0;
    while let Some(d) = s.get(i).filter(|c| c.is_ascii_digit()) {
        v = v.wrapping_mul(10).wrapping_add(i32::from(d - b'0'));
        i += 1;
    }
    if neg { v.wrapping_neg() } else { v }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The command line of `run_cmd_as_plugin`: `file` cut to `PATH_MAX - 1` bytes; a trailing
/// '*' (after at least one byte) is dropped and turns off `F_CONFIRM`.
fn cmd_as_plugin_line(file: &[u8], mut flags: u16) -> (Vec<u8>, u16) {
    let mut g_buf = strsncpy(file, PATH_MAX);
    let len = g_buf.len();
    if len > 1 && g_buf[len - 1] == b'*' {
        flags &= !F_CONFIRM; // Skip user confirmation
        g_buf.truncate(len - 1); // Get rid of trailing no confirmation symbol
    }
    (g_buf, flags)
}

/// `g_pipepath` as `plctrl_init` builds it: "<tmp dir>/nnn-pipe.<pid>" in a
/// `TMP_LEN_MAX` buffer (the pid is cut so the path stays under 63 bytes).
fn pipepath(tmpdir: &[u8], pid: u32) -> Vec<u8> {
    let mut out = strsncpy(tmpdir, TMP_LEN_MAX);
    let l = out.len();
    out.push(b'/');
    out.extend_from_slice(b"nnn-pipe.");
    let pid = pid.to_string().into_bytes();
    let room = 52usize.saturating_sub(l);
    out.extend_from_slice(&pid[..pid.len().min(room)]);
    out.truncate(TMP_LEN_MAX - 2);
    out
}

/// `g_pipepath` of this process (for `cleanup()`).
pub fn pipepath_for(tmpdir: &[u8]) -> Vec<u8> {
    pipepath(tmpdir, std::process::id())
}

/// `read_nointr`: read(2), retried on EINTR; -1 on error.
fn read_nointr(fd: BorrowedFd<'_>, buf: &mut [u8]) -> isize {
    loop {
        match rustix::io::read(fd, &mut *buf) {
            Ok(n) => return n as isize,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return -1,
        }
    }
}

/// What the forked child of run_plugin() runs: a plugin or a command through `sh -c`.
struct ChildJob {
    argv: Vec<Vec<u8>>,
    confirm: bool,
}

impl ChildJob {
    /// The child of run_plugin(): holds `wfd` open while `spawn` runs the job (flag 0 or
    /// `F_MULTI` [| `F_CONFIRM`]: no curses), then closes it.
    fn run(self, wfd: OwnedFd) {
        let mut cmd = Command::new(os(&self.argv[0]));
        cmd.args(self.argv[1..].iter().map(|a| os(a)));
        let _ = cmd.status();
        if self.confirm {
            let mut out = std::io::stdout();
            let _ = out.write_all(MESSAGES[MSG_ENTER]);
            let _ = out.flush();
            let mut c = [0u8; 1];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut c), Ok(n) if n > 0) && c[0] != b'\n' {}
        }
        drop(wfd);
    }
}

impl Nnn {
    /// `setexports`: the hovered file ($nnn), each active context's dir and file ($d1..$d8,
    /// $f1..$f8), NNN_INCLUDE_HIDDEN, NNN_PREFER_SELECTION and PWD for the children.
    pub fn setexports(&mut self, path: &[u8]) {
        let cc = self.cfg.curctx;
        if self.ndents > 0 {
            let name = cstr(&self.pdents[self.cur as usize].name).to_vec();
            setenv(ENV_NCUR, &name);
            self.g_ctx[cc].c_name = strsncpy(&name, NAME_MAX + 1);
        } else if !cstr(&self.g_ctx[cc].c_name).is_empty() {
            self.g_ctx[cc].c_name.clear();
        }

        for (i, c) in self.g_ctx.iter().enumerate() {
            if c.c_cfg.ctxactive {
                setenv(&format!("d{}", i + 1), cstr(&c.c_path));
                if !cstr(&c.c_name).is_empty() {
                    let (p, _) = mkpath(&c.c_path, &c.c_name, &self.home);
                    setenv(&format!("f{}", i + 1), &p);
                }
            }
        }
        setenv(
            "NNN_INCLUDE_HIDDEN",
            &xitoa(u32::from(self.cfg.sort.showhidden)),
        );
        setenv(
            "NNN_PREFER_SELECTION",
            &xitoa(u32::from(self.cfg.prefersel)),
        );
        setenv("PWD", cstr(path));
    }

    /// `run_cmd_as_plugin`: `file` through `sh -c`, its output paged (`F_PAGE`), in a
    /// floating window (`F_WINDOW`) or on the terminal.
    pub fn run_cmd_as_plugin(
        &mut self,
        file: &[u8],
        flags: u16,
        action: Option<&mut Option<Action>>,
    ) {
        let (g_buf, flags) = cmd_as_plugin_line(file, flags);

        if flags & F_PAGE != 0 {
            self.get_output(UTIL_SH_EXEC, Some(&g_buf), None, None, true);
        } else if flags & F_WINDOW != 0 {
            if let Some(content) = self.buffer_command_output(&[UTIL_SH_EXEC], Some(&g_buf), None) {
                self.show_content_in_floating_window(
                    &content,
                    action,
                    find(&g_buf, b"$nnn").is_some(),
                );
            }
        } else {
            self.spawn(UTIL_SH_EXEC, Some(&g_buf), None, None, flags);
        }
    }

    /// `g_pipepath`: the plugin control FIFO.
    fn g_pipepath(&self) -> Vec<u8> {
        pipepath(&self.g_tmpfpath, std::process::id())
    }

    /// `plctrl_init`: exports NNN_PIPE, the plugin control FIFO's path.
    pub fn plctrl_init(&mut self) {
        let p = self.g_pipepath();
        setenv(NNN_PIPE, &p);
    }

    /// `readpipe`: a plugin's request on the FIFO: "[-]<ctx><op>[data]" (ctx '0'-'8' or
    /// '+', op 'c'd, 'o'pen, 'l'ist or 'p'ick). Returns the path to visit; sets `ctxnum` and,
    /// for 'o', `openfile`.
    pub fn readpipe(
        &mut self,
        fd: BorrowedFd<'_>,
        ctxnum: &mut i32,
        openfile: &mut Vec<u8>,
    ) -> Option<Vec<u8>> {
        let mut nextpath = None;
        let mut b = [0u8; 1];

        if read_nointr(fd, &mut b) != 1 {
            return None;
        }

        if b[0] == b'-' {
            // Clear selection on '-'
            self.clearselection();
            if read_nointr(fd, &mut b) != 1 {
                return None;
            }
        }

        let ctx: i32 = if b[0] == b'+' {
            self.get_free_ctx() as i32 + 1
        } else if b[0] < b'0' {
            return None;
        } else {
            let c = i32::from(b[0] - b'0');
            if c > CTX_MAX as i32 {
                return None;
            }
            c
        };

        if read_nointr(fd, &mut b) != 1 {
            return None;
        }

        let op = b[0];

        if op == b'c' || op == b'o' {
            let mut g_buf = vec![0u8; PATH_MAX];
            let len = read_nointr(fd, &mut g_buf);
            if len <= 0 {
                return None;
            }
            g_buf.truncate(len as usize); // Terminate the path read
            let p = cstr(&g_buf).to_vec();

            if op == b'o' && p.first() == Some(&b'/') {
                let base = xbasename(&p);
                if base.is_empty() {
                    return None;
                }
                *openfile = strsncpy(base, NAME_MAX + 1);
                nextpath = Some(xdirname(&p));
            } else if op == b'c' && p.first() == Some(&b'/') {
                let mut p = p;
                // Trim all trailing '/'
                while p.len() > 1 && p.last() == Some(&b'/') {
                    p.pop();
                }
                nextpath = Some(p);
            }
        } else if op == b'l' {
            self.rmlistpath(); // Remove last list mode path, if any
            let path = self.g_ctx[self.cfg.curctx].c_path.clone();
            nextpath = self.load_input(fd, Some(&path));
        } else if op == b'p' {
            self.selpath = None;
            self.clearselection();
            self.g_state.picker = false;
            self.g_state.picked = true;
        }

        *ctxnum = ctx;

        nextpath
    }

    /// `run_plugin`: runs plugin `file` (or a "!cmd" command; "!|" pages its output, "!>"
    /// shows it in a floating window, "!&" runs it detached) with `runfile` (the hovered
    /// file), the current dir and, as a picker, the selection file; then acts on what it
    /// wrote to NNN_PIPE (`openfile` gets an 'o' request's file). path, lastdir and lastname
    /// are the current context's. False when the command is empty or the FIFO fails.
    pub fn run_plugin(
        &mut self,
        file: &[u8],
        runfile: Option<&[u8]>,
        openfile: &mut Vec<u8>,
        action: Option<&mut Option<Action>>,
    ) -> bool {
        self.run_plugin_impl(file, runfile, openfile, action, false)
    }

    /// `run_plugin(..., runfile, runfile, ...)`: the caller passes one buffer as both
    /// `runfile` and `openfile`, so clearing `openfile` empties `runfile` too.
    pub fn run_plugin_aliased(
        &mut self,
        file: &[u8],
        runfile: Option<&[u8]>,
        openfile: &mut Vec<u8>,
        action: Option<&mut Option<Action>>,
    ) -> bool {
        self.run_plugin_impl(file, runfile, openfile, action, true)
    }

    fn run_plugin_impl(
        &mut self,
        file: &[u8],
        runfile: Option<&[u8]>,
        openfile: &mut Vec<u8>,
        action: Option<&mut Option<Action>>,
        aliased: bool,
    ) -> bool {
        let mut ctx: i32 = 0;
        let mut flags: u16 = 0;
        let mut cmd_as_plugin = false;
        let mut file = cstr(file);

        if !self.g_state.pluginit {
            self.plctrl_init();
            self.g_state.pluginit = true;
        }

        let path = self.g_ctx[self.cfg.curctx].c_path.clone();
        self.setexports(&path);

        // Check for run-cmd-as-plugin mode
        if file.first() == Some(&b'!') {
            flags = F_MULTI | F_CONFIRM;
            file = &file[1..];

            match file.first() {
                // Check if output should be paged
                Some(b'|') => {
                    flags |= F_PAGE;
                    file = &file[1..];
                }
                // Check if floating window should be used
                Some(b'>') => {
                    flags |= F_WINDOW;
                    file = &file[1..];
                }
                // Check if GUI flags are to be used
                Some(b'&') => {
                    flags = F_MULTI | F_NOTRACE | F_NOWAIT;
                    file = &file[1..];
                }
                _ => {}
            }

            if file.is_empty() {
                return false;
            }

            if flags & (F_NOTRACE | F_PAGE | F_WINDOW) != 0 {
                self.run_cmd_as_plugin(file, flags, action);
                return true;
            }

            cmd_as_plugin = true;
        }

        let pipe = self.g_pipepath();
        if rustix::fs::mknodat(
            rustix::fs::CWD,
            os(&pipe),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .is_err()
        {
            return false;
        }

        // The child's job, built here as the forked child would.
        let job = if cmd_as_plugin {
            let (g_buf, flags) = cmd_as_plugin_line(file, flags);
            ChildJob {
                argv: vec![b"sh".to_vec(), b"-c".to_vec(), g_buf],
                confirm: flags & F_CONFIRM != 0,
            }
        } else {
            // Generate absolute path to plugin
            let (plugin, _) = mkpath(&self.plgpath, file, &self.home);
            let mut argv = vec![plugin];
            if let Some(rf) = runfile.filter(|r| !cstr(r).is_empty()) {
                argv.push(strsncpy(rf, NAME_MAX));
            }
            argv.push(path.clone());
            if self.g_state.picker {
                argv.push(self.selpath.clone().unwrap_or_else(|| b"-".to_vec()));
            }
            ChildJob {
                argv,
                confirm: false,
            }
        };

        self.exitcurses();

        // Both ends are opened here without blocking (the read end first, so opening the
        // write end succeeds); then they block as the original's do.
        use rustix::fs::{Mode, OFlags, fcntl_getfl, fcntl_setfl, open};
        let rfd = open(
            os(&pipe),
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok();
        let wfd = rfd.as_ref().and_then(|_| {
            open(
                os(&pipe),
                OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .ok()
        });
        for fd in rfd.iter().chain(wfd.iter()) {
            if let Ok(fl) = fcntl_getfl(fd) {
                let _ = fcntl_setfl(fd, fl - OFlags::NONBLOCK);
            }
        }

        // In child (a thread here); when its open fails it exits at once.
        let child = wfd.map(|w| std::thread::spawn(move || job.run(w)));

        openfile.clear();
        let nextpath = match &rfd {
            Some(fd) => self.readpipe(fd.as_fd(), &mut ctx, openfile),
            None => None,
        };
        if let Some(np) = nextpath {
            let of = openfile.clone();
            let f = if !of.is_empty() {
                Some(&of[..])
            } else if aliased {
                Some(&b""[..])
            } else {
                runfile
            };
            self.set_smart_ctx(ctx, &np, f);
            if !openfile.is_empty() {
                let cc = self.cfg.curctx;
                self.g_ctx[cc].c_name = strsncpy(openfile, NAME_MAX + 1);
            }
        }

        drop(rfd);

        if !openfile.is_empty() {
            self.cs().ungetch(i32::from(b'\r'));
        }

        // wait for the child to finish. no zombies allowed
        if let Some(c) = child {
            let _ = c.join();
        }
        if let Some(cs) = self.curses.as_mut() {
            cs.resume();
            cs.refresh();
        }

        crate::sys::unlink(&pipe);

        true
    }

    /// `launch_app`: the launch plugin when fzf is there and it is executable, else an
    /// "app: " prompt for a program to start detached. `newpath` gets the plugin's path.
    pub fn launch_app(&mut self, newpath: &mut Vec<u8>) -> bool {
        let mut r = F_NORMAL;
        *newpath = mkpath(&self.plgpath, UTIL_LAUNCH, &self.home).0;
        let mut tmp = newpath.clone();

        if !self.getutil(UTIL_FZF)
            || rustix::fs::access(os(newpath), rustix::fs::Access::EXEC_OK).is_err()
        {
            tmp = self.xreadline(None, MESSAGES[MSG_APP_NAME]);
            r = F_NOWAIT | F_NOTRACE | F_MULTI;
        }

        if !cstr(&tmp).is_empty() {
            let arg: Option<&[u8]> = if r == F_NORMAL { Some(b"0") } else { None };
            self.spawn(&tmp, arg, None, None, r);
        }

        false
    }

    /// `prompt_run`: the ">>> " prompt, each command run by $SHELL until an empty one;
    /// %j runs it once per selected file, a final %J once with all (through xargs).
    /// True if at least one command was run.
    pub fn prompt_run(&mut self) -> bool {
        let mut ret = false;

        loop {
            self.prompt.selcmdpos = INVALID_POS;
            let mut cmdline = cstr(&self.xreadline(None, PROMPT)).to_vec();

            // Check for an empty command
            if cmdline.is_empty() {
                break;
            }

            self.prompt.addcmdtohist(&cmdline);
            ret = true;
            let len = cmdline.len();

            let mut cnt_j = 0;
            let mut next = 0;
            while let Some(p) = find(&cmdline[next..], b"%j") {
                cnt_j += 1;
                // replace %j with {} for xargs later
                let at = next + p;
                cmdline[at] = b'{';
                cmdline[at + 1] = b'}';
                next = at + 1;
            }

            let mut cnt_upper_j = 0;
            next = 0;
            while next <= cmdline.len() {
                let Some(p) = find(&cmdline[next..], b"%J") else {
                    break;
                };
                cnt_upper_j += 1;
                let at = next + p;
                // %J should be the last thing in the command
                if at == len - 2 {
                    cmdline.truncate(len - 2);
                }
                next = at + 1;
            }

            // We can't handle both %j and %J in a single command
            if cnt_j != 0 && cnt_upper_j != 0 {
                break;
            }

            // snprintf's "%s" of a NULL selpath
            let sel = self.selpath.clone().unwrap_or_else(|| b"(null)".to_vec());
            let mut cmd = Vec::new();
            if cnt_j != 0 {
                cmd.extend_from_slice(b"xargs -0 -I{} ");
            } else if cnt_upper_j != 0 {
                cmd.extend_from_slice(b"xargs -0 ");
            }
            let xargs = !cmd.is_empty();
            if xargs {
                cmd.extend_from_slice(&cmdline);
                cmd.extend_from_slice(b" < '");
                cmd.extend_from_slice(cstr(&sel));
                cmd.push(b'\'');
                cmd.truncate(CMD_LEN_MAX + 32 - 1);
            }

            let shell = self.shell.clone();
            let arg = if xargs { &cmd } else { &cmdline };
            let cmd_ret = self.spawn(&shell, Some(b"-c"), Some(arg), None, F_CLI | F_CONFIRM);
            if xargs && cmd_ret == 0 {
                self.clearselection();
            }
        }

        ret
    }

    /// `handle_cmd`: SEL_LAUNCH, SEL_PROMPT and SEL_SHELL (a $SHELL with NNNLVL one up).
    /// True when the dir should be reloaded.
    pub fn handle_cmd(&mut self, sel: Action, path: &[u8], newpath: &mut Vec<u8>) -> bool {
        self.endselection(false);

        if sel == Action::Launch {
            return self.launch_app(newpath);
        }

        self.setexports(path);

        if sel == Action::Prompt {
            return self.prompt_run();
        }

        // Set nnn nesting level
        let r = std::env::var_os(NNNLVL).map_or(0, |v| atoi(&crate::platform::bytes(&v)));

        setenv(NNNLVL, &xitoa(r.wrapping_add(1) as u32));
        let shell = self.shell.clone();
        self.spawn(&shell, None, None, None, F_CLI);
        setenv(NNNLVL, &xitoa(r as u32));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_indexes() {
        assert_eq!(MESSAGES[MSG_ENTER], b"\n'Enter' to continue");
        assert_eq!(MESSAGES[MSG_APP_NAME], b"app: ");
    }

    #[test]
    fn atoi_like_glibc() {
        assert_eq!(atoi(b"3"), 3);
        assert_eq!(atoi(b"  -2x"), -2);
        assert_eq!(atoi(b"+7"), 7);
        assert_eq!(atoi(b"abc"), 0);
        assert_eq!(atoi(b""), 0);
    }

    #[test]
    fn pipe_path() {
        assert_eq!(pipepath(b"/tmp", 1234), b"/tmp/nnn-pipe.1234".to_vec());
        let long = vec![b'a'; 50];
        let p = pipepath(&long, 123_456);
        assert_eq!(p.len(), 62);
        assert!(p.ends_with(b"nnn-pipe.12"));
    }

    #[test]
    fn cmd_line_star() {
        assert_eq!(
            cmd_as_plugin_line(b"touch x*", F_MULTI | F_CONFIRM),
            (b"touch x".to_vec(), F_MULTI)
        );
        assert_eq!(
            cmd_as_plugin_line(b"*", F_MULTI | F_CONFIRM),
            (b"*".to_vec(), F_MULTI | F_CONFIRM)
        );
    }

    #[test]
    fn readpipe_ops() {
        let mut n = Nnn::default();
        n.g_ctx[0].c_path = b"/home".to_vec();
        let mut run = |data: &[u8]| {
            let (r, mut w) = std::io::pipe().expect("pipe");
            w.write_all(data).expect("write");
            drop(w);
            let mut ctx = -1;
            let mut of = Vec::new();
            let np = n.readpipe(r.as_fd(), &mut ctx, &mut of);
            (np, ctx, of)
        };
        assert_eq!(
            run(b"0c/usr/lib//"),
            (Some(b"/usr/lib".to_vec()), 0, vec![])
        );
        assert_eq!(run(b"3c/"), (Some(b"/".to_vec()), 3, vec![]));
        assert_eq!(run(b"0crel"), (None, 0, vec![]));
        assert_eq!(
            run(b"0o/usr/bin/ls"),
            (Some(b"/usr/bin".to_vec()), 0, b"ls".to_vec())
        );
        assert_eq!(run(b"9c/x"), (None, -1, vec![]));
        assert_eq!(run(b"0x"), (None, 0, vec![]));
        assert_eq!(run(b""), (None, -1, vec![]));
        assert_eq!(run(b"+c/a").1, 2);
    }
}
