//! slackers' behaviour, observed the way a user meets it (the kit's D19): keys in, screen and effects out, through
//! the kit's test harness. Slack is the messages the tests send as a Session would.

use std::time::Duration;

use jiff::tz::TimeZone;
use slackers_model::{
    ActivityItem, Author, Block, Button, ConversationId, ConversationKind, CustomEmojiSet, Emoji,
    Happened, Inline, MentionOf, Message, MessageRef, Presence, Press, Reaction, ReactionChange,
    RichText, ThreadSummary, Ts, Unread, UserId, WorkspaceId,
};
use slackers_slack::{
    ActivityPage, Bot, Connection, EmojiList, Error, FollowedThread, HistoryPage, LiveEvent, Part,
    Permalinks, Person, Request, SlashCommand, Snapshot, Update, User,
};
use wcgw_kit::gfx::Graphics;
use wcgw_kit::key::Mouse;
use wcgw_kit::testing::Harness;

use super::*;
use crate::config::ChannelRef;

const W0: WorkspaceKey = WorkspaceKey(0);
const W1: WorkspaceKey = WorkspaceKey(1);

type Test = Harness<Slackers>;

fn seconds(s: u64) -> Ts {
    Ts::from_micros(s * 1_000_000)
}

fn id(id: &str) -> ConversationId {
    ConversationId::new(id)
}

fn conversation(id: &str, kind: ConversationKind, name: &str) -> slackers_model::Conversation {
    slackers_model::Conversation {
        id: ConversationId::new(id),
        kind,
        name: name.into(),
    }
}

fn channel(id: &str, name: &str) -> slackers_model::Conversation {
    conversation(id, ConversationKind::Channel { private: false }, name)
}

fn message(s: u64, author: &str, text: &str) -> Message {
    Message {
        ts: seconds(s),
        author: Author::User(UserId::new(author)),
        content: RichText::plain(text),
        reply_to: None,
        thread: None,
        reactions: Vec::new(),
        files: Vec::new(),
        edited: false,
        ephemeral: false,
    }
}

fn reply(s: u64, parent: u64, author: &str, text: &str) -> Message {
    Message {
        reply_to: Some(seconds(parent)),
        ..message(s, author, text)
    }
}

fn user(id: &str, name: &str) -> User {
    User {
        id: UserId::new(id),
        name: name.into(),
        full_name: String::new(),
        title: String::new(),
        time_zone: None,
        avatar: None,
    }
}

fn snapshot(name: &str, conversations: Vec<(slackers_model::Conversation, Unread)>) -> Snapshot {
    Snapshot {
        workspace: WorkspaceId::new("T1"),
        name: name.into(),
        icon: None,
        me: UserId::new("UME"),
        permalinks: Permalinks::new("https://test.slack.com/"),
        conversations,
        followed_threads: Vec::new(),
    }
}

/// The first Workspace's Conversations: two Channels and a DM with Bob (`U9`), `#random` having unread Messages.
fn test_snapshot() -> Snapshot {
    let dm = ConversationKind::Dm {
        with: UserId::new("U9"),
    };
    snapshot(
        "Test",
        vec![
            (conversation("D1", dm, ""), Unread::default()),
            (
                channel("C2", "random"),
                Unread::counted(seconds(1), seconds(5), true, 0),
            ),
            (
                conversation("C1", ConversationKind::Channel { private: true }, "general"),
                Unread::default(),
            ),
        ],
    )
}

/// slackers signing in to `labels`, listing `virtual_workspaces`, in a `width` × `height` terminal, with plain
/// icons, times in UTC and no pictures.
fn starting(
    labels: &[&str],
    virtual_workspaces: Vec<VirtualWorkspace>,
    width: u16,
    height: u16,
) -> Test {
    let labels = labels.iter().map(|label| (*label).to_owned()).collect();
    let (mut app, mut kit) = Slackers::session(labels, virtual_workspaces);
    app.zone = TimeZone::UTC;
    kit.set_nerd_font_symbols(false);
    let _ = kit.set_graphics(Graphics::Off);
    Harness::new(app, kit, width, height)
}

/// News from the first Workspace.
fn slack(test: &mut Test, update: Update) -> Effects {
    test.send(Msg::Slack(W0, update))
}

/// slackers signed in to `test`, as [`test_snapshot`] shows it, in a terminal 100 × 20, knowing Bob's name.
fn ready() -> Test {
    let mut test = starting(&["test"], Vec::new(), 100, 20);
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    slack(&mut test, Update::User(user("U9", "Bob")));
    test
}

fn history(conversation: &str, messages: Vec<Message>, has_more: bool) -> Update {
    Update::History {
        conversation: id(conversation),
        result: Ok(HistoryPage { messages, has_more }),
    }
}

/// The requests to the first Workspace among `effects`.
fn asked(effects: &Effects) -> Vec<Request> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::App(Io::Slack(W0, request)) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

/// Whether `effects` ask the first Workspace for `request`.
fn asks(effects: &Effects, request: &Request) -> bool {
    asked(effects).contains(request)
}

fn rows(test: &Test) -> Vec<String> {
    test.screen().rows
}

/// What the screen says, row by row.
fn screen(test: &Test) -> String {
    rows(test).join("\n")
}

/// Whether some row of the screen says `text`.
fn shows(test: &Test, text: &str) -> bool {
    rows(test).iter().any(|row| row.contains(text))
}

/// The column and row where `text` first shows.
fn cell_of(test: &Test, text: &str) -> (u16, u16) {
    rows(test)
        .iter()
        .enumerate()
        .find_map(|(y, row)| {
            let at = row.find(text)?;
            let x = unicode_width::UnicodeWidthStr::width(&row[..at]);
            Some((
                u16::try_from(x).expect("narrow"),
                u16::try_from(y).expect("short"),
            ))
        })
        .unwrap_or_else(|| panic!("{text:?} isn't on screen:\n{}", screen(test)))
}

/// Clicks `mouse`'s button on `text`: once, `Mouse::Left`, or twice, `Mouse::DoubleLeft`.
fn click(test: &mut Test, mouse: Mouse, text: &str) -> Effects {
    let (column, row) = cell_of(test, text);
    test.update(wcgw_kit::Msg::Mouse { mouse, column, row })
}

/// Double clicks on `text`, which opens what it lands on, as in every app.
fn click_on(test: &mut Test, text: &str) -> Effects {
    click(test, Mouse::DoubleLeft, text)
}

/// The rows of the window right of the sidebar, from its winbar down.
fn main_rows(test: &Test) -> Vec<String> {
    rows(test)
        .iter()
        .map(|row| {
            row.split_once('│')
                .map_or(String::new(), |(_, right)| right.trim_end().to_owned())
        })
        .collect()
}

/// The sidebar's rows, trimmed.
fn sidebar(test: &Test) -> Vec<String> {
    rows(test)
        .iter()
        .map(|row| row.split('│').next().unwrap_or("").trim().to_owned())
        .collect()
}

/// What the command line says.
fn told(test: &Test) -> String {
    rows(test).last().cloned().unwrap_or_default()
}

/// The buffer of `place`, which must have been opened.
fn buffer(test: &Test, place: &Place) -> BufferId {
    test.app.places[place]
}

/// The text of the composer of `place`, which must have been opened.
fn composed(test: &Test, place: &Place) -> String {
    test.kit
        .composer(buffer(test, place))
        .expect("a composer")
        .text()
}

/// What the composer of `place` offers to complete the word with, as it would write each.
fn offered(test: &Test, place: &Place) -> Vec<String> {
    test.kit
        .composer(buffer(test, place))
        .expect("a composer")
        .suggestions()
        .iter()
        .map(|suggestion| suggestion.insert.clone())
        .collect()
}

/// The Message under the cursor of the current window.
fn under_cursor(test: &Test) -> Option<(Place, Ts)> {
    Slackers::message_under_cursor(&test.kit).ok()
}

fn place(conversation: &str, thread: Option<u64>) -> Place {
    Place {
        conversation: ConversationRef {
            workspace: W0,
            id: id(conversation),
        },
        thread: thread.map(seconds),
    }
}

