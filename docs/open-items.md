# Open items

Field findings waiting for their consuming change — an inbox, not a
backlog. Every item names its consumer; when the consuming change lands,
delete the item (its rationale then lives in that change's ADR). An item
with no consumer does not belong here.

## Interaction surfaces never render logs

Consumer: the ADR-0011 TUI implementation, over the ADR-0013 live stream.

First live `chat` run (2026-09-07): at the default `warn` filter the user
saw only genai's `EMPTY CHOICE CONTENT` spam and no run progress, and the
bare final answer did not read as addressed to them. Requirement from the
field: the interaction surface renders structured progress (turn blocks,
tool activity); logs go to a file or an opt-in verbose channel, never into
the interaction view.

## The agent loop has no tracing instrumentation

Consumer: same as above, or a standalone interim change.

`cadmus-core`'s agent loop emits nothing at any level: `RUST_LOG=info`
gives per-case progress for `eval` but stays silent for `chat`. If wanted
before the TUI lands, info-level instrumentation (turn start, tool-call
name, finish status) is a small standalone change. The TUI's structured
progress carrier is ADR-0013's live stream; interim tracing stays useful
for `eval` and pre-TUI `chat`.

## Traces carry no workspace or ruler identity

Consumer: phase 2's reflector input selection.

Run attributes record provider/model/version/eval_split only (ADR-0005 §3),
so all projects' traces mix in one date-organized pool and the workspace
can only be reverse-engineered from tool arguments. Eval traces likewise
can't be grouped by ruler without the score files. When the reflector
lands, add `selfevol.workspace` and the corpus digest to start_run
(additive attrs, ADR-0005 amendment).

## The OOD probe is policy, not yet a mechanism

Consumer: the phase-2 gate ADR.

Report §11.2 mechanism 2 (periodic OOD probes: 10–20 fresh real-usage
samples, human-curated into the set) has no cadence, sampling or admission
procedure — a policy sentence without a mechanism, the failure mode
ADR-0010's context calls out for holdout isolation. The phase-2 ADR should
pin all three.
