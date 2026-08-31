<!-- PR title = the squash-merge commit header. It MUST follow the commit convention:
       type(scope): subject
     lowercase type/scope, pure-ASCII English subject, header ≤ 100 chars.
     CI checks it (the check-commit subcommand of crates/xtask).

     PR 标题 = squash 合并后的 commit header，必须遵循上述提交规范（CI 会检查）。 -->

## Summary

<!-- What & why, in one or two sentences. Link the issue if there is one.
     The body may be Chinese; the title may not. / 正文可中文，标题不行。 -->

## Checklist

- [ ] PR title follows the commit convention — it becomes the squash-merge commit message (CI checks it)
- [ ] `just ci` is green locally (CI runs the exact same gate)
- [ ] Docs updated in the same PR when behavior or commands changed (English canonical; `*.zh-CN.md` translations follow)
- [ ] Snapshots reviewed with `just snapshot-review` and committed — CI is read-only
- [ ] New dependencies: went through the `adding-dependencies` skill; version declared only in the root `[workspace.dependencies]`; `just deny` green
- [ ] Architecture-significant change? Added an ADR under `docs/decisions/`
- [ ] Agent-facing docs (`AGENTS.md`, `.agents/skills/`) touched? `just agent-check` passes, and the change rides in this PR (co-evolution)

---

##### Was generative AI tooling used to co-author this PR?

- [ ] No
- [ ] Yes — <tool name and version>; the `pr-preflight` skill was run (or the diff was reviewed against it)
