//! Contract ↔ genai message and metadata mapping. Pure functions — the
//! recorded-replay tests exercise them without any HTTP.

use cadmus_contract::{ContentPart, FinishReason, Message, ModelError, Role, Usage};
use genai::chat::{
    ChatMessage, ChatRequest as GenaiChatRequest, ContentPart as GenaiContentPart, StopReason,
    Tool as GenaiTool, ToolCall as GenaiToolCall, ToolResponse,
};

/// Maps the contract history into a genai request: leading system messages
/// are hoisted into genai's dedicated `system` slot, the rest becomes chat
/// messages. `echo_reasoning` is the dialect's echo obligation — stripping
/// reasoning when there is none is an explicit policy, not a silent drop.
pub fn build_genai_request(
    request: &cadmus_contract::ChatRequest,
    echo_reasoning: bool,
) -> Result<GenaiChatRequest, ModelError> {
    let mut system: Vec<&str> = Vec::new();
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut leading_system = true;

    for message in &request.messages {
        if leading_system && message.role == Role::System {
            system.push(text_of(message)?);
            continue;
        }
        leading_system = false;
        messages.push(map_message(message, echo_reasoning)?);
    }

    let mut genai_request = GenaiChatRequest::new(messages);
    if !system.is_empty() {
        genai_request.system = Some(system.join("\n\n"));
    }
    if !request.tools.is_empty() {
        genai_request.tools = Some(request.tools.iter().map(map_tool_spec).collect());
    }
    Ok(genai_request)
}

fn map_message(message: &Message, echo_reasoning: bool) -> Result<ChatMessage, ModelError> {
    match message.role {
        Role::System => Ok(ChatMessage::system(text_of(message)?)),
        Role::User => Ok(ChatMessage::user(text_of(message)?)),
        Role::Tool => {
            let call_id = message.tool_call_id.as_deref().ok_or_else(|| {
                ModelError::InvalidRequest("tool message is missing its tool_call_id".into())
            })?;
            Ok(ChatMessage::tool(ToolResponse::new(
                call_id,
                text_of(message)?,
            )))
        }
        Role::Assistant => {
            let mut parts: Vec<GenaiContentPart> = Vec::new();
            for part in &message.content {
                match part {
                    ContentPart::Text { text } => parts.push(GenaiContentPart::Text(text.clone())),
                    ContentPart::Reasoning { text } => {
                        if echo_reasoning {
                            parts.push(GenaiContentPart::ReasoningContent(text.clone()));
                        }
                    }
                    ContentPart::ToolCall { call } => {
                        parts.push(GenaiContentPart::ToolCall(GenaiToolCall {
                            call_id: call.id.clone(),
                            fn_name: call.name.clone(),
                            fn_arguments: call.arguments.clone(),
                            thought_signatures: None,
                        }));
                    }
                    ContentPart::Image { .. } => {
                        return Err(ModelError::CapabilityMismatch(
                            "image parts are not wired for OpenAI-compatible dialects".into(),
                        ));
                    }
                }
            }
            Ok(ChatMessage::assistant(
                genai::chat::MessageContent::from_parts(parts),
            ))
        }
    }
}

fn map_tool_spec(spec: &cadmus_contract::ToolSpec) -> GenaiTool {
    GenaiTool {
        name: spec.name.clone().into(),
        description: Some(spec.description.clone()),
        schema: Some(spec.parameters.clone()),
        strict: None,
        config: None,
    }
}

/// The text payload of a message; tool results and user/system turns are
/// single-text by construction. Multiple text parts join with newlines.
fn text_of(message: &Message) -> Result<&str, ModelError> {
    let mut texts = message.content.iter().filter_map(|part| match part {
        ContentPart::Text { text } => Some(text.as_str()),
        _ => None,
    });
    let first = texts.next().unwrap_or("");
    if texts.next().is_some() {
        return Err(ModelError::InvalidRequest(
            "multi-text-part messages are not supported on this wire".into(),
        ));
    }
    Ok(first)
}

