//! What the sidebar lists: Activity first, then sections of Conversations and Threads, under headers that fold them;
//! and the cursor moving through them.
//!
//! The app decides which sections there are and what each holds; the sidebar keeps track of which are folded, and
//! keeps the cursor on the same row as sections fill and change.

use std::collections::HashSet;

use slackers_model::Ts;

use crate::app::{ConversationRef, WorkspaceKey};

/// A part of the sidebar, under a header that folds it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    /// Followed Threads with Replies to read, from every Workspace.
    Threads,
    /// A Virtual Workspace's Channels; it's known by name.
    VirtualWorkspace(String),
    /// A Workspace's Conversations that aren't in any Virtual Workspace.
    Workspace(WorkspaceKey),
}

/// What a row under a header shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Item {
    Conversation(ConversationRef),
    /// A Thread, by its Conversation and parent.
    Thread {
        conversation: ConversationRef,
        parent: Ts,
    },
}

impl Item {
    /// The Conversation it is, or holds.
    pub(crate) fn conversation(&self) -> &ConversationRef {
        match self {
            Self::Conversation(conversation) | Self::Thread { conversation, .. } => conversation,
        }
    }
}

/// One line of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// Activity, merged from every Workspace: always the first row, in no section.
    Activity,
    /// A section's header.
    Header(Section),
    /// An item in a section. The same one can be in several.
    Item { section: Section, item: Item },
}

impl Row {
    /// The item on this row; `None` on Activity's row and on a header.
    pub(crate) fn item(&self) -> Option<&Item> {
        match self {
            Self::Activity | Self::Header(_) => None,
            Self::Item { item, .. } => Some(item),
        }
    }

    /// The section this row heads or belongs to; `None` for Activity's row.
    fn section(&self) -> Option<&Section> {
        match self {
            Self::Activity => None,
            Self::Header(section) | Self::Item { section, .. } => Some(section),
        }
    }
}

/// A change to which sections are folded, as vim's `z` commands make them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    /// `za`: fold the cursor's section, or unfold it.
    Toggle,
    /// `zo`: unfold the cursor's section.
    Open,
    /// `zc`: fold the cursor's section.
    Close,
    /// `zR`: unfold every section.
    OpenAll,
    /// `zM`: fold every section.
    CloseAll,
}

/// The sidebar's sections, which of them are folded, and where the cursor is.
#[derive(Debug, Default)]
pub(crate) struct Sidebar {
    /// Each section, top to bottom, with its items in order, folded or not.
    sections: Vec<(Section, Vec<Item>)>,
    folded: HashSet<Section>,
    /// What shows: each section's header, followed by its items unless it's folded.
    rows: Vec<Row>,
    /// The row under the cursor, as a position in `rows`.
    cursor: usize,
    /// Whether the user has moved the cursor. Until then, [`Sidebar::suggest`] may.
    moved: bool,
}

impl Sidebar {
    /// Lists `sections`, top to bottom, each with its items in order, in place of what it listed. The cursor stays
    /// on its row if that's still there, else goes to the same item in another section, else to its section's
    /// header, else stays where it was.
    pub(crate) fn list(&mut self, sections: Vec<(Section, Vec<Item>)>) {
        self.sections = sections;
        self.folded
            .retain(|folded| self.sections.iter().any(|(section, _)| section == folded));
        self.relist();
    }

    /// Lists `items` in `section` in place of what it held, keeping the cursor as [`Sidebar::list`] does. Does
    /// nothing if `section` isn't listed or already holds `items`.
    pub(crate) fn set(&mut self, section: &Section, items: Vec<Item>) {
        let Some((_, held)) = self
            .sections
            .iter_mut()
            .find(|(listed, _)| listed == section)
        else {
            return;
        };
        if *held != items {
            *held = items;
            self.relist();
        }
    }

