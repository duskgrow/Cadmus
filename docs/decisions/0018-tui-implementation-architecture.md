# 0018. TUI implementation: ratatui stack, semantic-IR pipelines, modal-capable input layer, TOML config zone

- Status: accepted
- Date: 2026-09-13

## Context

The daily-driver milestone (ADR-0011/0008/0009) needs the TUI. The
interaction floor and philosophy are fixed (ADR-0011 floor v2,
ADR-0012); the client protocol is fixed (ADR-0013); the design system is
fixed (ADR-0017). What remained open was implementation-level: framework
and dependency admission, the inline-rendering mechanism, the streaming
markdown pipeline, the event loop, the input layer, the config file
formats, and the testing stack.

Research exhibits (frozen, `docs/research/`):
`2026-09-13-codex-tui-source-study.md` (mechanism blueprint from the
closest field precedent — Rust + ratatui, detachable client), and
`2026-09-13-tui-crate-selection.md` (same-day crates.io verification
against our constraints: MSRV 1.88, crates.io-only, the deny.toml
license allow-list).

Maintainer directives of 2026-09-13 recorded here: GUI is the end-state
primary interface, so content pipelines must be renderer-agnostic; two
candidate requirements were examined and found pseudo in their maximal
forms (items 6 and 7 record the verdicts and what survives); helix-style
modal interaction is wanted as a _capability the input layer must not
preclude_, not as a day-one feature.

## Decision

