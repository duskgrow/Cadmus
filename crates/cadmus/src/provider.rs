//! Building the wired provider from CLI coordinates — shared by `chat` and
//! `eval` so both run the identical model+harness composite (ADR-0010 §2).

use std::collections::BTreeMap;

use cadmus_llm_openai::{CustomDialect, Dialect, OpenAiProvider, dialect_by_name};
use serde_json::Value;

use crate::Error;

/// Resolves the dialect, fails fast on a missing key (instead of a wire
/// 401), and returns the provider plus the model name as sent on the wire
/// (trajectory provenance).
pub fn build(
    provider: &str,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Result<(OpenAiProvider, String), Error> {
    let dialect = build_dialect(provider, model, base_url)?;
    let wire_model = dialect.model_name().to_string();
    if std::env::var(dialect.api_key_env()).is_err() {
        return Err(Error::MissingApiKey {
            env: dialect.api_key_env(),
        });
    }
    let provider = OpenAiProvider::from_env(dialect).map_err(Error::Provider)?;
    Ok((provider, wire_model))
}

fn build_dialect(
    provider: &str,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Result<Box<dyn Dialect>, Error> {
    if provider == "custom" {
        let (Some(model), Some(base_url)) = (model, base_url) else {
            return Err(Error::CustomConfigMissing);
        };
        return Ok(Box::new(CustomDialect::new(model, base_url)));
    }
    dialect_by_name(provider).ok_or_else(|| Error::UnknownProvider(provider.to_string()))
}

/// Run-level provenance recorded on the start-run event (ADR-0005 §3): the
/// wired provider, the model name as sent on the wire, and the binary
/// version — the attributes every later projection groups by.
pub(crate) fn run_attributes(provider: &str, wire_model: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            cadmus_contract::attrs::PROVIDER.to_string(),
            provider.to_string().into(),
        ),
        (
            cadmus_contract::attrs::MODEL.to_string(),
            wire_model.to_string().into(),
        ),
        (
            cadmus_contract::attrs::CADMUS_VERSION.to_string(),
            env!("CARGO_PKG_VERSION").into(),
        ),
    ])
}
