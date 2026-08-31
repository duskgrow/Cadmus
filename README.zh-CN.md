# rust-template

[English](./README.md)（如有出入以英文版为准）

一条命令起家的 Rust 项目模板。提炼自 [docs/engspec.md](./docs/engspec.md)——对 Zed、rust-analyzer、DeepSeek Harness、LLVM、uv 等标杆仓库的工程规范调研——把 Rust 开发与发布流程需要的组件全部预配好，你只管写核心代码。

## 快速开始

前提：安装 [Nix](https://nixos.org/download/) 并启用 flakes（direnv 可选但推荐）。

```bash
mkdir my-cli && cd my-cli
nix run github:duskgrow/rust-template -- my-cli your-github-name   # 拷贝 + 改名 + 清理模板制品 + git init + 暂存 + git 钩子——一条命令
```

等价的两步路径：`nix flake init -t github:duskgrow/rust-template`（仅拷贝，无 git 历史），再 `nix develop -c just init my-cli your-github-name`。

结束后你就得到一个"从空仓库长出来"的项目：模板制品（模板 README、init 脚本、规范报告、冒烟自检 CI job）全部移除，git 历史为空——全部文件已暂存，第一个 commit 由你亲手提交。

不用 Nix 命令行也行：把本仓库标记为 Template repository（Settings → General → Template repository），点 "Use this template" 建仓库——一个一次性 workflow 会自动完成初始化（仓库名即项目名，请用 kebab-case；workflow 随后自毁，其 GITHUB_TOKEN 推送按 GitHub 递归保护不触发 CI——你的首次推送才会触发）。等价本地路径：clone 后跑 `nix develop -c just init <name> <owner>`。

## 包含什么

| 维度 | 选型 | SSOT 位置 |
|---|---|---|
| 工具链版本 | rustup 与 flake 双消费同一文件（rust-overlay `fromRustupToolchainFile`） | `rust-toolchain.toml` |
| 可复现环境 | flake + direnv，`flake.lock` 入库 | `flake.nix` |
| 任务层 | just 唯一入口（本地 / git 钩子 / CI 同调，无第二份实现） | `justfile` |
| workspace | 根虚拟清单 + `crates/*` 扁平布局；依赖与 lint 单点声明、成员继承 | 根 `Cargo.toml` |
| 测试 | cargo-nextest（进程隔离）+ doctest + insta 快照（CI 只读、人工批准） | `.config/nextest.toml` |
| 依赖治理 | cargo-deny 四检查（安全公告 / 许可证 / 重复版本 / 来源） | `deny.toml` |
| 提交信息 | 修改版 Conventional Commits（`type(scope): subject`，ASCII subject，header ≤100 字符），squash-only 合并；commit-msg 钩子与 CI 检查 PR 标题双端强制 | `crates/xtask`（`check-commit`） |
| Changelog | release-plz 从提交信息生成，按模块（commit scope）分组；日常条目不手写 | `CHANGELOG.md`（生成物） |
| 版本与发布 | Release PR 人工闸门 → tag `v*` → crates.io（OIDC Trusted Publishing） | `release-plz.toml` |
| 跨平台分发 | cargo-dist：四平台产物 + shell/powershell 安装脚本 + GitHub attestation | `dist-workspace.toml` |
| CI | 薄编排（`nix develop -c just ci` / `prek run`），action 按完整 SHA 钉死 | `.github/workflows/` |
| Agent 接入 | AGENTS.md（只含增量信息）+ `.agents/skills/` 判断层 skill + `just agent-check` 冒烟校验 | `AGENTS.md`、`.agents/skills/` |
| 决策记录 | ADR（MADR 轻量格式），理由只写一次 | `docs/decisions/` |
| 过程门禁 | PR 模板 + issue 表单 + 推荐分支保护清单；workflow YAML 由 actionlint 静态检查；提交规范由 xtask `check-commit` 子命令强制 | `.github/` |
| 语言 | 英文为正本；`*.zh-CN.md` 为译本（先改英文版） | — |

## 发布流程（配好之后零操心）

1. 日常用 Conventional Commits 提交（`feat:` / `fix:` / `!` 即版本意图）；
2. release-plz 自动维护一个 Release PR：算版本号、更新 CHANGELOG、跑 cargo-semver-checks；
3. 你点 merge → 自动推 tag `vX.Y.Z`、发布 crates.io（OIDC，无长期 token）、触发 cargo-dist 构建四平台产物并创建带 checksum 与 attestation 的 GitHub Release。

首次启用的一次性设置（约 5 分钟）：见 [CONTRIBUTING.zh-CN.md「发布」](./CONTRIBUTING.zh-CN.md#发布)。

## 目录导览

- `crates/<name>/` — 第一个 crate（薄 bin + 可测试 lib 骨架，含 insta 快照示例）
- `justfile` — 所有自动化入口（`just --list`）
- `docs/decisions/` — 模板的关键决策（ADR）；`just init` 后重置为你项目的全新起点
- `docs/engspec.md` — 本模板提炼自的工程规范报告（`just init` 时从你的项目中移除）

## 按需裁剪

- 纯库项目：删除 `dist-workspace.toml` 与 `.github/workflows/release.yml`
- 不发 crates.io：`release-plz.toml` 里对应包设 `publish = false`（保留 tag 与 GitHub Release）
- 不要 Windows 支持：删 ci.yml 的 `test-windows` job 与 `dist-workspace.toml` 里的 msvc target
- 需要原生 Windows 开发：走 WSL2（见 docs/decisions/0004）

## 给模板自身做贡献

本仓库自己也遵守同一套规范：`direnv allow && just ci`（进入 devShell 会自动安装 git 钩子）。

这里的每个改动都有**两重身份**，必须想两遍：

1. **作为本仓库**——模板自身的 `just ci` 必须保持绿（占位 crate 名 `__project_name__` 本身是合法的 crate 名，所以模板是一个真实可构建的 workspace）。
2. **作为未来所有生成项目**——凡是没有被 init 剥掉的东西都会随模板分发到下游：devShell 里的新工具、workflow 里的新 job、AGENTS.md 里的新规则。每个改动都要问一句：它属于哪个身份？模板专属内容放在 `# >>> template-only` / `# <<< template-only` 围栏内（Rust 源码用 `// …` 前缀；由 xtask 的 `init` 模块剥除），或放在 init 会替换/删除的文件里（本 README、`docs/engspec.md`、`docs/decisions/`、init 模块自身）。因此，面向模板维护者的引导只能写在本 README——绝不写进会随项目分发的 CONTRIBUTING.md。

CI 的 `smoke-init` job 就是第二重身份的质量门：它在临时目录跑完整的 `just init` → `just ci` → `just dist-check` 流程——漏掉的占位符或坏掉的生成项目会让模板自己的 CI 变红。引导机制的设计理由见 `docs/decisions/0001`。
