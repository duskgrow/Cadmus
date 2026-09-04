//! Test doubles for the telemetry ports — the determinism seam for loop and
//! trajectory tests. These are fakes (behavior, not mocks): the recording
//! sink keeps every event so tests assert on the trajectory itself.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cadmus_contract::{Clock, Event, EventSink, IdSequence, LogError};

use crate::Telemetry;

/// An in-memory [`EventSink`] keeping every appended event, in order.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingSink {
    /// A snapshot of everything appended so far.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("recording sink poisoned").clone()
    }
}

impl EventSink for RecordingSink {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.events
            .lock()
            .expect("recording sink poisoned")
            .push(event.clone());
        Ok(())
    }
}

/// A stopped clock: every timestamp is the same fixed instant.
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        self.0
    }
}

/// Sequential ids: 1, 2, 3, …
///
/// Deliberately duplicated from `cadmus::telemetry::SeqIds` (the wiring
/// layer's real impl); keep the two in sync (five lines each).
#[derive(Default)]
pub struct SeqIds(AtomicU64);

impl IdSequence for SeqIds {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// A telemetry bundle over a recording sink; the sink handle comes back for
/// assertions on the emitted trajectory.
#[must_use]
pub fn test_telemetry(trace_id: &str) -> (Telemetry, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let telemetry = Telemetry {
        sink: sink.clone(),
        clock: Arc::new(FixedClock(1_788_393_600_000)),
        ids: Arc::new(SeqIds::default()),
        trace_id: trace_id.into(),
        run_attributes: BTreeMap::new(),
    };
    (telemetry, sink)
}
