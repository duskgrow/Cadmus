# Cadmus TUI crates.io 选型调研:依赖合规核实与准入清单——2026-09-13 基线

> **Frozen research snapshot (baseline 2026-09-13).** Exhibit material for the
> ADRs in `docs/decisions/` — this is *not* living documentation and is never
> edited; where this document and the ADRs disagree, the ADRs win. Version,
> vendor and maintenance-status claims require re-verification at the start of
> the phase that depends on them (freshness policy: report §1.2.2). Current
> phase status lives in `docs/roadmap.md`.

数据来源:crates.io API 当日值(版本/license/MSRV/发布日期)、docs.rs API 文档、GitHub API 仓库元数据。
硬约束:MSRV ≤ 1.88(workspace `rust-version = "1.88"`)、crates.io-only(deny.toml 禁止 git 来源)、license 白名单 = `deny.toml [licenses] allow`(MIT / Apache-2.0 / Apache-2.0 WITH LLVM-exception / ISC / Unicode-3.0 / 0BSD)、发布序列停更 ≤ ~6 个月。
参照选型:Codex CLI 源码调研(`2026-09-13-codex-tui-source-study.md`)给出的依赖清单;本报告独立核实其对 Cadmus 约束的合规性并补齐 Codex 没有答案的项。
置信度标注:【已验证事实】= 当日一手来源;【未核实】= 来源缺位;【基于证据的推断】= 明示推理链。
消费者:ADR-0018(TUI 实现架构)的依赖准入节。

## 1. ratatui

【已验证事实】0.30.2,2026-06-19 发布,MIT,MSRV **1.88.0**(恰等于工作区 MSRV,合规但零余量)。节奏:0.29.0 (2024-10-21) → 0.30.0 (2025-12-26) → 0.30.1 (2026-06-05) → 0.30.2 (2026-06-19);GitHub 最近 push 2026-09-11,217 open issues——高度活跃。注意 0.30 起 ratatui 是元 crate,backend 拆分为 `ratatui-crossterm` 等,crossterm 版本经 `crossterm_0_29` feature 选择。

`Viewport::Inline` 能力边界(docs.rs 0.30.2 API 文档,【已验证事实】):

- 始终占满终端全宽;高度按行数指定并 clamp 到终端高度,超出内容裁掉(不是弹性高度)。
- 锚点 = 创建时及每次 resize 重算时光标所在行的第 0 列;光标近底时会滚动终端以保证 viewport 完整可见。
- 与普通 CLI 输出共存:`Terminal::insert_before` 在 UI 上方插入滚动输出——agent 的"对话流 + 底部输入区"正是这个模式。
- 陷阱:inline/fixed 下 `Frame::area()` 原点非 (0,0),布局必须以 `frame.area()` 为根,不能假设原点。

合规结论:✓。**推荐:0.30.2**。

## 2. crossterm

【已验证事实】0.29.0,2025-04-05 发布,MIT,MSRV 1.63。GitHub 最近 push 2026-09-01(活跃),246 open issues。功能逐项核对(docs.rs 0.29.0):raw mode ✓;`bracketed-paste` 为默认 feature ✓;`event-stream`(futures-core 异步事件流)✓;**`BeginSynchronizedUpdate` / `EndSynchronizedUpdate`(2026h 同步输出)在 0.29 的 `terminal` 模块中已存在** ✓;另有 `supports_keyboard_enhancement`(kitty 协议探测)。

即:crates.io 0.29 覆盖我们的全部需求,Codex 用 git fork 的理由不在 2026h,我们不需要 fork。

合规结论:**需例外说明**——发布停滞 17 个月,超停更线;但仓库在活跃提交,且 ratatui 组织官方维护 `ratatui-crossterm` 适配层,实质风险由上游承担。**推荐:0.29.0**,事件流走直接依赖 + `event-stream` feature。

## 3. pulldown-cmark

【已验证事实】0.13.4,2026-05-20 发布,MIT,MSRV 1.71.1。节奏:0.13.0 (2025-02-12) → 0.13.1 (2026-02-23) → 0.13.2/0.13.3 (2026-03) → 0.13.4 (2026-05-20),活跃。合规结论:✓。

