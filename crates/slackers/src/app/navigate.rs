//! Opening what the user asks for, in the windows slackers lays out (the kit's D11): the sidebar on the left; a
//! Conversation, Activity or the sign-in page in the main window; a Thread beside its Conversation where there's
//! room, else in its place. And keeping up as the user reads: loading older history at the top, and marking what's
//! read at the bottom.

use jiff::Timestamp;
use slackers_model::{ConversationKind, MessageRef, Timeline, Ts, Unread};
use slackers_slack::Request;
use wcgw_kit::BufferId;
use wcgw_kit::doc::{BlockKey, Cursor, Document};
use wcgw_kit::shell::{Placement, Size, WindowId};

use super::{
    ConversationRef, Effect, Effects, JUMP_PAGES, Kit, Place, SIDEBAR_WIDTH, SPLIT_AT, Shows,
    Slackers, Target, WorkspaceKey, compose, slack,
};
use crate::activity::Fetched;

/// The key of the block where what's new starts, which a Conversation or Thread with something unread opens on.
pub(super) const NEW: &str = "new";

impl Slackers {
    /// Opens what `target` refers to.
    pub(super) fn open_target(&mut self, kit: &mut Kit, target: Target) -> Result<Effects, String> {
        match target {
            Target::Activity => Ok(self.open_activity(kit)),
            Target::Conversation(conversation) => {
                Ok(self.open_conversation(kit, &conversation, None))
            }
            Target::Thread {
                conversation,
                parent,
            } => {
                let mut effects = self.open_conversation(kit, &conversation, None);
                let thread = Place {
                    conversation,
                    thread: Some(parent),
                };
                effects.extend(self.open_thread(kit, &thread, None));
                Ok(effects)
            }
            Target::ActivityItem(workspace, key) => self.open_activity_item(kit, workspace, &key),
            // Any Message can start a Thread: one without Replies yet opens empty, ready for one.
            Target::Message(
                Place {
                    conversation,
                    thread: None,
                },
                ts,
            ) => {
                let thread = Place {
                    conversation,
                    thread: Some(ts),
                };
                Ok(self.open_thread(kit, &thread, None))
            }
            Target::Message(..) => Ok(Vec::new()),
            Target::Url(url) => Ok(vec![Effect::OpenUrl(url)]),
            Target::Person(key, person) => {
                let workspace = self.workspaces.get(key).ok_or("not signed in there")?;
                let dm = workspace
                    .conversations
                    .iter()
                    .find(|conversation| {
                        matches!(&conversation.kind, ConversationKind::Dm { with } if *with == person)
                    })
                    .ok_or_else(|| format!("no DM with {}", workspace.user_name(&person)))?;
                let conversation = ConversationRef {
                    workspace: key,
                    id: dm.id.clone(),
                };
                Ok(self.open_conversation(kit, &conversation, None))
            }
            Target::React { place, ts, emoji } => Ok(self.react_with(&place, ts, &emoji)),
            Target::Press { place, ts, button } => {
                let request = Request::Press {
                    conversation: place.conversation.id.clone(),
                    ts,
                    button,
                };
                Ok(vec![slack(place.conversation.workspace, request)])
            }
        }
    }

    /// What `place`'s buffer is called: a Channel `#name`, a DM `@name`, a Thread its Conversation's and the time
    /// it started. With a namesake in another Workspace, the Workspace's label says which.
    pub(crate) fn buffer_name(&self, place: &Place) -> String {
        let conversation = &place.conversation;
        let Some((workspace, found)) = self.workspaces.conversation(conversation) else {
            return conversation.id.to_string();
        };
        let name = conversation_label(workspace.conversation_name(found), &found.kind);
        let namesake = self.workspaces.iter().any(|(key, sign_in)| {
            key != conversation.workspace
                && match &sign_in.phase {
                    super::Phase::Ready(other) => other.conversations.iter().any(|listed| {
                        conversation_label(other.conversation_name(listed), &listed.kind) == name
                    }),
                    _ => false,
                }
        });
        let name = match self.workspaces.sign_in(conversation.workspace) {
            Some(sign_in) if namesake => format!("{name} ({})", sign_in.label),
            _ => name,
        };
        match place.thread {
            Some(parent) => format!("{name} › {}", super::render::time(parent, &self.zone)),
            None => name,
        }
    }

