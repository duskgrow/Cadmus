# 0017. Design system: Linear-structured tokens, one theme SSOT, renderer-level degradation

- Status: accepted
- Date: 2026-09-13

## Context

Maintainer directives, 2026-09-13: the GUI is the **end-state primary
interface**; the TUI remains the first renderer (the 2026-09-11 survey's
five reasons stand); the TUI must **inherit the GUI's aesthetic** rather
than grow a terminal-native skin that the GUI later overthrows; and the
two frontends must share enough that we never maintain two design
systems. Aesthetics are near-irreversible once shipped, so the design
language is decided now — at GUI-grade fidelity — as the shared
foundation, not deferred to the GUI ADR.

Research exhibits (frozen, `docs/research/`):
`2026-09-13-gui-design-language-research.md` (Linear as primary
reference, terminal-adaptability verdict), `2026-09-13-design-token
engineering.md` (color science, token architecture, icon/motion/theme
engineering), `2026-09-13-tui-aesthetic-style-research.md` (terminal
precedents and degradation discipline). Time-sensitive claims were
verified same-day per the freshness policy.

Key facts from the exhibits. Linear's identity is carried by structure,
not surface effects: dark/light themes are co-generated in a perceptually
uniform space from three variables (base/accent/contrast) per theme; the
blue-violet accent is deliberately chroma-limited; text has four levels;
status icons encode by fill geometry (○→●), not color; motion is
~160 ms ease-out. Its concrete color values and type metrics were not
obtainable first-hand (marked unverified in the exhibit) and are treated
as **design freedom**: we construct our own scales by the same method
rather than copy Linear's pixels. The terminal-adaptability verdict: the
bones (grayscale discipline, accent restraint, density, keyboard
structure, status geometry) transfer directly; the skin (elevation
layers, dual typefaces, smooth animation, hover) degrades; only the
type-size ladder and social avatars are lost, both compensable. The
broader market converges on semantic color slots + dark/light dual
values + render-layer degradation (Gemini CLI, Claude Code, OpenCode,
bat, delta, zellij, yazi) — this ADR adopts that convergence as the
architecture.

## Decision

1. **One design system, two renderers.** The design language is defined
   once, GUI-grade, and owned by a shared crate (`cadmus-ui`: tokens,
   palettes, theme loader, icon registry, motion scales). The TUI is its
   first renderer and applies the degradation discipline below; the GUI
   renderer consumes the same tokens and theme files with no re-design.
   The GUI ADR's aesthetic scope is consumed by this ADR; what remains
   for it is the tech re-anchoring and the orchestration/review patterns
   (open item updated).
2. **Design direction: Linear's bones, terminal flesh.** Grayscale-led
   surfaces with one chroma-limited accent; hierarchy carried by type
   weight and subtlety steps, not chrome; status encoded by fill geometry
   plus color (double-coded always); motion rare and fast (~160 ms class)
   where the renderer supports real motion. Agent-scene precedents fill
   the gaps Linear never faced: streaming/tool-indicator/diff/approval
   surfaces from Zed's agent panel, command-palette structure from
   Raycast, multi-state lists from Linear's Inbox.
3. **Token architecture: raw scale → semantic slots → components, with
   an anti-bloat gate.** The semantic slot set is fixed at 18:
   `bg`, `bg-subtle`, `text`, `text-subtle`, `accent`, `on-accent`,
   `success`, `warning`, `error`, `info`, `border`, `border-active`,
   `diff-added`, `diff-removed`, `diff-added-bg`, `diff-removed-bg`,
   `mark`, `selection`. Components never hardcode values. A new slot is
   admitted only by naming the landed renderer that consumes it.
4. **Palette construction: OKLCH lives in the offline generator; the
   runtime stores sRGB hex only.** An xtask subcommand generates the
   palettes (Radix-style 12-step scales with the background / component /
   border / solid / text track semantics; WCAG contrast gates — 4.5:1
   body text, 3:1 large text and UI glyphs — as mechanical assertions in
   the generator); dark and light scales are constructed independently
   (dark is not an inversion; dark-mode borders are alpha overlays). No
   color-science dependency ships in the binary.
5. **Four built-in presets, detected not assumed.** `dark`, `light`,
   `dark-ansi`, `light-ansi`; default `auto` resolves terminal capability
   (color depth and background) via the chain COLORTERM/supports-color →
   OSC 10/11 query (100 ms timeout) → COLORFGBG → `dark-ansi` fallback,
   accepting the dark-default misjudgment cost on Windows Terminal and
   tmux (both documented non-responders). The `ansi` presets use only the
   16 named colors so the user's own terminal palette can take over
   entirely (gh CLI's accessibility precedent). Color depth
   (truecolor→256→16→none) and glyph tier (unicode→ascii) degrade in the
   render layer; ratatui does not degrade `Rgb` safely on its own, so the
   mapping is ours.
6. **Typography.** GUI type scale 12/13/14/16/20/24/28 with weights
   400/500/600 only; UI and code fonts pair per platform convention.
   TUI: the hierarchy maps onto bold/dim/inverse; italic may decorate
   (quotes, asides) but never carries sole semantics (screen-class
   terminfo drops it).
7. **Spacing.** Base-4 scale, nine steps. TUI renders in cell units with
   no sub-cell spacing; density is a discipline, not a knob.
8. **Motion: capability profiles, not per-feature flags.**
   `motion = full | reduced | none`. The whitelist is spinner frames and
   one-shot state-settle recolors; GUI adds transitions on Carbon
   productive curves. Piped output, line mode, `TERM=dumb`, and the
   config switch all force `none`. Terminals have no REDUCE_MOTION
   convention (verified 2026-09-13); ours is explicit.
9. **Icons: Lucide, three tiers, no private-use codepoints.** The
   registry maps a semantic name to `{ gui: Lucide SVG, unicode, ascii }`
   variants; Nerd Font PUA codepoints are banned; glyphs with default
   emoji presentation are avoided or pinned with VS15. The registry is
   locked by an icon-audit snapshot test (this pins the codepoints the
   exhibit left unverified).
10. **Theme files: TOML, small surface, hot-reloaded.** One file per
    theme under `$XDG_CONFIG_HOME/cadmus/themes`: `[meta]`, a `base`
    preset, and `[overrides.dark]` / `[overrides.light]` slot subsets.
    Unknown keys and invalid values warn and are ignored; structural
    errors report via miette's three-part diagnostic. Reload is mtime
    polling on the event loop (~1 s) — `notify` is CC0-1.0, off the
    license allow-list, and pulls no exception for a comfort feature.
11. **Testing.** Every slot in every preset is snapshot-locked; the CI
    render matrix is dark/light × truecolor/16-color/none ×
    unicode/ascii. Style rules that can be mechanized live as tests, not
    prose.

## Consequences

- ADR-0011 item 5's "theme configurable" is specified by items 3–10;
  the configuration-surface principle (every knob needs a real user) is
  unchanged and now guarded by the item-3 anti-bloat gate.
- The TUI renders the design system's degraded form from its first
  commit — there is no interim terminal-native skin to throw away.
- The GUI ADR inherits this ADR whole; aesthetics must not be
  re-litigated there.
- `palette`/`color` (generator-only) and `toml` (runtime theme loading)
  enter the tree through the adding-dependencies skill when the
  generator and theme loader land, respectively.
- Revisit triggers: a GUI tech choice that cannot consume TOML theme
  files or Lucide SVGs reopens items 9–10; field evidence of `auto`
  misdetecting backgrounds at scale reopens item 5's fallback.
