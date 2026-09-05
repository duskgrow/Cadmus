# CONTRIBUTING

[English](./CONTRIBUTING.md)（如有出入以英文版为准）

## 环境准备

```bash
direnv allow   # 或：nix develop（进入 devShell 会自动安装 git 钩子）
just ci        # 验证全绿
```

git 钩子（pre-commit + commit-msg）在进入 devShell 时自动安装——git 的安全模型不允许钩子随 clone 分发，非 Nix 环境下请手动跑一次 `just setup`（`prek uninstall` 可移除）。

全部日常命令见 `just --list`（活文档，随仓库演化）；核心入口：`just fmt` / `just lint` / `just test` / `just ci`。

## 合并策略与提交信息

合并策略为 **squash merge only**（在仓库设置中强制——见「仓库设置」）。main 的历史一线一义；PR 标题 + 正文会成为落地 commit 的 message——所以 **PR 标题必须遵循提交规范**，CI 会检查。合并流程：网页上评审 → 检查全绿 → 合并。squash 对话框会预填 PR 标题 + 正文，**无需任何编辑**——整个正文按构造即可落地。

提交规范 —— 修改版 Conventional Commits：

    type(scope)!: subject

- `type`：`feat fix docs style refactor perf test build ci chore revert` 之一（小写）
- `scope`：可选，小写（crate 名、`cli` 等）；`!` 标记破坏性变更（或 footer `BREAKING CHANGE:`）。scope 同时是 changelog 的模块小节名（release notes 按 scope 分组，Zed 风格），建议填写
- `subject`：纯英文 ASCII；整个 header 不超过 100 字符
- body：可中文、不限行宽；与 header 之间空一行。整个正文会原样入历史，因此必须整体可落地：不写 HTML 注释（模板的引导注释要在打开前删掉，不能合进历史）、不写任务列表（`- [ ]`）、不写单独的 `---` 行——流程脚手架一律拒绝（check-commit 强制）。保持可读：叙述段落最多 7 行——更长就分段或用列表（列表项、引用块、代码块不计）
- footer（`TOKEN: value` / `TOKEN #value`）：整个块前留空行。推荐 token：`Closes #N`（合并时 GitHub 自动关闭对应 issue）与 `BREAKING CHANGE:`（semver MAJOR 信号）。多作者 PR 的 `Co-authored-by:` 由 GitHub 自动追加；工具署名类 trailer 依旧禁止（见 AGENTS.md）

语义化版本映射：`fix` → PATCH，`feat` → MINOR，`!` → MAJOR。版本号推导与 CHANGELOG 都以提交信息为输入——type 写错等于版本发错。

强制点（SSOT：`crates/xtask` 的 `check-commit` 子命令）：

- 本地 commit-msg 钩子（进入 devShell 时自动安装；非 Nix 环境用 `just setup`）；
- CI 在每个 PR 上检查 PR 标题 + 正文。

注意：

- GitHub 会给 squash 合并的标题自动追加 ` (#NNN)`——预期行为，保留即可。
- `git revert` 默认的 `Revert "…"` 头不符合规范——改写成 `revert: <什么>`。
- PR 模板只有一区：HTML 引导注释以下的内容会原样成为 commit body（打开前删掉注释——CI 会拒绝）。AI 辅助通过 `ai-assisted` 标签披露，不写进提交信息。

### PR 门禁：密钥扫描

- **密钥扫描**：对 PR diff 的新增行做模式匹配（token / 私钥 / `.env` 赋值），命中即硬失败；命中的行会直接标注在 PR 的 Files 页上。泄露的密钥需要轮换，不是删掉就行。文档中的*示例*密钥可以在该行带 `pr-guard:allow` 标记（评审中可见）。同一扫描也作为本地 pre-commit 钩子运行在暂存 diff 上（SSOT：`crates/xtask` 的 `pr-guard` 子命令）。
- `ai-assisted`：生成式 AI 共同编写的 PR 的披露标签（已运行 `pr-preflight` skill，或对照它评审过 diff）。纯元数据，不是门禁——没有机器强制。

## 添加依赖

1. 版本号写进根 `Cargo.toml` 的 `[workspace.dependencies]`（全仓唯一位置）；
2. 成员 crate 用 `dep.workspace = true` 继承，只允许追加 `features` / `optional`；
3. `just deny` 会通过许可证与来源检查；新许可证需要先在 PR 中讨论再扩 `deny.toml` 白名单。

## 添加 crate

```bash
just new-crate <name>
```

