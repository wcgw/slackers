//! slackers' ex commands, default keys, pickers and statusline segments (the kit's D12, D13): every action on a
//! Message has a command, so `:map` can reach anything a key does.

use slackers_model::ConversationKind;
use slackers_slack::Connection;
use wcgw_kit::hl::Group;
use wcgw_kit::keymap::{Mode, Topic};
use wcgw_kit::shell::{Choice as Pick, Command, Invocation, Pick as Picked, Picker, Segment, Side};

use super::workspace::{Choice, LinkKind};
use super::{Effect, Effects, Kit, Phase, Slackers, Target, slack};
use crate::config::VirtualWorkspace;
use crate::hl::{ACCENT, ALERT, BUTTON, DIM, LIVE, PRIMARY_BUTTON, READ, RECONNECTING, UNREAD};
use crate::icons;

/// What slackers' own commands on Messages are about, in `:help`: between opening and buffers.
const MESSAGES: Topic = Topic::new(75, "Messages");

/// slackers' default bindings: keys, the ex command each runs, what which-key says of it, and its `:help` topic.
const KEYS: [(&str, &str, &str, Topic); 12] = [
    ("i", "startinsert", "write", MESSAGES),
    ("A", "amend", "edit your Message", MESSAGES),
    ("+", "react", "react", MESSAGES),
    ("U", "unread", "mark unread from here", MESSAGES),
    ("K", "details", "Message details", Topic::OPENING),
    (
        "gx",
        "browse link",
        "open a link of the Message",
        Topic::OPENING,
    ),
    ("gX", "browse", "open the Message in Slack", Topic::OPENING),
    ("gy", "yanklink", "yank the Message's link", Topic::YANKING),
    (
        "<leader>f",
        "pick conversations",
        "find a Conversation",
        Topic::BUFFERS,
    ),
    ("<leader>a", "activity", "Activity", Topic::BUFFERS),
    (
        "<Bslash>",
        "sidebar",
        "the sidebar: go, hide, bring back",
        Topic::WINDOWS,
    ),
    ("q", "exit", "close, or quit", Topic::WINDOWS),
];

/// Runs `act` as a command does, then keeps everything current as any change does. What the cursor reads after,
/// `keyed` marks read.
fn acting(
    app: &mut Slackers,
    kit: &mut Kit,
    act: impl FnOnce(&mut Slackers, &mut Kit) -> Result<Effects, String>,
) -> Result<Effects, String> {
    let mut effects = act(app, kit)?;
    effects.extend(app.changed(kit, &[]));
    Ok(effects)
}

