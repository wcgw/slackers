//! Where the user is in a pane of Messages: which one is selected, how far the pane is scrolled, and what was new
//! when it opened.
//!
//! As in vim, moving the selection scrolls the pane to show it, and scrolling the pane drags the selection along
//! to stay in view. Only drawing knows how many lines each Message takes, so drawing settles which ([`Pane::settle`])
//! and the app adopts the outcome ([`Pane::adopt`]).

use slackers_model::{Timeline, Ts};

/// The selection and scroll position of one pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pane {
    /// The selected Message; `None` for the newest, whichever that is as new Messages arrive.
    cursor: Option<Ts>,
    /// How many lines the pane is scrolled up from its bottom.
    scroll: usize,
    /// Which of the two the last key moved, so that drawing adjusts the other.
    anchor: Anchor,
    /// What the last draw settled.
    settled: Settled,
    /// The Read Marker as it stood when the pane opened, when something was unread past it: what's new starts
    /// after it, until the pane opens anew.
    new_after: Option<Ts>,
}

/// What the last key moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Anchor {
    /// The selection: scroll to show it.
    #[default]
    Cursor,
    /// The view: move the selection into it.
    Scroll,
}

/// Where a drawn pane ended up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The selected Message; `None` for the newest.
    pub(crate) cursor: Option<Ts>,
    /// How many lines the pane is scrolled up from its bottom.
    pub(crate) scroll: usize,
    /// How far up it can scroll, in lines.
    pub(crate) max_scroll: usize,
    /// How many lines it shows.
    pub(crate) height: usize,
    /// Whether some of the selected Message is above the pane's top.
    pub(crate) cut_above: bool,
    /// Whether some of the selected Message is below the pane's bottom.
    pub(crate) cut_below: bool,
}

/// One drawn block of a pane: the Message it shows, if it can be selected, and how many lines it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    /// The Message; `None` for something that can't be selected, such as a Message still being sent.
    pub(crate) ts: Option<Ts>,
    pub(crate) lines: usize,
}

impl Pane {
    /// A pane opening on what's unread past `read_up_to`: the first Message after it is selected once
    /// [`Pane::snap`] finds it held, and what's new starts there.
    pub(crate) fn unread_after(read_up_to: Ts) -> Self {
        Self {
            // Just past the Read Marker, until a Message there is held.
            cursor: Some(Ts::from_micros(read_up_to.as_micros().saturating_add(1))),
            new_after: Some(read_up_to),
            ..Self::default()
        }
    }

    /// Where what was new when the pane opened starts: after this; `None` when nothing was.
    pub(crate) fn new_after(&self) -> Option<Ts> {
        self.new_after
    }

    /// Moves a selection that isn't on a Message `timeline` holds onto the next one it holds, else the newest; leaves
    /// it while `timeline` holds none.
    pub(crate) fn snap(&mut self, timeline: &Timeline) {
        if let Some(cursor) = self.cursor
            && timeline.newest().is_some()
            && timeline.get(cursor).is_none()
        {
            self.cursor = timeline
                .after(cursor)
                .filter(|&next| Some(next) != timeline.newest());
        }
    }

    /// The selected Message; `None` for the newest.
    pub(crate) fn cursor(&self) -> Option<Ts> {
        self.cursor
    }

    /// Selects the Message at `ts`, and scrolls to show it. One not loaded yet stays selected, and shows once it is;
    /// until then the newest shows.
    pub(crate) fn select(&mut self, ts: Ts) {
        self.cursor = Some(ts);
        self.anchor = Anchor::Cursor;
    }

    /// The selected Message: the one under the cursor, else the newest in `timeline`.
    pub(crate) fn selected(&self, timeline: &Timeline) -> Option<Ts> {
        self.cursor.or_else(|| timeline.newest())
    }

    /// Whether the newest Message is selected and the pane is at its bottom: what counts as having seen it all.
    pub(crate) fn at_newest(&self) -> bool {
        self.cursor.is_none() && self.scroll == 0
    }

