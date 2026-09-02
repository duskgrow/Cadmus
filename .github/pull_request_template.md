<!--
PR title = the squash-merge commit header: `type(scope): subject` — lowercase
type/scope, pure-ASCII English subject, header ≤ 100 chars (CI: the
`check-commit` subcommand of crates/xtask).

Everything between this comment and the `---` cut line becomes the squash-merge
commit BODY verbatim — write it commit-ready, then DELETE THIS COMMENT before
opening (CI rejects HTML comments in the message):

- Summary: what & why in one or two sentences; any language (the title is
  English-only). Plain prose — no `##` headings, they read badly in `git log`.
- Footers (optional), one per line, the block preceded by a blank line:
    Closes #N           — links the issue; GitHub auto-closes it on merge
    BREAKING CHANGE: …  — semver-MAJOR signal (alternative to `!` in the title)
  GitHub itself appends `Co-authored-by:` for multi-author PRs; tooling
  attribution trailers stay banned (AGENTS.md NEVER).
- Below `---` is process scaffolding: it stays while the PR is open; the
  merger deletes from `---` down in the squash-merge dialog.

PR 标题 = squash 合并的 commit header（遵循上述规范，CI 检查）。本注释与 `---`
之间的正文会原样成为 commit body：一两句 what & why（可中文，标题必须纯英文）；
不写 `##` 标题；页脚可选（`Closes #N` / `BREAKING CHANGE: …`，块前空一行）。
提交前删除本注释——CI 拒绝 HTML 注释。`---` 以下由合并者在合并对话框中删除。
-->

---

## Checklist

- [ ] PR title follows the commit convention — it becomes the squash-merge commit header (CI checks title + body)
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
