//! Built-in Windows drive picker (ADR-030): OS-neutral formatting, key handling, and
//! floating-window UI, compiled on every OS so its tests run on Linux too.

use crate::curses::{
    A_BOLD, A_REVERSE, ERR, Input, KEY_DOWN, KEY_END, KEY_ENTER, KEY_HOME, KEY_LEFT, KEY_NPAGE,
    KEY_PPAGE, KEY_RESIZE, KEY_RIGHT, KEY_UP, Window,
};
use crate::fmt;
use crate::platform::{self, DriveInfo, DriveKind};
use crate::popup::{newwin_fits, win_fits_min};
use crate::prompt::ESC;
use crate::state::Nnn;

/// Outcome of handling one key in the drive picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    Stay,
    Cancel,
    Pick(Vec<u8>),
}

/// Interactive state of the floating drive picker.
#[derive(Debug, Clone)]
pub struct DrivePicker {
    pub drives: Vec<DriveInfo>,
    pub cur: usize,
    pub scroll: usize,
    pub cur_letter: Option<u8>,
}

impl DrivePicker {
    /// Builds picker state, pre-selecting the drive matching `cur_path` (if any).
    pub fn new(drives: Vec<DriveInfo>, cur_path: &[u8]) -> Self {
        let cur_letter = current_drive_letter(cur_path);
        let cur = cur_letter
            .and_then(|l| drives.iter().position(|d| d.letter == l))
            .unwrap_or(0);
        Self {
            drives,
            cur,
            scroll: 0,
            cur_letter,
        }
    }

    fn clamp_scroll(&mut self, max_rows: usize) {
        if max_rows == 0 {
            self.scroll = 0;
            return;
        }
        if self.cur < self.scroll {
            self.scroll = self.cur;
        } else if self.cur >= self.scroll + max_rows {
            self.scroll = self.cur + 1 - max_rows;
        }
    }

    /// Processes a single curses key code.
    pub fn handle_key(&mut self, ch: i32, max_rows: usize) -> PickerOutcome {
        if self.drives.is_empty() {
            return PickerOutcome::Cancel;
        }
        let last = self.drives.len() - 1;
        let step = max_rows.max(1);
        match ch {
            c if c == i32::from(b'q')
                || c == ESC
                || c == i32::from(b'h')
                || c == KEY_LEFT
                || c == KEY_RESIZE =>
            {
                return PickerOutcome::Cancel;
            }
            c if c == i32::from(b'\r')
                || c == i32::from(b'\n')
                || c == KEY_ENTER
                || c == i32::from(b'l')
                || c == KEY_RIGHT =>
            {
                return PickerOutcome::Pick(self.drives[self.cur].root.clone());
            }
            c if c == i32::from(b'j') || c == KEY_DOWN => {
                if self.cur < last {
                    self.cur += 1;
                }
            }
            c if c == i32::from(b'k') || c == KEY_UP => {
                self.cur = self.cur.saturating_sub(1);
            }
            c if c == i32::from(b'g') || c == KEY_HOME => {
                self.cur = 0;
            }
            KEY_END => {
                self.cur = last;
            }
            KEY_PPAGE => {
                self.cur = self.cur.saturating_sub(step);
            }
            KEY_NPAGE => {
                self.cur = (self.cur + step).min(last);
            }
            c if (i32::from(b'A')..=i32::from(b'Z')).contains(&c)
                || (i32::from(b'a')..=i32::from(b'z')).contains(&c) =>
            {
                let letter = (c as u8).to_ascii_uppercase();
                if let Some(d) = self.drives.iter().find(|d| d.letter == letter) {
                    return PickerOutcome::Pick(d.root.clone());
                }
            }
            _ => {}
        }
        self.clamp_scroll(max_rows);
        PickerOutcome::Stay
    }
}

/// Extracts the uppercase ASCII drive letter from a Windows path like `C:/...`.
pub fn current_drive_letter(path: &[u8]) -> Option<u8> {
    if path.len() >= 2 && path[0].is_ascii_alphabetic() && path[1] == b':' {
        Some(path[0].to_ascii_uppercase())
    } else {
        None
    }
}

