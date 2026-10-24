//! slackers: a terminal Slack client that groups Conversations by subject across Workspaces.
//!
//! ```sh
//! slackers WORKSPACE
//! ```
//!
//! signs in to `WORKSPACE` with the credentials stored in the keyring under that name (see [`credentials`]). Keys
//! work as in vim (see [`app`]); `:q` quits. Logs go to `$XDG_STATE_HOME/slackers/slackers.log`; `RUST_LOG` sets
//! their level.

mod app;
mod credentials;
mod emoji;
mod icons;
mod layout;
mod pictures;
mod search;
mod view;

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableFocusChange, EnableFocusChange, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt as _;
use ratatui::DefaultTerminal;
use slackers_gfx::{CellSize, Ids, Prepared};
use slackers_model::ConversationId;
use slackers_slack::{Credentials, Session};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use crate::app::{App, Command, Drawn, Event};

fn main() -> Result<()> {
    let label = std::env::args()
        .nth(1)
        .context("usage: slackers WORKSPACE")?;
    start_logging()?;
    // Before the runtime starts: reading the keyring blocks.
    let credentials = credentials::load(&label)?;
    let runtime = tokio::runtime::Runtime::new()?;

    let mut terminal = ratatui::try_init()?;
    terminal_extras(true)?;
    let cell = CellSize::query();
    let outcome = runtime.block_on(run(&mut terminal, label, credentials, cell));
    // Best effort on the way out: the terminal drops these with its screen anyway.
    let _ = terminal_extras(false);
    let _ = slackers_gfx::forget_all(&mut io::stdout());
    ratatui::restore();
    outcome
}

/// Turns on (or off) what slackers needs beyond ratatui's defaults: focus reports, so that Messages count as read
/// only while the window has focus, and the kitty keyboard protocol, so that Shift+Enter differs from Enter.
fn terminal_extras(on: bool) -> io::Result<()> {
    if on {
        execute!(
            io::stdout(),
            EnableFocusChange,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
    } else {
        execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableFocusChange
        )
    }
}

/// What reaches the event loop from background work.
enum Inbox {
    App(Event),
    /// A picture finished decoding, and still has to go to the terminal.
    Prepared {
        url: String,
        result: Result<Prepared, slackers_gfx::Error>,
    },
}

/// Runs the app until the user quits.
async fn run(
    terminal: &mut DefaultTerminal,
    label: String,
    credentials: Credentials,
    cell: CellSize,
) -> Result<()> {
    let (inbox, mut received) = mpsc::unbounded_channel();
    let session = {
        let inbox = inbox.clone();
        // Runs on the Session's tasks. Sending fails only once the loop below has ended.
        Session::start(credentials, move |update| {
            let _ = inbox.send(Inbox::App(Event::Slack(update)));
        })
    };
    let effects = Effects {
        session,
        inbox,
        ids: Arc::new(Ids::default()),
        cell,
    };
    let mut app = App::new(label);
    let mut terminal_events = EventStream::new();
    loop {
        let mut drawn = Drawn::default();
        terminal.draw(|frame| drawn = view::draw(frame, &app))?;
        let commands = app.update(Event::Drawn(drawn));
        if effects
            .carry_out(commands, terminal, &mut terminal_events)
            .await?
        {
            return Ok(());
        }

        // Wait for a key, news from Slack or a picture, or the next animation frame.
        let frame_due = app
            .next_change()
            .map(|at| at.saturating_sub(app.animation_time()));
        let first = tokio::select! {
            terminal_event = terminal_events.next() => match terminal_event {
                Some(event) => Inbox::App(Event::Terminal(event?)),
                None => return Ok(()),
            },
            Some(item) = received.recv() => item,
            () = until(frame_due) => continue,
        };
        // Take in everything that's waiting before drawing again.
        let mut items = vec![first];
        while let Ok(item) = received.try_recv() {
            items.push(item);
        }
        for item in items {
            let event = match item {
                Inbox::App(event) => event,
                Inbox::Prepared { url, result } => match show(result) {
                    Ok(picture) => Event::PictureReady { url, picture },
                    Err(error) => {
                        tracing::warn!(%error, "couldn't show a picture");
                        Event::PictureFailed { url }
                    }
                },
            };
            let commands = app.update(event);
            if effects
                .carry_out(commands, terminal, &mut terminal_events)
                .await?
            {
                return Ok(());
            }
        }
    }
}

/// Sends a prepared picture to the terminal. Call it between draws, so that the picture's commands don't
/// interleave with ratatui's output.
fn show(result: Result<Prepared, slackers_gfx::Error>) -> Result<slackers_gfx::Picture> {
    let mut stdout = io::stdout().lock();
    let picture = result?.send(&mut stdout)?;
    stdout.flush()?;
    Ok(picture)
}