fn workspace(test: &Test) -> &Workspace {
    test.app.workspaces.get(W0).expect("signed in")
}

fn unread(test: &Test, conversation: &str) -> bool {
    workspace(test).unread[&id(conversation)].has_unread()
}

fn posted(test: &mut Test, conversation: &str, message: Message) -> Effects {
    slack(
        test,
        Update::Live(LiveEvent::Posted {
            conversation: id(conversation),
            message,
        }),
    )
}

fn edited(test: &mut Test, conversation: &str, message: Message) -> Effects {
    slack(
        test,
        Update::Live(LiveEvent::Edited {
            conversation: id(conversation),
            message,
        }),
    )
}

fn text(blocks: Vec<Block>) -> RichText {
    RichText { blocks }
}

/// `ready`, with `#random` opened from the sidebar and holding Ada's Messages at 2 and 5 seconds, the latter with a
/// Reply, read to its newest.
fn reading_random() -> Test {
    let mut test = ready();
    test.press("gg4j<CR>");
    let parent = Message {
        thread: Some(ThreadSummary {
            replies: 1,
            latest: seconds(6),
        }),
        ..message(5, "U2", "hi")
    };
    slack(
        &mut test,
        history("C2", vec![parent, message(2, "U2", "hello")], true),
    );
    slack(&mut test, Update::User(user("U2", "Ada")));
    // It opened on its first unread Message: on to the newest, which reads it.
    test.press("G");
    test
}

// --- Signing in, and the sidebar. ---

#[test]
fn the_sign_in_page_says_how_each_workspace_goes() {
    let mut test = starting(&["test", "other"], Vec::new(), 100, 20);
    assert!(shows(&test, "signing in to test…"), "{}", screen(&test));
    assert!(shows(&test, "signing in to other…"));
    test.send(Msg::CantSignIn {
        workspace: W1,
        reason: "no token in the keyring".into(),
    });
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    assert!(shows(&test, "signed in to Test"));
    assert!(shows(&test, "couldn't sign in to other"));
    assert!(shows(&test, "no token in the keyring"));
    assert!(shows(&test, "test connecting"), "the statusline");
    assert!(shows(&test, "other signed out"));
}

#[test]
fn signing_in_lists_the_sidebar_and_asks_for_names_presence_and_activity() {
    let mut test = starting(&["test"], Vec::new(), 100, 20);
    let effects = slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    assert!(asks(&effects, &Request::User(UserId::new("U9"))));
    assert!(asks(
        &effects,
        &Request::FollowPresence(vec![UserId::new("U9")])
    ));
    assert!(asks(&effects, &Request::Activity { cursor: None }));
    assert_eq!(
        sidebar(&test)[..6],
        [
            "! Activity",
            "▾ ↳ Threads",
            "▾ T Test                    •",
            "* general",
            "# random                  •",
            "@ U9",
        ],
        "Channels by name, then DMs; what's unread has a dot"
    );
    slack(&mut test, Update::User(user("U9", "Bob")));
    assert!(
        shows(&test, "@ Bob"),
        "a DM is named after the other person"
    );
    let effects = test.press("<CR>");
    assert!(
        asks(
            &effects,
            &Request::History {
                conversation: id("C2"),
                before: None
            }
        ),
        "the cursor waited on the first unread, not opening it"
    );
}

#[test]
fn the_sidebar_suggests_the_first_unread_only_until_the_user_moves() {
    let mut test = starting(&["test", "other"], Vec::new(), 100, 20);
    test.press("j");
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    let effects = test.press("<CR>");
    assert!(
        !asks(
            &effects,
            &Request::History {
                conversation: id("C2"),
                before: None
            }
        ),
        "the user moved first"
    );
}

#[test]
fn sections_fold_as_vim_folds() {
    let mut test = ready();
    test.press("ggjjjza");
    assert!(!shows(&test, "general"), "{}", screen(&test));
    assert!(shows(&test, "▸ T Test"), "the closed fold's summary");
    test.press("zo");
    assert!(shows(&test, "general"));
    test.press("zM");
    assert!(!shows(&test, "general") && shows(&test, "▸ ↳ Threads"));
    test.press("zR");
    assert!(shows(&test, "general") && shows(&test, "▾ ↳ Threads"));
}

#[test]
fn virtual_workspaces_come_first_and_take_their_channels_out_of_their_workspaces() {
    let virtual_workspaces = vec![VirtualWorkspace {
        name: "K".into(),
        channels: vec![
            ChannelRef {
                workspace: "test".into(),
                channel: "random".into(),
            },
            ChannelRef {
                workspace: "other".into(),
                channel: "dev".into(),
            },
        ],
    }];
    let mut test = starting(&["test", "other"], virtual_workspaces, 100, 20);
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    let other = snapshot(
        "Other",
        vec![
            (channel("C5", "general"), Unread::default()),
            (channel("C6", "dev"), Unread::default()),
        ],
    );
    test.send(Msg::Slack(W1, Update::Ready(Box::new(other))));
    assert_eq!(
        sidebar(&test)[..10],
        [
            "! Activity",
            "▾ ↳ Threads",
            "▾ ≡ K                       •",
            "T # random                  •",
            "O # dev",
            "▾ T Test",
            "* general",
            "@ U9",
            "▾ O Other",
            "# general",
        ],
        "each Channel of a Virtual Workspace says which Workspace it's from"
    );
}

#[test]
fn a_virtual_workspace_listing_a_channel_the_user_isnt_in_says_so() {
    let virtual_workspaces = vec![VirtualWorkspace {
        name: "K".into(),
        channels: vec![ChannelRef {
            workspace: "test".into(),
            channel: "gone".into(),
        }],
    }];
    let mut test = starting(&["test"], virtual_workspaces, 100, 20);
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    assert_eq!(
        told(&test),
        "K lists test#gone, which isn't among your channels"
    );
}

#[test]
fn a_conversation_that_appears_joins_the_sidebar_where_it_belongs() {
    let mut test = ready();
    let nothing = ActivityPage {
        items: Vec::new(),
        next: None,
    };
    slack(
        &mut test,
        Update::Activity {
            cursor: None,
            result: Ok(nothing),
        },
    );
    let effects = slack(
        &mut test,
        Update::Live(LiveEvent::Joined {
            conversation: channel("C3", "jobs"),
            unread: Unread::default(),
        }),
    );
    assert_eq!(
        sidebar(&test)[3..7],
        [
            "* general",
            "# jobs",
            "# random                  •",
            "@ Bob"
        ]
    );
    assert!(asks(&effects, &Request::Activity { cursor: None }));
}

// --- Opening and reading. ---

#[test]
fn opening_a_conversation_loads_it_beside_the_sidebar_and_goes_there() {
    let mut test = ready();
    let effects = test.press("ggjjj<CR>");
    assert!(asks(
        &effects,
        &Request::History {
            conversation: id("C1"),
            before: None
        }
    ));
    let main = main_rows(&test);
    assert_eq!(main[0], " T * general", "the winbar");
    assert!(main[1].contains("loading…"));
    assert!(shows(&test, "NORMAL  #general"), "{}", screen(&test));
    slack(
        &mut test,
        history("C1", vec![message(1, "U9", "morning")], false),
    );
    let main = main_rows(&test);
    assert!(main.contains(&"○  Bob  00:00".to_owned()), "{main:#?}");
    assert!(main.contains(&"   morning".to_owned()));
    test.press("<C-w>h");
    let effects = test.press("<CR>");
    assert!(asked(&effects).is_empty(), "loaded already");
}

#[test]
fn messages_hang_off_their_authors_and_group_under_one_header() {
    let mut test = ready();
    test.press("ggjjj<CR>");
    let long =
        "a message long enough that it has to wrap to stay within the window beside the sidebar";
    slack(
        &mut test,
        history(
            "C1",
            vec![
                message(90_000, "U9", "the next day"),
                message(400, "U9", "later on"),
                message(61, "U9", "again"),
                message(60, "U9", long),
            ],
            false,
        ),
    );
    let main = main_rows(&test);
    let from = main
        .iter()
        .position(|row| row.contains("Thu 1 Jan 1970"))
        .expect("a day's rule");
    assert_eq!(
        main[from..from + 9],
        [
            "                       ─── Thu 1 Jan 1970 ───",
            "○  Bob  00:01",
            "   a message long enough that it has to wrap to stay within the",
            "   window beside the sidebar",
            " · again",
            "○  Bob  00:06",
            "   later on",
            "                       ─── Fri 2 Jan 1970 ───",
            "○  Bob  01:00",
        ],
        "{main:#?}"
    );
}