流式增量用法的基本模式:pulldown-cmark **没有增量解析状态机**,官方姿势就是对累积缓冲多次 `Parser::new_ext(&buffer, opts)` 重建;未闭合结构(代码围栏、引用式链接定义)会回改前缀的解析结果,所以"只解析增量部分"在语义上就不成立。`offset_iter()` 给出每个事件的源字节区间,可用来判定哪些事件已落入稳定前缀、只刷这部分进渲染缓存——属于优化手段。推荐:TUI 消息量 KB 级,**每个 chunk 全量重建 parser + 渲染缓存**即可,offset_iter 留作后续优化。

**推荐:0.13.4**(不必跟 Codex 锁 0.10.3, crates.io 直接取最新)。

## 4. syntect + two-face

- syntect:【已验证事实】5.3.0,2025-09-27 发布,MIT,**MSRV 未声明**(`rust_version` 为 null,edition 2021)——MSRV 合规性【未核实】,1.88 下实测编译即可,风险低;GitHub 最近 push 2026-04-28,136 open issues。发布停滞 11.5 个月 → **需例外**。
- two-face:【已验证事实】0.5.2+bat-0.26.1,2026-08-07 发布(5 周前),MIT OR Apache-2.0,MSRV 1.79,crate 压缩包 3.6MB(内含 bat 全量语法定义)。仓库已迁至 Codeberg(CosmicHarper/two-face),维护者为 bat 维护组成员。合规结论:✓。

启动成本/二进制体积:two-face 自 0.4 起语法定义 lazy 加载(once_cell 延迟解析);启动开销 = 首次加载时反序列化 flate2+bincode dump,量级为几十 ms【未核实具体数值】;二进制体积增量为 MB 级(syntect 833KB + two-face 3.6MB 压缩包)【未核实具体数值,量级估计】。建议 `default-features = false` + `syntect-fancy`(纯 Rust fancy-regex 引擎),裁掉 onig 的 C 工具链依赖。

tree-sitter 路线一句话:每个语言一个 C grammar + cc 构建链,适合需要增量重解析/结构化编辑的场景;"markdown 代码块展示级高亮"用 syntect+two-face 一次集成覆盖全部语言,明显更省事。

**推荐:syntect 5.3.0 + two-face 0.5.2(syntect-fancy 引擎)**。

## 5. composer:tui-textarea vs reedline vs 自研

- tui-textarea:【已验证事实】0.7.0,2024-10-22 发布,MIT,MSRV 1.56.1;GitHub 最后 push **2024-12-01**,50 open issues——停更 ~22 个月,✗;且停留在 ratatui 0.29 时代,不支持 ratatui 0.30【未核实,从发布时间推断】。
- reedline:【已验证事实】0.51.0,2026-08-22 发布(nushell 官方,活跃),MIT;但 0.50.0/0.51.0 的 MSRV 为 **1.95.0 > 1.88** → 最新版不合规;0.49.0 MSRV 1.63 合规但形态不符:reedline 是 readline 式行编辑器(自带历史/补全/hinter 整套 prompt 设施),不是嵌入 ratatui 渲染管线的多行 composer widget。不推荐。

**推荐:自研轻量 composer**(ratatui widget + unicode-segmentation 光标/grapheme 移动 + unicode-width 宽度计算)。理由:两个候选一个停更一个错场景;Codex 自研 4600 行证明复杂度可控;Cadmus 的注入式确定性风格也更贴合自有实现。范围建议收敛在:多行编辑、光标/选区、bracketed-paste 整段插入。

## 6. fuzzy:nucleo-matcher vs fuzzy-matcher

- nucleo-matcher:【已验证事实】0.3.1,2024-02-20 发布,**MPL-2.0——不在白名单**,MSRV 未声明;crates.io 停发 2.5 年;上游 helix-editor/nucleo 仓库本身最近 push 2026-06-24(活跃,但 matcher 不发版)。✗(许可证 + 停更,双重淘汰)。
- fuzzy-matcher:【已验证事实】0.3.7,2020-10-04 发布,MIT,MSRV 未声明(edition 2018);停更近 6 年,形式上触停更线——但它是 1515 行、零依赖的纯算法 crate(skim 的匹配引擎),停更原因是"完成"而非"弃坑"。