    /// Selects the Message before the selected one (`k`), or, while the selected one starts above the pane, scrolls
    /// up within it.
    pub(crate) fn up(&mut self, timeline: &Timeline) {
        if self.settled.cut_above {
            return self.scroll_by(self.half_page());
        }
        if let Some(before) = self.selected(timeline).and_then(|ts| timeline.before(ts)) {
            self.cursor = Some(before);
        }
        self.anchor = Anchor::Cursor;
    }

    /// Selects the Message after the selected one (`j`), or, while the selected one ends below the pane, scrolls
    /// down within it.
    pub(crate) fn down(&mut self, timeline: &Timeline) {
        if self.settled.cut_below {
            return self.scroll_by(-self.half_page());
        }
        if let Some(cursor) = self.cursor {
            let next = timeline.after(cursor);
            self.cursor = next.filter(|&next| Some(next) != timeline.newest());
        }
        self.anchor = Anchor::Cursor;
    }

    /// Selects the oldest Message (`g`).
    pub(crate) fn oldest(&mut self, timeline: &Timeline) {
        self.cursor = timeline
            .oldest()
            .filter(|&oldest| Some(oldest) != timeline.newest());
        self.anchor = Anchor::Cursor;
    }

    /// Selects the newest Message and scrolls to the bottom (`G`).
    pub(crate) fn newest(&mut self) {
        self.cursor = None;
        self.scroll = 0;
        self.anchor = Anchor::Cursor;
    }

    /// Scrolls `lines` up, or down when negative (`Ctrl-y`, `Ctrl-e`), dragging the selection along.
    pub(crate) fn scroll_by(&mut self, lines: isize) {
        self.scroll = self.scroll.saturating_add_signed(lines);
        self.anchor = Anchor::Scroll;
    }

    /// Half the pane's height, in lines, for `Ctrl-u` and `Ctrl-d`; at least one.
    pub(crate) fn half_page(&self) -> isize {
        isize::try_from(self.settled.height / 2)
            .unwrap_or(isize::MAX)
            .max(1)
    }

    /// Where the pane ends up when drawn `height` lines tall with `blocks`, oldest first.
    pub(crate) fn settle(&self, blocks: &[Block], height: usize) -> Settled {
        let total: usize = blocks.iter().map(|block| block.lines).sum();
        let max_scroll = total.saturating_sub(height);
        let mut scroll = self.scroll.min(max_scroll);
        let mut cursor = self.cursor;

        // Each block's lines, and which one the cursor is on; a cursor on a Message that's gone falls back to the
        // newest.
        let mut ranges = Vec::with_capacity(blocks.len());
        let mut offset = 0;
        for block in blocks {
            ranges.push((block.ts, offset, offset + block.lines));
            offset += block.lines;
        }
        let newest = ranges.iter().rposition(|(ts, ..)| ts.is_some());
        // The newest Message's range runs to the bottom, so that what's still being sent below it shows too.
        let range = |cursor: Option<Ts>| -> Option<(usize, usize)> {
            let index = match cursor {
                Some(ts) => ranges
                    .iter()
                    .position(|(held, ..)| *held == Some(ts))
                    .or(newest)?,
                None => newest?,
            };
            let (_, start, end) = ranges[index];
            Some((start, if Some(index) == newest { total } else { end }))
        };

        match self.anchor {
            // Following the newest Message: stay at the bottom, like a chat.
            Anchor::Cursor if cursor.is_none() => scroll = 0,
            Anchor::Cursor => {
                if let Some((start, end)) = range(cursor) {
                    // Bring the end into view, then the start, so that a Message taller than the pane shows its top.
                    if end > total - scroll {
                        scroll = total - end;
                    }
                    if start < (total - scroll).saturating_sub(height) {
                        scroll = total.saturating_sub(start + height);
                    }
                    scroll = scroll.min(max_scroll);
                }
            }
            Anchor::Scroll => {
                let bottom = total - scroll;
                let top = bottom.saturating_sub(height);
                if let Some((start, end)) = range(cursor) {
                    let selectable = ranges.iter().filter(|(ts, ..)| ts.is_some());
                    if end <= top {
                        cursor = selectable
                            .clone()
                            .find(|(_, _, end)| *end > top)
                            .and_then(|(ts, ..)| *ts);
                    } else if start >= bottom {
                        cursor = selectable
                            .rev()
                            .find(|(_, start, _)| *start < bottom)
                            .and_then(|(ts, ..)| *ts);
                    }
                    if newest.is_some_and(|newest| ranges[newest].0 == cursor) {
                        cursor = None;
                    }
                }
            }
        }

        let bottom = total - scroll;
        let top = bottom.saturating_sub(height);
        let (start, end) = range(cursor).unwrap_or((top, top));
        Settled {
            cursor,
            scroll,
            max_scroll,
            height,
            cut_above: start < top,
            cut_below: end > bottom,
        }
    }