#[test]
fn a_conversation_opens_on_its_first_unread_under_a_rule_and_is_read_at_its_newest() {
    let mut test = ready();
    test.press("gg4j<CR>");
    let effects = slack(
        &mut test,
        history(
            "C2",
            vec![message(5, "U2", "new"), message(1, "U2", "old")],
            false,
        ),
    );
    assert!(
        asked(&effects)
            .iter()
            .all(|request| !matches!(request, Request::Mark { .. }))
    );
    let main = main_rows(&test);
    let rule = main
        .iter()
        .position(|row| row.contains("─── new ───"))
        .expect("the rule");
    assert!(main[rule + 2].ends_with("new"), "{main:#?}");
    assert!(main[rule - 1].ends_with("old"));
    assert_eq!(
        under_cursor(&test),
        Some((place("C2", None), seconds(5))),
        "on the first unread"
    );
    assert!(unread(&test, "C2"));
    let effects = test.press("G");
    assert!(asks(
        &effects,
        &Request::Mark {
            conversation: id("C2"),
            up_to: seconds(5)
        }
    ));
    assert!(!unread(&test, "C2"));
    assert!(!rows(&test)[4].contains('•'), "the sidebar's dot goes");
}

#[test]
fn nothing_is_read_while_the_terminal_lacks_focus() {
    let mut test = ready();
    test.press("gg4j<CR>");
    test.update(wcgw_kit::Msg::FocusLost);
    slack(
        &mut test,
        history("C2", vec![message(5, "U2", "new")], false),
    );
    let effects = test.press("G");
    assert!(asked(&effects).is_empty());
    let effects = test.update(wcgw_kit::Msg::FocusGained);
    assert!(asks(
        &effects,
        &Request::Mark {
            conversation: id("C2"),
            up_to: seconds(5)
        }
    ));
}

#[test]
fn what_arrives_while_reading_at_the_newest_is_read_and_followed() {
    let mut test = reading_random();
    let effects = posted(&mut test, "C2", message(7, "U2", "fresh"));
    assert!(asks(
        &effects,
        &Request::Mark {
            conversation: id("C2"),
            up_to: seconds(7)
        }
    ));
    assert_eq!(
        under_cursor(&test),
        Some((place("C2", None), seconds(7))),
        "the window followed"
    );
}

#[test]
fn reaching_the_top_loads_older_history_once() {
    let mut test = ready();
    test.press("ggjjj<CR>");
    let page: Vec<Message> = (2..20)
        .rev()
        .map(|s| message(s * 600, "U9", "something"))
        .collect();
    slack(&mut test, history("C1", page, true));
    let histories = |effects: &Effects| {
        asked(effects)
            .into_iter()
            .filter(|request| matches!(request, Request::History { .. }))
            .count()
    };
    let effects = test.press("k");
    assert_eq!(histories(&effects), 0, "the top isn't in view");
    let effects = test.press("gg");
    assert!(asks(
        &effects,
        &Request::History {
            conversation: id("C1"),
            before: Some(seconds(1200))
        }
    ));
    let effects = test.press("j");
    assert_eq!(histories(&effects), 0, "on its way already");
    slack(
        &mut test,
        history("C1", vec![message(1, "U2", "first")], false),
    );
    let effects = test.press("gg");
    assert_eq!(histories(&effects), 0, "there's no more");
    assert!(main_rows(&test).iter().any(|row| row.ends_with("first")));
}

#[test]
fn reaching_back_for_the_first_unread_loads_pages_until_it_is_there() {
    let mut test = starting(&["test"], Vec::new(), 100, 20);
    let mut snapshot = test_snapshot();
    snapshot.conversations[1].1 = Unread::counted(seconds(1), seconds(9), true, 0);
    slack(&mut test, Update::Ready(Box::new(snapshot)));
    test.press("gg4j<CR>");
    let effects = slack(
        &mut test,
        history("C2", vec![message(9, "U2", "nine")], true),
    );
    assert!(asks(
        &effects,
        &Request::History {
            conversation: id("C2"),
            before: Some(seconds(9))
        }
    ));
    let effects = slack(
        &mut test,
        history(
            "C2",
            vec![message(4, "U2", "four"), message(1, "U2", "one")],
            true,
        ),
    );
    assert!(
        !asked(&effects)
            .iter()
            .any(|request| matches!(request, Request::History { .. })),
        "reached"
    );
    assert_eq!(under_cursor(&test), Some((place("C2", None), seconds(4))));
}

#[test]
fn a_thread_opens_in_place_of_its_conversation_on_a_narrow_screen_and_beside_it_on_a_wide_one() {
    let mut test = reading_random();
    let effects = test.press("k<CR>");
    assert!(asks(
        &effects,
        &Request::Replies {
            conversation: id("C2"),
            parent: seconds(5)
        }
    ));
    slack(
        &mut test,
        Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result: Ok(vec![message(5, "U2", "hi"), reply(6, 5, "U9", "hey")]),
        },
    );
    let main = main_rows(&test);
    assert_eq!(
        main[1], " ↳ thread in T # random",
        "below the tabline: {main:#?}"
    );
    assert!(main.iter().any(|row| row.contains("─── 1 reply ───")));
    assert!(main.contains(&"   hey".to_owned()));
    test.update(wcgw_kit::Msg::Resize {
        width: 160,
        height: 20,
    });
    let winbars = &rows(&test)[1];
    assert!(
        winbars.contains("│ T # random") && winbars.contains("│ ↳ thread in T # random"),
        "side by side:\n{}",
        screen(&test)
    );
    test.update(wcgw_kit::Msg::Resize {
        width: 100,
        height: 20,
    });
    assert_eq!(
        main_rows(&test)[1],
        " ↳ thread in T # random",
        "{}",
        screen(&test)
    );
    test.press("<C-t>");
    assert_eq!(
        main_rows(&test)[1],
        " T # random",
        "back to the conversation"
    );
}

#[test]
fn a_thread_opened_on_a_wide_screen_goes_beside_and_q_closes_it() {
    let mut test = reading_random();
    test.update(wcgw_kit::Msg::Resize {
        width: 160,
        height: 20,
    });
    test.press("k<CR>");
    assert!(
        rows(&test)[1].contains("│ ↳ thread in T # random"),
        "{}",
        screen(&test)
    );
    assert!(shows(&test, "NORMAL  #random › 00:00"), "it has focus");
    test.press("q");
    assert!(!shows(&test, "thread in"), "{}", screen(&test));
    assert!(shows(&test, "NORMAL  #random"));
}

#[test]
fn backslash_hides_the_sidebar_from_within_it_goes_to_it_and_brings_it_back() {
    let mut test = reading_random();
    test.press("<Bslash>");
    assert!(shows(&test, "NORMAL  sidebar"), "{}", screen(&test));
    test.press("<Bslash>");
    assert!(!shows(&test, "Activity"), "{}", screen(&test));
    assert_eq!(
        rows(&test)[0],
        " T Test › # random",
        "the winbar names the section, breadcrumb-style"
    );
    assert!(
        rows(&test)[1].contains(&" ".repeat(38)),
        "laid out again for the whole width"
    );
    test.press("<Bslash>");
    assert!(shows(&test, "Activity"));
}

#[test]
fn a_double_click_opens_what_it_lands_on_and_a_click_alone_does_not() {
    let mut test = ready();
    let effects = click(&mut test, Mouse::Left, "general");
    assert!(effects.is_empty(), "{effects:?}");
    assert_ne!(main_rows(&test)[0], " T * general");
    let effects = click_on(&mut test, "general");
    assert!(asks(
        &effects,
        &Request::History {
            conversation: id("C1"),
            before: None
        }
    ));
    assert_eq!(main_rows(&test)[0], " T * general");
}

