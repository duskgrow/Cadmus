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

## read_file's line axis has no column resume

Consumer: a read_file byte-window parameter, if evals or traces ever show
models needing it.

Lines over 64 KiB are cut inline with a marker and their tails are
unreachable; grep's match preview likewise shows only the 512 B head. Both
deliberate — such lines are machine-generated (minified bundles, source
maps, serialized records) and paged raw into context they are a net
negative. If evidence ever justifies it, the additive extension is a
byte-window parameter; do not build it ahead of evidence.

## ACP adoption seam assessment

Consumer: the ACP adoption ADR, whenever an ACP frontend is scheduled.

Assessed 2026-09-09 against ADR-0013: an ACP frontend is another client
kind of the client protocol, and the hard parts already align — trajectory
events map to session/update tool-call notifications, the approval path is
a command event shared by local and remote clients (ADR-0008 item 4), and
the tool-result is_error flag maps to ACP's failed status. The one real
seam: the built-in tools do their own filesystem IO, so ACP's client-side
fs capabilities (remote workspaces, editor-native diffs) would need an IO
port injected into the tools — additive behind `AgentTool`, aligned with
the constructor-injection style rule, not a redesign.
