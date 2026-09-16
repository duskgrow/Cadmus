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
   (match over tool input via globs; grant scope once/turn/session/
   persisted) per ADR-0011's 2026-09-11 amendment item 3, the four
   modes as presets over the rule layer. Diffs render from `similar`
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
