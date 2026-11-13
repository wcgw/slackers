//! Writing (the kit's D4): each Conversation and Thread has a composer docked under its buffer, which the kit edits.
//! slackers completes the word being written (`@` people and broadcasts, `:` emoji, `/` slash commands as the first
//! word), keeps what each picked label stands for, and posts what's sent, or runs it as a slash command, or saves
//! it as the edit of one of the user's Messages.

use std::collections::HashSet;

use slackers_model::{Broadcast, Emoji, ReactionChange, Ts, UserId};
use slackers_slack::Request;
use unicode_width::UnicodeWidthStr as _;
use wcgw_kit::BufferId;
use wcgw_kit::hl::Group;
use wcgw_kit::shell::{Composer, Suggestion, Word};

use super::workspace::{Offer, Offered, SUGGESTIONS};
use super::{Editing, Effects, Kit, Outgoing, Place, Slackers, slack};
use crate::draft::{Completing, Mentioned, Reference, References, Written};
use crate::emoji::Drawable;
use crate::hl::DIM;
use crate::icons;

/// The room an emoji gets in a menu of suggestions, a space after it included.
const EMOJI_CELLS: usize = 3;

/// The composer of `place`'s buffer: always shown under it, as a chat's is.
pub(super) fn composer(place: &Place) -> Composer<Slackers> {
    let placeholder = if place.thread.is_some() {
        "reply"
    } else {
        "message"
    };
    Composer::new(placeholder)
        .always_shown()
        .on_send(|app: &mut Slackers, kit, buffer, text| app.send(kit, buffer, text))
        .completing(
            |app: &Slackers, kit, buffer, word| app.complete(kit, buffer, word),
            |app: &mut Slackers, kit, buffer, chosen| app.accepted(kit, buffer, chosen),
        )
}

/// What a suggestion is, as the composer hands it back once chosen.
fn suggestion_key(offered: &Offered, name: &str) -> String {
    match offered {
        Offered::Mention(Mentioned::Person(id)) => format!("person {id}"),
        Offered::Mention(Mentioned::Broadcast(broadcast)) => {
            format!("broadcast {}", broadcast_word(*broadcast))
        }
        Offered::Mention(Mentioned::UserGroup(id)) => format!("group {id}"),
        Offered::Emoji => format!("emoji {name}"),
        Offered::Command => format!("command {name}"),
    }
}

fn broadcast_word(broadcast: Broadcast) -> &'static str {
    match broadcast {
        Broadcast::Here => "here",
        Broadcast::Channel => "channel",
        Broadcast::Everyone => "everyone",
    }
}

