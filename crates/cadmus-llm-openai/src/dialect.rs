use cadmus_contract::{
    Capabilities, ChatRequest, EffortLevel, ModelError, OutputMode, Reasoning, SoSupport,
};
use genai::chat::{ChatOptions, ChatResponseFormat, JsonSpec, ReasoningEffort};
use genai::resolver::{AuthData, Endpoint};
use serde_json::{Map, Value};

/// One dialect = one (vendor, model) wire personality: endpoint
/// configuration, delta-aggregation quirks, reasoning-field naming and the
/// structured-output degradation path. This is the *code* layer of ADR-0003
/// — prompt affinity lives in `ModelProfile` data instead.
pub trait Dialect: Send + Sync {
    /// Stable id for logs and errors, e.g. `kimi-k3`.
    fn id(&self) -> &str;

    fn endpoint(&self) -> Endpoint;

    /// The environment variable the API key is read from. Keys only ever
    /// travel through the environment — never code, never config files.
    fn api_key_env(&self) -> &'static str;

    fn model_name(&self) -> &str;

    /// The static registry entry for this (vendor, model). Config overrides
    /// and runtime probing are the later two levels of the capability
    /// resolution stack; phase 0 ships the registry only.
    fn capabilities(&self) -> Capabilities;

    /// Reasoning echo obligation: whether `Reasoning` parts of past assistant
    /// messages go back into the request (`DeepSeek` thinking, else a wire 400)
    /// or get stripped.
    fn echo_reasoning(&self) -> bool {
        false
    }

    /// Vendor-private extra request body merged at the top level (e.g.
    /// `DeepSeek` `{"thinking": {"type": "enabled"}}`). `None` by default.
    fn extra_body(&self, _reasoning: &Reasoning) -> Option<Value> {
        None
    }

    /// Maps the requested reasoning tier onto this model's capabilities.
    /// Clamping is reported via `tracing::warn!`; unsupported modes fail fast.
    fn map_reasoning(&self, reasoning: &Reasoning) -> Result<Option<ReasoningEffort>, ModelError> {
        default_map_reasoning(&self.capabilities(), reasoning)
    }

    /// Vendor sampling ranges differ (Moonshot clamps temperature to
    /// `[0, 1]`); clamping is reported via `tracing::warn!`.
    fn clamp_temperature(&self, temperature: f32) -> f32 {
        temperature
    }

    /// Walks the structured-output degradation ladder by capability. The
    /// ladder's bottom step (prompt-injected schema + validation retry) is a
    /// core-side concern; a vendor that only reaches it is a mismatch here.
    fn map_output(&self, output: &OutputMode) -> Result<Option<ChatResponseFormat>, ModelError> {
        default_map_output(&self.capabilities(), output)
    }

    /// Composes this dialect's request options. Vendors with unusual needs
    /// override pieces, not the whole method.
    fn build_options(&self, request: &ChatRequest) -> Result<ChatOptions, ModelError> {
        let capabilities = self.capabilities();
        if !request.tools.is_empty() && !capabilities.tools {
            return Err(ModelError::CapabilityMismatch(format!(
                "{} does not support tool calling",
                self.id()
            )));
        }

        let mut options = ChatOptions {
            // Usage at stream end (`OpenAI` `stream_options.include_usage`) and
            // index-consistent accumulated tool calls — our own assembler in
            // cadmus-core owns the aggregation semantics on top.
            capture_usage: Some(true),
            capture_tool_calls: Some(true),
            max_tokens: Some(request.max_output_tokens),
            top_p: request.sampling.top_p.map(f64::from),
            seed: request.sampling.seed,
            tool_choice: match &request.tool_choice {
                cadmus_contract::ToolChoice::Auto => None,
                cadmus_contract::ToolChoice::None => Some(genai::chat::ToolChoice::None),
                cadmus_contract::ToolChoice::Required => Some(genai::chat::ToolChoice::Required),
                cadmus_contract::ToolChoice::Named(name) => {
                    Some(genai::chat::ToolChoice::Tool { name: name.clone() })
                }
            },
            reasoning_effort: self.map_reasoning(&request.reasoning)?,
            response_format: self.map_output(&request.output)?,
            ..ChatOptions::default()
        };

        if let Some(temperature) = request.sampling.temperature {
            let clamped = self.clamp_temperature(temperature);
            if (clamped - temperature).abs() > f32::EPSILON {
                tracing::warn!(
                    requested = temperature,
                    applied = clamped,
                    "temperature clamped to vendor range"
                );
            }
            options.temperature = Some(f64::from(clamped));
        }

        if let Some(parallel) = request.parallel_tools {
            match capabilities.parallel_tools {
                cadmus_contract::Support::No => {
                    return Err(ModelError::CapabilityMismatch(format!(
                        "{} does not support parallel tool calls",
                        self.id()
                    )));
                }
                cadmus_contract::Support::Yes | cadmus_contract::Support::AllowedNotGuaranteed => {
                    merge_extra(&mut options, "parallel_tool_calls", Value::Bool(parallel));
                }
            }
        }

        match &request.cache {
            cadmus_contract::CacheDirective::None | cadmus_contract::CacheDirective::Auto => {}
            cadmus_contract::CacheDirective::Breakpoints(_) => {
                return Err(ModelError::CapabilityMismatch(format!(
                    "{} has no explicit cache breakpoints (cache mode: {:?})",
                    self.id(),
                    capabilities.prompt_cache
                )));
            }
        }

        options.extra_body = self.extra_body(&request.reasoning);

        // The escape hatch: unknown vendor fields are merged but loudly
        // announced — passing through is a deliberate act, never a silent
        // swallow (the new-api counter-example).
        if let Some(extra) = request.extra.as_object()
            && !extra.is_empty()
        {
            tracing::warn!(keys = ?extra.keys().collect::<Vec<_>>(), "passing through vendor-private request fields");
            for (key, value) in extra {
                merge_extra(&mut options, key, value.clone());
            }
        }

        Ok(options)
    }
}

