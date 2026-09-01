# 0003. Provider abstraction: OpenAI-compatible wire, dialect modules, ModelProfile data

- Status: accepted
- Date: 2026-09-02

## Context

The system must stay model-agnostic while serving several providers with
different "affinities": Kimi, MiMo, DeepSeek and GLM directly, plus
relay-station access to GPT and Claude. All expose OpenAI-compatible
endpoints, but they differ in wire quirks (tool-call incremental aggregation,
reasoning-field naming, structured-output support — report §4.2.1) and in
prompt affinity (system-prompt style, tool-description conventions,
per-vendor agent guides). Framework lock-in was rejected: Rig shipped a
breaking minor roughly every 2–4 weeks (verified at the report baseline,
§4.1.1), an ongoing adaptation tax a single maintainer cannot amortize.

## Decision

Self-built agent loop + thin client (rust-genai preferred, async-openai
`byot` fallback) + rmcp for the tool protocol (report §4.1.3).

Provider customization is split into two layers with different change
mechanisms:

1. **Wire dialect layer (code).** Per-provider modules inside the single
   `cadmus-llm-openai` adapter crate: endpoint configuration, tool-call delta
   aggregation quirks, reasoning-field naming, structured-output degradation
   path. A new provider means a new dialect module plus passing the shared
   provider contract test suite (report §9.2.1) — recorded-replay fakes, no
   live calls in CI.
2. **ModelProfile layer (versioned config data, not code).** Per-model prompt
   affinity: system-prompt preamble, tool-description style, few-shot format,
   cache hints. Profiles load at runtime and are versioned like skill data;
   from phase 2 on they are themselves candidates for the self-evolution
   loop, under the same gates as skill items (report §2.5).

Phase 0 connects **two** providers (one primary — Kimi or DeepSeek — plus a
relay-station GPT) to validate the dialect seam while redrawing it is still
cheap.

## Consequences

- Per-vendor prompt customization is never hardcoded in `cadmus-core`; the
  core sees only the minimal `Provider` trait plus a `Capabilities`
  declaration (report §4.2.2).
- Prompt-affinity tuning becomes data evolution with version history and
  rollback instead of code churn — and later an evolution target of the
  system itself.
- Exit cost if the thin client stalls or a dialect blocks the main path: 1–3
  person-days inside the adapter crate (report §11.1, row 1); the contract
  test suite keeps the seam honest.
- Re-verify at phase 0 start (report §1.2.2 freshness policy): current client
  versions (rust-genai / async-openai / rmcp) and each provider's tool-calling
  behavior.
