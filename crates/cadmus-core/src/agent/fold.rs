//! The fold machinery (ADR-0007 item 2 + the 2026-09-10 amendment): every
//! tool result's coordinates, the cadence + ceiling triggers, spill to the
//! artifact sink and placeholder substitution in the rendered request.

use cadmus_contract::{EstimateSource, EventKind, FoldedRef, Message};

use super::{AgentError, AgentLoop};
use crate::context::{fold_placeholder_message, message_text};

/// One tool result's fold-relevant coordinates: its history position (the
/// render-substitution key), its turn (the recency scope) and its event id
/// (the directive's reference — the log references by id, never by path).
#[derive(Clone)]
pub(super) struct ResultTrack {
    pub(super) msg_index: usize,
    pub(super) turn: usize,
    pub(super) event_id: String,
    pub(super) call_id: String,
}

impl AgentLoop {
    /// The fold decision point (ADR-0007 item 2 + the 2026-09-10
    /// amendment): periodic hygiene every Δ estimated tokens of growth, and
    /// the ceiling rule — fold first there; a ceiling fold with nothing
    /// foldable falls through to the pre-existing `ContextLength` path (the
    /// phase-2 compactor is that case's designed answer, not a silent carry).
    pub(super) fn maybe_fold(
        &self,
        messages: &[Message],
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let max_context = u64::from(self.provider.capabilities().max_context);
        let (estimate, estimator) = self.estimate_tokens(messages);
        let policy = &self.context.fold_policy;
        let baseline = *self.fold_baseline.lock().expect("baseline poisoned");
        let at_ceiling = estimate >= max_context * policy.ceiling_percent / 100;
        let delta = policy.growth_max_tokens.min(max_context / 10);
        if estimate.saturating_sub(baseline) < delta && !at_ceiling {
            return Ok(());
        }

        // Candidates: past the recency scope and the size floor, not already
        // folded. Locks come off before any spill IO.
        let candidates: Vec<ResultTrack> = {
            let folded = self.folded.lock().expect("folded poisoned");
            let tracks = self.tool_result_tracks.lock().expect("tracks poisoned");
            tracks
                .iter()
                .filter(|track| track.turn + policy.recent_turns < turn)
                .filter(|track| !folded.contains_key(&track.msg_index))
                .filter(|track| message_text(&messages[track.msg_index]).len() >= policy.min_bytes)
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return Ok(());
        }

        // Spill first (the directive references the artifacts), then record
        // the directive, then update the substitution state — a crash before
        // the directive leaves harmless orphan artifacts, never a dangling
        // reference (the inverse order would be the corrupt one).
        let mut folded_refs: Vec<(usize, FoldedRef, Message)> = Vec::new();
        for track in candidates {
            let original = message_text(&messages[track.msg_index]);
            let spill = self
                .context
                .artifacts
                .spill(&format!("m{}.txt", track.msg_index), &original)?;
            let fold_ref = FoldedRef {
                event_id: track.event_id,
                call_id: track.call_id,
                spill,
                original_bytes: u64::try_from(original.len()).unwrap_or(u64::MAX),
            };
            let placeholder = fold_placeholder_message(&messages[track.msg_index], &fold_ref);
            folded_refs.push((track.msg_index, fold_ref, placeholder));
        }
        let directive = self.turn_event(
            &self.next_span(),
            root_span,
            turn,
            EventKind::Fold {
                folded: folded_refs
                    .iter()
                    .map(|(_, fold_ref, _)| fold_ref.clone())
                    .collect(),
                estimate,
                estimator,
            },
        );
        self.emit(&directive)?;

        let mut folded = self.folded.lock().expect("folded poisoned");
        let mut saved_bytes = 0usize;
        for (msg_index, _, placeholder) in folded_refs {
            // Savings in the estimate's own unit (serialized render bytes),
            // so the post-fold baseline stays on the cadence's scale.
            let before = serde_json::to_vec(&messages[msg_index]).map_or(0, |v| v.len());
            let after = serde_json::to_vec(&placeholder).map_or(0, |v| v.len());
            saved_bytes += before.saturating_sub(after);
            folded.insert(msg_index, placeholder);
        }
        // The post-fold baseline: the cadence measures growth from here.
        let saved_tokens = u64::try_from(saved_bytes / 4).unwrap_or(u64::MAX);
        *self.fold_baseline.lock().expect("baseline poisoned") =
            estimate.saturating_sub(saved_tokens);
        Ok(())
    }