**推荐:fuzzy-matcher 0.3.7(需例外:停更;风险低)**。若维护者拒绝例外,备选是自研 fzf-v2 算法(公开算法,fuzzy-matcher 可作参照实现)。不为 nucleo-matcher 申请 MPL-2.0 例外:打分质量的增量不值得把一个文件级 copyleft 许可证引入依赖树。

## 7. diff 渲染:similar 够不够,diffy 还要不要

【已验证事实】similar 2.7.0(2.x 线最新,2025-01-19),Apache-2.0,MSRV 1.60;workspace 已有 `similar = { version = "2", default-features = false, features = ["text"] }`(`Cargo.toml` L46-51)。注意 similar 3.2.0 已于 2026-08-17 发布(MSRV 1.85、edition 2024)——升级 3.x 是独立议题,不阻塞 TUI。

word-level 能力:similar 的 **`inline` feature 提供 `TextDiff::iter_inline_changes`**——在行级 diff 的变更行内再做 token 级(word/grapheme)diff,正是 diff 视图的行内高亮所需;`unicode` feature 提供 grapheme 分割。当前声明未开 `inline`,TUI 落地时补上即可。

结论:similar 2 + `inline` 足够,**diffy 不需要**(diffy 只做 unified diff 计算与 patch 解析,无行内高亮;渲染层本来就要自写 ratatui widget;其维护状态不再影响我们,未核实)。

## 8. unicode-width / unicode-segmentation / textwrap

【已验证事实,crates.io API 当日值】:

| crate | 最新版 | 发布日期 | license | MSRV | 结论 |
|---|---|---|---|---|---|
| unicode-width | 0.2.2 | 2025-10-06 | MIT OR Apache-2.0 | 1.66 | ✓(发版停 11 个月,标注;它本就会经 ratatui 传递进树,直接依赖仅在我们自调 API 时需要) |
| unicode-segmentation | 1.13.3 | 2026-06-01 | MIT OR Apache-2.0 | 1.85 | ✓(1.13.x 系列 2026-03/06 连续发版,活跃) |
| textwrap | 0.16.3 | 2026-09-09(4 天前) | MIT | **1.90 > 1.88** | 最新版 ✗;锁 **0.16.2**(2025-03-03,MSRV 1.70)✓,序列活跃 |

textwrap 建议 `default-features = false, features = ["unicode-width"]`(裁掉 smawk / unicode-linebreak);或者先用 ratatui 自带 wrap、把 textwrap 列为可选,准入时决定。

## 9. vt100 + insta + vhs

- vt100:【已验证事实】0.16.2,2025-07-12 发布,MIT,MSRV 1.70;停发 14 个月,但 dev-only(不随发布产物分发)——合规结论:✓(dev;若严格执行停更规则则需零成本例外)。
- insta:【已验证事实】1.48.0,2026-06-11,Apache-2.0,MSRV 1.66,持续活跃;workspace 已有 `insta = "1"` dev 依赖。✓。
- vhs 进 CI 结论:vhs 是 Go 二进制(charmbracelet/vhs),进 CI 只有"GitHub Action 装 Go 二进制 / 官方 action"两条路,供应链与缓存都要单独治理;**没有成熟的 Rust 替代品**。推荐做法:vhs 不进质量门,TUI 的确定性验证走 vt100 + insta 快照(项目已有惯例),vhs 仅本地录 demo。

---

## 准入清单(adding-dependencies 五问,供维护者逐个确认)