/// Merges one key into `ChatOptions::extra_body`.
fn merge_extra(options: &mut ChatOptions, key: &str, value: Value) {
    options
        .extra_body
        .get_or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("extra_body is an object")
        .insert(key.to_string(), value);
}

/// The default reasoning-tier mapping, driven by the capability declaration:
/// exact tier if supported, otherwise clamp to the nearest (lower on ties).
pub fn default_map_reasoning(
    capabilities: &Capabilities,
    reasoning: &Reasoning,
) -> Result<Option<ReasoningEffort>, ModelError> {
    match reasoning {
        Reasoning::Adaptive => Ok(None),
        Reasoning::Off => match &capabilities.reasoning {
            Some(caps) if caps.off_capable => Ok(Some(ReasoningEffort::None)),
            _ => Err(ModelError::CapabilityMismatch(
                "reasoning cannot be turned off on this model".into(),
            )),
        },
        Reasoning::Effort(level) => {
            let caps = capabilities
                .reasoning
                .as_ref()
                .ok_or_else(|| ModelError::CapabilityMismatch("model has no reasoning".into()))?;
            if caps.efforts.contains(level) {
                return Ok(Some(map_level(*level)));
            }
            let nearest = caps
                .efforts
                .iter()
                .copied()
                .min_by_key(|candidate| {
                    let distance = effort_rank(*candidate).abs_diff(effort_rank(*level));
                    (distance, effort_rank(*candidate))
                })
                .ok_or_else(|| {
                    ModelError::CapabilityMismatch("model declares no reasoning tiers".into())
                })?;
            tracing::warn!(requested = ?level, applied = ?nearest, "reasoning effort clamped to nearest supported tier");
            Ok(Some(map_level(nearest)))
        }
        Reasoning::Budget(tokens) => match &capabilities.reasoning {
            Some(caps) if caps.budget_capable => Ok(Some(ReasoningEffort::Budget(*tokens))),
            _ => Err(ModelError::CapabilityMismatch(
                "model does not support token-budget reasoning".into(),
            )),
        },
    }
}

fn effort_rank(level: EffortLevel) -> u8 {
    match level {
        EffortLevel::Low => 0,
        EffortLevel::Medium => 1,
        EffortLevel::High => 2,
        EffortLevel::Max => 3,
    }
}

fn map_level(level: EffortLevel) -> ReasoningEffort {
    match level {
        EffortLevel::Low => ReasoningEffort::Low,
        EffortLevel::Medium => ReasoningEffort::Medium,
        EffortLevel::High => ReasoningEffort::High,
        EffortLevel::Max => ReasoningEffort::Max,
    }
}

/// The default structured-output ladder. Portable schemas are the
/// intersection of the vendors' subsets (flat objects, all-required,
/// nullable-instead-of-optional, enums, no numeric bounds, no root unions);
/// validating that intersection is left to the caller for now.
pub fn default_map_output(
    capabilities: &Capabilities,
    output: &OutputMode,
) -> Result<Option<ChatResponseFormat>, ModelError> {
    match output {
        OutputMode::Text => Ok(None),
        OutputMode::Json => match capabilities.structured_output {
            SoSupport::NativeStrict | SoSupport::NativeLoose | SoSupport::JsonMode => {
                Ok(Some(ChatResponseFormat::JsonMode))
            }
            SoSupport::PromptOnly => Err(ModelError::CapabilityMismatch(
                "json mode unsupported (prompt-only vendor)".into(),
            )),
        },
        OutputMode::Schema { schema, strict } => {
            if *strict {
                match capabilities.structured_output {
                    SoSupport::NativeStrict => {
                        let spec = JsonSpec::new("response", schema.clone());
                        Ok(Some(ChatResponseFormat::JsonSpec(JsonSpec {
                            schema: spec.schema_with_additional_properties_false(),
                            ..spec
                        })))
                    }
                    _ => Err(ModelError::CapabilityMismatch(
                        "native strict structured output unsupported".into(),
                    )),
                }
            } else {
                match capabilities.structured_output {
                    SoSupport::NativeStrict | SoSupport::NativeLoose => Ok(Some(
                        ChatResponseFormat::JsonSpec(JsonSpec::new("response", schema.clone())),
                    )),
                    SoSupport::JsonMode => Ok(Some(ChatResponseFormat::JsonMode)),
                    SoSupport::PromptOnly => Err(ModelError::CapabilityMismatch(
                        "structured output unsupported (prompt-only vendor)".into(),
                    )),
                }
            }
        }
    }
}

/// Reads the API key from the dialect's environment variable.
pub fn api_key_from_env(dialect: &dyn Dialect) -> Result<AuthData, ModelError> {
    std::env::var(dialect.api_key_env())
        .map(AuthData::from_single)
        .map_err(|_| {
            ModelError::Auth(format!(
                "environment variable {} is not set",
                dialect.api_key_env()
            ))
        })
}
