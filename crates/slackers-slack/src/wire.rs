//! Slack's JSON, and how it maps onto the model. The only module that knows Slack's field names.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;
use slackers_model::{
    Activity, Author, Block, Broadcast, Conversation, ConversationId, ConversationKind,
    CustomEmojiSet, Emoji, File, Image, Inline, Message, Reaction, ReactionChange, RichText, Style,
    ThreadSummary, Ts, UserId, WorkspaceId,
};

use crate::mrkdwn;
use crate::session::{FollowedThread, HistoryPage, LiveEvent, Person, Snapshot, User};
use crate::web::Permalinks;

/// Slack's timestamp text as a [`Ts`]; anything unparsable, such as an absent `latest`, reads as zero.
fn ts(text: &str) -> Ts {
    text.parse().unwrap_or_default()
}

#[derive(Deserialize)]
pub(crate) struct AuthTest {
    /// The team id, or the enterprise id for an org-level session.
    team_id: String,
    user_id: String,
    /// Where the Workspace is on the web, such as `https://example.slack.com/`.
    url: String,
}

/// `subscriptions.thread.getView`'s reply: the Followed Threads with the latest Replies, newest first.
#[derive(Deserialize)]
pub(crate) struct ThreadView {
    #[serde(default)]
    threads: Vec<ViewThread>,
}

#[derive(Deserialize)]
struct ViewThread {
    /// The parent Message, with the Thread's state.
    root_msg: Value,
}

/// The fields of a Thread's parent, as `subscriptions.thread.getView` lists it, that say where the Thread stands.
#[derive(Deserialize)]
struct ThreadState {
    channel: String,
    #[serde(default)]
    last_read: String,
    #[serde(default)]
    latest_reply: String,
    #[serde(default)]
    subscribed: Option<bool>,
}

impl ThreadView {
    /// The Threads the user follows.
    pub(crate) fn into_followed(self) -> Vec<FollowedThread> {
        self.threads
            .into_iter()
            .filter_map(|thread| {
                let state: ThreadState = serde_json::from_value(thread.root_msg.clone()).ok()?;
                if state.subscribed == Some(false) {
                    return None;
                }
                let (read, latest) = (ts(&state.last_read), ts(&state.latest_reply));
                Some(FollowedThread {
                    conversation: ConversationId::new(state.channel),
                    parent: message(thread.root_msg)?,
                    activity: Activity::counted(read, latest, latest > read, 0),
                })
            })
            .collect()
    }
}

#[derive(Deserialize)]
pub(crate) struct UserBoot {
    team: Team,
    #[serde(default)]
    channels: Vec<BootChannel>,
    #[serde(default)]
    ims: Vec<BootIm>,
}

#[derive(Deserialize)]
struct Team {
    name: String,
    #[serde(default)]
    icon: TeamIcon,
}

/// A Workspace's icon, at several sizes.
#[derive(Default, Deserialize)]
struct TeamIcon {
    #[serde(default)]
    image_44: Option<String>,
    #[serde(default)]
    image_68: Option<String>,
    #[serde(default)]
    image_34: Option<String>,
    /// Whether it's Slack's generated default rather than one the Workspace chose.
    #[serde(default)]
    image_default: bool,
}

#[derive(Deserialize)]
struct BootChannel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    is_private: bool,
    #[serde(default)]
    is_mpim: bool,
    #[serde(default)]
    is_archived: bool,
}

#[derive(Deserialize)]
struct BootIm {
    id: String,
    user: String,
}

#[derive(Deserialize)]
pub(crate) struct Counts {
    #[serde(default)]
    channels: Vec<Count>,
    #[serde(default)]
    mpims: Vec<Count>,
    #[serde(default)]
    ims: Vec<Count>,
}

#[derive(Deserialize)]
struct Count {
    id: String,
    #[serde(default)]
    last_read: String,
    #[serde(default)]
    latest: String,
    #[serde(default)]
    has_unreads: bool,
    #[serde(default)]
    mention_count: u32,
}

/// The user's sidebar: the Conversations `client.counts` lists (as Slack's own sidebar does), named from
/// `client.userBoot`, which also lists every group DM the user was ever in.
pub(crate) fn snapshot(
    auth: AuthTest,
    boot: UserBoot,
    counts: Counts,
    followed_threads: Vec<FollowedThread>,
) -> Snapshot {
    let channels: HashMap<&str, &BootChannel> =
        boot.channels.iter().map(|c| (c.id.as_str(), c)).collect();
    let ims: HashMap<&str, &BootIm> = boot.ims.iter().map(|im| (im.id.as_str(), im)).collect();
    let activity = |count: &Count| {
        Activity::counted(
            ts(&count.last_read),
            ts(&count.latest),
            count.has_unreads,
            count.mention_count,
        )
    };

    let mut conversations = Vec::new();
    for count in counts.channels.iter().chain(&counts.mpims) {
        let Some(channel) = channels.get(count.id.as_str()) else {
            continue;
        };
        if channel.is_archived {
            continue;
        }
        let (kind, name) = if channel.is_mpim {
            (ConversationKind::GroupDm, group_dm_name(&channel.name))
        } else {
            (
                ConversationKind::Channel {
                    private: channel.is_private,
                },
                channel.name.clone(),
            )
        };
        conversations.push((
            Conversation {
                id: ConversationId::new(&channel.id),
                kind,
                name,
            },
            activity(count),
        ));
    }
    for count in &counts.ims {
        let Some(im) = ims.get(count.id.as_str()) else {
            continue;
        };
        let kind = ConversationKind::Dm {
            with: UserId::new(&im.user),
        };
        conversations.push((
            Conversation {
                id: ConversationId::new(&im.id),
                kind,
                name: String::new(),
            },
            activity(count),
        ));
    }
    let icon = boot.team.icon;
    let icon = if icon.image_default {
        None
    } else {
        icon.image_44.or(icon.image_68).or(icon.image_34)
    };
    Snapshot {
        workspace: WorkspaceId::new(auth.team_id),
        name: boot.team.name,
        icon,
        me: UserId::new(auth.user_id),
        permalinks: Permalinks::new(auth.url),
        conversations,
        followed_threads,
    }
}

