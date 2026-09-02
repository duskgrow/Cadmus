# 0004. Defer rmcp adoption until an external MCP server is actually needed

- Status: accepted
- Date: 2026-09-02

## Context

The research report adopted rmcp as the tool protocol layer (§4.1.3) and
listed "rmcp 工具协议接入" among the phase-0 deliverables (§10.2.1). At phase-0
close-out, all three acceptance criteria are met without it — provider
contract suite green, wire-level replay coverage of the two required pitfalls,
three conversation snapshots — and the roadmap's phase-0 goal line ("chat +
coding tools + two providers validating the dialect seam") is fully served by
the built-in read-only toolset.

The seam rmcp would validate already exists and is exercised: `AgentTool` in
`cadmus-core` is the single tool port, currently proven by the built-in tools,
test fakes, and the hallucinated-tool recovery path. rmcp would add a
sizeable dependency tree to re-prove a seam that is already green, against
zero acceptance criteria.

## Decision

Phase 0 closes without rmcp. `AgentTool` remains the tool seam; when adoption
triggers, external MCP servers are wrapped into `AgentTool` at the wiring
layer (and/or cadmus tools are exposed as an MCP server), following the
`adding-dependencies` skill.

Adoption triggers — revisit when any fires:

1. The agent needs to consume an existing external MCP server.
2. Cadmus tools should be exposed to other agents as an MCP server.
3. On adoption, re-verify per the report's freshness policy (§1.2.2): current
   rmcp version and MCP spec version (§4.1.3 cited rmcp 3.1.4 / MCP
   2026-07-28 at the 2026-08-29 baseline).

## Consequences

- Phase 0 ships a smaller dependency tree; this ADR records the deviation
  from the report's deliverable list (the report itself stays frozen).
- Tool integrations built before adoption lose nothing: they implement
  `AgentTool`, which any future rmcp wiring wraps rather than replaces.
