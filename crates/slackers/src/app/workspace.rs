//! The Workspaces the user signs in to, and what slackers knows of each: its Conversations and what's Unread in
//! them, their Messages, its people and bots, emoji, slash commands, and Followed Threads.

use std::collections::{HashMap, HashSet};

use slackers_model::{
    Author, Block, BotId, Broadcast, Conversation, ConversationId, ConversationKind, Inline,
    Message, Presence, RichText, Timeline, Ts, Unread, UserId,
};
use slackers_slack::{Bot, Connection, Error, Permalinks, Person, Request, SlashCommand, Snapshot};

use super::{ConversationRef, Effect, Effects, Place, WorkspaceKey, slack};
use crate::config::ChannelRef;
use crate::draft::{Draft, Mentioned, Reference};
use crate::emoji::Emojis;
use crate::{markdown, search};

/// The most people and broadcasts offered to @-mention at once, and the most emoji and slash commands.
pub(crate) const SUGGESTIONS: usize = 8;

/// The Workspaces the user signs in to, in the order the config lists them.
#[derive(Debug)]
pub(crate) struct Workspaces(Vec<SignIn>);

/// A Workspace the user signs in to, and how far that got.
#[derive(Debug)]
pub(crate) struct SignIn {
    /// What the config and the keyring call it.
    pub(crate) label: String,
    pub(crate) phase: Phase,
}

/// Where signing in to a Workspace stands.
#[derive(Debug)]
pub(crate) enum Phase {
    SigningIn,
    Ready(Box<Workspace>),
    /// It failed, for this reason.
    Failed(String),
}

/// A Workspace signed in to, as the user sees it.
#[derive(Debug)]
pub(crate) struct Workspace {
    /// What Slack calls it.
    pub(crate) name: String,
    /// Where its icon is; `None` when it has none of its own.
    pub(crate) icon: Option<String>,
    pub(crate) me: UserId,
    /// Its live connection; `None` until that first comes up.
    pub(crate) connection: Option<Connection>,
    /// Its people, as far as they've been looked up.
    pub(crate) users: HashMap<UserId, slackers_slack::User>,
    requested_users: HashSet<UserId>,
    /// Its bots that posted without saying their names, as far as they've been looked up.
    pub(crate) bots: HashMap<BotId, Bot>,
    requested_bots: HashSet<BotId>,
    /// Whether people are using Slack, as far as Slack said.
    pub(crate) presence: HashMap<UserId, Presence>,
    /// Whose presence it was last asked to follow, in sidebar order.
    presence_followed: Vec<UserId>,
    pub(crate) emojis: Emojis,
    permalinks: Permalinks,
    /// Channels by name, then DMs, most recently active first.
    pub(crate) conversations: Vec<Conversation>,
    pub(crate) unread: HashMap<ConversationId, Unread>,
    /// Conversations' top-level Messages.
    pub(crate) timelines: HashMap<ConversationId, Timeline>,
    /// Threads, by their Conversation and parent.
    pub(crate) threads: HashMap<(ConversationId, Ts), Timeline>,
    /// Conversations with a history request in flight.
    pub(crate) loading: HashSet<ConversationId>,
    /// Threads with a request for their Replies in flight.
    pub(crate) loading_threads: HashSet<(ConversationId, Ts)>,
    /// Followed Threads, by their Conversation and parent.
    pub(crate) followed: HashMap<(ConversationId, Ts), Followed>,
    /// Its slash commands, by name; empty until Slack lists them.
    pub(crate) commands: Vec<SlashCommand>,
    /// Whom Slack found by name, by the name searched for, in lower case. A failed search found no one.
    pub(crate) found: HashMap<String, Vec<Person>>,
    /// The name Slack is searching for, if any.
    pub(crate) finding: Option<String>,
}

/// A Followed Thread.
#[derive(Debug)]
pub(crate) struct Followed {
    /// Its Replies past its Read Marker, and the Mentions among them.
    pub(crate) unread: Unread,
    /// The Message that started it; `None` when it hasn't been seen.
    pub(crate) parent: Option<Message>,
}