/// Adds slackers' commands, keys, pickers and statusline segments to `kit`.
pub(super) fn install(kit: &mut Kit) {
    let commands = [
        Command::<Slackers>::new("sidebar", 3, |app, kit, _| {
            acting(app, kit, |app, kit| {
                app.toggle_sidebar(kit);
                Ok(Vec::new())
            })
        })
        .help("hide the sidebar from within it, go to it from anywhere else, or bring it back"),
        Command::<Slackers>::new("exit", 3, |app, kit, _| app.exit(kit))
            .help("close the window; quit rather than leave the sidebar alone, or from it"),
        Command::<Slackers>::new("activity", 3, |app, kit, _| {
            acting(app, kit, |app, kit| Ok(app.open_activity(kit)))
        })
        .jumps()
        .help("Activity: Mentions, Reactions, Replies and DMs, newest first"),
        Command::<Slackers>::new("thread", 3, |app, kit, _| {
            acting(app, kit, |app, kit| {
                let (place, ts) = Slackers::message_under_cursor(kit)?;
                app.open_target(kit, Target::Message(place, ts))
            })
        })
        .jumps()
        .help("the Thread of the Message"),
        Command::<Slackers>::new("amend", 3, |app, kit, _| acting(app, kit, Slackers::amend))
            .help("edit your Message in the composer"),
        Command::<Slackers>::new("react", 3, |app, kit, line| {
            acting(app, kit, |app, kit| app.react(kit, line))
        })
        .help("react to the Message, or take your Reaction back"),
        Command::<Slackers>::new("unread", 3, |app, kit, _| {
            acting(app, kit, Slackers::mark_unread)
        })
        .help("mark the Message and those after it Unread"),
        Command::<Slackers>::new("details", 3, |app, kit, _| {
            acting(app, kit, Slackers::details)
        })
        .help("who wrote the Message, who reacted, where it leads"),
        // As in the other apps, `:browse` alone is the thing itself on the web, `:browse link` the link under the
        // cursor.
        Command::<Slackers>::new("browse", 3, |app, kit, line| match line.args {
            "link" => acting(app, kit, Slackers::browse),
            "" => acting(app, kit, |app, kit| {
                Ok(vec![Effect::OpenUrl(app.message_url(kit)?)])
            }),
            args => Err(format!("E475: Invalid argument: {args}")),
        })
        .help("open the Message in Slack; with link, the link under the cursor, or one of the Message's")
        .complete(|_, _, before, _| {
            if before.is_empty() {
                vec!["link".to_owned()]
            } else {
                Vec::new()
            }
        }),
        Command::<Slackers>::new("yanklink", 5, |app, kit, _| app.yank_link(kit))
            .help("yank the link under the cursor, else the Message's"),
        Command::<Slackers>::new("vw", 2, |app, kit, line| {
            acting(app, kit, |app, kit| app.virtual_workspace(kit, line))
        })
        .help("vw add NAME, vw rm NAME: put the Channel in a Virtual Workspace, or take it out")
        .complete(|app, _, before, _| match before {
            [] => vec!["add".to_owned(), "rm".to_owned()],
            [_] => app
                .virtual_workspaces
                .iter()
                .map(|virtual_workspace| virtual_workspace.name.clone())
                .collect(),
            _ => Vec::new(),
        }),
    ];
    for command in commands {
        kit.add_command(command);
    }
    for (notation, command, description, topic) in KEYS {
        kit.keymap_mut()
            .bind(&[Mode::Normal], notation, command, description, topic);
    }
    kit.add_picker("conversations", |app, kit| app.conversation_picker(kit));
    // The connections, coloured by how they stand, as many segments.
    for (standing, group) in [
        (Standing::Down, ALERT),
        (Standing::Reconnecting, RECONNECTING),
        (Standing::Connecting, ACCENT),
        (Standing::Live, LIVE),
    ] {
        let segment = Segment::new(Side::Left, 10, move |app: &Slackers, kit| {
            app.connections(kit, standing)
        });
        kit.add_segment(segment.with_group(group));
    }
    kit.add_segment(Segment::new(Side::Right, 0, |app: &Slackers, kit| {
        app.typing_segment(kit)
    }));
}

impl Slackers {
    /// `:react [emoji]`: reacts to the Message under the cursor with `emoji`, or takes the user's reaction back;
    /// without one, picks it.
    fn react(&mut self, kit: &mut Kit, line: Invocation<'_>) -> Result<Effects, String> {
        let (place, ts) = Self::message_under_cursor(kit)?;
        let named = line.args.trim().trim_matches(':');
        if !named.is_empty() {
            return Ok(self.react_with(&place, ts, named));
        }
        let workspace = self
            .workspaces
            .get(place.conversation.workspace)
            .ok_or("not signed in there")?;
        let mine = self.reacted(&place, ts);
        let mut choices: Vec<Pick<Target>> = workspace
            .emojis
            .offered()
            .into_iter()
            .map(|(name, aliases)| {
                let shown = match workspace
                    .emojis
                    .drawable(&slackers_model::Emoji::named(name))
                {
                    crate::emoji::Drawable::Text(text) => text.to_owned(),
                    crate::emoji::Drawable::Picture(_) | crate::emoji::Drawable::Name => {
                        String::new()
                    }
                };
                let pad =
                    3usize.saturating_sub(unicode_width::UnicodeWidthStr::width(shown.as_str()));
                let detail = if mine.contains(name) {
                    "  remove".to_owned()
                } else {
                    aliases
                        .first()
                        .map(|alias| format!("  {alias}"))
                        .unwrap_or_default()
                };
                Pick {
                    label: vec![
                        (format!("{shown}{}", " ".repeat(pad.max(1))), Group::NORMAL),
                        (format!(":{name}:"), Group::NORMAL),
                        (detail, DIM),
                    ],
                    text: format!(":{name}: {}", aliases.join(" ")),
                    pick: Picked::Target(Target::React {
                        place: place.clone(),
                        ts,
                        emoji: name.to_owned(),
                    }),
                }
            })
            .collect();
        // The user's own reactions first, to take back.
        choices.sort_by_key(|choice| match &choice.pick {
            Picked::Target(Target::React { emoji, .. }) => !mine.contains(emoji),
            _ => true,
        });
        let picker = Picker::list("React", choices, "No emoji by that name");
        Ok(kit.open_picker(self, picker))
    }

