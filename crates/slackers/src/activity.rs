//! Activity, merged from every Workspace: what's loaded of each, what to ask Slack for next, where the user is in
//! it, and the Messages fetched to show for its items.
//!
//! Each Workspace's Activity loads a page at a time, newest first. Merged, the items show newest first down to where
//! a Workspace with older pages still to load stops: past that, its items would be missing from among the others'.

use std::collections::{HashMap, HashSet};

use slackers_model::{ActivityItem, ConversationId, Message, MessageRef, Ts};
use slackers_slack::{ActivityCursor, ActivityPage, Request};

use crate::app::WorkspaceKey;

/// Every Workspace's Activity, merged, newest first.
#[derive(Debug, Default)]
pub(crate) struct Activity {
    feeds: HashMap<WorkspaceKey, Feed>,
    /// Messages fetched to show for items, by Workspace and where they are; `None` for one that can't be had.
    messages: HashMap<(WorkspaceKey, ConversationId, Ts), Option<Message>>,
    /// Messages asked for and not answered yet.
    fetching: HashSet<(WorkspaceKey, ConversationId, Ts)>,
    /// The selected item, by Workspace and key; `None` for the newest.
    selected: Option<(WorkspaceKey, String)>,
}

/// One Workspace's Activity, as far as loaded.
#[derive(Debug, Default)]
struct Feed {
    /// Newest first.
    items: Vec<ActivityItem>,
    /// Whether the newest page has arrived.
    started: bool,
    /// Where the next, older page starts; `None` after the last, and before the first.
    next: Option<ActivityCursor>,
    /// The page asked for and not answered yet.
    loading: Option<Page>,
    /// Whether to ask for the newest page again once the one asked for arrives.
    stale: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Newest,
    Older,
}

/// An item of the merged Activity, and the Workspace it's from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry<'a> {
    pub(crate) workspace: WorkspaceKey,
    pub(crate) item: &'a ActivityItem,
}

/// Where a Message fetched for an item stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fetched<'a> {
    /// Not asked for.
    Unasked,
    /// On its way.
    Fetching,
    Got(&'a Message),
    /// Gone, or Slack wouldn't say.
    Unavailable,
}

