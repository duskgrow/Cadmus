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
   transport cannot truncate the transcript.
5. **Event loop: actors, capped frame rate, input never blocks.** A
   FrameRequester/FrameScheduler actor pair coalesces redraw requests
   and caps at 60 FPS; frames render inside a synchronized-update
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
