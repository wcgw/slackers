//! What the sidebar lists: Activity first, then sections of Conversations and Threads, under headers that fold
//! them. The app decides which sections there are and what each holds; the kit keeps the cursor and the folds of
//! the window showing them.

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

impl Section {
    /// What its block in the sidebar's buffer is known by: the same for as long as the section is listed, so that
    /// its fold stays open or closed.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Threads => "threads".to_owned(),
            Self::VirtualWorkspace(name) => format!("virtual workspace {name}"),
            Self::Workspace(key) => format!("workspace {}", key.index()),
        }
    }
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

/// The sidebar's sections, top to bottom, each with its items in order.
#[derive(Debug, Default)]
pub(crate) struct Sidebar {
    sections: Vec<(Section, Vec<Item>)>,
}

impl Sidebar {
    /// Lists `sections`, top to bottom, each with its items in order, in place of what it listed.
    pub(crate) fn list(&mut self, sections: Vec<(Section, Vec<Item>)>) {
        self.sections = sections;
    }

    /// Lists `items` in `section` in place of what it held. Does nothing if `section` isn't listed.
    pub(crate) fn set(&mut self, section: &Section, items: Vec<Item>) {
        if let Some((_, held)) = self
            .sections
            .iter_mut()
            .find(|(listed, _)| listed == section)
        {
            *held = items;
        }
    }

    /// Each section with its items, top to bottom.
    pub(crate) fn sections(&self) -> &[(Section, Vec<Item>)] {
        &self.sections
    }

    /// The first section that holds `conversation`; `None` when none does.
    pub(crate) fn section_of(&self, conversation: &ConversationRef) -> Option<&Section> {
        let wanted = Item::Conversation(conversation.clone());
        self.sections
            .iter()
            .find(|(_, items)| items.contains(&wanted))
            .map(|(section, _)| section)
    }

    /// Every Conversation listed, each once, in order. Threads aren't Conversations.
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

    fn virtual_workspace(name: &str) -> Section {
        Section::VirtualWorkspace(name.to_owned())
    }

    /// A sidebar with a Virtual Workspace holding a Channel from each of two Workspaces, which list one more each.
    fn listed() -> Sidebar {
        let mut sidebar = Sidebar::default();
        sidebar.list(vec![
            (virtual_workspace("K"), vec![item(0, "C1"), item(1, "C1")]),
            (
                Section::Workspace(WorkspaceKey::nth(0)),
                vec![item(0, "C2")],
            ),
            (
                Section::Workspace(WorkspaceKey::nth(1)),
                vec![item(1, "D1"), item(1, "C1")],
            ),
        ]);
        sidebar
    }

    #[test]
    fn a_conversation_belongs_to_the_first_section_listing_it() {
        let sidebar = listed();
        assert_eq!(
            sidebar.section_of(&conversation(1, "C1")),
            Some(&virtual_workspace("K"))
        );
        assert_eq!(sidebar.section_of(&conversation(1, "C9")), None);
    }

    #[test]
    fn every_conversation_is_listed_once() {
        let sidebar = listed();
        assert_eq!(sidebar.conversations().len(), 4);
        let mut sidebar = sidebar;
        sidebar.set(&virtual_workspace("K"), Vec::new());
        assert_eq!(sidebar.sections()[0].1, []);
        assert_eq!(sidebar.conversations().len(), 3);
    }

    #[test]
    fn each_section_keeps_its_key() {
        assert_eq!(Section::Threads.key(), "threads");
        assert_ne!(
            virtual_workspace("0").key(),
            Section::Workspace(WorkspaceKey::nth(0)).key()
        );
    }
}
