//! nnn's global state (the GLOBALS section of src/nnn.c) as one struct. Functions of
//! nnn.c become methods of `Nnn` in the module that ports them; each module keeps its own
//! extra state in its own struct below, so modules can be ported independently.

use crate::curses::Curses;
use crate::opts::Opts;
use crate::sort::{Entry, SortCfg, TimeType};

pub const CTX_MAX: usize = 8;
pub const PATH_MAX: usize = 4096;
pub const NAME_MAX: usize = 255;
pub const REGEX_MAX: usize = 48;

/// `settings` (`cfg` and each context's `c_cfg`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub filtermode: bool,
    pub sort: SortCfg,
    pub preview: bool,
    pub ctxactive: bool,
    pub curctx: usize,
    pub prefersel: bool,
    pub fileinfo: bool,
    pub nonavopen: bool,
    pub autoenter: bool,
    pub useeditor: bool,
    pub fuzzy: bool,
    pub regex: bool,
    pub x11: bool,
    pub cliopener: bool,
    pub waitedit: bool,
    pub rollover: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            filtermode: false,
            sort: SortCfg {
                timetype: TimeType::Mod,
                ..SortCfg::default()
            },
            preview: false,
            ctxactive: true,
            curctx: 0,
            prefersel: false,
            fileinfo: false,
            nonavopen: false,
            autoenter: true,
            useeditor: false,
            fuzzy: false,
            regex: false,
            x11: false,
            cliopener: false,
            waitedit: false,
            rollover: true,
        }
    }
}

/// `runstate` (`g_state`).
#[derive(Debug, Clone, Default)]
pub struct RunState {
    pub autofifo: bool,
    pub autonext: bool,
    pub dircolor: bool,
    pub dirctx: bool,
    pub duinit: bool,
    pub fifomode: bool,
    pub forcequit: bool,
    pub initfile: bool,
    pub interrupt: bool,
    pub mv: bool,
    pub oldcolor: bool,
    pub picked: bool,
    pub picker: bool,
    pub pluginit: bool,
    pub prstssn: bool,
    pub rangesel: bool,
    pub runctx: usize,
    pub runplugin: bool,
    pub selbm: bool,
    pub selmode: bool,
    pub stayonsel: bool,
    pub uidgid: bool,
    pub usebsdtar: bool,
    pub xprompt: bool,
    pub showlines: bool,
    pub plugparsed: bool,
    pub lastctx: usize,
    pub closedctx: usize,
}

/// `context` (`g_ctx[]`).
#[derive(Debug, Clone)]
pub struct Context {
    pub c_path: Vec<u8>,
    pub c_last: Vec<u8>,
    pub c_name: Vec<u8>,
    /// `c_fltr`: [0] is the filter type ('/' string, '\\' regex... as in nnn.c), then the
    /// filter text; `fltr[REGEX_MAX - 1]` keeps the last filter's first char (clearfilter).
    pub c_fltr: Vec<u8>,
    pub c_cfg: Settings,
    pub color: u32,
}

impl Default for Context {
    /// `g_ctx` is zeroed static storage: every context starts inactive, with a zeroed `c_cfg`.
    fn default() -> Self {
        Context {
            c_path: Vec::new(),
            c_last: Vec::new(),
            c_name: Vec::new(),
            c_fltr: vec![0; REGEX_MAX],
            c_cfg: Settings {
                ctxactive: false,
                autoenter: false,
                rollover: false,
                sort: SortCfg {
                    timetype: TimeType::Access,
                    ..SortCfg::default()
                },
                ..Settings::default()
            },
            color: 0,
        }
    }
}

/// Key-value pairs from the environment (`kv`): key and value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kv {
    pub key: i32,
    pub val: Vec<u8>,
}

/// All of nnn's globals.
#[derive(Default)]
pub struct Nnn {
    pub cfg: Settings,
    pub g_state: RunState,
    pub g_ctx: [Context; CTX_MAX],
    pub opts: Opts,
    pub curses: Option<Curses>,

    pub pdents: Vec<Entry>,
    pub ndents: i32,
    pub cur: i32,
    pub last: i32,
    pub curscroll: i32,
    pub last_curscroll: i32,
    pub nselected: i32,
    pub gtimesecs: i64,
    pub idletimeout: i32,
    pub idle: i32,
    pub xlines: i32,
    pub xcols: i32,
    pub cfgsort: [u8; CTX_MAX + 1],
    pub blk_shift: u32,
    pub dir_blocks: i64,
    pub num_files: u64,
    pub scroll_lines: i32,
    pub middle_click_key: i32,

    pub opener: Vec<u8>,
    pub editor: Vec<u8>,
    pub enveditor: Vec<u8>,
    pub pager: Vec<u8>,
    pub shell: Vec<u8>,
    pub home: Vec<u8>,
    pub initpath: Vec<u8>,
    pub cfgpath: Vec<u8>,
    pub selpath: Option<Vec<u8>>,
    pub listpath: Option<Vec<u8>>,
    pub listroot: Option<Vec<u8>>,
    pub plgpath: Vec<u8>,
    pub mark: Option<Vec<u8>>,
    pub trashcmd: Option<Vec<u8>>,
    pub hostname: Vec<u8>,
    pub fifopath: Option<Vec<u8>>,
    pub g_tmpfpath: Vec<u8>,

    pub bookmark: Vec<Kv>,
    pub plug: Vec<Kv>,
    pub order: Vec<Kv>,
    pub fcolors: [u32; CTX_MAX + 1],

    /// `pselbuf`/`selbufpos`: NUL-separated selected paths.
    pub pselbuf: Vec<u8>,
    pub findselpos: Option<usize>,

    pub prompt: crate::prompt::PromptState,
    pub popup: crate::popup::PopupState,
    pub listmode: crate::listmode::ListState,
    pub core: crate::draw::CoreState,
    pub browse: crate::browse::BrowseState,
    pub du: crate::du::DuState,
}

impl Nnn {
    /// `stdscr` and the terminal; curses is started before any function that draws.
    pub fn cs(&mut self) -> &mut Curses {
        // INFALLIBLE: every caller runs after initcurses, as in the original.
        self.curses.as_mut().expect("curses initialised")
    }
}
