---
status: accepted
---

# Target Ghostty only, drawing images with kitty Unicode placeholders

slackers draws images, animated GIFs and Custom Emoji only through the kitty graphics protocol's Unicode placeholders,
and supports only Ghostty run directly, with no terminal multiplexer. Each image is uploaded once, then drawn by writing
ordinary cells that reference it. So images clip, scroll and diff with ratatui's buffer like text, and a Custom Emoji
can sit inline in a line of a Message.

Ghostty 1.3.1 has placeholder support but not kitty animation, which landed on Ghostty's `main` branch on 2026-08-21.
So slackers animates by uploading each frame as its own image and changing which image the cells reference on each
tick. Moving to the terminal's own animation once a Ghostty release ships it stays inside the graphics module.

## Considered options

- **ratatui-image (11.1.0) with Sixel, iTerm2 or kitty.** It draws one image per rectangle, so it can't put an emoji
  inline in text, and it doesn't animate.
- **Direct kitty placements.** These live outside ratatui's buffer, so every scroll or overlap needs explicit delete
  and re-place commands.

## Consequences

- No fallback for other terminals, and no tmux or zellij for now. Placeholders are designed to pass through tmux, so
  that is the likely first extension.