/// `conversations.info`'s reply.
#[derive(Deserialize)]
pub(crate) struct ConversationInfo {
    channel: InfoChannel,
}

#[derive(Deserialize)]
struct InfoChannel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    is_im: bool,
    /// The other person in a DM.
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    is_mpim: bool,
    #[serde(default)]
    is_private: bool,
    #[serde(default)]
    is_archived: bool,
    /// Absent for a DM, which the user is always in.
    #[serde(default)]
    is_member: Option<bool>,
    #[serde(default)]
    last_read: String,
}

impl ConversationInfo {
    /// The Conversation as the sidebar lists it, with its Activity up to its Read Marker; `None` when the sidebar
    /// wouldn't list it: it's archived, or the user isn't in it.
    pub(crate) fn into_listed(self) -> Option<(Conversation, Activity)> {
        let channel = self.channel;
        if channel.is_archived || channel.is_member == Some(false) {
            return None;
        }
        let (kind, name) = if channel.is_im {
            let with = UserId::new(channel.user?);
            (ConversationKind::Dm { with }, String::new())
        } else if channel.is_mpim {
            (ConversationKind::GroupDm, group_dm_name(&channel.name))
        } else {
            let private = channel.is_private;
            (ConversationKind::Channel { private }, channel.name)
        };
        let read = ts(&channel.last_read);
        let conversation = Conversation {
            id: ConversationId::new(channel.id),
            kind,
            name,
        };
        Some((conversation, Activity::counted(read, read, false, 0)))
    }
}

/// `mpdm-alice--bob--carol-1` as `alice, bob, carol`.
fn group_dm_name(slack_name: &str) -> String {
    let members = slack_name.strip_prefix("mpdm-").unwrap_or(slack_name);
    let members = members
        .rsplit_once('-')
        .map_or(members, |(members, suffix)| {
            if suffix.bytes().all(|b| b.is_ascii_digit()) {
                members
            } else {
                slack_name
            }
        });
    members.split("--").collect::<Vec<_>>().join(", ")
}

/// A page of `conversations.replies`.
#[derive(Deserialize)]
pub(crate) struct Replies {
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    response_metadata: Option<ResponseMetadata>,
}

#[derive(Deserialize)]
struct ResponseMetadata {
    #[serde(default)]
    next_cursor: String,
}

impl Replies {
    /// Where the next page starts; empty after the last one.
    pub(crate) fn next_cursor(&self) -> &str {
        self.response_metadata
            .as_ref()
            .map_or("", |metadata| metadata.next_cursor.as_str())
    }

    pub(crate) fn into_messages(self) -> impl Iterator<Item = Message> {
        self.messages.into_iter().filter_map(message)
    }
}

#[derive(Deserialize)]
pub(crate) struct History {
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    has_more: bool,
}

impl History {
    pub(crate) fn into_page(self) -> HistoryPage {
        HistoryPage {
            messages: self.messages.into_iter().filter_map(message).collect(),
            has_more: self.has_more,
        }
    }
}

#[derive(Deserialize)]
struct WireMessage {
    ts: String,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    bot_profile: Option<BotProfile>,
    /// An integration's avatar, when it has no bot profile.
    #[serde(default)]
    icons: Icons,
    #[serde(default)]
    text: String,
    #[serde(default)]
    blocks: Vec<Value>,
    #[serde(default)]
    attachments: Vec<WireAttachment>,
    #[serde(default)]
    files: Vec<WireFile>,
    #[serde(default)]
    reactions: Vec<WireReaction>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    reply_count: u32,
    #[serde(default)]
    latest_reply: Option<String>,
    #[serde(default)]
    edited: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct BotProfile {
    name: String,
    #[serde(default)]
    icons: Icons,
}

/// A bot's avatar, at several sizes.
#[derive(Default, Deserialize)]
struct Icons {
    #[serde(default)]
    image_48: Option<String>,
    #[serde(default)]
    image_72: Option<String>,
    #[serde(default)]
    image_36: Option<String>,
}

impl Icons {
    /// The avatar closest to 48 pixels square.
    fn avatar(self) -> Option<String> {
        self.image_48.or(self.image_72).or(self.image_36)
    }
}

#[derive(Deserialize)]
struct WireAttachment {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    fallback: Option<String>,
}

#[derive(Deserialize)]
struct WireFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    mimetype: String,
    /// Renditions of an image, by the longest side they fit in, with their sizes in pixels. Slack makes only those
    /// smaller than the original.
    #[serde(default)]
    thumb_720: Option<String>,
    #[serde(default)]
    thumb_720_w: Option<u32>,
    #[serde(default)]
    thumb_720_h: Option<u32>,
    #[serde(default)]
    thumb_480: Option<String>,
    #[serde(default)]
    thumb_480_w: Option<u32>,
    #[serde(default)]
    thumb_480_h: Option<u32>,
    #[serde(default)]
    thumb_360: Option<String>,
    #[serde(default)]
    thumb_360_w: Option<u32>,
    #[serde(default)]
    thumb_360_h: Option<u32>,
    /// A GIF's animated rendition, as big as `thumb_360`.
    #[serde(default)]
    thumb_360_gif: Option<String>,
    /// Its page in the web client.
    #[serde(default)]
    permalink: Option<String>,
}

impl WireFile {
    /// The rendition to show inline: a GIF's animated one, else the largest still one up to 720 pixels, which
    /// suits a terminal and is far smaller to fetch than the original. `None` when Slack made none: it isn't an
    /// image, or it was deleted.
    fn image(&self) -> Option<Image> {
        let rendition = |url: &Option<String>, width: Option<u32>, height: Option<u32>| {
            Some(Image {
                url: url.clone()?,
                width: width?,
                height: height?,
            })
        };
        let animated = if self.mimetype == "image/gif" {
            rendition(&self.thumb_360_gif, self.thumb_360_w, self.thumb_360_h)
        } else {
            None
        };
        animated
            .or_else(|| rendition(&self.thumb_720, self.thumb_720_w, self.thumb_720_h))
            .or_else(|| rendition(&self.thumb_480, self.thumb_480_w, self.thumb_480_h))
            .or_else(|| rendition(&self.thumb_360, self.thumb_360_w, self.thumb_360_h))
    }
}

