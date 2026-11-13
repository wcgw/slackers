//! Wrapping styled text, with unbreakable pieces such as emoji pictures, into lines of a given width.
//!
//! The kit wraps a buffer's lines too, but from the window's left edge: slackers wraps a Message's text itself, to
//! the window's width, so that every line of it hangs off its author's avatar.

use unicode_width::UnicodeWidthStr as _;
use wcgw_kit::doc::Span;

/// Something to lay out.
#[derive(Debug)]
pub(crate) enum Piece<T> {
    /// Text, which may break at spaces (or anywhere, for a word longer than a line) and contains no line breaks
    /// other than `\n`, which ends the line. Each piece it breaks into keeps its group, style and target.
    Text(Span<T>),
    /// Something drawn as one, `width` cells wide: never split, and wrapped to the next line when it doesn't fit.
    Atom(Span<T>, u16),
}

/// Lays `pieces` out in lines at most `width` cells wide (at least one). Spaces at a wrap are dropped; spaces after
/// a `\n` are kept, so indentation survives. Returns at least one line, possibly empty.
pub(crate) fn wrap<T: Clone>(pieces: Vec<Piece<T>>, width: u16) -> Vec<Vec<Span<T>>> {
    let mut lines = Lines {
        width: usize::from(width.max(1)),
        done: Vec::new(),
        line: Vec::new(),
        used: 0,
        wrapped: false,
    };
    for piece in pieces {
        match piece {
            Piece::Atom(span, cells) => {
                let cells = usize::from(cells);
                if lines.used + cells > lines.width && lines.used > 0 {
                    lines.wrap();
                }
                lines.push(span, cells);
            }
            Piece::Text(span) => {
                for (index, paragraph) in span.text.split('\n').enumerate() {
                    if index > 0 {
                        lines.end();
                    }
                    for token in tokens(paragraph) {
                        lines.text(token, &span);
                    }
                }
            }
        }
    }
    if !lines.line.is_empty() || lines.done.is_empty() {
        lines.end();
    }
    lines.done
}

/// `like`, holding `text` instead.
fn holding<T: Clone>(like: &Span<T>, text: String) -> Span<T> {
    Span {
        text,
        ..like.clone()
    }
}

struct Lines<T> {
    width: usize,
    done: Vec<Vec<Span<T>>>,
    line: Vec<Span<T>>,
    /// Cells used on `line`.
    used: usize,
    /// Whether `line` started at a wrap rather than at a line break.
    wrapped: bool,
}

impl<T: Clone> Lines<T> {
    fn push(&mut self, span: Span<T>, cells: usize) {
        self.line.push(span);
        self.used += cells;
    }

    /// Ends the line at a line break.
    fn end(&mut self) {
        self.done.push(std::mem::take(&mut self.line));
        self.used = 0;
        self.wrapped = false;
    }

    /// Ends the line because the next piece doesn't fit, dropping the spaces it ended with.
    fn wrap(&mut self) {
        while self.line.last().is_some_and(|span| {
            span.picture.is_none() && !span.text.is_empty() && span.text.chars().all(|c| c == ' ')
        }) {
            self.line.pop();
        }
        self.end();
        self.wrapped = true;
    }

    fn text(&mut self, token: &str, like: &Span<T>) {
        let cells = token.width();
        if token.starts_with(' ') {
            if self.used + cells > self.width {
                self.wrap();
            } else if !(self.used == 0 && self.wrapped) {
                self.push(holding(like, token.to_owned()), cells);
            }
            return;
        }
        if self.used + cells <= self.width {
            self.push(holding(like, token.to_owned()), cells);
            return;
        }
        if cells <= self.width {
            self.wrap();
            self.push(holding(like, token.to_owned()), cells);
            return;
        }
        // A word longer than a whole line: fill lines character by character.
        let mut chunk = String::new();
        let mut chunk_cells = 0;
        for c in token.chars() {
            let c_cells = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if self.used + chunk_cells + c_cells > self.width {
                if !chunk.is_empty() {
                    self.push(holding(like, std::mem::take(&mut chunk)), chunk_cells);
                    chunk_cells = 0;
                }
                self.wrap();
            }
            chunk.push(c);
            chunk_cells += c_cells;
        }
        if !chunk.is_empty() {
            self.push(holding(like, chunk), chunk_cells);
        }
    }
}

/// `text` split into runs of spaces and runs of anything else.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let is_space = first == ' ';
        let end = rest
            .find(|c: char| (c == ' ') != is_space)
            .unwrap_or(rest.len());
        let (token, tail) = rest.split_at(end);
        rest = tail;
        Some(token)
    })
}

#[cfg(test)]
mod tests {
    use wcgw_kit::hl::Group;

    use super::*;

    fn text(text: &str) -> Piece<()> {
        Piece::Text(Span::new(text, Group::NORMAL))
    }

    fn rendered(lines: &[Vec<Span<()>>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.iter().map(|span| span.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn wraps_at_spaces_and_drops_them() {
        let lines = wrap(vec![text("the quick brown fox")], 10);
        assert_eq!(rendered(&lines), ["the quick", "brown fox"]);
    }

    #[test]
    fn breaks_words_longer_than_a_line() {
        let lines = wrap(vec![text("abcdefghij klm")], 4);
        assert_eq!(rendered(&lines), ["abcd", "efgh", "ij", "klm"]);
    }

    #[test]
    fn line_breaks_keep_indentation() {
        let lines = wrap(vec![text("a\n  b\n\nc")], 10);
        assert_eq!(rendered(&lines), ["a", "  b", "", "c"]);
    }

    #[test]
    fn atoms_wrap_whole() {
        let atom = Piece::Atom(Span::new("EE", Group::NORMAL), 2);
        let lines = wrap(vec![text("abcd"), atom, text("x")], 5);
        assert_eq!(rendered(&lines), ["abcd", "EEx"]);
    }

    #[test]
    fn wide_characters_count_double() {
        let lines = wrap(vec![text("ab 👍👍 c")], 5);
        assert_eq!(rendered(&lines), ["ab", "👍👍", "c"]);
    }

    #[test]
    fn nothing_lays_out_as_one_empty_line() {
        assert_eq!(rendered(&wrap::<()>(Vec::new(), 10)), [""]);
    }

    #[test]
    fn pieces_keep_their_group_and_target() {
        let link = Piece::Text(Span::new("see this", Group::UNDERLINED).with_target(7));
        let lines = wrap(vec![link], 4);
        assert!(
            lines
                .iter()
                .flatten()
                .all(|span| span.target == Some(7) && span.group == Group::UNDERLINED)
        );
    }
}
