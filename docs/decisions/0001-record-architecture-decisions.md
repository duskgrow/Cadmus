# 0001. Record architecture decisions with ADRs

- Status: accepted
- Date: 2026-08-31

## Context

The hardest thing to track over a project's lifetime is the *motivation* behind
decisions; rationale scattered across PR descriptions, chat and wikis rots or
contradicts itself.

## Decision

Record architecturally significant decisions as lightweight MADR files in
`docs/decisions/`, named `NNNN-title.md`, sequentially numbered, numbers never
reused. A superseded decision is marked `superseded by ADR-NNNN`, never deleted.
Code comments, docs and PR descriptions reference the number ("see ADR-0007")
instead of restating the rationale.

## Consequences

Each decision's rationale has exactly one authoritative location; a new decision
is a new file, so there is no merge-conflict hot spot. ADRs record "why we
decided this back then"; current-state descriptions belong to architecture docs
and code.