/// The three-bucket usage normalization: genai's `prompt_tokens` is the total
/// input including cache hits, so the uncached bucket is derived.
pub fn map_usage(usage: &genai::chat::Usage) -> Usage {
    let as_u64 = |value: Option<i32>| u64::try_from(value.unwrap_or(0)).unwrap_or_default();
    let total_input = as_u64(usage.prompt_tokens);
    let cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .map_or(0, |details| as_u64(details.cached_tokens));
    let cache_write = usage
        .prompt_tokens_details
        .as_ref()
        .map_or(0, |details| as_u64(details.cache_creation_tokens));
    Usage {
        input: total_input.saturating_sub(cache_read),
        cache_read,
        cache_write,
        output: as_u64(usage.completion_tokens),
        reasoning: usage
            .completion_tokens_details
            .as_ref()
            .map_or(0, |details| as_u64(details.reasoning_tokens)),
        raw: serde_json::to_value(usage).unwrap_or_default(),
    }
}

pub fn map_stop_reason(reason: &StopReason) -> FinishReason {
    match reason {
        StopReason::Completed(_) | StopReason::StopSequence(_) => FinishReason::Stop,
        StopReason::MaxTokens(_) => FinishReason::Length,
        StopReason::ToolCall(_) => FinishReason::ToolCalls,
        StopReason::ContentFilter(_) => FinishReason::ContentFilter,
        StopReason::Other(raw) => FinishReason::Other(raw.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadmus_contract::{ChatRequest, ToolCall};
    use serde_json::json;

    #[test]
    fn hoists_leading_system_messages() {
        let request = ChatRequest::user_text("hi", 100).with_messages(vec![
            Message::system("you are terse"),
            Message::system("answer in json"),
            Message::user("hi"),
        ]);
        let genai_request = build_genai_request(&request, false).expect("map");
        assert_eq!(
            genai_request.system.as_deref(),
            Some("you are terse\n\nanswer in json")
        );
        assert_eq!(genai_request.messages.len(), 1);
    }

    #[test]
    fn strips_reasoning_without_echo_obligation() {
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::Reasoning { text: "hmm".into() },
                ContentPart::Text {
                    text: "answer".into(),
                },
            ],
            tool_call_id: None,
            opaque: None,
        };
        let request = ChatRequest::user_text("q", 100).with_messages(vec![assistant]);
        let stripped = build_genai_request(&request, false).expect("map");
        let stripped_parts: Vec<_> = stripped.messages[0].content.clone().into_parts();
        assert_eq!(stripped_parts.len(), 1);
        assert!(matches!(&stripped_parts[0], GenaiContentPart::Text(text) if text == "answer"));

        let echoed = build_genai_request(&request, true).expect("map");
        let parts: Vec<_> = echoed.messages[0].content.clone().into_parts();
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], GenaiContentPart::ReasoningContent(text) if text == "hmm"));
    }

    #[test]
    fn maps_tool_calls_and_tool_results() {
        let history = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::ToolCall {
                    call: ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "/a"}),
                    },
                }],
                tool_call_id: None,
                opaque: None,
            },
            Message::tool_result("c1", json!("contents")),
        ];
        let request = ChatRequest::user_text("q", 100).with_messages(history);
        let genai_request = build_genai_request(&request, false).expect("map");
        let parts0: Vec<_> = genai_request.messages[0].content.clone().into_parts();
        assert!(
            matches!(&parts0[0], GenaiContentPart::ToolCall(call) if call.call_id == "c1" && call.fn_name == "read_file")
        );
        let parts1: Vec<_> = genai_request.messages[1].content.clone().into_parts();
        assert!(
            matches!(&parts1[0], GenaiContentPart::ToolResponse(response) if response.call_id == "c1" && response.content == "contents")
        );
    }

    #[test]
    fn normalizes_three_bucket_usage() {
        let genai_usage = genai::chat::Usage {
            prompt_tokens: Some(1_000),
            prompt_tokens_details: Some(genai::chat::PromptTokensDetails {
                cached_tokens: Some(400),
                cache_creation_tokens: Some(150),
                ..genai::chat::PromptTokensDetails::default()
            }),
            completion_tokens: Some(200),
            completion_tokens_details: Some(genai::chat::CompletionTokensDetails {
                reasoning_tokens: Some(50),
                ..genai::chat::CompletionTokensDetails::default()
            }),
            total_tokens: Some(1_200),
        };
        let usage = map_usage(&genai_usage);
        assert_eq!(usage.input, 600);
        assert_eq!(usage.cache_read, 400);
        assert_eq!(usage.cache_write, 150);
        assert_eq!(usage.output, 200);
        assert_eq!(usage.reasoning, 50);
        assert!(usage.raw.is_object());
    }
}
