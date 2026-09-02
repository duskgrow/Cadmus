use std::collections::BTreeMap;

use cadmus_contract::{ContentPart, FinishReason, Message, Role, StreamChunk, ToolCall, Usage};
use serde_json::Value;

/// The result of folding one provider stream into a domain turn.
#[derive(Debug, Clone)]
pub struct AssembledTurn {
    /// Always `Role::Assistant`.
    pub message: Message,
    /// `None` when the provider never reported usage (a missing terminal
    /// record means truncation, not a zero-usage success — pitfall #8).
    pub usage: Option<Usage>,
    pub finish: FinishReason,
    /// Non-fatal anomalies worth surfacing in the trajectory: quarantined
    /// truncated calls, fragments without a start, overwritten indexes…
    pub warnings: Vec<String>,
    pub outcome: TurnOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Text and/or tool calls present.
    Content,
    /// The stream ended with open tool calls or without a terminal record —
    /// truncated, not a zero-usage success (pitfall #8).
    Truncated,
    /// No text, no reasoning, no tool calls. Legitimate result or error is
    /// decided by the finish reason (pitfall #5): a reasoning model that
    /// spends its whole budget on hidden thinking returns empty content with
    /// `Length`, which is retryable/escalatable, while `Stop` is a protocol
    /// anomaly.
    Empty,
}

struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Folds a stream of [`StreamChunk`] into an [`AssembledTurn`]. Incremental
/// tool-call arguments are merged by `index` — interleaved parallel calls and
/// endpoints that emit a call in one shot are both handled (pitfall #1).
#[derive(Default)]
pub struct MessageAssembler {
    text: String,
    reasoning: String,
    partials: BTreeMap<u32, PartialCall>,
    completed: Vec<(u32, ToolCall)>,
    opaque: Option<Value>,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    warnings: Vec<String>,
}

impl MessageAssembler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: StreamChunk) {
        match chunk {
            StreamChunk::TextDelta(delta) => self.text.push_str(&delta),
            StreamChunk::ReasoningDelta(delta) => self.reasoning.push_str(&delta),
            StreamChunk::ToolCallStart { index, id, name } => {
                if self
                    .partials
                    .insert(
                        index,
                        PartialCall {
                            id,
                            name,
                            args: String::new(),
                        },
                    )
                    .is_some()
                {
                    self.warnings.push(format!(
                        "tool call index {index} reused; previous fragments dropped"
                    ));
                }
            }
            StreamChunk::ToolArgsDelta { index, fragment } => {
                if let Some(partial) = self.partials.get_mut(&index) {
                    partial.args.push_str(&fragment);
                } else {
                    // The adapter is responsible for emitting a start first;
                    // stay lossless but loud rather than dropping data.
                    self.warnings.push(format!(
                        "arguments fragment for unknown tool call index {index}; call synthesized"
                    ));
                    self.partials.insert(
                        index,
                        PartialCall {
                            id: format!("call_{index}"),
                            name: String::new(),
                            args: fragment,
                        },
                    );
                }
            }
            StreamChunk::ToolCallEnd { index } => {
                if let Some(partial) = self.partials.remove(&index) {
                    self.seal_call(index, partial);
                } else {
                    self.warnings
                        .push(format!("tool call end for unknown index {index}; ignored"));
                }
            }
            StreamChunk::OpaqueDelta(value) => merge_opaque(&mut self.opaque, value),
            StreamChunk::Usage(usage) => self.usage = Some(usage),
            StreamChunk::Done { finish } => self.finish = Some(finish),
        }
    }

    /// Ends the stream: incomplete calls (never sealed, or sealed with invalid
    /// JSON) are quarantined — dropped with a warning — while legal text and
    /// sibling calls are kept (pitfall #3).
    #[must_use]
    pub fn complete(mut self) -> AssembledTurn {
        for (index, partial) in std::mem::take(&mut self.partials) {
            self.warnings.push(format!(
                "stream ended with open tool call {} (index {index}); call quarantined",
                partial.id
            ));
        }

        let mut content: Vec<ContentPart> = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(ContentPart::Reasoning {
                text: std::mem::take(&mut self.reasoning),
            });
        }
        if !self.text.is_empty() {
            content.push(ContentPart::Text {
                text: std::mem::take(&mut self.text),
            });
        }
        // Emit calls ordered by index, not by completion order: index is the
        // routing identity of a call (pitfall #1) and OpenAI orders the final
        // message's tool_calls by it.
        self.completed.sort_by_key(|(index, _)| *index);
        content.extend(
            self.completed
                .into_iter()
                .map(|(_, call)| ContentPart::ToolCall { call }),
        );

        let had_open_calls = self.warnings.iter().any(|w| w.contains("open tool call"));
        let outcome = if content.is_empty() {
            TurnOutcome::Empty
        } else if had_open_calls || self.finish.is_none() {
            TurnOutcome::Truncated
        } else {
            TurnOutcome::Content
        };

        AssembledTurn {
            message: Message {
                role: Role::Assistant,
                content,
                tool_call_id: None,
                opaque: self.opaque,
            },
            usage: self.usage,
            finish: self.finish.unwrap_or(FinishReason::Stop),
            warnings: self.warnings,
            outcome,
        }
    }

    fn seal_call(&mut self, index: u32, partial: PartialCall) {
        let args = partial.args.trim();
        let parsed = if args.is_empty() {
            // A call with no arguments is legal; an empty body is `{}`.
            Ok(Value::Object(serde_json::Map::new()))
        } else {
            serde_json::from_str(args)
        };
        match parsed {
            Ok(arguments) => self.completed.push((
                index,
                ToolCall {
                    id: partial.id,
                    name: partial.name,
                    arguments,
                },
            )),
            Err(err) => self.warnings.push(format!(
                "tool call {} has malformed arguments ({err}); call quarantined",
                partial.id
            )),
        }
    }
}

