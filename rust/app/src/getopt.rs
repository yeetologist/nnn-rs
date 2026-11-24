//! glibc `getopt(3)` as nnn calls it: no long options, `opterr` on, argument
//! permutation unless `POSIXLY_CORRECT` is set. Works on bytes, as C does.

use std::io::Write;

/// One result of `getopt`.
#[derive(Debug, PartialEq, Eq)]
pub enum Opt {
    /// An option from the option string, with its argument when it takes one.
    Flag(u8, Option<Vec<u8>>),
    /// glibc returned `'?'` and already printed its message.
    Error,
}

/// glibc's state across calls, for one parse of one argv.
pub struct Getopt<'a> {
    argv: &'a [Vec<u8>],
    optstring: &'a [u8],
    permute: bool,
    index: usize,
    next: usize,
    operands: Vec<usize>,
    after_dashdash: usize,
}

impl<'a> Getopt<'a> {
    pub fn new(argv: &'a [Vec<u8>], optstring: &'a [u8]) -> Self {
        Getopt {
            argv,
            optstring,
            permute: std::env::var_os("POSIXLY_CORRECT").is_none(),
            index: 1,
            next: 0,
            operands: Vec::new(),
            after_dashdash: argv.len(),
        }
    }

    fn spec(&self, c: u8) -> Option<bool> {
        if c == b':' || c == b';' {
            return None;
        }
        let at = self.optstring.iter().position(|&o| o == c)?;
        Some(self.optstring.get(at + 1) == Some(&b':'))
    }

    fn complain(&self, what: &str, c: u8) {
        let mut line = self.argv.first().cloned().unwrap_or_default();
        line.extend_from_slice(format!(": {what} -- '").as_bytes());
        line.push(c);
        line.extend_from_slice(b"'\n");
        let _ = std::io::stderr().write_all(&line);
    }

    /// The operands in the order glibc leaves them once parsing is done: words it skipped,
    /// then the words after `--`.
    pub fn operands(&self) -> Vec<&'a [u8]> {
        let mut out: Vec<&[u8]> = self
            .operands
            .iter()
            .map(|&i| self.argv[i].as_slice())
            .collect();
        out.extend(
            self.argv[self.after_dashdash.min(self.argv.len())..]
                .iter()
                .map(Vec::as_slice),
        );
        out
    }

    /// The next option, or `None` once every option has been read.
    pub fn next_opt(&mut self) -> Option<Opt> {
        if self.next == 0 {
            loop {
                let word = self.argv.get(self.index)?;
                if word.as_slice() == b"--" {
                    self.after_dashdash = self.index + 1;
                    self.index = self.argv.len();
                    return None;
                }
                if word.len() > 1 && word[0] == b'-' {
                    self.next = 1;
                    break;
                }
                if !self.permute {
                    self.after_dashdash = self.index;
                    self.index = self.argv.len();
                    return None;
                }
                self.operands.push(self.index);
                self.index += 1;
            }
        }
        let word = &self.argv[self.index];
        let c = word[self.next];
        self.next += 1;
        let at_end = self.next >= word.len();
        let advance = |s: &mut Self| {
            s.index += 1;
            s.next = 0;
        };
        match self.spec(c) {
            None => {
                if at_end {
                    advance(self);
                }
                self.complain("invalid option", c);
                Some(Opt::Error)
            }
            Some(false) => {
                if at_end {
                    advance(self);
                }
                Some(Opt::Flag(c, None))
            }
            Some(true) => {
                if !at_end {
                    let arg = word[self.next..].to_vec();
                    advance(self);
                    return Some(Opt::Flag(c, Some(arg)));
                }
                advance(self);
                match self.argv.get(self.index) {
                    Some(arg) => {
                        let arg = arg.clone();
                        self.index += 1;
                        Some(Opt::Flag(c, Some(arg)))
                    }
                    None => {
                        self.complain("option requires an argument", c);
                        Some(Opt::Error)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<Vec<u8>> {
        words.iter().map(|w| w.as_bytes().to_vec()).collect()
    }

    #[test]
    fn clusters_and_arguments() {
        let a = argv(&["nnn", "-dbx", "dir", "-T", "s"]);
        let mut g = Getopt::new(&a, b"b:dT:");
        assert_eq!(g.next_opt(), Some(Opt::Flag(b'd', None)));
        assert_eq!(g.next_opt(), Some(Opt::Flag(b'b', Some(b"x".to_vec()))));
        assert_eq!(g.next_opt(), Some(Opt::Flag(b'T', Some(b"s".to_vec()))));
        assert_eq!(g.next_opt(), None);
        assert_eq!(g.operands(), vec![b"dir".as_slice()]);
    }

    #[test]
    fn dashdash_ends_options() {
        let a = argv(&["nnn", "a", "--", "-V"]);
        let mut g = Getopt::new(&a, b"V");
        assert_eq!(g.next_opt(), None);
        assert_eq!(g.operands(), vec![b"a".as_slice(), b"-V".as_slice()]);
    }
}
