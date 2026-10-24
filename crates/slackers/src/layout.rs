//! Wrapping styled text, with unbreakable pieces such as emoji pictures, into lines of a given width.

use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr as _;

/// Something to lay out.
#[derive(Debug)]
pub(crate) enum Piece {
    /// Text, which may break at spaces (or anywhere, for a word longer than a line) and contains no line breaks
    /// other than `\n`, which ends the line.
    Text(String, Style),
    /// Something drawn as one, `width` cells wide: never split, and wrapped to the next line when it doesn't fit.
    Atom(Span<'static>, u16),
}

/// Lays `pieces` out in lines at most `width` cells wide (at least one). Spaces at a wrap are dropped; spaces after
/// a `\n` are kept, so indentation survives. Returns at least one line, possibly empty.
pub(crate) fn wrap(pieces: Vec<Piece>, width: u16) -> Vec<Vec<Span<'static>>> {
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
            Piece::Text(text, style) => {
                for (index, paragraph) in text.split('\n').enumerate() {
                    if index > 0 {
                        lines.end();
                    }
                    for token in tokens(paragraph) {
                        lines.text(token, style);
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

struct Lines {
    width: usize,
    done: Vec<Vec<Span<'static>>>,
    line: Vec<Span<'static>>,
    /// Cells used on `line`.
    used: usize,
    /// Whether `line` started at a wrap rather than at a line break.
    wrapped: bool,
}

impl Lines {
    fn push(&mut self, span: Span<'static>, cells: usize) {
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
        while self
            .line
            .last()
            .is_some_and(|span| span.content.chars().all(|c| c == ' '))
        {
            self.line.pop();
        }
        self.end();
        self.wrapped = true;
    }

    fn text(&mut self, token: &str, style: Style) {
        let cells = token.width();
        if token.starts_with(' ') {
            if self.used + cells > self.width {
                self.wrap();
            } else if !(self.used == 0 && self.wrapped) {
                self.push(Span::styled(token.to_owned(), style), cells);
            }
            return;
        }
        if self.used + cells <= self.width {
            self.push(Span::styled(token.to_owned(), style), cells);
            return;
        }
        if cells <= self.width {
            self.wrap();
            self.push(Span::styled(token.to_owned(), style), cells);
            return;
        }
        // A word longer than a whole line: fill lines character by character.
        let mut chunk = String::new();
        let mut chunk_cells = 0;
        for c in token.chars() {
            let c_cells = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if self.used + chunk_cells + c_cells > self.width {
                if !chunk.is_empty() {
                    self.push(Span::styled(std::mem::take(&mut chunk), style), chunk_cells);
                    chunk_cells = 0;
                }
                self.wrap();
            }
            chunk.push(c);
            chunk_cells += c_cells;
        }
        if !chunk.is_empty() {
            self.push(Span::styled(chunk, style), chunk_cells);
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
    use super::*;

    fn text(text: &str) -> Piece {
        Piece::Text(text.into(), Style::new())
    }

    fn rendered(lines: &[Vec<Span<'static>>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.iter().map(|span| span.content.as_ref()).collect())
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
        let lines = wrap(
            vec![text("abcd"), Piece::Atom(Span::raw("EE"), 2), text("x")],
            5,
        );
        assert_eq!(rendered(&lines), ["abcd", "EEx"]);
    }

    #[test]
    fn wide_characters_count_double() {
        let lines = wrap(vec![text("ab 👍👍 c")], 5);
        assert_eq!(rendered(&lines), ["ab", "👍👍", "c"]);
    }

    #[test]
    fn nothing_lays_out_as_one_empty_line() {
        assert_eq!(rendered(&wrap(Vec::new(), 10)), [""]);
    }
}