/// What a Message's details offer to choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// One of the buttons an app put in it, to press.
    Press(slackers_model::Button),
    /// Somewhere it leads, to open or copy.
    Open(Link),
}

/// Somewhere a Message leads, to open in a browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) kind: LinkKind,
    /// What to call it.
    pub(crate) label: String,
    /// Where it goes: always an `http` or `https` URL.
    pub(crate) url: String,
}

/// What a [`Link`] leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// A web page linked in the Message's text.
    Web,
    /// A file shared with the Message, on Slack's web client.
    File,
    /// The Message itself, on Slack's web client.
    Message,
}

/// Something to complete the word being written with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) offered: Offered,
    /// What it's called: what an @-mention calls someone, a slash command's name, or an emoji's.
    pub(crate) name: String,
    /// More to tell it by, such as someone's full name, or another name an emoji goes by; empty when there's nothing
    /// more.
    pub(crate) detail: String,
}

/// What an [`Offer`] writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Offered {
    Mention(Mentioned),
    /// The emoji the offer names.
    Emoji,
    /// The slash command the offer names.
    Command,
}

impl Workspaces {
    /// The Workspaces with these `labels`, in this order, all signing in.
    pub(crate) fn new(labels: Vec<String>) -> Self {
        Self(
            labels
                .into_iter()
                .map(|label| SignIn {
                    label,
                    phase: Phase::SigningIn,
                })
                .collect(),
        )
    }

    /// How many there are, signed in to or not.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// Every Workspace, signed in to or not, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (WorkspaceKey, &SignIn)> {
        self.0
            .iter()
            .enumerate()
            .map(|(index, sign_in)| (WorkspaceKey(index), sign_in))
    }

    /// The Workspace at `key`, signed in to or not.
    pub(crate) fn sign_in(&self, key: WorkspaceKey) -> Option<&SignIn> {
        self.0.get(key.0)
    }

    /// The Workspace at `key`, once signed in to.
    pub(crate) fn get(&self, key: WorkspaceKey) -> Option<&Workspace> {
        match &self.sign_in(key)?.phase {
            Phase::Ready(workspace) => Some(workspace),
            Phase::SigningIn | Phase::Failed(_) => None,
        }
    }

    pub(crate) fn get_mut(&mut self, key: WorkspaceKey) -> Option<&mut Workspace> {
        match &mut self.0.get_mut(key.0)?.phase {
            Phase::Ready(workspace) => Some(workspace),
            Phase::SigningIn | Phase::Failed(_) => None,
        }
    }

    pub(crate) fn set_phase(&mut self, key: WorkspaceKey, phase: Phase) {
        if let Some(sign_in) = self.0.get_mut(key.0) {
            sign_in.phase = phase;
        }
    }

    /// `conversation` and its Workspace, once signed in to.
    pub(crate) fn conversation(
        &self,
        conversation: &ConversationRef,
    ) -> Option<(&Workspace, &Conversation)> {
        let workspace = self.get(conversation.workspace)?;
        Some((workspace, workspace.conversation(&conversation.id)?))
    }

    /// What's Unread in `conversation`, as far as known.
    pub(crate) fn unread(&self, conversation: &ConversationRef) -> Option<&Unread> {
        self.get(conversation.workspace)?
            .unread
            .get(&conversation.id)
    }

    /// The Followed Thread under `parent` in `conversation`, if the user follows it.
    pub(crate) fn followed(&self, conversation: &ConversationRef, parent: Ts) -> Option<&Followed> {
        self.get(conversation.workspace)?
            .followed
            .get(&(conversation.id.clone(), parent))
    }

    /// The Channel `channel` names, once its Workspace is signed in to and lists it.
    pub(crate) fn resolve(&self, channel: &ChannelRef) -> Option<ConversationRef> {
        let (key, _) = self
            .iter()
            .find(|(_, sign_in)| sign_in.label == channel.workspace)?;
        let found = self.get(key)?.conversations.iter().find(|c| {
            matches!(c.kind, ConversationKind::Channel { .. }) && c.name == channel.channel
        })?;
        Some(ConversationRef {
            workspace: key,
            id: found.id.clone(),
        })
    }

    /// The Channel at `conversation`, as the config names it: only a Channel can be in a Virtual Workspace.
    ///
    /// # Errors
    ///
    /// Says why `conversation` can't be: it's a DM, or isn't known.
    pub(crate) fn channel_ref(&self, conversation: &ConversationRef) -> Result<ChannelRef, String> {
        let (_, found) = self
            .conversation(conversation)
            .ok_or("open a channel first")?;
        match found.kind {
            ConversationKind::Channel { .. } => Ok(ChannelRef {
                workspace: self
                    .sign_in(conversation.workspace)
                    .ok_or("open a channel first")?
                    .label
                    .clone(),
                channel: found.name.clone(),
            }),
            ConversationKind::Dm { .. } | ConversationKind::GroupDm => {
                Err("DMs stay with their workspace: only channels join a virtual workspace".into())
            }
        }
    }
}