fn drive_kind_tag(kind: DriveKind) -> &'static str {
    match kind {
        DriveKind::Fixed => "[Local]",
        DriveKind::Removable => "[USB]",
        DriveKind::Remote => "[Network]",
        DriveKind::CdRom => "[CD-ROM]",
        DriveKind::RamDisk => "[RAM]",
        DriveKind::Unknown => "",
    }
}

/// Formats one drive row for the floating window.
pub fn format_drive_row(
    d: &DriveInfo,
    cur_letter: Option<u8>,
    label_w: usize,
    fs_w: usize,
) -> Vec<u8> {
    let mark = if cur_letter == Some(d.letter) {
        '*'
    } else {
        ' '
    };
    let tag = drive_kind_tag(d.kind);
    let mut out = format!("{mark} {}  {tag:<9}", String::from_utf8_lossy(&d.root)).into_bytes();

    if label_w > 0 {
        out.extend_from_slice(b"  ");
        let lbl = String::from_utf8_lossy(&d.label);
        let mut shown = String::new();
        for ch in lbl.chars().take(label_w) {
            shown.push(ch);
        }
        let pad = label_w.saturating_sub(shown.chars().count());
        out.extend_from_slice(shown.as_bytes());
        out.extend(std::iter::repeat_n(b' ', pad));
    }

    if fs_w > 0 {
        out.extend_from_slice(b"  ");
        let fs = String::from_utf8_lossy(&d.fs_type);
        out.extend_from_slice(format!("{fs:<fs_w$}").as_bytes());
    }

    if let Some((avail, total)) = d.space {
        out.extend_from_slice(
            format!(
                "  {} / {}",
                fmt::coolsize(avail as i64),
                fmt::coolsize(total as i64)
            )
            .as_bytes(),
        );
    }

    while out.last() == Some(&b' ') {
        out.pop();
    }
    out
}

/// Geometry `(height, width, y, x)` for the drive picker window, or `None` if the screen
/// is too small and should use the status-bar fallback.
pub fn picker_geometry(xlines: i32, xcols: i32, count: usize) -> Option<(i32, i32, i32, i32)> {
    if !win_fits_min(xlines, xcols, 7, 24) {
        return None;
    }
    let max_h = 20.min(xlines - 4).max(5);
    let h = (count as i32 + 2).clamp(4, max_h);
    let w = ((xcols * 3) / 4).clamp(24, xcols);
    let y = (xlines - h) / 2;
    let x = (xcols - w) / 2;
    newwin_fits(h, w, y, x).then_some((h, w, y, x))
}

/// Paints the drive picker into `win` (without calling `wrefresh`).
pub fn paint_drive_picker(win: &mut Window, picker: &DrivePicker) {
    let (h, w) = (win.lines, win.cols);
    let max_rows = (h - 2).max(0) as usize;
    let inner_w = (w - 2).max(0) as usize;

    win.erase();
    win.border();

    // Title
    let title = format!(" Drives ({}) ", picker.drives.len());
    if win.mv(0, 2) != ERR {
        win.attron(A_BOLD);
        win.addnstr(title.as_bytes(), (w - 4).max(0));
        win.attroff(A_BOLD);
    }

    let label_w = picker
        .drives
        .iter()
        .map(|d| String::from_utf8_lossy(&d.label).chars().count())
        .max()
        .unwrap_or(0)
        .min(18);
    let fs_w = picker
        .drives
        .iter()
        .map(|d| d.fs_type.len())
        .max()
        .unwrap_or(0)
        .min(8);

    for row in 0..max_rows {
        let idx = picker.scroll + row;
        let Some(d) = picker.drives.get(idx) else {
            break;
        };
        let mut line = format_drive_row(d, picker.cur_letter, label_w, fs_w);
        let selected = idx == picker.cur;
        if selected && line.len() < inner_w {
            line.resize(inner_w, b' ');
        }
        if win.mv(1 + row as i32, 1) != ERR {
            if selected {
                win.attron(A_REVERSE);
            }
            win.addnstr(&line, inner_w as i32);
            if selected {
                win.attroff(A_REVERSE);
            }
        }
    }

    // Scroll indicators
    if picker.drives.len() > max_rows && max_rows > 0 {
        if picker.scroll > 0 && win.mv(1, w - 2) != ERR {
            win.addch('^');
        }
        if picker.scroll + max_rows < picker.drives.len() && win.mv(h - 2, w - 2) != ERR {
            win.addch('v');
        }
    }

    // Footer hint
    let hint = b" j/k:move  Enter/A-Z:open  q/Esc:close ";
    if win.mv(h - 1, 1) != ERR {
        win.addnstr(hint, (w - 2).max(0));
    }
}

