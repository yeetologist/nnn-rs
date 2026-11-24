//! Key bindings (`bindings[]` in src/nnn.h, default build) and `-K`.

/// A key as curses reports it: a byte, or a named key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(u8),
    Down,
    Up,
    Left,
    Right,
    Home,
    Npage,
    Ppage,
    Enter,
    Btab,
    End,
    Mouse,
}

impl Key {
    /// The curses key code (`wint_t`).
    pub fn code(self) -> u32 {
        match self {
            Key::Char(c) => u32::from(c),
            Key::Down => 0o402,
            Key::Up => 0o403,
            Key::Left => 0o404,
            Key::Right => 0o405,
            Key::Home => 0o406,
            Key::Npage => 0o522,
            Key::Ppage => 0o523,
            Key::Enter => 0o527,
            Key::Btab => 0o541,
            Key::End => 0o550,
            Key::Mouse => 0o631,
        }
    }

    /// ncurses `keyname()`.
    pub fn name(self) -> String {
        match self {
            Key::Char(c @ 0..=31) => format!("^{}", char::from(c + 64)),
            Key::Char(127) => "^?".to_string(),
            Key::Char(c) => char::from(c).to_string(),
            Key::Down => "KEY_DOWN".to_string(),
            Key::Up => "KEY_UP".to_string(),
            Key::Left => "KEY_LEFT".to_string(),
            Key::Right => "KEY_RIGHT".to_string(),
            Key::Home => "KEY_HOME".to_string(),
            Key::Npage => "KEY_NPAGE".to_string(),
            Key::Ppage => "KEY_PPAGE".to_string(),
            Key::Enter => "KEY_ENTER".to_string(),
            Key::Btab => "KEY_BTAB".to_string(),
            Key::End => "KEY_END".to_string(),
            Key::Mouse => "KEY_MOUSE".to_string(),
        }
    }
}

/// What a key does (`enum action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Back,
    Open,
    NavIn,
    Next,
    Prev,
    Pgdn,
    Pgup,
    CtrlD,
    CtrlU,
    Home,
    End,
    First,
    Jump,
    Young,
    Cdhome,
    Cdbegin,
    Cdlast,
    Cdroot,
    #[cfg(windows)]
    Drives,
    Bmopen,
    Remote,
    Cycle,
    Cycler,
    Ctx1,
    Ctx2,
    Ctx3,
    Ctx4,
    Ctx5,
    Ctx6,
    Ctx7,
    Ctx8,
    Mark,
    Bmark,
    Fltr,
    Mfltr,
    Hidden,
    Detail,
    Stats,
    Chmodx,
    Archive,
    Sort,
    Redraw,
    Sel,
    Selmul,
    Selall,
    Selinv,
    Seledit,
    Cp,
    Mv,
    Cpmvas,
    Trash,
    RmRf,
    Openwith,
    New,
    Rename,
    Renamemul,
    Umount,
    Help,
    Autonext,
    Edit,
    Plugin,
    Selsize,
    Shell,
    Launch,
    Prompt,
    Lock,
    Sessions,
    Export,
    Timetype,
    Preview,
    Reopenctx,
    Quitctx,
    Quitcd,
    Quit,
    Quiterr,
    Click,
}