    /// `:unread` (`U`): marks the Message under the cursor unread, and everything after it, as Slack's "Mark unread"
    /// does: the Conversation stays unread until the user opens one.
    fn mark_unread(&mut self, kit: &mut Kit) -> Result<Effects, String> {
        let (place, ts) = Self::message_under_cursor(kit)?;
        if place.thread.is_some() {
            return Err("only a conversation's messages can be marked unread".to_owned());
        }
        let conversation = place.conversation;
        let unread = self
            .workspaces
            .get_mut(conversation.workspace)
            .and_then(|workspace| workspace.unread.get_mut(&conversation.id))
            .ok_or("only a conversation you're in can be marked unread")?;
        unread.mark_unread_from(ts);
        let request = slackers_slack::Request::Mark {
            conversation: conversation.id.clone(),
            up_to: unread.read_up_to(),
        };
        let effect = slack(conversation.workspace, request);
        self.kept_unread = Some(conversation);
        kit.info("marked unread: it stays so until you open a conversation");
        Ok(vec![effect])
    }

    /// `:details` (`K`): a card about the Message under the cursor, asking Slack for everyone who reacted to it when
    /// its history named only some.
    fn details(&mut self, kit: &mut Kit) -> Result<Effects, String> {
        let (place, ts) = Self::message_under_cursor(kit)?;
        let card = self
            .details_card(kit, &place, ts)
            .ok_or("no message under the cursor")?;
        kit.show_hover(card);
        let effects = self.want_reactors(&place, ts);
        self.details = Some((place, ts));
        Ok(effects)
    }

    /// `:browse` (`o`, `gx`): opens the link under the cursor; else the Message's link or file when it has just one,
    /// and a picker of where it leads and its buttons otherwise, `<C-y>` copying a link.
    fn browse(&mut self, kit: &mut Kit) -> Result<Effects, String> {
        if let Some(Target::Url(url)) = kit.target_under_cursor() {
            return Ok(vec![Effect::OpenUrl(url)]);
        }
        let (place, ts) = Self::message_under_cursor(kit)?;
        let (workspace, message) = self
            .message(&place, ts)
            .ok_or("no message under the cursor")?;
        let choices = workspace.choices(&place.conversation.id, message);
        let mut own = choices.iter().filter(|choice| match choice {
            Choice::Open(link) => link.kind != LinkKind::Message,
            Choice::Press(_) => true,
        });
        if let (Some(Choice::Open(only)), None) = (own.next(), own.next()) {
            return Ok(vec![Effect::OpenUrl(only.url.clone())]);
        }
        let picks = choices
            .into_iter()
            .map(|choice| match choice {
                Choice::Press(button) => {
                    let group = if button.primary {
                        PRIMARY_BUTTON
                    } else {
                        BUTTON
                    };
                    Pick {
                        label: vec![(format!("[ {} ]", button.label), group)],
                        text: button.label.clone(),
                        pick: Picked::Target(Target::Press {
                            place: place.clone(),
                            ts,
                            button,
                        }),
                    }
                }
                Choice::Open(link) => {
                    let icon = match link.kind {
                        LinkKind::Web => icons::LINK,
                        LinkKind::File => icons::FILE,
                        LinkKind::Message => icons::SLACK,
                    };
                    let mut label = vec![
                        (kit.icon(icon), ACCENT),
                        (link.label.clone(), Group::NORMAL_FLOAT),
                    ];
                    if link.label != link.url {
                        label.push((format!("  {}", link.url), DIM));
                    }
                    Pick {
                        label,
                        text: format!("{} {}", link.label, link.url),
                        pick: Picked::Target(Target::Url(link.url)),
                    }
                }
            })
            .collect();
        let picker =
            Picker::list("Open", picks, "Nothing to open").verb("<C-y>", "yank", |_, kit, pick| {
                match pick {
                    Picked::Target(Target::Url(url)) => {
                        // A verb leaves the picker open: copied, there is nothing more to pick.
                        kit.close_picker();
                        kit.info(format!("Yanked {url}"));
                        vec![Effect::Copy(url)]
                    }
                    _ => Vec::new(),
                }
            });
        Ok(kit.open_picker(self, picker))
    }

