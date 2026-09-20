//! The run wall-clock behind the run-status row and the scrollback
//! completion rows: measures submit→run-end, pauses across approval waits
//! (a gate wait is not work) and freezes at the run's outcome. Pure state
//! over injected instants (AGENTS.md's determinism seam, the
//! [`crate::debounce`] precedent): `Instant::now()` is only ever called at
//! the app's event/render edges.

use std::time::Duration;

use tokio::time::Instant;

/// One run's working-time accumulator. See the module docs.
#[derive(Clone, Copy, Debug)]
pub struct RunClock {
    /// Working time banked across completed running stretches.
    accumulated: Duration,
    /// The start of the current running stretch; `None` while paused or
    /// frozen.
    resumed_at: Option<Instant>,
}

impl RunClock {
    /// A running clock, started at submit.
    #[must_use]
    pub const fn start(now: Instant) -> Self {
        Self {
            accumulated: Duration::ZERO,
            resumed_at: Some(now),
        }
    }

    /// The working time at `now`: paused stretches never count.
    #[must_use]
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.accumulated
            + self.resumed_at.map_or(Duration::ZERO, |resumed| {
                now.saturating_duration_since(resumed)
            })
    }

    /// Hold the clock across an approval wait; idempotent so the pause
    /// helper can fire after every queue mutation without tracking the
    /// transition itself.
    pub fn pause(&mut self, now: Instant) {
        if let Some(resumed) = self.resumed_at.take() {
            self.accumulated += now.saturating_duration_since(resumed);
        }
    }

    /// Continue after the wait; idempotent (a running clock ignores it).
    pub fn resume(&mut self, now: Instant) {
        if self.resumed_at.is_none() {
            self.resumed_at = Some(now);
        }
    }

    /// Stop the clock at the run's outcome and report the final total —
    /// after this `elapsed` never moves again (a frozen `Failed` row needs
    /// no ticks).
    pub fn freeze(&mut self, now: Instant) -> Duration {
        self.pause(now);
        self.accumulated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_clock_counts_from_start() {
        let t0 = Instant::now();
        let clock = RunClock::start(t0);
        assert_eq!(clock.elapsed(t0), Duration::ZERO);
        assert_eq!(
            clock.elapsed(t0 + Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn a_pause_excludes_the_wait_and_resume_continues() {
        let t0 = Instant::now();
        let mut clock = RunClock::start(t0);
        clock.pause(t0 + Duration::from_secs(2));
        // Paused: the elapsed never moves, however late the read.
        assert_eq!(
            clock.elapsed(t0 + Duration::from_secs(10)),
            Duration::from_secs(2)
        );
        clock.resume(t0 + Duration::from_secs(10));
        assert_eq!(
            clock.elapsed(t0 + Duration::from_secs(13)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn pause_and_resume_are_idempotent() {
        let t0 = Instant::now();
        let mut clock = RunClock::start(t0);
        clock.resume(t0 + Duration::from_secs(1));
        assert_eq!(
            clock.elapsed(t0 + Duration::from_secs(2)),
            Duration::from_secs(2),
            "resuming a running clock must not restart it"
        );
        clock.pause(t0 + Duration::from_secs(3));
        clock.pause(t0 + Duration::from_secs(9));
        assert_eq!(
            clock.elapsed(t0 + Duration::from_secs(20)),
            Duration::from_secs(3),
            "a second pause must not bank the wait"
        );
    }

    #[test]
    fn freeze_stops_the_clock_for_good() {
        let t0 = Instant::now();
        let mut clock = RunClock::start(t0);
        let total = clock.freeze(t0 + Duration::from_secs(7));
        assert_eq!(total, Duration::from_secs(7));
        assert_eq!(
            clock.elapsed(t0 + Duration::from_secs(60)),
            Duration::from_secs(7)
        );
        // A freeze during an approval wait reports the working time only.
        let mut clock = RunClock::start(t0);
        clock.pause(t0 + Duration::from_secs(2));
        assert_eq!(
            clock.freeze(t0 + Duration::from_secs(30)),
            Duration::from_secs(2)
        );
    }
}
