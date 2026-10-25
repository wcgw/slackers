use crate::Ts;

/// What the user hasn't read in one Conversation, and how much of it mentions them.
///
/// It starts from Slack's own counts ([`Unread::counted`]) and follows new Messages and Read Marker moves from
/// there. Slack counts Mentions without saying which Messages they are in, so those counted ones clear only when
/// the Read Marker passes everything that was counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unread {
    /// The Read Marker: everything up to and including it has been read.
    read_up_to: Ts,
    /// The newest Message known; zero when none is.
    latest: Ts,
    /// Mentions Slack counted at startup, all in Messages at or before `counted_until`.
    counted_mentions: u32,
    counted_until: Ts,
    /// Mentions seen since, by Message, oldest first.
    new_mentions: Vec<Ts>,
}

impl Unread {
    /// What's unread, as Slack reports it: the Read Marker, the newest Message, whether anything is unread, and how many
    /// unread Messages mention the user.
    pub fn counted(read_up_to: Ts, latest: Ts, has_unreads: bool, mentions: u32) -> Self {
        // Trust Slack's verdict over the timestamps: it knows about muted and hidden Messages we don't.
        let read_up_to = if has_unreads {
            read_up_to
        } else {
            read_up_to.max(latest)
        };
        Self {
            read_up_to,
            latest,
            counted_mentions: mentions,
            counted_until: latest,
            new_mentions: Vec::new(),
        }
    }

    /// Whether there's a Message newer than the Read Marker.
    pub fn has_unread(&self) -> bool {
        self.latest > self.read_up_to
    }

    /// The Read Marker: everything posted up to and including it has been read.
    pub fn read_up_to(&self) -> Ts {
        self.read_up_to
    }

    /// When the newest Message known was posted; zero when none is.
    pub fn latest(&self) -> Ts {
        self.latest
    }

    /// How many unread Messages mention the user.
    pub fn mentions(&self) -> u32 {
        let counted = if self.read_up_to < self.counted_until {
            self.counted_mentions
        } else {
            0
        };
        counted + u32::try_from(self.new_mentions.len()).unwrap_or(u32::MAX)
    }

    /// Notes a new Message posted at `ts`, which mentions the user when `mentions_me`.
    ///
    /// A Message at or before the Read Marker changes nothing: it was read already.
    pub fn record(&mut self, ts: Ts, mentions_me: bool) {
        if ts <= self.read_up_to {
            return;
        }
        self.latest = self.latest.max(ts);
        if mentions_me && !self.new_mentions.contains(&ts) {
            let at = self.new_mentions.partition_point(|&m| m < ts);
            self.new_mentions.insert(at, ts);
        }
    }

    /// Moves the Read Marker to just before the Message at `first`, as the user asked: it and everything after it
    /// are unread, and everything before it read. Mentions already read don't come back unread: Slack doesn't say
    /// which Messages they're in.
    pub fn mark_unread_from(&mut self, first: Ts) {
        self.read_up_to = Ts::from_micros(first.as_micros().saturating_sub(1));
        self.latest = self.latest.max(first);
        let read_up_to = self.read_up_to;
        self.new_mentions.retain(|&m| m > read_up_to);
    }

    /// Moves the Read Marker to `ts`. It never moves back: Slack reports only forward moves, and a stale report
    /// mustn't resurrect read Messages.
    pub fn mark_read(&mut self, ts: Ts) {
        self.read_up_to = self.read_up_to.max(ts);
        let read_up_to = self.read_up_to;
        self.new_mentions.retain(|&m| m > read_up_to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(seconds: u64) -> Ts {
        Ts::from_micros(seconds * 1_000_000)
    }

    #[test]
    fn slacks_counts_seed_it() {
        let unread = Unread::counted(ts(10), ts(20), true, 2);
        assert!(unread.has_unread());
        assert_eq!(unread.mentions(), 2);
    }

    #[test]
    fn slack_saying_nothing_is_unread_wins_over_timestamps() {
        let unread = Unread::counted(ts(10), ts(20), false, 0);
        assert!(!unread.has_unread());
    }

    #[test]
    fn new_messages_and_mentions_add_up() {
        let mut unread = Unread::counted(ts(10), ts(10), false, 0);
        unread.record(ts(11), false);
        unread.record(ts(12), true);
        unread.record(ts(12), true); // the same Message again
        assert!(unread.has_unread());
        assert_eq!(unread.mentions(), 1);
    }

    #[test]
    fn already_read_messages_are_ignored() {
        let mut unread = Unread::counted(ts(10), ts(10), false, 0);
        unread.record(ts(9), true);
        assert!(!unread.has_unread());
        assert_eq!(unread.mentions(), 0);
    }

    #[test]
    fn marking_unread_from_a_message_moves_the_marker_either_way() {
        let mut unread = Unread::counted(ts(30), ts(30), false, 0);
        unread.record(ts(31), true);
        unread.mark_read(ts(31));

        unread.mark_unread_from(ts(20));
        assert!(unread.has_unread());
        assert_eq!(unread.read_up_to(), Ts::from_micros(ts(20).as_micros() - 1));
        assert_eq!(unread.mentions(), 0, "read mentions stay read");
        unread.mark_read(ts(25));
        assert_eq!(unread.read_up_to(), ts(25));

        unread.record(ts(40), true);
        unread.mark_unread_from(ts(41));
        assert_eq!(unread.read_up_to(), Ts::from_micros(ts(41).as_micros() - 1));
        assert_eq!(unread.mentions(), 0, "forward, it reads what it passes");
        assert_eq!(
            unread.latest(),
            ts(41),
            "it's unread, so there's something to read"
        );
        assert!(unread.has_unread());
    }

    #[test]
    fn marking_read_partway_keeps_later_mentions() {
        let mut unread = Unread::counted(ts(10), ts(20), true, 3);
        unread.record(ts(21), true);
        unread.record(ts(23), true);

        unread.mark_read(ts(15));
        assert_eq!(unread.mentions(), 5, "the counted ones may be after 15");

        unread.mark_read(ts(22));
        assert_eq!(unread.mentions(), 1, "only the one at 23 is left");
        assert!(unread.has_unread());

        unread.mark_read(ts(23));
        assert_eq!(unread.mentions(), 0);
        assert!(!unread.has_unread());
    }

    #[test]
    fn the_read_marker_never_moves_back() {
        let mut unread = Unread::counted(ts(10), ts(20), true, 0);
        unread.mark_read(ts(20));
        unread.mark_read(ts(15));
        assert!(!unread.has_unread());
    }
}