#[test]
fn leader_f_finds_a_conversation_by_name() {
    let mut test = ready();
    test.press("<Space>f");
    assert!(shows(&test, "Conversations"), "{}", screen(&test));
    test.press("rand<CR>");
    assert_eq!(main_rows(&test)[0], " T # random");
}

#[test]
fn e_opens_a_conversation_by_its_buffer_name() {
    let mut test = ready();
    test.press(":e @Bob<CR>");
    assert_eq!(main_rows(&test)[0], " T @ Bob", "{}", screen(&test));
}

// --- Writing. ---

#[test]
fn i_writes_in_the_composer_and_enter_posts() {
    let mut test = reading_random();
    test.press("ihello there");
    assert!(shows(&test, "INSERT"));
    let effects = test.press("<CR>");
    assert!(asks(
        &effects,
        &Request::Post {
            conversation: id("C2"),
            thread: None,
            text: vec![Part::Text("hello there".into())],
            id: 0,
        }
    ));
    test.advance(Duration::ZERO);
    assert!(shows(&test, "sending"), "{}", screen(&test));
    assert!(shows(&test, "   hello there"));
    slack(
        &mut test,
        Update::Posted {
            id: 0,
            conversation: id("C2"),
            result: Ok(message(8, "UME", "hello there")),
        },
    );
    assert!(!shows(&test, "sending"));
    assert_eq!(
        under_cursor(&test),
        Some((place("C2", None), seconds(8))),
        "the window followed what was sent"
    );
}

#[test]
fn what_wasnt_sent_goes_back_in_the_composer() {
    let mut test = reading_random();
    test.press("ioops<CR>later");
    slack(
        &mut test,
        Update::Posted {
            id: 0,
            conversation: id("C2"),
            result: Err(Error::SignedOut),
        },
    );
    assert_eq!(composed(&test, &place("C2", None)), "oops\nlater");
    assert!(
        told(&test).starts_with("not sent, back in the draft: signed out"),
        "{}",
        told(&test)
    );
}

#[test]
fn an_at_offers_whom_to_mention_and_the_label_posts_as_the_mention() {
    let mut test = reading_random();
    slack(&mut test, Update::User(user("U1", "Adam")));
    let effects = test.press("i@ad");
    assert!(asks(&effects, &Request::FindPeople { query: "a".into() }));
    assert_eq!(
        offered(&test, &place("C2", None)),
        ["@Ada ", "@Adam "],
        "who wrote here first"
    );
    test.press("<CR>");
    assert_eq!(composed(&test, &place("C2", None)), "@Ada ");
    assert!(
        shows(&test, " @Ada "),
        "the composer names whom it mentions"
    );
    let effects = test.press("hi<CR>");
    assert!(asks(
        &effects,
        &Request::Post {
            conversation: id("C2"),
            thread: None,
            text: vec![Part::User(UserId::new("U2")), Part::Text(" hi".into())],
            id: 0,
        }
    ));
}

#[test]
fn only_channels_offer_their_broadcasts() {
    let mut test = reading_random();
    test.press("i@he");
    assert_eq!(offered(&test, &place("C2", None)), ["@here "]);
    test.press("<Esc><Esc><C-w>h5j<CR>i@he");
    slack(&mut test, history("D1", Vec::new(), false));
    assert!(offered(&test, &place("D1", None)).is_empty());
}

#[test]
fn whom_slack_finds_joins_those_offered() {
    let mut test = reading_random();
    let effects = test.press("i@zo");
    let asked_for: Vec<Request> = asked(&effects)
        .into_iter()
        .filter(|request| matches!(request, Request::FindPeople { .. }))
        .collect();
    assert_eq!(
        asked_for,
        [Request::FindPeople { query: "z".into() }],
        "one search at a time"
    );
    let zoe = || {
        Ok(vec![Person {
            id: UserId::new("U7"),
            name: "Zoë".into(),
            full_name: "Zoë Example".into(),
        }])
    };
    let effects = slack(
        &mut test,
        Update::FoundPeople {
            query: "z".into(),
            result: zoe(),
        },
    );
    assert!(
        asks(&effects, &Request::FindPeople { query: "zo".into() }),
        "the name grew meanwhile"
    );
    slack(
        &mut test,
        Update::FoundPeople {
            query: "zo".into(),
            result: zoe(),
        },
    );
    assert_eq!(offered(&test, &place("C2", None)), ["@Zoë "]);
    let effects = test.press("<BS>");
    assert!(asked(&effects).is_empty(), "each name once");
}

#[test]
fn a_colon_and_two_more_offer_emoji_and_a_slash_the_workspaces_commands() {
    let mut test = reading_random();
    slack(
        &mut test,
        Update::Emoji(EmojiList {
            custom: CustomEmojiSet::default(),
            standard: vec!["tada".into(), "taco".into()],
        }),
    );
    slack(
        &mut test,
        Update::Commands(vec![SlashCommand {
            name: "/giphy".into(),
            description: "a gif".into(),
            usage: "[text]".into(),
        }]),
    );
    test.press("inice :ta");
    assert_eq!(offered(&test, &place("C2", None)), [":tada: ", ":taco: "]);
    test.press("<CR>");
    assert_eq!(composed(&test, &place("C2", None)), "nice :tada: ");
    for _ in 0.."nice :tada: ".len() {
        test.press("<BS>");
    }
    test.press("/gi");
    assert_eq!(offered(&test, &place("C2", None)), ["/giphy "]);
}

#[test]
fn a_slash_command_runs_rather_than_posting_and_an_unknown_one_stays_in_the_composer() {
    let mut test = reading_random();
    slack(
        &mut test,
        Update::Commands(vec![SlashCommand {
            name: "/giphy".into(),
            description: String::new(),
            usage: String::new(),
        }]),
    );
    let effects = test.press("i/giphy cats<CR>");
    assert!(asks(
        &effects,
        &Request::RunCommand {
            conversation: id("C2"),
            thread: None,
            command: "/giphy".into(),
            text: vec![Part::Text("cats".into())],
            id: 0,
        }
    ));
    let effects = test.press("/nope x<CR>");
    assert!(asked(&effects).is_empty());
    assert!(
        told(&test).starts_with("no /nope command here"),
        "{}",
        told(&test)
    );
    assert_eq!(composed(&test, &place("C2", None)), "/nope x");
}

#[test]
fn a_command_that_posts_for_the_user_elsewhere_is_followed_there() {
    let mut test = reading_random();
    test.press("i/dm @bob hi<CR><Esc>");
    let effects = posted(
        &mut test,
        "D1",
        Message {
            ts: now(),
            ..message(0, "UME", "hi")
        },
    );
    assert!(asks(
        &effects,
        &Request::History {
            conversation: id("D1"),
            before: None
        }
    ));
    assert_eq!(main_rows(&test)[1], " T @ Bob", "{}", screen(&test));
}

#[test]
fn shift_a_edits_ones_own_message_and_enter_saves_it() {
    let mut test = reading_random();
    posted(&mut test, "C2", message(7, "UME", "mine"));
    test.press("G");
    let effects = test.press("A");
    assert!(asked(&effects).is_empty());
    assert_eq!(composed(&test, &place("C2", None)), "mine");
    assert!(shows(&test, "INSERT"));
    assert!(shows(&test, " editing "), "{}", screen(&test));
    let effects = test.press("!<CR>");
    assert!(asks(
        &effects,
        &Request::Edit {
            conversation: id("C2"),
            ts: seconds(7),
            text: vec![Part::Text("mine!".into())],
        }
    ));
    assert!(!shows(&test, " editing "));
}