    /// Takes on where the pane was last drawn.
    pub(crate) fn adopt(&mut self, settled: Settled) {
        self.cursor = settled.cursor;
        self.scroll = settled.scroll;
        self.settled = settled;
    }
}

#[cfg(test)]
mod tests {
    use slackers_model::{Author, Message, RichText, UserId};

    use super::*;

    fn ts(seconds: u64) -> Ts {
        Ts::from_micros(seconds * 1_000_000)
    }

    /// A Timeline of Messages at 1, 2, … `count` seconds.
    fn timeline(count: u64) -> Timeline {
        let mut timeline = Timeline::default();
        for second in 1..=count {
            timeline.insert(Message {
                ts: ts(second),
                author: Author::User(UserId::new("U1")),
                content: RichText::plain("hi"),
                reply_to: None,
                thread: None,
                reactions: Vec::new(),
                files: Vec::new(),
                edited: false,
                ephemeral: false,
            });
        }
        timeline
    }

    /// Blocks for Messages at 1, 2, … seconds, `lines` tall each.
    fn blocks(lines: &[usize]) -> Vec<Block> {
        (1..)
            .zip(lines)
            .map(|(second, &lines)| Block {
                ts: Some(ts(second)),
                lines,
            })
            .collect()
    }

    /// Draws `pane` and adopts the outcome, as the app does.
    fn draw(pane: &mut Pane, blocks: &[Block], height: usize) -> Settled {
        let settled = pane.settle(blocks, height);
        pane.adopt(settled);
        settled
    }

    #[test]
    fn opening_on_whats_unread_snaps_to_its_first_message_once_held() {
        let mut pane = Pane::unread_after(ts(2));
        assert_eq!(pane.new_after(), Some(ts(2)));
        assert!(!pane.at_newest(), "not read while it's sought");
        pane.snap(&Timeline::default());
        assert_eq!(
            pane.cursor(),
            Some(Ts::from_micros(ts(2).as_micros() + 1)),
            "nothing held yet"
        );
        pane.snap(&timeline(5));
        assert_eq!(pane.cursor(), Some(ts(3)));
        pane.snap(&timeline(5));
        assert_eq!(pane.cursor(), Some(ts(3)), "on a Message, it stays");

        let mut pane = Pane::unread_after(ts(4));
        pane.snap(&timeline(5));
        assert_eq!(pane.cursor(), None, "only the newest is new");
        assert!(pane.at_newest());
        assert_eq!(Pane::default().new_after(), None);
    }

    #[test]
    fn it_starts_at_the_newest_and_follows_it() {
        let mut pane = Pane::default();
        let settled = draw(&mut pane, &blocks(&[3, 3, 3]), 4);
        assert_eq!(
            (settled.cursor, settled.scroll, settled.max_scroll),
            (None, 0, 5)
        );
        assert!(pane.at_newest());
        assert_eq!(pane.selected(&timeline(3)), Some(ts(3)));
        assert_eq!(
            pane.selected(&timeline(4)),
            Some(ts(4)),
            "a new Message is selected as it arrives"
        );
    }

