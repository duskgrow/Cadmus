# 0023. Design system goes terminal-native; cadmus-ui merges into cadmus-tui

- Status: accepted
- Date: 2026-09-23

## Context

Maintainer direction (2026-09-23): the GUI will be redesigned
independently in its own repository (ADR-0019) and will not inherit
this repo's design language; the provisions made here for GUI–TUI
compatibility are over-design and are dropped.

This supersedes ADR-0017's core premise — "the GUI is the end-state
primary interface; the TUI must inherit the GUI's aesthetic; one design
system, two renderers" sharing `cadmus-ui` — and the "GUI-inherited"
framing in the roadmap's capability track.

The inventory at the pivot makes the descope cheap. **Unbuilt** — pure
plan deletion: the OKLCH palette generator with WCAG contrast gates
(xtask), the full four-preset set and TOML theme loader, the Lucide
icon registry with its audit, the GUI type scale and motion curves.
**Built**: the 18-slot semantic token set wired through the TUI, the
ANSI preset with capability degradation, the content pipelines
(streaming markdown, syntect highlighting, diffs — ~3000 lines), and
the semantic-style IR the pipelines emit, whose documented
justification was "the future GUI maps it onto its rich-text
equivalent" (`cadmus-ui` crate docs).

## Decision

1. **Terminal-native scope.** This repo's design system serves the TUI
   alone. Kept — as terminal legitimacy, not GUI inheritance: the
   semantic slot set (established terminal-theming practice — delta,
   bat, gh), dark/light/ANSI presets, capability degradation (color
   depth, glyph tier) and the content pipelines, which the TUI needs
   regardless of who else consumes them. Dropped: everything in the
   unbuilt inventory above. User TOML themes are deferred, not
   promised — trigger: field demand; ADR-0011 item 5's "theme
   configurable" is satisfied by preset selection in config.
2. **`cadmus-ui` merges into `cadmus-tui` and the IR seam is
   removed.** With one renderer, the renderer-agnostic IR is an
   abstraction for a single use; the pipelines emit presentation types
   directly, with slot→style resolution kept at one seam. This is a
   behavior-preserving cleanup (the `code-simplification` skill), its
   own PR, sequenced after the phase-1 closeout (ADR-0022) and before
   `serve` (ADR-0021) so it never fights in-flight TUI work. The arch
   test keeps ADR-0013 item 9's rule: the merged crate is a frontend
   crate, and presentation dependencies stay forbidden in core and
   contract.
3. **Design independence is explicit.** The GUI repo owns its design
   language from scratch; no tokens, themes or icons cross the
   repository boundary (ADR-0019 item 5). The cost — two design
   languages to evolve — is accepted because each is native to its
   surface and neither waits on the other.
4. **Amendments.** ADR-0017's premise and two-renderer machinery are
   superseded as above; its renderer-level degradation discipline
   stands, now terminal-scoped. ADR-0018's "semantic-IR pipelines"
   architecture is amended by item 2. The roadmap's "GUI-inherited"
   wording is removed in this PR.

## Consequences

- The deletion is fearless because it is reversible: item 2 does not
  touch the pipelines' internals, and re-extracting a shared crate is
  cheap if an in-repo second renderer ever appears — recorded so that
  future decision does not relitigate this one.
- ADR-0017's testing item trims to the TUI render matrix; the slot
  snapshots stay as the regression net.
- The arch test's crate-topology expectations lose one crate edge when
  the merge lands (item 2's PR updates them).
- `docs/tools.md`, AGENTS.md and the skills are untouched by this
  ADR; the open item "Reassess history ownership on the next ratatui
  bump" is unaffected — ratatui coupling shrinks but does not vanish.
