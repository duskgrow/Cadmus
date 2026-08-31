---
name: code-simplification
description: Use when asked to simplify or clean up code, or after a feature lands and before merging. Finds duplication, dead code, speculative abstraction and oversized modules, and removes them without changing behavior — always as its own change, never mixed with feature work.
---

# Code simplification

The hard boundary: **behavior-preserving, and always its own change**. Never
mix simplification with feature work in one PR/commit — mixed diffs are
unreviewable and bisect-hostile.

## Hunt list (in rough priority order)

- **Duplication** — three similar blocks → extract once, at the lowest shared
  layer. Two similar blocks: leave them; the wrong abstraction costs more than
  duplication.
- **Dead code** — unused `pub` items, unreachable branches, features nobody
  enables, tests of deleted behavior. Untested *and* unreachable code is a
  deletion candidate, not a test-writing candidate.
- **Speculative abstraction** — a trait with one implementation, a helper used
  exactly once, a parameter "for future flexibility". Inline or delete; add
  the abstraction when the second caller arrives.
- **Oversized modules** — a file past ~500 LoC (excluding tests) usually hides
  two modules; split along the seam that keeps invariants next to the code
  that owns them, and move the tests with the code.
- **Error-type sprawl** — variants no caller distinguishes merge into one;
  callers that only report get `miette::Report`, not a wider enum (see
  AGENTS.md style rules).
- **Unused dependency features** — re-check `default-features` and feature
  lists after deletions.

## Method

1. Map before cutting: `cargo metadata` / the crate layout; identify the
   module that owns each invariant.
2. One mechanical step at a time; after each: `cargo check -p <crate>` and the
   crate's tests.
3. Finish with `just ci`. Tests passing before and after is the safety
   argument — if a move needs test changes beyond paths, stop and reconsider.

## Report

List what was removed/simplified and the evidence it is safe (which tests pin
the behavior). If a suspected-dead item turns out load-bearing, that discovery
goes in the report too.
