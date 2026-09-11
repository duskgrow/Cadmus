use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    /// Required when `role == Role::Tool`: the call this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Marks a tool result as an error outcome (`Role::Tool` only): the
    /// model reads it as a correction, not as data (ADR-0008 item 2).
    /// Dialects render the flag per wire capability.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    /// Vendor-opaque payload (`Gemini` thought signatures, `DeepSeek`
    /// `reasoning_content`, …). Never parsed here — adapters persist it and
    /// echo it back verbatim on the next request; dropping it is a wire 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Value>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_call_id: None,
            is_error: false,
            opaque: None,
        }
    }

    /// The answer to a completed tool call. String values pass through raw;
    /// structured payloads serialize to JSON.
    pub fn tool_result(call_id: impl Into<String>, content: Value) -> Self {
        let text = match content {
            Value::String(text) => text,
            other => other.to_string(),
        };
        Self {
            role: Role::Tool,
            content: vec![ContentPart::Text { text }],
            tool_call_id: Some(call_id.into()),
            is_error: false,
            opaque: None,
        }
    }

    /// The error answer to a completed tool call: same payload shaping as
    /// [`tool_result`](Self::tool_result), marked so the model reads it as
    /// a correction.
    pub fn tool_error(call_id: impl Into<String>, content: Value) -> Self {
        let mut message = Self::tool_result(call_id, content);
        message.is_error = true;
        message
    }

    /// All tool calls in this message, in wire order.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|part| match part {
            ContentPart::ToolCall { call } => Some(call),
            _ => None,
        })
    }

    /// The text parts joined with newlines — the message's human-readable
    /// body. Reasoning, tool-call, image and opaque parts are excluded.
    #[must_use]
    pub fn text_body(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// serde's skip_serializing_if requires the by-reference signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// Normalized reasoning body. Whether it is echoed back on the next
    /// request is the adapter's call, driven by `Capabilities::opaque_echo`
    /// (`DeepSeek` echoes it, other vendors must strip it).
    Reasoning {
        text: String,
    },
    ToolCall {
        call: ToolCall,
    },
    Image {
        bytes: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_marks_the_flag_and_shares_the_payload_shaping() {
        let error = Message::tool_error("c1", json!("boom"));
        assert!(error.is_error);
        assert_eq!(error.role, Role::Tool);
        assert_eq!(error.tool_call_id.as_deref(), Some("c1"));

        let ok = Message::tool_result("c1", json!("boom"));
        assert!(!ok.is_error);
        assert_eq!(ok.content, error.content);
    }

    #[test]
    fn is_error_stays_wire_compatible() {
        // Old payloads without the flag keep deserializing (default false),
        // and a false flag never serializes (skip), so persisted streams
        // are unchanged unless an error actually flows.
        let plain: Message = serde_json::from_str(
            r#"{"role":"tool","content":[{"type":"text","text":"x"}],"tool_call_id":"c1"}"#,
        )
        .expect("legacy payload");
        assert!(!plain.is_error);

        let serialized = serde_json::to_value(&plain).expect("serialize");
        assert!(serialized.get("is_error").is_none());

        let serialized = serde_json::to_value(Message::tool_error("c1", json!("x"))).unwrap();
        assert_eq!(serialized["is_error"], json!(true));
    }

    use serde_json::json;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Normalized: adapters mint an id when the wire format lacks one
    /// (pitfall #2), so two calls to the same tool never overwrite each other.
    pub id: String,
    pub name: String,
    /// Complete JSON. A truncated/incomplete call is quarantined inside the
    /// adapter and never reaches the domain layer (pitfall #3).
    pub arguments: Value,
}
