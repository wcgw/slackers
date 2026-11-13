---
status: accepted
---

# Run in wcgw-kit, as an application of its editor

slackers runs in [wcgw-kit](https://github.com/wcgw/wcgw-kit), the toolkit that gives devnull, ghd, muttrs and
slackers one Neovim look & feel (its `docs/plan.md`, milestone M3.2). The kit owns the event loop, the terminal,
keys and the keymap, `:` commands, windows, the statusline and command line, highlight groups and colorschemes,
pictures, the composer and the mouse. slackers keeps its domain: Workspaces, their Sessions and what they tell it,
what's Unread, Followed Threads, Activity, who's typing, Drafts' references, edits, the outbox, notifications held
back, and the Virtual Workspaces in its config. It shows them as kit buffers, windows, commands, pickers and
statusline segments.

- **Every pane is a buffer.** The sidebar is an unlisted buffer whose sections are blocks that fold as Vim folds
  (`za`, `zo`, `zc`, `zR`, `zM`). Each Conversation and each Thread opened has a buffer, its Messages blocks keyed by
  their timestamps, so the cursor stays on its Message as Messages come, change and go; the buffer follows its end,
  as a chat does. What's new is a block of its own, which opening a Conversation lands on. Activity is a buffer, and
  so is the page saying how signing in goes. slackers renders them from its state after every change.
- **Keys are Vim's** (the kit's D12). Motions are the kit's and pure Vim; each action of slackers' is an ex command
  bound to a default key, so `:map` reaches anything a key does. `<CR>` (`:open`) opens what's under the cursor: a
  sidebar row's Conversation, a Message's Thread, a link. Windows move with `<C-w>`, and `q` closes one.
- **Windows** (the kit's D11): the sidebar on the left, 30 cells wide; Conversations in the main window; a Thread
  beside its Conversation when there are 120 cells beside the sidebar, else in its place. `\` toggles the sidebar.
- **Writing** is in the kit's composer, docked under each Conversation and Thread. slackers completes the word being
  written (people, broadcasts, emoji, slash commands) and keeps what each picked label stands for.
- **Look**: slackers' colours are highlight groups (`SlackersMention`, `SlackersNew`, `SlackersAuthor0`…) linked to
  the kit's standard ones, with tokyonight's colours as before; its icons have plain fallbacks for terminals without
  a Nerd Font. No permanent key hints: which-key after a prefix, and `:help`.
- **Pictures** are the kit's (`wcgw-kit-gfx`, which grew out of slackers-gfx): avatars, Workspace icons and Custom
  Emoji in lines of text, images on lines of their own. slackers fetches them with each Workspace's credentials:
  a picture's source names the Workspace it's from.

slackers-gfx goes, and so do slackers' modes, panes, focus, key handling, drawing and picture store.

## Considered options

- **Keep slackers' own UI**, and share only the picture crate. Its keys, modes and chrome would keep drifting from
  the other three applications', which is what the kit is for (its D1).

## Consequences

- Keys change: motions are Vim's (`gg` rather than `g`; no `h` or `Esc` to go back: `q` closes a Thread's window,
  `<C-t>` goes back); editing one's Message is `A`, leaving Vim's `e` its motion; Tab is the jumplist's, not a
  window switch. README.md lists them.
- Notices stay in the command line until replaced, as Vim's messages do, rather than going after 8 seconds; the
  status line says how connections stand and who's typing, but no longer which keys apply.
- The kit sets a window's cursor only when the window first shows a buffer: opening a Conversation shown before goes
  back where its window left it, rather than to its first unread Message or to an Activity item's Message.
- slackers depends on the kit by path until the kit is published (the kit's D18), which raises the Rust it builds
  with to the kit's 1.99.
- Pictures are drawn in more terminals: the kit falls back on half blocks where kitty's Unicode placeholders aren't
  known to work (its D15), which supersedes [ADR 0002](0002-ghostty-kitty-unicode-placeholders.md)'s Ghostty-only
  stance.
