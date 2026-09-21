//! The shared vt100 rig for the inline-shell integration suites
//! (`dynamic_height_spike.rs`, `stream_flush.rs`): a `Backend` impl that
//! feeds a `vt100::Parser` the same escape sequences ratatui-crossterm emits
//! (CUP + symbol per cell, `\n` per appended line, ED for clears), plus raw
//! history writes, so vt100 applies terminal semantics instead of us
//! re-deriving them. Cursor-position queries use the emulated screen. The
//! cell-draw path omits SGR; raw history writes retain it for style checks.
//! Guard bytes (2026h) go to a separate sink and are asserted verbatim.
//! Full-world assertions inject `FullScreen`: vt100 0.16 discards departing
//! rows for partial scroll regions, so `Standard` tests judge visible rows
//! and raw sequences without claiming to validate its native scrollback.
//!
//! The rig is a toolbox: each suite uses a subset, so unused-method lints
//! are off here.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::io::{self, Write};
use std::rc::Rc;

use crossterm::event::Event;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell as BufCell;
use ratatui::layout::{Position, Size};
use tokio::sync::mpsc;

pub const SCREEN_ROWS: u16 = 24;
pub const SCREEN_COLS: u16 = 80;
/// Deep enough that no assertion ever hits the scrollback cap.
pub const SCROLLBACK_LEN: usize = 500;

pub const BSU: &[u8] = b"\x1b[?2026h";
pub const ESU: &[u8] = b"\x1b[?2026l";

/// Records the shell's guard bytes. Guards never reach the vt100 parser (the
/// screen model ignores them), so the one-wrapper invariant is asserted on
/// this verbatim recording instead.
#[derive(Clone, Default)]
pub struct GuardSink {
    log: Rc<RefCell<Vec<u8>>>,
}

impl GuardSink {
    /// Drain the recording: each op should leave exactly one begin/end pair.
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.log.borrow_mut())
    }
}

impl Write for GuardSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.log.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Shared handle to the emulated terminal; cloning gives a recreated
/// `Terminal` the same screen state — which is the whole point of the
/// recreation protocol (the OS terminal also outlives the `Terminal`).
/// `fail_queries` rejects accidental post-boot CPR; the query counter also
/// catches callers that swallow the error.
#[derive(Clone)]
pub struct VtBackend {
    parser: Rc<RefCell<vt100::Parser>>,
    raw_bytes: Rc<RefCell<Vec<u8>>>,
    fail_queries: Rc<Cell<bool>>,
    cursor_queries: Rc<Cell<usize>>,
    fail_write_containing: Rc<Cell<Option<u8>>>,
    failed_write_end: Rc<Cell<Option<usize>>>,
    resize_on_raw_flush: Rc<Cell<Option<(u16, u16)>>>,
}

impl VtBackend {
    pub fn new() -> Self {
        Self {
            parser: Rc::new(RefCell::new(vt100::Parser::new(
                SCREEN_ROWS,
                SCREEN_COLS,
                SCROLLBACK_LEN,
            ))),
            raw_bytes: Rc::new(RefCell::new(Vec::new())),
            fail_queries: Rc::new(Cell::new(false)),
            cursor_queries: Rc::new(Cell::new(0)),
            fail_write_containing: Rc::new(Cell::new(None)),
            failed_write_end: Rc::new(Cell::new(None)),
            resize_on_raw_flush: Rc::new(Cell::new(None)),
        }
    }

    fn emit(&self, bytes: &str) {
        self.parser.borrow_mut().process(bytes.as_bytes());
    }

    /// Attempted raw writes, including failures, separately from cell draws
    /// and `GuardSink`. Draining does not change the parser's screen.
    pub fn take_raw_bytes(&self) -> Vec<u8> {
        std::mem::take(&mut self.raw_bytes.borrow_mut())
    }

    /// Fail a payload byte after margins have been set, independent of how
    /// the writer chunks its output. Cleanup writes can then succeed.
    pub fn fail_next_raw_write_containing(&self, byte: u8) {
        self.fail_write_containing.set(Some(byte));
        self.failed_write_end.set(None);
    }

    /// Offset after the failed write attempt, so cleanup must be a later
    /// write rather than bytes inside the same rejected buffer.
    pub fn failed_raw_write_end(&self) -> usize {
        self.failed_write_end.get().expect("a raw write failed")
    }

    /// A physical resize after raw history bytes reach the terminal, before
    /// the shell's post-insert bookkeeping. Backend/guard flushes do not fire it.
    pub fn resize_on_next_raw_flush(&self, rows: u16, cols: u16) {
        self.resize_on_raw_flush.set(Some((rows, cols)));
    }
}

impl Write for VtBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.raw_bytes.borrow_mut().extend_from_slice(buf);
        if self
            .fail_write_containing
            .get()
            .is_some_and(|byte| buf.contains(&byte))
        {
            self.fail_write_containing.set(None);
            self.failed_write_end
                .set(Some(self.raw_bytes.borrow().len()));
            return Err(io::Error::other("injected raw write failure"));
        }
        self.parser.borrow_mut().process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some((rows, cols)) = self.resize_on_raw_flush.take() {
            self.parser.borrow_mut().screen_mut().set_size(rows, cols);
        }
        Ok(())
    }
}

