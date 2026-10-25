# slackers

A terminal Slack client that organises Conversations by subject across Slack Workspaces, rather than one Workspace
at a time.

## Language

### Where conversations live

**Workspace**:
A Slack workspace, or a whole Enterprise Grid org, that the user is signed in to with one session, owning its own
members, Conversations and Custom Emoji.
_Avoid_: team, server, org

**Virtual Workspace**:
A user-defined, named set of Channels, drawn from one or more Workspaces, that belong to the same subject.
_Avoid_: group, context, topic, space, project, section, "VW"

**Conversation**:
Anything in a Workspace that holds Messages: a Channel or a DM.
_Avoid_: chat, room

**Channel**:
A public or private Slack channel in a Workspace.
_Avoid_: room, group

**DM**:
A private Conversation between the user and one or more members of the same Workspace.
_Avoid_: IM, MPIM, chat

### What conversations hold

**Message**:
A post in a Conversation, identified by its timestamp within that Conversation.
_Avoid_: post, event

**Ephemeral Message**:
A Message an app shows the user alone, such as Giphy's preview of a GIF before it's sent. Slack doesn't keep it,
so history never has it.
_Avoid_: hidden message, private message

**Thread**:
The Replies to one Message, which is the Thread's parent.

**Reply**:
A Message posted in a Thread rather than directly in the Conversation.

**Draft**:
What the user is writing in a Conversation or a Thread, not posted yet. Each Conversation and each Thread has its
own. Editing one of the user's Messages writes the edit as the Draft where the Message is, setting aside what was
there until the edit is saved or given up.
_Avoid_: composer (that's the box a Draft is written in)

**Reaction**:
An emoji that people added to a Message, shown once per emoji with how many added it and who.
_Avoid_: emoji (a Reaction uses one)

**Custom Emoji**:
An image, possibly animated, that a Workspace defines under a `:name:`.
_Avoid_: reaction (a reaction uses an emoji; it isn't one)

### Keeping up

**Followed Thread**:
A Thread the user started, replied to or chose to follow.
_Avoid_: subscription

**Read Marker**:
The point in a Conversation, or in a Followed Thread, up to which the user has read.
_Avoid_: last_read, cursor

**Unread**:
What the user hasn't read yet: Messages past the Read Marker in a Conversation they're a member of, or Replies past
the Read Marker of a Followed Thread.
_Avoid_: new, unseen

**Mention**:
A Message that @-mentions the user. Mentions are counted separately, on top of what's Unread.
_Avoid_: ping, highlight

**Activity**:
What happens in the user's Workspaces that concerns them, as Slack's Activity lists it: Mentions, Reactions to their
Messages, Replies in their Followed Threads, DM Messages, and being added to Channels. Each item is about one Message.
slackers merges every Workspace's, newest first.
_Avoid_: notifications, feed, inbox

## Relationships

- A **Workspace** has many **Conversations**; each **Conversation** belongs to exactly one **Workspace**.
- A **Virtual Workspace** holds **Channels** from one or more **Workspaces**. A **Channel** can be in any number of
  **Virtual Workspaces**, including none.
- A **DM** stays with its **Workspace** and never joins a **Virtual Workspace**. The same person in two
  **Workspaces** is two unrelated people to slackers.
- A **Message** belongs to one **Conversation**; a **Reply** also belongs to one **Thread**.
- A **Custom Emoji** belongs to one **Workspace**: `:yay:` in one **Workspace** and `:yay:` in another are different
  emoji.
- **Activity** is merged from every **Workspace**: each of its items belongs to one **Workspace**, and is about one
  **Message** there.
- A **Virtual Workspace** shows the combined **Unread** Messages and **Mentions** of its **Channels**. Reading a **Channel**
  in one **Virtual Workspace** clears it in all of them, because the **Read Marker** belongs to the **Channel**.

## Example dialogue

> **Dev:** "Project X has channels in both the Red Hat and the CNCF **Workspaces**. Is that one **Virtual Workspace**?"
> **User:** "Yes. One **Virtual Workspace**, 'Project X', holding the Red Hat channel and the CNCF channel. It lights up
> when either has anything **Unread**."
> **Dev:** "And when a Project X maintainer DMs me in both **Workspaces**?"
> **User:** "Those are two **DMs**, one under each **Workspace**. slackers doesn't link people across **Workspaces**."

## Flagged ambiguities

- "team" (Slack API: `team_id`, `team.info`) means **Workspace**.
- On Enterprise Grid, the org is one **Workspace** to slackers: Red Hat's session is org-wide, and Conversation ids,
  members and Custom Emoji are shared across the org. The org's member workspaces aren't modelled.
- "workspace" alone always means a real Slack **Workspace**. Always write **Virtual Workspace** in full, in prose and
  in code (`VirtualWorkspace`), never shortened to "workspace" or "VW".
- Slack's API "conversation" covers channels, IMs and MPIMs, which matches **Conversation**. Slack's legacy "group" is
  a private **Channel**, not a **Virtual Workspace**.
- "DMs remain workspace centric" was read as: **DMs** never join a **Virtual Workspace**, and people are never linked
  across **Workspaces**.
- Open: whether `@here`/`@channel`, and every **Message** in a **DM**, count as **Mentions**.
- An @-mention the user writes in a **Draft**, of a person or of `@here`/`@channel`, notifies others. It isn't a
  **Mention**: only a **Message** that @-mentions the user is.
- Open: a Slack Connect **Channel** shared between two of the user's **Workspaces**. Is it one **Channel** or two?
- Slack's "Activity" tab is **Activity**; what it calls "unreads" is **Unread**. Until 2026-10-25, slackers called
  **Unread** "Activity".