impl Slackers {
    /// What the word before the cursor in `buffer`'s composer may be completed to, best first; and for an
    /// @-mention, whom to have Slack find by that name once the key is handled.
    fn complete(&self, kit: &Kit, buffer: BufferId, word: &Word<'_>) -> Vec<Suggestion> {
        let Some(place) = self.place_of(buffer) else {
            return Vec::new();
        };
        let Some(completing) = Completing::of(word) else {
            return Vec::new();
        };
        let key = place.conversation.workspace;
        let Some(workspace) = self.workspaces.get(key) else {
            return Vec::new();
        };
        let references = self.references.get(place).cloned().unwrap_or_default();
        let offers: Vec<Offer> = match &completing {
            Completing::Mention(name) => {
                self.wanted_people.replace(Some((key, name.clone())));
                workspace.suggest(place, name)
            }
            Completing::Command(name) => workspace.suggest_commands(name),
            Completing::Emoji(name) => workspace
                .emojis
                .complete(name, SUGGESTIONS)
                .into_iter()
                .map(|found| Offer {
                    offered: Offered::Emoji,
                    name: found.name,
                    detail: found.alias.to_owned(),
                })
                .collect(),
        };
        offers
            .into_iter()
            .map(|offer| {
                let mut label = Vec::new();
                let insert = match &offer.offered {
                    Offered::Mention(mentioned) => {
                        let icon = match mentioned {
                            Mentioned::Person(_) => icons::AVATAR,
                            Mentioned::Broadcast(_) => icons::BROADCAST,
                            Mentioned::UserGroup(_) => icons::GROUP_DM,
                        };
                        label.push((kit.icon(icon), crate::hl::ACCENT));
                        let reference = Reference::Mention(mentioned.clone());
                        references.label_for(&format!("@{}", offer.name), &reference)
                    }
                    Offered::Command => {
                        label.push((kit.icon(icons::COMMAND), crate::hl::ACCENT));
                        offer.name.clone()
                    }
                    Offered::Emoji => {
                        let shown =
                            match workspace.emojis.drawable(&Emoji::named(offer.name.clone())) {
                                Drawable::Text(text) => text.to_owned(),
                                Drawable::Picture(_) | Drawable::Name => String::new(),
                            };
                        let pad = EMOJI_CELLS.saturating_sub(shown.width()).max(1);
                        label.push((format!("{shown}{}", " ".repeat(pad)), Group::NORMAL));
                        format!(":{}:", offer.name)
                    }
                };
                label.push((offer.name.clone(), Group::NORMAL));
                if !offer.detail.is_empty() {
                    label.push((format!("  {}", offer.detail), DIM));
                }
                Suggestion {
                    label,
                    insert: format!("{insert} "),
                    key: suggestion_key(&offer.offered, &offer.name),
                }
            })
            .collect()
    }

    /// `chosen` was put in place in `buffer`'s composer: an @-mention's label now stands for whom it names, and an
    /// emoji counts as used once more.
    fn accepted(&mut self, kit: &mut Kit, buffer: BufferId, chosen: &Suggestion) -> Effects {
        let Some(place) = self.place_of(buffer).cloned() else {
            return Vec::new();
        };
        let label = chosen.insert.trim_end().to_owned();
        let (kind, what) = chosen.key.split_once(' ').unwrap_or((&chosen.key, ""));
        let mentioned = match kind {
            "person" => Some(Mentioned::Person(UserId::new(what))),
            "broadcast" => Some(Mentioned::Broadcast(match what {
                "here" => Broadcast::Here,
                "channel" => Broadcast::Channel,
                _ => Broadcast::Everyone,
            })),
            "group" => Some(Mentioned::UserGroup(what.to_owned())),
            "emoji" => {
                if let Some(workspace) = self.workspaces.get_mut(place.conversation.workspace) {
                    workspace.emojis.note_use(what);
                }
                None
            }
            _ => None,
        };
        if let Some(mentioned) = mentioned {
            self.references
                .entry(place.clone())
                .or_default()
                .hold(label, Reference::Mention(mentioned));
        }
        self.title_composer(kit, buffer, &place);
        Vec::new()
    }

    /// What sending `text` from `buffer`'s composer does: saves the edit being written there, runs the slash
    /// command it starts with, or posts it.
    fn send(&mut self, kit: &mut Kit, buffer: BufferId, text: String) -> Effects {
        let Some(place) = self.place_of(buffer).cloned() else {
            return Vec::new();
        };
        let written = self.references.entry(place.clone()).or_default().take(text);
        let mut effects = if self.editing.contains_key(&place) {
            self.save_edit(kit, &place, written)
        } else {
            self.post(kit, &place, written)
        };
        self.title_composer(kit, buffer, &place);
        // Drawn apart from the key, so that a window reading at the end follows what was sent.
        effects.push(super::Effect::After {
            after: std::time::Duration::ZERO,
            msg: super::Msg::Refresh,
        });
        effects
    }

