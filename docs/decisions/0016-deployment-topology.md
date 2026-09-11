# 0016. Deployment topology: one client protocol, three deployment modes — embedded, `serve`, service-managed daemon

- Status: accepted
- Date: 2026-09-11

## Context

ADR-0013 separated client and server logically (one client kind, N
frontends); today's `chat` simply plays both roles in one process. The
maintainer's 2026-09-11 topology discussion fixed the deployment shape,
choosing explicitness over magic at three points:

- **Explicit server over auto-spawn.** The tmux-style auto-spawn model
  was costed and rejected: leaked processes, spawn races, environment
  drift (the server inherits the first spawning client's env — stale
  provider keys), version skew with lingering old servers, first-start
  latency, and platform detach quirks (tmux itself never solved native
  Windows; we must). Instead: an explicit `serve` command, with residency
  delegated to the user's service manager.
- **Casual use must need zero setup** (maintainer): a user who never
  wants a background server just runs the client — no server found, the
  client is its own server and dies with the client — and the two modes
  must not fragment (same binary, protocol, data dir; switching is
  invisible because the log, not the process, is the SSOT).
- **Runs unified in one process** (maintainer's lean), with the
  concurrency and crash-isolation questions answered below.

Market references: OpenCode `serve` (the TUI is just a client), Codex
`app-server` (detachable TUI), Crush `serve` (workspace torn down on last
disconnect), herdr (background server; its experimental live handoff is
the cautionary upgrade tale), Superset #7426 (host service dying under
session load — daemon lifecycle is a real failure surface).

## Decision

1. **Three deployment modes, one protocol.**
   - _Embedded_: the client embeds the server in-process — today's model.
     Zero setup; casual use, headless `chat --json`, tests. Client exit
     interrupts any active run (recorded; resumable from the log).
   - _`cadmus serve`_: an explicit foreground server bound to the local
     socket — development, debugging, manual operation.
   - _Service-managed daemon_: the same `serve` under the user's service
     manager (`systemd --user`, launchd, a per-user scheduled task) —
     resident.
     All three share the binary, the socket path, the protocol and the
     data dir. A client connects to a running server if one answers, else
     runs embedded. There is no auto-spawn anywhere. One boundary is
     explicit: attaching a second client (or the GUI) to a session
     requires `serve` — embedded mode is single-client.
2. **Never root; one server per user per machine.** Every operation the
   server performs uses the user's own files, credentials and processes;
   Landlock (phase 3) is unprivileged by design. A root or multi-tenant
   deployment buys only the setuid/credential-broker complexity — it is
   excluded. Multi-user machine = one server per user, isolated by that
   user's runtime dir (0700) and data dir.
3. **Runs unified in one process.** The wiring layer runs a tokio
   multi-thread work-stealing runtime (workers ≈ cores; core stays
   runtime-free per ADR-0013, so the flavor is reversible). Runs are
   task trees; the workload is ~all IO-bound (provider streams, fs,
   subprocesses). Two disciplines: nothing blocks a worker thread (CPU
   chunks beyond ~1 ms go to `spawn_blocking`; incremental folds per the
   attach-payload open item); panic isolation is trusted at task
   boundaries (a panicking run dies alone, recorded errored — the others
   continue). Residual whole-process risks (OOM, abort-class bugs) are
   bounded by bounded buffers/windows and are always recoverable via the
   log. Escape hatch, documented not built: a process-per-run supervisor,
   triggered by field pain — a run repeatedly OOMing the daemon, or
   upgrade disruption (item 4) proving costly.
4. **Graceful upgrade = drain-and-resume.** On SIGTERM (service
   restart), the server enters draining: it rejects new runs with a
   draining error, interrupts active runs chunk-granularly (ADR-0013's
   interrupt semantics — turn boundaries, approval waits and chunk gaps
   all honor it) recording `reason=upgrade-drain`, flushes and exits 0,
   bounded by the service manager's stop timeout; on hard timeout it
   exits anyway (crash-only; the log is consistent). On start, the
   server auto-resumes exactly the drain-interrupted runs and touches
   nothing else. Load-bearing requirement (acceptance criterion of the
   `serve` work, not an assumption): a run interrupted at any chunk
   gap / turn boundary / approval wait resumes losslessly, with pending
   approvals re-armed. Upgrades are lazy and user-paced: a semver
   compatibility window in the client/server handshake lets same-major
   pairs keep working after the binary is replaced; outside the window
   the client fails loudly and names the restart command.
5. **Concurrency needs no daemon.** Embedded mode hosts multiple
   concurrent sessions in-process (task trees), so the phase-1 TUI
   session dashboard (ADR-0015 item 8) needs no server. The daemon
   becomes necessary only for sessions-outliving-clients, cross-client
   attach, and remote attach.
6. **Scheduling.** `serve` + the local socket land in phase 5 at the
   latest — earlier only if the GUI renderer work or a multi-client
   attach need fires first (the GUI ADR, scheduled after the
   daily-driver milestone, decides). Phase 5's daemon content is this
   topology; phase 5 proper adds the remote transport (CF Tunnel →
   iroh) and control plane. ADR-0012 item 4's CLI-first/socket-later
   stands as written — the socket arrives with `serve`.
7. **Security.** Local socket only, in the user's runtime dir (0700); no
   unauthenticated localhost TCP (any local process — including a
   browser — must not be able to drive runs). Remote authentication is
   phase 5's (Ed25519 whitelist per the report).
8. **Concurrent embedded clients: the trace lease and the foreign
   view.** Embedded clients share the config dir and the log store, so
   every client sees every trace on disk — durable events are appended
   synchronously (ADR-0013), so a live run's trace file grows visibly in
   other clients' session lists. One run still has a single writer
   (ADR-0002): the owning process holds an advisory lock (`flock`) per
   active trace, which the OS releases on process death — a crashed
   owner never leaves a stale lease. A client that does not hold the
   lease gets a read-only foreign view of the trace (replayed from disk,
   marked active-elsewhere, no live deltas — the owner's in-process
   broadcaster is unreachable by design) and may fork it (copy-on-read
   into a new trace id). Resuming an interrupted trace is taking its
   lease. Readers tolerate a torn trailing line (append-only JSONL
   discipline). Two terminals steering one live session is exactly the
   need that fires `serve` (item 1's explicit boundary).

## Consequences

- The auto-spawn cost list is rejected with the tmux model; the price is
  one explicit step (`serve`, optionally under a service manager) for
  residents, and casual use stays zero-setup but single-client.
- The contract's semver surface becomes load-bearing on the wire, as it
  already is for library consumers (ADR-0013).
- Drain-and-resume makes resume-from-log at arbitrary interrupt points a
  hard acceptance test of the `serve` work (interrupt at chunk gap /
  turn boundary / approval wait → lossless resume, pending approval
  re-armed).
- The process-per-run escape hatch records its two triggers here (OOM
  pressure; upgrade disruption) so the revisit is evidence-driven, not
  speculative.
- No roadmap change: the phase-1 TUI dashboard needs no server (item 5);
  `serve` lands with phase 5 or an earlier second-process need (item 6).
- Herdr's live-handoff contortions are the counter-example this topology
  avoids: because we own the trajectory, interrupt-and-replay is
  lossless; no state handoff between processes is ever needed.
- The trace lease (item 8) is the single-writer invariant's filesystem
  realization; it makes cross-terminal session handoff (close terminal
  A, resume in terminal B) lossless without any server.
