//! Frame scheduling (ADR-0018 item 5): a [`FrameRequester`]/[`FrameScheduler`]
//! actor pair coalesces redraw requests and clamps the emit rate to
//! [`MIN_FRAME_INTERVAL`] — the Codex precedent, source-verified in the
//! 2026-09-13 exhibit, at 120 FPS per the maintainer directive of 2026-09-14.
//! The cap is a ceiling on demand-driven redraws, not a tick: idle draws
//! nothing (terminals expose no display-refresh query, and frames past the
//! emulator's own composite rate only burn CPU in the cell diff).
//!
//! The determinism seam (AGENTS.md): the rate gate is a pure function over
//! injected instants (the private `FrameRateLimiter`), and the actor is
//! tested under tokio's paused clock.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

/// The 120 FPS ceiling on frame emission.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 120);

/// Requests beyond this depth coalesce: a full channel means earlier requests
/// are already pending, and the frame they produce covers the dropped one.
const REQUEST_DEPTH: usize = 64;

/// One redraw demand; the app loop's draw branch receives these.
#[derive(Clone, Copy, Debug)]
pub struct Draw;

/// The clonable handle widgets and background tasks request redraws through.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    tx: mpsc::Sender<Instant>,
}

impl FrameRequester {
    /// Request a frame as soon as the rate gate allows.
    pub fn schedule_frame(&self) {
        self.schedule_frame_at(Instant::now());
    }

    /// Request a frame at `at` plus the rate gate — near-future animation
    /// horizons (blink, typewriter steps). Requests coalesce: an already-armed
    /// earlier frame absorbs this one (it fires early, covering the request),
    /// and a frame armed later never delays this one.
    pub fn schedule_frame_at(&self, at: Instant) {
        // Full = requests are already pending = this one coalesces into the
        // frame they produce. Closed = the app is gone, and a redraw request
        // is then void by definition.
        let _ = self.tx.try_send(at);
    }
}

/// The actor half. [`FrameScheduler::run`] is the task body; it exits once
/// every requester is dropped.
#[derive(Debug)]
pub struct FrameScheduler {
    rx: mpsc::Receiver<Instant>,
    draw: mpsc::Sender<Draw>,
    limiter: FrameRateLimiter,
}

/// Wire the trio: widgets hold the requester, the app loop owns the draw
/// receiver and spawns the scheduler (`tokio::spawn(scheduler.run())`).
#[must_use]
pub fn frame_scheduler() -> (FrameRequester, FrameScheduler, mpsc::Receiver<Draw>) {
    let (tx, rx) = mpsc::channel(REQUEST_DEPTH);
    let (draw, draw_rx) = mpsc::channel(1);
    (
        FrameRequester { tx },
        FrameScheduler {
            rx,
            draw,
            limiter: FrameRateLimiter::new(MIN_FRAME_INTERVAL),
        },
        draw_rx,
    )
}

impl FrameScheduler {
    pub async fn run(mut self) {
        while let Some(at) = self.rx.recv().await {
            let mut fire_at = self.limiter.clamp(at);
            while let Ok(next) = self.rx.try_recv() {
                fire_at = fire_at.min(self.limiter.clamp(next));
            }
            // Wait out the gate, interruptibly: a request arriving mid-wait
            // pulls the frame earlier (never later), so an armed far-future
            // horizon cannot stall interactive redraws. Once the frame is due
            // the request branch closes, so a request flood cannot starve the
            // draw either.
            let sleep = tokio::time::sleep_until(fire_at);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    biased;
                    maybe = self.rx.recv(), if fire_at > Instant::now() => {
                        match maybe {
                            Some(next) => {
                                fire_at = fire_at.min(self.limiter.clamp(next));
                                sleep.as_mut().reset(fire_at);
                            }
                            None => break,
                        }
                    }
                    () = &mut sleep => break,
                }
            }
            match self.draw.try_send(Draw) {
                // Full: a draw is already queued and covers this one — and
                // since draws render latest state, the gate still advances,
                // so a flooded loop throttles instead of spinning.
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {
                    self.limiter.mark_sent(Instant::now());
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
    }
}

/// The pure rate gate: after a frame leaves at `t`, the next may not leave
/// before `t + interval`.
#[derive(Debug)]
struct FrameRateLimiter {
    interval: Duration,
    next_allowed: Option<Instant>,
}

impl FrameRateLimiter {
    const fn new(interval: Duration) -> Self {
        Self {
            interval,
            next_allowed: None,
        }
    }