    /// The link to the message under the cursor in Slack's web client, which `:browse` opens.
    fn message_url(&self, kit: &Kit) -> Result<String, String> {
        let (place, ts) = Self::message_under_cursor(kit)?;
        let (workspace, message) = self
            .message(&place, ts)
            .ok_or("no message under the cursor")?;
        workspace
            .links(&place.conversation.id, message)
            .into_iter()
            .find(|link| link.kind == LinkKind::Message)
            .map(|link| link.url)
            .ok_or_else(|| "Slack doesn't keep this message: it has no link".to_owned())
    }

    /// `:yanklink` (`gy`): the link under the cursor, else the Message's on Slack's web client, to the clipboard.
    fn yank_link(&mut self, kit: &mut Kit) -> Result<Effects, String> {
        let url = match kit.target_under_cursor() {
            Some(Target::Url(url)) => url,
            _ => self.message_url(kit)?,
        };
        kit.info(format!("Yanked {url}"));
        Ok(vec![Effect::Copy(url)])
    }

    /// `:vw add NAME` and `:vw rm NAME`: puts the Channel the main window shows in a Virtual Workspace, or takes
    /// it out, and saves the config.
    fn virtual_workspace(&mut self, kit: &Kit, line: Invocation<'_>) -> Result<Effects, String> {
        let (verb, name) = split_word(line.args);
        match (verb, name) {
            ("add", name) if !name.is_empty() => self.add_to_virtual_workspace(kit, name)?,
            ("rm", name) if !name.is_empty() => self.remove_from_virtual_workspace(kit, name)?,
            _ => return Err("usage: :vw add NAME, or :vw rm NAME".to_owned()),
        }
        self.list_sidebar();
        let saved = self.virtual_workspaces.clone();
        Ok(vec![Effect::App(super::Io::SaveVirtualWorkspaces(saved))])
    }

    /// Puts the open Channel in the Virtual Workspace called `name` (in any case), creating it if there's none.
    fn add_to_virtual_workspace(&mut self, kit: &Kit, name: &str) -> Result<(), String> {
        let open = self.shown_conversation(kit).ok_or("open a channel first")?;
        let channel = self.workspaces.channel_ref(&open)?;
        let existing = self
            .virtual_workspaces
            .iter()
            .position(|virtual_workspace| same_name(&virtual_workspace.name, name));
        match existing {
            Some(at) => {
                let virtual_workspace = &mut self.virtual_workspaces[at];
                if virtual_workspace.channels.contains(&channel) {
                    return Err(format!(
                        "#{} is in {} already",
                        channel.channel, virtual_workspace.name
                    ));
                }
                virtual_workspace.channels.push(channel);
            }
            None => self.virtual_workspaces.push(VirtualWorkspace {
                name: name.to_owned(),
                channels: vec![channel],
            }),
        }
        Ok(())
    }

    /// Takes the open Channel out of the Virtual Workspace called `name` (in any case), and drops the Virtual
    /// Workspace once it holds nothing.
    fn remove_from_virtual_workspace(&mut self, kit: &Kit, name: &str) -> Result<(), String> {
        let open = self.shown_conversation(kit).ok_or("open a channel first")?;
        let channel = self.workspaces.channel_ref(&open)?;
        let at = self
            .virtual_workspaces
            .iter()
            .position(|virtual_workspace| same_name(&virtual_workspace.name, name))
            .ok_or_else(|| format!("no virtual workspace is called {name}"))?;
        let virtual_workspace = &mut self.virtual_workspaces[at];
        let Some(listed) = virtual_workspace
            .channels
            .iter()
            .position(|c| *c == channel)
        else {
            return Err(format!(
                "#{} isn't in {}",
                channel.channel, virtual_workspace.name
            ));
        };
        virtual_workspace.channels.remove(listed);
        if virtual_workspace.channels.is_empty() {
            self.virtual_workspaces.remove(at);
        }
        Ok(())
    }