/// Headless driver for unit tests: renders `drives` on an `xlines` x `xcols` screen,
/// feeds `keys`, and returns `(final_rows, picked_root)`.
#[cfg(test)]
pub fn drive_picker_rows(
    xlines: i32,
    xcols: i32,
    drives: &[DriveInfo],
    cur_path: &[u8],
    keys: &[i32],
) -> (Option<Vec<String>>, Option<Vec<u8>>) {
    let Some((h, w, y, x)) = picker_geometry(xlines, xcols, drives.len()) else {
        return (None, None);
    };
    let mut win = Window::new(h, w, y, x);
    let mut picker = DrivePicker::new(drives.to_vec(), cur_path);
    let max_rows = (h - 2).max(0) as usize;
    picker.clamp_scroll(max_rows);
    paint_drive_picker(&mut win, &picker);

    let mut picked = None;
    for &ch in keys {
        match picker.handle_key(ch, max_rows) {
            PickerOutcome::Stay => paint_drive_picker(&mut win, &picker),
            PickerOutcome::Cancel => break,
            PickerOutcome::Pick(root) => {
                picked = Some(root);
                break;
            }
        }
    }
    let rows = (0..h).map(|r| win.row_text(r)).collect();
    (Some(rows), picked)
}

/// Compact status-bar prompt text for small terminals: `"drives: C D E > "`.
fn statusbar_prompt(drives: &[DriveInfo]) -> Vec<u8> {
    let mut prompt = b"drives:".to_vec();
    for d in drives {
        prompt.push(b' ');
        prompt.push(d.letter);
    }
    prompt.extend_from_slice(b" > ");
    prompt
}

