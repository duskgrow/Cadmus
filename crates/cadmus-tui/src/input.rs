//! The input broker owns the crossterm event stream and can drop/recreate it
//! (ADR-0018 item 5). The consumer is the **`$EDITOR` handoff (Ctrl-G)**: a
//! live [`EventStream`] parks a reader thread on stdin, which both holds
//! crossterm's process-global event-reader lock — starving any
//! cursor-position query for the full two-second lock timeout (verified on a
//! real pty, 2026-09-16; the mechanism behind upstream ratatui #2640) — and
//! steals the external editor's input (the Codex broker's file-header
//! lesson, the 2026-09-13 exhibit). [`InputBroker::quiesce`] is the
//! designated seam — the guard's lifetime *is* the quiesced-stdin window.
//!
//! The shell no longer needs quiescing: its cursor-position queries are
//! answered by the cursor tracker ([`crate::cursor`]), so the event stream
//! is never dropped in the app loop.
//!
//! Keyboard handling itself is synchronous in-memory work on every path
//! (ADR-0018 item 5); nothing here blocks the loop on input.
//!
//! Untestable surface note: an [`EventStream`] reads the process's real
//! stdin, so this module carries no unit tests — the terminal-facing glue is
//! exercised by the inline-spike harness, the same split as
//! [`crate::shell`].

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

    /// Take the event stream down — dropping it wakes the stdin reader thread
    /// out of its parked poll (it holds the global event-reader lock while
    /// parked, which would starve any cursor-position query), and hands back a
    /// guard that recreates the stream on drop. Hold it across shell
    /// (re)construction and `$EDITOR` handoff. Resize notifications during the
    /// window are missed: re-query the terminal size after the guard drops.
    ///
    /// The drop's wake byte is consumed by the thread it wakes — unless that
    /// thread was between tasks, in which case the byte lingers in the reader's
    /// waker pipe and the NEXT poll returns instantly (mio wakes persist). That
    /// lingering byte poisons a cursor-position query (its poll reads the wake
    /// as an instant timeout), so quiesce ends with a best-effort zero-timeout
    /// poll to drain it; pending key events are buffered by the reader, never
    /// consumed here.
    #[must_use]
    pub fn quiesce(&mut self) -> Quiesced<'_> {
        self.stream = None;
        let _ = crossterm::event::poll(std::time::Duration::ZERO);
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

/// The input seam the app loop drives (ADR-0018 item 5): the real broker
/// reads crossterm's event stream; test rigs script events. The quiesce
/// guard's lifetime IS the quiesced-stdin window — the type encoding of the
/// `$EDITOR`-handoff contract.
pub trait EventSource {
    /// The quiesced-stdin guard ([`InputBroker::quiesce`]).
    type Quiesced<'a>
    where
        Self: 'a;

    /// The next terminal event; pends forever while quiesced.
    fn next_event(&mut self) -> impl Future<Output = Option<io::Result<Event>>>;

    /// Take the event stream down until the guard drops.
    fn quiesce(&mut self) -> Self::Quiesced<'_>;
}

impl EventSource for InputBroker {
    type Quiesced<'a> = Quiesced<'a>;

    fn next_event(&mut self) -> impl Future<Output = Option<io::Result<Event>>> {
        InputBroker::next_event(self)
    }

    fn quiesce(&mut self) -> Quiesced<'_> {
        InputBroker::quiesce(self)
    }
}
