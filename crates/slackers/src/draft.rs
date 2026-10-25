//! Drafts: what the user writes before posting it, and whom it @-mentions.

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui_textarea::{CursorMove, TextArea};
use slackers_model::{Broadcast, UserId};
use slackers_slack::Part;

use crate::app::Place;

/// The text written for one Place, and the @-mentions picked while writing it.
///
/// A picked @-mention is written as its label, such as `@Ada Lovelace`, and is posted as a real one for as long as
/// that label stays whole in the text. Anything else that looks like one, such as an `@ada` typed without picking
/// anyone, is posted as typed.
#[derive(Debug)]
pub(crate) struct Draft {
    area: TextArea<'static>,
    picked: Vec<Picked>,
    /// What an empty Draft shows: whether it's a Message or a Reply.
    placeholder: &'static str,
}

/// Who an @-mention notifies.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Mentioned {
    Person(UserId),
    Broadcast(Broadcast),
}

/// An @-mention picked for a Draft.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Picked {
    /// How it's written, `@` included.
    label: String,
    mentioned: Mentioned,
}

/// What a Draft held when it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    /// As typed, with picked @-mentions written as their labels.
    pub(crate) text: String,
    picked: Vec<Picked>,
}

impl Draft {
    /// An empty Draft for `place`.
    pub(crate) fn new(place: &Place) -> Self {
        let placeholder = if place.thread.is_some() {
            "reply"
        } else {
            "message"
        };
        Self {
            area: text_area("", placeholder),
            picked: Vec::new(),
            placeholder,
        }
    }

    pub(crate) fn lines(&self) -> &[String] {
        self.area.lines()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.area.is_empty()
    }

    /// Its text, lines joined by `\n`.
    pub(crate) fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// Edits it as `key` says, as a text field does.
    pub(crate) fn input(&mut self, key: KeyEvent) {
        self.area.input(key);
    }

    pub(crate) fn insert_newline(&mut self) {
        self.area.insert_newline();
    }

    /// Replaces its text with `text`, say as an external editor left it, with the cursor at the end. @-mentions
    /// picked before stay picked wherever their labels are still whole.
    pub(crate) fn set_text(&mut self, text: &str) {
        self.area = text_area(text, self.placeholder);
    }

    /// The name being written as an @-mention, if one is: the word before the cursor, less the `@` it starts with.
    /// It's empty right after the `@`. A word with an `@` inside, such as an email address, isn't one.
    pub(crate) fn mentioning(&self) -> Option<&str> {
        let cursor = self.area.cursor();
        let line = self.area.lines().get(cursor.0)?;
        let end = line
            .char_indices()
            .nth(cursor.1)
            .map_or(line.len(), |(at, _)| at);
        let before = &line[..end];
        let word = before
            .rsplit_once(char::is_whitespace)
            .map_or(before, |(_, word)| word);
        let name = word.strip_prefix('@')?;
        name.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '\''))
            .then_some(name)
    }

    /// Writes an @-mention of `mentioned`, called `name`, and a space, in place of the one being written. A name
    /// already picked for someone else is told apart by a number.
    pub(crate) fn pick(&mut self, mentioned: Mentioned, name: &str) {
        let Some(written) = self.mentioning().map(|name| name.chars().count()) else {
            return;
        };
        // The name, and its `@`.
        for _ in 0..=written {
            self.area.delete_char();
        }
        let label = (1..)
            .map(|n| match n {
                1 => format!("@{name}"),
                n => format!("@{name} ({n})"),
            })
            .find(|label| {
                self.picked
                    .iter()
                    .all(|picked| picked.label != *label || picked.mentioned == mentioned)
            })
            .expect("only as many labels are taken as there are picks");
        self.area.insert_str(format!("{label} "));
        if !self.picked.iter().any(|picked| picked.label == label) {
            self.picked.push(Picked { label, mentioned });
        }
    }

    /// The labels of the @-mentions its text holds, in the order they were picked.
    pub(crate) fn mentions(&self) -> impl Iterator<Item = &str> {
        let text = self.text();
        let mut held = vec![false; self.picked.len()];
        for part in split(&text, &self.picked) {
            if let Split::Picked(index) = part {
                held[index] = true;
            }
        }
        self.picked
            .iter()
            .zip(held)
            .filter(|(_, held)| *held)
            .map(|(picked, _)| picked.label.as_str())
    }

    /// Takes what it holds, leaving it empty; or `None`, leaving it as it is, when it holds only blanks.
    pub(crate) fn take(&mut self) -> Option<Written> {
        let text = self.text();
        if text.trim().is_empty() {
            return None;
        }
        self.area = text_area("", self.placeholder);
        Some(Written {
            text,
            picked: std::mem::take(&mut self.picked),
        })
    }

    /// Puts back `written`, which couldn't be sent, ahead of anything written since.
    pub(crate) fn put_back(&mut self, written: Written) {
        let since = self.text();
        let text = if since.is_empty() {
            written.text
        } else {
            format!("{}\n{since}", written.text)
        };
        self.set_text(&text);
        // A label picked since for someone else keeps them.
        for picked in written.picked {
            if !self.picked.iter().any(|kept| kept.label == picked.label) {
                self.picked.push(picked);
            }
        }
    }
}