#[test]
fn someone_elses_message_cant_be_edited_and_esc_gives_an_edit_up() {
    let mut test = reading_random();
    test.press("A");
    assert_eq!(told(&test), "only your own messages can be edited");
    posted(&mut test, "C2", message(7, "UME", "ok"));
    test.press("idraft<Esc>GA");
    assert_eq!(composed(&test, &place("C2", None)), "ok");
    test.press("<BS><BS><CR>");
    assert_eq!(told(&test), "an edit can't be empty: <Esc> gives it up");
    assert!(shows(&test, " editing "), "still editing");
    test.press("<Esc>");
    assert_eq!(
        composed(&test, &place("C2", None)),
        "draft",
        "what was set aside comes back"
    );
    assert!(!shows(&test, " editing "));
    assert!(
        !shows(&test, "INSERT"),
        "back on the Message, as it started"
    );
}

#[test]
fn up_in_an_empty_composer_edits_ones_newest_and_esc_goes_back_to_writing() {
    let mut test = reading_random();
    posted(&mut test, "C2", message(7, "UME", "mine"));
    posted(&mut test, "C2", message(8, "U2", "theirs"));
    test.press("i<Up>");
    assert_eq!(composed(&test, &place("C2", None)), "mine");
    assert!(shows(&test, " editing "), "{}", screen(&test));
    let effects = test.press("!<CR>");
    assert!(asks(
        &effects,
        &Request::Edit {
            conversation: id("C2"),
            ts: seconds(7),
            text: vec![Part::Text("mine!".into())],
        }
    ));
    test.press("<Up><Esc>");
    assert!(composed(&test, &place("C2", None)).is_empty());
    assert!(shows(&test, "INSERT"), "back to writing, where it started");
    test.press("x<Up>");
    assert_eq!(
        composed(&test, &place("C2", None)),
        "x",
        "only an empty composer"
    );
}

#[test]
fn a_click_elsewhere_keeps_an_edit_for_later() {
    let mut test = reading_random();
    posted(&mut test, "C2", message(7, "UME", "mine"));
    test.press("GA!");
    click_on(&mut test, "general");
    assert!(!shows(&test, "INSERT"));
    assert_eq!(composed(&test, &place("C2", None)), "mine!");
    assert!(test.app.editing.contains_key(&place("C2", None)));
    test.press("<C-o>");
    assert!(shows(&test, " editing "), "{}", screen(&test));
}

#[test]
fn a_refused_edit_comes_back_to_the_composer() {
    let mut test = reading_random();
    posted(&mut test, "C2", message(7, "UME", "ok"));
    test.press("GA!<CR>");
    slack(
        &mut test,
        Update::Edited {
            conversation: id("C2"),
            ts: seconds(7),
            result: Err(Error::SignedOut),
        },
    );
    assert_eq!(composed(&test, &place("C2", None)), "ok!");
    assert!(told(&test).starts_with("not edited, back in the composer"));
}

// --- Acting on Messages. ---

#[test]
fn plus_picks_an_emoji_to_react_with_and_picking_ones_own_takes_it_back() {
    let mut test = reading_random();
    slack(
        &mut test,
        Update::Emoji(EmojiList {
            custom: CustomEmojiSet::default(),
            standard: vec!["+1".into(), "tada".into()],
        }),
    );
    test.press("k+");
    assert!(shows(&test, "React"), "{}", screen(&test));
    let effects = test.press("tada<CR>");
    assert!(asks(
        &effects,
        &Request::React {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: Emoji::named("tada"),
            change: ReactionChange::Added,
        }
    ));
    assert!(shows(&test, "🎉  1"), "shown at once: {}", screen(&test));
    test.press("+");
    assert!(shows(&test, ":tada:  remove"), "{}", screen(&test));
    let effects = test.press("<CR>");
    assert!(asks(
        &effects,
        &Request::React {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: Emoji::named("tada"),
            change: ReactionChange::Removed,
        }
    ));
    assert!(!shows(&test, "🎉"));
}

#[test]
fn a_refused_reaction_is_undone() {
    let mut test = reading_random();
    test.press(":react tada<CR>");
    assert!(shows(&test, "🎉  1"), "{}", screen(&test));
    slack(
        &mut test,
        Update::Reacted {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: Emoji::named("tada"),
            change: ReactionChange::Added,
            result: Err(Error::SignedOut),
        },
    );
    assert!(!shows(&test, "🎉"));
    assert!(told(&test).starts_with("couldn't react with :tada:"));
}

#[test]
fn reactions_show_how_many_reacted_and_yours_stand_out() {
    let mut test = reading_random();
    let reacted = |test: &mut Test, user: &str| {
        slack(
            test,
            Update::Live(LiveEvent::Reacted {
                conversation: id("C2"),
                ts: seconds(5),
                emoji: Emoji::named("+1"),
                user: UserId::new(user),
                author: Some(UserId::new("U2")),
                change: ReactionChange::Added,
            }),
        )
    };
    reacted(&mut test, "U9");
    assert!(shows(&test, "   👍  1"), "{}", screen(&test));
    let (x, y) = cell_of(&test, "👍");
    assert_eq!(
        test.style_at(x + 3, y).fg,
        test.kit.style(crate::hl::DIM).fg
    );
    reacted(&mut test, "UME");
    assert!(shows(&test, "👍  2"));
    let (x, y) = cell_of(&test, "👍");
    assert_eq!(
        test.style_at(x + 3, y).fg,
        test.kit.style(crate::hl::MENTION).fg,
        "the user's own"
    );
}

#[test]
fn k_shows_a_messages_details_and_asks_for_everyone_who_reacted() {
    let mut test = reading_random();
    edited(
        &mut test,
        "C2",
        Message {
            reactions: vec![Reaction {
                emoji: Emoji::named("+1"),
                users: vec![UserId::new("U9")],
                count: 3,
            }],
            content: text(vec![Block::Paragraph(vec![Inline::Link {
                url: "https://example.com/a".into(),
                label: Some("the docs".into()),
            }])]),
            ..message(5, "U2", "")
        },
    );
    let effects = test.press("kK");
    assert!(asks(
        &effects,
        &Request::Reactions {
            conversation: id("C2"),
            ts: seconds(5)
        }
    ));
    assert!(shows(&test, "Bob and 2 others"), "{}", screen(&test));
    assert!(shows(&test, "the docs  example.com"));
    assert!(shows(&test, "this message in Slack"));
    slack(
        &mut test,
        Update::Reactions {
            conversation: id("C2"),
            ts: seconds(5),
            result: Ok(vec![Reaction {
                emoji: Emoji::named("+1"),
                users: vec![UserId::new("U9"), UserId::new("UME"), UserId::new("U2")],
                count: 3,
            }]),
        },
    );
    assert!(
        shows(&test, "Bob, you, Ada"),
        "kept current: {}",
        screen(&test)
    );
    test.press("j");
    assert!(
        !shows(&test, "this message in Slack"),
        "gone with the next key"
    );
}

#[test]
fn gx_opens_a_messages_only_link_and_otherwise_picks_one() {
    let mut test = reading_random();
    let link = |url: &str| Inline::Link {
        url: url.into(),
        label: None,
    };
    edited(
        &mut test,
        "C2",
        Message {
            content: text(vec![Block::Paragraph(vec![link("https://a.com")])]),
            ..message(5, "U2", "")
        },
    );
    let effects = test.press("k0gx");
    assert!(effects.contains(&Effect::OpenUrl("https://a.com".into())));
    let send = Button {
        label: "Send".into(),
        primary: true,
        press: Press("send".into()),
    };
    edited(
        &mut test,
        "C2",
        Message {
            content: text(vec![
                Block::Paragraph(vec![link("https://a.com"), link("https://b.com")]),
                Block::Actions(vec![send.clone()]),
            ]),
            ..message(5, "U2", "")
        },
    );
    test.press("gx");
    assert!(
        shows(&test, "[ Send ]") && shows(&test, "https://b.com"),
        "{}",
        screen(&test)
    );
    let effects = test.press("Send<CR>");
    assert!(asks(
        &effects,
        &Request::Press {
            conversation: id("C2"),
            ts: seconds(5),
            button: send,
        }
    ));
}

