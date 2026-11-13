# slackers

A terminal Slack client that groups Conversations by subject across Workspaces, rather than one Workspace at a time.

![slackers in Ghostty: CNCF's #praxis, with its newest post's Thread open beside it](docs/screenshot.png)

Channels from different Workspaces that belong to the same subject (a project, a team) go together in a **Virtual
Workspace**, which lights up when any of them has something unread. DMs stay with their Workspace. The words used
here, and in the code, are defined in [CONTEXT.md](CONTEXT.md).

## What it does

- **Every Workspace at once**: the sidebar lists your Virtual Workspaces, then each Workspace's other Channels and
  DMs, with unread and Mention badges; **Activity** merges every Workspace's Mentions, Reactions, Thread Replies and
  DMs, newest first; your Followed Threads with unread Replies have a section of their own.
- **Picks up where you left off**: a Conversation or Followed Thread opens on its first unread Message, under a "new"
  rule, and is marked read once you reach its newest.
- **Reading**: Threads open beside their Conversation; images, animated GIFs, Custom Emoji and avatars show inline;
  who's typing shows on the status line, and whether a DM's other person is active on its sidebar icon.
- **Writing**: @-mentions, `:emoji:` and slash commands complete as you type; reactions, editing your Messages,
  marking Messages unread, and pressing apps' buttons (such as Giphy's).
- **Desktop notifications** for DMs, Mentions and Replies in your Followed Threads, while the window doesn't have
  focus.

## Requirements

