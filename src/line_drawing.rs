//! Line drawing: the DEC special graphics character set.
//!
//! A server that has no UTF-8 locale falls back to the VT220 special graphics
//! set, where ncurses draws a box with `lqqk` for the top edge and `x` for the
//! sides. `vt100` knows nothing about character sets - it stores the byte it was
//! handed - so the letters are translated into the Unicode box-drawing
//! characters they stand for before the parser ever sees them. A cell therefore
//! holds a real `─`, which the font can paint and a copy can hand on.

use std::borrow::Cow;

/// `SO` - shift the right hand set into the grid.
const SHIFT_OUT: u8 = 0x0e;
/// `SI` - shift the left hand set back in.
const SHIFT_IN: u8 = 0x0f;
/// `ESC` - the lead byte of every character set sequence there is.
const ESCAPE: u8 = 0x1b;
/// `BEL`, which is how most servers end an OSC string.
const BELL: u8 = 0x07;
/// `ST` as a single C1 byte: the one byte form of an escape terminated string.
const TERMINATOR: u8 = 0x9c;

/// The set a server designates for the printable bytes it sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Charset {
    /// Plain ASCII: every printable byte means itself.
    #[default]
    Ascii,
    /// `ESC ( 0` - the VT220 special graphics set.
    Special,
}

impl Charset {
    /// The set chosen by the designator byte that closes `ESC ( 0` and friends.
    ///
    /// The United Kingdom set and the alternate ROM sets are treated as ASCII:
    /// they differ in a handful of punctuation glyphs nobody draws boxes with.
    fn designated(byte: u8) -> Self {
        if byte == b'0' {
            Self::Special
        } else {
            Self::Ascii
        }
    }

    /// The character `byte` stands for, or `None` when it means itself.
    fn glyph(self, byte: u8) -> Option<char> {
        if self != Self::Special {
            return None;
        }
        Some(match byte {
            b'_' => '▒',
            b'`' => '◆',
            b'a' => '░',
            b'b' => '␉',
            b'c' => '␌',
            b'd' => '␍',
            b'e' => '␊',
            b'f' => '°',
            b'g' => '±',
            b'h' => '␤',
            b'i' => '␋',
            b'j' => '┘',
            b'k' => '┐',
            b'l' => '┌',
            b'm' => '└',
            b'n' => '┼',
            b'o' => '⎺',
            b'p' => '⎻',
            b'q' => '─',
            b'r' => '⎼',
            b's' => '⎽',
            b't' => '├',
            b'u' => '┤',
            b'v' => '┴',
            b'w' => '┬',
            b'x' => '│',
            b'y' => '≤',
            b'z' => '≥',
            b'{' => 'π',
            b'|' => '≠',
            b'}' => '£',
            b'~' => '·',
            _ => return None,
        })
    }
}

/// How far into an escape sequence the filter has got.
#[derive(Debug, Default, PartialEq, Eq)]
enum State {
    /// Between sequences, where the printable bytes live.
    #[default]
    Ground,
    /// A two byte escape, or one that may yet designate a set.
    Esc,
    /// `ESC (`, `ESC )`, `ESC *` or `ESC +`: the designator comes next.
    Designate,
    /// `ESC` plus an intermediate: one more byte and it is over.
    Intermediate,
    /// Inside a CSI, up to its final byte.
    Csi,
    /// Inside an OSC, DCS, APC or PM string.
    StringSeq,
    /// An `ESC` inside a string, where `\` closes it.
    StringEsc,
}

/// Translates the special graphics bytes in server output.
///
/// The state is kept between chunks, because a designation sequence - and, more
/// often, its UTF-8 replacement - can straddle a packet boundary as easily as a
/// word does.
#[derive(Debug, Default)]
pub struct CharsetFilter {
    state: State,
    /// What `ESC ( 0` and `ESC ) 0` pointed the two halves at.
    g0: Charset,
    g1: Charset,
    /// Whether `SO` has swapped the right hand set into the grid.
    shifted: bool,
    /// The `(`, `)`, `*` or `+` whose designator is still to come.
    designator: u8,
}

impl CharsetFilter {
    /// Rewrite one chunk of server output.
    ///
    /// Chunks without anything to translate come back as they were, which is
    /// every chunk an ordinary prompt sends.
    #[must_use]
    pub fn translate<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        let mut out: Option<Vec<u8>> = None;
        // Start of the stretch of `bytes` that is still worth its original
        // bytes; everything before it has already been copied into `out`.
        let mut keep = 0;

