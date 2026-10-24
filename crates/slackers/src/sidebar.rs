//! What the sidebar lists: sections of Conversations, under headers that fold them, and the cursor moving through
//! them.
//!
//! The app decides which sections there are and what each holds; the sidebar keeps track of which are folded, and
//! keeps the cursor on the same row as sections fill and change.

use std::collections::HashSet;

use crate::app::{ConversationRef, WorkspaceKey};

/// A part of the sidebar, under a header that folds it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    /// A Virtual Workspace's Channels; it's known by name.
    VirtualWorkspace(String),
    /// A Workspace's Conversations that aren't in any Virtual Workspace.
    Workspace(WorkspaceKey),
}

/// One line of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// A section's header.
    Header(Section),
    /// A Conversation in a section. The same Conversation can be in several.
    Conversation {
        section: Section,
        conversation: ConversationRef,
    },
}

impl Row {
    /// The Conversation on this row; `None` on a header.
    pub(crate) fn conversation(&self) -> Option<&ConversationRef> {
        match self {
            Self::Header(_) => None,
            Self::Conversation { conversation, .. } => Some(conversation),
        }
    }

    /// The section this row heads or belongs to.
    fn section(&self) -> &Section {
        match self {
            Self::Header(section) | Self::Conversation { section, .. } => section,
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
    /// Each section, top to bottom, with its Conversations in order, folded or not.
    sections: Vec<(Section, Vec<ConversationRef>)>,
    folded: HashSet<Section>,
    /// What shows: each section's header, followed by its Conversations unless it's folded.
    rows: Vec<Row>,
    /// The row under the cursor, as a position in `rows`.
    cursor: usize,
    /// Whether the user has moved the cursor. Until then, [`Sidebar::suggest`] may.
    moved: bool,
}

impl Sidebar {
    /// Lists `sections`, top to bottom, each with its Conversations in order, in place of what it listed. The
    /// cursor stays on its row if that's still there, else goes to the same Conversation in another section, else
    /// to its section's header, else stays where it was.
    pub(crate) fn list(&mut self, sections: Vec<(Section, Vec<ConversationRef>)>) {
        self.sections = sections;
        self.folded
            .retain(|folded| self.sections.iter().any(|(section, _)| section == folded));
        self.relist();
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

    /// The Conversations in `section`, folded or not, in order.
    pub(crate) fn conversations_in(&self, section: &Section) -> &[ConversationRef] {
        self.sections
            .iter()
            .find(|(listed, _)| listed == section)
            .map_or(&[], |(_, conversations)| conversations.as_slice())
    }

    /// The section `conversation` is listed in: the cursor's, if it's on `conversation`, else the first that holds it,
    /// folded or not; `None` when none does.
    pub(crate) fn section_of(&self, conversation: &ConversationRef) -> Option<&Section> {
        let selected = self
            .selected()
            .filter(|row| row.conversation() == Some(conversation));
        selected.map(Row::section).or_else(|| {
            self.sections
                .iter()
                .find(|(_, conversations)| conversations.contains(conversation))
                .map(|(section, _)| section)
        })
    }

    /// Every Conversation listed, folded or not, each once, in order.
    pub(crate) fn conversations(&self) -> Vec<&ConversationRef> {
        let mut seen = HashSet::new();
        self.sections
            .iter()
            .flat_map(|(_, conversations)| conversations)
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
        let holding = self
            .sections
            .iter()
            .find(|(_, conversations)| conversations.contains(conversation));
        let Some((section, _)) = holding else {
            return;
        };
        if self.folded.remove(section) {
            self.relist();
        }
        let showing = self
            .rows
            .iter()
            .position(|row| row.conversation() == Some(conversation));
        if let Some(at) = showing {
            self.cursor = at;
            self.moved = true;
        }
    }

    /// Folds or unfolds sections. Folding the cursor's section moves the cursor to its header.
    pub(crate) fn fold(&mut self, change: Fold) {
        let Some(section) = self.selected().map(|row| row.section().clone()) else {
            return;
        };
        match change {
            Fold::Toggle if self.folded.contains(&section) => {
                self.folded.remove(&section);
            }
            Fold::Toggle | Fold::Close => {
                self.folded.insert(section.clone());
            }
            Fold::Open => {
                self.folded.remove(&section);
            }
            Fold::OpenAll => self.folded.clear(),
            Fold::CloseAll => {
                self.folded = self.sections.iter().map(|(s, _)| s.clone()).collect();
            }
        }
        self.relist();
        // Rather than to the same Conversation in another section, which `relist` would pick.
        if self.folded.contains(&section) {
            self.cursor = self
                .rows
                .iter()
                .position(|row| row == &Row::Header(section.clone()))
                .unwrap_or(0);
        }
        self.moved = true;
    }

    /// Lays the rows out afresh from the sections and which are folded, keeping the cursor as [`Sidebar::list`]
    /// says.
    fn relist(&mut self) {
        let was = self.selected().cloned();
        self.rows = self
            .sections
            .iter()
            .flat_map(|(section, conversations)| {
                let shown = if self.folded.contains(section) {
                    &[][..]
                } else {
                    conversations.as_slice()
                };
                let header = Row::Header(section.clone());
                let rows = shown.iter().map(|conversation| Row::Conversation {
                    section: section.clone(),
                    conversation: conversation.clone(),
                });
                std::iter::once(header).chain(rows)
            })
            .collect();
        let found = was.and_then(|was| {
            let same = |row: &Row| *row == was;
            let same_conversation = |row: &Row| {
                row.conversation().is_some() && row.conversation() == was.conversation()
            };
            let header = Row::Header(was.section().clone());
            self.rows
                .iter()
                .position(same)
                .or_else(|| self.rows.iter().position(same_conversation))
                .or_else(|| self.rows.iter().position(|row| *row == header))
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
            (
                virtual_workspace("K"),
                vec![conversation(0, "C1"), conversation(1, "C1")],
            ),
            (workspace(0), vec![conversation(0, "C2")]),
            (workspace(1), vec![conversation(1, "D1")]),
        ]);
        sidebar
    }

    /// The rows, as `>` for a header followed by the section, or the Conversation as `workspace:id`.
    fn shown(sidebar: &Sidebar) -> Vec<String> {
        sidebar
            .rows()
            .iter()
            .map(|row| match row {
                Row::Header(Section::VirtualWorkspace(name)) => format!(">{name}"),
                Row::Header(Section::Workspace(key)) => format!(">{}", key.index()),
                Row::Conversation { conversation, .. } => {
                    format!("{}:{}", conversation.workspace.index(), conversation.id)
                }
            })
            .collect()
    }

    #[test]
    fn sections_list_their_header_then_their_conversations() {
        let sidebar = listed();
        assert_eq!(
            shown(&sidebar),
            [">K", "0:C1", "1:C1", ">0", "0:C2", ">1", "1:D1"]
        );
        assert_eq!(
            sidebar.selected(),
            Some(&Row::Header(virtual_workspace("K")))
        );
        assert_eq!(sidebar.conversations().len(), 4);
        assert_eq!(
            sidebar.section_of(&conversation(1, "C1")),
            Some(&virtual_workspace("K"))
        );
        assert_eq!(sidebar.section_of(&conversation(1, "C9")), None);
    }

    #[test]
    fn folding_hides_a_sections_conversations_and_brings_the_cursor_to_its_header() {
        let mut sidebar = listed();
        sidebar.move_by(2);
        sidebar.fold(Fold::Toggle);
        assert_eq!(shown(&sidebar), [">K", ">0", "0:C2", ">1", "1:D1"]);
        assert_eq!(sidebar.cursor(), 0);
        assert!(sidebar.is_folded(&virtual_workspace("K")));
        assert_eq!(sidebar.conversations_in(&virtual_workspace("K")).len(), 2);

        sidebar.fold(Fold::Toggle);
        assert_eq!(shown(&sidebar).len(), 7);

        sidebar.move_by(4);
        sidebar.fold(Fold::CloseAll);
        assert_eq!(shown(&sidebar), [">K", ">0", ">1"]);
        assert_eq!(sidebar.cursor(), 1, "on its section's header");
        sidebar.fold(Fold::OpenAll);
        assert_eq!(shown(&sidebar).len(), 7);
        assert_eq!(sidebar.cursor(), 3);
    }

    #[test]
    fn folding_keeps_the_cursor_in_its_section_though_its_conversation_shows_in_another() {
        let mut sidebar = Sidebar::default();
        sidebar.list(vec![
            (virtual_workspace("K"), vec![conversation(0, "C1")]),
            (virtual_workspace("L"), vec![conversation(0, "C1")]),
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
        sidebar.move_by(4);
        assert_eq!(
            sidebar.selected().and_then(Row::conversation),
            Some(&conversation(0, "C2"))
        );

        // A Workspace above signs in: the cursor stays on its row.
        sidebar.list(vec![
            (
                workspace(2),
                vec![conversation(2, "C9"), conversation(2, "C8")],
            ),
            (workspace(0), vec![conversation(0, "C2")]),
        ]);
        assert_eq!(sidebar.cursor(), 4);

        // Its Conversation moves to a Virtual Workspace: the cursor follows it.
        sidebar.list(vec![
            (virtual_workspace("K"), vec![conversation(0, "C2")]),
            (workspace(0), Vec::new()),
        ]);
        assert_eq!(
            sidebar.selected(),
            Some(&Row::Conversation {
                section: virtual_workspace("K"),
                conversation: conversation(0, "C2"),
            })
        );

        // The Virtual Workspace goes, and the Conversation with it: the cursor stays in bounds.
        sidebar.list(vec![(workspace(0), Vec::new())]);
        assert_eq!(sidebar.cursor(), 0);
    }

    #[test]
    fn a_suggestion_moves_the_cursor_only_until_the_user_does() {
        let mut sidebar = listed();
        sidebar.suggest(|row| row.conversation() == Some(&conversation(1, "D1")));
        assert_eq!(sidebar.cursor(), 6);
        sidebar.move_by(-1);
        sidebar.suggest(|row| row.conversation().is_some());
        assert_eq!(sidebar.cursor(), 5);
    }

    #[test]
    fn selecting_a_conversation_unfolds_its_section() {
        let mut sidebar = listed();
        sidebar.fold(Fold::CloseAll);
        sidebar.select(&conversation(1, "D1"));
        assert_eq!(shown(&sidebar), [">K", ">0", ">1", "1:D1"]);
        assert_eq!(sidebar.cursor(), 3);

        sidebar.select(&conversation(1, "C1"));
        assert_eq!(
            sidebar.selected(),
            Some(&Row::Conversation {
                section: virtual_workspace("K"),
                conversation: conversation(1, "C1"),
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
        assert_eq!(sidebar.cursor(), 6);
        sidebar.first();
        assert_eq!(sidebar.cursor(), 0);
        sidebar.last();
        assert_eq!(sidebar.cursor(), 6);
    }
}
