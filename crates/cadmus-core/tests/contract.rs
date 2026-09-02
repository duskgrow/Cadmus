//! One-line integration of the provider contract suite (report §9.2.1) for
//! the recorded-replay fake — run against the default capability declaration
//! and a tools-less profile to exercise both branches of the capability case.

mod default_capabilities {
    cadmus_contract::provider_contract_tests!(|| cadmus_core::ReplayProvider::new(Vec::new()));
}

mod without_tool_support {
    use cadmus_contract::{CacheSupport, Capabilities, SoSupport, Support};
    use cadmus_core::ReplayProvider;

    fn no_tools() -> Capabilities {
        Capabilities {
            tools: false,
            parallel_tools: Support::No,
            structured_output: SoSupport::PromptOnly,
            reasoning: None,
            prompt_cache: CacheSupport::None,
            logprobs: false,
            max_context: 8_000,
            max_output: 1_000,
            opaque_echo: vec![],
        }
    }

    cadmus_contract::provider_contract_tests!(
        || ReplayProvider::new(Vec::new()).with_capabilities(no_tools())
    );
}
