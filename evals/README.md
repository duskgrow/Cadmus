# Eval set v1

The root trust anchor of the evolution loop (report §2, ADR-0005 §7): ≥50
versioned scenario cases, run as-is against a live provider. Evolution
artifacts never enter the set; `holdout` cases measure the model+harness
composite only at gate time, and their traces are excluded from reflection by
construction (ADR-0010 §4 — every eval run's start-run command and every
score event carries the `selfevol.eval_split` attribute).

## Layout

- `cases/<case-id>.json` — one file per case; the schema is `EvalCase` in
  `cadmus-contract` (prompt, split, fixture, expectations).
- `fixtures/<name>/` — synthetic workspaces. Each run copies the fixture to a
  scratch dir, so fixtures stay pristine and later edit-capable cases
  (ADR-0008) get a disposable workspace.

## Run

`just eval` runs the full set against the live provider (default `kimi`) and
writes `target/eval/latest.json`. A failed case scores 0 and never aborts the
set — the score file is the output, and per-run scores also land in that
run's trajectory log. A full live run is minutes of API time; per-case
progress logs at info level (`RUST_LOG=info just eval`).

CI never calls a live provider: the corpus is validated mechanically by the
`eval_corpus_is_well_formed` test in `crates/cadmus/tests/eval.rs`, and the
harness is tested end-to-end with scripted providers there.

## Authoring cases

- Needles are short verbatim tokens the fixture unambiguously supports (a
  path, a value, a name) — never full sentences.
- The prompt pins the answer format ("Answer with the number only.").
- The `note` field records where the answer lives, so a later fixture edit
  can re-derive expectations.
- Holdout cases measure the same skills as search cases but stay unseen by
  the reflector; keep the split ratio near 4:1.
