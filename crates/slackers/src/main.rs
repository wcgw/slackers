//! slackers: a terminal Slack client that groups Conversations by subject across Workspaces.
//!
//! ```sh
//! slackers WORKSPACE
//! ```
//!
//! signs in to `WORKSPACE` with the credentials stored in the keyring under that name (see [`credentials`]), read
//! only for now. Logs go to `$XDG_STATE_HOME/slackers/slackers.log`; `RUST_LOG` sets their level.

mod app;
mod credentials;
mod emoji;
mod icons;
mod layout;
mod view;

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::EventStream;
use futures_util::StreamExt as _;
use ratatui::DefaultTerminal;
use slackers_gfx::{CellSize, Ids, Prepared};
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
    let cell = CellSize::query();
    let outcome = runtime.block_on(run(&mut terminal, label, credentials, cell));
    // Best effort on the way out: the terminal frees images with its screen anyway.
    let _ = slackers_gfx::forget_all(&mut io::stdout());
    ratatui::restore();
    outcome
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
    let ids = Arc::new(Ids::default());
    let mut app = App::new(label);
    let mut terminal_events = EventStream::new();
    loop {
        let mut drawn = Drawn::default();
        terminal.draw(|frame| drawn = view::draw(frame, &app))?;
        let commands = app.update(Event::Drawn(drawn));
        if execute(commands, &session, &inbox, &ids, cell) {
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
                Inbox::Prepared { url, result } => {
                    match result.map_err(anyhow::Error::from).and_then(|prepared| {
                        // Between draws, so that the picture's commands don't interleave with ratatui's output.
                        let mut stdout = io::stdout().lock();
                        let picture = prepared.send(&mut stdout)?;
                        stdout.flush()?;
                        Ok(picture)
                    }) {
                        Ok(picture) => Event::PictureReady { url, picture },
                        Err(error) => {
                            tracing::warn!(%error, "couldn't show a picture");
                            Event::PictureFailed { url }
                        }
                    }
                }
            };
            let commands = app.update(event);
            if execute(commands, &session, &inbox, &ids, cell) {
                return Ok(());
            }
        }
    }
}

/// Carries out `commands`. Returns whether one of them was to quit.
fn execute(
    commands: Vec<Command>,
    session: &Session,
    inbox: &mpsc::UnboundedSender<Inbox>,
    ids: &Arc<Ids>,
    cell: CellSize,
) -> bool {
    for command in commands {
        match command {
            Command::Quit => return true,
            Command::Slack(request) => session.request(request),
            Command::Prepare { url, bytes, fit } => {
                let (inbox, ids) = (inbox.clone(), Arc::clone(ids));
                // Decoding and resizing are CPU-heavy, and staging writes files: keep them off the loop's thread.
                // The result comes back through the inbox; a panic there would leave the emoji drawn by name.
                drop(tokio::task::spawn_blocking(move || {
                    let result = slackers_gfx::prepare(&bytes, fit, cell, &ids);
                    let _ = inbox.send(Inbox::Prepared { url, result });
                }));
            }
        }
    }
    false
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