    /// Posts `written` at `place`, or runs the slash command it starts with.
    fn post(&mut self, kit: &mut Kit, place: &Place, written: Written) -> Effects {
        let id = self.next_post;
        self.next_post += 1;
        let key = place.conversation.workspace;
        if let Some((command, text)) = written.command() {
            let known = self
                .workspaces
                .get(key)
                .map(|workspace| &workspace.commands);
            // Until Slack lists them, let it say whether there's such a command.
            if known.is_some_and(|known| {
                !known.is_empty() && !known.iter().any(|offered| offered.name == command)
            }) {
                kit.error(format!(
                    "no {command} command here: start with a space to post it as text"
                ));
                self.put_back(kit, place, written);
                return Vec::new();
            }
            let request = Request::RunCommand {
                conversation: place.conversation.id.clone(),
                thread: place.thread,
                command,
                text,
                id,
            };
            self.follow_command(place);
            self.commands_sent.push((id, place.clone(), written));
            return vec![slack(key, request)];
        }
        let request = Request::Post {
            conversation: place.conversation.id.clone(),
            thread: place.thread,
            text: written.parts(),
            id,
        };
        self.outbox.push(Outgoing {
            id,
            place: place.clone(),
            written,
        });
        vec![slack(key, request)]
    }

    /// Puts `written`, which couldn't be sent, back in `place`'s composer, ahead of anything written since.
    pub(super) fn put_back(&mut self, kit: &mut Kit, place: &Place, written: Written) {
        let buffer = self.place_buffer(kit, place);
        let Some(composer) = kit.composer_mut(buffer) else {
            return;
        };
        let since = composer.text();
        let text = if since.is_empty() {
            written.text.clone()
        } else {
            format!("{}\n{since}", written.text)
        };
        composer.set_text(&text);
        self.references
            .entry(place.clone())
            .or_default()
            .put_back(written);
        self.title_composer(kit, buffer, place);
    }

    /// `:amend` (`A`): edits the user's Message under the cursor in its composer, setting aside what's written
    /// there until the edit is saved, or given up by emptying the composer.
    pub(super) fn amend(&mut self, kit: &mut Kit) -> Result<Effects, String> {
        let (place, ts) = Self::message_under_cursor(kit)?;
        let (workspace, message) = self
            .message(&place, ts)
            .ok_or("no message under the cursor")?;
        if !message.is_by(&workspace.me) {
            return Err("only your own messages can be edited".to_owned());
        }
        let draft = workspace.draft_of(&message.content);
        self.write_edit(kit, &place, ts, &draft.text, draft.references);
        kit.run_ex(self, "startinsert")
    }

    /// Puts `text`, an edit of the Message at `ts`, in `place`'s composer, with what its labels stand for, setting
    /// aside what's being written there.
    pub(super) fn write_edit(
        &mut self,
        kit: &mut Kit,
        place: &Place,
        ts: Ts,
        text: &str,
        references: References,
    ) {
        let buffer = self.place_buffer(kit, place);
        let Some(composer) = kit.composer_mut(buffer) else {
            return;
        };
        let before = composer.text();
        composer.set_text(text);
        let before = (before, self.references.insert(place.clone(), references));
        // Editing another Message there drops that edit, but keeps what was set aside for it.
        let set_aside = match self.editing.remove(place) {
            Some(earlier) => earlier.set_aside,
            None => (!before.0.is_empty()).then(|| (before.0, before.1.unwrap_or_default())),
        };
        self.editing
            .insert(place.clone(), Editing { ts, set_aside });
        self.title_composer(kit, buffer, place);
    }

    /// Saves `written` as the edit at `place`, bringing back what was set aside for it.
    fn save_edit(&mut self, kit: &mut Kit, place: &Place, written: Written) -> Effects {
        let Some(editing) = self.editing.get(place) else {
            return Vec::new();
        };
        let ts = editing.ts;
        self.stop_editing(kit, place);
        let request = Request::Edit {
            conversation: place.conversation.id.clone(),
            ts,
            text: written.parts(),
        };
        self.edits_sent.push((place.clone(), ts, written));
        vec![slack(place.conversation.workspace, request)]
    }