| crate(版本) | necessity(一句话) | health | alternatives | license | features |
|---|---|---|---|---|---|
| **ratatui 0.30.2** | TUI 框架本体,无替代品可谈 | 活跃(最近发布 3 个月前,push 2026-09-11);MSRV 恰为 1.88.0,无余量 | 无(termion/termwiz 均为其内部 backend 选项) | MIT ✓ | 默认即可(含 `crossterm` 后端);不要 `palette` |
| **crossterm 0.29.0** | 事件流 / bracketed paste / 2026h 同步输出,ratatui 不封装事件循环 | 仓库活跃(2026-09-01)但发布停 17 个月——**需例外说明** | 无实质替代(ratatui 官方后端) | MIT ✓ | `default + event-stream` |
| **pulldown-cmark 0.13.4** | assistant 输出的 markdown 渲染解析器 | 活跃(4 个月前) | comrak(AMM 绑定更重) | MIT ✓ | `default-features = false`(裁掉 getopts/html 二进制路径) |
| **syntect 5.3.0** | 代码块语法高亮引擎 | 发布停 11.5 个月、仓库有活动、MSRV 未声明——**需例外** | tree-sitter(构建链重,见 §4) | MIT ✓ | 经 two-face 转发:`syntect-fancy`(纯 Rust 正则,免 C 依赖) |
| **two-face 0.5.2** | bat 全量语法/主题定义,免去自维护语法包 | 活跃(5 周前),bat 维护组成员维护 | syntect 自带 default-syntaxes(覆盖面小一半) | MIT OR Apache-2.0 ✓ | `default-features = false, features = ["syntect-fancy"]` |
| **fuzzy-matcher 0.3.7** | @ 文件补全的 fuzzy 打分 | 停更近 6 年——**需例外**;1515 行零依赖纯算法,停更是"完成"而非弃坑 | nucleo-matcher(MPL-2.0,许可证出局);自研 fzf-v2 | MIT ✓ | 无(默认) |
| **unicode-width 0.2.2** | composer/渲染的显示宽度计算 | 发版停 11 个月,unicode-rs 官方核心 crate;经 ratatui 传递已在树 | 无 | MIT OR Apache-2.0 ✓ | 无(默认) |
| **unicode-segmentation 1.13.3** | grapheme 级光标移动与分割 | 活跃(3 个月前) | 无 | MIT OR Apache-2.0 ✓ | 无(默认) |
| **textwrap 0.16.2** | 长行按词换行(markdown/对话流) | 序列活跃(0.16.3 于 4 天前发布);锁 0.16.2 因 0.16.3 MSRV 1.90 超标 | ratatui 自带 wrap(功能弱)——可选,准入时定 | MIT ✓ | `default-features = false, features = ["unicode-width"]` |
| **vt100 0.16.2**(dev) | TUI 端到端快照测试的虚拟终端 | 停发 14 个月,dev-only 不分发 | 无 | MIT ✓ | 无(默认) |

**不进清单**:tui-textarea(停更 ✗)、reedline(最新版 MSRV 1.95 超标 + 场景不符)、nucleo-matcher(MPL-2.0 + 停发)、diffy(similar `inline` 已覆盖)。composer 走自研。

**遗留项**:① syntect MSRV 未声明,准入时以 1.88 实测编译为准;② crossterm / syntect / fuzzy-matcher / unicode-width / vt100 的停更例外需维护者逐一签字;③ 维护者若同意,textwrap 可整体砍掉(用 ratatui 自带 wrap),少一个 MSRV 1.90 的前瞻风险。

## 补记(2026-09-13 维护者讨论后的配置语言核实)

讨论配置语言时当日核实三个候选(crates.io API):

- **starlark** 0.14.2:Apache-2.0,edition 2024,`rust_version` 未声明;2026-06-05 发布(facebook/starlark-rust,Meta/Buck2 血统,0.14.2 由 dtolnay 发布),月下载量 2M 级。同领域先例:Codex execpolicy。
- **rhai** 1.26.1:MIT OR Apache-2.0,MSRV 1.66,2026-09-10 发布(极活跃);但定位是通用嵌入式脚本,非 hermetic。
- **nickel-lang-core** 0.18.0:MIT,MSRV **1.89 > 1.88** 不合规(0.16.1 为 1.85);默认 features 拖 comrak/termimad/tree-sitter。

结论(经伪需求分析,见 ADR-0018):可编程配置平台作为最大形态是伪需求,**Starlark 推迟到其真实消费者**(scoped 审批规则需要计算条件,或 hooks 触发器点燃);配置 v1 用 TOML 数据。starlark/rhai/nickel-lang-core 均不进依赖树,本条仅存档核实数据。