/// Carries out [`Command`]s: the app's only way to reach Slack, the terminal and other programs.
struct Effects {
    session: Session,
    /// Where background work reports back to the event loop.
    inbox: mpsc::UnboundedSender<Inbox>,
    ids: Arc<Ids>,
    cell: CellSize,
}

impl Effects {
    /// Carries out `commands`. Returns whether one of them was to quit.
    ///
    /// # Errors
    ///
    /// If the terminal can't be restored after handing it to an editor.
    async fn carry_out(
        &self,
        commands: Vec<Command>,
        terminal: &mut DefaultTerminal,
        terminal_events: &mut EventStream,
    ) -> Result<bool> {
        for command in commands {
            match command {
                Command::Quit => return Ok(true),
                Command::Slack(request) => self.session.request(request),
                Command::Prepare { url, bytes, fit } => {
                    let (inbox, ids, cell) = (self.inbox.clone(), Arc::clone(&self.ids), self.cell);
                    // Decoding and resizing are CPU-heavy, and staging writes files: keep them off the loop's
                    // thread. The result comes back through the inbox; should the task panic, the emoji simply
                    // stays drawn by name.
                    drop(tokio::task::spawn_blocking(move || {
                        let result = slackers_gfx::prepare(&bytes, fit, cell, &ids);
                        let _ = inbox.send(Inbox::Prepared { url, result });
                    }));
                }
                Command::Edit { conversation, text } => {
                    // The old stream's reader thread would steal the editor's keystrokes: drop it now. The new
                    // one reads nothing until the loop polls it, after the editor exits.
                    *terminal_events = EventStream::new();
                    self.edit(conversation, &text, terminal).await?;
                }
            }
        }
        Ok(false)
    }

    /// Hands the terminal to `$VISUAL` or `$EDITOR` (or `vi`) to edit `text`, and reports the outcome through the
    /// inbox.
    async fn edit(
        &self,
        conversation: ConversationId,
        text: &str,
        terminal: &mut DefaultTerminal,
    ) -> Result<()> {
        let path = std::env::temp_dir().join(format!("slackers-draft-{}.md", std::process::id()));
        execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableFocusChange,
            LeaveAlternateScreen,
            Show
        )?;
        disable_raw_mode()?;
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".to_owned());
        let outcome = edit_file(&path, text, &editor).await;
        let _ = tokio::fs::remove_file(&path).await;
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        terminal_extras(true)?;
        terminal.clear()?;
        // Whatever the terminal did with pictures while the editor had it, start afresh.
        slackers_gfx::forget_all(&mut io::stdout())?;
        let _ = self.inbox.send(Inbox::App(Event::PicturesLost));
        let outcome = outcome.map_err(|error| format!("{error:#}"));
        let _ = self.inbox.send(Inbox::App(Event::Edited {
            conversation,
            outcome,
        }));
        Ok(())
    }
}

/// Writes `text` to `path`, readable by the user alone, runs `editor` on it (a program, optionally followed by
/// arguments, split at whitespace), and returns what the file holds then, without trailing line breaks.
async fn edit_file(path: &Path, text: &str, editor: &str) -> Result<String> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o600);
    let mut file = options
        .open(path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    tokio::io::AsyncWriteExt::write_all(&mut file, text.as_bytes()).await?;
    drop(file);

    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    let status = tokio::process::Command::new(program)
        .args(words)
        .arg(path)
        .status()
        .await
        .with_context(|| format!("couldn't run {program}"))?;
    ensure!(
        status.success(),
        "{program} exited with {status}; the draft is unchanged"
    );
    let edited = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(edited.trim_end_matches('\n').to_owned())
}

/// Completes after `duration`, or never for `None`.
async fn until(duration: Option<Duration>) {
    match duration {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending().await,
    }
}

/// Sends logs to `$XDG_STATE_HOME/slackers/slackers.log`: the terminal belongs to the UI.
fn start_logging() -> Result<()> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("neither XDG_STATE_HOME nor HOME is set")?;
    let directory = state.join("slackers");
    fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;
    let path = directory.join("slackers.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory holding a fake editor that runs `script` on its file argument (`$1`).
    fn fake_editor(name: &str, script: &str) -> (PathBuf, String) {
        let directory =
            std::env::temp_dir().join(format!("slackers-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let editor = directory.join("editor.sh");
        fs::write(&editor, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        (directory, editor.display().to_string())
    }

    #[tokio::test]
    async fn the_editor_edits_a_private_copy_of_the_draft() {
        let (directory, editor) = fake_editor(
            "edits",
            r#"[ "$(cat "$1")" = "draft" ] && printf 'edited\n\n' > "$1""#,
        );
        let path = directory.join("draft.md");
        assert_eq!(edit_file(&path, "draft", &editor).await.unwrap(), "edited");
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn a_failing_editor_leaves_the_draft_alone() {
        let (directory, editor) = fake_editor("fails", "exit 3");
        let error = edit_file(&directory.join("draft.md"), "draft", &editor)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("the draft is unchanged"),
            "{error}"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