    /// Ends the edit at `place`, bringing back what was set aside for it.
    fn stop_editing(&mut self, kit: &mut Kit, place: &Place) {
        let Some(editing) = self.editing.remove(place) else {
            return;
        };
        let (text, references) = editing.set_aside.unwrap_or_default();
        self.references.insert(place.clone(), references);
        let buffer = self.place_buffer(kit, place);
        if let Some(composer) = kit.composer_mut(buffer) {
            composer.set_text(&text);
        }
        self.title_composer(kit, buffer, place);
    }

    /// After a key: an edit whose composer was emptied is given up, and each composer's border says whether it
    /// holds an edit and whom it @-mentions.
    pub(super) fn keep_composers(&mut self, kit: &mut Kit) -> Effects {
        let given_up: Vec<Place> = self
            .editing
            .keys()
            .filter(|place| {
                self.places
                    .get(place)
                    .and_then(|&buffer| kit.composer(buffer))
                    .is_some_and(|composer| composer.is_empty())
            })
            .cloned()
            .collect();
        for place in given_up {
            self.stop_editing(kit, &place);
        }
        let buffer = kit.current_buffer();
        if let Some(place) = self.place_of(buffer).cloned() {
            self.title_composer(kit, buffer, &place);
        }
        Vec::new()
    }

    /// Says on the border of `buffer`'s composer, which writes at `place`, whether it holds an edit, and whom its
    /// text @-mentions.
    fn title_composer(&self, kit: &mut Kit, buffer: BufferId, place: &Place) {
        let Some(composer) = kit.composer_mut(buffer) else {
            return;
        };
        let text = composer.text();
        let mentions = self
            .references
            .get(place)
            .map(|references| references.mentions(&text).join(" · "))
            .unwrap_or_default();
        let title = match (self.editing.contains_key(place), mentions.is_empty()) {
            (true, true) => Some("editing".to_owned()),
            (true, false) => Some(format!("editing · {mentions}")),
            (false, true) => None,
            (false, false) => Some(mentions),
        };
        composer.set_title(title);
    }

    /// Asks Slack whom it knows by the name completion last saw, unless it was asked already.
    pub(super) fn find_wanted_people(&mut self) -> Effects {
        let mut effects = Vec::new();
        if let Some((key, name)) = self.wanted_people.take()
            && let Some(workspace) = self.workspaces.get_mut(key)
        {
            workspace.find_people(key, &name, &mut effects);
        }
        effects
    }

    /// Reacts to the Message at `ts`, held at `place`, with the emoji `name`; or, when the user has reacted with it
    /// already, in any skin tone, takes that back. Shows it straight away, and undoes it should Slack refuse.
    pub(super) fn react_with(&mut self, place: &Place, ts: Ts, name: &str) -> Effects {
        let Some((workspace, message)) = self.message(place, ts) else {
            return Vec::new();
        };
        let mine = message
            .reactions
            .iter()
            .find(|reaction| reaction.emoji.name == name && reaction.users.contains(&workspace.me))
            .map(|reaction| reaction.emoji.clone());
        let (emoji, change) = match mine {
            Some(emoji) => (emoji, ReactionChange::Removed),
            None => (Emoji::named(name), ReactionChange::Added),
        };
        let key = place.conversation.workspace;
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return Vec::new();
        };
        let me = workspace.me.clone();
        for timeline in workspace.timelines_of(&place.conversation.id) {
            timeline.react(ts, &emoji, &me, change);
        }
        if change == ReactionChange::Added {
            workspace.emojis.note_use(name);
        }
        let request = Request::React {
            conversation: place.conversation.id.clone(),
            ts,
            emoji,
            change,
        };
        vec![slack(key, request)]
    }

    /// The emoji the user reacted to the Message at `ts`, held at `place`, with.
    pub(super) fn reacted(&self, place: &Place, ts: Ts) -> HashSet<String> {
        self.message(place, ts)
            .map(|(workspace, message)| {
                message
                    .reactions
                    .iter()
                    .filter(|reaction| reaction.users.contains(&workspace.me))
                    .map(|reaction| reaction.emoji.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}
