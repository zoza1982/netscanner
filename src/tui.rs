use std::{
  ops::{Deref, DerefMut},
  time::Duration,
};

use color_eyre::eyre::Result;
use crossterm::{
  cursor,
  event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as CrosstermEvent,
    KeyEvent, KeyEventKind, MouseEvent,
  },
  terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend as Backend;
use serde::{Deserialize, Serialize};
use tokio::{
  sync::mpsc::{self, Receiver, Sender},
  task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub type IO = std::io::Stdout;
pub fn io() -> IO {
  std::io::stdout()
}
pub type Frame<'a> = ratatui::Frame<'a>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
  Init,
  Quit,
  Error,
  Closed,
  Tick,
  Render,
  FocusGained,
  FocusLost,
  Paste(String),
  Key(KeyEvent),
  Mouse(MouseEvent),
  Resize(u16, u16),
}

pub struct Tui {
  pub terminal: ratatui::Terminal<Backend<IO>>,
  pub task: JoinHandle<()>,
  pub cancellation_token: CancellationToken,
  pub event_rx: Receiver<Event>,
  pub event_tx: Sender<Event>,
  pub frame_rate: f64,
  pub tick_rate: f64,
  pub mouse: bool,
  pub paste: bool,
}

impl Tui {
  pub fn new() -> Result<Self> {
    let tick_rate = 4.0;
    let frame_rate = 60.0;
    let terminal = ratatui::Terminal::new(Backend::new(io()))?;
    // Use bounded channel with capacity of 100 for high-frequency UI events
    // This prevents memory exhaustion during event bursts
    let (event_tx, event_rx) = mpsc::channel(100);
    let cancellation_token = CancellationToken::new();
    let task = tokio::spawn(async {});
    let mouse = false;
    let paste = false;
    Ok(Self { terminal, task, cancellation_token, event_rx, event_tx, frame_rate, tick_rate, mouse, paste })
  }

  pub fn tick_rate(mut self, tick_rate: f64) -> Self {
    self.tick_rate = tick_rate;
    self
  }

  pub fn frame_rate(mut self, frame_rate: f64) -> Self {
    self.frame_rate = frame_rate;
    self
  }

  pub fn mouse(mut self, mouse: bool) -> Self {
    self.mouse = mouse;
    self
  }

  pub fn paste(mut self, paste: bool) -> Self {
    self.paste = paste;
    self
  }

  pub fn start(&mut self) {
    let tick_delay = std::time::Duration::from_secs_f64(1.0 / self.tick_rate);
    let render_delay = std::time::Duration::from_secs_f64(1.0 / self.frame_rate);
    self.cancel();
    self.cancellation_token = CancellationToken::new();
    let _cancellation_token = self.cancellation_token.clone();
    let _event_tx = self.event_tx.clone();
    self.task = tokio::spawn(async move {
      let mut reader = crossterm::event::EventStream::new();
      let mut tick_interval = tokio::time::interval(tick_delay);
      let mut render_interval = tokio::time::interval(render_delay);
      // Send init event; if this fails, the receiver is already dropped
      if _event_tx.try_send(Event::Init).is_err() {
        return;
      }
      loop {
        let tick_delay = tick_interval.tick();
        let render_delay = render_interval.tick();
        let crossterm_event = reader.next().fuse();
        tokio::select! {
          _ = _cancellation_token.cancelled() => {
            break;
          }
          maybe_event = crossterm_event => {
            match maybe_event {
              Some(Ok(evt)) => {
                match evt {
                  CrosstermEvent::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                      // Ignore send errors - channel may be full or receiver dropped
                      let _ = _event_tx.try_send(Event::Key(key));
                    }
                  },
                  CrosstermEvent::Mouse(mouse) => {
                    let _ = _event_tx.try_send(Event::Mouse(mouse));
                  },
                  CrosstermEvent::Resize(x, y) => {
                    let _ = _event_tx.try_send(Event::Resize(x, y));
                  },
                  CrosstermEvent::FocusLost => {
                    let _ = _event_tx.try_send(Event::FocusLost);
                  },
                  CrosstermEvent::FocusGained => {
                    let _ = _event_tx.try_send(Event::FocusGained);
                  },
                  CrosstermEvent::Paste(s) => {
                    let _ = _event_tx.try_send(Event::Paste(s));
                  },
                }
              }
              Some(Err(_)) => {
                let _ = _event_tx.try_send(Event::Error);
              }
              None => {},
            }
          },
          _ = tick_delay => {
              let _ = _event_tx.try_send(Event::Tick);
          },
          _ = render_delay => {
              let _ = _event_tx.try_send(Event::Render);
          },
        }
      }
    });
  }

  pub fn stop(&self) -> Result<()> {
    self.cancel();
    let mut counter = 0;
    while !self.task.is_finished() {
      std::thread::sleep(Duration::from_millis(1));
      counter += 1;
      if counter > 50 {
        self.task.abort();
      }
      if counter > 100 {
        log::error!(
          "TUI event task did not stop gracefully within 100ms timeout. \
          This may indicate the event loop is blocked or unresponsive."
        );
        break;
      }
    }
    Ok(())
  }

  pub fn enter(&mut self) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(io(), EnterAlternateScreen, cursor::Hide)?;
    if self.mouse {
      crossterm::execute!(io(), EnableMouseCapture)?;
    }
    if self.paste {
      crossterm::execute!(io(), EnableBracketedPaste)?;
    }
    self.start();
    Ok(())
  }

  pub fn exit(&mut self) -> Result<()> {
    self.stop()?;
    if crossterm::terminal::is_raw_mode_enabled()? {
      self.flush()?;
      if self.paste {
        crossterm::execute!(io(), DisableBracketedPaste)?;
      }
      if self.mouse {
        crossterm::execute!(io(), DisableMouseCapture)?;
      }
      crossterm::execute!(io(), LeaveAlternateScreen, cursor::Show)?;
      crossterm::terminal::disable_raw_mode()?;
    }
    Ok(())
  }

  pub fn cancel(&self) {
    self.cancellation_token.cancel();
  }

  pub fn suspend(&mut self) -> Result<()> {
    self.exit()?;
    #[cfg(not(windows))]
    signal_hook::low_level::raise(signal_hook::consts::signal::SIGTSTP)?;
    Ok(())
  }

  pub fn resume(&mut self) -> Result<()> {
    self.enter()?;
    Ok(())
  }

  pub async fn next(&mut self) -> Option<Event> {
    self.event_rx.recv().await
  }
}

impl Deref for Tui {
  type Target = ratatui::Terminal<Backend<IO>>;

  fn deref(&self) -> &Self::Target {
    &self.terminal
  }
}

impl DerefMut for Tui {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.terminal
  }
}

impl Drop for Tui {
  fn drop(&mut self) {
    if let Err(e) = self.exit() {
      eprintln!("Error during TUI cleanup: {}", e);
    }
  }
}