pub const BINDINGS: &[(Key, Action)] = &[
    (Key::Left, Action::Back),
    (Key::Char(104), Action::Back),
    (Key::Enter, Action::Open),
    (Key::Char(13), Action::Open),
    (Key::Right, Action::NavIn),
    (Key::Char(108), Action::NavIn),
    (Key::Char(106), Action::Next),
    (Key::Down, Action::Next),
    (Key::Char(107), Action::Prev),
    (Key::Up, Action::Prev),
    (Key::Npage, Action::Pgdn),
    (Key::Ppage, Action::Pgup),
    (Key::Char(4), Action::CtrlD),
    (Key::Char(21), Action::CtrlU),
    (Key::Home, Action::Home),
    (Key::Char(103), Action::Home),
    (Key::Char(1), Action::Home),
    (Key::End, Action::End),
    (Key::Char(71), Action::End),
    (Key::Char(5), Action::End),
    (Key::Char(39), Action::First),
    (Key::Char(74), Action::Jump),
    (Key::Char(25), Action::Young),
    (Key::Char(126), Action::Cdhome),
    (Key::Char(64), Action::Cdbegin),
    (Key::Char(45), Action::Cdlast),
    (Key::Char(96), Action::Cdroot),
    #[cfg(windows)]
    (Key::Char(b'D'), Action::Drives),
    (Key::Char(98), Action::Bmopen),
    (Key::Char(31), Action::Bmopen),
    (Key::Char(99), Action::Remote),
    (Key::Char(9), Action::Cycle),
    (Key::Btab, Action::Cycler),
    (Key::Char(49), Action::Ctx1),
    (Key::Char(50), Action::Ctx2),
    (Key::Char(51), Action::Ctx3),
    (Key::Char(52), Action::Ctx4),
    (Key::Char(53), Action::Ctx5),
    (Key::Char(54), Action::Ctx6),
    (Key::Char(55), Action::Ctx7),
    (Key::Char(56), Action::Ctx8),
    (Key::Char(44), Action::Mark),
    (Key::Char(66), Action::Bmark),
    (Key::Char(47), Action::Fltr),
    (Key::Char(14), Action::Mfltr),
    (Key::Char(46), Action::Hidden),
    (Key::Char(100), Action::Detail),
    (Key::Char(102), Action::Stats),
    (Key::Char(6), Action::Stats),
    (Key::Char(42), Action::Chmodx),
    (Key::Char(122), Action::Archive),
    (Key::Char(116), Action::Sort),
    (Key::Char(20), Action::Sort),
    (Key::Char(12), Action::Redraw),
    (Key::Char(32), Action::Sel),
    (Key::Char(43), Action::Sel),
    (Key::Char(109), Action::Selmul),
    (Key::Char(97), Action::Selall),
    (Key::Char(65), Action::Selinv),
    (Key::Char(69), Action::Seledit),
    (Key::Char(112), Action::Cp),
    (Key::Char(16), Action::Cp),
    (Key::Char(118), Action::Mv),
    (Key::Char(22), Action::Mv),
    (Key::Char(119), Action::Cpmvas),
    (Key::Char(23), Action::Cpmvas),
    (Key::Char(120), Action::Trash),
    (Key::Char(24), Action::Trash),
    (Key::Char(88), Action::RmRf),
    (Key::Char(111), Action::Openwith),
    (Key::Char(15), Action::Openwith),
    (Key::Char(110), Action::New),
    (Key::Char(18), Action::Rename),
    (Key::Char(114), Action::Renamemul),
    (Key::Char(117), Action::Umount),
    (Key::Char(63), Action::Help),
    (Key::Char(10), Action::Autonext),
    (Key::Char(101), Action::Edit),
    (Key::Char(59), Action::Plugin),
    (Key::Char(83), Action::Selsize),
    (Key::Char(33), Action::Shell),
    (Key::Char(29), Action::Shell),
    (Key::Char(61), Action::Launch),
    (Key::Char(93), Action::Prompt),
    (Key::Char(48), Action::Lock),
    (Key::Char(115), Action::Sessions),
    (Key::Char(62), Action::Export),
    (Key::Char(84), Action::Timetype),
    (Key::Char(80), Action::Preview),
    (Key::Char(82), Action::Reopenctx),
    (Key::Char(113), Action::Quitctx),
    (Key::Char(7), Action::Quitcd),
    (Key::Char(17), Action::Quit),
    (Key::Char(81), Action::Quiterr),
    (Key::Mouse, Action::Click),
];

/// `check_key_collision`: report keys bound twice.
pub fn check_key_collision() {
    let mut seen = std::collections::HashSet::new();
    for (key, _) in BINDINGS {
        if !seen.insert(key.code()) {
            eprintln!("key collision! [{}]", key.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_collisions_and_lookup() {
        let mut seen = std::collections::HashSet::new();
        assert!(BINDINGS.iter().all(|(k, _)| seen.insert(k.code())));
        #[cfg(windows)]
        assert_eq!(BINDINGS.len(), 95);
        #[cfg(not(windows))]
        assert_eq!(BINDINGS.len(), 94);
        assert_eq!(Key::Char(7).name(), "^G");
    }
}