impl Nnn {
    /// Opens the interactive drive picker and returns the chosen drive root (e.g. `b"D:/"`),
    /// or `None` if cancelled or no drives are available.
    pub fn pick_drive(&mut self, cur_path: &[u8]) -> Option<Vec<u8>> {
        let drives = platform::list_drives();
        if drives.is_empty() {
            self.printmsg(b"no drives");
            return None;
        }

        let Some((h, w, y, x)) = picker_geometry(self.xlines, self.xcols, drives.len()) else {
            let prompt = statusbar_prompt(&drives);
            let ch = self.get_input(Some(&prompt));
            if (i32::from(b'A')..=i32::from(b'Z')).contains(&ch)
                || (i32::from(b'a')..=i32::from(b'z')).contains(&ch)
            {
                let letter = (ch as u8).to_ascii_uppercase();
                return drives
                    .into_iter()
                    .find(|d| d.letter == letter)
                    .map(|d| d.root);
            }
            return None;
        };

        let mut win = self.cs().newwin(h, w, y, x);
        let mut picker = DrivePicker::new(drives, cur_path);
        let max_rows = (h - 2).max(0) as usize;
        picker.clamp_scroll(max_rows);

        let picked = loop {
            paint_drive_picker(&mut win, &picker);
            let cs = self.cs();
            cs.wrefresh(&win);

            let ch = match cs.get_wch() {
                None => continue,
                Some(Input::Char(c) | Input::Key(c)) => c,
            };

            match picker.handle_key(ch, max_rows) {
                PickerOutcome::Stay => {}
                PickerOutcome::Cancel => break None,
                PickerOutcome::Pick(root) => break Some(root),
            }
        };

        let cs = self.cs();
        cs.delwin(&win);
        cs.refresh();
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_drive(
        letter: u8,
        kind: DriveKind,
        label: &str,
        fs: &str,
        space: Option<(u64, u64)>,
    ) -> DriveInfo {
        DriveInfo {
            letter,
            root: vec![letter, b':', b'/'],
            kind,
            label: label.as_bytes().to_vec(),
            fs_type: fs.as_bytes().to_vec(),
            space,
        }
    }

    fn sample_drives() -> Vec<DriveInfo> {
        vec![
            mk_drive(
                b'C',
                DriveKind::Fixed,
                "Windows",
                "NTFS",
                Some((120 * 1024 * 1024 * 1024, 500 * 1024 * 1024 * 1024)),
            ),
            mk_drive(
                b'D',
                DriveKind::Fixed,
                "Data",
                "NTFS",
                Some((310 * 1024 * 1024 * 1024, 1024 * 1024 * 1024 * 1024)),
            ),
            mk_drive(b'H', DriveKind::Remote, "", "", None),
        ]
    }

    #[test]
    fn preselects_current_drive() {
        let p = DrivePicker::new(sample_drives(), b"D:/Projects/nnn");
        assert_eq!(p.cur, 1);
        assert_eq!(p.cur_letter, Some(b'D'));
    }

    #[test]
    fn nav_keys_move_and_select() {
        let drives = sample_drives();
        let (_, picked) = drive_picker_rows(
            24,
            80,
            &drives,
            b"C:/Users",
            &[i32::from(b'j'), KEY_DOWN, KEY_UP, i32::from(b'\r')],
        );
        assert_eq!(picked, Some(b"D:/".to_vec()));

        let (_, picked) = drive_picker_rows(24, 80, &drives, b"C:/", &[KEY_END, i32::from(b'l')]);
        assert_eq!(picked, Some(b"H:/".to_vec()));
    }

    #[test]
    fn direct_letter_jump_and_cancel() {
        let drives = sample_drives();
        // Lowercase 'd' jumps directly to D:/
        let (_, picked) = drive_picker_rows(24, 80, &drives, b"C:/", &[i32::from(b'd')]);
        assert_eq!(picked, Some(b"D:/".to_vec()));

        // Uppercase 'H' jumps to H:/, while lowercase 'h' cancels (vim left).
        let (_, picked) = drive_picker_rows(24, 80, &drives, b"C:/", &[i32::from(b'H')]);
        assert_eq!(picked, Some(b"H:/".to_vec()));

        let (_, picked) = drive_picker_rows(24, 80, &drives, b"C:/", &[i32::from(b'h')]);
        assert_eq!(picked, None);

        let (_, picked) = drive_picker_rows(24, 80, &drives, b"C:/", &[ESC]);
        assert_eq!(picked, None);

        // Unknown letter stays open; subsequent Enter picks current drive.
        let (_, picked) = drive_picker_rows(24, 80, &drives, b"C:/", &[i32::from(b'z'), KEY_ENTER]);
        assert_eq!(picked, Some(b"C:/".to_vec()));
    }

    #[test]
    fn formats_labels_and_missing_space() {
        let drives = sample_drives();
        let (rows, _) = drive_picker_rows(24, 80, &drives, b"C:/Users", &[]);
        let rows = rows.expect("window fits");
        assert!(rows[0].contains("Drives (3)"), "title: {:?}", rows[0]);
        assert!(rows[1].contains("* C:/"), "row 1: {:?}", rows[1]);
        assert!(rows[1].contains("[Local]"), "row 1: {:?}", rows[1]);
        assert!(rows[1].contains("Windows"), "row 1: {:?}", rows[1]);
        assert!(rows[1].contains("NTFS"), "row 1: {:?}", rows[1]);
        assert!(rows[3].contains("H:/"), "row 3: {:?}", rows[3]);
        assert!(rows[3].contains("[Network]"), "row 3: {:?}", rows[3]);
        assert_eq!(rows[3].matches('/').count(), 1);
    }

    #[test]
    fn scrolls_when_more_drives_than_rows() {
        let drives: Vec<DriveInfo> = (b'A'..=b'J')
            .map(|l| mk_drive(l, DriveKind::Fixed, "Disk", "NTFS", None))
            .collect();
        // 9-line terminal -> max_h = 5 -> 3 visible drive rows.
        let (rows, picked) = drive_picker_rows(9, 80, &drives, b"A:/", &[KEY_END, KEY_ENTER]);
        let rows = rows.expect("window fits");
        assert_eq!(picked, Some(b"J:/".to_vec()));
        assert!(rows.iter().any(|r| r.contains("J:/")), "{rows:?}");
    }

    #[test]
    fn tiny_terminal_returns_none_geometry_and_builds_statusbar_prompt() {
        let drives = sample_drives();
        assert_eq!(picker_geometry(6, 80, drives.len()), None);
        assert_eq!(picker_geometry(24, 20, drives.len()), None);
        assert_eq!(statusbar_prompt(&drives), b"drives: C D H > ");
    }
}
