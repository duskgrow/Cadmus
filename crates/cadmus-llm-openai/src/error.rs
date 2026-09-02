//! genai error → contract [`ModelError`] classification. The classification
//! drives routing decisions (retry / escalate / fail), so it is deliberately
//! conservative: anything unrecognized degrades to `Protocol`/`InvalidRequest`
//! rather than masquerading as a transient error.

use cadmus_contract::ModelError;
use genai::Error as GenaiError;

pub fn map_genai_error(error: genai::Error) -> ModelError {
    match error {
        GenaiError::HttpError {
            status,
            canonical_reason: _,
            body,
        } => classify_http(status.as_u16(), &body),
        GenaiError::WebAdapterCall { webc_error, .. }
        | GenaiError::WebModelCall { webc_error, .. } => {
            ModelError::Network(webc_error.to_string())
        }
        // Stream transport failures wrap the underlying error boxed; an HTTP
        // error response (e.g. 429) arrives this way, so recover and classify
        // it instead of degrading it to a generic network error.
        GenaiError::WebStream { error, cause, .. } => error
            .downcast_ref::<GenaiError>()
            .map_or_else(|| ModelError::Network(cause), map_genai_error_ref),
        GenaiError::StreamParse { serde_error, .. } => {
            ModelError::Protocol(serde_error.to_string())
        }
        GenaiError::RequiresApiKey { .. }
        | GenaiError::NoAuthResolver { .. }
        | GenaiError::NoAuthData { .. } => ModelError::Auth(error.to_string()),
        // An error event inside the stream (provider JSON error mid-flight).
        GenaiError::ChatResponse { body, .. } => classify_stream_error(&body),
        other => ModelError::InvalidRequest(other.to_string()),
    }
}

fn map_genai_error_ref(error: &GenaiError) -> ModelError {
    match error {
        GenaiError::HttpError { status, body, .. } => classify_http(status.as_u16(), body),
        other => ModelError::Network(other.to_string()),
    }
}

fn classify_http(status: u16, body: &str) -> ModelError {
    match status {
        401 | 403 => ModelError::Auth(excerpt(body)),
        408 | 409 | 425 | 429 =>
        // genai's HttpError carries no headers, so `Retry-After` is lost here
        // (pitfall #11 — recorded gap; a custom reqwest layer recovers it).
        {
            ModelError::RateLimited { retry_after: None }
        }
        400 => {
            if body.contains("context_length") || body.contains("maximum context length") {
                ModelError::ContextLength
            } else {
                ModelError::InvalidRequest(excerpt(body))
            }
        }
        413 => ModelError::ContextLength,
        _ if status >= 500 => ModelError::Server {
            status,
            retriable: true,
        },
        _ => ModelError::Server {
            status,
            retriable: false,
        },
    }
}

fn classify_stream_error(body: &serde_json::Value) -> ModelError {
    let text = body.to_string();
    if text.contains("rate_limit") || text.contains("insufficient_quota") {
        ModelError::RateLimited { retry_after: None }
    } else {
        ModelError::Protocol(excerpt(&text))
    }
}

/// Bodies can be arbitrarily large; errors only ever need the head.
fn excerpt(body: &str) -> String {
    body.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_http_statuses() {
        assert!(matches!(
            classify_http(429, "slow down"),
            ModelError::RateLimited { retry_after: None }
        ));
        assert!(matches!(classify_http(401, "bad key"), ModelError::Auth(_)));
        assert!(matches!(
            classify_http(500, "boom"),
            ModelError::Server {
                status: 500,
                retriable: true
            }
        ));
        assert!(matches!(
            classify_http(400, "maximum context length exceeded"),
            ModelError::ContextLength
        ));
        assert!(matches!(
            classify_http(400, "bad json"),
            ModelError::InvalidRequest(_)
        ));
    }
}
