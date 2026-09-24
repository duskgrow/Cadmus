# 0024. Session trace model: one growing trace per conversation, continuation as a command

- Status: proposed
- Date: 2026-09-24

## Context

The TUI mints a **new trace per prompt**: `TuiDriver::start` mints a fresh
`trace_id` per call, and the app seeds each run with the client-side
`history` clone plus the new prompt, so every trace's `StartRun` embeds the
conversation's entire prior messages (and the frozen `PrefixRecord`). This
was the shortest path from one-shot `chat` to an interactive session — the
loop, the telemetry and the log were all shaped "one run = one trace".

The costs are now structural:

- **Storage grows quadratically** with conversation length (turn K's file
  duplicates turns 1..K−1's messages), and file count grows linearly —
  one JSONL plus one artifacts dir per prompt.
- **No lineage links between a session's traces**: resume/fork (the
  phase-1 closeout set, ADR-0022) cannot tell which N traces form one
  conversation; a session picker would show one row per prompt.
- The model fights ADR-0009's assumption ("resume = replay the event
  prefix; fork = copy the prefix with lineage in `start_run` attributes"),
  which presumes a session is **one growing trace**.

The 2026-09-24 survey
(`docs/research/2026-09-24-agent-session-storage-survey.md`) checked six
mainstream agent CLIs: **all keep one persistent store per conversation
and append new events in place; none mint per-turn files embedding full
history.** The closest precedent for our current shape was Gemini CLI's
old whole-document rewrite — which they migrated away from, to an
append-only per-session JSONL. Codex CLI is the most isomorphic evidence:
append-only JSONL rollout per thread as SSOT, SQLite only as a rebuildable
index (our ADR-0005 layering), resume = append to the same file, fork =
lineage metadata on the child, compaction = marker + embedded snapshot
with the pre-compaction log untouched.

