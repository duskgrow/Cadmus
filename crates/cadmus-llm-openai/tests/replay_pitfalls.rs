//! Wire-level replay coverage of the streaming pitfall list (report §4.1,
//! phase-0 acceptance): recorded-shape SSE fixtures are driven through the
//! real stack — genai parsing, the dialect seam's stream mapping, and the
//! core's assembly — never live calls. See `fixtures/README.md` for fixture
//! provenance.

mod support;

use cadmus_contract::{ChatRequest, ContentPart, FinishReason, Provider, ToolSpec};
use cadmus_core::{AssembledTurn, MessageAssembler, TurnOutcome};
use cadmus_llm_openai::{DeepSeekDialect, Dialect, KimiDialect};
use serde_json::{Value, json};
use support::stub::RecordedRequest;
use support::wire::StubProvider;
use tokio_stream::StreamExt;

async fn assemble_fixture(
    dialect: Box<dyn Dialect>,
    fixture: &str,
    request: &ChatRequest,
) -> (AssembledTurn, Vec<RecordedRequest>) {
    let provider = StubProvider::new(dialect);
    provider.queue_raw_sse(fixture);
    let stream = provider
        .chat_stream(request)
        .await
        .expect("fixture call succeeds");
    let chunks: Vec<_> = stream.collect().await;

    let mut assembler = MessageAssembler::new();
    for chunk in chunks {
        assembler.push(chunk.expect("fixture streams are all-Ok"));
    }
    (assembler.complete(), provider.requests())
}

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("{name} tool"),
        parameters: json!({"type": "object"}),
    }
}

// Pitfall #1 (tool_calls 增量聚合): two parallel calls on the wire — `call_a`
// fragmented across two interleaved events, `call_b` in one shot — must
// assemble into two intact calls ordered by index. Usage rides the finishing
// chunk, the most common `include_usage` shape.
#[tokio::test]
async fn tool_call_incremental_aggregation_replay() {
    let request = ChatRequest {
        tools: vec![tool_spec("read_file"), tool_spec("grep")],
        ..ChatRequest::user_text("check /a for TODOs", 4_096)
    };
    let (turn, requests) = assemble_fixture(
        Box::new(KimiDialect::k3()),
        include_str!("fixtures/tool_calls_interleaved.sse"),
        &request,
    )
    .await;

    assert_eq!(turn.outcome, TurnOutcome::Content);
    assert_eq!(turn.finish, FinishReason::ToolCalls);
    assert!(turn.warnings.is_empty(), "warnings: {:?}", turn.warnings);

    let calls: Vec<_> = turn.message.tool_calls().collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, json!({"path": "/a"}));
    assert_eq!(calls[1].id, "call_b");
    assert_eq!(calls[1].name, "grep");
    assert_eq!(calls[1].arguments, json!({"pattern": "TODO"}));

    let usage = turn.usage.expect("usage rides the finishing chunk");
    assert_eq!(usage.input, 120);
    assert_eq!(usage.output, 25);

    // Request-side seam: the dialect's model and streaming mode on the wire.
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/chat/completions");
    let body: Value = serde_json::from_str(&request.body).expect("request body is json");
    assert_eq!(body["model"], "kimi-k3");
    assert_eq!(body["stream"], true);
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
}

// 空 reasoning 字段 (evidence: kimi-code issue 2226 — empty reasoning_content
// padding breaks naive stream merging): empty-string and null reasoning frames
// plus empty content frames interleaved with real deltas must not corrupt the
// turn — no phantom empty parts, bodies exact. Uses the trailing usage-only
// chunk shape (`choices: []`).
#[tokio::test]
async fn empty_reasoning_field_replay() {
    let (turn, _) = assemble_fixture(
        Box::new(DeepSeekDialect::v4_flash()),
        include_str!("fixtures/empty_reasoning_field.sse"),
        &ChatRequest::user_text("what is the answer", 4_096),
    )
    .await;

    assert_eq!(turn.outcome, TurnOutcome::Content);
    assert_eq!(turn.finish, FinishReason::Stop);
    assert!(turn.warnings.is_empty(), "warnings: {:?}", turn.warnings);
    assert_eq!(
        turn.message.content,
        vec![
            ContentPart::Reasoning {
                text: "Let me think.".into()
            },
            ContentPart::Text {
                text: "The answer is 42.".into()
            },
        ],
        "empty reasoning/content frames must not corrupt or pad the turn"
    );

    let usage = turn.usage.expect("trailing usage-only chunk");
    assert_eq!(usage.input, 50);
    assert_eq!(usage.output, 12);
}