默认创建**内部 crate**（`version = "0.0.0"`, `publish = false`），不承担 semver 负担。若某个 crate 要对外发布：改为 `version.workspace = true`、去掉 `publish = false`，并在 `release-plz.toml` 登记 `[[package]]`。注意：被发布 crate 依赖的 path 依赖必须带版本号（cargo publish 的硬性要求）。

## 测试与快照

- 测试写在能捕获该 bug 的最低层；CLI 行为走 `tests/`（assert_cmd），纯逻辑走单元测试。
- 大输出断言（help 文本、诊断、序列化格式）用 insta 快照：快照入库评审，CI 只读；时间戳/路径/UUID 等不确定字段必须先 filter 再快照。
- 更新快照：`just snapshot-review`，逐条看 diff 再批准。

## 发布

日常什么都不用做。release-plz 会持续维护一个 Release PR（版本号 + CHANGELOG + semver 检查结论）；**合并它**即触发：

1. 推 tag `vX.Y.Z`；
2. tag 触发 cargo-dist：四平台构建 → GitHub Release（含安装脚本、checksum、attestation），Release 正文渲染该版本的按模块分组 changelog 小节。

crates.io 发布是**可选启用**：项目默认运行在 release-plz 的 `git_only` 模式（版本号来自 git tag，不接触任何 cargo registry），在你明确启用之前不会有任何东西发到 crates.io——见下文「启用 crates.io 发布」。

changelog 由提交信息生成并按模块分组：commit 的 scope（`feat(cli): …`）即小节名——写好带 scope 的提交，发布说明就免费得到了。

不想发版就不合并——Release PR 会自动累积更新。

### 一次性设置（Release PR 需要）

**GitHub**：Settings → Actions → General → Workflow permissions 设 "Read and write"，勾选 "Allow GitHub Actions to create and approve pull requests"——否则 Release PR 无法创建。

### 启用 crates.io 发布

1. 在 `release-plz.toml` 中删掉 workspace 级的 `git_only = true` / `publish = false` 两行；在 `.github/workflows/release-plz.yml` 中给 release job 的 permissions 加 `id-token: write`（OIDC Trusted Publishing——无长期 token）。
2. **crates.io 首发**：Trusted Publishing 只能绑定已存在的 crate，第一次需手动：`cargo publish`（本地 token 用完即可吊销）。
3. **crates.io TP 注册**：crate → Settings → Trusted Publishing → 添加 GitHub 仓库与 workflow 文件名 `release-plz.yml`。之后在 crates.io 开启 "Trusted Publishing only" 可彻底禁用 token 发布。
4. 校验：`git tag` 无手工标签遗留；secrets 里没有任何 crates.io token。

## CI 地图

| job / workflow                 | 作用                                                                                   |
| ------------------------------ | -------------------------------------------------------------------------------------- |
| `ci.yml` → quality-gate        | `prek run --all-files`，与本地 git 钩子同源                                            |
| `ci.yml` → test (ubuntu/macos) | `nix develop -c just ci`                                                               |
| `ci.yml` → test (windows)      | rustup 原生路线，消费同一 `rust-toolchain.toml`                                        |
| `commits.yml`                  | 对 PR 标题 + 正文做提交规范检查（即 squash 合并后的 commit message）；改标题会自动重跑 |
| `pr-guard.yml`                 | PR diff 密钥扫描——硬失败，命中行直接标注在 PR 的 Files 页上                            |
| `ci.yml` → dist-drift          | release.yml 生成物与 `dist-workspace.toml` 的一致性                                    |
| `release-plz.yml`              | Release PR + tag（crates.io 发布为可选启用）                                           |
| `release.yml`（dist 生成）     | tag 触发跨平台构建与 GitHub Release；PR 上跑 `dist plan`                               |
| `flake-update.yml`             | 每周 flake.lock 升级 PR                                                                |
| `toolchain-update.yml`         | 每周 rust-toolchain.toml 升级 PR（由 job 内 `just ci` 验证）                           |

## 仓库设置（一次性，GitHub 侧）

这些项在 GitHub 设置里而非代码中，建仓库时设一次：

- General → Pull Requests：**仅 squash merge**（禁用 merge commit 与 rebase merge）；squash 提交信息选 "Default to pull request title and description"；开启 "Automatically delete head branches"。
- Branches → 保护 `main`：要求经 PR 合入；要求状态检查 `quality gate (prek)`、`test (ubuntu-latest)`、`test (macos-latest)`、`test (windows)`、`conventional commits`、`pr guard`、`release.yml drift check`；要求分支保持最新。
- Actions → General：按上文「发布」设置 workflow 权限。