    /// The earliest instant a frame requested for `at` may fire.
    fn clamp(&self, at: Instant) -> Instant {
        self.next_allowed.map_or(at, |next| at.max(next))
    }

    fn mark_sent(&mut self, now: Instant) {
        self.next_allowed = Some(now + self.interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_gates_only_after_a_send() {
        let t0 = Instant::now();
        let mut limiter = FrameRateLimiter::new(MIN_FRAME_INTERVAL);
        // Nothing sent yet: requests pass through ungated.
        assert_eq!(limiter.clamp(t0), t0);
        limiter.mark_sent(t0);
        assert_eq!(limiter.clamp(t0), t0 + MIN_FRAME_INTERVAL);
        let past_the_gate = t0 + MIN_FRAME_INTERVAL * 3;
        assert_eq!(limiter.clamp(past_the_gate), past_the_gate);
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_coalesces_into_one_draw() {
        let (requester, scheduler, mut draws) = frame_scheduler();
        let task = tokio::spawn(scheduler.run());
        requester.schedule_frame();
        requester.schedule_frame();
        requester.schedule_frame();
        draws.recv().await.unwrap();
        // No trailing frames: advancing well past the gate surfaces nothing.
        tokio::time::advance(MIN_FRAME_INTERVAL * 4).await;
        assert!(draws.try_recv().is_err());
        drop(requester);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_spaces_frames() {
        let (requester, scheduler, mut draws) = frame_scheduler();
        tokio::spawn(scheduler.run());
        let t0 = Instant::now();
        requester.schedule_frame();
        draws.recv().await.unwrap();
        requester.schedule_frame();
        draws.recv().await.unwrap();
        let elapsed = t0.elapsed();
        // The gate holds (never earlier than the interval) and fires as soon
        // as allowed — tokio's timer wheel rounds sub-ms deadlines up to the
        // next millisecond.
        assert!(elapsed >= MIN_FRAME_INTERVAL, "fired early: {elapsed:?}");
        assert!(
            elapsed < MIN_FRAME_INTERVAL + Duration::from_millis(1),
            "fired late: {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_draws_nothing() {
        let (_requester, scheduler, mut draws) = frame_scheduler();
        tokio::spawn(scheduler.run());
        tokio::time::advance(MIN_FRAME_INTERVAL * 100).await;
        assert!(draws.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_future_request_fires_on_time() {
        let (requester, scheduler, mut draws) = frame_scheduler();
        tokio::spawn(scheduler.run());
        let t0 = Instant::now();
        requester.schedule_frame_at(t0 + Duration::from_millis(50));
        draws.recv().await.unwrap();
        assert_eq!(t0.elapsed(), Duration::from_millis(50));
    }

    /// An armed horizon must not stall interactive redraws: an immediate
    /// request arriving mid-wait pulls the frame earlier — and absorbs the
    /// horizon (no trailing duplicate frame).
    #[tokio::test(start_paused = true)]
    async fn an_immediate_request_preempts_an_armed_horizon() {
        let (requester, scheduler, mut draws) = frame_scheduler();
        tokio::spawn(scheduler.run());
        let t0 = Instant::now();
        // Arm the gate, then arm a horizon past it.
        requester.schedule_frame();
        draws.recv().await.unwrap();
        requester.schedule_frame_at(t0 + Duration::from_millis(100));
        // Let the scheduler arm the horizon's sleep before the keystroke.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;
        requester.schedule_frame();
        draws.recv().await.unwrap();
        assert_eq!(t0.elapsed(), Duration::from_millis(10));
        tokio::time::advance(Duration::from_millis(200)).await;
        assert!(draws.try_recv().is_err());
    }
}