impl Workspace {
    /// The Workspace `snapshot` shows, with its Conversations in sidebar order: Channels by name, then DMs with the
    /// most recent first, as Slack's sidebar does by default.
    pub(crate) fn new(snapshot: Snapshot) -> Self {
        let followed = snapshot
            .followed_threads
            .into_iter()
            .map(|thread| {
                let key = (thread.conversation, thread.parent.ts);
                let followed = Followed {
                    unread: thread.unread,
                    parent: Some(thread.parent),
                };
                (key, followed)
            })
            .collect();
        let mut conversations = snapshot.conversations;
        conversations.sort_by(|(a, a_unread), (b, b_unread)| {
            is_dm(a).cmp(&is_dm(b)).then_with(|| {
                if is_dm(a) {
                    b_unread.latest().cmp(&a_unread.latest())
                } else {
                    a.name.cmp(&b.name)
                }
            })
        });
        let unread = conversations
            .iter()
            .map(|(c, a)| (c.id.clone(), a.clone()))
            .collect();
        Self {
            name: snapshot.name,
            icon: snapshot.icon,
            me: snapshot.me,
            connection: None,
            users: HashMap::new(),
            requested_users: HashSet::new(),
            bots: HashMap::new(),
            requested_bots: HashSet::new(),
            presence: HashMap::new(),
            presence_followed: Vec::new(),
            emojis: Emojis::default(),
            permalinks: snapshot.permalinks,
            conversations: conversations.into_iter().map(|(c, _)| c).collect(),
            unread,
            timelines: HashMap::new(),
            threads: HashMap::new(),
            loading: HashSet::new(),
            loading_threads: HashSet::new(),
            followed,
            commands: Vec::new(),
            found: HashMap::new(),
            finding: None,
        }
    }

    /// The Messages held at `place`: its Conversation's top-level ones, or its Thread's, as far as loaded.
    pub(crate) fn timeline(&self, place: &Place) -> Option<&Timeline> {
        let id = &place.conversation.id;
        match place.thread {
            Some(parent) => self.threads.get(&(id.clone(), parent)),
            None => self.timelines.get(id),
        }
    }

    /// Counts `reply`, posted under `parent` in `conversation`, on its Followed Thread. Replying to a Thread follows
    /// it, as in Slack; so does being mentioned in one here, so that the Mention isn't missed.
    pub(crate) fn count_reply(
        &mut self,
        conversation: &ConversationId,
        parent: Ts,
        reply: &Message,
    ) {
        let thread = (conversation.clone(), parent);
        let mine = reply.is_by(&self.me);
        let mentions_me = reply.mentions(&self.me);
        if !(self.followed.contains_key(&thread) || mine || mentions_me) {
            return;
        }
        let held = self.held_parent(conversation, parent);
        let followed = self.followed.entry(thread).or_insert_with(|| Followed {
            unread: Unread::default(),
            parent: held,
        });
        if mine {
            followed.unread.mark_read(reply.ts);
        } else {
            followed.unread.record(reply.ts, mentions_me);
        }
    }

