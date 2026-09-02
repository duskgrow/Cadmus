//! One-line-per-dialect integration of the provider contract suite (report
//! §9.2.1): every dialect runs the identical port semantics against the local
//! replay stub — recorded-replay, never live calls. This is what keeps the
//! dialect seam honest (ADR-0003).

mod support;

mod kimi_k3 {
    use crate::support::wire::StubProvider;
    use cadmus_llm_openai::KimiDialect;

    cadmus_contract::provider_contract_tests!(|| StubProvider::new(Box::new(KimiDialect::k3())));
}

mod deepseek_v4_flash {
    use crate::support::wire::StubProvider;
    use cadmus_llm_openai::DeepSeekDialect;

    cadmus_contract::provider_contract_tests!(|| StubProvider::new(Box::new(
        DeepSeekDialect::v4_flash()
    )));
}

mod custom {
    use crate::support::wire::StubProvider;
    use cadmus_llm_openai::CustomDialect;

    // The stub doubles as the custom endpoint; its address is injected by
    // StubProvider, so any placeholder base URL works here.
    cadmus_contract::provider_contract_tests!(|| StubProvider::new(Box::new(CustomDialect::new(
        "gpt-5.2",
        "http://custom.invalid"
    ))));
}
