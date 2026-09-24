# 0020. Fleet topology: discovery-layer mesh over direct P2P connections

- Status: accepted
- Date: 2026-09-23

## Context

Maintainer direction (2026-09-23): servers deploy on multiple machines
connected in a designed topology. The topology is **mesh, not star** —
connecting to one machine must suffice to reach all machines — chosen
for robustness; the connectivity is a low-level abstraction, invisible
in use. Cross-machine _task_ collaboration is neither committed to nor
foreclosed.

Existing decisions bound the space. ADR-0015 item 1 licenses the
single-user fleet projection "generalized across nodes by phase 5" and,
with ADR-0002, excludes cross-node task division. The roadmap's phase 5
planned "CF Tunnel → iroh" as the remote-transport evolution; the
report specifies Ed25519 whitelist authentication for remote attach.

Two failure shapes informed the mesh choice. A hub or proxy topology
(star) makes the hub a single point for every session routed through it
and taxes each interaction with a forwarding hop. A pure client-side
multi-connection (the client independently configured with every
server) answers robustness but fails "connect to one, reach all":
enrollment and discovery become per-client toil.

## Decision

1. **Mesh at the membership layer; direct connections on the data
   plane.** Each server holds a _roster_ — the fleet's peer list (node
   identity = Ed25519 public key, addressing, capabilities), every
   entry signed by the user's fleet key. A client connecting to any
   server receives the roster, then establishes its own direct
   connection to each peer (iroh: QUIC with public-key addressing and
   NAT hole-punching). Servers never proxy or forward session traffic
   for each other: a dying server kills only its own runs, and no
   interaction pays a relay hop it did not choose.
2. **Robustness is connectivity robustness, not run failover.** A run
   lives single-writer on one server (ADR-0002); if that machine dies,
   the run resumes from that machine's log — it does not migrate.
   Cross-machine session migration is recorded as unsolved: the trace
   is portable (a JSONL copy), the workspace state (the repo,
   uncommitted work) is the hard part. A future need files its own
   ADR.
3. **The swarm exclusion is unchanged.** Membership exchange is
   transport, not collaboration: servers share presence, never task
   division, work delegation or swarm logic — ADR-0002 and ADR-0015
   item 1 stand as written. Reopening requires a future ADR with field
   motivation.
4. **CF Tunnel is skipped; the transport goes straight to iroh.** The
   mesh requirement collapses the roadmap's "CF Tunnel → iroh"
   evolution into its endpoint. Ed25519 node identity is native to
   iroh's addressing, so the report's whitelist authentication becomes
   roster signing: enrollment is the fleet key signing a new peer
   record. Where hole-punching fails, iroh's relay fallback carries
   traffic; whether the public relays suffice or the fleet self-hosts
   one is an implementation-time measurement, not a prediction.
5. **Single-user scale keeps the mechanism plain.** A fleet is a
   handful of machines: roster sync on connect suffices — no gossip
   protocol, no consensus, no DHT. Any of those enters only with field
   evidence that roster sync hurts.
6. **Transparency lands in the client library.** "Connect to one,
   reach all" is invisible in use because the client owns roster sync,
   connection management and reconnection; the fleet view merges N
   servers' event-stream projections (ADR-0015 item 2). The client
   protocol (ADR-0013) is untouched — the mesh is a transport concern
   beneath it. Both client implementations (the in-repo TUI, the
   foreign GUI per ADR-0019) implement this layer; the roster's
   serialized format joins the wire schema's artifact family so the
   two read it identically.

## Consequences

- Roadmap phase 5's goal line changes (mesh; no CF Tunnel stage); the
  capability track's remote-attach row is unchanged in substance —
  same protocol, new transport.
- Key management becomes a real UX surface at implementation time:
  fleet key custody, roster signing, revocation. Normative formats land
  with the implementation (ADR-0013 item 10's precision discipline),
  not here.
- The mesh adds exactly one server-to-server behavior (roster
  exchange); no run state crosses, so ADR-0002's single-writer
  invariant and audit triggers are unaffected.
- iroh's dependency admission goes through the `adding-dependencies`
  skill when the transport lands; the relay fallback (item 4) is the
  one external infrastructure dependency this topology accepts.
- Cross-machine session migration (item 2) is the recorded open
  question of this ADR — its consumer, if one ever fires, is a field
  need, not a phase.