/// Folds vendor-opaque deltas into one value: objects merge key-wise, a newer
/// scalar replaces the previous one. The core never interprets the content.
fn merge_opaque(slot: &mut Option<Value>, value: Value) {
    match slot.as_mut() {
        Some(Value::Object(existing)) => {
            if let Value::Object(new) = value {
                existing.extend(new);
            } else {
                *slot = Some(value);
            }
        }
        Some(_) | None => *slot = Some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadmus_contract::FinishReason;
    use serde_json::json;

    fn assemble(chunks: Vec<StreamChunk>) -> AssembledTurn {
        let mut assembler = MessageAssembler::new();
        for chunk in chunks {
            assembler.push(chunk);
        }
        assembler.complete()
    }

    fn text_turn(text: &str) -> Vec<StreamChunk> {
        vec![
            StreamChunk::TextDelta(text.to_string()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ]
    }

    // Pitfall #1: incremental tool-call aggregation with interleaved
    // parallel calls — fragments of two calls must stay separated by index.
    #[test]
    fn aggregates_interleaved_parallel_tool_calls() {
        let turn = assemble(vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: "call_a".into(),
                name: "read_file".into(),
            },
            StreamChunk::ToolCallStart {
                index: 1,
                id: "call_b".into(),
                name: "grep".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 0,
                fragment: "{\"path\":\"/a".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 1,
                fragment: "{\"pattern\":\"fn".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 0,
                fragment: "\"}".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 1,
                fragment: " main\"}".into(),
            },
            StreamChunk::ToolCallEnd { index: 1 },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::Done {
                finish: FinishReason::ToolCalls,
            },
        ]);
        assert_eq!(turn.outcome, TurnOutcome::Content);
        let calls: Vec<_> = turn.message.tool_calls().cloned().collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].arguments, json!({"path": "/a"}));
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].arguments, json!({"pattern": "fn main"}));
        assert!(turn.warnings.is_empty());
    }

    // Pitfall #1, dual mode: an endpoint that emits a whole call in one shot
    // (llama.cpp style) is just start + zero-or-one delta + end.
    #[test]
    fn aggregates_one_shot_tool_call() {
        let turn = assemble(vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "noop".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::Done {
                finish: FinishReason::ToolCalls,
            },
        ]);
        let calls: Vec<_> = turn.message.tool_calls().cloned().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({}));
    }

    // Pitfall #3: a truncated call is quarantined, but legal text and sibling
    // calls survive — a naive implementation drops the whole turn.
    #[test]
    fn quarantines_truncated_call_keeps_text_and_siblings() {
        let turn = assemble(vec![
            StreamChunk::TextDelta("let me check".into()),
            StreamChunk::ToolCallStart {
                index: 0,
                id: "good".into(),
                name: "read_file".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 0,
                fragment: "{\"path\":\"/a\"}".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::ToolCallStart {
                index: 1,
                id: "broken".into(),
                name: "read_file".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 1,
                fragment: "{\"path\":\"/b".into(),
            },
            StreamChunk::Done {
                finish: FinishReason::Length,
            },
        ]);
        assert_eq!(turn.outcome, TurnOutcome::Truncated);
        let calls: Vec<_> = turn.message.tool_calls().cloned().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "good");
        assert!(
            turn.warnings
                .iter()
                .any(|w| w.contains("open tool call broken"))
        );
        assert!(
            turn.message
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Text { text } if text == "let me check"))
        );
    }

    // Pitfall #3 variant: the call is sealed but its JSON is malformed.
    #[test]
    fn quarantines_malformed_arguments() {
        let turn = assemble(vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: "broken".into(),
                name: "read_file".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 0,
                fragment: "{not json".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::Done {
                finish: FinishReason::ToolCalls,
            },
        ]);
        assert_eq!(turn.outcome, TurnOutcome::Empty);
        assert!(turn.message.tool_calls().next().is_none());
        assert!(
            turn.warnings
                .iter()
                .any(|w| w.contains("malformed arguments"))
        );
    }

    // Pitfall #5: empty turn semantics — no text, reasoning or calls. The
    // outcome is Empty and the finish reason travels with it so the caller
    // can distinguish "model spent everything on hidden thinking" (Length)
    // from a protocol anomaly (Stop).
    #[test]
    fn classifies_empty_turn_by_finish_reason() {
        let turn = assemble(vec![
            StreamChunk::Usage(Usage {
                output: 0,
                reasoning: 512,
                ..Usage::default()
            }),
            StreamChunk::Done {
                finish: FinishReason::Length,
            },
        ]);
        assert_eq!(turn.outcome, TurnOutcome::Empty);
        assert_eq!(turn.finish, FinishReason::Length);
        assert_eq!(turn.usage.expect("usage").reasoning, 512);
    }

    // Pitfall #5: reasoning-only content is NOT an empty turn — hidden
    // reasoning that surfaces as ReasoningDelta must be preserved.
    #[test]
    fn reasoning_only_turn_is_content() {
        let turn = assemble(vec![
            StreamChunk::ReasoningDelta("thinking…".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ]);
        assert_eq!(turn.outcome, TurnOutcome::Content);
        assert!(matches!(
            turn.message.content.first(),
            Some(ContentPart::Reasoning { text }) if text == "thinking…"
        ));
    }

    // Pitfall #8: a stream ending without a terminal record (no Done) is
    // truncation, not a zero-usage success.
    #[test]
    fn missing_terminal_record_is_truncated() {
        let turn = assemble(vec![StreamChunk::TextDelta("partial".into())]);
        assert_eq!(turn.outcome, TurnOutcome::Truncated);
    }

    // Opaque deltas fold into Message::opaque for verbatim echo-back.
    #[test]
    fn folds_opaque_deltas() {
        let mut chunks = vec![
            StreamChunk::OpaqueDelta(json!({"thought_signature": "abc"})),
            StreamChunk::OpaqueDelta(json!({"other": 1})),
        ];
        chunks.extend(text_turn("hi"));
        let turn = assemble(chunks);
        assert_eq!(
            turn.message.opaque,
            Some(json!({"thought_signature": "abc", "other": 1}))
        );
    }
}