#[derive(Deserialize)]
struct WireReaction {
    name: String,
    #[serde(default)]
    count: u32,
    #[serde(default)]
    users: Vec<String>,
}

/// One Message, as `conversations.history` lists it or the socket sends it; `None` when it's malformed.
fn message(value: Value) -> Option<Message> {
    let wire: WireMessage = serde_json::from_value(value)
        .inspect_err(
            |error| tracing::warn!(%error, "skipping a message slack sent in an unknown shape"),
        )
        .ok()?;
    let ts = ts(&wire.ts);
    let author = match (wire.bot_profile, wire.user, wire.username) {
        (Some(bot), _, _) => Author::Bot {
            name: bot.name,
            avatar: bot.icons.avatar(),
        },
        (None, Some(user), _) => Author::User(UserId::new(user)),
        (None, None, name) => Author::Bot {
            name: name.unwrap_or_else(|| "bot".to_owned()),
            avatar: wire.icons.avatar(),
        },
    };
    // A broadcast Reply also shows in the Conversation, so it's top-level here.
    let broadcast = wire.subtype.as_deref() == Some("thread_broadcast");
    let reply_to = wire
        .thread_ts
        .as_deref()
        .map(self::ts)
        .filter(|&parent| parent != ts && !broadcast);
    let thread = (wire.reply_count > 0).then(|| ThreadSummary {
        replies: wire.reply_count,
        latest: wire.latest_reply.as_deref().map_or(ts, self::ts),
    });
    let reactions = wire
        .reactions
        .into_iter()
        .map(|reaction| {
            let named = u32::try_from(reaction.users.len()).unwrap_or(u32::MAX);
            Reaction {
                emoji: reaction_emoji(&reaction.name),
                count: reaction.count.max(named).max(1),
                users: reaction.users.into_iter().map(UserId::new).collect(),
            }
        })
        .collect();
    let files = wire
        .files
        .into_iter()
        .map(|file| File {
            image: file.image(),
            name: file.name,
            mimetype: file.mimetype,
            link: file.permalink.filter(|link| !link.is_empty()),
        })
        .collect();
    Some(Message {
        ts,
        author,
        content: content(wire.blocks, &wire.text, wire.attachments),
        reply_to,
        thread,
        reactions,
        files,
        edited: wire.edited.is_some(),
    })
}

