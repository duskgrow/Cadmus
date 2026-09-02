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
            opaque: None,
        }
    }

    /// All tool calls in this message, in wire order.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|part| match part {
            ContentPart::ToolCall { call } => Some(call),
            _ => None,
        })
    }
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