The gap in today's loop is small and precise: at the finish line (no tool
calls, queue empty) the loop already polls the command channel and already
continues the run when a queued steer is present ("the user's one more
thing"); only when nothing is queued does it record the terminal event and
exit. A session model is the generalization of that finish line.

## Decision

1. **One conversation session = one growing trace.** `StartRun` opens a
   session trace exactly once; subsequent prompts append their turns to
   the same file. The trace remains self-sufficient (ADR-0005's fold
   invariant): replaying it rebuilds the full session history without any
   other file. The per-turn `history` embedding disappears — the trace
   _is_ the history, and the TUI's client-side `history` re-seeding dies
   with it. One-shot `chat` and `eval` are unaffected: a single-turn
   session degenerates to today's shape, which stays correct there.

2. **Continuation is a contract command.** `Command::Continue { command_id,
   text }` joins ADR-0013's command vocabulary: user text entering a
   _parked_ session, appended as a user message at the next request
   boundary — the same semantics as `Steer` mid-run, at the finish line.
   Record-on-effect stands: the recorded command is the application, so a
   replayed trace shows exactly the continuations that happened, with the
   client's idempotency keys.

3. **The loop parks at the finish line for interactive sessions; it never
   parks unattended.** When the run would finish, an interactive command
   source parks the loop (approvals settled, partial state flushed,
   terminal event recorded) and waits for `Continue` or the channel's
   close. One-shot and eval runs keep today's behavior — an unattended
   source ends the run at the finish line, so Blackhole-driven runs can
   never hang. Parking vs. re-entering via replay is an implementation
   choice **behind** the same invariant: the recorded event stream must be
   identical either way, and event/sequence ids continue monotonically
   across continuations (Codex's ordinal scan is the reference).

4. **Cross-process resume replays the prefix and appends to the same
   trace** (ADR-0009 item 4's consumer lands): no second `StartRun`, the
   replayer rebuilds working state, new events append with continued
   ordinals. Fork keeps ADR-0009's shape — prefix copied under a new trace
   id with lineage in `start_run` attributes; Codex's `forked_from_id` +
   ordinal metadata validates the approach, and its zero-copy
   referenced-prefix variant stays a future optimization, not v1.

5. **`/clear` ends the session trace.** The next prompt mints a new trace
   (the 2026-09-24 semantics: new conversation, new session scope for
   `/usage`). No lineage is recorded — a clear shares no prefix with its
   predecessor; the old trace remains in the log untouched.

6. **Compaction, when it lands, follows the mainstream marker pattern**:
   a compaction record carrying the replacement snapshot is appended, the
   pre-compaction log is never rewritten, and replay treats the newest
   complete compaction as its boundary (Codex `Compacted`, Gemini
   `$set:{messages}`, Claude Code `summary`+`compact_boundary` — all three
   convergent). Today's mechanical compaction already leaves the log
   untouched; this item binds the phase-2 LLM archival compaction
   (ADR-0007) to the same shape.

7. **Checkpoint/rewind stays ADR-0022's sibling closeout item**; this
   model fixes only its frame. Three constraints the rewind change
   inherits:
   - **Conversation rewind is the fork primitive, not a new mechanism.**
     Rewind-to-turn-N = copy the trace prefix at that boundary under a
     new trace id with lineage (item 4), then switch the session pointer.
     Edit-and-fork and the explicit user fork are the same primitive's
     other entry points — one mechanism, three triggers. A Gemini-style
     rewind marker (bytes kept, replay truncates) is the alternative;
     marker vs. fork-copy is the rewind change's own call, but fork-copy
     reuses machinery the closeout builds anyway. v1 rewinds only parked
     sessions — interrupt first, never rewind a running turn.
   - **Code rewind needs no shadow git under the phase-1 tool surface.**
     `write_file`/`edit_file` are the only workspace mutators and name
     their paths at call time, so a copy-based store (pre-write bytes
     per prompt, bounded retention) is _complete_ — the approval seam
     already sees every mutation. ADR-0011's 2026-09-11 amendment chose
     a shadow git repo on the Gemini precedent; the survey adds a
     counter-signal — Codex _removed_ its ghost-commit checkpoints in
     favor of revert files (reason unverified; the maintenance class —
     user-tree edge cases, nested repos, git-config leakage — is
     documented in Gemini CLI's own sanitization code). ADR-0011's store
     mechanism is re-opened for the rewind change to re-decide with this
     evidence.
   - **The sandbox is the phase-3 convergence point, not a phase-1
     shortcut.** Once `shell_exec` exists, path tracking stops being
     complete (a shell command can touch anything); an overlay/CoW
     filesystem inside the sandbox snapshots everything for free and is
     the honest phase-3 checkpoint mechanism. The checkpoint store is
     therefore a port — file-copy impl now, overlay impl when the sandbox
     lands — and pulling the sandbox itself into phase 1 is rejected: it
     would import phase-3 platform risk (Landlock/Seatbelt, WSL2) into
     the closeout to solve a problem the closed tool surface does not
     have.

**Amendments.** ADR-0005's "one file per trace" is clarified: the trace
unit is the conversation session, not the provider run — for interactive
sessions the two used to coincide 1:1 per prompt and no longer do.
ADR-0009 item 4's mechanism (replay-to-resume, lineage-on-fork) is
unchanged; this ADR lands its consumer. ADR-0011's shadow-git checkpoint
mechanism is re-opened (item 7); its rewind algebra is narrowed for v1
to restore/edit-fork/re-decide on parked sessions, with summarize
waiting on phase-2 compaction. ADR-0013's command vocabulary
grows `Continue`; rewind remains contract-future until item 7's change.

## Consequences

- Storage per session becomes the sum of its turn events (plus future
  compaction snapshots) instead of an O(N²) re-embedding; file count per
  session drops from turns to one (plus the artifacts dir). The day shard
  layout and read-root tiering are unchanged and sufficient.
- The TUI's driver gains a session lifetime: `start` once per session,
  `Continue` per prompt; the app's idle detection keys on the loop's
  parked signal (a live item) instead of run termination. The client-side
  `history` field loses its seeding role.
- The fold must tolerate a terminal record followed by a continuation
  within one trace; acceptance pins it: a multi-continuation trace replays
  to the live session history (insta-locked), and two exports of the same
  trace stay byte-identical (ADR-0005's gate).
- Existing per-turn traces remain readable and replayable — no migration;
  they are simply sessions that happen to be one prompt long.
- The `/usage` session scope (the conversation between `/clear`s) becomes
  one file's fold, and the log-derived all-time view (open item) becomes a
  scan over session traces rather than over per-turn fragments.
- The session picker (resume UX) inherits a sane unit: one row per
  conversation, ordered by the trace's own timestamps.
- Rewind's mechanism count shrinks instead of growing: conversation
  rewind, edit-and-fork and user fork share one primitive (item 7), and
  code rewind is a small file-copy store riding the write-tool seam. The
  remaining test matrix is bounded by the parked-sessions-only rule.
- Status is `proposed` until the maintainer accepts; on acceptance the
  "Session storage model survey (2026-09-24)" open item is consumed and
  deleted in the same change.