impl Activity {
    /// The merged items, newest first: every Workspace's, down to where one with older pages still to load stops.
    pub(crate) fn entries(&self) -> Vec<Entry<'_>> {
        // The newest of the oldest items loaded of each Workspace with more to load.
        let frontier = self
            .feeds
            .values()
            .filter(|feed| feed.next.is_some())
            .filter_map(|feed| feed.items.last().map(|item| item.at))
            .max();
        let mut entries: Vec<Entry<'_>> = self
            .feeds
            .iter()
            .flat_map(|(&workspace, feed)| {
                feed.items.iter().map(move |item| Entry { workspace, item })
            })
            .filter(|entry| frontier.is_none_or(|frontier| entry.item.at >= frontier))
            .collect();
        entries.sort_by(|a, b| {
            b.item
                .at
                .cmp(&a.item.at)
                .then_with(|| a.workspace.cmp(&b.workspace))
                .then_with(|| a.item.key.cmp(&b.item.key))
        });
        entries
    }

    /// Whether a page is on its way.
    pub(crate) fn is_loading(&self) -> bool {
        self.feeds.values().any(|feed| feed.loading.is_some())
    }

    /// How many of the [entries](Activity::entries) are unread.
    pub(crate) fn unread(&self) -> usize {
        self.entries()
            .iter()
            .filter(|entry| entry.item.unread)
            .count()
    }

    /// Asks for `key`'s newest page; if that's on its way already, asks again once it arrives, as what's on its way
    /// may miss what just happened.
    pub(crate) fn refresh(&mut self, key: WorkspaceKey) -> Option<Request> {
        let feed = self.feeds.entry(key).or_default();
        if feed.loading.is_some() {
            feed.stale = true;
            return None;
        }
        feed.loading = Some(Page::Newest);
        Some(Request::Activity { cursor: None })
    }

    /// Asks for the next older page of each Workspace whose loaded items stop where the merged ones do, unless one
    /// is on its way already.
    pub(crate) fn load_older(&mut self) -> Vec<(WorkspaceKey, Request)> {
        let oldest_shown = self.entries().last().map(|entry| entry.item.at);
        let mut requests = Vec::new();
        for (&key, feed) in &mut self.feeds {
            let stops_here = feed.items.last().map(|item| item.at) == oldest_shown;
            if !stops_here || feed.loading.is_some() {
                continue;
            }
            if let Some(cursor) = &feed.next {
                feed.loading = Some(Page::Older);
                let cursor = Some(cursor.clone());
                requests.push((key, Request::Activity { cursor }));
            }
        }
        requests
    }

    /// Takes in `key`'s page that started at `cursor`. Returns what to ask for next, if anything.
    pub(crate) fn received(
        &mut self,
        key: WorkspaceKey,
        cursor: Option<ActivityCursor>,
        page: ActivityPage,
    ) -> Option<Request> {
        let feed = self.feeds.entry(key).or_default();
        let replaced: HashSet<String> = page.items.iter().map(|item| item.key.clone()).collect();
        feed.items.retain(|item| !replaced.contains(&item.key));
        feed.items.extend(page.items);
        feed.items
            .sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.key.cmp(&b.key)));
        // The newest page says where the next starts only the first time: after that, older pages are loaded
        // already, or the first's cursor still leads to them.
        if cursor.is_some() || !feed.started {
            feed.next = page.next;
        }
        feed.started = true;
        self.answered(key)
    }

    /// Notes that asking for `key`'s page failed. Returns what to ask for next, if anything.
    pub(crate) fn failed(&mut self, key: WorkspaceKey) -> Option<Request> {
        self.answered(key)
    }

    fn answered(&mut self, key: WorkspaceKey) -> Option<Request> {
        let feed = self.feeds.entry(key).or_default();
        feed.loading = None;
        if std::mem::take(&mut feed.stale) {
            return self.refresh(key);
        }
        None
    }

    /// Whether `key` has an unread item about a Message in `conversation`.
    pub(crate) fn has_unread_in(&self, key: WorkspaceKey, conversation: &ConversationId) -> bool {
        self.feeds.get(&key).is_some_and(|feed| {
            feed.items
                .iter()
                .any(|item| item.unread && item.message.conversation == *conversation)
        })
    }

    /// The position of the selected item among the [entries](Activity::entries).
    pub(crate) fn selected(&self) -> usize {
        let Some((workspace, key)) = &self.selected else {
            return 0;
        };
        self.entries()
            .iter()
            .position(|entry| entry.workspace == *workspace && entry.item.key == *key)
            .unwrap_or(0)
    }

    /// The selected item, if there are any.
    pub(crate) fn selected_entry(&self) -> Option<Entry<'_>> {
        self.entries().get(self.selected()).copied()
    }

    /// Moves the selection `by` items down, older, or up, newer, when negative, stopping at either end.
    pub(crate) fn select_by(&mut self, by: isize) {
        let entries = self.entries();
        let last = entries.len().saturating_sub(1);
        let at = self.selected().saturating_add_signed(by).min(last);
        self.selected = entries
            .get(at)
            .map(|entry| (entry.workspace, entry.item.key.clone()));
    }

    /// Selects the newest item.
    pub(crate) fn select_newest(&mut self) {
        self.selected = None;
    }

    /// Selects the oldest item loaded.
    pub(crate) fn select_oldest(&mut self) {
        self.select_by(isize::MAX);
    }

    /// Where the Message at `at`, in `key`, stands, as fetched for an item.
    pub(crate) fn message(&self, key: WorkspaceKey, at: &MessageRef) -> Fetched<'_> {
        let id = (key, at.conversation.clone(), at.ts);
        match self.messages.get(&id) {
            Some(Some(message)) => Fetched::Got(message),
            Some(None) => Fetched::Unavailable,
            None if self.fetching.contains(&id) => Fetched::Fetching,
            None => Fetched::Unasked,
        }
    }

    /// Asks for the Message at `at`, in `key`, unless it was asked for already.
    pub(crate) fn fetch(&mut self, key: WorkspaceKey, at: &MessageRef) -> Option<Request> {
        let id = (key, at.conversation.clone(), at.ts);
        if self.messages.contains_key(&id) || !self.fetching.insert(id) {
            return None;
        }
        Some(Request::Message {
            conversation: at.conversation.clone(),
            ts: at.ts,
        })
    }

    /// Takes in the Message at `ts` in `conversation`, in `key`, as fetched; `None` when it can't be had.
    pub(crate) fn fetched(
        &mut self,
        key: WorkspaceKey,
        conversation: ConversationId,
        ts: Ts,
        message: Option<Message>,
    ) {
        let id = (key, conversation, ts);
        self.fetching.remove(&id);
        self.messages.insert(id, message);
    }
}