    #[test]
    fn k_and_j_move_by_message_and_scroll_to_show_it() {
        let (timeline, blocks) = (timeline(3), blocks(&[3, 3, 3]));
        let mut pane = Pane::default();
        draw(&mut pane, &blocks, 4);

        pane.up(&timeline);
        let settled = draw(&mut pane, &blocks, 4);
        assert_eq!(settled.cursor, Some(ts(2)));
        assert_eq!(
            settled.scroll, 2,
            "the least scrolling that shows it: its top at the pane's top"
        );
        assert!(!pane.at_newest());

        pane.up(&timeline);
        assert_eq!(
            draw(&mut pane, &blocks, 4).scroll,
            5,
            "the first Message shows from its top"
        );
        pane.up(&timeline);
        assert_eq!(
            draw(&mut pane, &blocks, 4).cursor,
            Some(ts(1)),
            "nothing older"
        );

        pane.down(&timeline);
        pane.down(&timeline);
        let settled = draw(&mut pane, &blocks, 4);
        assert_eq!(
            (settled.cursor, settled.scroll),
            (None, 0),
            "back on the newest, following again"
        );
        assert!(pane.at_newest());
    }

    #[test]
    fn scrolling_drags_the_selection_along() {
        let (timeline, blocks) = (timeline(3), blocks(&[3, 3, 3]));
        let mut pane = Pane::default();
        draw(&mut pane, &blocks, 4);

        pane.scroll_by(4);
        let settled = draw(&mut pane, &blocks, 4);
        assert_eq!(settled.scroll, 4);
        assert_eq!(
            settled.cursor,
            Some(ts(2)),
            "the newest left the view; the lowest visible took over"
        );

        pane.scroll_by(-4);
        assert_eq!(
            draw(&mut pane, &blocks, 4).cursor,
            Some(ts(2)),
            "still in view: it stays"
        );
        pane.newest();
        assert!(draw(&mut pane, &blocks, 4).cursor.is_none());
        let _ = timeline;
    }

    #[test]
    fn a_message_taller_than_the_pane_is_read_in_steps() {
        let (timeline, blocks) = (timeline(3), blocks(&[2, 10, 2]));
        let mut pane = Pane::default();
        draw(&mut pane, &blocks, 4);

        pane.up(&timeline);
        let settled = draw(&mut pane, &blocks, 4);
        assert_eq!(
            (settled.cursor, settled.scroll),
            (Some(ts(2)), 8),
            "its top shows"
        );
        assert!(settled.cut_below);

        pane.down(&timeline);
        let settled = draw(&mut pane, &blocks, 4);
        assert_eq!(
            (settled.cursor, settled.scroll),
            (Some(ts(2)), 6),
            "j scrolls within it"
        );
        pane.down(&timeline);
        pane.down(&timeline);
        let settled = draw(&mut pane, &blocks, 4);
        assert_eq!(settled.cursor, Some(ts(2)));
        assert!(!settled.cut_below, "its end is in view now");
        pane.down(&timeline);
        assert_eq!(
            draw(&mut pane, &blocks, 4).cursor,
            None,
            "only now does j move on"
        );
    }

    #[test]
    fn g_selects_the_oldest_and_a_vanished_selection_falls_back_to_the_newest() {
        let (timeline, mut blocks) = (timeline(3), blocks(&[1, 1, 1]));
        let mut pane = Pane::default();
        pane.oldest(&timeline);
        assert_eq!(draw(&mut pane, &blocks, 2).cursor, Some(ts(1)));

        blocks.remove(0);
        let settled = draw(&mut pane, &blocks, 2);
        assert!(!settled.cut_above && !settled.cut_below);
        assert_eq!(settled.scroll, 0, "shows the newest instead");
    }

    #[test]
    fn pending_blocks_show_below_the_newest_but_cant_be_selected() {
        let mut blocks = blocks(&[2, 2]);
        blocks.push(Block { ts: None, lines: 2 });
        let mut pane = Pane::default();
        let settled = draw(&mut pane, &blocks, 3);
        assert_eq!(
            (settled.cursor, settled.scroll),
            (None, 0),
            "the bottom, pending block included"
        );

        pane.scroll_by(1);
        let settled = draw(&mut pane, &blocks, 3);
        assert_eq!(settled.cursor, None, "the newest Message is still in view");
    }
}
