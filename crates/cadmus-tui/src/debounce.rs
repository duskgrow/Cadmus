//! Resize-burst coalescing (inline-spike discipline 2): processing a resize
//! re-anchors the terminal, and each processed event leaves the previous
//! frame as scrollback residue — so a drag burst is processed once it
//! settles, latest size wins. The ~75 ms default is Codex's
//! `TRANSCRIPT_REFLOW_DEBOUNCE` precedent (2026-09-13 exhibit).
//!
//! Pure cadence state over injected instants; the event loop arms its sleep
//! from [`ResizeDebounce::fire_at`] and calls [`ResizeDebounce::take`] when
//! the timer fires.

use std::time::Duration;

use tokio::time::Instant;

/// Latest-wins coalescer for resize events. See the module docs.
pub struct ResizeDebounce {
    delay: Duration,
    pending: Option<Pending>,
}

struct Pending {
    size: (u16, u16),
    fire_at: Instant,
}

impl ResizeDebounce {
    /// The spike-pinned cadence: bounds a drag gesture to ≤1 stale frame of
    /// scrollback residue while keeping reflow responsive.
    pub const DEFAULT_DELAY: Duration = Duration::from_millis(75);

    #[must_use]
    pub const fn new(delay: Duration) -> Self {
        Self {
            delay,
            pending: None,
        }
    }

    /// Record a resize event; re-arms the settle timer from `now`.
    pub fn record(&mut self, now: Instant, cols: u16, rows: u16) {
        self.pending = Some(Pending {
            size: (cols, rows),
            fire_at: now + self.delay,
        });
    }

    /// When the pending resize may fire; `None` when nothing is pending.
    #[must_use]
    pub fn fire_at(&self) -> Option<Instant> {
        self.pending.as_ref().map(|p| p.fire_at)
    }

    /// The settled size, once. The loop calls this when its timer fires.
    pub fn take(&mut self) -> Option<(u16, u16)> {
        self.pending.take().map(|p| p.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_wins_and_the_timer_rearms() {
        let t0 = Instant::now();
        let mut debounce = ResizeDebounce::new(ResizeDebounce::DEFAULT_DELAY);
        assert_eq!(debounce.fire_at(), None);
        debounce.record(t0, 80, 24);
        assert_eq!(debounce.fire_at(), Some(t0 + ResizeDebounce::DEFAULT_DELAY));
        // A second event inside the window replaces the size and re-arms.
        debounce.record(t0 + Duration::from_millis(10), 100, 30);
        assert_eq!(
            debounce.fire_at(),
            Some(t0 + ResizeDebounce::DEFAULT_DELAY + Duration::from_millis(10))
        );
        assert_eq!(debounce.take(), Some((100, 30)));
        assert_eq!(debounce.take(), None);
        assert_eq!(debounce.fire_at(), None);
    }
}