#[cfg(test)]
mod tests {
    use slackers_model::{Happened, UserId};

    use super::*;

    fn w(index: usize) -> WorkspaceKey {
        WorkspaceKey::nth(index)
    }

    fn item(key: &str, seconds: u64, unread: bool) -> ActivityItem {
        ActivityItem {
            key: key.into(),
            at: Ts::from_micros(seconds * 1_000_000),
            unread,
            message: MessageRef {
                conversation: ConversationId::new("C1"),
                ts: Ts::from_micros(seconds * 1_000_000),
                thread: None,
            },
            happened: Happened::Added {
                author: UserId::new("U1"),
            },
        }
    }

    fn page(items: Vec<ActivityItem>, next: Option<&str>) -> ActivityPage {
        ActivityPage {
            items,
            next: next.map(|cursor| ActivityCursor(cursor.into())),
        }
    }

    fn keys(activity: &Activity) -> Vec<&str> {
        activity
            .entries()
            .iter()
            .map(|entry| entry.item.key.as_str())
            .collect()
    }

    #[test]
    fn workspaces_merge_newest_first_down_to_where_one_with_more_to_load_stops() {
        let mut activity = Activity::default();
        assert_eq!(
            activity.refresh(w(0)),
            Some(Request::Activity { cursor: None })
        );
        assert_eq!(
            activity.refresh(w(1)),
            Some(Request::Activity { cursor: None })
        );
        activity.received(
            w(0),
            None,
            page(vec![item("a9", 9, true), item("a5", 5, false)], None),
        );
        activity.received(
            w(1),
            None,
            page(
                vec![item("b8", 8, false), item("b6", 6, true)],
                Some("b-older"),
            ),
        );
        assert_eq!(
            keys(&activity),
            ["a9", "b8", "b6"],
            "a5 might come after b's next page"
        );
        assert_eq!(activity.unread(), 2);

        assert_eq!(
            activity.load_older(),
            [(
                w(1),
                Request::Activity {
                    cursor: Some(ActivityCursor("b-older".into()))
                }
            )]
        );
        assert!(activity.load_older().is_empty(), "already on its way");
        let older = Some(ActivityCursor("b-older".into()));
        activity.received(w(1), older, page(vec![item("b4", 4, false)], None));
        assert_eq!(keys(&activity), ["a9", "b8", "b6", "a5", "b4"]);
    }

    #[test]
    fn only_the_workspace_that_stops_the_merge_soonest_loads_older_items() {
        let mut activity = Activity::default();
        let a = page(
            vec![item("a9", 9, false), item("a3", 3, false)],
            Some("a-older"),
        );
        let b = page(
            vec![item("b8", 8, false), item("b6", 6, false)],
            Some("b-older"),
        );
        activity.received(w(0), None, a);
        activity.received(w(1), None, b);
        assert_eq!(
            keys(&activity),
            ["a9", "b8", "b6"],
            "a3 might come after b's next page"
        );
        assert_eq!(
            activity.load_older(),
            [(
                w(1),
                Request::Activity {
                    cursor: Some(ActivityCursor("b-older".into()))
                }
            )],
            "a has more loaded than shows already"
        );
    }