impl Widget for &Draft {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.area.render(area, buffer);
    }
}

impl Written {
    /// Its text, with each picked @-mention in place of its label.
    pub(crate) fn parts(&self) -> Vec<Part> {
        split(&self.text, &self.picked)
            .into_iter()
            .map(|part| match part {
                Split::Text(text) => Part::Text(text.to_owned()),
                Split::Picked(index) => match &self.picked[index].mentioned {
                    Mentioned::Person(id) => Part::User(id.clone()),
                    Mentioned::Broadcast(broadcast) => Part::Broadcast(*broadcast),
                },
            })
            .collect()
    }
}

/// A piece of a Draft's text: plain, or the label of a picked @-mention, by its index among those picked.
enum Split<'a> {
    Text(&'a str),
    Picked(usize),
}

/// `text` in pieces, each label of `picked` found where it starts a word and ends one. Where several labels could
/// start, the longest wins.
fn split<'a>(text: &'a str, picked: &[Picked]) -> Vec<Split<'a>> {
    let mut by_length: Vec<(usize, &Picked)> = picked.iter().enumerate().collect();
    by_length.sort_by_key(|(_, picked)| std::cmp::Reverse(picked.label.len()));
    let mut parts = Vec::new();
    let (mut plain, mut at) = (0, 0);
    while at < text.len() {
        let starts_word = text[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let found = by_length.iter().find(|(_, picked)| {
            let rest = &text[at..];
            rest.starts_with(&picked.label)
                && rest[picked.label.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        });
        match found {
            Some((index, picked)) if starts_word => {
                if plain < at {
                    parts.push(Split::Text(&text[plain..at]));
                }
                parts.push(Split::Picked(*index));
                at += picked.label.len();
                plain = at;
            }
            _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    if plain < text.len() {
        parts.push(Split::Text(&text[plain..]));
    }
    parts
}

/// A text field holding `text`, with the cursor at its end, showing `placeholder` while empty.
fn text_area(text: &str, placeholder: &'static str) -> TextArea<'static> {
    let mut area = TextArea::new(text.lines().map(str::to_owned).collect());
    area.set_cursor_line_style(Style::default());
    area.set_placeholder_text(placeholder);
    area.move_cursor(CursorMove::Bottom);
    area.move_cursor(CursorMove::End);
    area
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use slackers_model::ConversationId;

    use super::*;
    use crate::app::{ConversationRef, WorkspaceKey};

    fn draft(text: &str) -> Draft {
        let place = Place {
            conversation: ConversationRef {
                workspace: WorkspaceKey::nth(0),
                id: ConversationId::new("C1"),
            },
            thread: None,
        };
        let mut draft = Draft::new(&place);
        draft.set_text(text);
        draft
    }

    fn person(id: &str) -> Mentioned {
        Mentioned::Person(UserId::new(id))
    }

    fn sent(draft: &mut Draft) -> Vec<Part> {
        draft.take().expect("something to send").parts()
    }

    fn text(text: &str) -> Part {
        Part::Text(text.into())
    }

    #[test]
    fn a_word_starting_with_an_at_is_a_mention_being_written() {
        assert_eq!(draft("hi @ad").mentioning(), Some("ad"));
        assert_eq!(draft("@").mentioning(), Some(""));
        assert_eq!(draft("first\n@o'bri").mentioning(), Some("o'bri"));
        for not_one in ["hi", "hi @ada ", "ada@example.com", "@ada,", "(@ada"] {
            assert_eq!(draft(not_one).mentioning(), None, "{not_one}");
        }
        // It's the word before the cursor that counts.
        let mut moved = draft("@ada and more");
        moved.area.move_cursor(CursorMove::Head);
        for _ in 0.."@ad".len() {
            moved.area.move_cursor(CursorMove::Forward);
        }
        assert_eq!(moved.mentioning(), Some("ad"));
    }

    #[test]
    fn picking_writes_the_label_and_sending_turns_it_into_the_mention() {
        let mut draft = draft("thanks @ad");
        draft.pick(person("U1"), "Ada Lovelace");
        draft.input(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(draft.text(), "thanks @Ada Lovelace !");
        assert_eq!(draft.mentioning(), None);
        assert_eq!(draft.mentions().collect::<Vec<_>>(), ["@Ada Lovelace"]);
        assert_eq!(
            sent(&mut draft),
            [text("thanks "), Part::User(UserId::new("U1")), text(" !")]
        );
        assert!(draft.is_empty() && draft.mentions().next().is_none());
    }

    #[test]
    fn only_whole_labels_starting_a_word_are_mentions() {
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        let typed = draft.text() + "x@Ada @Adam @Ada_ @Ada, @Ada";
        draft.set_text(&typed);
        assert_eq!(
            sent(&mut draft),
            [
                Part::User(UserId::new("U1")),
                text(" x@Ada @Adam @Ada_ "),
                Part::User(UserId::new("U1")),
                text(", "),
                Part::User(UserId::new("U1")),
            ]
        );
    }

    #[test]
    fn the_longest_label_wins_and_namesakes_are_numbered() {
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        draft.area.insert_str("@");
        draft.pick(person("U2"), "Ada Lovelace");
        draft.area.insert_str("@");
        draft.pick(person("U3"), "Ada");
        draft.area.insert_str("@");
        draft.pick(person("U1"), "Ada");
        assert_eq!(draft.text(), "@Ada @Ada Lovelace @Ada (2) @Ada ");
        let mentioned: Vec<Part> = sent(&mut draft)
            .into_iter()
            .filter(|part| *part != text(" "))
            .collect();
        assert_eq!(
            mentioned,
            ["U1", "U2", "U3", "U1"].map(|id| Part::User(UserId::new(id)))
        );
    }

    #[test]
    fn edited_text_keeps_the_labels_left_whole() {
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        draft.area.insert_str("@");
        draft.pick(Mentioned::Broadcast(Broadcast::Here), "here");
        draft.set_text("@Ada, @her");
        assert_eq!(draft.mentions().collect::<Vec<_>>(), ["@Ada"]);
        assert_eq!(
            sent(&mut draft),
            [Part::User(UserId::new("U1")), text(", @her")]
        );
    }

    #[test]
    fn what_wasnt_sent_goes_back_ahead_of_what_was_written_since() {
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        let written = draft.take().unwrap();
        assert!(draft.take().is_none(), "nothing left to send");
        draft.set_text("later");
        draft.put_back(written);
        assert_eq!(draft.lines(), ["@Ada ", "later"]);
        assert_eq!(draft.mentions().collect::<Vec<_>>(), ["@Ada"]);
    }
}
