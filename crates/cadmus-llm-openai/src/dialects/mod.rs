mod custom;
mod deepseek;
mod kimi;

pub use custom::CustomDialect;
pub use deepseek::DeepSeekDialect;
pub use kimi::KimiDialect;

/// The vendor registry: provider name → dialect constructor. Adding a
/// provider means a new module plus one entry here — downstream crates never
/// change (SSOT for the provider-name surface).
#[must_use]
pub fn dialect_by_name(name: &str) -> Option<Box<dyn crate::Dialect>> {
    match name {
        "kimi" => Some(Box::new(KimiDialect::k3())),
        "deepseek" => Some(Box::new(DeepSeekDialect::v4_flash())),
        _ => None,
    }
}

/// Every valid provider name: registry vendors plus `custom` (an explicit
/// OpenAI-compatible endpoint via [`CustomDialect`]).
#[must_use]
pub fn dialect_names() -> &'static [&'static str] {
    &["kimi", "deepseek", "custom"]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dialect;

    // genai joins `chat/completions` onto the endpoint with an RFC-3986 URL
    // join: a base carrying a path without a trailing slash loses its last
    // segment (`/v1` on Kimi). Every dialect endpoint must therefore be
    // slash-terminated; `CustomDialect::new` normalizes user input.
    #[test]
    fn endpoints_are_slash_terminated_for_url_join() {
        let dialects: Vec<Box<dyn Dialect>> = vec![
            Box::new(KimiDialect::k3()),
            Box::new(DeepSeekDialect::v4_flash()),
            Box::new(CustomDialect::new("gpt-5.2", "http://localhost:8080/v1")),
        ];
        for dialect in dialects {
            let base = dialect.endpoint().base_url().to_string();
            assert!(
                base.ends_with('/'),
                "{} endpoint `{base}` must end with `/`",
                dialect.id()
            );
        }
    }

    #[test]
    fn registry_resolves_every_builtin_name() {
        for name in ["kimi", "deepseek"] {
            assert!(dialect_by_name(name).is_some(), "missing dialect: {name}");
        }
        assert!(
            dialect_by_name("custom").is_none(),
            "custom takes coordinates"
        );
        assert!(dialect_by_name("bogus").is_none());
        assert_eq!(dialect_names(), &["kimi", "deepseek", "custom"]);
    }
}