#[test]
fn enter_on_a_link_opens_it_and_gy_yanks_it_or_the_messages() {
    let mut test = reading_random();
    edited(
        &mut test,
        "C2",
        Message {
            content: text(vec![Block::Paragraph(vec![Inline::Link {
                url: "https://a.com".into(),
                label: Some("there".into()),
            }])]),
            ..message(5, "U2", "")
        },
    );
    let effects = click_on(&mut test, "there");
    assert!(effects.contains(&Effect::OpenUrl("https://a.com".into())));
    let effects = test.press("gy");
    assert!(effects.contains(&Effect::Copy("https://a.com".into())));
    let effects = test.press("0gy");
    assert!(
        effects.iter().any(|effect| matches!(effect,
            Effect::Copy(url) if url.starts_with("https://test.slack.com/"))),
        "{effects:?}"
    );
    // gX opens the message itself in Slack, wherever the cursor is on it, as the other apps' gX.
    let effects = test.press("$gX");
    assert!(
        effects.iter().any(|effect| matches!(effect,
            Effect::OpenUrl(url) if url.starts_with("https://test.slack.com/"))),
        "{effects:?}"
    );
}

#[test]
fn shift_u_marks_messages_unread_and_the_conversation_stays_so_until_one_opens() {
    let mut test = reading_random();
    let effects = test.press("kkU");
    assert!(asks(
        &effects,
        &Request::Mark {
            conversation: id("C2"),
            up_to: Ts::from_micros(seconds(2).as_micros() - 1)
        }
    ));
    assert!(unread(&test, "C2"));
    let effects = test.press("G");
    assert!(asked(&effects).is_empty(), "kept unread");
    let effects = test.press("<C-w>h<CR>");
    assert!(
        asked(&effects).is_empty(),
        "opened again on its first unread"
    );
    assert_eq!(under_cursor(&test), Some((place("C2", None), seconds(2))));
    let effects = test.press("G");
    assert!(asks(
        &effects,
        &Request::Mark {
            conversation: id("C2"),
            up_to: seconds(5)
        }
    ));
}

#[test]
fn a_threads_replies_cant_be_marked_unread() {
    let mut test = reading_random();
    test.press("k<CR>");
    slack(
        &mut test,
        Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result: Ok(vec![message(5, "U2", "hi"), reply(6, 5, "U9", "hey")]),
        },
    );
    test.press("GU");
    assert_eq!(
        told(&test),
        "only a conversation's messages can be marked unread"
    );
}

// --- Keeping up. ---

#[test]
fn whos_typing_shows_on_the_statusline_until_they_stop_or_post() {
    let mut test = reading_random();
    let typing = |test: &mut Test| {
        slack(
            test,
            Update::Live(LiveEvent::Typing {
                conversation: id("C2"),
                thread: None,
                user: UserId::new("U9"),
            }),
        )
    };
    typing(&mut test);
    assert!(shows(&test, "Bob is typing…"), "{}", screen(&test));
    test.advance(Duration::from_secs(5));
    typing(&mut test);
    test.advance(Duration::from_secs(5));
    assert!(shows(&test, "Bob is typing…"), "said so again");
    test.advance(Duration::from_secs(2));
    assert!(!shows(&test, "Bob is typing…"));
    typing(&mut test);
    posted(&mut test, "C2", message(9, "U9", "done"));
    assert!(!shows(&test, "Bob is typing…"));
}

#[test]
fn the_statusline_says_which_workspaces_arent_live() {
    let mut test = ready();
    assert!(shows(&test, "test connecting"));
    slack(&mut test, Update::Connection(Connection::Live));
    assert!(shows(&test, "● live"), "{}", screen(&test));
    slack(
        &mut test,
        Update::Connection(Connection::Reconnecting { attempt: 2 }),
    );
    assert!(shows(&test, "test reconnecting (2)"));
}

