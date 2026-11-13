//! What slackers does with what its Workspaces' Sessions tell it.

use std::cmp::Reverse;
use std::collections::HashSet;

use slackers_model::{
    Author, ConversationId, ConversationKind, Happened, Inline, MessageRef, ReactionChange,
    Timeline, UserId,
};
use slackers_slack::{EmojiList, LiveEvent, Snapshot, Update};

use super::workspace::describe;
use super::{
    ConversationRef, Effect, Effects, Following, HELD_NOTIFICATIONS, Kit, Phase, Place, Slackers,
    Workspace, WorkspaceKey, slack,
};
use crate::draft::References;
use crate::sidebar::{Item, Section};
use crate::typing::TYPING_FOR;

impl Slackers {
    pub(super) fn slack(&mut self, kit: &mut Kit, key: WorkspaceKey, update: Update) -> Effects {
        let mut effects = Vec::new();
        match update {
            Update::Ready(snapshot) => effects.extend(self.ready(kit, key, *snapshot)),
            Update::SignInFailed(error) => {
                self.workspaces
                    .set_phase(key, Phase::Failed(error.to_string()));
            }
            Update::Commands(offered) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.commands = offered;
                }
            }
            Update::RanCommand { id, result } => {
                let Some(at) = self.commands_sent.iter().position(|(sent, ..)| *sent == id) else {
                    return effects;
                };
                let (_, place, written) = self.commands_sent.remove(at);
                match result {
                    Ok(Some(reply)) => {
                        if let Some(workspace) = self.workspaces.get(key) {
                            kit.info(workspace.gist(&reply));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if self
                            .following
                            .as_ref()
                            .is_some_and(|following| following.from == place.conversation)
                        {
                            self.following = None;
                        }
                        let command = written
                            .command()
                            .map(|(command, _)| command)
                            .unwrap_or_default();
                        kit.error(format!(
                            "couldn't run {command}, back in the draft: {}",
                            describe(&error)
                        ));
                        self.put_back(kit, &place, written);
                    }
                }
            }
            Update::Edited {
                conversation,
                ts,
                result,
            } => {
                let sent = self.edits_sent.iter().position(|(place, sent, _)| {
                    place.conversation.workspace == key
                        && place.conversation.id == conversation
                        && *sent == ts
                });
                let Some((place, ts, written)) = sent.map(|at| self.edits_sent.remove(at)) else {
                    return effects;
                };
                match result {
                    Ok(content) => {
                        if let Some(workspace) = self.workspaces.get_mut(key) {
                            for timeline in workspace.timelines_of(&conversation) {
                                timeline.revise(ts, content.clone());
                            }
                        }
                    }
                    Err(error) => {
                        kit.error(format!(
                            "not edited, back in the composer: {}",
                            describe(&error)
                        ));
                        let mut references = References::default();
                        let text = written.text.clone();
                        references.put_back(written);
                        self.write_edit(kit, &place, ts, &text, references);
                    }
                }
            }
            Update::Reacted {
                conversation,
                ts,
                emoji,
                change,
                result: Err(error),
            } => {
                // Undo what was shown.
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    let me = workspace.me.clone();
                    let undo = match change {
                        ReactionChange::Added => ReactionChange::Removed,
                        ReactionChange::Removed => ReactionChange::Added,
                    };
                    for timeline in workspace.timelines_of(&conversation) {
                        timeline.react(ts, &emoji, &me, undo);
                    }
                }
                let what = match change {
                    ReactionChange::Added => "react with",
                    ReactionChange::Removed => "take back",
                };
                kit.error(format!(
                    "couldn't {what} :{}:: {}",
                    emoji.name,
                    describe(&error)
                ));
            }
            Update::Reacted { result: Ok(()), .. } => {}
            Update::Pressed {
                button,
                result: Err(error),
                ..
            } => {
                kit.error(format!(
                    "couldn't press {}: {}",
                    button.label,
                    describe(&error)
                ));
            }
            // What pressing it did arrives live.
            Update::Pressed { result: Ok(()), .. } => {}
            Update::Activity { cursor, result } => {
                let next = match result {
                    Ok(page) => {
                        let people: Vec<UserId> =
                            page.items
                                .iter()
                                .filter_map(|item| match &item.happened {
                                    Happened::Mention { author, .. }
                                    | Happened::Added { author } => Some(author.clone()),
                                    Happened::Reaction { user, .. } => Some(user.clone()),
                                    Happened::Replies { .. } | Happened::Dm => None,
                                })
                                .collect();
                        if let Some(workspace) = self.workspaces.get_mut(key) {
                            workspace.want_users(key, people, &mut effects);
                        }
                        self.activity.received(key, cursor, page)
                    }
                    Err(error) => {
                        tracing::warn!(%error, "couldn't load activity");
                        self.activity.failed(key)
                    }
                };
                effects.extend(next.map(|request| slack(key, request)));
            }
            Update::Message {
                conversation,
                ts,
                result,
            } => {
                let message = result.unwrap_or_else(|error| {
                    tracing::warn!(%error, "couldn't fetch a message");
                    None
                });
                if let Some(message) = &message
                    && let Some(workspace) = self.workspaces.get_mut(key)
                {
                    workspace.want_names_in(key, [message], &mut effects);
                }
                self.activity.fetched(key, conversation, ts, message);
            }
            Update::Emoji(EmojiList { custom, standard }) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.emojis.set(custom, standard);
                }
            }
            Update::EmojiUse(used) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.emojis.set_used(used);
                }
            }
            Update::History {
                conversation,
                result,
            } => {
                let Some(workspace) = self.workspaces.get_mut(key) else {
                    return effects;
                };
                workspace.loading.remove(&conversation);
                match result {
                    Ok(page) => {
                        workspace.want_names_in(key, &page.messages, &mut effects);
                        let timeline = workspace.timelines.entry(conversation.clone()).or_default();
                        timeline.add_history(page.messages, page.has_more);
                    }
                    Err(error) => kit.error(describe(&error)),
                }
                let conversation = ConversationRef {
                    workspace: key,
                    id: conversation,
                };
                effects.extend(self.reach(&conversation));
            }
            Update::Replies {
                conversation,
                parent,
                result,
            } => {
                let Some(workspace) = self.workspaces.get_mut(key) else {
                    return effects;
                };
                let thread = (conversation, parent);
                workspace.loading_threads.remove(&thread);
                match result {
                    Ok(messages) => {
                        workspace.want_names_in(key, &messages, &mut effects);
                        workspace
                            .threads
                            .entry(thread)
                            .or_insert_with(|| Timeline::thread(parent))
                            .add_history(messages, false);
                    }
                    Err(error) => kit.error(describe(&error)),
                }
            }
            Update::Posted {
                id,
                conversation,
                result,
            } => {
                let Some(at) = self.outbox.iter().position(|outgoing| outgoing.id == id) else {
                    return effects;
                };
                let outgoing = self.outbox.remove(at);
                match result {
                    Ok(message) => {
                        let Some(workspace) = self.workspaces.get_mut(key) else {
                            return effects;
                        };
                        // Slack moves the Read Marker past the user's own Message itself.
                        match message.reply_to {
                            Some(parent) => workspace.count_reply(&conversation, parent, &message),
                            None => {
                                if let Some(unread) = workspace.unread.get_mut(&conversation) {
                                    unread.mark_read(message.ts);
                                }
                            }
                        }
                        // It may @-mention people found by name, whose names are known only to the search.
                        workspace.want_names_in(key, [&message], &mut effects);
                        workspace.add(&conversation, message);
                    }
                    Err(error) => {
                        kit.error(format!("not sent, back in the draft: {}", describe(&error)));
                        self.put_back(kit, &outgoing.place, outgoing.written);
                    }
                }
            }
            Update::User(user) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.users.insert(user.id.clone(), user);
                }
                effects.extend(self.notify_held(key));
            }
            Update::Bot(bot) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.bots.insert(bot.id.clone(), bot);
                }
                effects.extend(self.notify_held(key));
            }
            Update::Reactions {
                conversation,
                ts,
                result,
            } => match result {
                Ok(reactions) => {
                    if let Some(workspace) = self.workspaces.get_mut(key) {
                        for timeline in workspace.timelines_of(&conversation) {
                            timeline.set_reactions(ts, reactions.clone());
                        }
                    }
                }
                Err(error) => {
                    kit.error(format!(
                        "couldn't list everyone who reacted: {}",
                        describe(&error)
                    ));
                }
            },
            Update::FoundPeople { query, result } => {
                let Some(workspace) = self.workspaces.get_mut(key) else {
                    return effects;
                };
                workspace.finding = None;
                // Counted as finding no one, so as not to ask again with every key.
                let people = result.unwrap_or_else(|error| {
                    tracing::warn!(%error, "couldn't search for people");
                    Vec::new()
                });
                workspace.found.insert(query, people);
                kit.refresh_completion(self);
                // The name may have grown while Slack searched.
                effects.extend(self.find_wanted_people());
            }
            Update::Live(event) => effects.extend(self.live(kit, key, event)),
            Update::Connection(connection) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.connection = Some(connection);
                }
            }
        }
        effects
    }

    fn ready(&mut self, kit: &mut Kit, key: WorkspaceKey, snapshot: Snapshot) -> Effects {
        let mut effects = Vec::new();
        let mut workspace = Workspace::new(snapshot);
        let partners: Vec<UserId> = workspace.dm_partners().cloned().collect();
        workspace.want_users(key, partners, &mut effects);
        workspace.follow_presence(key, &mut effects);
        self.workspaces
            .set_phase(key, Phase::Ready(Box::new(workspace)));

        let Some(label) = self.workspaces.sign_in(key).map(|s| s.label.as_str()) else {
            return effects;
        };
        let missing = self
            .virtual_workspaces
            .iter()
            .flat_map(|virtual_workspace| {
                virtual_workspace
                    .channels
                    .iter()
                    .filter(|channel| channel.workspace == label)
                    .filter(|channel| self.workspaces.resolve(channel).is_none())
                    .map(move |channel| (&virtual_workspace.name, channel))
            });
        for (name, channel) in missing {
            tracing::warn!(virtual_workspace = %name, %channel, "not among the user's channels");
            kit.warn(format!(
                "{name} lists {channel}, which isn't among your channels"
            ));
        }

        self.list_sidebar();
        effects.extend(self.refresh_activity(key));
        effects
    }

    fn live(&mut self, kit: &mut Kit, key: WorkspaceKey, event: LiveEvent) -> Effects {
        let mut effects = Vec::new();
        let focused = self.focused;
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return effects;
        };
        let place = |conversation: &ConversationId, thread| Place {
            conversation: ConversationRef {
                workspace: key,
                id: conversation.clone(),
            },
            thread,
        };
        let mut joined = false;
        // Where a slash command the user ran posted for them.
        let mut landed = None;
        // Whether it may add to the user's Activity, or change it.
        let mut touches_activity = false;
        // A Message to notify the user of.
        let mut notify = None;
        match event {
            LiveEvent::Joined {
                conversation,
                unread,
            } => {
                let partner = match &conversation.kind {
                    ConversationKind::Dm { with } => Some(with.clone()),
                    ConversationKind::Channel { .. } | ConversationKind::GroupDm => None,
                };
                joined = workspace.join(conversation, unread);
                if joined {
                    workspace.want_users(key, partner, &mut effects);
                    workspace.follow_presence(key, &mut effects);
                }
                // Someone added the user to a Channel, or wrote to them.
                touches_activity = joined;
            }
            LiveEvent::Posted {
                conversation,
                message,
            } => {
                if message.is_by(&workspace.me)
                    && self
                        .following
                        .as_ref()
                        .is_some_and(|following| following.led_to(key, &conversation, message.ts))
                {
                    landed = Some(MessageRef {
                        conversation: conversation.clone(),
                        ts: message.ts,
                        thread: message.reply_to,
                    });
                }
                workspace.want_names_in(key, [&message], &mut effects);
                if let Author::User(author) = &message.author {
                    self.typing
                        .posted(&place(&conversation, message.reply_to), author);
                }
                let kind = workspace.conversation(&conversation).map(|c| &c.kind);
                let is_dm = matches!(kind, Some(ConversationKind::Dm { .. }));
                let in_followed = message.reply_to.is_some_and(|parent| {
                    workspace
                        .followed
                        .contains_key(&(conversation.clone(), parent))
                });
                let to_everyone = message
                    .content
                    .inlines()
                    .any(|inline| matches!(inline, Inline::Broadcast(_)));
                let by_someone_else = !message.is_by(&workspace.me);
                let in_dm = is_dm || matches!(kind, Some(ConversationKind::GroupDm));
                let mentions_me = message.mentions(&workspace.me);
                touches_activity =
                    by_someone_else && (in_dm || in_followed || to_everyone || mentions_me);
                // As Slack notifies by default: of DMs, Mentions and Replies in Followed Threads.
                if by_someone_else && (in_dm || in_followed || mentions_me) && !focused {
                    notify = Some((conversation.clone(), message.clone()));
                }
                if let Some(parent) = message.reply_to {
                    workspace.count_reply(&conversation, parent, &message);
                } else if !message.is_by(&workspace.me)
                    && let Some(unread) = workspace.unread.get_mut(&conversation)
                {
                    // Every DM Message counts as a Mention, as Slack's badges do.
                    unread.record(message.ts, is_dm || message.mentions(&workspace.me));
                }
                workspace.add(&conversation, message);
            }
            LiveEvent::Edited {
                conversation,
                message,
            } => {
                if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                    timeline.replace(message.clone());
                }
                let thread = (conversation, message.reply_to.unwrap_or(message.ts));
                if let Some(thread) = workspace.threads.get_mut(&thread) {
                    thread.replace(message);
                }
            }
            LiveEvent::Deleted { conversation, ts } => {
                for timeline in workspace.timelines_of(&conversation) {
                    timeline.remove(ts);
                }
            }
            LiveEvent::Reacted {
                conversation,
                ts,
                emoji,
                user,
                author,
                change,
            } => {
                touches_activity = change == ReactionChange::Added
                    && user != workspace.me
                    && author.as_ref() == Some(&workspace.me);
                for timeline in workspace.timelines_of(&conversation) {
                    timeline.react(ts, &emoji, &user, change);
                }
            }
            LiveEvent::Presence { people, presence } => {
                for person in people {
                    workspace.presence.insert(person, presence);
                }
            }
            LiveEvent::Typing {
                conversation,
                thread,
                user,
            } => {
                // The user's own, from another client, is no news to them.
                if user != workspace.me {
                    workspace.want_users(key, [user.clone()], &mut effects);
                    let place = place(&conversation, thread);
                    let heard = self.typing.heard(place.clone(), user.clone());
                    effects.push(Effect::After {
                        after: TYPING_FOR,
                        msg: super::Msg::StoppedTyping { place, user, heard },
                    });
                }
            }
            LiveEvent::Marked {
                conversation,
                up_to,
            } => {
                // Slack may now see the user's Activity there as read.
                touches_activity = self.activity.has_unread_in(key, &conversation);
                if let Some(unread) = workspace.unread.get_mut(&conversation) {
                    unread.mark_read(up_to);
                }
            }
            LiveEvent::ThreadFollowed {
                conversation,
                parent,
                read_up_to,
            } => workspace.follow(conversation, parent, read_up_to),
            LiveEvent::ThreadUnfollowed {
                conversation,
                parent,
            } => {
                workspace.followed.remove(&(conversation, parent));
            }
            LiveEvent::ThreadMarked {
                conversation,
                parent,
                up_to,
            } => {
                touches_activity = self.activity.has_unread_in(key, &conversation);
                if let Some(followed) = workspace.followed.get_mut(&(conversation, parent)) {
                    followed.unread.mark_read(up_to);
                }
            }
        }
        if joined {
            self.list_sidebar();
        }
        if let Some(at) = landed {
            self.following = None;
            let there = ConversationRef {
                workspace: key,
                id: at.conversation.clone(),
            };
            if self.shown_conversation(kit).as_ref() != Some(&there) {
                match self.go_to(kit, key, at) {
                    Ok(more) => effects.extend(more),
                    Err(error) => kit.error(error),
                }
            }
        }
        if touches_activity {
            effects.extend(self.refresh_activity(key));
        }
        if let Some((conversation, message)) = notify {
            self.held_notifications.push((key, conversation, message));
            if self.held_notifications.len() > HELD_NOTIFICATIONS {
                self.held_notifications.remove(0);
            }
            effects.extend(self.notify_held(key));
        }
        effects
    }

    /// Notifies of the held Messages in the Workspace at `key` whose people all have names now, and keeps holding the
    /// others.
    fn notify_held(&mut self, key: WorkspaceKey) -> Effects {
        let mut effects = Vec::new();
        let Some(workspace) = self.workspaces.get(key) else {
            return effects;
        };
        let several = self.workspaces.len() > 1;
        self.held_notifications
            .retain(|(held, conversation, message)| {
                if *held != key || !workspace.knows_names_in(message) {
                    return true;
                }
                effects.push(workspace.notification(conversation, message, several));
                false
            });
        effects
    }

    /// Asks the Workspace at `key` for its newest Activity.
    pub(super) fn refresh_activity(&mut self, key: WorkspaceKey) -> Effects {
        self.activity
            .refresh(key)
            .map(|request| slack(key, request))
            .into_iter()
            .collect()
    }

    /// Asks every Workspace signed in to for its newest Activity.
    pub(super) fn refresh_activity_everywhere(&mut self) -> Effects {
        let ready: Vec<WorkspaceKey> = self
            .workspaces
            .iter()
            .filter(|(_, sign_in)| matches!(sign_in.phase, Phase::Ready(_)))
            .map(|(key, _)| key)
            .collect();
        ready
            .into_iter()
            .flat_map(|key| self.refresh_activity(key))
            .collect()
    }

    /// Lists the sidebar's sections afresh: the Followed Threads with Replies to read, then each Virtual Workspace
    /// with its Channels, in the config's order, then each Workspace with its Conversations in no Virtual Workspace.
    pub(super) fn list_sidebar(&mut self) {
        let mut in_virtual_workspaces = HashSet::new();
        let threads = self
            .sidebar
            .sections()
            .iter()
            .find(|(section, _)| *section == Section::Threads)
            .map(|(_, items)| items.clone())
            .unwrap_or_default();
        let mut sections = vec![(Section::Threads, threads)];
        for virtual_workspace in &self.virtual_workspaces {
            let channels: Vec<ConversationRef> = virtual_workspace
                .channels
                .iter()
                .filter_map(|channel| self.workspaces.resolve(channel))
                .collect();
            in_virtual_workspaces.extend(channels.iter().cloned());
            let section = Section::VirtualWorkspace(virtual_workspace.name.clone());
            sections.push((
                section,
                channels.into_iter().map(Item::Conversation).collect(),
            ));
        }
        for (key, sign_in) in self.workspaces.iter() {
            let conversations = match &sign_in.phase {
                Phase::Ready(workspace) => workspace
                    .conversations
                    .iter()
                    .map(|c| ConversationRef {
                        workspace: key,
                        id: c.id.clone(),
                    })
                    .filter(|c| !in_virtual_workspaces.contains(c))
                    .map(Item::Conversation)
                    .collect(),
                Phase::SigningIn | Phase::Failed(_) => Vec::new(),
            };
            sections.push((Section::Workspace(key), conversations));
        }
        self.sidebar.list(sections);
    }

    /// The Followed Threads the Threads section lists: those with Replies to read, and those shown, so that one
    /// doesn't go as soon as it's read; the newest Reply first.
    pub(super) fn followed_items(&self, kit: &Kit) -> Vec<Item> {
        let shown: HashSet<&Place> = kit
            .window_ids()
            .into_iter()
            .filter_map(|window| self.place_of(kit.window_buffer(window)?))
            .collect();
        let mut threads = Vec::new();
        for (key, sign_in) in self.workspaces.iter() {
            let Phase::Ready(workspace) = &sign_in.phase else {
                continue;
            };
            for ((id, parent), followed) in &workspace.followed {
                let conversation = ConversationRef {
                    workspace: key,
                    id: id.clone(),
                };
                let place = Place {
                    conversation: conversation.clone(),
                    thread: Some(*parent),
                };
                let is_shown = shown.contains(&place);
                if (followed.unread.has_unread() || is_shown)
                    && workspace.conversation(id).is_some()
                {
                    threads.push((followed.unread.latest(), key.0, *parent, conversation));
                }
            }
        }
        // Newest first; the rest only makes the order the same every time.
        threads.sort_by_cached_key(|(latest, index, parent, conversation)| {
            (
                Reverse(*latest),
                *index,
                *parent,
                conversation.id.to_string(),
            )
        });
        threads
            .into_iter()
            .map(|(_, _, parent, conversation)| Item::Thread {
                conversation,
                parent,
            })
            .collect()
    }

    /// Starts following the slash command just run at `place`: what it posts elsewhere soon, slackers goes to.
    pub(super) fn follow_command(&mut self, place: &Place) {
        self.following = Some(Following {
            from: place.conversation.clone(),
            at: super::now(),
        });
    }
}
