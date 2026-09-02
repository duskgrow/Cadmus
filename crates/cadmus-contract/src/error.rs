use std::time::Duration;

/// Error classification is an input to cascade routing (report §4.2.3), not
/// just a message for humans: which variant occurred decides whether to
/// retry, escalate to a bigger model, or fail the turn.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// Transparent `Retry-After` passthrough; a model swap may dodge the wait.
    #[error("rate limited{}", retry_after.map(|d| format!(" (retry after {d:?})")).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },
    /// 5xx and gateway failures; `retriable` distinguishes "try again" from
    /// "this request will never succeed".
    #[error("provider server error (HTTP {status})")]
    Server { status: u16, retriable: bool },
    #[error("network error: {0}")]
    Network(String),
    /// The wire did not conform to the OpenAI-compatible dialect — a strong
    /// escalation signal.
    #[error("wire protocol violation: {0}")]
    Protocol(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The request asked for something the model's [`crate::Capabilities`]
    /// declare unsupported — caught before the wire, never silently downgraded.
    #[error("capability mismatch: {0}")]
    CapabilityMismatch(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("context length exceeded")]
    ContextLength,
}
