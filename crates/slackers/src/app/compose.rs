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
use wcgw_kit::key::{Code, Key, Mods};
use wcgw_kit::shell::{Composer, Suggestion, Word};

use super::workspace::{Offer, Offered, SUGGESTIONS};
use super::{Editing, Effect, Effects, Kit, Outgoing, Place, Slackers, Started, slack};
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
        .on_word(|app: &mut Slackers, _, buffer, word| app.written(buffer, word))
        .on_key(|app: &mut Slackers, kit, buffer, key| app.composer_key(kit, buffer, key))
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
            Completing::Mention(name) => workspace.suggest(place, name),
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

    /// The word before the cursor in `buffer`'s composer changed to `word`: for an @-mention, Slack is asked whom it
    /// knows by that name, unless it was asked already.
    fn written(&mut self, buffer: BufferId, word: &Word<'_>) -> Effects {
        let mut effects = Vec::new();
        let Some(key) = self
            .place_of(buffer)
            .map(|place| place.conversation.workspace)
        else {
            return effects;
        };
        self.mentioning = match Completing::of(word) {
            Some(Completing::Mention(name)) => Some((key, name)),
            _ => None,
        };
        if let Some((key, name)) = &self.mentioning
            && let Some(workspace) = self.workspaces.get_mut(*key)
        {
            workspace.find_people(*key, name, &mut effects);
        }
        effects
    }

    /// A key typed in `buffer`'s composer that slackers takes before the composer does: `<Up>` in an empty one
    /// edits the user's newest Message there, as in Slack; `<Esc>` gives an edit up, back to writing what was set
    /// aside when the edit started there; `<CR>` on an emptied edit says why nothing happens.
    fn composer_key(&mut self, kit: &mut Kit, buffer: BufferId, key: Key) -> Option<Effects> {
        let place = self.place_of(buffer)?.clone();
        let composer = kit.composer(buffer)?;
        let (empty, blank) = (composer.is_empty(), composer.text().trim().is_empty());
        let menu = !composer.suggestions().is_empty();
        let editing = self.editing.get(&place).map(|editing| editing.started);
        match (key.code, key.mods == Mods::NONE, editing) {
            (Code::Up, true, None) if empty => {
                let ts = self.newest_of_mine(&place)?;
                let (workspace, message) = self.message(&place, ts)?;
                let draft = workspace.draft_of(&message.content);
                self.write_edit(
                    kit,
                    &place,
                    ts,
                    &draft.text,
                    draft.references,
                    Started::InComposer,
                );
                Some(Vec::new())
            }
            // With suggestions showing, `<Esc>` closes them first.
            (Code::Esc, _, Some(started)) if !menu => {
                self.stop_editing(kit, &place);
                match started {
                    Started::InComposer => Some(Vec::new()),
                    Started::OnMessage => None,
                }
            }
            (Code::Enter, true, Some(_)) if blank && !menu => {
                kit.error("an edit can't be empty: <Esc> gives it up");
                Some(Vec::new())
            }
            _ => None,
        }
    }

    /// The user's newest Message at `place`, if there's one.
    fn newest_of_mine(&self, place: &Place) -> Option<Ts> {
        let workspace = self.workspaces.get(place.conversation.workspace)?;
        workspace
            .timeline(place)?
            .messages()
            .rev()
            .find(|message| message.is_by(&workspace.me))
            .map(|message| message.ts)
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
            // As in Slack, on to the newest, which the window then follows to what's sent.
            let end = super::navigate::cursor_at_end(kit, buffer);
            let window = kit.current_window();
            kit.set_cursor(window, end);
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
    /// there until the edit is saved, or given up with `<Esc>`.
    pub(super) fn amend(&mut self, kit: &mut Kit) -> Result<Effects, String> {
        let (place, ts) = Self::message_under_cursor(kit)?;
        let (workspace, message) = self
            .message(&place, ts)
            .ok_or("no message under the cursor")?;
        if !message.is_by(&workspace.me) {
            return Err("only your own messages can be edited".to_owned());
        }
        let draft = workspace.draft_of(&message.content);
        self.write_edit(
            kit,
            &place,
            ts,
            &draft.text,
            draft.references,
            Started::OnMessage,
        );
        kit.run_ex(self, "startinsert")
    }

    /// Puts `text`, an edit of the Message at `ts`, in `place`'s composer, with what its labels stand for, setting
    /// aside what's being written there; giving it up goes back where it `started`.
    pub(super) fn write_edit(
        &mut self,
        kit: &mut Kit,
        place: &Place,
        ts: Ts,
        text: &str,
        references: References,
        started: Started,
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
        self.editing.insert(
            place.clone(),
            Editing {
                ts,
                set_aside,
                started,
            },
        );
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

    /// After a key: the current composer's border says whether it holds an edit, and whom it @-mentions.
    pub(super) fn keep_composers(&mut self, kit: &mut Kit) -> Effects {
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

    /// The Messages, edits and slash commands on their way to Slack.
    pub(super) fn sending(&self) -> usize {
        self.outbox.len() + self.edits_sent.len() + self.commands_sent.len()
    }

    /// Quits once nothing is on its way to Slack any more, if `:qa` asked to; the kit frees the pictures the
    /// terminal holds.
    pub(super) fn quit_when_sent(&self) -> Effects {
        if !self.quitting || self.sending() > 0 {
            return Vec::new();
        }
        vec![Effect::Quit]
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