/// A Message's text: its rich text blocks when it has any, else its `mrkdwn` text; legacy attachments follow as
/// quotes.
fn content(blocks: Vec<Value>, text: &str, attachments: Vec<WireAttachment>) -> RichText {
    let rich: Vec<Block> = blocks
        .into_iter()
        .filter_map(|block| serde_json::from_value::<WireBlock>(block).ok())
        .flat_map(|block| match block {
            WireBlock::RichText { elements } => {
                elements.into_iter().filter_map(rich_block).collect()
            }
            WireBlock::Image {
                image_url: Some(url),
                image_width: Some(width),
                image_height: Some(height),
                alt_text,
            } => vec![Block::Image {
                image: Image { url, width, height },
                alt_text,
            }],
            // An image Slack didn't measure, or one of its own files: neither can take its room before it loads.
            WireBlock::Image { .. } | WireBlock::Other => Vec::new(),
        })
        .collect();
    let mut content = if rich.is_empty() {
        mrkdwn::parse(text)
    } else {
        RichText { blocks: rich }
    };
    for attachment in attachments {
        let Some(body) = attachment.text.or(attachment.fallback) else {
            continue;
        };
        content.blocks.extend(
            mrkdwn::parse(&body)
                .blocks
                .into_iter()
                .map(|block| match block {
                    Block::Paragraph(inlines) => Block::Quote(inlines),
                    other => other,
                }),
        );
    }
    content
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    RichText {
        #[serde(default)]
        elements: Vec<Value>,
    },
    /// An image by URL, as Giphy posts them. Slack adds its size to those it stores.
    Image {
        #[serde(default)]
        image_url: Option<String>,
        #[serde(default)]
        image_width: Option<u32>,
        #[serde(default)]
        image_height: Option<u32>,
        #[serde(default)]
        alt_text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RichElement {
    RichTextSection {
        #[serde(default)]
        elements: Vec<Value>,
    },
    RichTextQuote {
        #[serde(default)]
        elements: Vec<Value>,
    },
    RichTextPreformatted {
        #[serde(default)]
        elements: Vec<Value>,
    },
    RichTextList {
        #[serde(default)]
        style: String,
        #[serde(default)]
        elements: Vec<ListItem>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ListItem {
    #[serde(default)]
    elements: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InlineElement {
    Text {
        #[serde(default)]
        text: String,
        #[serde(default)]
        style: TextStyle,
    },
    Emoji {
        name: String,
        #[serde(default)]
        unicode: Option<String>,
        #[serde(default)]
        skin_tone: Option<u8>,
    },
    User {
        user_id: String,
    },
    Channel {
        channel_id: String,
    },
    Usergroup {
        usergroup_id: String,
    },
    Broadcast {
        range: String,
    },
    Link {
        url: String,
        #[serde(default)]
        text: Option<String>,
    },
    Date {
        #[serde(default)]
        fallback: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Default, Deserialize)]
struct TextStyle {
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    strike: bool,
    #[serde(default)]
    code: bool,
}

fn rich_block(element: Value) -> Option<Block> {
    Some(match serde_json::from_value::<RichElement>(element).ok()? {
        RichElement::RichTextSection { elements } => Block::Paragraph(inlines(elements)),
        RichElement::RichTextQuote { elements } => Block::Quote(inlines(elements)),
        RichElement::RichTextPreformatted { elements } => Block::Preformatted(
            inlines(elements)
                .into_iter()
                .map(|inline| match inline {
                    Inline::Text { text, .. } => text,
                    Inline::Link { url, label } => label.unwrap_or(url),
                    _ => String::new(),
                })
                .collect(),
        ),
        RichElement::RichTextList { style, elements } => Block::List {
            ordered: style == "ordered",
            items: elements
                .into_iter()
                .map(|item| inlines(item.elements))
                .collect(),
        },
        RichElement::Other => return None,
    })
}

fn inlines(elements: Vec<Value>) -> Vec<Inline> {
    elements
        .into_iter()
        .filter_map(|element| serde_json::from_value::<InlineElement>(element).ok())
        .filter_map(|element| {
            Some(match element {
                InlineElement::Text { text, style } => {
                    let mut flags = Style::PLAIN;
                    for (set, flag) in [
                        (style.bold, Style::BOLD),
                        (style.italic, Style::ITALIC),
                        (style.strike, Style::STRIKE),
                        (style.code, Style::CODE),
                    ] {
                        if set {
                            flags = flags | flag;
                        }
                    }
                    Inline::Text { text, style: flags }
                }
                InlineElement::Emoji {
                    name,
                    unicode,
                    skin_tone,
                } => Inline::Emoji(Emoji {
                    name,
                    skin_tone: skin_tone.filter(|tone| (2..=6).contains(tone)),
                    unicode: unicode.as_deref().and_then(unicode_from_hex),
                }),
                InlineElement::User { user_id } => Inline::User(UserId::new(user_id)),
                InlineElement::Channel { channel_id } => {
                    Inline::Channel(ConversationId::new(channel_id))
                }
                InlineElement::Usergroup { usergroup_id } => Inline::UserGroup(usergroup_id),
                InlineElement::Broadcast { range } => Inline::Broadcast(match range.as_str() {
                    "here" => Broadcast::Here,
                    "everyone" => Broadcast::Everyone,
                    _ => Broadcast::Channel,
                }),
                InlineElement::Link { url, text } => Inline::Link {
                    url,
                    label: text.filter(|t| !t.is_empty()),
                },
                InlineElement::Date { fallback } => Inline::Text {
                    text: fallback?,
                    style: Style::PLAIN,
                },
                InlineElement::Other => return None,
            })
        })
        .collect()
}

/// Slack's spelling of an emoji's code points, such as `1f468-200d-1f4bb`, as the characters.
fn unicode_from_hex(hex: &str) -> Option<String> {
    hex.split('-')
        .map(|point| u32::from_str_radix(point, 16).ok().and_then(char::from_u32))
        .collect()
}

/// A reaction's name, such as `thumbsup::skin-tone-2`, as an [`Emoji`].
fn reaction_emoji(name: &str) -> Emoji {
    let (base, tone) = name
        .split_once("::skin-tone-")
        .map_or((name, None), |(base, tone)| (base, tone.parse().ok()));
    Emoji {
        name: base.to_owned(),
        skin_tone: tone.filter(|t| (2..=6).contains(t)),
        unicode: None,
    }
}

/// `chat.postMessage`'s reply.
#[derive(Deserialize)]
pub(crate) struct Posted {
    message: Value,
}

impl Posted {
    /// The Message as Slack stored it; `None` if it came back in an unknown shape.
    pub(crate) fn into_message(self) -> Option<Message> {
        message(self.message)
    }
}

/// `reactions.get`'s reply: the Message reacted to.
#[derive(Deserialize)]
pub(crate) struct ReactedTo {
    message: Value,
}

impl ReactedTo {
    /// The Message's reactions; `None` if it came back in an unknown shape.
    pub(crate) fn into_reactions(self) -> Option<Vec<Reaction>> {
        Some(message(self.message)?.reactions)
    }
}

#[derive(Deserialize)]
pub(crate) struct UserInfo {
    user: WireUser,
}

#[derive(Deserialize)]
struct WireUser {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    real_name: String,
    #[serde(default)]
    deleted: bool,
    /// Their time zone's IANA name, such as `Europe/Paris`.
    #[serde(default)]
    tz: Option<String>,
    #[serde(default)]
    profile: Profile,
}

impl WireUser {
    /// What to call them: their display name, else their full name, else their handle, else their id.
    fn display_name(&self) -> String {
        [
            &self.profile.display_name,
            &self.profile.real_name,
            &self.real_name,
            &self.name,
        ]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or(&self.id)
        .clone()
    }

    /// Their full name, as their profile gives it; empty when it gives none.
    fn full_name(&self) -> String {
        [&self.profile.real_name, &self.real_name]
            .into_iter()
            .find(|name| !name.is_empty())
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Default, Deserialize)]
struct Profile {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    real_name: String,
    /// What they do, such as their job title.
    #[serde(default)]
    title: String,
    #[serde(default)]
    image_48: Option<String>,
}

impl UserInfo {
    pub(crate) fn into_user(self) -> User {
        let user = self.user;
        User {
            name: user.display_name(),
            full_name: user.full_name(),
            id: UserId::new(user.id),
            avatar: user.profile.image_48.filter(|url| !url.is_empty()),
            title: user.profile.title,
            time_zone: user.tz.filter(|zone| !zone.is_empty()),
        }
    }
}

/// The edge API's `users/search` reply: the people found, best first.
#[derive(Deserialize)]
pub(crate) struct PeopleSearch {
    #[serde(default)]
    results: Vec<WireUser>,
}

impl PeopleSearch {
    /// The people found, best first, less any deactivated account.
    pub(crate) fn into_people(self) -> Vec<Person> {
        self.results
            .into_iter()
            .filter(|user| !user.deleted)
            .map(|user| Person {
                name: user.display_name(),
                full_name: user.full_name(),
                id: UserId::new(user.id),
            })
            .collect()
    }
}

#[derive(Deserialize)]
pub(crate) struct EmojiList {
    #[serde(default)]
    emoji: HashMap<String, String>,
}

impl EmojiList {
    pub(crate) fn into_set(self) -> CustomEmojiSet {
        let mut set = CustomEmojiSet::default();
        for (name, target) in self.emoji {
            match target.strip_prefix("alias:") {
                Some(of) => set.insert_alias(name, of),
                None => set.insert_image(name, target),
            }
        }
        set
    }
}

/// What a frame on the live socket means to us.
#[derive(Debug, PartialEq)]
pub(crate) enum SocketMessage {
    /// The connection is up and events will follow.
    Hello,
    /// Where to reconnect, should this connection drop.
    ReconnectUrl(String),
    Live(LiveEvent),
    /// A Conversation appeared in the user's sidebar: they joined a Channel, or a DM opened.
    Appeared(ConversationId),
    /// Anything we don't follow.
    Ignored,
}

/// Interprets one frame from the live socket.
pub(crate) fn socket_message(frame: &str) -> SocketMessage {
    let Ok(event) = serde_json::from_str::<Value>(frame) else {
        return SocketMessage::Ignored;
    };
    let text = |field: &str| event[field].as_str().unwrap_or_default();
    let conversation = || ConversationId::new(text("channel"));
    let live = match text("type") {
        "hello" => return SocketMessage::Hello,
        "reconnect_url" => return SocketMessage::ReconnectUrl(text("url").to_owned()),
        "message" => match text("subtype") {
            "message_changed" => {
                let Some(edited) = message(event["message"].clone()) else {
                    return SocketMessage::Ignored;
                };
                LiveEvent::Edited {
                    conversation: conversation(),
                    message: edited,
                }
            }
            "message_deleted" => LiveEvent::Deleted {
                conversation: conversation(),
                ts: ts(text("deleted_ts")),
            },
            "message_replied" => return SocketMessage::Ignored,
            _ => {
                let Some(posted) = message(event.clone()) else {
                    return SocketMessage::Ignored;
                };
                LiveEvent::Posted {
                    conversation: conversation(),
                    message: posted,
                }
            }
        },
        kind @ ("reaction_added" | "reaction_removed") => LiveEvent::Reacted {
            conversation: ConversationId::new(
                event["item"]["channel"].as_str().unwrap_or_default(),
            ),
            ts: ts(event["item"]["ts"].as_str().unwrap_or_default()),
            emoji: reaction_emoji(text("reaction")),
            user: UserId::new(text("user")),
            change: if kind == "reaction_added" {
                ReactionChange::Added
            } else {
                ReactionChange::Removed
            },
        },
        "channel_marked" | "im_marked" | "group_marked" | "mpim_marked" => LiveEvent::Marked {
            conversation: conversation(),
            up_to: ts(text("ts")),
        },
        kind @ ("thread_marked" | "thread_subscribed" | "thread_unsubscribed") => {
            let subscription = &event["subscription"];
            let field = |name: &str| subscription[name].as_str().unwrap_or_default();
            if field("type") != "thread" {
                return SocketMessage::Ignored;
            }
            let (conversation, parent) = (
                ConversationId::new(field("channel")),
                ts(field("thread_ts")),
            );
            match kind {
                "thread_marked" => LiveEvent::ThreadMarked {
                    conversation,
                    parent,
                    up_to: ts(field("last_read")),
                },
                "thread_subscribed" => LiveEvent::ThreadFollowed {
                    conversation,
                    parent,
                    read_up_to: ts(field("last_read")),
                },
                _ => LiveEvent::ThreadUnfollowed {
                    conversation,
                    parent,
                },
            }
        }
        "channel_joined" | "group_joined" | "mpim_joined" | "im_created" | "im_open"
        | "mpim_open" => {
            // Some carry the Conversation, others only its id.
            let channel = &event["channel"];
            return match channel["id"].as_str().or_else(|| channel.as_str()) {
                Some(id) => SocketMessage::Appeared(ConversationId::new(id)),
                None => SocketMessage::Ignored,
            };
        }
        _ => return SocketMessage::Ignored,
    };
    SocketMessage::Live(live)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn me() -> UserId {
        UserId::new("UME")
    }

    fn seconds(s: u64) -> Ts {
        Ts::from_micros(s * 1_000_000)
    }

    #[test]
    fn the_sidebar_comes_from_counts_named_from_boot() {
        let auth: AuthTest = serde_json::from_value(
            json!({"team_id": "E1", "user_id": "UME", "url": "https://org.slack.com/"}),
        )
        .unwrap();
        let boot: UserBoot = serde_json::from_value(json!({
            "self": {"id": "UME"},
            "team": {"name": "Org", "icon": {"image_34": "https://i/34.png", "image_44": "https://i/44.png"}},
            "channels": [
                {"id": "C1", "name": "general", "is_private": false},
                {"id": "G1", "name": "secret", "is_private": true},
                {"id": "C2", "name": "old", "is_archived": true},
                {"id": "M1", "name": "mpdm-alice--bob-1", "is_mpim": true},
                {"id": "M2", "name": "mpdm-closed--group-1", "is_mpim": true}
            ],
            "ims": [{"id": "D1", "user": "U9"}]
        }))
        .unwrap();
        let counts: Counts = serde_json::from_value(json!({
            "channels": [
                {"id": "C1", "last_read": "10.000000", "latest": "20.000000", "has_unreads": true, "mention_count": 2},
                {"id": "G1", "last_read": "5.000000", "latest": "5.000000", "has_unreads": false},
                {"id": "C2", "has_unreads": true}
            ],
            "mpims": [{"id": "M1", "has_unreads": false}],
            "ims": [{"id": "D1", "latest": "30.000000", "has_unreads": true}]
        }))
        .unwrap();

        let snapshot = snapshot(auth, boot, counts, Vec::new());
        assert_eq!(
            (
                snapshot.workspace.as_str(),
                snapshot.name.as_str(),
                snapshot.me.as_str()
            ),
            ("E1", "Org", "UME")
        );
        assert_eq!(snapshot.icon.as_deref(), Some("https://i/44.png"));
        assert_eq!(
            snapshot.permalinks,
            Permalinks::new("https://org.slack.com/")
        );
        let listed: Vec<(&str, &ConversationKind, &str)> = snapshot
            .conversations
            .iter()
            .map(|(c, _)| (c.id.as_str(), &c.kind, c.name.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                (
                    "C1",
                    &ConversationKind::Channel { private: false },
                    "general"
                ),
                ("G1", &ConversationKind::Channel { private: true }, "secret"),
                ("M1", &ConversationKind::GroupDm, "alice, bob"),
                (
                    "D1",
                    &ConversationKind::Dm {
                        with: UserId::new("U9")
                    },
                    ""
                ),
            ]
        );
        let general = &snapshot.conversations[0].1;
        assert!(general.has_unread());
        assert_eq!(general.mentions(), 2);
        assert!(!snapshot.conversations[1].1.has_unread());
    }

    #[test]
    fn rich_text_blocks_become_the_content() {
        let message = message(json!({
            "ts": "100.000001", "user": "U1", "text": "fallback ignored",
            "blocks": [{"type": "rich_text", "elements": [
                {"type": "rich_text_section", "elements": [
                    {"type": "text", "text": "hi ", "style": {"bold": true}},
                    {"type": "emoji", "name": "wave", "unicode": "1f44b", "skin_tone": 3},
                    {"type": "user", "user_id": "UME"},
                    {"type": "link", "url": "https://x", "text": ""},
                    {"type": "something_new"}
                ]},
                {"type": "rich_text_list", "style": "ordered", "elements": [
                    {"type": "rich_text_section", "elements": [{"type": "text", "text": "one"}]}
                ]},
                {"type": "rich_text_preformatted", "elements": [{"type": "text", "text": "code"}]}
            ]}],
            "reactions": [
                {"name": "+1::skin-tone-2", "count": 52, "users": ["U1", "UME"]},
                {"name": "tada", "count": 1, "users": ["U1", "U2"]}
            ],
            "reply_count": 3, "latest_reply": "200.000000", "thread_ts": "100.000001",
            "edited": {"user": "U1", "ts": "101.000000"}
        }))
        .unwrap();

        assert_eq!(message.author, Author::User(UserId::new("U1")));
        assert!(message.mentions(&me()));
        assert!(message.edited);
        assert_eq!(message.reply_to, None, "a thread's parent isn't a reply");
        assert_eq!(
            message.thread,
            Some(ThreadSummary {
                replies: 3,
                latest: seconds(200)
            })
        );
        let wave = Emoji {
            name: "wave".into(),
            skin_tone: Some(3),
            unicode: Some("👋".into()),
        };
        assert_eq!(
            message.content.blocks,
            [
                Block::Paragraph(vec![
                    Inline::Text {
                        text: "hi ".into(),
                        style: Style::BOLD
                    },
                    Inline::Emoji(wave),
                    Inline::User(me()),
                    Inline::Link {
                        url: "https://x".into(),
                        label: None
                    },
                ]),
                Block::List {
                    ordered: true,
                    items: vec![vec![Inline::Text {
                        text: "one".into(),
                        style: Style::PLAIN
                    }]]
                },
                Block::Preformatted("code".into()),
            ]
        );
        let reactions: Vec<_> = message
            .reactions
            .iter()
            .map(|r| (r.emoji.name.as_str(), r.emoji.skin_tone, r.count))
            .collect();
        assert_eq!(reactions, [("+1", Some(2), 52), ("tada", None, 2)]);
        assert_eq!(
            message.reactions[0].users,
            [UserId::new("U1"), me()],
            "only the first are listed"
        );
    }

    #[test]
    fn bots_without_rich_text_use_mrkdwn_and_attachments() {
        let message = message(json!({
            "ts": "1.000000", "subtype": "bot_message", "username": "CI", "text": "*build* failed",
            "icons": {"image_48": "https://a/48.png"},
            "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": "ignored"}}],
            "attachments": [{"fallback": "details"}]
        }))
        .unwrap();
        assert_eq!(
            message.author,
            Author::Bot {
                name: "CI".into(),
                avatar: Some("https://a/48.png".into())
            }
        );
        assert_eq!(
            message.content.blocks,
            [
                Block::Paragraph(vec![
                    Inline::Text {
                        text: "build".into(),
                        style: Style::BOLD
                    },
                    Inline::Text {
                        text: " failed".into(),
                        style: Style::PLAIN
                    },
                ]),
                Block::Quote(vec![Inline::Text {
                    text: "details".into(),
                    style: Style::PLAIN
                }]),
            ]
        );
    }

    #[test]
    fn replies_point_at_their_parent_unless_broadcast() {
        let reply = message(json!({"ts": "2.0", "user": "U1", "thread_ts": "1.0"})).unwrap();
        assert_eq!(reply.reply_to, Some(seconds(1)));
        let broadcast = message(
            json!({"ts": "2.0", "user": "U1", "thread_ts": "1.0", "subtype": "thread_broadcast"}),
        )
        .unwrap();
        assert_eq!(broadcast.reply_to, None);
    }

    #[test]
    fn socket_frames_become_live_events() {
        assert_eq!(socket_message(r#"{"type":"hello"}"#), SocketMessage::Hello);
        assert_eq!(
            socket_message(r#"{"type":"reconnect_url","url":"wss://again"}"#),
            SocketMessage::ReconnectUrl("wss://again".into())
        );
        assert_eq!(
            socket_message(r#"{"type":"im_marked","channel":"D1","ts":"5.000000"}"#),
            SocketMessage::Live(LiveEvent::Marked {
                conversation: ConversationId::new("D1"),
                up_to: seconds(5)
            })
        );
        assert_eq!(
            socket_message(
                r#"{"type":"reaction_added","user":"UME","reaction":"tada","item":{"type":"message","channel":"C1","ts":"7.000000"}}"#
            ),
            SocketMessage::Live(LiveEvent::Reacted {
                conversation: ConversationId::new("C1"),
                ts: seconds(7),
                emoji: Emoji::named("tada"),
                user: me(),
                change: ReactionChange::Added,
            })
        );
        let SocketMessage::Live(LiveEvent::Posted {
            conversation,
            message,
        }) = socket_message(
            r#"{"type":"message","channel":"C1","user":"U2","text":"hi","ts":"8.000000"}"#,
        )
        else {
            panic!("expected a posted message")
        };
        assert_eq!((conversation.as_str(), message.ts), ("C1", seconds(8)));
        assert_eq!(
            socket_message(
                r#"{"type":"message","subtype":"message_deleted","channel":"C1","deleted_ts":"8.000000"}"#
            ),
            SocketMessage::Live(LiveEvent::Deleted {
                conversation: ConversationId::new("C1"),
                ts: seconds(8)
            })
        );
        assert_eq!(
            socket_message(r#"{"type":"user_typing"}"#),
            SocketMessage::Ignored
        );
        assert_eq!(
            socket_message(r#"{"type":"channel_joined","channel":{"id":"C9","name":"new"}}"#),
            SocketMessage::Appeared(ConversationId::new("C9"))
        );
        assert_eq!(
            socket_message(r#"{"type":"im_open","user":"U2","channel":"D9"}"#),
            SocketMessage::Appeared(ConversationId::new("D9"))
        );
        assert_eq!(
            socket_message(r#"{"type":"mpim_joined"}"#),
            SocketMessage::Ignored
        );
        assert_eq!(socket_message("not json"), SocketMessage::Ignored);
    }

    #[test]
    fn conversation_info_lists_what_the_sidebar_would() {
        let listed = |channel: Value| {
            let info: ConversationInfo =
                serde_json::from_value(json!({ "channel": channel })).unwrap();
            info.into_listed()
        };
        let (dm, activity) =
            listed(json!({"id": "D9", "is_im": true, "user": "U2", "last_read": "4.000000"}))
                .unwrap();
        assert_eq!(
            (dm.kind, dm.name.as_str()),
            (
                ConversationKind::Dm {
                    with: UserId::new("U2")
                },
                ""
            )
        );
        assert_eq!(
            (activity.read_up_to(), activity.has_unread()),
            (seconds(4), false)
        );

        let (channel, _) =
            listed(json!({"id": "C9", "name": "new", "is_private": true, "is_member": true}))
                .unwrap();
        assert_eq!(
            (channel.kind, channel.name.as_str()),
            (ConversationKind::Channel { private: true }, "new")
        );
        let (group, _) =
            listed(json!({"id": "G9", "name": "mpdm-ana--bo-1", "is_mpim": true})).unwrap();
        assert_eq!(
            (group.kind, group.name.as_str()),
            (ConversationKind::GroupDm, "ana, bo")
        );

        assert_eq!(
            listed(json!({"id": "C8", "name": "x", "is_member": false})),
            None
        );
        assert_eq!(
            listed(json!({"id": "C7", "name": "x", "is_archived": true})),
            None
        );
        assert_eq!(
            listed(json!({"id": "D8", "is_im": true})),
            None,
            "a DM with nobody"
        );
    }

    #[test]
    fn image_files_show_their_best_rendition() {
        let image = |file: Value| {
            let posted = message(json!({"ts": "1.0", "user": "U1", "files": [file]})).unwrap();
            posted.files[0].image.clone()
        };
        let rendition = |url: &str, width, height| {
            Some(Image {
                url: url.into(),
                width,
                height,
            })
        };
        let big = json!({
            "name": "shot.png", "mimetype": "image/png",
            "thumb_360": "https://f/360", "thumb_360_w": 360, "thumb_360_h": 203,
            "thumb_480": "https://f/480", "thumb_480_w": 480, "thumb_480_h": 270,
            "thumb_720": "https://f/720", "thumb_720_w": 720, "thumb_720_h": 405,
            "thumb_1024": "https://f/1024", "thumb_1024_w": 1024, "thumb_1024_h": 576
        });
        assert_eq!(image(big), rendition("https://f/720", 720, 405));
        let small = json!({
            "name": "icon.png", "mimetype": "image/png",
            "thumb_360": "https://f/360", "thumb_360_w": 300, "thumb_360_h": 300
        });
        assert_eq!(image(small), rendition("https://f/360", 300, 300));
        let gif = json!({
            "name": "party.gif", "mimetype": "image/gif",
            "thumb_360": "https://f/360", "thumb_360_w": 360, "thumb_360_h": 360,
            "thumb_360_gif": "https://f/360.gif",
            "thumb_480": "https://f/480", "thumb_480_w": 480, "thumb_480_h": 480
        });
        assert_eq!(
            image(gif),
            rendition("https://f/360.gif", 360, 360),
            "animated"
        );
        let document =
            json!({"name": "notes.pdf", "mimetype": "application/pdf", "thumb_360": "https://f/p"});
        assert_eq!(image(document), None, "no size, no image");
        assert_eq!(image(json!({"name": "gone", "mode": "tombstone"})), None);
    }

    #[test]
    fn reactions_got_in_full_name_everyone() {
        let reacted: ReactedTo = serde_json::from_value(json!({
            "ok": true, "type": "message", "channel": "C1",
            "message": {"ts": "1.000000", "user": "U1", "text": "hi",
                        "reactions": [{"name": "tada", "count": 3, "users": ["U1", "U2", "UME"]}]}
        }))
        .unwrap();
        let reactions = reacted.into_reactions().unwrap();
        assert_eq!(
            reactions
                .iter()
                .map(|r| (r.emoji.name.as_str(), r.users.len(), r.count))
                .collect::<Vec<_>>(),
            [("tada", 3, 3)]
        );
        let unreadable: ReactedTo =
            serde_json::from_value(json!({"message": {"text": "no ts"}})).unwrap();
        assert_eq!(unreadable.into_reactions(), None);
    }

    #[test]
    fn files_link_to_their_page_when_slack_gives_one() {
        let link = |file: Value| {
            let posted = message(json!({"ts": "1.0", "user": "U1", "files": [file]})).unwrap();
            posted.files[0].link.clone()
        };
        assert_eq!(
            link(json!({"name": "a.pdf", "permalink": "https://x.slack.com/files/U1/F1/a.pdf"})),
            Some("https://x.slack.com/files/U1/F1/a.pdf".into())
        );
        assert_eq!(
            link(json!({"name": "gone", "mode": "hidden_by_limit"})),
            None
        );
    }

    #[test]
    fn image_blocks_show_inline_when_slack_measured_them() {
        let posted = message(
            json!({"ts": "1.0", "bot_id": "B1", "text": "fallback", "blocks": [
                {"type": "image", "image_url": "https://media4.giphy.com/media/x/giphy.gif",
                 "image_width": 480, "image_height": 270, "image_bytes": 1000, "is_animated": true,
                 "alt_text": "a dancing cat", "title": {"type": "plain_text", "text": "dance"}},
                {"type": "image", "image_url": "https://unmeasured.example/i.png", "alt_text": "?"},
                {"type": "context", "elements": []}
            ]}),
        )
        .unwrap();
        assert_eq!(
            posted.content.blocks,
            [Block::Image {
                image: Image {
                    url: "https://media4.giphy.com/media/x/giphy.gif".into(),
                    width: 480,
                    height: 270,
                },
                alt_text: "a dancing cat".into(),
            }]
        );
    }

    #[test]
    fn followed_threads_come_with_their_read_markers() {
        let view: ThreadView = serde_json::from_value(json!({
            "total_unread_replies": 1,
            "threads": [
                {"root_msg": {"ts": "10.000000", "thread_ts": "10.000000", "user": "U2", "text": "parent",
                    "channel": "C1", "reply_count": 2, "latest_reply": "12.000000", "last_read": "11.000000",
                    "subscribed": true},
                 "latest_replies": []},
                {"root_msg": {"ts": "20.000000", "user": "U2", "channel": "C1", "latest_reply": "21.000000",
                    "last_read": "21.000000"}},
                {"root_msg": {"ts": "30.000000", "user": "U2", "channel": "C1", "subscribed": false}},
                {"root_msg": {"user": "U2", "channel": "C1"}}
            ]
        }))
        .unwrap();
        let followed = view.into_followed();
        let parents: Vec<Ts> = followed.iter().map(|thread| thread.parent.ts).collect();
        assert_eq!(
            parents,
            [seconds(10), seconds(20)],
            "subscribed and well-formed only"
        );
        assert_eq!(followed[0].conversation, ConversationId::new("C1"));
        assert_eq!(followed[0].activity.read_up_to(), seconds(11));
        assert!(followed[0].activity.has_unread());
        assert!(!followed[1].activity.has_unread());
    }

    #[test]
    fn thread_subscription_frames_become_live_events() {
        let frame = |kind: &str, subscription_type: &str| {
            format!(
                r#"{{"type":"{kind}","subscription":{{"type":"{subscription_type}","channel":"C1","thread_ts":"10.000000","last_read":"12.000000","active":true}}}}"#
            )
        };
        let (conversation, parent) = (ConversationId::new("C1"), seconds(10));
        assert_eq!(
            socket_message(&frame("thread_marked", "thread")),
            SocketMessage::Live(LiveEvent::ThreadMarked {
                conversation: conversation.clone(),
                parent,
                up_to: seconds(12)
            })
        );
        assert_eq!(
            socket_message(&frame("thread_subscribed", "thread")),
            SocketMessage::Live(LiveEvent::ThreadFollowed {
                conversation: conversation.clone(),
                parent,
                read_up_to: seconds(12)
            })
        );
        assert_eq!(
            socket_message(&frame("thread_unsubscribed", "thread")),
            SocketMessage::Live(LiveEvent::ThreadUnfollowed {
                conversation,
                parent
            })
        );
        assert_eq!(
            socket_message(&frame("thread_marked", "channel")),
            SocketMessage::Ignored
        );
    }

    #[test]
    fn a_replies_page_holds_the_parent_and_says_where_to_go_next() {
        let page: Replies = serde_json::from_value(json!({
            "messages": [
                {"ts": "1.000000", "user": "U1", "text": "parent", "thread_ts": "1.000000", "reply_count": 1},
                {"ts": "2.000000", "user": "U2", "text": "reply", "thread_ts": "1.000000"}
            ],
            "has_more": true,
            "response_metadata": {"next_cursor": "abc"}
        }))
        .unwrap();
        assert_eq!(page.next_cursor(), "abc");
        let replies: Vec<Option<Ts>> = page.into_messages().map(|m| m.reply_to).collect();
        assert_eq!(replies, [None, Some(seconds(1))]);
    }

    #[test]
    fn an_edited_reply_is_passed_on() {
        let frame = r#"{"type":"message","subtype":"message_changed","channel":"C1",
            "message":{"ts":"2.000000","user":"U2","text":"fixed","thread_ts":"1.000000","edited":{"user":"U2","ts":"3.0"}}}"#;
        let SocketMessage::Live(LiveEvent::Edited { message, .. }) = socket_message(frame) else {
            panic!("expected an edit")
        };
        assert_eq!((message.reply_to, message.edited), (Some(seconds(1)), true));
    }

    #[test]
    fn users_are_named_by_display_name_first() {
        let info: UserInfo = serde_json::from_value(json!({
            "user": {"id": "U1", "name": "handle", "real_name": "Real", "tz": "Europe/Paris",
                     "profile": {"display_name": "", "real_name": "Profile Real", "title": "Engineer",
                                 "image_48": "https://a/u1.png"}}
        }))
        .unwrap();
        assert_eq!(
            info.into_user(),
            User {
                id: UserId::new("U1"),
                name: "Profile Real".into(),
                full_name: "Profile Real".into(),
                title: "Engineer".into(),
                time_zone: Some("Europe/Paris".into()),
                avatar: Some("https://a/u1.png".into()),
            }
        );
        let bare: UserInfo =
            serde_json::from_value(json!({"user": {"id": "U2", "tz": ""}})).unwrap();
        let bare = bare.into_user();
        assert_eq!(
            (bare.name.as_str(), bare.title.as_str(), bare.time_zone),
            ("U2", "", None)
        );
    }

    #[test]
    fn people_found_keep_their_order_and_leave_deactivated_accounts_out() {
        let found: PeopleSearch = serde_json::from_value(json!({
            "ok": true,
            "results": [
                {"id": "U1", "name": "ada", "real_name": "Ada Lovelace", "profile": {"display_name": "Ada", "real_name": "Ada Lovelace"}},
                {"id": "U2", "name": "gone", "deleted": true, "profile": {"display_name": "Gone"}},
                {"id": "U3", "name": "alan", "profile": {"display_name": "", "real_name": ""}}
            ]
        }))
        .unwrap();
        assert_eq!(
            found.into_people(),
            [
                Person {
                    id: UserId::new("U1"),
                    name: "Ada".into(),
                    full_name: "Ada Lovelace".into(),
                },
                Person {
                    id: UserId::new("U3"),
                    name: "alan".into(),
                    full_name: String::new(),
                },
            ]
        );
    }

    #[test]
    fn emoji_list_keeps_images_and_aliases() {
        let list: EmojiList = serde_json::from_value(json!({
            "emoji": {"parrot": "https://emoji/parrot.gif", "pp": "alias:parrot"}
        }))
        .unwrap();
        let set = list.into_set();
        assert_eq!(
            set.resolve("pp"),
            Some(slackers_model::CustomEmoji::Image {
                url: "https://emoji/parrot.gif"
            })
        );
    }
}