        for (index, &byte) in bytes.iter().enumerate() {
            let glyph = match self.state {
                State::Ground => match byte {
                    ESCAPE => {
                        self.state = State::Esc;
                        None
                    }
                    SHIFT_OUT => {
                        self.shifted = true;
                        None
                    }
                    SHIFT_IN => {
                        self.shifted = false;
                        None
                    }
                    _ => self.active().glyph(byte),
                },
                State::Esc => match byte {
                    b'(' | b')' | b'*' | b'+' => {
                        self.designator = byte;
                        self.state = State::Designate;
                        None
                    }
                    b'[' => {
                        self.state = State::Csi;
                        None
                    }
                    // OSC, DCS, APC and PM strings: their text is not the grid's.
                    b']' | b'P' | b'_' | b'^' => {
                        self.state = State::StringSeq;
                        None
                    }
                    // DECKPAM, DECALN and friends end after one more byte.
                    b' ' | b'#' | b'%' => {
                        self.state = State::Intermediate;
                        None
                    }
                    // RIS starts the terminal over, character sets included.
                    b'c' => {
                        *self = Self::default();
                        None
                    }
                    _ => {
                        self.state = State::Ground;
                        None
                    }
                },
                State::Designate => {
                    self.designate(byte);
                    self.state = State::Ground;
                    None
                }
                State::Intermediate => {
                    self.state = State::Ground;
                    None
                }
                State::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = State::Ground;
                    }
                    None
                }
                State::StringSeq => {
                    match byte {
                        BELL | TERMINATOR => self.state = State::Ground,
                        ESCAPE => self.state = State::StringEsc,
                        _ => {}
                    }
                    None
                }
                State::StringEsc => {
                    self.state = if byte == b'\\' {
                        State::Ground
                    } else {
                        State::Esc
                    };
                    None
                }
            };

            if let Some(glyph) = glyph {
                let buffer = out.get_or_insert_with(|| Vec::with_capacity(bytes.len() + 16));
                buffer.extend_from_slice(&bytes[keep..index]);
                let mut encoded = [0u8; 4];
                buffer.extend_from_slice(glyph.encode_utf8(&mut encoded).as_bytes());
                keep = index + 1;
            }
        }

        match out {
            None => Cow::Borrowed(bytes),
            Some(mut out) => {
                out.extend_from_slice(&bytes[keep..]);
                Cow::Owned(out)
            }
        }
    }

    /// The set currently shifted into the grid.
    fn active(&self) -> Charset {
        if self.shifted {
            self.g1
        } else {
            self.g0
        }
    }

    /// Point the set being designated at the charset `byte` chooses.
    fn designate(&mut self, byte: u8) {
        let charset = Charset::designated(byte);
        match self.designator {
            b'(' => self.g0 = charset,
            b')' => self.g1 = charset,
            // Reaching G2 and G3 needs a single shift, which no line drawing
            // program of note sends; leaving them alone keeps ASCII honest.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Translate one server write, in one go.
    fn drawn(bytes: &[u8]) -> String {
        String::from_utf8(CharsetFilter::default().translate(bytes).into_owned()).unwrap()
    }

    #[test]
    fn a_box_comes_out_as_lines() {
        assert_eq!(drawn(b"\x1b(0lqqqqk\x1b(B"), "\u{1b}(0┌────┐\u{1b}(B");
    }

    #[test]
    fn nothing_is_touched_until_a_set_is_designated() {
        assert_eq!(drawn(b"quiet q and x"), "quiet q and x");
        assert_eq!(drawn(b"\x1b(0qqq\x1b(Bqqq"), "\u{1b}(0───\u{1b}(Bqqq");
    }

    #[test]
    fn a_designation_may_straddle_chunks() {
        let mut filter = CharsetFilter::default();
        assert_eq!(filter.translate(b"\x1b(").as_ref(), b"\x1b(");
        assert_eq!(
            String::from_utf8(filter.translate(b"0lqk").into_owned()).unwrap(),
            "0┌─┐"
        );
    }

    #[test]
    fn escape_sequences_keep_their_own_letters() {
        // Colours, cursor shapes and window titles all spell parameters and text
        // with the very letters the line drawing set would steal.
        assert_eq!(
            drawn(b"\x1b(0\x1b[38;5;10mq\x1b]2;aqqb\x07q"),
            "\u{1b}(0\u{1b}[38;5;10m─\u{1b}]2;aqqb\u{07}─"
        );
    }

    #[test]
    fn shift_out_brings_in_the_right_hand_set() {
        assert_eq!(drawn(b"\x1b)0\x0eqq\x0fqq"), "\u{1b})0\u{0e}──\u{0f}qq");
    }

    #[test]
    fn a_reset_forgets_the_set() {
        assert_eq!(drawn(b"\x1b(0q\x1bcq"), "\u{1b}(0─\u{1b}cq");
    }

    #[test]
    fn utf8_passes_untouched() {
        let text = "中文 ✓";
        assert_eq!(drawn(b"\x1b(0"), "\u{1b}(0");
        assert_eq!(drawn(text.as_bytes()), text);
    }
}
