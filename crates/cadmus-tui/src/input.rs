//! The input broker owns the crossterm event stream and can drop/recreate it
//! (ADR-0018 item 5). Two consumers need that:
//!
//! - **`$EDITOR` handoff (Ctrl-G)**: a live [`EventStream`] keeps a reader
//!   thread on stdin, which steals the external editor's input and the
//!   terminal's query replies (the Codex broker's file-header lesson,
//!   2026-09-13 exhibit).
//! - **The shell's quiesced-stdin contract**: [`crate::shell::InlineShell`]'s
//!   construction and `set_height` recreation issue CPR queries that race the
//!   reader thread (upstream ratatui #2640, open).
//!   [`InputBroker::quiesce`] is the designated seam — the guard's lifetime
//!   *is* the quiesced-stdin window.
//!
//! Keyboard handling itself is synchronous in-memory work on every path
//! (ADR-0018 item 5); nothing here blocks the loop on input.
//!
//! Untestable surface note: an [`EventStream`] reads the process's real
//! stdin, so this module carries no unit tests — the mechanism is borrow
//! encoding (shell recreation is impossible without holding the guard), and
//! the terminal-facing glue is exercised by the inline-spike harness, the
//! same split as [`crate::shell`].

use std::io;

use crossterm::event::{Event, EventStream};
use tokio_stream::StreamExt as _;

/// Owns the terminal event stream. See the module docs for the contracts.
pub struct InputBroker {
    stream: Option<EventStream>,
}

impl InputBroker {
    /// Construct with a live event stream — spawns crossterm's stdin reader
    /// thread, hence an explicit `new` rather than `Default`.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            stream: Some(EventStream::new()),
        }
    }

    /// The next terminal event. Pends forever while the broker is quiesced —
    /// the app loop's other `select!` branches stay live.
    pub async fn next_event(&mut self) -> Option<io::Result<Event>> {
        match &mut self.stream {
            Some(stream) => stream.next().await,
            None => std::future::pending().await,
        }
    }

    /// Take the event stream down — dropping it signals the stdin reader
    /// thread to wake and exit (no join, but it never reads stdin again) —
    /// and hand back a guard that recreates the stream on drop. Hold it
    /// across shell (re)construction and `$EDITOR` handoff. Resize
    /// notifications during the window are missed: re-query the terminal
    /// size after the guard drops.
    #[must_use]
    pub fn quiesce(&mut self) -> Quiesced<'_> {
        self.stream = None;
        Quiesced { broker: self }
    }
}

/// The quiesced-stdin window; see [`InputBroker::quiesce`].
pub struct Quiesced<'a> {
    broker: &'a mut InputBroker,
}

impl Quiesced<'_> {
    /// Drain and discard every already-buffered input event — the
    /// self-implemented `discard_buffered_input` (ADR-0018 item 1: Codex's
    /// crossterm fork carries it; their own Unix fallback is this
    /// `poll(0)`+`read` loop). Safe only with the reader thread down, hence a
    /// method on this guard — holding it *is* the precondition, type-encoded,
    /// so the body never touches `self`. Consumers: approval prompts
    /// (pre-typed input must never answer them) and the return from
    /// `$EDITOR`.
    #[allow(clippy::unused_self)]
    pub fn discard_buffered_input(&self) -> io::Result<()> {
        while crossterm::event::poll(std::time::Duration::ZERO)? {
            let _ = crossterm::event::read()?;
        }
        Ok(())
    }
}

impl Drop for Quiesced<'_> {
    fn drop(&mut self) {
        self.broker.stream = Some(EventStream::new());
    }
}