    /// The rows showing, top to bottom.
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The position of the row under the cursor, among [`Sidebar::rows`].
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// The row under the cursor; `None` when there are no rows.
    pub(crate) fn selected(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    pub(crate) fn is_folded(&self, section: &Section) -> bool {
        self.folded.contains(section)
    }

    /// The items in `section`, folded or not, in order.
    pub(crate) fn items_in(&self, section: &Section) -> &[Item] {
        self.sections
            .iter()
            .find(|(listed, _)| listed == section)
            .map_or(&[], |(_, items)| items.as_slice())
    }

    /// The section `conversation` is listed in: the cursor's, if it's on `conversation` or one of its Threads, else
    /// the first that holds `conversation`, folded or not; `None` when none does.
    pub(crate) fn section_of(&self, conversation: &ConversationRef) -> Option<&Section> {
        let selected = self
            .selected()
            .filter(|row| row.item().map(Item::conversation) == Some(conversation));
        selected.and_then(Row::section).or_else(|| {
            let wanted = Item::Conversation(conversation.clone());
            self.sections
                .iter()
                .find(|(_, items)| items.contains(&wanted))
                .map(|(section, _)| section)
        })
    }

    /// Every Conversation listed, folded or not, each once, in order. Threads aren't Conversations.
    pub(crate) fn conversations(&self) -> Vec<&ConversationRef> {
        let mut seen = HashSet::new();
        self.sections
            .iter()
            .flat_map(|(_, items)| items)
            .filter_map(|item| match item {
                Item::Conversation(conversation) => Some(conversation),
                Item::Thread { .. } => None,
            })
            .filter(|conversation| seen.insert(*conversation))
            .collect()
    }

    /// Moves the cursor `rows` down, or up when negative, stopping at either end.
    pub(crate) fn move_by(&mut self, rows: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(rows).min(last);
        self.moved = true;
    }

    /// Moves the cursor to the first row.
    pub(crate) fn first(&mut self) {
        self.cursor = 0;
        self.moved = true;
    }

    /// Moves the cursor to the last row.
    pub(crate) fn last(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
        self.moved = true;
    }

    /// Moves the cursor to the first row `wanted` accepts, if there is one, unless the user has moved it.
    pub(crate) fn suggest(&mut self, wanted: impl Fn(&Row) -> bool) {
        if self.moved {
            return;
        }
        if let Some(at) = self.rows.iter().position(wanted) {
            self.cursor = at;
        }
    }

    /// Moves the cursor to `conversation`, unfolding the first section that holds it if need be. Does nothing when
    /// no section does.
    pub(crate) fn select(&mut self, conversation: &ConversationRef) {
        let wanted = Item::Conversation(conversation.clone());
        let holding = self
            .sections
            .iter()
            .find(|(_, items)| items.contains(&wanted));
        let Some((section, _)) = holding else {
            return;
        };
        if self.folded.remove(section) {
            self.relist();
        }
        let showing = self.rows.iter().position(|row| row.item() == Some(&wanted));
        if let Some(at) = showing {
            self.cursor = at;
            self.moved = true;
        }
    }

    /// Folds or unfolds sections. Folding the cursor's section moves the cursor to its header.
    pub(crate) fn fold(&mut self, change: Fold) {
        // Activity's row is in no section: only folding every section does anything there.
        let section = self.selected().and_then(Row::section).cloned();
        match (change, &section) {
            (Fold::Toggle, Some(section)) if self.folded.contains(section) => {
                self.folded.remove(section);
            }
            (Fold::Toggle | Fold::Close, Some(section)) => {
                self.folded.insert(section.clone());
            }
            (Fold::Open, Some(section)) => {
                self.folded.remove(section);
            }
            (Fold::Toggle | Fold::Close | Fold::Open, None) => return,
            (Fold::OpenAll, _) => self.folded.clear(),
            (Fold::CloseAll, _) => {
                self.folded = self.sections.iter().map(|(s, _)| s.clone()).collect();
            }
        }
        self.relist();
        // Rather than to the same item in another section, which `relist` would pick.
        if let Some(section) = section.filter(|section| self.folded.contains(section)) {
            self.cursor = self
                .rows
                .iter()
                .position(|row| *row == Row::Header(section.clone()))
                .unwrap_or(0);
        }
        self.moved = true;
    }

    /// Lays the rows out afresh from the sections and which are folded, keeping the cursor as [`Sidebar::list`]
    /// says.
    fn relist(&mut self) {
        let was = self.selected().cloned();
        let sections = self.sections.iter().flat_map(|(section, items)| {
            let shown = if self.folded.contains(section) {
                &[][..]
            } else {
                items.as_slice()
            };
            let header = Row::Header(section.clone());
            let rows = shown.iter().map(|item| Row::Item {
                section: section.clone(),
                item: item.clone(),
            });
            std::iter::once(header).chain(rows)
        });
        self.rows = std::iter::once(Row::Activity).chain(sections).collect();
        let found = was.and_then(|was| {
            let same = |row: &Row| *row == was;
            let same_item = |row: &Row| row.item().is_some() && row.item() == was.item();
            let header = was.section().map(|section| Row::Header(section.clone()));
            self.rows
                .iter()
                .position(same)
                .or_else(|| self.rows.iter().position(same_item))
                .or_else(|| {
                    self.rows
                        .iter()
                        .position(|row| Some(row) == header.as_ref())
                })
        });
        self.cursor = found
            .unwrap_or(self.cursor)
            .min(self.rows.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use slackers_model::ConversationId;

    use super::*;

    fn conversation(workspace: usize, id: &str) -> ConversationRef {
        ConversationRef {
            workspace: WorkspaceKey::nth(workspace),
            id: ConversationId::new(id),
        }
    }

    fn item(workspace: usize, id: &str) -> Item {
        Item::Conversation(conversation(workspace, id))
    }

    fn thread(workspace: usize, id: &str, parent: u64) -> Item {
        Item::Thread {
            conversation: conversation(workspace, id),
            parent: Ts::from_micros(parent),
        }
    }

    fn virtual_workspace(name: &str) -> Section {
        Section::VirtualWorkspace(name.to_owned())
    }

    fn workspace(index: usize) -> Section {
        Section::Workspace(WorkspaceKey::nth(index))
    }

    /// A sidebar with a Virtual Workspace holding a Channel from each of two Workspaces, which list one more each.
    fn listed() -> Sidebar {
        let mut sidebar = Sidebar::default();
        sidebar.list(vec![
            (virtual_workspace("K"), vec![item(0, "C1"), item(1, "C1")]),
            (workspace(0), vec![item(0, "C2")]),
            (workspace(1), vec![item(1, "D1")]),
        ]);
        sidebar
    }

    /// The rows, as `>` for a header followed by the section, a Conversation as `workspace:id`, and a Thread as
    /// `workspace:id/parent`.
    fn shown(sidebar: &Sidebar) -> Vec<String> {
        sidebar
            .rows()
            .iter()
            .map(|row| match row {
                Row::Activity => "activity".to_owned(),
                Row::Header(Section::Threads) => ">threads".to_owned(),
                Row::Header(Section::VirtualWorkspace(name)) => format!(">{name}"),
                Row::Header(Section::Workspace(key)) => format!(">{}", key.index()),
                Row::Item {
                    item: Item::Conversation(conversation),
                    ..
                } => format!("{}:{}", conversation.workspace.index(), conversation.id),
                Row::Item {
                    item:
                        Item::Thread {
                            conversation,
                            parent,
                        },
                    ..
                } => format!(
                    "{}:{}/{}",
                    conversation.workspace.index(),
                    conversation.id,
                    parent.as_micros()
                ),
            })
            .collect()
    }

    #[test]
    fn sections_list_their_header_then_their_items() {
        let sidebar = listed();
        assert_eq!(
            shown(&sidebar),
            ["activity", ">K", "0:C1", "1:C1", ">0", "0:C2", ">1", "1:D1"]
        );
        assert_eq!(sidebar.selected(), Some(&Row::Activity), "always first");
        assert_eq!(sidebar.conversations().len(), 4);
        assert_eq!(
            sidebar.section_of(&conversation(1, "C1")),
            Some(&virtual_workspace("K"))
        );
        assert_eq!(sidebar.section_of(&conversation(1, "C9")), None);
    }

    #[test]
    fn folding_hides_a_sections_items_and_brings_the_cursor_to_its_header() {
        let mut sidebar = listed();
        sidebar.fold(Fold::Toggle);
        assert_eq!(shown(&sidebar).len(), 8, "Activity's row folds nothing");
        sidebar.move_by(3);
        sidebar.fold(Fold::Toggle);
        assert_eq!(
            shown(&sidebar),
            ["activity", ">K", ">0", "0:C2", ">1", "1:D1"]
        );
        assert_eq!(sidebar.cursor(), 1);
        assert!(sidebar.is_folded(&virtual_workspace("K")));
        assert_eq!(sidebar.items_in(&virtual_workspace("K")).len(), 2);

        sidebar.fold(Fold::Toggle);
        assert_eq!(shown(&sidebar).len(), 8);

        sidebar.move_by(4);
        sidebar.fold(Fold::CloseAll);
        assert_eq!(shown(&sidebar), ["activity", ">K", ">0", ">1"]);
        assert_eq!(sidebar.cursor(), 2, "on its section's header");
        sidebar.fold(Fold::OpenAll);
        assert_eq!(shown(&sidebar).len(), 8);
        assert_eq!(sidebar.cursor(), 4);
    }

    #[test]
    fn folding_keeps_the_cursor_in_its_section_though_its_conversation_shows_in_another() {
        let mut sidebar = Sidebar::default();
        sidebar.list(vec![
            (virtual_workspace("K"), vec![item(0, "C1")]),
            (virtual_workspace("L"), vec![item(0, "C1")]),
        ]);
        sidebar.move_by(1);
        sidebar.fold(Fold::Close);
        assert_eq!(
            sidebar.selected(),
            Some(&Row::Header(virtual_workspace("K")))
        );
    }

    #[test]
    fn the_cursor_keeps_its_row_as_sections_change() {
        let mut sidebar = listed();
        sidebar.move_by(5);
        assert_eq!(sidebar.selected().and_then(Row::item), Some(&item(0, "C2")));

        // A Workspace above signs in: the cursor stays on its row.
        sidebar.list(vec![
            (workspace(2), vec![item(2, "C9"), item(2, "C8")]),
            (workspace(0), vec![item(0, "C2")]),
        ]);
        assert_eq!(sidebar.cursor(), 5);

        // Its Conversation moves to a Virtual Workspace: the cursor follows it.
        sidebar.list(vec![
            (virtual_workspace("K"), vec![item(0, "C2")]),
            (workspace(0), Vec::new()),
        ]);
        assert_eq!(
            sidebar.selected(),
            Some(&Row::Item {
                section: virtual_workspace("K"),
                item: item(0, "C2"),
            })
        );

        // The Virtual Workspace goes, and the Conversation with it: the cursor stays in bounds.
        sidebar.list(vec![(workspace(0), Vec::new())]);
        assert_eq!(sidebar.cursor(), 1);
    }

    #[test]
    fn setting_a_section_relists_only_what_changed() {
        let mut sidebar = Sidebar::default();
        sidebar.list(vec![
            (Section::Threads, Vec::new()),
            (workspace(0), vec![item(0, "C1")]),
        ]);
        sidebar.move_by(3);
        sidebar.set(
            &Section::Threads,
            vec![thread(0, "C1", 5), thread(0, "C1", 3)],
        );
        assert_eq!(
            shown(&sidebar),
            ["activity", ">threads", "0:C1/5", "0:C1/3", ">0", "0:C1"]
        );
        assert_eq!(sidebar.cursor(), 5, "still on #C1");
        assert_eq!(
            sidebar.conversations().len(),
            1,
            "threads aren't conversations"
        );

        // A Thread is read and leaves: the cursor on it goes to the header.
        sidebar.move_by(-3);
        sidebar.set(&Section::Threads, vec![thread(0, "C1", 3)]);
        assert_eq!(sidebar.selected(), Some(&Row::Header(Section::Threads)));
        assert_eq!(
            sidebar.section_of(&conversation(0, "C1")),
            Some(&workspace(0)),
            "the header isn't on it"
        );
        sidebar.move_by(1);
        assert_eq!(
            sidebar.section_of(&conversation(0, "C1")),
            Some(&Section::Threads),
            "the cursor is on one of its Threads"
        );
    }

    #[test]
    fn a_suggestion_moves_the_cursor_only_until_the_user_does() {
        let mut sidebar = listed();
        sidebar.suggest(|row| row.item() == Some(&item(1, "D1")));
        assert_eq!(sidebar.cursor(), 7);
        sidebar.move_by(-1);
        sidebar.suggest(|row| row.item().is_some());
        assert_eq!(sidebar.cursor(), 6);
    }

    #[test]
    fn selecting_a_conversation_unfolds_its_section() {
        let mut sidebar = listed();
        sidebar.fold(Fold::CloseAll);
        sidebar.select(&conversation(1, "D1"));
        assert_eq!(shown(&sidebar), ["activity", ">K", ">0", ">1", "1:D1"]);
        assert_eq!(sidebar.cursor(), 4);

        sidebar.select(&conversation(1, "C1"));
        assert_eq!(
            sidebar.selected(),
            Some(&Row::Item {
                section: virtual_workspace("K"),
                item: item(1, "C1"),
            }),
            "the first section holding it"
        );
    }

    #[test]
    fn moving_stops_at_either_end() {
        let mut sidebar = listed();
        sidebar.move_by(-3);
        assert_eq!(sidebar.cursor(), 0);
        sidebar.move_by(100);
        assert_eq!(sidebar.cursor(), 7);
        sidebar.first();
        assert_eq!(sidebar.cursor(), 0);
        sidebar.last();
        assert_eq!(sidebar.cursor(), 7);
    }
}