    /// Follows the Thread under `parent` in `conversation`, read up to `read_up_to`. Its newest Reply is known only if
    /// its parent is held.
    pub(crate) fn follow(&mut self, conversation: ConversationId, parent: Ts, read_up_to: Ts) {
        let held = self.held_parent(&conversation, parent);
        let latest = held
            .as_ref()
            .and_then(|parent| parent.thread)
            .map_or(read_up_to, |summary| summary.latest.max(read_up_to));
        let followed = self
            .followed
            .entry((conversation, parent))
            .or_insert_with(|| Followed {
                unread: Unread::counted(read_up_to, latest, latest > read_up_to, 0),
                parent: held,
            });
        followed.unread.mark_read(read_up_to);
    }

    /// The Message at `parent` in `conversation`, if it's held.
    fn held_parent(&self, conversation: &ConversationId, parent: Ts) -> Option<Message> {
        self.timelines.get(conversation)?.get(parent).cloned()
    }

    /// Lists `conversation`, which just appeared, with its `unread`: a Channel among the Channels by name, a DM
    /// first among the DMs, as the most recently active. Returns whether it wasn't listed yet.
    pub(crate) fn join(&mut self, conversation: Conversation, unread: Unread) -> bool {
        if self.conversation(&conversation.id).is_some() {
            return false;
        }
        let at = if is_dm(&conversation) {
            self.conversations.iter().position(is_dm)
        } else {
            self.conversations
                .iter()
                .position(|listed| is_dm(listed) || listed.name > conversation.name)
        };
        self.unread.insert(conversation.id.clone(), unread);
        self.conversations
            .insert(at.unwrap_or(self.conversations.len()), conversation);
        true
    }

