//! Images, animated GIFs and Custom Emoji in Ghostty, drawn by ratatui through kitty Unicode placeholders (see
//! `docs/adr/0002-ghostty-kitty-unicode-placeholders.md`).
//!
//! Getting a picture on screen takes two steps. [`prepare`] decodes and sizes it and stages its pixels; it's slow,
//! so run it off the UI thread. [`Prepared::send`] then hands it to the terminal between two draws, and the
//! resulting [`Picture`] draws as ratatui [`Span`](ratatui::text::Span)s, one per row, that animate with time.

mod kitty;
mod picture;

use std::io::{self, Write};

pub use kitty::Ids;
pub use picture::{CellBox, CellSize, Error, Fit, Picture, Prepared, prepare};

/// Deletes every picture from the terminal, freeing its memory: this run's, and any an earlier run left behind, as one
/// that crashed does. Call it on entering the terminal's alternate screen, before sending any picture, and before
/// leaving it.
///
/// # Errors
///
/// If writing to `out` fails.
pub fn forget_all(out: &mut impl Write) -> io::Result<()> {
    out.write_all(kitty::DELETE_ALL)
}