    /// The buffer showing `place`, made, with its composer, if it isn't yet.
    pub(super) fn place_buffer(&mut self, kit: &mut Kit, place: &Place) -> BufferId {
        if let Some(&buffer) = self.places.get(place) {
            return buffer;
        }
        let buffer = kit.add_buffer(self.buffer_name(place), Document::new(), true);
        kit.set_composer(buffer, compose::composer(place));
        self.shows.insert(buffer, Shows::Place(place.clone()));
        self.places.insert(place.clone(), buffer);
        buffer
    }

    /// Asks for what `place` shows, unless it's loaded or on its way; `again` asks for its newest Messages anyway.
    pub(super) fn load(&mut self, place: &Place, again: bool) -> Effects {
        let key = place.conversation.workspace;
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return Vec::new();
        };
        let id = &place.conversation.id;
        let request = match place.thread {
            None => ((again || !workspace.timelines.contains_key(id))
                && workspace.loading.insert(id.clone()))
            .then(|| Request::History {
                conversation: id.clone(),
                before: None,
            }),
            Some(parent) => {
                let thread = (id.clone(), parent);
                ((again || !workspace.threads.contains_key(&thread))
                    && workspace.loading_threads.insert(thread))
                .then(|| Request::Replies {
                    conversation: id.clone(),
                    parent,
                })
            }
        };
        request
            .map(|request| slack(key, request))
            .into_iter()
            .collect()
    }

    /// Opens `conversation` in the main window: at the Message `at` when given, loading older history as far as
    /// [`JUMP_PAGES`] pages to reach it; else on its first unread Message when it has any, the same way; else where
    /// the window left it, or at its newest. A Thread of another Conversation beside it goes.
    pub(super) fn open_conversation(
        &mut self,
        kit: &mut Kit,
        conversation: &ConversationRef,
        at: Option<Ts>,
    ) -> Effects {
        self.kept_unread = None;
        let place = Place::of(conversation);
        let unread = self
            .workspaces
            .unread(conversation)
            .filter(|unread| unread.has_unread())
            .map(Unread::read_up_to);
        match unread {
            Some(read_up_to) => self.new_after.insert(place.clone(), read_up_to),
            None => self.new_after.remove(&place),
        };
        let landing = match (at, unread) {
            (Some(ts), _) => {
                self.reaching.insert(conversation.clone(), (ts, JUMP_PAGES));
                Some(ts.to_string())
            }
            (None, Some(read_up_to)) => {
                self.reaching
                    .insert(conversation.clone(), (read_up_to, JUMP_PAGES));
                Some(NEW.to_owned())
            }
            (None, None) => {
                self.reaching.remove(conversation);
                None
            }
        };
        let buffer = self.place_buffer(kit, &place);
        if let Some(window) = self.thread_window(kit)
            && kit
                .window_buffer(window)
                .and_then(|shown| self.place_of(shown))
                .is_none_or(|thread| thread.conversation != *conversation)
        {
            kit.close_window_id(window);
            self.thread_window = None;
        }
        let main = self.main_window(kit);
        kit.focus_window(main);
        self.show_here(kit, buffer, landing, true);
        let mut effects = self.load(&place, false);
        effects.extend(self.reach(conversation));
        effects
    }

    /// Opens the Thread at `place`: beside its Conversation where there's room, else in its place; at the Reply
    /// `at` when given, else on its first unread Reply when the user follows it and there's one.
    pub(super) fn open_thread(&mut self, kit: &mut Kit, place: &Place, at: Option<Ts>) -> Effects {
        let Some(parent) = place.thread else {
            return Vec::new();
        };
        let unread = self
            .workspaces
            .followed(&place.conversation, parent)
            .map(|followed| &followed.unread)
            .filter(|unread| unread.has_unread())
            .map(Unread::read_up_to);
        match unread {
            Some(read_up_to) => self.new_after.insert(place.clone(), read_up_to),
            None => self.new_after.remove(place),
        };
        let landing = at
            .map(|ts| ts.to_string())
            .or_else(|| unread.map(|_| NEW.to_owned()));
        let buffer = self.place_buffer(kit, place);
        if self.room(kit) >= SPLIT_AT {
            match self.thread_window(kit) {
                Some(window) => {
                    kit.focus_window(window);
                }
                None => {
                    let main = self.main_window(kit);
                    kit.focus_window(main);
                    // Opened on another buffer first, so that the Thread's cursor starts where it should.
                    let window = kit.open_window(
                        Placement::Right,
                        Size::Cells(self.thread_width(kit)),
                        self.sign_in_buffer,
                    );
                    self.thread_window = Some(window);
                }
            }
        } else {
            let main = self.main_window(kit);
            kit.focus_window(main);
        }
        self.show_here(kit, buffer, landing, true);
        self.load(place, false)
    }

    /// Opens Activity in the main window, and asks every Workspace for what's new in it.
    pub(super) fn open_activity(&mut self, kit: &mut Kit) -> Effects {
        let buffer = match self.activity_buffer {
            Some(buffer) => buffer,
            None => {
                let buffer = kit.add_buffer("activity", Document::new(), true);
                self.shows.insert(buffer, Shows::Activity);
                self.activity_buffer = Some(buffer);
                buffer
            }
        };
        if let Some(window) = self.thread_window(kit) {
            kit.close_window_id(window);
            self.thread_window = None;
        }
        let main = self.main_window(kit);
        kit.focus_window(main);
        self.show_here(kit, buffer, None, false);
        self.refresh_activity_everywhere()
    }

    /// Opens the Message the Activity item with `key`, from the Workspace at `workspace`, is about, as
    /// [`Slackers::go_to`] does.
    fn open_activity_item(
        &mut self,
        kit: &mut Kit,
        workspace: WorkspaceKey,
        key: &str,
    ) -> Result<Effects, String> {
        let entry = self
            .activity
            .entry(workspace, key)
            .ok_or("that item isn't in Activity any more")?;
        let mut at = entry.item.message.clone();
        at.thread = at.thread.or(match self.activity_message(workspace, &at) {
            Fetched::Got(message) => message.reply_to,
            Fetched::Unasked | Fetched::Fetching | Fetched::Unavailable => None,
        });
        self.go_to(kit, workspace, at)
    }

    /// The Message at `at` in the Workspace at `key`: as loaded in its Conversation or Thread, else as fetched for
    /// Activity.
    pub(super) fn activity_message(&self, key: WorkspaceKey, at: &MessageRef) -> Fetched<'_> {
        let loaded = self.workspaces.get(key).and_then(|workspace| {
            let place = Place {
                conversation: ConversationRef {
                    workspace: key,
                    id: at.conversation.clone(),
                },
                thread: at.thread,
            };
            workspace.timeline(&place)?.get(at.ts)
        });
        loaded.map_or_else(|| self.activity.message(key, at), Fetched::Got)
    }

    /// Opens the Message at `at` in the Workspace at `key`, the cursor on it: in its Thread if it's a Reply, else in
    /// its Conversation, loading older history as far as [`JUMP_PAGES`] pages to reach it. A window that showed it
    /// before goes back where it left it: the kit lets an application place a cursor only in a window new to the
    /// buffer.
    pub(super) fn go_to(
        &mut self,
        kit: &mut Kit,
        key: WorkspaceKey,
        at: MessageRef,
    ) -> Result<Effects, String> {
        let conversation = ConversationRef {
            workspace: key,
            id: at.conversation,
        };
        if self.workspaces.conversation(&conversation).is_none() {
            return Err("that conversation isn't in your sidebar".to_owned());
        }
        Ok(match at.thread {
            Some(parent) => {
                let mut effects = self.open_conversation(kit, &conversation, None);
                let thread = Place {
                    conversation,
                    thread: Some(parent),
                };
                effects.extend(self.open_thread(kit, &thread, Some(at.ts)));
                effects
            }
            None => self.open_conversation(kit, &conversation, Some(at.ts)),
        })
    }

    /// Shows `buffer` in the current window. With `landing`, the cursor starts on the block of that key (when the
    /// window never showed the buffer), and the window follows the buffer's end only once it's there; else, for a
    /// buffer that `follows` its end, as a chat does, it starts and stays at the end.
    fn show_here(
        &mut self,
        kit: &mut Kit,
        buffer: BufferId,
        landing: Option<String>,
        follows: bool,
    ) {
        match landing {
            Some(key) => {
                kit.buffer_mut(buffer).set_follows_end(false);
                self.landing.insert(buffer, key.clone());
                kit.show_buffer_at(buffer, Cursor::on_block(BlockKey::new(key)));
            }
            None => {
                if !self.landing.contains_key(&buffer) {
                    kit.buffer_mut(buffer).set_follows_end(follows);
                }
                kit.show_buffer(buffer);
            }
        }
    }

    /// The sidebar's window, while it's open.
    pub(super) fn sidebar_window(&self, kit: &Kit) -> Option<WindowId> {
        self.sidebar_window
            .filter(|&window| kit.window_buffer(window).is_some())
    }

    /// The window beside the Conversation that shows its Thread, while it's open.
    pub(super) fn thread_window(&self, kit: &Kit) -> Option<WindowId> {
        self.thread_window
            .filter(|&window| kit.window_buffer(window).is_some())
    }

    /// The window Conversations open in: the one it was, unless the user closed it; else the Thread's, or any but
    /// the sidebar's; else a new one beside the sidebar.
    pub(super) fn main_window(&mut self, kit: &mut Kit) -> WindowId {
        let sidebar = self.sidebar_window(kit);
        if kit.window_buffer(self.main_window).is_some() && Some(self.main_window) != sidebar {
            return self.main_window;
        }
        let window = match self.thread_window(kit) {
            Some(thread) => {
                self.thread_window = None;
                thread
            }
            None => match kit
                .window_ids()
                .into_iter()
                .find(|&window| Some(window) != sidebar)
            {
                Some(window) => window,
                None => {
                    if let Some(sidebar) = sidebar {
                        kit.focus_window(sidebar);
                    }
                    kit.open_window(Placement::Right, Size::Shared, self.sign_in_buffer)
                }
            },
        };
        self.main_window = window;
        window
    }

    /// `:sidebar` (`\`): hides the sidebar from within it, goes to it from anywhere else, and brings it back.
    pub(super) fn toggle_sidebar(&mut self, kit: &mut Kit) {
        match self.sidebar_window(kit) {
            Some(window) if kit.current_window() == window => {
                if kit.close_window_id(window) {
                    self.sidebar_window = None;
                }
            }
            Some(window) => {
                kit.focus_window(window);
            }
            None => {
                let window = kit.open_window(
                    Placement::FarLeft,
                    Size::Cells(SIDEBAR_WIDTH),
                    self.sidebar_buffer,
                );
                self.sidebar_window = Some(window);
            }
        }
        self.reshape(kit);
    }

    /// Lays the windows out for the room there is now: a Thread beside its Conversation where there's room for
    /// both, else instead of it.
    pub(super) fn reshape(&mut self, kit: &mut Kit) {
        let wide = self.room(kit) >= SPLIT_AT;
        match self.thread_window(kit) {
            Some(window) if !wide => {
                let Some(thread) = kit.window_buffer(window) else {
                    return;
                };
                let was_current = kit.current_window() == window;
                kit.close_window_id(window);
                self.thread_window = None;
                let main = self.main_window(kit);
                kit.show_buffer_in(main, thread);
                if was_current {
                    kit.focus_window(main);
                }
            }
            Some(window) => kit.set_window_size(window, Size::Cells(self.thread_width(kit))),
            None if wide => {
                let main = self.main_window(kit);
                let Some(shown) = kit.window_buffer(main) else {
                    return;
                };
                let Some(Place {
                    conversation,
                    thread: Some(_),
                }) = self.place_of(shown).cloned()
                else {
                    return;
                };
                let conversation = self.place_buffer(kit, &Place::of(&conversation));
                let current = kit.current_window();
                kit.focus_window(main);
                kit.show_buffer(conversation);
                let window =
                    kit.open_window(Placement::Right, Size::Cells(self.thread_width(kit)), shown);
                self.thread_window = Some(window);
                if current != main {
                    kit.focus_window(current);
                }
            }
            None => {}
        }
    }

    /// The cells beside the sidebar.
    pub(super) fn room(&self, kit: &Kit) -> u16 {
        let (width, _) = kit.size();
        let sidebar = self.sidebar_window(kit).map_or(0, |_| SIDEBAR_WIDTH + 1);
        width.saturating_sub(sidebar)
    }

    /// The width of a Thread's window beside its Conversation's.
    pub(super) fn thread_width(&self, kit: &Kit) -> u16 {
        (self.room(kit) * 45 / 100).saturating_sub(1).max(20)
    }

    /// The width of the text of the window showing `buffer`, as slackers lays its windows out: what Messages are
    /// wrapped to.
    pub(super) fn width_of(&self, kit: &Kit, buffer: BufferId) -> u16 {
        let room = self.room(kit);
        match self.thread_window(kit) {
            Some(window) if kit.window_buffer(window) == Some(buffer) => self.thread_width(kit),
            Some(_) => room.saturating_sub(self.thread_width(kit) + 1),
            None => room,
        }
    }

    /// The Conversation the main window shows, or whose Thread it shows.
    pub(super) fn shown_conversation(&self, kit: &Kit) -> Option<ConversationRef> {
        let buffer = kit.window_buffer(self.main_window)?;
        Some(self.place_of(buffer)?.conversation.clone())
    }

    /// The Places shown in windows, in window order, each once.
    pub(super) fn shown_places(&self, kit: &Kit) -> Vec<Place> {
        let mut places: Vec<Place> = Vec::new();
        for window in kit.window_ids() {
            if let Some(place) = kit
                .window_buffer(window)
                .and_then(|buffer| self.place_of(buffer))
                && !places.contains(place)
            {
                places.push(place.clone());
            }
        }
        places
    }

    /// The Message under the cursor, where it's held.
    pub(super) fn message_under_cursor(kit: &Kit) -> Result<(Place, Ts), String> {
        match kit.targets_under_cursor() {
            [_, Some(Target::Message(place, ts))] => Ok((place.clone(), *ts)),
            _ => Err("no message under the cursor".to_owned()),
        }
    }

    /// Asks for the page before the oldest Message of `conversation`, if there is one and it isn't on its way.
    pub(super) fn load_older(&mut self, conversation: &ConversationRef) -> Effects {
        let Some(workspace) = self.workspaces.get_mut(conversation.workspace) else {
            return Vec::new();
        };
        let Some(timeline) = workspace.timelines.get(&conversation.id) else {
            return Vec::new();
        };
        if !(timeline.has_older() && workspace.loading.insert(conversation.id.clone())) {
            return Vec::new();
        }
        let request = Request::History {
            conversation: conversation.id.clone(),
            before: timeline.oldest(),
        };
        vec![slack(conversation.workspace, request)]
    }

    /// Loads `conversation`'s older history while the Message it opened on is older than what's loaded, as far as
    /// [`JUMP_PAGES`] pages.
    pub(super) fn reach(&mut self, conversation: &ConversationRef) -> Effects {
        let Some(&(ts, pages)) = self.reaching.get(conversation) else {
            return Vec::new();
        };
        let Some(workspace) = self.workspaces.get(conversation.workspace) else {
            return Vec::new();
        };
        let Some(timeline) = workspace.timelines.get(&conversation.id) else {
            // The first page is on its way.
            return Vec::new();
        };
        if workspace.loading.contains(&conversation.id) {
            return Vec::new();
        }
        let beyond = timeline.oldest().is_some_and(|oldest| ts < oldest);
        if beyond && timeline.has_older() && pages > 0 {
            self.reaching.insert(conversation.clone(), (ts, pages - 1));
            self.load_older(conversation)
        } else {
            self.reaching.remove(conversation);
            Vec::new()
        }
    }

    /// Loads older history of the Conversation in the current window when the window shows its top.
    pub(super) fn load_older_at_top(&mut self, kit: &Kit) -> Effects {
        let buffer = kit.current_buffer();
        let Some(Place {
            conversation,
            thread: None,
        }) = self.place_of(buffer).cloned()
        else {
            return Vec::new();
        };
        if kit.top_row(buffer) == Some(0) {
            self.load_older(&conversation)
        } else {
            Vec::new()
        }
    }

    /// Loads older Activity when the cursor nears the oldest item loaded.
    pub(super) fn load_older_activity(&mut self, kit: &Kit) -> Effects {
        if self.activity_buffer != Some(kit.current_buffer()) || !kit.cursor_near_end() {
            return Vec::new();
        }
        self.activity
            .load_older()
            .into_iter()
            .map(|(key, request)| slack(key, request))
            .collect()
    }

    /// The Places shown in a window whose cursor is on their newest Message, as far as loaded: those being read.
    pub(super) fn reading_newest(&self, kit: &mut Kit) -> Vec<Place> {
        let current = kit.current_window();
        let mut reading = Vec::new();
        for window in kit.window_ids() {
            let Some(place) = kit
                .window_buffer(window)
                .and_then(|buffer| self.place_of(buffer))
                .cloned()
            else {
                continue;
            };
            kit.focus_window(window);
            let on = match kit.targets_under_cursor() {
                [_, Some(Target::Message(_, ts))] => Some(*ts),
                _ => None,
            };
            let newest = self
                .workspaces
                .get(place.conversation.workspace)
                .and_then(|workspace| workspace.timeline(&place))
                .and_then(Timeline::newest);
            if on.is_some() && on == newest && !reading.contains(&place) {
                reading.push(place);
            }
        }
        kit.focus_window(current);
        reading
    }

    /// Moves the Read Marker of each of `reading`, Places the user reads at their newest Message, to their newest,
    /// while the terminal has focus: in a Conversation, unless the user marked it unread; in a Thread, if they
    /// follow it.
    pub(super) fn mark_read(&mut self, reading: &[Place]) -> Effects {
        let mut effects = Vec::new();
        if !self.focused {
            return effects;
        }
        for place in reading {
            let conversation = &place.conversation;
            if place.thread.is_none() && self.kept_unread.as_ref() == Some(conversation) {
                continue;
            }
            let Some(workspace) = self.workspaces.get_mut(conversation.workspace) else {
                continue;
            };
            let Some(newest) = workspace.timeline(place).and_then(Timeline::newest) else {
                continue;
            };
            let id = conversation.id.clone();
            let request = match place.thread {
                None => {
                    let Some(unread) = workspace.unread.get_mut(&id) else {
                        continue;
                    };
                    if newest <= unread.read_up_to() {
                        continue;
                    }
                    unread.mark_read(newest);
                    Request::Mark {
                        conversation: id,
                        up_to: newest,
                    }
                }
                Some(parent) => {
                    let Some(followed) = workspace.followed.get_mut(&(id.clone(), parent)) else {
                        continue;
                    };
                    if newest <= followed.unread.read_up_to() {
                        continue;
                    }
                    followed.unread.mark_read(newest);
                    Request::MarkThread {
                        conversation: id,
                        parent,
                        up_to: newest,
                    }
                }
            };
            effects.push(slack(conversation.workspace, request));
        }
        effects
    }
}

/// What a Conversation called `name` is called in a buffer's name: `#name` for a Channel, `@name` for a DM.
fn conversation_label(name: &str, kind: &ConversationKind) -> String {
    match kind {
        ConversationKind::Channel { .. } => format!("#{name}"),
        ConversationKind::Dm { .. } => format!("@{name}"),
        ConversationKind::GroupDm => name.to_owned(),
    }
}

/// When `ts` was, from `now`: the time of day if it's today, else the day, and the year if it's another.
pub(super) fn when(ts: Ts, zone: &jiff::tz::TimeZone, now: Timestamp) -> String {
    let (Some(then), today) = (super::render::zoned(ts, zone), now.to_zoned(zone.clone())) else {
        return String::new();
    };
    let format = if then.date() == today.date() {
        "%H:%M"
    } else if then.year() == today.year() {
        "%d %b"
    } else {
        "%d %b %Y"
    };
    then.strftime(format).to_string()
}