    /// The Conversation with `id`, if the Workspace lists it.
    pub(crate) fn conversation(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == *id)
    }

    /// What to call `user`: their name once looked up, else their id.
    pub(crate) fn user_name<'a>(&'a self, user: &'a UserId) -> &'a str {
        self.users
            .get(user)
            .map_or(user.as_str(), |user| user.name.as_str())
    }

    /// What `author` is called, as far as known: a bot not looked up yet is just "bot".
    pub(crate) fn author_name<'a>(&'a self, author: &'a Author) -> &'a str {
        match author {
            Author::User(id) => self.user_name(id),
            Author::Bot { name, .. } => name,
            Author::UnnamedBot(id) => self.bots.get(id).map_or("bot", |bot| bot.name.as_str()),
        }
    }

    /// Where `message`, posted in `conversation`, leads: the web pages its text links to, each once, then its files,
    /// then the Message itself. Only `http` and `https` links are among them, so that none starts a program the user
    /// didn't mean to.
    pub(crate) fn links(&self, conversation: &ConversationId, message: &Message) -> Vec<Link> {
        let mut links: Vec<Link> = Vec::new();
        for inline in message.content.inlines() {
            if let Inline::Link { url, label } = inline
                && !links.iter().any(|link| link.url == *url)
            {
                links.push(Link {
                    kind: LinkKind::Web,
                    label: label.clone().unwrap_or_else(|| url.clone()),
                    url: url.clone(),
                });
            }
        }
        links.extend(message.files.iter().filter_map(|file| {
            Some(Link {
                kind: LinkKind::File,
                label: file.name.clone(),
                url: file.link.clone()?,
            })
        }));
        // Slack doesn't keep an ephemeral Message, so there's no page for it.
        if !message.ephemeral {
            // The link to a Thread's parent or Reply opens the Thread.
            let thread = message.reply_to.or(message.thread.map(|_| message.ts));
            links.push(Link {
                kind: LinkKind::Message,
                label: "this message in Slack".to_owned(),
                url: self.permalinks.message(conversation, message.ts, thread),
            });
        }
        links.retain(|link| is_web(&link.url));
        links
    }

    /// What `message`'s details offer, in order: the buttons an app put in it, then its [`Workspace::links`].
    pub(crate) fn choices(&self, conversation: &ConversationId, message: &Message) -> Vec<Choice> {
        let buttons = message.content.blocks.iter().flat_map(|block| match block {
            Block::Actions(buttons) => buttons.as_slice(),
            Block::Paragraph(_)
            | Block::Quote(_)
            | Block::Preformatted(_)
            | Block::List { .. }
            | Block::Image { .. } => &[],
        });
        buttons
            .cloned()
            .map(Choice::Press)
            .chain(
                self.links(conversation, message)
                    .into_iter()
                    .map(Choice::Open),
            )
            .collect()
    }

    /// Where `author`'s avatar image is, as far as known.
    pub(crate) fn avatar<'a>(&'a self, author: &'a Author) -> Option<&'a str> {
        match author {
            Author::User(id) => self.users.get(id)?.avatar.as_deref(),
            Author::Bot { avatar, .. } => avatar.as_deref(),
            Author::UnnamedBot(id) => self.bots.get(id)?.avatar.as_deref(),
        }
    }

    /// A Draft holding `content` as one would write it: in Markdown, emoji by name, and people, broadcasts, user
    /// groups and Channels as labels standing for them. Images and buttons go: they aren't text.
    pub(crate) fn draft_of(&self, content: &RichText) -> Draft {
        let mut draft = Draft::default();
        for (index, block) in content.blocks.iter().enumerate() {
            if index > 0 {
                draft.push_text("\n");
            }
            match block {
                Block::Paragraph(inlines) => self.write_inlines(&mut draft, inlines, ""),
                Block::Quote(inlines) => {
                    draft.push_text("> ");
                    self.write_inlines(&mut draft, inlines, "> ");
                }
                Block::Preformatted(text) => draft.push_text(&format!("```\n{text}\n```")),
                Block::List { ordered, items } => {
                    for (number, item) in items.iter().enumerate() {
                        if number > 0 {
                            draft.push_text("\n");
                        }
                        let marker = if *ordered {
                            format!("{}. ", number + 1)
                        } else {
                            "• ".to_owned()
                        };
                        draft.push_text(&marker);
                        self.write_inlines(&mut draft, item, "");
                    }
                }
                Block::Image { .. } | Block::Actions(_) => {}
            }
        }
        draft
    }

    /// Writes `inlines` at the end of `draft`, starting each line after the first with `line_start`.
    fn write_inlines(&self, draft: &mut Draft, inlines: &[Inline], line_start: &str) {
        for inline in inlines {
            match inline {
                Inline::Text { text, style } => {
                    let styled = markdown::styled(text, *style);
                    draft.push_text(&styled.replace('\n', &format!("\n{line_start}")));
                }
                Inline::Emoji(emoji) => {
                    let tone = emoji
                        .skin_tone
                        .map(|tone| format!(":skin-tone-{tone}:"))
                        .unwrap_or_default();
                    draft.push_text(&format!(":{}:{tone}", emoji.name));
                }
                Inline::User(id) => {
                    let label = format!("@{}", self.user_name(id));
                    draft.push_reference(&label, Reference::Mention(Mentioned::Person(id.clone())));
                }
                Inline::Broadcast(broadcast) => {
                    draft.push_reference(
                        broadcast_name(*broadcast),
                        Reference::Mention(Mentioned::Broadcast(*broadcast)),
                    );
                }
                Inline::UserGroup(id) => {
                    let group = Reference::Mention(Mentioned::UserGroup(id.clone()));
                    draft.push_reference("@group", group);
                }
                Inline::Channel(id) => {
                    let name = self
                        .conversation(id)
                        .map_or(id.as_str(), |found| found.name.as_str());
                    draft.push_reference(&format!("#{name}"), Reference::Channel(id.clone()));
                }
                Inline::Link { url, label } => {
                    draft.push_text(&markdown::link(url, label.as_deref()));
                }
            }
        }
    }

    /// `text` as one line of plain words: people, Channels and emoji by name, links by their label.
    pub(crate) fn gist(&self, text: &RichText) -> String {
        let mut gist = String::new();
        for inline in text.inlines() {
            match inline {
                Inline::Text { text, .. } => gist.push_str(text),
                Inline::Emoji(emoji) => gist.push_str(&format!(":{}:", emoji.name)),
                Inline::User(id) => gist.push_str(&format!("@{}", self.user_name(id))),
                Inline::Channel(id) => {
                    let name = self
                        .conversation(id)
                        .map_or(id.as_str(), |c| c.name.as_str());
                    gist.push_str(&format!("#{name}"));
                }
                Inline::Broadcast(broadcast) => gist.push_str(broadcast_name(*broadcast)),
                Inline::UserGroup(_) => gist.push_str("@group"),
                Inline::Link { url, label } => gist.push_str(label.as_deref().unwrap_or(url)),
            }
        }
        gist.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// A notification of `message`, in `conversation`: who wrote it and where, and its gist. With `several`
    /// Workspaces signed in to, it names this one too.
    pub(crate) fn notification(
        &self,
        conversation: &ConversationId,
        message: &Message,
        several: bool,
    ) -> Effect {
        let author = self.author_name(&message.author).to_owned();
        let place = self
            .conversation(conversation)
            .and_then(|found| match &found.kind {
                ConversationKind::Channel { .. } => Some(format!("#{}", found.name)),
                ConversationKind::GroupDm => Some(self.conversation_name(found).to_owned()),
                ConversationKind::Dm { .. } => None,
            });
        let mut title = match (message.reply_to.is_some(), place) {
            (false, Some(place)) => format!("{author} in {place}"),
            (true, Some(place)) => format!("{author} in a thread in {place}"),
            (false, None) => author,
            (true, None) => format!("{author} in a thread"),
        };
        if several {
            title.push_str(&format!(" · {}", self.name));
        }
        let mut body = self.gist(&message.content);
        if body.is_empty() {
            let files: Vec<&str> = message
                .files
                .iter()
                .map(|file| file.name.as_str())
                .collect();
            body = files.join(", ");
        }
        // Within the 2048 bytes Ghostty takes.
        Effect::Notify {
            title: cut(&title, 256),
            body: cut(&body, 1024),
        }
    }

    /// What to call `conversation`: a DM after the other person, anything else by its name.
    pub(crate) fn conversation_name<'a>(&'a self, conversation: &'a Conversation) -> &'a str {
        match &conversation.kind {
            ConversationKind::Dm { with } => self.user_name(with),
            ConversationKind::Channel { .. } | ConversationKind::GroupDm => &conversation.name,
        }
    }

    /// The slash commands to offer for `name`, being written after a `/`, best first: those whose names it starts, then
    /// those it starts a later word of, each by name.
    pub(crate) fn suggest_commands(&self, name: &str) -> Vec<Offer> {
        let mut found: Vec<(bool, Offer)> = self
            .commands
            .iter()
            .filter_map(|command| {
                let bare = command.name.strip_prefix('/').unwrap_or(&command.name);
                let positions = search::word_start(bare, name)?;
                let detail = [command.usage.as_str(), command.description.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("  ");
                let later_word = positions.first().is_some_and(|&at| at > 0);
                Some((
                    later_word,
                    Offer {
                        offered: Offered::Command,
                        name: command.name.clone(),
                        detail,
                    },
                ))
            })
            .collect();
        // Stable: those equally good stay by name.
        found.sort_by_key(|(later_word, _)| *later_word);
        found
            .into_iter()
            .map(|(_, offer)| offer)
            .take(SUGGESTIONS)
            .collect()
    }

    /// Asks Slack, through this Workspace at `key`, whom it knows by `name`, unless it was asked already. One search
    /// at a time: the next waits for the answer, by which time the name has likely grown.
    pub(crate) fn find_people(&mut self, key: WorkspaceKey, name: &str, effects: &mut Effects) {
        let query = name.to_lowercase();
        if query.is_empty() || self.finding.is_some() || self.found.contains_key(&query) {
            return;
        }
        self.finding = Some(query.clone());
        effects.push(slack(key, Request::FindPeople { query }));
    }

    /// Whom to offer for an @-mention of `name` written at `place`, best first. First, those known here whose names
    /// `name` starts a word of, those it starts first: the people who wrote there most recently, then in a Channel
    /// its broadcasts, then everyone else. After them, whom Slack found by that name, which may be more loosely.
    pub(crate) fn suggest(&self, place: &Place, name: &str) -> Vec<Offer> {
        let conversation = &place.conversation.id;
        let writers = self
            .timeline(place)
            .into_iter()
            .flat_map(|timeline| timeline.messages().rev())
            .filter_map(|message| match &message.author {
                Author::User(id) => Some(id),
                Author::Bot { .. } | Author::UnnamedBot(_) => None,
            });
        let mut everyone: Vec<&slackers_slack::User> = self.users.values().collect();
        everyone.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let mut seen = HashSet::from([&self.me]);
        let mut known = |id: &UserId| -> Option<(Mentioned, String)> {
            let user = self.users.get(id)?;
            seen.insert(&user.id)
                .then(|| (Mentioned::Person(user.id.clone()), user.name.clone()))
        };
        let mut candidates: Vec<(Mentioned, String)> = writers.filter_map(&mut known).collect();
        let in_channel = self
            .conversation(conversation)
            .is_some_and(|c| matches!(c.kind, ConversationKind::Channel { .. }));
        if in_channel {
            candidates.extend([
                (Mentioned::Broadcast(Broadcast::Here), "here".to_owned()),
                (
                    Mentioned::Broadcast(Broadcast::Channel),
                    "channel".to_owned(),
                ),
            ]);
        }
        candidates.extend(everyone.iter().filter_map(|user| known(&user.id)));
        let mut offers: Vec<(bool, Offer)> = candidates
            .into_iter()
            .filter_map(|(mentioned, candidate)| {
                let positions = search::word_start(&candidate, name)?;
                let later_word = positions.first().is_some_and(|&at| at > 0);
                Some((
                    later_word,
                    Offer {
                        offered: Offered::Mention(mentioned),
                        name: candidate,
                        detail: String::new(),
                    },
                ))
            })
            .collect();
        // Stable: those equally good keep their order.
        offers.sort_by_key(|(later_word, _)| *later_word);
        let mut offers: Vec<Offer> = offers.into_iter().map(|(_, offer)| offer).collect();
        for person in self.found.get(&name.to_lowercase()).into_iter().flatten() {
            if person.id == self.me {
                continue;
            }
            let detail = if person.full_name.eq_ignore_ascii_case(&person.name) {
                String::new()
            } else {
                person.full_name.clone()
            };
            let offered = Offered::Mention(Mentioned::Person(person.id.clone()));
            match offers.iter_mut().find(|offer| offer.offered == offered) {
                // Known already: Slack can still tell them apart by their full name.
                Some(known) => known.detail = detail,
                None => offers.push(Offer {
                    offered,
                    name: person.name.clone(),
                    detail,
                }),
            }
        }
        offers.truncate(SUGGESTIONS);
        offers
    }

    /// Asks this Workspace, at `key`, for the names of the people `messages` name, and of the bots that wrote them
    /// without saying theirs, not known or asked for yet.
    pub(crate) fn want_names_in<'a>(
        &mut self,
        key: WorkspaceKey,
        messages: impl IntoIterator<Item = &'a Message>,
        effects: &mut Effects,
    ) {
        for message in messages {
            // A bot is known only once asked for.
            if let Author::UnnamedBot(bot) = &message.author
                && self.requested_bots.insert(bot.clone())
            {
                effects.push(slack(key, Request::Bot(bot.clone())));
            }
            self.want_users(key, people_in(message), effects);
        }
    }

    /// Whether everyone `message` names has a name here: its author, and the people it mentions.
    pub(crate) fn knows_names_in(&self, message: &Message) -> bool {
        let author = match &message.author {
            Author::UnnamedBot(bot) => self.bots.contains_key(bot),
            Author::User(_) | Author::Bot { .. } => true,
        };
        author
            && people_in(message)
                .iter()
                .all(|person| self.users.contains_key(person))
    }

    /// The people the user has DMs with, in sidebar order.
    pub(crate) fn dm_partners(&self) -> impl Iterator<Item = &UserId> {
        self.conversations.iter().filter_map(|c| match &c.kind {
            ConversationKind::Dm { with } => Some(with),
            ConversationKind::Channel { .. } | ConversationKind::GroupDm => None,
        })
    }

    /// Asks this Workspace, at `key`, to follow whether the people the user has DMs with are using Slack, unless
    /// it follows just them already.
    pub(crate) fn follow_presence(&mut self, key: WorkspaceKey, effects: &mut Effects) {
        let partners: Vec<UserId> = self.dm_partners().cloned().collect();
        if partners != self.presence_followed {
            self.presence_followed.clone_from(&partners);
            effects.push(slack(key, Request::FollowPresence(partners)));
        }
    }

    /// Whether `person` is using Slack right now; `None` when Slack hasn't said, as it only does of the people the
    /// user has DMs with.
    pub(crate) fn presence(&self, person: &UserId) -> Option<Presence> {
        self.presence.get(person).copied()
    }

    /// Asks this Workspace, at `key`, for the names of `people` not known or asked for yet.
    pub(crate) fn want_users(
        &mut self,
        key: WorkspaceKey,
        people: impl IntoIterator<Item = UserId>,
        effects: &mut Effects,
    ) {
        for person in people {
            if !self.users.contains_key(&person) && self.requested_users.insert(person.clone()) {
                effects.push(slack(key, Request::User(person)));
            }
        }
    }

    /// Adds a Message posted in `conversation` wherever it's held: its Timeline, which counts a Reply on its parent,
    /// and its Thread's.
    pub(crate) fn add(&mut self, conversation: &ConversationId, message: Message) {
        if let Some(parent) = message.reply_to
            && let Some(thread) = self.threads.get_mut(&(conversation.clone(), parent))
        {
            thread.insert(message.clone());
        }
        if let Some(timeline) = self.timelines.get_mut(conversation) {
            timeline.insert(message);
        }
    }

    /// `conversation`'s Timeline and those of its Threads, as far as they're held.
    pub(crate) fn timelines_of<'a>(
        &'a mut self,
        conversation: &'a ConversationId,
    ) -> impl Iterator<Item = &'a mut Timeline> {
        let threads = self
            .threads
            .iter_mut()
            .filter(move |((c, _), _)| c == conversation)
            .map(|(_, t)| t);
        self.timelines
            .get_mut(conversation)
            .into_iter()
            .chain(threads)
    }
}

