# 0010. Evolution gate discipline: paired statistics, mechanical isolation, lifecycle

- Status: accepted
- Date: 2026-09-06

## Context

Phase 2's acceptance is "the first skill item with positive gain on eval set
v1", search/test separated (report §10.2.3). Three soft spots were known at
writing or found in the 2026-09 design review: (i) statistical power at
n≈50 — the binomial standard error peaks near 7.1pp at p=0.5, so a 95%
confidence interval is about ±14pp and small deltas are indistinguishable
from noise (report tracks this as open question O2); (ii) live-API drift —
models change under us, so comparing a candidate against historical parent
scores aims at a moving target; (iii) holdout non-reflux (report §11.2) is a
policy sentence without a mechanism, while score events flow into the same
trace log the reflector reads (ADR-0005 item 7).

Evidence base beyond the frozen report: ai-agent-book v2.0 (fetched
2026-09-06), chapter 7 (paired comparison with per-item deltas and
McNemar/paired bootstrap; 3–5 seeds per configuration with alternating run
order; single runs screen direction only; expand the sample when expected
gain is below the noise bandwidth; tighten thresholds under multiple
comparisons; the evaluation object is the model+harness composite — a model
swap is itself an experiment, not a silent upgrade) and chapter 9 (the
two-sided gate: boundary set must improve _and_ retention set must not
regress _and_ transfer set validates generalization; gate metrics beyond
pass rate — activation and adherence — so "skill written but not loaded or
not followed" is not misread as evolution failure; extractor/verifier
independence; refusal when evidence is insufficient; validators, holdout and
release gates are outside the self-modifiable space; lifecycle management
beyond counters — usage/staleness/archive states, deterministic idle
pruning, snapshot-before-reorganize, rollback for mistaken curation).

## Decision

1. **Paired comparison is the only accepted gate signal.** Parent and
   candidate run on identical tasks with matched seeds; per-item deltas are
   aggregated via McNemar or paired bootstrap. 3–5 seeds per configuration,
   alternating run order to cancel time-direction drift. A single run
   screens direction only and never gates. Bare "not worse than parent" is
   replaced by the two-sided form in item 3.
2. **The evaluation object is the model+harness composite.** Model id is
   already in run attributes (ADR-0005); longitudinal comparisons are valid
   only within one pinned model version, and any model change — including a
   detectable provider-side upgrade — is itself a candidate change that goes
   through the paired gate. Prompt builds are pinned the same way via
   ADR-0007's prefix hash.
3. **Two-sided gate with three split sets.** The boundary set (failure
   triggers the delta claims to fix) must improve; the retention set must
   not regress; the transfer set (tasks not used during extraction)
   validates generalization. Gate metrics add skill activation rate and
   adherence rate alongside pass rate.
4. **Isolation is mechanical, not policy.** Score events carry a
   `selfevol.eval_split` attribute; the reflector's trace selection excludes
   holdout splits by construction, with a CI test that a holdout-marked
   trace can never enter a reflection input. Validators, the holdout set,
   release gates and audit logs sit outside the agent's writable space and
   are non-self-modifiable. Low-confidence verdicts are refused and excluded
   from the learning set rather than forced.
5. **Sample-size honesty.** When the expected gain is below the noise
   bandwidth, the eval set grows first (SE scales as 1/√n) instead of
   relaxing the criterion; repeated delta proposals against the same search
   set tighten the significance threshold (multiple comparisons); every
   accepted positive is independently re-run once before commit.
6. **Lifecycle beyond counters.** Skill and memory entries carry
   usage/staleness/archive states with deterministic idle-based archiving
   (never deletion) — a
   snapshot before every reorganization, and rollback for mistaken curation;
   decay/archive decisions are themselves gate-visible events.

## Consequences

- Binding on phase 2's acceptance criteria: the roadmap row references this
  ADR at phase-2 kickoff, and the "concrete pp threshold" the report defers
  to baseline measurement is expressed as a paired non-inferiority
  statistic, not a difference of independent pass rates.
- O2 is reframed: pairing is the sensitivity lever at small n; enlarging the
  eval set is second. The open question survives (combinations of guardrails
  at personal scale remain unproven) but its minimal experiment now has a
  defined statistical protocol.
- Gate cost is bounded by configuration (order: 50 tasks × 3–5 seeds × 2
  configurations ≈ 300–500 paired runs per gate decision) and stays an
  occasional cost behind the shouldRefine pre-gate (report
  §11.2 mechanism 5).
- The book's program/harness evolution pathway (compiling stable experience
  into deterministic workflows/validators) is acknowledged and deferred: it
  requires before/after state checks plus independent replay verification,
  and is candidate material for a phase-2+ ADR — together with optimizer
  self-evolution, which stays excluded (report §2.5.1, meta-cognition row).
