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
            detail: detail(body),
        },
        _ => ModelError::Server {
            status,
            retriable: false,
            detail: detail(body),
        },
    }
}

/// The provider's own reason for a failed request: the `error.message` of
/// the JSON error body when there is one (the OpenAI-compatible error
/// shape), else a bounded raw excerpt — gateways answer HTML. `None` for an
/// empty body (the status line then says everything there is to say).
fn detail(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(|message| message.as_str().map(str::to_string))
        });
    Some(parsed.map_or_else(|| excerpt(body), |message| excerpt(&message)))
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
                retriable: true,
                ..
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

    #[test]
    fn server_errors_carry_the_providers_reason() {
        // The OpenAI-compatible error shape: the message field wins.
        let body =
            r#"{"error":{"message":"Service is too busy.","type":"service_unavailable_error"}}"#;
        let ModelError::Server { detail, .. } = classify_http(503, body) else {
            panic!("a 503 classifies as Server")
        };
        assert_eq!(detail.as_deref(), Some("Service is too busy."));
        assert_eq!(
            classify_http(503, body).to_string(),
            "provider server error (HTTP 503): Service is too busy."
        );
        // A gateway's HTML error page degrades to the bounded raw excerpt.
        let ModelError::Server { detail, .. } = classify_http(502, "<html>Bad Gateway</html>")
        else {
            panic!("a 502 classifies as Server")
        };
        assert_eq!(detail.as_deref(), Some("<html>Bad Gateway</html>"));
        // An empty body leaves the status line to speak for itself.
        let ModelError::Server { detail, .. } = classify_http(500, "") else {
            panic!("a 500 classifies as Server")
        };
        assert_eq!(detail, None);
        assert_eq!(
            classify_http(500, "").to_string(),
            "provider server error (HTTP 500)"
        );
    }
}