/// The notifications among `effects`, as their titles and bodies.
fn notified(effects: &Effects) -> Vec<(String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Notify { title, body } => Some((title.clone(), body.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn dms_mentions_and_followed_replies_notify_while_the_terminal_is_elsewhere() {
    let mut test = ready();
    let effects = posted(&mut test, "D1", message(10, "U9", "focused"));
    assert!(notified(&effects).is_empty());
    test.update(wcgw_kit::Msg::FocusLost);
    let effects = posted(&mut test, "D1", message(11, "U9", "psst"));
    assert_eq!(notified(&effects), [("Bob".to_owned(), "psst".to_owned())]);
    let effects = posted(&mut test, "C1", message(12, "U9", "nothing for you"));
    assert!(notified(&effects).is_empty());
    test.update(wcgw_kit::Msg::FocusGained);
    let effects = posted(&mut test, "D1", message(14, "U9", "back"));
    assert!(notified(&effects).is_empty());
}

#[test]
fn a_held_notification_shows_once_the_names_in_it_are_known() {
    let mut test = ready();
    test.update(wcgw_kit::Msg::FocusLost);
    let mention = Message {
        content: text(vec![Block::Paragraph(vec![Inline::User(UserId::new(
            "UME",
        ))])]),
        ..message(13, "U8", "")
    };
    let effects = posted(&mut test, "C1", mention);
    assert!(notified(&effects).is_empty(), "held");
    assert!(asks(&effects, &Request::User(UserId::new("U8"))));
    slack(&mut test, Update::User(user("UME", "me")));
    let effects = slack(&mut test, Update::User(user("U8", "Cy")));
    assert_eq!(
        notified(&effects),
        [("Cy in #general".to_owned(), "@me".to_owned())]
    );
}

#[test]
fn live_messages_are_unread_but_not_ones_own() {
    let mut test = ready();
    posted(&mut test, "C1", message(10, "UME", "mine"));
    assert!(!unread(&test, "C1"));
    posted(&mut test, "C1", message(11, "U9", "theirs"));
    assert!(unread(&test, "C1"));
    assert!(rows(&test)[3].contains('•'), "{}", screen(&test));
}

#[test]
fn followed_threads_with_replies_to_read_head_the_sidebar_and_open_on_the_first_unread() {
    let mut test = starting(&["test"], Vec::new(), 100, 20);
    let mut snapshot = test_snapshot();
    snapshot.followed_threads = vec![FollowedThread {
        conversation: id("C1"),
        parent: message(5, "U9", "a question"),
        unread: Unread::counted(seconds(6), seconds(8), true, 0),
    }];
    slack(&mut test, Update::Ready(Box::new(snapshot)));
    slack(&mut test, Update::User(user("U9", "Bob")));
    assert!(
        sidebar(&test)[2].starts_with("T ↳ general: a question"),
        "{}",
        screen(&test)
    );
    let effects = test.press("ggjj<CR>");
    assert!(asks(
        &effects,
        &Request::Replies {
            conversation: id("C1"),
            parent: seconds(5)
        }
    ));
    slack(
        &mut test,
        Update::Replies {
            conversation: id("C1"),
            parent: seconds(5),
            result: Ok(vec![
                message(5, "U9", "a question"),
                reply(6, 5, "U9", "read"),
                reply(8, 5, "U9", "unread"),
            ]),
        },
    );
    assert_eq!(
        under_cursor(&test),
        Some((place("C1", Some(5)), seconds(8)))
    );
    let effects = test.press("G");
    assert!(asks(
        &effects,
        &Request::MarkThread {
            conversation: id("C1"),
            parent: seconds(5),
            up_to: seconds(8)
        }
    ));
}

#[test]
fn replying_to_a_thread_follows_it_and_its_replies_head_the_sidebar() {
    let mut test = reading_random();
    posted(&mut test, "C2", reply(9, 5, "UME", "me too"));
    assert!(
        workspace(&test)
            .followed
            .contains_key(&(id("C2"), seconds(5)))
    );
    posted(&mut test, "C2", reply(10, 5, "U9", "and me"));
    assert!(shows(&test, "↳ random: hi"), "{}", screen(&test));
    slack(
        &mut test,
        Update::Live(LiveEvent::ThreadUnfollowed {
            conversation: id("C2"),
            parent: seconds(5),
        }),
    );
    assert!(!shows(&test, "↳ random: hi"));
}

#[test]
fn presence_shows_on_a_dms_icon() {
    let mut test = ready();
    slack(
        &mut test,
        Update::Live(LiveEvent::Presence {
            people: vec![UserId::new("U9")],
            presence: Presence::Active,
        }),
    );
    assert_eq!(sidebar(&test)[5], "● Bob", "{}", screen(&test));
}

#[test]
fn bots_that_dont_say_their_names_are_looked_up_once() {
    let mut test = ready();
    test.press("ggjjj<CR>");
    let bot = |s| Message {
        author: Author::UnnamedBot(slackers_model::BotId::new("B1")),
        ..message(s, "", "beep")
    };
    let effects = slack(&mut test, history("C1", vec![bot(400), bot(1)], false));
    let asked_for_bot = asked(&effects)
        .iter()
        .filter(|request| matches!(request, Request::Bot(_)))
        .count();
    assert_eq!(asked_for_bot, 1);
    assert!(shows(&test, "bot  00:00"), "{}", screen(&test));
    slack(
        &mut test,
        Update::Bot(Bot {
            id: slackers_model::BotId::new("B1"),
            name: "giphy".into(),
            avatar: None,
        }),
    );
    assert!(shows(&test, "giphy  00:06"));
}

// --- Activity. ---

#[test]
fn activity_lists_what_happened_and_opens_an_items_message() {
    let mut test = ready();
    test.press("gg<CR>");
    assert_eq!(main_rows(&test)[0], " ! Activity", "{}", screen(&test));
    let mention = ActivityItem {
        key: "m1".into(),
        at: seconds(30),
        unread: true,
        message: MessageRef {
            conversation: id("C1"),
            ts: seconds(30),
            thread: None,
        },
        happened: Happened::Mention {
            author: UserId::new("U9"),
            of: MentionOf::User,
        },
    };
    let effects = slack(
        &mut test,
        Update::Activity {
            cursor: None,
            result: Ok(ActivityPage {
                items: vec![mention],
                next: None,
            }),
        },
    );
    assert!(asks(
        &effects,
        &Request::Message {
            conversation: id("C1"),
            ts: seconds(30)
        }
    ));
    assert!(
        shows(&test, "Bob mentioned you in #general"),
        "{}",
        screen(&test)
    );
    assert!(shows(&test, "1 unread"));
    slack(
        &mut test,
        Update::Message {
            conversation: id("C1"),
            ts: seconds(30),
            result: Ok(Some(message(30, "U9", "hey you"))),
        },
    );
    assert!(shows(&test, "       hey you"), "{}", screen(&test));
    let effects = test.press("<CR>");
    assert!(asks(
        &effects,
        &Request::History {
            conversation: id("C1"),
            before: None
        }
    ));
    assert_eq!(main_rows(&test)[1], " T * general", "{}", screen(&test));
    slack(
        &mut test,
        history(
            "C1",
            vec![message(40, "U9", "after"), message(30, "U9", "hey you")],
            false,
        ),
    );
    assert_eq!(
        under_cursor(&test),
        Some((place("C1", None), seconds(30))),
        "on the item's Message"
    );
}

// --- Commands. ---

#[test]
fn vw_add_puts_the_channel_in_a_virtual_workspace_and_rm_takes_it_out() {
    let mut test = reading_random();
    let effects = test.press(":vw add Stuff<CR>");
    assert!(
        effects.contains(&Effect::App(Io::SaveVirtualWorkspaces(vec![
            VirtualWorkspace {
                name: "Stuff".into(),
                channels: vec![ChannelRef {
                    workspace: "test".into(),
                    channel: "random".into()
                }],
            }
        ])))
    );
    assert!(shows(&test, "≡ Stuff"), "{}", screen(&test));
    test.press(":vw add stuff<CR>");
    assert_eq!(told(&test), "#random is in Stuff already");
    let effects = test.press(":vw rm Stuff<CR>");
    assert!(effects.contains(&Effect::App(Io::SaveVirtualWorkspaces(Vec::new()))));
    assert!(!shows(&test, "Stuff"));
    test.press(":vw<CR>");
    assert_eq!(told(&test), "usage: :vw add NAME, or :vw rm NAME");
}

#[test]
fn dms_stay_out_of_virtual_workspaces() {
    let mut test = ready();
    test.press("gg5j<CR>:vw add K<CR>");
    assert_eq!(
        told(&test),
        "DMs stay with their workspace: only channels join a virtual workspace"
    );
}

#[test]
fn saving_the_config_can_fail() {
    let mut test = ready();
    test.send(Msg::SaveFailed("read-only".into()));
    assert_eq!(
        told(&test),
        "couldn't save the virtual workspaces: read-only"
    );
}

// --- Pictures. ---

#[test]
fn pictures_come_through_their_workspace() {
    let mut test = ready();
    let _ = test.kit.set_graphics(Graphics::HalfBlocks);
    test.press("ggjjj<CR>");
    slack(
        &mut test,
        Update::User(User {
            avatar: Some("https://avatars/bob.png".into()),
            ..user("U9", "Bob")
        }),
    );
    let effects = slack(
        &mut test,
        history("C1", vec![message(1, "U9", "hi")], false),
    );
    let pictures: Vec<&str> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Picture { source, .. } => Some(source.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(pictures, ["0 https://avatars/bob.png"]);
    assert_eq!(
        picture_of(pictures[0]),
        Some((W0, "https://avatars/bob.png"))
    );
}

#[test]
fn a_thread_shows_on_its_parent_with_how_many_replies_and_when_the_last_came() {
    let test = reading_random();
    let main = main_rows(&test);
    let hi = main.iter().position(|row| row == " · hi").expect("grouped");
    assert_eq!(main[hi + 1], "   ↳ 1  00:00", "{main:#?}");
}

#[test]
fn files_show_by_name_and_images_take_their_room() {
    let mut test = ready();
    test.press("ggjjj<CR>");
    let shared = Message {
        files: vec![
            slackers_model::File {
                name: "notes.pdf".into(),
                mimetype: "application/pdf".into(),
                link: Some("https://files.slack.com/notes.pdf".into()),
                image: None,
            },
            slackers_model::File {
                name: "cat.png".into(),
                mimetype: "image/png".into(),
                link: None,
                image: Some(slackers_model::Image {
                    url: "https://files.slack.com/cat.png".into(),
                    width: 100,
                    height: 60,
                }),
            },
        ],
        ..message(1, "U9", "look")
    };
    slack(&mut test, history("C1", vec![shared], false));
    let main = main_rows(&test);
    let file = main
        .iter()
        .position(|row| row == "   ▤ notes.pdf")
        .unwrap_or_else(|| panic!("{main:#?}"));
    assert_eq!(
        main[file + 1..file + 4],
        ["", "", ""],
        "an image 3 rows tall, drawn by the kit"
    );
    test.press("G");
    let effects = test.press("3k$<CR>");
    assert!(effects.contains(&Effect::OpenUrl("https://files.slack.com/notes.pdf".into())));
}

#[test]
fn with_several_workspaces_a_notification_names_its_own() {
    let mut test = starting(&["test", "other"], Vec::new(), 100, 20);
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    slack(&mut test, Update::User(user("U9", "Bob")));
    test.update(wcgw_kit::Msg::FocusLost);
    let effects = posted(&mut test, "D1", message(11, "U9", "psst"));
    assert_eq!(
        notified(&effects),
        [("Bob · Test".to_owned(), "psst".to_owned())]
    );
}

#[test]
fn two_virtual_workspaces_of_one_name_are_two_sections() {
    let named_k = |channel: &str| VirtualWorkspace {
        name: "K".into(),
        channels: vec![ChannelRef {
            workspace: "test".into(),
            channel: channel.into(),
        }],
    };
    let mut test = starting(
        &["test"],
        vec![named_k("random"), named_k("general")],
        100,
        20,
    );
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    assert_eq!(
        sidebar(&test)[2..6],
        [
            "▾ ≡ K                       •",
            "T # random                  •",
            "▾ ≡ K",
            "T * general"
        ]
    );
}

#[test]
fn enter_on_a_sections_header_folds_it() {
    let mut test = ready();
    test.press("ggjj<CR>");
    assert!(!shows(&test, "general"), "{}", screen(&test));
    test.press("<CR>");
    assert!(shows(&test, "general"));
    click_on(&mut test, "T Test");
    assert!(!shows(&test, "general"), "a double click too");
}

#[test]
fn the_message_under_the_cursor_is_tinted_whole() {
    let test = reading_random();
    let cursorline = test.kit.style(wcgw_kit::hl::Group::CURSOR_LINE).bg;
    let (x, y) = cell_of(&test, "· hi");
    assert_eq!(test.style_at(x + 20, y).bg, cursorline, "{}", screen(&test));
    let (x, y) = cell_of(&test, "↳ 1");
    assert_eq!(test.style_at(x, y).bg, cursorline, "every row of it");
    let (x, y) = cell_of(&test, "hello");
    assert_ne!(test.style_at(x, y).bg, cursorline, "not the one before");
}

#[test]
fn an_activity_item_goes_to_its_message_in_a_conversation_shown_before() {
    let mut test = ready();
    test.press("ggjjj<CR>");
    slack(
        &mut test,
        history(
            "C1",
            vec![message(40, "U9", "after"), message(30, "U9", "hey you")],
            false,
        ),
    );
    assert_eq!(under_cursor(&test), Some((place("C1", None), seconds(40))));
    test.press(":activity<CR>");
    let mention = ActivityItem {
        key: "m1".into(),
        at: seconds(30),
        unread: true,
        message: MessageRef {
            conversation: id("C1"),
            ts: seconds(30),
            thread: None,
        },
        happened: Happened::Mention {
            author: UserId::new("U9"),
            of: MentionOf::User,
        },
    };
    slack(
        &mut test,
        Update::Activity {
            cursor: None,
            result: Ok(ActivityPage {
                items: vec![mention],
                next: None,
            }),
        },
    );
    test.press("gg<CR>");
    assert_eq!(under_cursor(&test), Some((place("C1", None), seconds(30))));
}

#[test]
fn messages_are_laid_out_again_for_a_window_split_beside() {
    let mut test = reading_random();
    let long =
        "a message long enough that it wraps once the window beside the sidebar is split in two";
    posted(&mut test, "C2", message(7, "U2", long));
    let whole = " · a message long enough that it wraps once the window beside the";
    assert!(shows(&test, whole), "{}", screen(&test));
    test.press(":vsplit<CR>j");
    assert!(!shows(&test, whole), "{}", screen(&test));
    assert!(shows(&test, "│ · a message long enough that it"));
}

#[test]
fn a_dms_buffer_takes_the_name_of_whom_its_with_once_known() {
    let mut test = starting(&["test"], Vec::new(), 100, 20);
    slack(&mut test, Update::Ready(Box::new(test_snapshot())));
    test.press("gg5j<CR>");
    assert!(shows(&test, "NORMAL  @U9"), "{}", screen(&test));
    slack(&mut test, Update::User(user("U9", "Bob")));
    assert!(shows(&test, "NORMAL  @Bob"), "{}", screen(&test));
}

#[test]
fn the_connections_are_coloured_by_how_they_stand() {
    let mut test = ready();
    let (x, y) = cell_of(&test, "test connecting");
    assert_eq!(test.style_at(x, y).fg, test.kit.style(crate::hl::ACCENT).fg);
    slack(&mut test, Update::Connection(Connection::Live));
    let (x, y) = cell_of(&test, "live");
    assert_eq!(test.style_at(x, y).fg, test.kit.style(crate::hl::LIVE).fg);
}

#[test]
fn help_opens_beside_the_sidebar_which_keeps_its_width() {
    let mut test = ready();
    test.press(":help<CR>");
    assert!(shows(&test, "NORMAL  help"));
    assert_eq!(cell_of(&test, "│").0, 30, "{}", screen(&test));
}

#[test]
fn quitting_waits_for_what_is_on_its_way_to_slack() {
    let mut test = reading_random();
    let quits = |effects: &Effects| effects.contains(&Effect::Quit);
    test.press("isoon<CR><Esc>");
    assert!(!quits(&test.press(":qa<CR>")));
    assert!(
        told(&test).starts_with("quitting once Slack has what you sent"),
        "{}",
        told(&test)
    );
    let effects = slack(
        &mut test,
        Update::Posted {
            id: 0,
            conversation: id("C2"),
            result: Ok(message(9, "UME", "soon")),
        },
    );
    assert!(quits(&effects));
}

#[test]
fn q_quits_from_the_conversation_or_the_sidebar_rather_than_leave_the_sidebar_alone() {
    let mut test = reading_random();
    assert!(test.press("q").contains(&Effect::Quit), "{}", screen(&test));
    let mut test = reading_random();
    test.press("<Bslash>");
    assert!(shows(&test, "NORMAL  sidebar"), "{}", screen(&test));
    assert!(test.press("q").contains(&Effect::Quit), "{}", screen(&test));
    // A window of its own, as :help's, closes.
    let mut test = reading_random();
    test.press(":help<CR>");
    assert!(!test.press("q").contains(&Effect::Quit));
    assert!(!shows(&test, "NORMAL  help"), "{}", screen(&test));
}

#[test]
fn quitting_with_nothing_on_its_way_or_a_bang_is_at_once() {
    let mut test = reading_random();
    assert!(test.press(":qa<CR>").contains(&Effect::Quit));
    let mut test = reading_random();
    test.press("isoon<CR><Esc>");
    assert!(test.press(":qa!<CR>").contains(&Effect::Quit));
}

#[test]
fn ctrl_y_in_the_open_picker_yanks_a_link() {
    let mut test = reading_random();
    let link = |url: &str| Inline::Link {
        url: url.into(),
        label: None,
    };
    edited(
        &mut test,
        "C2",
        Message {
            content: text(vec![Block::Paragraph(vec![
                link("https://a.com"),
                link("https://b.com"),
            ])]),
            ..message(5, "U2", "")
        },
    );
    test.press("kgx");
    assert!(shows(&test, "<C-y> yank"), "{}", screen(&test));
    let effects = test.press("b.com<C-y>");
    assert!(effects.contains(&Effect::Copy("https://b.com".into())));
    assert_eq!(told(&test), "Yanked https://b.com");
}

#[test]
fn k_twice_enters_the_details_and_enter_opens_where_a_row_leads() {
    let mut test = reading_random();
    let send = Button {
        label: "Send".into(),
        primary: true,
        press: Press("send".into()),
    };
    edited(
        &mut test,
        "C2",
        Message {
            content: text(vec![
                Block::Paragraph(vec![Inline::Link {
                    url: "https://example.com/a".into(),
                    label: Some("the docs".into()),
                }]),
                Block::Actions(vec![send.clone()]),
            ]),
            ..message(5, "U2", "")
        },
    );
    test.press("kKK");
    assert!(shows(&test, "the docs  example.com"), "{}", screen(&test));
    let effects = test.press("Gk<CR>");
    assert!(
        effects.contains(&Effect::OpenUrl("https://example.com/a".into())),
        "{effects:?}\n{}",
        screen(&test)
    );
    assert!(!shows(&test, "the docs  example.com"), "the card goes");
    let effects = test.press("KKGkk<CR>");
    assert!(asks(
        &effects,
        &Request::Press {
            conversation: id("C2"),
            ts: seconds(5),
            button: send,
        }
    ));
    test.press("KK<Esc>");
    assert!(!shows(&test, "the docs  example.com"), "left");
}

#[test]
fn a_workspaces_icon_shows_in_the_winbar() {
    let mut test = starting(&["test"], Vec::new(), 100, 20);
    let _ = test.kit.set_graphics(Graphics::HalfBlocks);
    let mut snapshot = test_snapshot();
    snapshot.icon = Some("https://icons/test.png".into());
    let effects = slack(&mut test, Update::Ready(Box::new(snapshot)));
    assert!(
        effects.iter().any(|effect| matches!(effect,
            Effect::Picture { source, cols: 2, rows: 1, .. } if source == "0 https://icons/test.png")),
        "the sidebar's, which the winbar's shares: {effects:?}"
    );
    test.press("ggjjj<CR>");
    assert_eq!(
        main_rows(&test)[0],
        " \u{2800}\u{2800}* general",
        "room for it: {}",
        screen(&test)
    );
}
