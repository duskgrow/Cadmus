# 0019. Foreign-language GUI: the client wire protocol becomes the public API

- Status: accepted
- Date: 2026-09-23

## Context

Maintainer direction (2026-09-23, one design discussion settling the
system's shape): Cadmus converges to server + clients. Users interact
primarily through a GUI; servers deploy on multiple machines in a mesh
(ADR-0020) and support Linux + macOS only (ADR-0021). The GUI will be
written in a non-Rust language — the shortlist is Flutter (Dart) vs
Kotlin (Compose Multiplatform), final pick at the GUI project's
kickoff — and designed independently: it does not inherit this repo's
design language (ADR-0023 supersedes ADR-0017's premise).

This rewrites ADR-0013 item 8's GUI boundary. The old plan: a
Rust-native GUI (GPUI candidate), out-of-repo over GPUI's license,
importing cadmus as a library, landing after phase 5. Three of its four
load-bearing facts are gone:

- A foreign-language GUI cannot link Rust crates; the client protocol
  over a byte transport is the only attachment seam.
- The license rationale for living out-of-repo is void — nothing links
  anything. The toolchain boundary takes its place: this repo is
  Nix + cargo shaped, and a Dart/Gradle toolchain does not belong in
  its flake, `just ci` or dist pipeline.
- "Imports cadmus as a library" made the Rust public APIs of
  `cadmus-contract`/`cadmus-core` the semver surface. A foreign client
  never sees those APIs; what it sees is the serialized protocol.

What stands: ADR-0013 item 1 (one client protocol, N frontends — the
foreign GUI is the same client kind), item 7 (semantics in core,
presentation in clients — events carry semantic payloads, so a foreign
renderer needs no core changes), item 8's second half (contract types
stay runtime-agnostic pure data — now strictly load-bearing), and the
out-of-repo location under the new rationale. ADR-0014 (ACP) is
unchanged: a secondary surface that cannot carry orchestration
semantics; the foreign GUI speaks the native protocol.

## Decision

1. **The GUI is a foreign-language, independent-repository client of
   the wire protocol.** It attaches over the byte transport like any
   remote client (ADR-0013 item 1). This repo never gains a
   Dart/Gradle toolchain; the GUI repo never links cadmus.
2. **The wire schema becomes a versioned public API.** The normative
   vocabulary stays `cadmus-contract` (ADR-0013 item 10); a JSON
   Schema export is generated from those types mechanically (dependency
   admission per the `adding-dependencies` skill when it lands) and is
   the language-agnostic artifact the GUI consumes. Evolution stays
   additive-only with serde defaults; the insta byte-locks already gate
   the shapes. Schema versions ride the workspace version (single
   storage point; release-plz rewrites it), and the client/server
   semver handshake window (ADR-0016 item 4) now binds across
   languages.
3. **A conformance corpus is the cross-language semantics lock.**
   Recorded event streams plus their expected client projections are
   published from this repo, versioned with the schema; the GUI's CI
   runs them. They extend, not replace, the executable client rules
   (`client_protocol_tests!`: drop positions ≤ `as_of_seq`, `Lagged`
   forces re-sync, idempotent commands), which stay in-repo.
4. **The GUI implements the client protocol in its own language
   first.** The attach state machine (Sync baseline, delta application,
   re-sync) is a few hundred lines; the corpus is the drift guard.
   Binding the Rust core via FFI (flutter_rust_bridge / UniFFI) is a
   documented escape hatch, triggered by evidence of drift pain —
   never built speculatively.
5. **Nothing design-shaped flows to the GUI.** Tokens, themes and
   icons are not published as artifacts (ADR-0023). The artifact
   family is exactly two: wire schema, conformance corpus.
6. **Scheduling.** The GUI spike starts once the wire-hardening slice
   lands (`serve` + local socket + schema/corpus export — ADR-0021's
   phase-5-lite). This replaces ADR-0013 item 8's "landing after
   phase 5" and the GUI-ADR scheduling note in ADR-0015's consequences:
   the direction is decided here; detailed GUI design is the GUI repo's
   own record, and the Flutter/Kotlin final pick is that project's
   first decision, not this repo's.

## Consequences

- ADR-0013 item 8 is amended as above; the original text stands as
  historical context per the amendment convention.
- `cadmus-contract`'s serde shapes become a cross-language public
  surface: changes to them are reviewed as API changes, and a schema
  drift check (generated vs exported) joins CI when the export lands.
- The TUI's reference-renderer role is reaffirmed with a sharper duty:
  it is this repo's only live exerciser of the interactive protocol
  (approvals, steering) until the GUI matures. ADR-0022 demotes its
  scope, not this duty.
- The client state machine is implemented twice (Rust TUI, foreign
  GUI) — bounded by item 3's corpus, with item 4's escape hatch
  recorded so any revisit is evidence-driven.
- Consumes the tech re-anchoring remainder of the 2026-09-11 UI/UX
  survey open item: the GPUI path is abandoned and its licensing
  question (ADR-0013's GPL claim vs the survey's Apache-2.0 finding)
  is moot; the survey's orchestration/review patterns stay as design
  input for the GUI repo's kickoff.