impl Backend for VtBackend {
    // The shell unifies guard emission and backend errors on io::Error.
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a BufCell)>,
    {
        let mut out = String::new();
        for (x, y, cell) in content {
            // Naive per-cell CUP mirrors crossterm's semantics (the real
            // backend only elides contiguous moves); SGR omitted by design.
            let _ = write!(out, "\x1b[{};{}H{}", y + 1, x + 1, cell.symbol());
        }
        self.emit(&out);
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        // ratatui-crossterm emits plain `\n` x n; vt100 applies IND semantics
        // (cursor down, scrolling at the bottom margin) like a real terminal.
        self.emit(&"\n".repeat(usize::from(n)));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.emit("\x1b[?25l");
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.emit("\x1b[?25h");
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.cursor_queries.set(self.cursor_queries.get() + 1);
        if self.fail_queries.get() {
            return Err(io::Error::other("injected CPR failure"));
        }
        let (row, col) = self.parser.borrow().screen().cursor_position();
        Ok(Position::new(col, row))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let Position { x, y } = position.into();
        self.emit(&format!("\x1b[{};{}H", y + 1, x + 1));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.emit("\x1b[2J");
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.emit(match clear_type {
            ClearType::All => "\x1b[2J",
            ClearType::AfterCursor => "\x1b[0J",
            ClearType::BeforeCursor => "\x1b[1J",
            ClearType::CurrentLine => "\x1b[2K",
            ClearType::UntilNewLine => "\x1b[0K",
        });
        Ok(())
    }

    fn size(&self) -> Result<Size, Self::Error> {
        let (rows, cols) = self.parser.borrow().screen().size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: self.size()?,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Read-side view over the emulated terminal: rows as the user would see
/// them, including what already scrolled off.
pub struct World {
    pub backend: VtBackend,
}

impl World {
    pub fn new() -> Self {
        Self {
            backend: VtBackend::new(),
        }
    }

    /// The shell session above the future TUI: printed before `Terminal`
    /// creation, like real pre-existing scrollback content.
    pub fn print_lines(&self, lines: &[String]) {
        for line in lines {
            self.backend.emit(&format!("{line}\r\n"));
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        self.backend
            .parser
            .borrow_mut()
            .screen_mut()
            .set_size(rows, cols);
    }

    /// Make cursor-position queries fail until cleared — a real terminal's
    /// CPR times out under resize storms and quirky stdio (spike fact F3).
    pub fn fail_queries(&self, fail: bool) {
        self.backend.fail_queries.set(fail);
    }

    /// Count failed queries too: a swallowed CPR error is still a round-trip.
    pub fn cursor_queries(&self) -> usize {
        self.backend.cursor_queries.get()
    }

    pub fn visible_rows(&self) -> Vec<String> {
        let parser = self.backend.parser.borrow();
        // Read at the screen's current width: a resize mid-session changes
        // it, and a stale width would truncate (or pad) the rows.
        let (_rows, cols) = parser.screen().size();
        parser
            .screen()
            .rows(0, cols)
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    pub fn scrollback_rows(&self) -> Vec<String> {
        let mut parser = self.backend.parser.borrow_mut();
        let screen = parser.screen_mut();
        // vt100 reads scrollback through a view offset, and one view is only
        // one screen tall: with more scrollback than that, page backwards
        // window by window or the newest rows silently fall off the read.
        screen.set_scrollback(usize::MAX);
        let depth = screen.scrollback();
        let (screen_rows, cols) = screen.size();
        let mut rows = Vec::with_capacity(depth);
        let mut start = 0;
        while start < depth {
            screen.set_scrollback(depth - start);
            let take = (depth - start).min(usize::from(screen_rows));
            rows.extend(
                screen
                    .rows(0, cols)
                    .take(take)
                    .map(|row| row.trim_end().to_string()),
            );
            start += take;
        }
        screen.set_scrollback(0);
        rows
    }

    /// Every non-blank row the user can reach, oldest first: the strongest
    /// invariant the suites have — any lost, duplicated or stale row breaks
    /// the expected sequence.
    pub fn nonblank_rows(&self) -> Vec<String> {
        self.scrollback_rows()
            .into_iter()
            .chain(self.visible_rows())
            .filter(|row| !row.is_empty())
            .collect()
    }

    pub fn cursor(&self) -> (u16, u16) {
        self.backend.parser.borrow().screen().cursor_position()
    }

    pub fn cell(&self, row: u16, col: u16) -> vt100::Cell {
        self.backend
            .parser
            .borrow()
            .screen()
            .cell(row, col)
            .expect("cell inside the visible screen")
            .clone()
    }
}

/// The scripted input source (the app loop's [`cadmus_tui::input::EventSource`]
/// seam): the suite feeds crossterm events through the channel; an empty
/// channel pends, exactly like a quiet terminal. Quiescing is a no-op —
/// there is no stdin reader to race here.
pub struct ScriptedInput {
    rx: mpsc::UnboundedReceiver<Event>,
}

impl ScriptedInput {
    pub fn channel() -> (mpsc::UnboundedSender<Event>, Self) {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, Self { rx })
    }
}

impl cadmus_tui::input::EventSource for ScriptedInput {
    type Quiesced<'a> = ();

    async fn next_event(&mut self) -> Option<io::Result<Event>> {
        self.rx.recv().await.map(Ok)
    }

    fn quiesce(&mut self) -> Self::Quiesced<'_> {}
}