/// How a broadcast is written.
pub(crate) fn broadcast_name(broadcast: Broadcast) -> &'static str {
    match broadcast {
        Broadcast::Here => "@here",
        Broadcast::Channel => "@channel",
        Broadcast::Everyone => "@everyone",
    }
}

/// `text`, cut at a character boundary to at most `bytes` bytes, ellipsis included, when it's longer.
fn cut(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let room = bytes.saturating_sub('…'.len_utf8());
    let end = (0..=room)
        .rev()
        .find(|&at| text.is_char_boundary(at))
        .unwrap_or(0);
    format!("{}…", &text[..end])
}

/// Whether `url` is an `http` or `https` link.
pub(crate) fn is_web(url: &str) -> bool {
    let scheme = url.split_once("://").map_or("", |(scheme, _)| scheme);
    scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
}

/// Whether `conversation` is a DM or a group DM, which the sidebar lists after the Channels.
pub(crate) fn is_dm(conversation: &Conversation) -> bool {
    !matches!(conversation.kind, ConversationKind::Channel { .. })
}

/// Everyone a Message names: its author and the people it mentions.
fn people_in(message: &Message) -> Vec<UserId> {
    let author = match &message.author {
        Author::User(id) => Some(id.clone()),
        Author::Bot { .. } | Author::UnnamedBot(_) => None,
    };
    let mentioned = message.content.inlines().filter_map(|inline| match inline {
        Inline::User(id) => Some(id.clone()),
        _ => None,
    });
    author.into_iter().chain(mentioned).collect()
}

/// A Slack error, as the command line puts it.
pub(crate) fn describe(error: &Error) -> String {
    match error {
        Error::SignedOut => {
            "signed out: copy a fresh d cookie from the browser into the keyring".to_owned()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_text_is_cut_at_a_character() {
        assert_eq!(cut("short", 10), "short");
        let long = cut(&"日本".repeat(1000), 1024);
        assert!(long.len() <= 1024 && long.ends_with('…'));
    }

    #[test]
    fn only_web_links_lead_anywhere() {
        assert!(is_web("https://example.com") && is_web("HTTP://example.com"));
        assert!(!is_web("file:///etc/passwd") && !is_web("slack://open"));
    }
}
