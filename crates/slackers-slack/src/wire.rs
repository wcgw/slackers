//! Slack's JSON, and how it maps onto the model. The only module that knows Slack's field names.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;
use slackers_model::{
    Activity, Author, Block, Broadcast, Conversation, ConversationId, ConversationKind,
    CustomEmojiSet, Emoji, File, Inline, Message, Reaction, ReactionChange, RichText, Style,
    ThreadSummary, Ts, UserId, WorkspaceId,
};

use crate::mrkdwn;
use crate::session::{HistoryPage, LiveEvent, Snapshot, User};

/// Slack's timestamp text as a [`Ts`]; anything unparsable, such as an absent `latest`, reads as zero.
fn ts(text: &str) -> Ts {
    text.parse().unwrap_or_default()
}

#[derive(Deserialize)]
pub(crate) struct AuthTest {
    /// The team id, or the enterprise id for an org-level session.
    team_id: String,
    user_id: String,
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
pub(crate) fn snapshot(auth: AuthTest, boot: UserBoot, counts: Counts) -> Snapshot {
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
    Snapshot {
        workspace: WorkspaceId::new(auth.team_id),
        name: boot.team.name,
        me: UserId::new(auth.user_id),
        conversations,
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

#[derive(Deserialize)]
pub(crate) struct History {
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    has_more: bool,
}

impl History {
    pub(crate) fn into_page(self, me: &UserId) -> HistoryPage {
        HistoryPage {
            messages: self
                .messages
                .into_iter()
                .filter_map(|m| message(m, me))
                .collect(),
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
fn message(value: Value, me: &UserId) -> Option<Message> {
    let wire: WireMessage = serde_json::from_value(value)
        .inspect_err(
            |error| tracing::warn!(%error, "skipping a message slack sent in an unknown shape"),
        )
        .ok()?;
    let ts = ts(&wire.ts);
    let author = match (&wire.bot_profile, wire.user, wire.username) {
        (Some(bot), _, _) => Author::Bot {
            name: bot.name.clone(),
        },
        (None, Some(user), _) => Author::User(UserId::new(user)),
        (None, None, name) => Author::Bot {
            name: name.unwrap_or_else(|| "bot".to_owned()),
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
        .map(|reaction| Reaction {
            mine: reaction.users.iter().any(|user| user == me.as_str()),
            emoji: reaction_emoji(&reaction.name),
            count: reaction.count.max(1),
        })
        .collect();
    let files = wire
        .files
        .into_iter()
        .map(|file| File {
            name: file.name,
            mimetype: file.mimetype,
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
            WireBlock::Other => Vec::new(),
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
    profile: Profile,
}

#[derive(Default, Deserialize)]
struct Profile {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    real_name: String,
}

impl UserInfo {
    pub(crate) fn into_user(self) -> User {
        let user = self.user;
        let name = [
            user.profile.display_name,
            user.profile.real_name,
            user.real_name,
            user.name,
        ]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| user.id.clone());
        User {
            id: UserId::new(user.id),
            name,
        }
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
    /// Anything we don't follow.
    Ignored,
}

/// Interprets one frame from the live socket; `me` decides which reactions are the user's.
pub(crate) fn socket_message(frame: &str, me: &UserId) -> SocketMessage {
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
                let Some(edited) = message(event["message"].clone(), me) else {
                    return SocketMessage::Ignored;
                };
                // An edited Reply isn't shown in its Conversation, and mustn't count as a new Reply.
                if edited.reply_to.is_some() {
                    return SocketMessage::Ignored;
                }
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
                let Some(posted) = message(event.clone(), me) else {
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
            mine: text("user") == me.as_str(),
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
        let auth: AuthTest =
            serde_json::from_value(json!({"team_id": "E1", "user_id": "UME"})).unwrap();
        let boot: UserBoot = serde_json::from_value(json!({
            "self": {"id": "UME"},
            "team": {"name": "Org"},
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

        let snapshot = snapshot(auth, boot, counts);
        assert_eq!(
            (
                snapshot.workspace.as_str(),
                snapshot.name.as_str(),
                snapshot.me.as_str()
            ),
            ("E1", "Org", "UME")
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
        let message = message(
            json!({
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
                "reactions": [{"name": "+1::skin-tone-2", "count": 2, "users": ["U1", "UME"]}],
                "reply_count": 3, "latest_reply": "200.000000", "thread_ts": "100.000001",
                "edited": {"user": "U1", "ts": "101.000000"}
            }),
            &me(),
        )
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
        let thumbs = &message.reactions[0];
        assert_eq!(
            (
                thumbs.emoji.name.as_str(),
                thumbs.emoji.skin_tone,
                thumbs.count,
                thumbs.mine
            ),
            ("+1", Some(2), 2, true)
        );
    }

    #[test]
    fn bots_without_rich_text_use_mrkdwn_and_attachments() {
        let message = message(
            json!({
                "ts": "1.000000", "subtype": "bot_message", "username": "CI", "text": "*build* failed",
                "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": "ignored"}}],
                "attachments": [{"fallback": "details"}]
            }),
            &me(),
        )
        .unwrap();
        assert_eq!(message.author, Author::Bot { name: "CI".into() });
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
        let reply = message(
            json!({"ts": "2.0", "user": "U1", "thread_ts": "1.0"}),
            &me(),
        )
        .unwrap();
        assert_eq!(reply.reply_to, Some(seconds(1)));
        let broadcast = message(
            json!({"ts": "2.0", "user": "U1", "thread_ts": "1.0", "subtype": "thread_broadcast"}),
            &me(),
        )
        .unwrap();
        assert_eq!(broadcast.reply_to, None);
    }

    #[test]
    fn socket_frames_become_live_events() {
        assert_eq!(
            socket_message(r#"{"type":"hello"}"#, &me()),
            SocketMessage::Hello
        );
        assert_eq!(
            socket_message(r#"{"type":"reconnect_url","url":"wss://again"}"#, &me()),
            SocketMessage::ReconnectUrl("wss://again".into())
        );
        assert_eq!(
            socket_message(
                r#"{"type":"im_marked","channel":"D1","ts":"5.000000"}"#,
                &me()
            ),
            SocketMessage::Live(LiveEvent::Marked {
                conversation: ConversationId::new("D1"),
                up_to: seconds(5)
            })
        );
        assert_eq!(
            socket_message(
                r#"{"type":"reaction_added","user":"UME","reaction":"tada","item":{"type":"message","channel":"C1","ts":"7.000000"}}"#,
                &me()
            ),
            SocketMessage::Live(LiveEvent::Reacted {
                conversation: ConversationId::new("C1"),
                ts: seconds(7),
                emoji: Emoji::named("tada"),
                mine: true,
                change: ReactionChange::Added,
            })
        );
        let SocketMessage::Live(LiveEvent::Posted {
            conversation,
            message,
        }) = socket_message(
            r#"{"type":"message","channel":"C1","user":"U2","text":"hi","ts":"8.000000"}"#,
            &me(),
        )
        else {
            panic!("expected a posted message")
        };
        assert_eq!((conversation.as_str(), message.ts), ("C1", seconds(8)));
        assert_eq!(
            socket_message(
                r#"{"type":"message","subtype":"message_deleted","channel":"C1","deleted_ts":"8.000000"}"#,
                &me()
            ),
            SocketMessage::Live(LiveEvent::Deleted {
                conversation: ConversationId::new("C1"),
                ts: seconds(8)
            })
        );
        assert_eq!(
            socket_message(r#"{"type":"user_typing"}"#, &me()),
            SocketMessage::Ignored
        );
        assert_eq!(socket_message("not json", &me()), SocketMessage::Ignored);
    }

    #[test]
    fn users_are_named_by_display_name_first() {
        let info: UserInfo = serde_json::from_value(json!({
            "user": {"id": "U1", "name": "handle", "real_name": "Real", "profile": {"display_name": "", "real_name": "Profile Real"}}
        }))
        .unwrap();
        assert_eq!(
            info.into_user(),
            User {
                id: UserId::new("U1"),
                name: "Profile Real".into()
            }
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