    /// The boundary usage estimate (amendment item 4): the provider's last
    /// reported input tokens when present — exact but for the not-yet-sent
    /// growth — else the chars/4 heuristic over the upcoming render (prefix
    /// plus history with folded substitutions applied, so the estimate
    /// measures what the model actually sees and the cadence cannot
    /// degenerate into per-turn folding; the trailer is noise here).
    fn estimate_tokens(&self, messages: &[Message]) -> (u64, EstimateSource) {
        if let Some(tokens) = *self.last_reported_input.lock().expect("usage poisoned") {
            return (tokens, EstimateSource::Provider);
        }
        let folded = self.folded.lock().expect("folded poisoned");
        let bytes = self.context.prefix.byte_len()
            + messages
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    let rendered = folded.get(&index).unwrap_or(message);
                    serde_json::to_vec(rendered).map_or(0, |bytes| bytes.len())
                })
                .sum::<usize>();
        (
            u64::try_from(bytes / 4).unwrap_or(u64::MAX),
            EstimateSource::Chars4,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cadmus_contract::{ChatRequest, Event, FinishReason, ModelError, StreamChunk, ToolSpec};
    use serde_json::{Value, json};

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::fixtures::{test_capabilities, text_of, text_script};
    use crate::agent::{AgentTool, ContextBundle, ToolError};
    use crate::testing::test_telemetry;

    /// A tool returning a fixed-size string — the fold tests' bulky result.
    struct BigTool(usize);

    #[async_trait]
    impl AgentTool for BigTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "big".into(),
                description: "returns a big string".into(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Ok(Value::String("x".repeat(self.0)))
        }
    }

    fn big_result_script(
        id: &str,
        input_tokens: Option<u64>,
    ) -> Vec<Result<StreamChunk, ModelError>> {
        let mut chunks = vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: id.into(),
                name: "big".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
        ];
        if let Some(input) = input_tokens {
            chunks.push(StreamChunk::Usage(cadmus_contract::Usage {
                input,
                ..cadmus_contract::Usage::default()
            }));
        }
        chunks.push(StreamChunk::Done {
            finish: FinishReason::ToolCalls,
        });
        ReplayProvider::script(chunks)
    }

    #[tokio::test]
    async fn fold_directive_records_refs_and_the_render_substitutes() {
        let mut capabilities = test_capabilities();
        capabilities.max_context = 10_000; // Δ = 1000 tokens; usage 1500 fires it
        let provider = Arc::new(
            ReplayProvider::new([big_result_script("c1", Some(1_500)), text_script("done")])
                .with_capabilities(capabilities),
        );
        let (telemetry, sink) = test_telemetry("tr-fold");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: test_fold_policy(0),
                ..crate::testing::test_context()
            },
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        let outcome = agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let events = sink.events();
        let (folded_refs, estimate, estimator) = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::Fold {
                    folded,
                    estimate,
                    estimator,
                } => Some((folded, estimate, estimator)),
                _ => None,
            })
            .expect("a fold directive at the turn-2 boundary");
        assert_eq!(*estimate, 1_500);
        assert_eq!(*estimator, EstimateSource::Provider);
        assert_eq!(folded_refs.len(), 1);
        let fold_ref = &folded_refs[0];
        assert_eq!(fold_ref.call_id, "c1");
        assert_eq!(fold_ref.original_bytes, 3_000);
        // The directive references the result's event id, and that event
        // exists in the same log (the id-reference discipline).
        assert!(events.iter().any(|event| event.id == fold_ref.event_id
            && matches!(event.kind, EventKind::ToolResult { .. })));
        // The spill keeps the full text outside the log.
        assert_eq!(
            artifacts.spills().values().next().map(String::len),
            Some(3_000)
        );

        // The render substituted the placeholder; the live history and the
        // replayed fold keep the full text (fold invariant).
        let requests = provider.requests();
        let rendered = &requests[1].messages[3];
        assert_eq!(rendered.tool_call_id.as_deref(), Some("c1"));
        let placeholder = text_of(rendered);
        assert!(placeholder.contains("[COMPRESSED"), "{placeholder}");
        assert!(placeholder.len() < 3_000);
        assert_eq!(text_of(&outcome.messages[2]).len(), 3_000);
        let folded = crate::replay_trace(&events);
        assert_eq!(folded.messages, outcome.messages);
    }

    #[tokio::test]
    async fn fold_estimator_falls_back_to_chars4_without_reported_usage() {
        let mut capabilities = test_capabilities();
        capabilities.max_context = 4_000; // Δ = 400; the ~800-token chars/4 estimate fires it
        let provider = Arc::new(
            ReplayProvider::new([big_result_script("c1", None), text_script("done")])
                .with_capabilities(capabilities),
        );
        let (telemetry, sink) = test_telemetry("tr-fold-chars4");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: test_fold_policy(0),
                ..crate::testing::test_context()
            },
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let estimator = sink.events().iter().find_map(|event| match &event.kind {
            EventKind::Fold { estimator, .. } => Some(*estimator),
            _ => None,
        });
        assert_eq!(estimator, Some(EstimateSource::Chars4));
    }

    #[tokio::test]
    async fn recent_results_stay_verbatim_within_the_scope() {
        let mut capabilities = test_capabilities();
        capabilities.max_context = 10_000;
        let provider = Arc::new(
            ReplayProvider::new([
                big_result_script("c1", Some(1_500)),
                big_result_script("c2", Some(1_500)),
                text_script("done"),
            ])
            .with_capabilities(capabilities),
        );
        let (telemetry, sink) = test_telemetry("tr-fold-recent");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: test_fold_policy(1),
                ..crate::testing::test_context()
            }, // only results strictly older than one turn fold
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let events = sink.events();
        let fold_events: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Fold { .. }))
            .collect();
        assert_eq!(fold_events.len(), 1, "only the turn-3 boundary can fold");
        let EventKind::Fold { folded, .. } = &fold_events[0].kind else {
            unreachable!()
        };
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].call_id, "c1", "turn 2's result stays verbatim");

        let requests = provider.requests();
        let last = &requests[2].messages;
        assert!(text_of(&last[3]).contains("[COMPRESSED"), "c1 folded");
        assert_eq!(text_of(&last[5]).len(), 3_000, "c2 verbatim");
    }

    #[tokio::test]
    async fn a_spill_failure_is_fatal_not_silent() {
        struct FailingArtifacts;
        impl cadmus_contract::ArtifactSink for FailingArtifacts {
            fn spill(
                &self,
                name: &str,
                _content: &str,
            ) -> Result<String, cadmus_contract::LogError> {
                Err(cadmus_contract::LogError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("cannot write {name}"),
                )))
            }
        }

        let mut capabilities = test_capabilities();
        capabilities.max_context = 10_000;
        let provider = Arc::new(
            ReplayProvider::new([big_result_script("c1", Some(1_500))])
                .with_capabilities(capabilities),
        );
        let (telemetry, _sink) = test_telemetry("tr-fold-fail");
        let context = ContextBundle {
            artifacts: Arc::new(FailingArtifacts),
            fold_policy: test_fold_policy(0),
            ..crate::testing::test_context()
        };
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            context,
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        let err = agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect_err("a spill failure aborts the run");
        assert!(matches!(err, AgentError::Log(_)));
    }

    /// Runs a scripted fold scenario; returns the sink's events and the
    /// provider's recorded requests.
    async fn fold_scenario(
        scripts: Vec<Vec<Result<StreamChunk, ModelError>>>,
        max_context: u32,
        policy: crate::context::FoldPolicy,
        tool_size: usize,
    ) -> (
        Vec<Event>,
        Vec<ChatRequest>,
        Arc<crate::testing::RecordingArtifacts>,
    ) {
        let mut capabilities = test_capabilities();
        capabilities.max_context = max_context;
        let provider = Arc::new(ReplayProvider::new(scripts).with_capabilities(capabilities));
        let (telemetry, sink) = test_telemetry("tr-fold-scenario");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(tool_size))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: policy,
                ..crate::testing::test_context()
            },
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");
        (sink.events(), provider.requests(), artifacts)
    }

    /// The early-firing test policy (small numbers so short runs fold).
    fn test_fold_policy(recent_turns: usize) -> crate::context::FoldPolicy {
        crate::context::FoldPolicy {
            recent_turns,
            min_bytes: 64,
            growth_max_tokens: 100_000,
            ceiling_percent: 80,
        }
    }

    fn fold_events(events: &[Event]) -> Vec<&[FoldedRef]> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Fold { folded, .. } => Some(folded.as_slice()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn ceiling_fires_without_new_growth_after_a_fold() {
        // Turn 1 folds on the cadence; turn 2's growth is under Δ but still
        // over the 80% ceiling — the ceiling rule, not the cadence, folds it.
        let (events, _, _) = fold_scenario(
            vec![
                big_result_script("c1", Some(8_500)),
                big_result_script("c2", Some(8_600)),
                text_script("done"),
            ],
            10_000, // Δ = 1000, ceiling = 8000
            test_fold_policy(0),
            3_000,
        )
        .await;
        let folds = fold_events(&events);
        assert_eq!(folds.len(), 2, "cadence folds t1, the ceiling folds t2");
        assert_eq!(folds[1][0].call_id, "c2");
    }

    #[tokio::test]
    async fn ceiling_with_nothing_foldable_just_continues() {
        // Over the ceiling with every result still inside the recency scope:
        // no directive, no error — the pre-existing ContextLength path owns
        // this case until the phase-2 compactor lands.
        let (events, _, _) = fold_scenario(
            vec![big_result_script("c1", Some(8_500)), text_script("done")],
            10_000,
            test_fold_policy(5),
            3_000,
        )
        .await;
        assert!(fold_events(&events).is_empty());
    }

    #[tokio::test]
    async fn the_second_fold_waits_for_delta_of_new_growth() {
        // t1 folds at 1500 (baseline ≈ 1050 post-fold); t2's 1800 is under
        // baseline+Δ, so no fold; t3's 2200 crosses it. The second directive
        // skips the already-folded c1.
        let (events, _, _) = fold_scenario(
            vec![
                big_result_script("c1", Some(1_500)),
                big_result_script("c2", Some(1_800)),
                big_result_script("c3", Some(2_200)),
                text_script("done"),
            ],
            10_000,
            test_fold_policy(0),
            3_000,
        )
        .await;
        let folds = fold_events(&events);
        assert_eq!(folds.len(), 2, "boundaries 2 and 4 fold, 3 waits");
        assert_eq!(folds[0].len(), 1);
        assert_eq!(folds[0][0].call_id, "c1");
        let second: Vec<&str> = folds[1].iter().map(|r| r.call_id.as_str()).collect();
        assert_eq!(second, vec!["c2", "c3"], "already-folded results skip");
    }

    #[tokio::test]
    async fn results_under_the_size_floor_are_never_folded() {
        // The trigger fires (over the ceiling), but a 1 KB result under the
        // 2 KB floor is not a candidate: a placeholder would not shrink it.
        let (events, _, _) = fold_scenario(
            vec![big_result_script("c1", Some(8_500)), text_script("done")],
            10_000,
            crate::context::FoldPolicy {
                min_bytes: 2_048,
                ..test_fold_policy(0)
            },
            1_000,
        )
        .await;
        assert!(fold_events(&events).is_empty());
    }
}
