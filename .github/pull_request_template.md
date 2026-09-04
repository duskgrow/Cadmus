<!--
PR title = the squash-merge commit header: `type(scope): subject` — lowercase
type/scope, pure-ASCII English subject, header ≤ 100 chars (CI: the
`check-commit` subcommand of crates/xtask).

Everything below this comment becomes the squash-merge commit BODY verbatim —
the whole body lands in git history (the dialog needs zero edits), so write it
commit-ready and DELETE THIS COMMENT before opening:

- Summary: what & why in one or two sentences; any language (the title is
  English-only). Plain prose — no `##` headings, they read badly in `git log`;
  a prose paragraph runs at most 7 lines, then split it or use a list (CI).
- No process scaffolding: CI rejects task lists (`- [ ]`) and bare `---`
  lines. The pre-merge checks live in the `pr-preflight` skill, not here.
- Footers (optional), one per line, the block preceded by a blank line:
    Closes #N           — links the issue; GitHub auto-closes it on merge
    BREAKING CHANGE: …  — semver-MAJOR signal (alternative to `!` in the title)
  GitHub itself appends `Co-authored-by:` for multi-author PRs; tooling
  attribution trailers stay banned (AGENTS.md NEVER).
- AI-assisted? Apply the `ai-assisted` label — disclosure rides the PR
  timeline, not the commit message.

PR 标题 = squash 合并的 commit header（遵循上述规范，CI 检查）。本注释以下
的正文会原样成为 commit body（整段入历史，合并对话框零编辑）：一两句
what & why（可中文，标题必须纯英文）；不写 `##` 标题；叙述段落不超过 7
行；不写任务列表（`- [ ]`）和 `---` 分割线（CI 会拒绝）。页脚可选
（`Closes #N` / `BREAKING CHANGE: …`，块前空一行）。提交前删除本注释——
CI 拒绝 HTML 注释。AI 参与？给 PR 打 `ai-assisted` 标签即可。
-->