- A terminal; [Ghostty](https://ghostty.org) for the best of it. Pictures use the kitty graphics protocol's Unicode
  placeholders in Ghostty and kitty, and half blocks elsewhere, as in tmux.
- A [Nerd Font](https://www.nerdfonts.com) for the icons, which Ghostty, kitty and WezTerm ship; elsewhere they're
  plain characters (`:set icons=nerd` or `plain` says otherwise).
- Linux, with `secret-tool` from libsecret and a Secret Service keyring (GNOME Keyring, KeePassXC, …), or macOS.
- Rust 1.99 or newer, to build it, and [wcgw-kit](https://github.com/wcgw/wcgw-kit) checked out beside slackers:
  slackers runs in it (see [ADR 0003](docs/adr/0003-run-in-wcgw-kit.md)).

## Install

```sh
cargo install --path crates/slackers
```

or `cargo build --release` and run `target/release/slackers`.

## Sign in

slackers signs in to each Workspace as Slack's web client does, with your browser session's credentials: a token per
Workspace and the session's `d` cookie, shared by every Workspace (see
[ADR 0001](docs/adr/0001-slack-session-token-auth.md) for why). They grant full access to your account: treat them as
passwords. slackers reads them from your system keyring, never from a file.

### Find the token and the cookies

1. In your browser, sign in to every Workspace you want, then open <https://app.slack.com>.
2. Open the developer tools.
3. **Tokens** (`xoxc-…`, one per Workspace): in the console, run
   `Object.values(JSON.parse(localStorage.localConfig_v2).teams).map(t => [t.name, t.token])`.
4. **Cookies**: under the site's storage, in the cookies for `.slack.com`, copy the value of `d` (`xoxd-…`) and, if
   there is one, `d-s`.

They all belong to the same browser session. Logging out, or signing in to another Workspace in that browser, replaces
the `d` cookie: when slackers says it's signed out, store a fresh one.

### Store them

Each token is stored under the label you give its Workspace in the config (below), such as `cncf`. Each command
prompts for the secret, which keeps it out of your shell's history.

**Linux** (Secret Service):

```sh
secret-tool store --label='slackers d cookie'   service slackers key d-cookie
secret-tool store --label='slackers d-s cookie' service slackers key d-s-cookie   # only if your browser has one
secret-tool store --label='slackers cncf token' service slackers key token workspace cncf
```

**macOS** (login Keychain):

```sh
security add-generic-password -U -s slackers -a d-cookie -w
security add-generic-password -U -s slackers -a d-s-cookie -w   # only if your browser has one
security add-generic-password -U -s slackers -a token:cncf -w
```

Repeat the token line for every Workspace, with its label.

## Configure

`~/.config/slackers/config.toml` (or under `$XDG_CONFIG_HOME`):

```toml
# Workspaces to sign in to, by their label in the keyring, in sidebar order.
workspaces = ["redhat", "cncf"]

[[virtual_workspace]]
name = "Kuadrant"
channels = ["redhat#kuadrant", "cncf#kuadrant-dev"]
```

`:vw add NAME` puts the open Channel in the Virtual Workspace called NAME, creating it if need be, and `:vw rm NAME`
takes it out; slackers rewrites the file, keeping its comments and layout.

Logs go to `~/.local/state/slackers/slackers.log` (or under `$XDG_STATE_HOME`).

## Keys

slackers runs in [wcgw-kit](https://github.com/wcgw/wcgw-kit), which works as Neovim does: motions, counts, `/`
search, folds, windows (`<C-w>h`, `<C-w>l`, `q` to close one), the jumplist (`<C-o>`, `<C-i>`), `:` commands with
completion, `:map` and `:hi`. `:help` lists every key and command; after `<Space>`, which-key shows what may follow.
Settings, maps and colours go in `~/.config/slackers/init.vim`, after `~/.config/wcgw/init.vim`, which every wcgw
app reads.

| Key | Command | Does |
|---|---|---|
| `<CR>`, a click | `:open` | Opens what's under the cursor: a sidebar row, a Message's Thread, a link, an Activity item's Message |
| `<C-t>` | `:pop` | Back from what was opened |
| `\` | `:sidebar` | Hides the sidebar from within it, goes to it from elsewhere, and brings it back |
| `za` `zo` `zc` `zM` `zR` | | Fold the sidebar's sections |
| `<Space>f` | `:pick conversations` | Finds a Conversation by name, fzf-style |
| `<Space>a` | `:activity` | Activity |
| `i` | `:startinsert` | Writes in the Conversation's or Thread's composer |
| `A` | `:amend` | Edits your Message in the composer; `Enter` saves it, emptying the composer gives it up |
| `+` | `:react [emoji]` | Reacts to the Message, or takes your reaction back |
| `U` | `:unread` | Marks the Message unread, and those after it |
| `K` | `:details` | Who wrote the Message and when, who reacted, its buttons and where it leads |
| `o`, `gx` | `:browse` | Opens the link under the cursor, or the Message's; with several, picks one, or a button |
| `gy` | `:yanklink` | Copies the link under the cursor, or the Message's on Slack's web client |

In the composer, `Enter` sends, `Shift-Enter` or `Alt-Enter` breaks the line, `@`, `:` and `/` complete people, emoji
and commands (`Ctrl-n`/`Ctrl-p` choose, `Enter` picks), `Ctrl-x Ctrl-e` hands the Draft to `$EDITOR`, and `Esc`
leaves it. What slackers tells you, such as why a Message wasn't sent, shows in the command line until the next
key; `:messages` lists it all.

The mouse works too: a click opens what it lands on, the wheel scrolls the window under it, and a click on the
composer writes there. Hold Shift to select text, as most terminals let you while an app uses the mouse.

Drafts are written in Markdown: `**bold**`, `_italic_`, `~~strike~~`, `` `code` ``, code blocks, `> quotes`,
headings and `[links](https://…)` post as Slack shows them, and editing a Message brings it back in Markdown. `*bold*`
stays bold, as in Slack. Headings post as bold lines, and code blocks lose their language: Slack has neither.

A Draft starting with `/` runs that slash command, as in Slack: start it with a space to post it as text. When a
command posts for you elsewhere, as `/dm @someone hi` does, slackers follows it there.

## License

[GPL-3.0-or-later](LICENSE).
