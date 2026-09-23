# 0021. Platform convergence: Linux + macOS server; `serve` pulled forward

- Status: accepted
- Date: 2026-09-23

## Context

Maintainer direction (2026-09-23): the server supports Linux and macOS
only; native Windows support is dropped. The primary user surface moves
to a foreign-language GUI (ADR-0019) that runs anywhere — including
Windows — and attaches to servers remotely, so users on Windows are
served by the topology rather than by a Windows port.

Today's footprint: CI tests ubuntu/macos/windows (the windows job runs
a rustup-native route because Nix has no native Windows support); dist
ships Windows targets; AGENTS.md routes native Windows development
through WSL2; the codebase carries Windows portability shims (paths,
`cfg(windows)`, flock and signal semantics). Two standing plans had
Windows costs baked in: ADR-0016's context names "platform detach
quirks (tmux itself never solved native Windows; we must)" as a
constraint on the daemon model, and phase 3's sandbox is Landlock —
Linux-only — which would have forced a third backend or a documented
gap on Windows.

Also in this decision's blast radius: ADR-0016 item 6 scheduled
`serve` + the local socket "in phase 5 at the latest — earlier only if
the GUI renderer work or a multi-client attach need fires first". The
maintainer's 2026-09-23 direction (GUI-primary, multi-machine fleet) is
that trigger firing.

## Decision

1. **Server and all in-repo crates: Linux + macOS only.** The
   mechanical cut lands as its own PR immediately after this batch:
   remove the `test-windows` CI job and the Windows dist targets,
   simplify `cfg(windows)` paths and portability shims, and rewrite
   AGENTS.md's WSL2 clause plus any contributor-doc Windows references
   in that same PR (docs co-evolve with the behavior change). Code
   that incidentally still compiles on Windows is unsupported, not
   maintained. WSL2-hosted use is unaffected — WSL2 is Linux.
2. **Windows users are served by the topology, not the port.** The
   foreign GUI (ADR-0019) runs natively on Windows and attaches over
   the fleet transport (ADR-0020). There is no supported way to run a
   server, the TUI or `chat` on native Windows.
3. **Phase 3's sandbox is two backends, not three:** Landlock (Linux)
   - Seatbelt (macOS). The Windows sandbox question dissolves rather
     than being answered.
4. **`serve` pulls forward, amending ADR-0016 item 6.** Landing order:
   phase-1 closeout (ADR-0022) → the Windows cut (item 1) →
   _phase-5-lite_: `serve` + local socket + wire hardening (ADR-0019's
   schema export and conformance corpus) → the GUI spike → the iroh
   fleet transport (ADR-0020) → phase 2 (self-evolution), with the
   fleet orchestration surface growing in the GUI as it matures.
   ADR-0016 item 4's drain-and-resume acceptance becomes load-bearing
   at the `serve` landing, which makes resume/fork (ADR-0022's
   closeout set) its hard prerequisite.
5. **The daemon model keeps its Unix shape.** Service-manager
   residency is `systemd --user` / launchd (ADR-0016 item 1); the
   per-user scheduled task reference and the Windows detach constraint
   in ADR-0016's context are removed.

## Consequences

- CI loses a job and a special-case toolchain route; `just ci`'s
  local ≡ CI equivalence is unchanged for the remaining platforms.
- The release matrix shrinks in the cut PR (dist-workspace.toml and
  the generated release.yml — the drift check keeps them honest).
- ADR-0016 items 1 and 6 are amended as above; the original text
  stands as historical context per the amendment convention.
- The excluded population is native-Windows local users — near zero
  for a pre-release, single-maintainer project that already develops
  through WSL2 — and the accepted cost is that no field evidence from
  that population can ever fire a revisit. A revisit would need a
  Windows-native server demand that the remote-GUI story cannot
  answer; none is anticipated.
