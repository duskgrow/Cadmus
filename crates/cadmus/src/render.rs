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
        // Outstanding span → tool name; provider call ids may repeat, and
        // per-call execution need not start in result order.
        let mut calls: HashMap<String, String> = HashMap::new();
        let mut attachment = first;
        loop {
            match attachment.tail.next() {
                None => break,
                Some(LiveUpdate::Lagged) => {
                    eprintln!("… progress display fell behind; resynced");
                    calls.clear();
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
        // Print mode needs no provisional feedback for a human deciding a
        // sibling; the ordered durable result below owns its progress output.
        LiveKind::AssistantDelta { .. } | LiveKind::ToolCompleted { .. } => {}
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
            EventKind::Fold { folded, .. } => {
                eprintln!("  ⑃ context folded: {} result(s) compressed", folded.len());
            }
            EventKind::ToolCall { call } => {
                calls.insert(event.span_id.clone(), call.name.clone());
                eprintln!("  → {}", call.name);
            }
            EventKind::ToolResult { call_id, .. } => {
                let name = calls.remove(&event.span_id);
                if event.status == Status::Error {
                    let tool = name.as_deref().unwrap_or(call_id);
                    let detail = event
                        .error
                        .as_ref()
                        .map_or("failed", |error| error.message.as_str());
                    eprintln!("  ✗ {tool}: {detail}");
                }
            }
            EventKind::RunFinished { .. } => return true,
            _ => {}
        },
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadmus_contract::{Event, ToolCall};
    use serde_json::json;

    fn item(span: &str, kind: EventKind) -> LiveItem {
        LiveItem {
            seq: 1,
            trace_id: "test".into(),
            kind: LiveKind::Recorded {
                event: Box::new(Event::new(
                    1,
                    "event".into(),
                    "test".into(),
                    span.into(),
                    None,
                    0,
                    kind,
                )),
            },
        }
    }

    #[test]
    fn repeated_provider_ids_keep_independent_span_names_until_each_result() {
        let mut calls = HashMap::new();
        for (span, name) in [("s1", "write_file"), ("s2", "edit_file")] {
            render(
                &item(
                    span,
                    EventKind::ToolCall {
                        call: ToolCall {
                            id: "same".into(),
                            name: name.into(),
                            arguments: json!({}),
                        },
                    },
                ),
                &mut calls,
            );
        }
        assert_eq!(calls.get("s1").map(String::as_str), Some("write_file"));
        assert_eq!(calls.get("s2").map(String::as_str), Some("edit_file"));
        for span in ["s1", "s2"] {
            render(
                &item(
                    span,
                    EventKind::ToolResult {
                        call_id: "same".into(),
                        result: json!("ok"),
                    },
                ),
                &mut calls,
            );
            assert!(!calls.contains_key(span));
        }
        assert!(calls.is_empty());
    }
}