    #[test]
    fn a_newer_page_replaces_items_that_changed_and_keeps_older_ones() {
        let mut activity = Activity::default();
        activity.refresh(w(0));
        activity.received(
            w(0),
            None,
            page(vec![item("t", 5, true), item("m", 4, false)], Some("x")),
        );
        activity.refresh(w(0));
        activity.received(
            w(0),
            None,
            page(vec![item("n", 7, true), item("t", 6, true)], Some("y")),
        );
        assert_eq!(keys(&activity), ["n", "t", "m"]);
        assert_eq!(
            activity.load_older(),
            [(
                w(0),
                Request::Activity {
                    cursor: Some(ActivityCursor("x".into()))
                }
            )],
            "still after what's loaded"
        );
    }

    #[test]
    fn a_refresh_asked_for_while_one_is_on_its_way_follows_it() {
        let mut activity = Activity::default();
        assert!(activity.refresh(w(0)).is_some());
        assert_eq!(activity.refresh(w(0)), None);
        assert_eq!(activity.refresh(w(0)), None);
        assert_eq!(
            activity.received(w(0), None, page(Vec::new(), None)),
            Some(Request::Activity { cursor: None }),
            "once"
        );
        assert_eq!(activity.failed(w(0)), None);
        assert!(activity.refresh(w(0)).is_some());
    }

    #[test]
    fn the_selection_stays_on_its_item_as_newer_ones_come() {
        let mut activity = Activity::default();
        activity.received(
            w(0),
            None,
            page(vec![item("b", 2, false), item("a", 1, false)], None),
        );
        assert_eq!(activity.selected(), 0);
        activity.select_by(1);
        assert_eq!(
            activity.selected_entry().map(|e| e.item.key.as_str()),
            Some("a")
        );
        activity.select_by(5);
        assert_eq!(activity.selected(), 1, "stops at the end");
        activity.refresh(w(0));
        activity.received(w(0), None, page(vec![item("c", 3, false)], None));
        assert_eq!(activity.selected(), 2, "still on a");
        activity.select_newest();
        assert_eq!(activity.selected(), 0);
        activity.select_oldest();
        assert_eq!(activity.selected(), 2);
        activity.select_by(-10);
        assert_eq!(activity.selected(), 0);
    }

    #[test]
    fn a_message_is_fetched_once_and_kept() {
        let mut activity = Activity::default();
        let at = item("k", 3, false).message;
        assert_eq!(activity.message(w(0), &at), Fetched::Unasked);
        assert_eq!(
            activity.fetch(w(0), &at),
            Some(Request::Message {
                conversation: ConversationId::new("C1"),
                ts: at.ts
            })
        );
        assert_eq!(activity.fetch(w(0), &at), None);
        assert_eq!(activity.message(w(0), &at), Fetched::Fetching);
        activity.fetched(w(0), at.conversation.clone(), at.ts, None);
        assert_eq!(activity.message(w(0), &at), Fetched::Unavailable);
        assert_eq!(activity.fetch(w(0), &at), None, "not asked for again");
        assert_eq!(
            activity.message(w(1), &at),
            Fetched::Unasked,
            "another Workspace's"
        );
    }

    #[test]
    fn unread_items_are_found_by_conversation() {
        let mut activity = Activity::default();
        activity.received(w(0), None, page(vec![item("a", 1, true)], None));
        assert!(activity.has_unread_in(w(0), &ConversationId::new("C1")));
        assert!(!activity.has_unread_in(w(0), &ConversationId::new("C2")));
        assert!(!activity.has_unread_in(w(1), &ConversationId::new("C1")));
    }
}
