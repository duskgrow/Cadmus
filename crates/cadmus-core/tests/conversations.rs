//! Phase-0 acceptance: insta snapshots locking the message sequences of three
//! fixed conversations driven through the agent loop with a recorded-replay
//! provider (no live calls, fully deterministic).

use std::sync::Arc;

use async_trait::async_trait;
use cadmus_contract::{ChatRequest, FinishReason, ModelError, StreamChunk, ToolSpec};
use cadmus_core::{AgentLoop, AgentTool, ReplayProvider, ToolError};
use serde_json::{Value, json};

struct ReadFileTool;

#[async_trait]
impl AgentTool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "read a file from disk".into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let path = arguments["path"].as_str().unwrap_or("?");
        Ok(Value::String(format!("contents of {path}")))
    }
}

struct GrepTool;

#[async_trait]
impl AgentTool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "search file contents".into(),
            parameters: json!({
                "type": "object",
                "properties": {"pattern": {"type": "string"}},
                "required": ["pattern"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let pattern = arguments["pattern"].as_str().unwrap_or("?");
        Ok(Value::String(format!("3 matches for {pattern}")))
    }
}

fn text_turn(text: &str) -> Vec<Result<StreamChunk, ModelError>> {
    ReplayProvider::script(vec![
        StreamChunk::TextDelta(text.into()),
        StreamChunk::Done {
            finish: FinishReason::Stop,
        },
    ])
}

fn tool_turn(calls: &[(&str, &str, &str)]) -> Vec<Result<StreamChunk, ModelError>> {
    let mut chunks = Vec::new();
    for (index, (id, name, args)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("tool call index");
        chunks.push(StreamChunk::ToolCallStart {
            index,
            id: (*id).into(),
            name: (*name).into(),
        });
        chunks.push(StreamChunk::ToolArgsDelta {
            index,
            fragment: (*args).into(),
        });
    }
    for index in 0..calls.len() {
        chunks.push(StreamChunk::ToolCallEnd {
            index: u32::try_from(index).expect("tool call index"),
        });
    }
    chunks.push(StreamChunk::Done {
        finish: FinishReason::ToolCalls,
    });
    ReplayProvider::script(chunks)
}

async fn run(scripts: Vec<Vec<Result<StreamChunk, ModelError>>>) -> Vec<cadmus_contract::Message> {
    let provider = Arc::new(ReplayProvider::new(scripts));
    let tools: Vec<Arc<dyn AgentTool>> = vec![Arc::new(ReadFileTool), Arc::new(GrepTool)];
    let agent = AgentLoop::new(provider, tools, 8);
    let outcome = agent
        .run(&ChatRequest::user_text("look up TODOs in main.rs", 4_096))
        .await
        .expect("run");
    outcome.messages
}

// Conversation 1: a plain question with a plain answer.
#[tokio::test]
async fn snapshot_plain_qa() {
    let messages = run(vec![text_turn("There is one TODO at line 42.")]).await;
    insta::assert_json_snapshot!(messages);
}

// Conversation 2: one tool call, one tool result, final answer.
#[tokio::test]
async fn snapshot_single_tool_call() {
    let messages = run(vec![
        tool_turn(&[("call_1", "read_file", "{\"path\":\"src/main.rs\"}")]),
        text_turn("The file has one TODO at line 42."),
    ])
    .await;
    insta::assert_json_snapshot!(messages);
}

// Conversation 3: two parallel tool calls in one assistant turn.
#[tokio::test]
async fn snapshot_parallel_tool_calls() {
    let messages = run(vec![
        tool_turn(&[
            ("call_1", "read_file", "{\"path\":\"src/main.rs\"}"),
            ("call_2", "grep", "{\"pattern\":\"TODO\"}"),
        ]),
        text_turn("main.rs line 42 has a TODO; 3 matches in total."),
    ])
    .await;
    insta::assert_json_snapshot!(messages);
}