1. **Framework: ratatui 0.30 + crossterm 0.29 from crates.io, no
   forks.** Codex maintains a crossterm fork for
   `discard_buffered_input`; we self-implement the equivalent with a
   `poll(0)`+`read` drain loop (their own Unix fallback does this). The
   dependency admission record, incl. the maintainer-signed staleness
   exceptions (crossterm, syntect, fuzzy-matcher, vt100 — "finished, not
   abandoned" or actively-committed despite slow release lines):

   | crate                                | version        | role                            | notes                                                                 |
   | ------------------------------------ | -------------- | ------------------------------- | --------------------------------------------------------------------- |
   | ratatui                              | 0.30.2         | render framework                | MIT; MSRV exactly 1.88 — zero headroom, toolchain bumps must check it |
   | crossterm                            | 0.29.0         | terminal IO + `event-stream`    | MIT; slow release line, active repo                                   |
   | pulldown-cmark                       | 0.13.4         | markdown parser                 | MIT; full reparse per chunk, `offset_iter` as later optimization      |
   | syntect                              | 5.3.0          | syntax highlighting             | MIT; MSRV undeclared — verify by compiling                            |
   | two-face                             | 0.5.2          | bat syntax/theme corpus         | MIT/Apache-2.0; `syntect-fancy` engine, no C toolchain                |
   | fuzzy-matcher                        | 0.3.7          | picker scoring                  | MIT; 1.5k-line pure algorithm                                         |
   | unicode-width / unicode-segmentation | 0.2.2 / 1.13.3 | display width / graphemes       | MIT/Apache-2.0                                                        |
   | toml                                 | 1.1.6          | human config files (item 7)     | whitelist-clean per the token exhibit                                 |
   | vt100 + insta                        | dev-only       | virtual-terminal snapshot tests | vt100 stale 14 mo, never distributed                                  |

   Rejected: tui-textarea (22 months stale), reedline (MSRV 1.95, wrong
   shape), nucleo-matcher (MPL-2.0, off allow-list), diffy (`similar`'s
   `inline` feature covers word-level diff highlighting), `notify`
   (CC0-1.0), starlark/rhai/nickel (item 7). `textwrap` is deferred:
   ratatui's built-in wrap goes first; admission requires field evidence
   it is insufficient.

2. **Crate topology and the one-pipeline-two-renderers seam.**
   `cadmus-ui` (ADR-0017's crate) also owns the content pipelines —
   streaming markdown, syntax highlighting, diff modeling — whose output
   is a **semantic-style IR** (spans tagged with ADR-0017's slot roles),
   never ratatui types. `cadmus-tui` maps the IR onto ratatui
   `Line`s; the future GUI maps it onto its rich-text equivalent. The
   hard problems (incremental parse, highlight state, diff computation)
   are solved once, in the shared crate.
3. **Inline rendering: spike the stock viewport first, fork thin if it
   fails.** Codex forks ratatui's `Terminal` (~1.4k lines) because
   `Viewport::Inline` predates the combination of dynamic height,
   escape-sequence history writes, and per-terminal scroll strategies.
   ratatui 0.30's `Viewport::Inline` + `Terminal::insert_before` may now
   suffice; a timeboxed spike (acceptance: streaming while history
   inserts above, resize reflow at narrow widths, Zellij and Windows
   Terminal quirks) decides. If it fails, the fallback is Codex's
   blueprint: a thin derived `Terminal` (MIT attribution preserved),
   history written to scrollback via DEC scroll-region escape sequences,
   terminal quirks corralled in a `ScrollbackStrategy` enum. Either way:
   completed turns leave the viewport into real scrollback; resize
   reflows re-materialize from the event stream — our SSOT is stronger
   than Codex's in-memory cells, with their invariant kept (a reflow
   requested mid-stream repeats once the stream is source-backed).
4. **Streaming markdown: source is SSOT, rendering is derived.** The
   pipeline is Codex's three conservative layers, reimplemented against
   the IR: a newline-gated collector (incomplete trailing lines never
   render); incremental rendering at top-level block boundaries (stable
   prefix renders once, only the tail block re-renders); an open-fence
   fast path continuing syntect's `HighlightState`/`ParseState` per
   complete line, falling back to the full path on any maybe-closing
   line. Tables hold back from the header until the stream settles and
   transpose to key/value records when too narrow. Item completion is
   authoritative over the delta stream at finalize, so a saturated
   transport cannot truncate the transcript. Flush is continuous,
   never one batch at turn end (Codex's stable/tail two-region model,
   source-verified in the 2026-09-13 exhibit): a long turn's head must
   be readable above the band while its tail still streams — the
   inline spike's flush-on-complete is a harness simplification, not
   the design. The flush unit is the rendered line whose _shape_ (row
   count, wrap) no future source line can change — not the closed
   block. In a terminal this is nearly free: no font metrics, so the
   renderer keeps every reclassifiable variant shape-identical (a
   heading renders with the same rows and wraps as its paragraph form;
   lists use uniform spacing, ignoring tight/loose), and the
   reclassifications that remain are zero-width inline styling
   (emphasis spanning lines, reference-style links) — an accepted
   cost class on frozen rows: rare, cosmetic, self-healed by the next
   resize reflow (the stock path cannot delete its own scrollback).
   Line-flushable therefore: paragraphs, completed list items (the
   open item can still lazy-continue, so the item is the unit), fence
   and indented-code bodies (literal). The one genuine holdout is
   tables — column widths depend on all rows — held until settled,
   the narrow-table record transposition doubling as their streaming
   presentation. Codex's own flush granularity is not pinned in the
   exhibit; this invariant is derived from CommonMark semantics and
   the vt100 suite locks each channel at implementation. A held
   block's unread head stays reachable live through the from-source
   transcript fallback (open item).
5. **Event loop: actors, capped frame rate, input never blocks.** A
   FrameRequester/FrameScheduler actor pair coalesces redraw requests
   and caps at 120 FPS — the Codex precedent, source-verified in the
   2026-09-13 exhibit (maintainer directive 2026-09-14 overrides that
   exhibit's start-at-60 suggestion). The cap is a ceiling on
   demand-driven redraws, not a tick: idle draws nothing. Terminals
   expose no display-refresh query, and frames past the emulator's own
   composite rate only burn CPU in the cell diff, so screen-Hz
   adaptation is neither possible nor missed; the GUI in any case runs
   its own vsync-driven loop and shares only the item-2 pipelines.
   Frames render inside a synchronized-update
   (2026h) guard. Stream draining follows a two-regime hysteresis policy
   (smooth typewriter vs catch-up flush) expressed as a pure function
   over unmaterialized-event count and age — the materialized-view-
   catching-up-with-the-log reading of Codex's queue-depth policy. The
   crossterm event stream is owned by a broker that can drop/recreate
   it, so `$EDITOR` handoff (Ctrl-G) never fights over stdin. Keyboard
   handling is synchronous in-memory work on every path.
6. **Input layer: mode × key → named command, keymap as data.**
   ADR-0012's live-keymap hint bar already requires the keymap to be
   data; the input layer is therefore a mode-scoped state machine
   dispatching to a command registry whose names double as the command
   palette's vocabulary and the hint bar's source. This mechanism keeps
   the helix-style modal door open at near-zero cost. The **default
   keymap content** (helix-flavored selection-first grammar; hjkl/q/?//
   conventions; the three steering granularities' bindings) is decided
   in the binding-design task at implementation, per ADR-0011's
   amendment (the field's Enter/Tab split is unconverged). The composer
   is self-built: multiline, grapheme-correct cursor and word ops,
   snapshot undo (bounded: 64 steps / 1 MB), bracketed-paste plus a
   paste-burst heuristic for terminals without it, Ctrl-G out to
   `$EDITOR`. **Pseudo-requirement verdicts (maintainer discussion
   2026-09-13):** a global modal UI (modal pickers, operators over
   blocks) and editor core features (registers, macros, marks) have no
   consumer — the agent edits files, the human writes prompts, heavy
   editing exits to `$EDITOR`; composer vim mode carries a trigger (the
   maintainer's own first pain editing a long prompt). None are built
   ahead of evidence; the state machine keeps all of them possible.
7. **Config: TOML for human files, JSON for machine boundaries;
   Starlark deferred to its consumer.** Settings, keymap and theme files
   are TOML (comments and forgiving syntax for hand-maintained files;
   helix/alacritty/Codex precedent); the NDJSON event stream and socket
   protocol stay JSON. No JSONC/YAML/KDL. The programmable-config
   platform (neovim-style user scripting) is a pseudo-requirement at
   single-user scale — its would-be consumers are already served by the
   composition layer (`chat --json`, the future socket) — and ADR-0011
   already rejects plugin ecosystems; the surviving real need, computed
   approval conditions and hooks, is Starlark's trigger (same-domain
   precedent: Codex execpolicy; candidates verified 2026-09-13:
   starlark 0.14.2 Apache-2.0, rhai 1.26.1, nickel-lang-core rejected on
   MSRV). Nothing to migrate: data-shaped config stays TOML; the
   expression zone, when triggered, is new files.
8. **Approvals and diff UI.** Scoped rules land data-shaped first
   (match over tool input via globs; the grant-scope lattice is
   ADR-0011's to state — its 2026-09-19 amendment), the four modes as
   presets over the rule layer. Diffs render from `similar`
   (line + inline word-level) with theme-aware added/removed tints from
   ADR-0017's `diff-*` slots; approval prompts embed the diff;
   rejections carry an optional comment into the trajectory.
9. **Testing: vt100 backend + insta, matrix-locked.** The primary
   backend is a vt100 virtual terminal (asserts final screen _and_
   scrollback — ratatui's TestBackend has no scrollback concept);
   snapshots co-locate with their modules; integration tests share one
   binary. The render matrix from ADR-0017 item 11 applies. `just
   snapshot-review` discipline stands; vhs stays out of the gates (Go
   supply chain) as a local demo recorder.
10. **Scale discipline, learned from Codex's 345k-line TUI.** The TUI
    crate is wiring: protocol events flow into a view-model
    materialization, and widgets read materialized state only. A
    god-enum of UI-internal events (Codex's AppEvent) is barred; the
    arch test gains the dependency rule (`cadmus-tui` sees the client
    protocol, not core internals).

## Amendment — 2026-09-14: the inline spike verdict — stock viewport adopted

The item-3 spike landed as `crates/cadmus-tui/examples/inline_spike.rs`
(manual harness, kept as the terminal-quirk regression tool). Verdict:
**stock `Viewport::Inline` + `Terminal::insert_before` suffices; no
fork.** The Windows binary for the WT run was cross-built with the
windows-gnu target via the nix mingw stdenv.

Evidence (harness diagnostics per terminal):

- Zed terminal (xterm-class), tmux, Windows Terminal: streaming with
  history inserting above is loss-free and correctly ordered (WT: 19
  turns / 56 inserts / 1170 rows across 186 resizes).
- Shrink reflow re-materializes the visible history tail from source at
  the new width on all three terminals (37 replays on WT, 5 on tmux).
- WT shows zero sign of its known partial-DEC-scroll-region line drops:
  the portable insert path never emits DEC regions (source-verified), so
  the quirk class is dodged structurally rather than by strategy
  dispatch.
- Zellij untested (not installed); its known quirk also targets DEC
  scroll regions. Residual risk accepted: field corruption reports
  reopen this item.

The spike also pinned three event-loop disciplines and two accepted
costs, all binding on the first TUI PR:

1. **Fixed-height viewport layout** — the inline height has no mutation
   API (source-verified); the layout design works within a fixed height.
   What is fixed is only the band's total row count: per-frame re-split
   among the widgets inside it (stream tail, composer, status) is
   ordinary layout — a growing composer borrows rows from the stream
   area, never pushes the band taller (dynamic height, the fork's
   feature, would claim more rows of the existing screen mid-turn; it
   cannot grow the window itself). Bounding the composer (max lines,
   then internal scroll) is part of the first PR's layout rule. On
   terminal resize the band keeps its row count and width always
   follows; a taller window simply shows more scrollback above the band.
   If fixed height ever becomes the hard requirement that reopens
   item 3, the first thing to spike is fact F1's untested escape
   hatch: recreate the `Terminal` on height change — dynamic height
   without the fork. _Superseded the same evening in the height
   dimension: the second 2026-09-14 amendment adopts dynamic height
   on exactly this escape hatch; the re-split, composer-bounding and
   resize rules above survive._
2. **Resize debounce (~75 ms, Codex precedent)** — re-anchoring scrolls
   the terminal, so each processed resize leaves the previous frame as
   scrollback residue; debounce bounds it to ≤1 stale frame per drag
   gesture.
3. **Cursor-query error tolerance on every path** — re-anchoring issues
   a DA cursor-position round-trip, including inside `draw`'s
   autoresize; queries time out under resize storms and quirky stdio.
   Tolerate and repaint on the next tick; never die mid-run.
4. Accepted cost: scrollback duplication when a shrink replay fires —
   stock ratatui cannot delete its own scrollback rows (Codex's
   DEC-row-delete is the unportable trick this decision forgoes).
5. Accepted cosmetic item: the exit path's treatment of the final
   viewport frame is a design decision for the first TUI PR (the harness
   overprints it).

The thin-fork fallback (Codex blueprint, MIT attribution) stays on the
shelf: reconsider if field use shows the residue/duplication costs are
unacceptable, or if a terminal in the support matrix misbehaves under
the portable path.

## Amendment — 2026-09-14 (2nd): band height is dynamic, via Terminal recreation

Maintainer call after the fixed-vs-dynamic review: **the band's row
count changes at event boundaries**, delivered by recreating the
`Terminal` — amendment item 1's fixed-height layout is superseded in
the height dimension (its per-frame re-split, composer-borrows-rows
and resize rules stand; the escape hatch it pointed at is now the
mechanism). Rationale: with the shape/style flush invariant (item 4)
already dissolving most of the long-unstable-block problem at the
render layer, the remaining cost of a fixed band is UX, not
mechanism — held blocks taller than the stream area (tables) and a
composer squeezed against a small window — while the escape hatch's
verified price is one CPR round-trip plus one in-guard repaint per
change, and the flush invariant is needed under either height policy.

Mechanism (evidence: `docs/research/2026-09-14-terminal-recreation-spike.md`):

1. Height changes are event-driven, never per-frame: composer line
   crossings, held-block settle, resize. The height function (desired
   band height over content, capped at the screen — Codex's
   `desired_height` precedent) and the layout rules already scoped in
   amendment item 1 (composer cap, short-window corner) land in PR 1's
   shell — the inline shell, the cadmus-tui library layer that owns the
   raw terminal and the band's lifecycle (anchor, height, `Terminal`
   recreation, resize reflow, guarded draws) and frames the band the
   widgets live in; a chrome shell, not a command interpreter.
2. Grow: `insert_before(Δ blank rows)`, park the cursor at the future
   band top, recreate — the re-anchor's append lands exactly at the
   bottom row (zero scroll), and the taller band's first repaint
   covers only the inserted blanks (zero history loss, zero residue).
3. Shrink: `clear()` the old band, park the cursor Δ rows lower,
   recreate — the vacated Δ rows are the bounded blank residue
   (consumed by later flushes); collapsing rows upward is the
   DEC-row-delete trick this stack forgoes.
4. Height policy works on effective (screen-clamped) heights and
   no-ops on equality (the full-screen band has nothing to grow into).
5. One 2026h guard wraps insert + recreate + draw; the PR 1 input
   broker is the designated seam for the construction-time CPR race
   (upstream ratatui #2640, open).

Verified: eight vt100 scenarios (`tests/dynamic_height_spike.rs`, in
CI) plus a three-terminal mechanical matrix (Zed, Windows Terminal,
tmux — capture-replay, never eyewitness): zero rows lost or
duplicated, guards balanced, no swallowed keys; the naive-recreation
control produced exactly its predicted residue on all three. Accepted
residual: a transient flash on terminals that ignore 2026h — bounded,
cosmetic, and identical for every implementation including the fork.
Zellij untested; the portable path emits no DEC scroll regions, so
its known quirk class is dodged structurally, and field corruption
reports reopen this item.

Unaffected: the shape/style flush invariant (item 4) — height policy
and flush policy are orthogonal axes (Codex flushes completed cells
with dynamic height too); spike disciplines 2–5 stand; the thin fork
stays shelved, re-entering only if a support-matrix terminal
misbehaves under the portable path.

## Consequences

- The daily-driver acceptance line is ADR-0011's floor v2 plus this
  ADR's mechanisms; the inline spike (item 3) gates the first TUI PR.
- The interaction-surfaces-never-render-logs open item gets its
  consumer: structured progress is a projection of the live stream, and
  logs never enter the interaction view.
- Deferred with named triggers, all recorded above: composer vim mode,
  Starlark expression zone, textwrap, hooks (ADR-0011 item 6
  unchanged).
- ratatui's exact-MSRV fit (1.88) makes toolchain bumps coupled events:
  `just toolchain-bump` must verify ratatui's published MSRV before
  raising ours.
- The spike's verdict amends item 3 (stock-viewport vs thin fork); no
  other item is affected by the outcome.
- Band height is dynamic via Terminal recreation (second 2026-09-14
  amendment); PR 1 owns the height function, the layout rules and the
  recreation seam inside the shell.
- PR sequencing (2026-09-15): the inline shell landed first — the band
  mechanism (guarded ops, the recreation protocols, the resize replay)
  plus the spike harness's collapse into a thin driver over
  `cadmus_tui::shell`. The input broker (the CPR-race seam, amendment
  item 5), the resize debounce and the frame scheduler landed with the
  event loop. The composer/stream widgets landed with the content-driven
  height function and the widget layout rules (composer cap,
  short-window corner — `cadmus_tui::layout`), together with
  `cadmus-ui`'s streaming-markdown pipeline, syntect highlighting and
  the semantic-style IR they render (items 2 and 4), all per this
  ordering. The app wiring landed last (2026-09-16): the view-model
  materialization (`cadmus_tui::transcript`, item 10 — one snapshot per
  pump batch drives flush, band render and layout, closing the
  per-accessor re-render open item), the draw pump, the `EventSource`
  input seam and the real-terminal boot (`cadmus_tui::app`, item 5), and
  the binary's session driver — bare `cadmus chat` on a terminal now
  launches the interactive session (one run per prompt, history carried
  client-side), Esc interrupts, approvals auto-resolve by the
  unattended policy until the item-8 slice, and tracing redirects to
  `{trace_root}/cadmus.log` (consuming the never-render-logs item). The
  mouse-capture decision landed with it: capture stays off (native
  scroll/select/copy is why item 2 of ADR-0012 chose inline). Remaining,
  each with its named consumer: item 6's keymap-as-data layer (the
  binding-design task), item 8's approval/diff UX, item 7's TOML
  loaders.

## Amendment — 2026-09-19: the per-call approval dialog's interaction contract

With per-call settlement landed (ADR-0008's 2026-09-18 amendment), item 8's
approval surface has a contract worth stating before item 6's keymap slice
owns the concrete bindings:

- The dialog lists every presented call. Tab / Shift-Tab move the selection
  (the composer keeps the arrow keys), and y / n answer only the _selected_
  call — one `resolve_approval_call` per answer, never a batch-wide key. The
  batch stays the presentation unit (ADR-0008 item 4); its submission may be
  per call.
- **Tab arms before an answer lands.** y / n do nothing until a Tab has armed
  the selected call, because a key held or queued across a re-sync, a new
  request or a new prompt must never answer a prompt the user has not seen;
  submitting and receiving a remote decision both disarm. This is a safety
  property, not a default binding: item 6 may rebind the keys but must keep
  the deliberate-act-before-answer rule.
- Submission is not settlement: the dialog keeps the request until the
  recorded decision arrives (a racing client or the gate's timeout may win),
  and a submitted call reads as `sent` meanwhile. Once nothing is answerable
  the dialog says it waits rather than advertising keys that do nothing.
- Local state is display only: the recorded command is the truth, the first
  recorded decision per call wins (ADR-0008), and a call the user answered
  must not become answerable again.

The diff embedding is unchanged (item 8; the file-backed cumulative diff
slice remains its consumer), and the dialog's keys remain subject to item
6's binding-design task.

## Amendment — 2026-09-20: grow-only band during a run — the high-water hold

Field evidence (2026-09-20, vt100 end-to-end reproduction of a real
session): the shrink residue fires **per block mid-stream**, not just at
turn end — blank rows interleave with flushed content (one landed between
tight-list items 2 and 3). The pump recomputed the desired height from the
post-flush live tail every batch, so every block settle (a held paragraph
flushing several rows at once) and every approval-dialog or composer
appear/disappear shrank the band and left Δ vacated rows mid-page.
**While a run is active the band's desired height never shrinks**: the app
tracks the run's high-water floor — seeded at submit with the band's
current height, BEFORE the composer clears, so the prompt flush and the
composer collapse produce no residue either — raised by the content's own
want (`desired = max(content, floor)`), and released at the run's outcome
(interrupt rides the same path). The held slack pads the top of the
bottom-anchored stream slice (`BandLayout::held_at`; its render already
tails, so the padding is blank above the live tail). Idle-session behavior
(the composer's own grow/shrink while typing) is unchanged; the floor is a
desired-height concept, screen-clamped by the shell like any other, so the
resize rules stand; back-to-back runs seed a fresh floor.

The release collapses the band once to the idle height, and the collapse
is **top-anchored**: the band keeps its top edge, so the Δ vacated rows
sit BELOW the band as a blank buffer — never a gap inside the transcript.
Later growth re-absorbs the buffer without scrolling (`grow_from`'s absorb
phase); later inserts descend into it (the portable insert path re-anchors
the viewport below the inserted rows and clamps its scroll at zero while
room remains below). The pump order is flush-before-collapse, so the
run's final batch lands before the buffer forms. Net effect: ZERO visible
residue per run — the transcript carries exactly the markdown's own
separators, and the dead rows sit where a CLI's trailing space naturally
does. (Two earlier positions were field-rejected the same day: blanks
interleaved mid-content — the pre-hold mechanism — and blanks wedged
between the last content block and the completion note — the first
shrink-before-flush cut of this amendment.) The recorded alternatives from
the 2026-09-17 forensics (scroll-relocation, holding the settled tail
in-band across the collapse, DEC row deletion) stay on the shelf: the hold
removes the mid-stream corruption without a flush-contract change, and the
top-anchored collapse removes the visible collapse residue with stock
mechanics.

Verified: the blank-preserving characterization suite (`tests/app_loop.rs`)
— a two-paragraph-plus-tight-list turn streams through several settle
cycles with zero blanks interleaved beyond the markdown's own single
separators, the dialog/type-ahead and width-grow holds, the interrupt
collapse, and back-to-back runs each leaving the buffer below the band,
never in the transcript — plus the spike suite's flipped shrink protocol
(`tests/dynamic_height_spike.rs`: the band keeps its top edge, the buffer
hangs below) and the pre-existing vt100 suites green. Companion fix, same
pass: between the terminal record and the outcome the run-status row's
`Idle` light rendered a blank row while the run was still active; it now
reads `Working · Ns` (the state-truthfulness rule).

## Amendment — 2026-09-20 (2nd): paced emission — stable rows type out, the unstable tail is never rendered

Field evidence (maintainer report, 2026-09-20): the live window renders the
_unstable_ markdown tail — paragraphs re-wrap as they grow, fences
re-highlight, tables pop in on close ("吐字一块一块地喷出来，喷出来马上又渲
染，看起来非常凌乱"). The flush contract already knows exactly which rows
are stable (`Render::flushable_len`); rendering anything past it was the
mistake. **Rows are emitted only after they are stable, at a paced
typewriter rhythm, and the unstable tail is completely hidden** — the
design discussion's option (a), chosen over any raw-text preview so that no
source of jumpiness survives. The reference precedent is codex's
`streaming/commit_tick.rs` (stable content queues and drains at a paced
rate, catching up under backlog), adapted to Cadmus's flush contract.

Mechanism:

1. **The emission queue lives transcript-side.** The snapshot no longer
   returns rows for direct insertion; it appends every newly stable row to
   a per-run FIFO of _emissions_ (one per block slice, carrying the flush
   plan for that slice). Rows leave only through the app's paced drain,
   and a row is acked to the stream only after its _successful_ shell
   insert — the ack contract is unchanged from the pre-queue model.
2. **The band loses its stream slice.** Slices are now: the `receiving…`
   row, the approval section, the run-status row, the composer, the floor
   line. `STREAM_MAX_ROWS` and the `↑ N more lines` overflow indicator die
   with the slice (the feature is superseded); `Snapshot.live_rows` and the
   transcript's live-tail plumbing die with it. The visible typewriter IS
   the paced scrollback inserts: each drained row appears above the band,
   the band slides down one row — smooth, no window.
3. **Pacing is tick-driven, app-side.** While the queue is non-empty the
   app self-reschedules a frame at 33 ms (the `schedule_frame_at` horizon
   pattern the 1 Hz clock already uses; all timing stays injected at the
   app's edges). The per-tick budget steps by queue depth: `< 8` rows → 1,
   `8..24` → 2, `≥ 24` → 4, with a floor of 4 while a run is finishing
   (constants in one place). Esc dumps the queue whole until the run's
   drain completes. Attach/resync replay bypasses pacing entirely
   (replayed history must not re-type). `TERM=dumb` disables pacing
   (instant emission — the cadmus-tui terminal boundary detects it,
   `style::detect_paced`, mirroring `detect_depth`); the full
   `full|reduced|none` motion profile lands with the item-7 TOML loader
   (recorded as an open item consumed by that work). Item 5's two-regime
   hysteresis lands in this form — a depth-tiered budget over the tick;
   the age dimension stays deferred for lack of evidence.
4. **The `receiving…` row is the liveness signal** replacing the hidden
   tail: one subtle row at the band's top, visible iff a run is active AND
   (queue non-empty OR unstable tail non-empty), event-driven like every
   other slice, gone by the time the run's drain completes.
5. **End-of-run is an explicit state (`run_end`), not ordering luck.** At
   the outcome the clock freezes as today; the run's remaining stable rows
   AND the completion note go through the queue LAST, so the note types
   out after the content by construction. The floor still releases at the
   outcome (the band falls to the run's resting height — the receiving row
   already gone); the run-status row rides its frozen clock until the
   queue empties and the note is inserted, and ONLY THEN hides — the
   band's final collapse is exactly that one row (3→2), so the end-of-run
   composer jump dies. Interrupt: instant dump, then the note, then the
   collapse.
6. **Approvals are not a display gate**: emission continues while an
   approval blocks the run; the dialog keeps band priority; the run clock
   keeps pausing across the wait.
7. **The high-water floor stays.** With no stream slice, mid-run band
   oscillation is just the approval section and the receiving row — the
   floor is cheap insurance over exactly those, unchanged (seeded at
   submit, running max, released at the outcome).
8. **Resize folds the queue back.** Queued rows carry the old width's
   wrap, so a width change rewinds the queue into its blocks and the next
   snapshot re-queues at the new width (sound because nothing queued is
   acked yet). A partially drained front emission is the one exception:
   its remaining rows keep the old wrap — their already-inserted prefix is
   scrollback stock ratatui cannot delete, and re-queuing them would
   duplicate it. Bounded to one emission, cosmetic, the same accepted cost
   class as item 4's frozen-row styling.

Verified: `tests/app_loop.rs` — the paused-clock pacing suite (33 ms →
exactly the tier's budget, backlog batches), the unstable tail never
rendered on any tick, the `receiving…` lifecycle, the end-of-run pin
(content types, THEN the note, THEN the 3→2 collapse with Δ = 1), the
interrupt's instant dump, the attach replay's instant insert, and the
blank-structure characterization suites byte-identical to the pre-queue
model (emission changes WHEN rows appear, never WHAT appears) — plus the
transcript rewind unit tests, `tests/stream_flush.rs` (the flush oracle,
now with the never-rendered property pinned at that level too), and the
pre-existing suites.

Companion fix, same pass: the vt100 suites exposed a latent ratatui bug
that the constant insert cadence made reproducible — the portable
`insert_before` path's closing `Terminal::clear` restores the cursor to
its pre-insert position, a row the viewport slide just pushed ABOVE the
band; a resize reading that cursor offset saturates it to zero and pulls
the viewport UP over the inserted rows, erasing them (any resize within
the debounce window after streaming inserts). The shell now parks the
cursor inside the viewport after every insert (`tests/dynamic_height_spike.rs`
locks it; recorded in the ratatui-bump open item as upstream evidence).

## Amendment — 2026-09-21: own history insertion and band geometry

Field evidence reopens the first spike's `insert_before` verdict: ratatui's
portable path writes wide-character continuation cells as spaces. Chinese
text gains physical width, wraps unexpectedly and loses its tail under the
next insert, despite an intact trajectory. Waiting for upstream #2527 is
not acceptable for a daily driver. Adopt Codex-style span writes and CRLF
scrolling, with terminal-specific fallback, without a dependency fork.

The recorded small-shim premise was wrong: stock ratatui's viewport setter
is private, unlike Codex's custom Terminal. Recreating Inline after each
insert also couples cached cursor offsets, extra size reads and automatic
clears; a resize between insert and redraw can erase just-acknowledged
history. **Use Inline only for the initial reservation, then stock Fixed
as the drawing surface.** The shell owns subsequent geometry, retains the
high-water/top-anchored-collapse policy and keeps one synchronized-update
wrapper per operation. The post-insert cursor-parking workaround retires;
a hidden cursor no longer participates in geometry.

The port follows Codex's `insert_history.rs` and `tui/scrollback.rs`
(re-read 2026-09-21; Apache-2.0 attribution retained in `history.rs`). CRLF
at the history region's lower margin is intentional: CSI S discards
native scrollback in QTermWidget/xterm.js. Windows Terminal uses the
full-screen fallback; Zellij takes precedence and uses the standard path
for Cadmus's pre-wrapped rows. Zero/one-row history regions also fall back
because DECSTBM requires distinct margins. Clear the old band before
scrolling and reset margins even on write failure; at the screen origin,
clear rows individually so tmux's default scroll-on-clear cannot copy a
transient composer into permanent history.

Debounced horizontal shrink still replaces the visible history from source.
Intervening draws and height changes cannot consume that replay obligation
or clear the history independently. Vertical fitting scrolls reachable
history before occupying its rows. The display-row prefix of a partial
emission now has successful-insert coordinates (source slice, old wrap
width, confirmed row count): whole-emission acks alone omitted it from
replay. Reconstruct it before reflow, never expose queued continuations,
and retain no second rendered-history cache. The kept remainder retains
its old row boundaries, rewrapped only if too wide for the current screen.
Controls are filtered at the raw-write boundary; a grapheme wider than the
entire screen uses an ASCII Unicode escape instead of silently disappearing.
The source stays intact for later replay at a usable width.

Evidence: `tests/history_insert.rs`, the dynamic-height/app/stream vt100
suites and the partial-replay unit tests cover text, blanks, styles,
cleanup, hidden cursors and physical resizes before/after raw writes.
The emulator drops partial-region departures, so full-world assertions
use the full-screen path and standard-path assertions cover visible
rows/protocol only. Native tmux probes with default scroll-on-clear
matched the harness's nonblank history and two long CJK histories including
blank rows. Windows Terminal, Zellij and GUI-terminal physical reflow still
need manual matrix checks; the tmux run is not evidence for those terminals.

## Amendment — 2026-09-21 (2nd): the steer bindings — Enter injects, Tab queues

Item 6 leaves the default keymap content to the binding-design task at
implementation; the steer slice decides its first mid-run content.
**Enter = Inject, Tab = Queue** — Codex's Tab/Enter split, because
ADR-0011's 2026-09-11 amendment pins inject-at-next-tool-boundary as the
default granularity and the default deserves the unmodified submit key.
Claude Code's Enter=queue is thereby rejected, as is a mode-toggle design
(Tab arms queue mode, Enter sends): two direct submit keys, one keystroke,
no mode state to display and forget. The three named granularities
realize as two bindings because inject-now cannot touch the in-flight
request (its bytes are on the wire) — its only deterministic realization
is the next request boundary, which is the third granularity's own
definition; Esc + steer covers redirect-now.

Precedence: while the approval dialog is open it owns Tab/Shift-Tab and
y/n (the 2026-09-19 dialog amendment); Enter is not the dialog's and
steers — typing and steering during an approval wait both work, the
steer applying after the gate settles. Idle Tab is a no-op (nothing to
steer), as is an empty composer.

Feedback follows record-on-effect: the composer clears on send, the core
records the steer at application, and the recorded command renders the
prompt block (the transcript's steer arm pre-exists). The gap is bridged
by a pending count in the composer placeholder — a queued steer's hold
can outlive minutes of streaming, and an unacknowledged hold reads as a
lost keystroke. The count retires per recorded steer, zeroes at the
outcome (unapplied steers die unlogged) and at a resync (the hole's
applications landed in the fold, the still-buffered ones are unknowable,
so the count can only over-report from there). The acknowledgment lives
on the placeholder deliberately: it renders on the empty buffer, which is
exactly the post-send moment — while the user composes the next steer the
count hides, accepted because the next send re-displays it and a
persistent indicator would cost the run-status row's budgeted width.

The running placeholder names the working mid-run keys (the truthfulness
rule) with two variants: the full pair (Enter steer · Tab queue · Esc)
normally, and without the Tab clause while the approval dialog is open —
the dialog owns Tab there and its own hint row says so; a placeholder
claiming otherwise would lie through the whole wait. One policy point
(`sync_composer_placeholder`, the `sync_clock_pause` pattern) recomputes
the text at every transition of its three inputs: run presence, pending
count, dialog visibility.
