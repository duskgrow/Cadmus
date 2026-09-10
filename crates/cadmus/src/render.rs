//! Headless chat's live progress renderer: a plain thread draining the
//! attach tail, printing structured progress to stderr (turn blocks, tool
//! activity, denials). This is the interaction surface, not logging —
//! tracing stays behind `RUST_LOG`, and the final answer still goes to
//! stdout alone (open-items: interaction surfaces never render logs).
//!
//! Text deltas are deliberately not rendered: headless chat is print mode,
//! the answer lands at the end. The TUI's streaming markdown rendering is
//! ADR-0011 item 3's floor, over this same stream.

use std::collections::HashMap;
use std::sync::Arc;

use cadmus_contract::{EventKind, LiveItem, LiveKind, LiveUpdate, Status, attrs};
use cadmus_transport::Broadcaster;

/// Renders a run's progress. The caller attaches synchronously *before*
/// anything can publish (position 0 by construction — the attach-inside-
/// the-thread variant races the run's first events into the discarded
/// baseline), then hands the attachment in. The thread renders until the
/// run's terminal record, the tail's end, or a lag re-sync; `run_chat`
/// joins it after `Broadcaster::close`, which guarantees the tail ends even
/// on paths without a terminal record (a failed trajectory log aborts the
/// run mid-flight).
pub fn spawn(
    broadcaster: Arc<Broadcaster>,
    first: cadmus_contract::Attachment,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // call id → tool name, so a failed result line can name its tool.
        let mut calls: HashMap<String, String> = HashMap::new();
        let mut attachment = first;
        loop {
            match attachment.tail.next() {
                None => break,
                Some(LiveUpdate::Lagged) => {
                    eprintln!("… progress display fell behind; resynced");
                    attachment = broadcaster.attach();
                }
                Some(LiveUpdate::Item { item }) => {
                    if render(&item, &mut calls) {
                        break;
                    }
                }
            }
        }
    })
}

/// Renders one item; returns true at the run's terminal record.
fn render(item: &LiveItem, calls: &mut HashMap<String, String>) -> bool {
    match &item.kind {
        LiveKind::AssistantDelta { .. } => {}
        LiveKind::ApprovalRequested { calls: batch, .. } => {
            let names = batch
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("  ! approval requested: {names}");
        }
        LiveKind::Recorded { event } => match &event.kind {
            EventKind::LlmRequest { .. } => {
                let turn = event
                    .attributes
                    .get(attrs::TURN)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                eprintln!("• turn {turn}");
            }
            EventKind::InstructionInjected { path, .. } => {
                eprintln!("  + instructions: {path}");
            }
            EventKind::ToolCall { call } => {
                calls.insert(call.id.clone(), call.name.clone());
                eprintln!("  → {}", call.name);
            }
            EventKind::ToolResult { call_id, .. } if event.status == Status::Error => {
                let tool = calls.get(call_id).map_or(call_id.as_str(), String::as_str);
                let detail = event
                    .error
                    .as_ref()
                    .map_or("failed", |error| error.message.as_str());
                eprintln!("  ✗ {tool}: {detail}");
            }
            EventKind::RunFinished { .. } => return true,
            _ => {}
        },
    }
    false
}