同样的设置也有一份一次性 `gh` 命令块（在仓库目录内执行，需先用 `gh auth login` 登录你本人的账号——`{owner}`/`{repo}` 会从 remote 自动解析）。刻意写成文档里的命令块而不是随仓库分发的脚本：它每个仓库只跑一次、需要你本人的管理员凭据，而随仓库分发的一次性脚本只会悄悄腐烂。

```bash
gh repo edit --enable-squash-merge --enable-merge-commit=false --enable-rebase-merge=false --delete-branch-on-merge --squash-merge-commit-message=pr-title-description
gh api repos/{owner}/{repo}/actions/permissions/workflow -X PUT -F default_workflow_permissions=write -F can_approve_pull_request_reviews=true
gh api repos/{owner}/{repo}/branches/main/protection -X PUT -F 'required_status_checks[strict]=true' -F 'required_status_checks[contexts][]=quality gate (prek)' -F 'required_status_checks[contexts][]=test (ubuntu-latest)' -F 'required_status_checks[contexts][]=test (macos-latest)' -F 'required_status_checks[contexts][]=test (windows)' -F 'required_status_checks[contexts][]=conventional commits' -F 'required_status_checks[contexts][]=pr guard' -F 'required_status_checks[contexts][]=release.yml drift check' -F 'required_pull_request_reviews[required_approving_review_count]=0' -F enforce_admins=null -F restrictions=null
gh label create ai-assisted --color 8250df --description "Generative AI co-authored this PR; the pr-preflight judgment pass was run"
```

设置在 git 之外，可能漂移——校验命令：

```bash
gh api repos/{owner}/{repo} --jq '{allow_squash_merge, allow_merge_commit, allow_rebase_merge, delete_branch_on_merge, squash_merge_commit_title, squash_merge_commit_message}'
# 期望：仅 squash；title PR_TITLE + message PR_BODY；delete_branch_on_merge 为 true
```

（上面的命令块是一次性命令式设置。若将来想要持续生效的 settings-as-code，probot 的 settings app 读取 `.github/settings.yml`——它会同时管住 `.github/` 变更，采纳前请先讨论。）

## 本地验证 CI（可选）

CI 跑的每条命令本来就能本地复现（`just ci`、`prek run --all-files`、`just dist-check`）——这是设计使然。workflow YAML 本身由静态检查兜底：`just lint` 已含 actionlint。经评估后我们刻意放弃了编排层的动态重放（act/Docker 方案）：当任务层已是唯一事实源时，这点额外的仿真度不值 Docker 依赖的代价。

需要 secrets/OIDC 的 job（release-plz 发布、crates.io Trusted Publishing）*按设计*不能本地运行——它们的合并前验证缝是 Release PR 加上每个 PR 都会跑的 `dist plan`。

## Agent 辅助开发

本仓库是人机双读制品。agent 侧的接入面：

- `AGENTS.md` —— 常驻约束（命令、NEVER 禁令、风格）。只放增量信息：每条规则都要通过“agent 不看这条会犯错吗”的检验。
- `.agents/skills/<name>/SKILL.md` —— 按任务激活的流程（渐进披露）；权威清单位于 AGENTS.md。约束放 AGENTS.md，流程放 skills，参考知识放 `docs/`（只链接不复制）。
- `.claude/skills` 是指向 `.agents/skills` 的软链——单一事实源，多端共用。
- `just agent-check`（在 `just ci` 内）冒烟校验整个接入面：frontmatter 形状、name/目录一致、体量预算、指针完整性。

边界：agent 可以开 PR，但永不合并、永不推 tag、永不发布；硬边界是分支保护，不是指令文件。agent 犯的重复性错误要经 `rule-maintenance` skill 回写为永久规则——与修复同 PR，而不是在聊天里反复纠正。理由见模板仓库的 ADR-0005。

## 文档纪律

手写文档只承载三件事：**意图**（why）、**理由**（ADR）、**入口**（可执行命令）。代码能自证的事实（参数表、版本号、命令清单）不手抄进散文；架构决策写进 `docs/decisions/`，别处只引用编号。

语言：英文为正本。`*.zh-CN.md` 是译本——先改英文版，译本滞后视为 bug。译本只为对外文档（README、CONTRIBUTING）维护；ADR、AGENTS.md 与 `.agents/skills` 仅英文——翻译为读者存在，而这些文档的读者（维护者、agent）都读英文。