    /// The picker over every Conversation the sidebar lists, by name and Workspace, fzf-style.
    fn conversation_picker(&self, kit: &Kit) -> Picker<Slackers> {
        let choices = self
            .sidebar
            .conversations()
            .into_iter()
            .filter_map(|conversation| {
                let (workspace, found) = self.workspaces.conversation(conversation)?;
                let unread = self
                    .workspaces
                    .unread(conversation)
                    .is_some_and(slackers_model::Unread::has_unread);
                let icon = match &found.kind {
                    ConversationKind::Channel { private: false } => icons::CHANNEL,
                    ConversationKind::Channel { private: true } => icons::PRIVATE_CHANNEL,
                    ConversationKind::Dm { .. } => icons::DM,
                    ConversationKind::GroupDm => icons::GROUP_DM,
                };
                let name = workspace.conversation_name(found).to_owned();
                let (icon_group, name_group) = if unread {
                    (ACCENT, UNREAD)
                } else {
                    (DIM, READ)
                };
                Some(Pick {
                    label: vec![
                        (kit.icon(icon), icon_group),
                        (name.clone(), name_group),
                        (format!("  {}", workspace.name), DIM),
                    ],
                    text: format!("{name} {}", workspace.name),
                    pick: Picked::Target(Target::Conversation(conversation.clone())),
                })
            })
            .collect();
        Picker::list("Conversations", choices, "No conversation by that name")
    }

    /// The Workspaces whose connections stand as `standing`, for the statusline; `live` once every one is.
    fn connections(&self, kit: &Kit, standing: Standing) -> Option<String> {
        let mut parts = Vec::new();
        let mut all_live = true;
        for (_, sign_in) in self.workspaces.iter() {
            let (icon, state, stands) = match &sign_in.phase {
                Phase::SigningIn => (
                    icons::LOADING,
                    "signing in".to_owned(),
                    Standing::Connecting,
                ),
                Phase::Failed(_) => (icons::ALERT, "signed out".to_owned(), Standing::Down),
                Phase::Ready(workspace) => match workspace.connection {
                    Some(Connection::Live) => continue,
                    None => (
                        icons::LOADING,
                        "connecting".to_owned(),
                        Standing::Connecting,
                    ),
                    Some(Connection::Reconnecting { attempt }) => (
                        icons::OFFLINE,
                        format!("reconnecting ({attempt})"),
                        Standing::Reconnecting,
                    ),
                },
            };
            all_live = false;
            if stands == standing {
                parts.push(format!("{}{} {state}", kit.icon(icon), sign_in.label));
            }
        }
        match standing {
            Standing::Live => all_live.then(|| format!("{}live", kit.icon(icons::LIVE))),
            _ => (!parts.is_empty()).then(|| parts.join("  ")),
        }
    }

    /// Who's typing in the Conversations and Threads shown, for the statusline.
    fn typing_segment(&self, kit: &Kit) -> Option<String> {
        let mut said = Vec::new();
        for place in self.shown_places(kit) {
            let Some(workspace) = self.workspaces.get(place.conversation.workspace) else {
                continue;
            };
            let names: Vec<&str> = self
                .typing
                .at(&place)
                .into_iter()
                .map(|id| workspace.user_name(id))
                .collect();
            let there = if place.thread.is_some() {
                " in the thread"
            } else {
                ""
            };
            said.push(match names.as_slice() {
                [] => continue,
                [one] => format!("{one} is typing{there}…"),
                [one, other] => format!("{one} and {other} are typing{there}…"),
                _ => format!("several people are typing{there}…"),
            });
        }
        (!said.is_empty()).then(|| said.join("  "))
    }
}

/// How a Workspace's connection stands, as the statusline colours it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// Signed out, or never signed in.
    Down,
    /// Live once, and coming back.
    Reconnecting,
    /// Signing in, or not live yet.
    Connecting,
    /// Every one live.
    Live,
}

/// `text`'s first word, and the rest without leading spaces.
fn split_word(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (text, ""),
    }
}

/// Whether two Virtual Workspaces' names are the same, ignoring case.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}
