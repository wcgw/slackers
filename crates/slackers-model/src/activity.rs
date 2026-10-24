use crate::Ts;

/// What the user hasn't read in one Conversation, and how much of it mentions them.
///
/// It starts from Slack's own counts ([`Activity::counted`]) and follows new Messages and Read Marker moves from
/// there. Slack counts Mentions without saying which Messages they are in, so those counted ones clear only when
/// the Read Marker passes everything that was counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
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

impl Activity {
    /// Activity as Slack reports it: the Read Marker, the newest Message, whether anything is unread, and how many
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
        let activity = Activity::counted(ts(10), ts(20), true, 2);
        assert!(activity.has_unread());
        assert_eq!(activity.mentions(), 2);
    }

    #[test]
    fn slack_saying_nothing_is_unread_wins_over_timestamps() {
        let activity = Activity::counted(ts(10), ts(20), false, 0);
        assert!(!activity.has_unread());
    }

    #[test]
    fn new_messages_and_mentions_add_up() {
        let mut activity = Activity::counted(ts(10), ts(10), false, 0);
        activity.record(ts(11), false);
        activity.record(ts(12), true);
        activity.record(ts(12), true); // the same Message again
        assert!(activity.has_unread());
        assert_eq!(activity.mentions(), 1);
    }

    #[test]
    fn already_read_messages_are_ignored() {
        let mut activity = Activity::counted(ts(10), ts(10), false, 0);
        activity.record(ts(9), true);
        assert!(!activity.has_unread());
        assert_eq!(activity.mentions(), 0);
    }

    #[test]
    fn marking_read_partway_keeps_later_mentions() {
        let mut activity = Activity::counted(ts(10), ts(20), true, 3);
        activity.record(ts(21), true);
        activity.record(ts(23), true);

        activity.mark_read(ts(15));
        assert_eq!(activity.mentions(), 5, "the counted ones may be after 15");

        activity.mark_read(ts(22));
        assert_eq!(activity.mentions(), 1, "only the one at 23 is left");
        assert!(activity.has_unread());

        activity.mark_read(ts(23));
        assert_eq!(activity.mentions(), 0);
        assert!(!activity.has_unread());
    }

    #[test]
    fn the_read_marker_never_moves_back() {
        let mut activity = Activity::counted(ts(10), ts(20), true, 0);
        activity.mark_read(ts(20));
        activity.mark_read(ts(15));
        assert!(!activity.has_unread());
    }
}
