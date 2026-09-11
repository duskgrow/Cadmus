# Agent 界面 UIUX 深度调研：交互模型、编排器↔agent 缝隙与一体式设计指导——2026-09-11 基线

> **Frozen research snapshot (baseline 2026-09-11).** Exhibit material for the
> ADRs in `docs/decisions/` — this is *not* living documentation and is never
> edited; where this document and the ADRs disagree, the ADRs win. Version,
> vendor and maintenance-status claims require re-verification at the start of
> the phase that depends on them (freshness policy: report §1.2.2). Current
> phase status lives in `docs/roadmap.md`.

研究基线：2026-09-11；语言：中文。本文件为当日第二版：应维护者要求从"功能清单"升级为机制级分析与设计指导（学习交互模型本身、看见没人做到的部分、兑现"既做 agent 又做编排器"的一体优势）。

## 摘要

### 核心结论

#### 一、市场的结构性裂缝：agent 与编排器分家，缝隙两侧各有结构上解决不了的事

【已验证事实】机制级证据（§3）：编排器观察外来 agent 的全部手段收敛为八种机制（§3.1 表），其中真正承载语义的只有两种——"往 agent 自己的配置里装生命周期钩子"（Orca、herdr、Superset、Warp 全这么做）和 ACP 协议；其余皆是屏幕抓取与启发式。结果是六项关键信息**从未**跨过缝隙（§3.2）：逐事件成本归因、带策略的结构化审批、检查点/rewind 语义、跨会话上下文、注意力意图、限额临近度。痛点有公开 issue 实证（§3.3）：ccmanager #227（Claude Code 删掉 `esc to interrupt` 字符串，检测器立刻把"忙碌"误报为"空闲"）、Superset #7395（主 agent 等待自己的子 agent 时，看板显示"无工作发生"）、Windsurf worktree 超 20 个即 LRU 静默删除未合并工作。这些全是**结构性**故障——编排器对 agent 的 UI 没有特权通道，agent 厂商每周改 UI，抓取层永远落后。

【基于证据的推断】Cadmus 既做 agent 又做编排器，恰好站在唯一能闭合这条缝的位置：ADR-0002 的事件溯源核心 + ADR-0013 的客户端协议，使"编排视图 = agent 自身事件流的投影"成为构造性事实而非抓取近似（ADR-0012 条目 3 已规定会话状态必须可从事件派生）。本调研确认的每个市场结构性痛点，在我们这边都对应一块已经打好的地基（§4.1 对照表）。这是"没人做到、我们地基已就位"的唯一位置，也是本项目交互层的差异化所在。

#### 二、交互地板在 ADR-0011 基线（2026-09-06）后已被抬高和分叉

【已验证事实】转向出现第三粒度"在下一个工具调用边界注入当前轮"（Claude 排队消息语义、Cursor Cmd+Enter；Codex 与 Claude 的 Enter/Tab 绑定恰好相反——未收敛，须刻意选择）；审批表面超越模式枚举（Codex 沙箱×审批双轴 + execpolicy Starlark 规则，OpenCode 按工具输入 glob 规则引擎，Claude 新增分类器模式 `auto` 并于 2026-08 转为默认）；rewind 长成四动作代数（restore / fork / summarize / re-decide）；子代理与后台任务可视化进入地板；Claude Code v2 双渲染器（inline + 全屏 alt-screen）挑战 ADR-0012 的 inline-only 立场。详见 §2.1。

#### 三、评审往返有三种保真度，最高者把评论当共享对象而非文本 prompt

【已验证事实】GUI 编排器的 diff 评审→agent 回注收敛为三模式（§2.2.4）：拼成一条行锚定文本 prompt（Orca、Vibe Kanban）＜ 评论作为共享可寻址对象、agent 经 MCP/CLI 读写并可 resolve（Conductor）＜ 人直接改 diff（零延迟但对 agent 零反馈信号）。合并侧：PR gate 主导一切；冲突解决是 prompt 级移交（Orca "Resolve with AI"）；best-of-n 是**选赢家**不是合并；无人做语义级多 worktree 整合。

#### 四、GPUI 前提三段式更正：许可证问题解决，可用性问题浮出

【已验证事实】`gpui` crate 为 Apache-2.0（crates.io API 核实），ADR-0011/0012/0013 修订中"GPL → 独立仓库"的论证失去前提；GPL 仅覆盖 Zed 应用层 crate（`terminal`、`ui`），胶水需自写。【已验证事实 + 维护者实测】但 crates.io 发布序列止于 0.2.2（2025-10-22，基线前约 11 个月，全序列集中在 2025-10 一个月内）——证实维护者"已停更"判断；用 git 版本则依赖树复杂、实测有问题（维护者，2026-09-11；佐证：macOS 拉 Zed font-kit fork 的 git 依赖）。两条消费路径摩擦都高，GUI 技术选型实质变为 egui/iced、Tauri+webview、自绘（tui-term 方向）与 GPUI(git) 之间的权衡（§5）。

#### 五、排序维持 TUI 先行；ACP 前置；GUI = 编排层 + 嵌真终端

【基于证据的推断】整个编排器世代建立在编排 TUI 之上（Orca/Superset 嵌真实 PTY 跑各家 CLI，Zed 把 agent TUI 列为一等线程）；纯 GUI 取代终端的尝试或沦为控制面板（opcode）、或转型（Crystal→Nimbalyst）、或退场（Vibe Kanban 2026-04 停止维护，事后声明直言价值归于 agent 拥有者而非外壳）。TUI 是被生态免费编排的可移植界面，须先行；ACP 是"GUI 之前的 GUI"最高杠杆动作（open-items 已有 seam 评估条目且结论为正）；GUI 落地形态 = 机群投影 + 注意力路由 + 评审往返 + 嵌真实终端核心，不重画聊天（§6）。

## 1. 范围与方法

### 1.1 触发问题与任务边界

维护者指示（2026-09-11）：调研 agent 软件的 TUI/GUI UIUX，不止看人家做到了什么，更要看到人家没做到什么；Cadmus 的定位是**既做 agent 又做编排器**——市场上两者分家且结合不好，我们要把结合做好做强；调研产出须服务后续开发，即转化为设计指导而非产品清单。

边界：只调研交互与界面维度；模型能力、定价策略不在范围。维护者"既做编排器"的方向超出现行 ADR 文本两处（基线报告 §2.5.1 裁掉了多会话 swarm 编排；ADR-0012 条目 3 的 dashboard 仅为 candidate）——§4.0 画出本报告采用的边界，落地时须以 ADR 正式化。

### 1.2 五路调研与置信度标注

2026-09-11 当日五路并行桌面研究：agent TUI 格局、agent GUI 格局、终端风格 Rust GUI 技术栈、**编排器↔agent 缝隙机制**（逐产品查清什么信息/控制以什么机制跨过边界）、**痛点挖掘**（GitHub issue、HN 讨论、评测文、停更/转型事后声明）。标注法沿用基线报告三级制：【已验证事实】= 当日一手来源；【未核实】= 来源缺位，训练数据；【基于证据的推断】= 由已验证事实经明示推理链得出。

### 1.3 用户点名产品的身份确认

herd → **herdr**（herdrdev/herdr）：Rust 编写的 agent 感知终端 workspace 运行时；orca → **Orca**（stablyai/orca）：MIT、66k★ 量级开源 ADE；wrap → **Warp**（warpdotdev/warp）：2026 年已开源（AGPLv3），定位 Agentic Development Environment + 云工厂。

## 2. 交互模型剖析

按模型组织而非按产品——我们学的是设计决策本身。产品事实汇总见 §2.3 速查表。

### 2.1 会话层：agent TUI 的五个模型决策

#### 2.1.1 渲染模型：inline 与 alt-screen 从立场变成双态

【已验证事实】Claude Code v2 同时提供经典 inline 渲染器与全屏 alt-screen 渲染器；全屏版带 `/diff` 侧栏（宽度 ≥144 列自动打开——"空间够就升舱"的触发策略）、Ctrl+O transcript 查看器（逐消息模型/时间戳、`{`/`}` 按 prompt 跳转）。其余主流（Codex、Gemini、OpenCode、Crush）仍是 inline 为主。【基于证据的推断】ADR-0012"inline 为默认、alt-screen 仅给真模态子应用"的立场不必推翻，但应补一条触发策略：alt-screen 升舱由空间与任务形态驱动，而非用户手动切换。

#### 2.1.2 转向语法：三粒度并存，绑定未收敛

【已验证事实】三种粒度并存于市：queue（下一轮）、**边界注入**（Claude：排队消息在当前轮的工具调用边界注入；Cursor：Cmd+Enter 同语义）、inject-now（Codex：Enter 直注当前轮）。绑定恰好相反（Claude Enter=queue；Codex Enter=inject），且 Codex 队列可混存 prompt/slash/`!` shell 行、执行时解析。轮内设置转向成为现实：Claude 的 `/model`、`/effort` 对运行中轮内的下一请求生效。【基于证据的推断】三粒度中"边界注入"兼顾即时性与确定性（注入点可进事件流、可回放），与 ADR-0013"命令在生效时记录"天然同构——建议为我们的默认语义；绑定选择属于 TUI 地板重基线。

#### 2.1.3 审批模型谱系：从模式枚举到规则引擎到分类器

【已验证事实】四代并存：模式枚举（Claude Shift+Tab 五模式：default/acceptEdits/plan/bypassPermissions/**auto**）；双轴（Codex：沙箱策略×审批策略 + `/permissions` 预设 + execpolicy `.rules`——Starlark，`bash -lc` 链条经 tree-sitter 切分为**逐子命令**求值，最严匹配胜出，可 `codex execpolicy check` 离线测试）；规则引擎（OpenCode：allow/ask/deny × 工具输入 glob + `external_directory`/`doom_loop`（3 次相同调用）护栏，"always" 白名单工具建议模式）；分类器（Claude `auto`：模型判断，2026-08 起默认）。机制要点：Codex app-server 的审批是**服务器→客户端的类型化请求**，客户端程序化作答——`acceptForSession`、子集授权（`scope: turn|session`）、`acceptWithExecpolicyAmendment`（拒绝顺带修订规则）。【基于证据的推断】这与 ADR-0008 条目 4"审批命令事件"与 open-items"scoped approval rules"（auto-resolving 策略装饰器）形状完全一致：我们的核心形状已对，缺的是**按工具输入匹配**的表达力与 `acceptForSession` 这类作用域语义；模式枚举应保留为规则之上的糖（OpenCode 佐证预设只是糖）。

#### 2.1.4 rewind 长成四动作代数

【已验证事实】四个不同动作在市场上都被实现：restore（Gemini：`~/.gemini/history` 影子 git 仓库快照文件+会话+挂起工具调用，`/restore` 还原并**重新提议原工具调用**——"重新决定"而非"回退"）；fork（Codex Esc-Esc 编辑历史消息并分叉；app-server `thread/fork(lastTurnId)`）；summarize（Claude `/rewind` 菜单含 summarize-from-here / summarize-up-to-here——rewind 与定点压缩合并）；checkpoint（Conductor：按轮回退代码+会话状态，机制未公开）。【基于证据的推断】ADR-0011 的"conversation-only / code-only / both"三分只覆盖 restore；完整代数是 restore/fork/summarize/re-decide，且 restore 的存储机制上**影子 git 仓库**（Gemini）优于自造文件快照——diff/log 语义免费获得，且不碰用户 git 状态的约束仍满足。可分阶段落地。

#### 2.1.5 会话隐喻：从聊天到"带机器的可寻址任务"，恢复=重放事件流

【已验证事实】会话隐喻漂移：herdr workspace、Amp orb（每线程云端机）、Codex cloud 环境、Crush serve（workspace 按 cwd 键控、多客户端实时镜像、`IsBusy`/`AttachedClients`）。协议层面最关键的一条：ACP 的 `session/load` 定义为**agent 把完整历史作为 `session/update` 通知重放**——与 ADR-0013 的 sync-on-subscribe（attach = replay+sync+tail）同构。【基于证据的推断】行业独立收敛到"恢复即重放自有轨迹"，佐证我们协议形状是收敛方向而非异类；herdr/ccmanager/claude-squad 恢复外来会话只能靠被管程序自己的 `--resume` 且进程死了就只剩像素回放（herdr `pane_history` 实验特性），反证"轨迹自有"是恢复语义的前提。

### 2.2 编排层：五个模型决策

#### 2.2.1 隔离单元：worktree 收敛与一条反潮流

【已验证事实】本地标准=每任务一个 git worktree（Orca、Superset、Crystal/Nimbalyst、Vibe Kanban、Zed、VS Code、Windsurf）；云等价物=每任务 microVM（Conductor Firecracker、Codex、Jules、Devin、Warp Factories）。反潮流：GitButler 联合创始人明确拒绝 worktree，走共享 workspace + **编辑前文件锁**、agent 互见编辑（HN 原话："no merge conflicts… code 不在语义上发散"）。【基于证据的推断】worktree 是单人多会话的正确默认；GitButler 路线依赖"agent 互见编辑"这一编排器自有能力，恰是我们一体性可选的远期甜点，但不应进 v1。

#### 2.2.2 屏幕隐喻四分，无赢家

【已验证事实】worktree 仪表盘/侧栏（Orca、Superset）善扇出比较；看板（Vibe Kanban）善计划→执行→评审流水线；收件箱带单行 diff 统计（Codex cloud、Copilot agent 页）善异步委托；多人房间（Conductor：presence、跟随、共同 prompt）。四者并存。【基于证据的推断】单人尺度下收件箱+仪表盘的杂交（状态行列表 + blocked 优先排序 + 单行 diff 统计）已够；看板是团队协作物，不追。

#### 2.2.3 注意力路由是被评测验证的第一原语，真实性是其生死线

【已验证事实】herdr 评测原话："**Blocked was the most useful signal**"；成熟工具全力把轮询变成中断（dock 徽章、完成音、未读标记、OS 通知、移动推送——Orca 移动端推送是官网主打）。真实性反面教材成堆：ccmanager #227（厂商删掉一个 UI 字符串即误报 idle）、herdr 对未集成 agent 的 blocked 检测"刻意保守"即**审批提示按设计漏报**、Superset #7395（子代理工作期误报 idle）。【基于证据的推断】注意力路由的排序键与徽章只是表层，**状态真实性**才是产品——误报 idle 是注意力路由器最坏的默认。我们的 blocked 是审批请求事件本身（ADR-0013 Sync 的 in_flight 即含挂起审批），从构造上免疫此类误报。ccmanager #227 的修复方向也值得记录：fallback 应保持现态而非回落 idle。

#### 2.2.4 评审往返三模式，保真度递升

【已验证事实】模式一：评论拼成**一条行锚定文本 prompt** 发给选定 agent（Orca：评论跨 diff 位移被跟踪、未 resolve 者并入下一批；Superset：载荷含文件/行区间/侧别，可为此创建 PR-checkout workspace 并以评论为首条 prompt；Vibe Kanban：评论收集后附于下一条聊天消息）。模式二：评论是**共享可寻址对象**，agent 经 MCP/CLI 读写、可迭代可 resolve（Conductor Changes 面板："Review my workspace diff and leave inline comments"/"Address the comments" 是官方流）。模式三：**人直接改 diff**（Conductor 默认、Orca Monaco、Superset "Edit Here"）——零延迟，但作为**反馈**对 agent 不可见。【基于证据的推断】模式二保真度最高且天然是 orchestrator 可读队列；模式三必须与事件流打通（人的直改作为事件入流），否则 agent 对代码变化无感知（Conductor 的教训）；模式一在我们这里无需存在——那是没有共享事件流时的代偿。

#### 2.2.5 合并：PR gate 主导，语义整合无人做

【已验证事实】全部 GUI 以 push→hosted PR 为主流合并门（Orca 显式 force-with-lease、hook 失败有 "Fix with AI"）；冲突解决是 prompt 级移交（Orca "Resolve with AI" 把冲突集交给 agent；Conductor 组织级自定义冲突解决指令）；**无人**做语义级多 worktree 整合；best-of-n 流以**选赢家**收场而非合并。HN 痛点实证：五个并行 agent 重写同一基类，"merge conflict no neural net could ever untangle"；"most of these tools don't make working with Git merges or conflicts simpler"。【基于证据的推断】诚实的答案不是语义合并引擎（单人尺度不追），而是纪律：小任务切分 + worktree 隔离 + PR 评审门 + 冲突移交 agent；Windsurf 的 20 个 worktree LRU 静默删除是反例——**永不静默删除未合并工作**应写成不变量。

### 2.3 产品速查表

【已验证事实】（栈与定位经当日核实；详情见其官网/仓库，§8 来源清单）

| 产品 | 类别 | 技术栈 | 一句话模型 |
|---|---|---|---|
| Claude Code v2 | agent TUI | TS | 双渲染器 + 五模式 + rewind 代数 + `/btw` 旁问 |
| Codex CLI | agent TUI | Rust+ratatui | 双轴审批 + app-server 可拆客户端 + 可组合状态行 |
| Gemini CLI | agent TUI | Node/Ink【未核实】 | 影子 git 检查点 + 深度主题化 |
| Aider | agent CLI | Python | 行式 REPL + 自动 git 提交即 undo |
| OpenCode | agent TUI+server | Go/TS | client/server 分体 + glob 规则审批 + `/share` |
| Crush | agent TUI | Go/Bubble Tea | serve 多客户端实时镜像 + cwd 键控 workspace |
| Goose | 三表面 | Rust（AAIF） | 桌面/CLI/嵌入 API + recipes 模板 |
| Amp | CLI+云 | — | Dial effort 模式 + Oracle 第二意见 + orbs |
| Cursor CLI | agent CLI | — | Agent/Plan/Ask + `&` 云移交 + sudo 安全 IPC |
| herdr | 编排器 TUI | Rust | 服务器持有 PTY + 双状态权威 + blocked 上卷 |
| ccmanager | 管理器 TUI | — | PTY 直管 + 屏幕模式检测 + LLM 审判自动审批 |
| claude-squad | 管理器 TUI | Go+tmux | worktree 每任务 + 暂停=commit+checkout |
| Orca | 编排器 GUI | Electron+xterm.js | worktree 机群 + 注释 diff 回注 + 可被 agent 脚本化 |
| Warp | 终端 GUI | Rust（AGPLv3） | blocks 原语 + 外来 agent toolbelt + 云工厂 |
| Wave | 终端 GUI | Electron+Go | widget blocks + CLI 数据喂自定义 widget |
| Conductor | 编排器 GUI | 闭源 | 云 microVM + 多人 + diff 直改 + 检查点回退 |
| Crystal→Nimbalyst | 转型案例 | Electron | 编排器→协作多文档编辑器（编排不是产品） |
| Superset | 编排器 GUI | ELv2 | worktree+dock 徽章+MCP-server-as-surface |
| opcode | 单 agent GUI | Tauri+Rust | ~/.claude 项目浏览器 + 检查点 fork/diff 代数 |
| Vibe Kanban | 编排器 web | Rust+web（停维护） | 看板三阶段分离 + 评论回注先驱 |
| Zed+ACP | 编辑器 | Rust/GPUI | Terminal Threads + agent 的 LSP |
| Cursor/VS Code/Devin Desktop | 编辑器在位者 | — | Agents 窗口 + queue/steer/stop 三态 + 检查点 |
| Codex cloud/Jules/Copilot/Devin | 云异步 | — | 任务收件箱 + plan 门 + PR 为最终人工门 |

## 3. 缝隙分析：编排器↔agent 今天跨过了什么

### 3.1 八种集成机制

【已验证事实】逐产品查清后的归类（谁用、跨过什么）：

| # | 机制 | 谁在用 | 实际跨过缝隙的东西 |
|---|---|---|---|
| 1 | PTY 直通（自持有终端） | 全部八家的底线 | 字节、cwd、退出码；零语义 |
| 2 | 屏幕抓取/启发式 | ccmanager（逐工具硬编码模式）、claude-squad（capture-pane 轮询+sha256+子串）、herdr（底部 buffer 的 TOML 清单+OSC 佐证）、Superset（`terminals_read` 返回屏幕文本）、Warp（自家 buffer） | 推断的 idle/busy/waiting、提示文本 |
| 3 | 环境变量缝 | herdr（`HERDR_*` 入、`pane report-agent` 回）、ccmanager（`CCMANAGER_*` 出给钩子）、Superset（钩子 env 门控） | 身份、语义状态、迁移 |
| 4 | 编排器往 agent 配置里装生命周期钩子/插件 | Orca（statusline OSC + 状态钩子，端点文件每次调用重源以活过重启）、herdr（7 家 agent 的钩子/插件权威）、Superset（`superset-hooks`/包装器）、Warp（通知插件，仅 Claude/Codex/OpenCode） | started/finished/waiting、会话 id、通知 |
| 5 | 带外消费 headless 结构化输出 | ccmanager（`claude -p --output-format json` 当审批**审判官**）、Orca（读 `~/.claude` 等本地账本）、Superset（本地会话日志定价） | 裁决、用量——永不含实时流 |
| 6 | SDK/受管二进制嵌入 | Conductor（自带 harness 二进制、检查点；机制未公开）、Orca（`orca claude-teams`） | 模式、检查点、transcript（内部） |
| 7 | MCP 暴露（编排器作为工具服务器） | Superset、Conductor、Orca | workspaces、terminals 读/写、automations |
| 8 | JSON-RPC 协议（agent 自身事件流） | **ACP**（Zed 生态）；herdr socket API 是反方向（agent 驱动编排器） | 完整 turn/tool/permission/plan/usage 事件 |

### 3.2 从未跨过缝隙的六项

【已验证事实】外来 agent 场景下，没有任何产品搬动过：

1. **逐事件成本归因**——全是事后读本地账本或 provider 配额端点；没有任何东西把成本绑到具体工具调用（ACP `usage_update` 也只是会话级累计）。
2. **带策略的结构化审批**——载荷（什么命令、什么 diff、命中什么规则）从不作为数据跨缝。各家只能：透传（PTY）、预放行（Orca 默认预填 `--dangerously-skip-permissions`/`--yolo`！）、假按键（claude-squad autoyes 写 `0x0D`；ccmanager 屏幕抓 ≤300 行喂 Haiku 审判后写 `\r`；Warp 远程转向是往会话里打字）。仅 ACP 承载带语义的权限选项（`allow_always` 等），且只对 ACP 原生 agent。
3. **检查点/rewind 事件**——Conductor 检查点是 harness 内部特性；herdr/ccmanager 的"恢复"= 重调 `--resume <id>`；无跨产品 rewind 语义（ACP 的 fork 尚在 RFD）。
4. **跨会话上下文**——会话 id 跨缝（为恢复），transcript 与工作上下文不跨；现状顶点是 ccmanager **逐字拷贝** `~/.claude/projects/<src>` 到目标目录。
5. **注意力意图**——"blocked"全靠像素反推或启发钩子；子代理树坍缩成"聚焦主 agent 终端"（Orca）。"agent 下一步想要什么"从不作为结构化数据存在。
6. **限额临近度**——只来自本地文件/配额端点（Orca、Superset），从不来自 agent 事件。

### 3.3 痛点证据清单

【已验证事实】（主题、证据、受影响者、结构性归因）

| 痛点 | 证据 | 受影响 | 为何是"分家"的结构性病 |
|---|---|---|---|
| 状态误读：抓取对象周更 | ccmanager #227（`esc to interrupt` 被删即误报）；herdr 保守 idle 回退 | ccmanager、herdr、Superset(#7395/#7417)、claude-squad | agent 厂商拥有终端 UI 且恕不通知；编排器无特权通道。只有拥有 agent 才能修 |
| 子代理工作上卷不可见 | Superset #7395（主 agent 等子 agent 时显示无工作） | Superset 及一切 pane 观察者 | 状态自主进程屏幕推断；agent 内部 spawn 从不上屏。须 agent 内部事件级插桩 |
| 审批策略 per-agent/per-vendor/per-session，无机群级 | Stoneforge 设计公理"默认无门，评审在 merge steward"；HN 审批疲劳引文（疲劳→`--dangerously-skip-permissions`）；ccmanager #194 求按 worktree 开关 | 全部编排器；Claude/Codex 的 auto 均单 agent | 权限活在各家信任模型内；不拥有工具调用边界就无法"机群范围批准这类动作" |
| 合并/择优是 worktree 并行的账单 | HN："五个并行 Claude 重写同一基类"；sathish316："这些工具没让合并更简单" | Superset、Cursor、claude-squad 等 | 编排器造出 N 个隔离状态却无 agent 意图的语义模型，整合被扔回 git 和人 |
| 静默清理/丢工作 | Windsurf 20 个 worktree LRU 自动删未合并工作；ccmanager #196 恢复坏；claude-squad #266 prompt 静默丢失 | Devin Desktop、ccmanager、claude-squad | 编排器管 worktree、agent 管会话——联合生命周期无主，各方都能毁对方所需状态 |
| 跨会话上下文只在人脑里 | Orcha 作者："我是人肉中间件，在 Claude 窗口间复制粘贴"；live-log-viewer："agent 悄悄死掉，我半小时后才发现"；CAS 作者：往复用器"写原始字节注入 prompt，worked, but barely" | 全体 | agent 看不见兄弟（厂商沙箱），编排器看不见意图（只有屏幕）。两者之间是渲染目标不是数据 API |
| 机群成本/限额事后补课 | Orca 热切账号的存在理由（2026-05/06 限额收紧）；Abralo/amux 逐 agent 计量被单点称道；限额下调+集体诉讼 | Orca、Abralo、amux | 限额在厂商侧按账号计；编排器只能展示燃烧不能调度燃烧——除非同时拥有 auth 与路由（agent 侧） |
| 通知真实性 | herdr 对未知提示按设计漏报；Superset 子代理期假 idle | herdr、Superset、ccmanager | 抓取派生的"needs you"徽章上限=抓取质量；假 idle 是注意力路由最坏默认，且是非集成 agent 的被迫回退 |

### 3.4 四条事后教训（停更/转型声明）

【已验证事实】

1. **纯编排器无法自立**：Vibe Kanban（2026-04-10 停维护声明）——日活数千、品类定义级功能（多 agent、diff 评论、实时预览、远程访问均为其首发），但"绝大多数是免费用户，找不到商业模式"：价值归于 agent 或订阅的拥有者，不归外壳。
2. **会话管理是功能不是产品**：Crystal → Nimbalyst 转型——替代品以编辑器（markdown/表格/Excalidraw/Monaco）领衔，会话管理降为一个 bullet：用户为"理解发生的地方"（diff、文档、设计）付费，不为进程表付费。
3. **agent 厂商从下方吃掉这个槽位**：Devin Desktop（原 Windsurf）把 IDE 改名"agent 指挥中心"；Claude Code 自出 Teams/auto 模式。独立编排器在与自己供应商的路线图赛跑。
4. **抓取式集成是必输的跑步机**：ccmanager #227 是模式缩影（检测→厂商改 UI→假 idle→补丁→循环）；存活者（herdr 钩子、KanVibe 钩子、Orca CLI）都移向 agent 自报状态——只有 agent 拥有者能可靠提供的东西。

### 3.5 ACP：唯一接近"投影"的协议，与其缺口

【已验证事实】ACP 是唯一让客户端视图字面上成为 agent 事件流投影的缝：每个 `session/update` 是 agent 发出的事件（消息块、tool_call 生命周期含 `kind`/`status`、diff 以 old/new 文本而非像素、plan、模式迁移、usage），`session/load` 定义为重放同一流；反向通道（`session/request_permission` 带 `allow_once|allow_always|reject_once|reject_always` 选项、elicitation、fs、terminal/*）让 agent 以类型化请求而非按键拉取人的决定。其缺口同样清单化：逐事件成本（usage 仅累计）、检查点/rewind（fork 是 RFD）、压缩事件（RFD）、限额临近、子代理树、以及**一切编排形态**——无多会话扇出、无 blocked/needs-attention 语义（协议假设客户端就是有人的编辑器）。【基于证据的推断】ACP 与我们协议同构（§2.1.5），做 ACP 适配层是把我们的事件流**翻译**给编辑器；而编排语义（多会话、blocked、机群策略）是 ACP 刻意不覆盖、正好由我们自己的编排层占据的空间。两者不竞争，是上下游。

## 4. 设计指导：把一体性兑现为 UX

### 4.0 边界说明（本方向超出现行 ADR 文本处）

维护者方向（2026-09-11）"既做 agent 又做编排器"超出两处现行文本：基线报告 §2.5.1 裁掉多会话 swarm 编排；ADR-0012 条目 3 把 dashboard 列为 candidate。本报告采用的边界：编排层 = **单用户多会话机群的投影/注意力/评审面**——不含跨节点任务分工（ADR-0002 排除项不变），不含 swarm 协作逻辑；其状态、审批、评审全部由我们自己的事件流驱动。落地时须一则 ADR 把此边界正式化（候选消费者：phase-5 控制面 ADR 或独立的编排层 ADR）。

### 4.1 总纲：一个事件流，四种投影

ADR-0005 的 JSONL 事件日志是轨迹资产；ADR-0013 的活流+命令通道让同一资产同时驱动四种投影：**会话投影**（TUI 的对话视图）、**编排投影**（多会话状态/注意力）、**评审投影**（diff/评论/合并门）、**进化投影**（skill/memory delta 与门禁）。市场痛点对照：

| 市场结构性痛点（§3.3） | 分家世界的 workaround | 我们的构造性答案（地基） |
|---|---|---|
| 状态误读 | 屏幕抓取/钩子启发 | 状态机从事件派生（ADR-0012 条目 3 已定） |
| 子代理不可见 | 无（聚焦主终端） | 嵌套运行=嵌套事件流（persona open item 已列 parent/child span 问题） |
| 无机群审批策略 | yolo 旗帜/LLM 审判假按键 | 审批=命令事件（ADR-0008 条目 4）；策略装饰器服务 N 会话（scoped-rules open item） |
| 评审往返丢上下文 | 拼文本 prompt | 评论=结构化对象入事件流，agent 读同一对象 |
| 成本不透明 | 读本地账本 | 事件日志精确可回放（ADR-0011 `/usage` 已定） |
| 会话脆弱（进程死/scrollback 丢） | 像素回放/`--resume` 外部程序 | attach=replay+sync+tail（ADR-0013）；会话存活不依赖进程 |
| 合并痛苦 | 选赢家/prompt 级 AI 冲突解决 | 诚实无银弹：小任务+worktree+PR 门+冲突移交 agent |
| 限额临近不可见 | 轮询 provider 端点 | 成本精确自有；限额仍是 provider 侧数据，不入事件流（诚实边界） |

### 4.2 会话层指导（TUI）

地板以 ADR-0011 条目 3 为底，按 §2.1 抬升项取舍：

- **采纳**：边界注入为默认转向语义（§2.1.2）；审批叠加按工具输入匹配与 turn/session 作用域（§2.1.3，配置层实现时）；rewind 四动作分阶段（§2.1.4，存储用影子 git 仓库）；子代理/后台任务面板（§2.1.5，随 persona/subagent ADR）；轮内设置转向；可组合状态行（Codex `/statusline` 式挑选-排序）；通知 = 失焦推送 + 聚焦即摘要（拉推双模）。
- **缓议**：alt-screen 双渲染（先看 Claude 触发策略实效，复审 ADR-0012 条目 2）；`/btw` 旁问（优雅但非地板）。
- **不追**：语音、agent teams 多人、会话分享链接（单人尺度，ADR-0011 既定不追项一致）。

### 4.3 编排层指导

- **状态机与真实性**：四态（idle/working/blocked/done，ADR-0012 条目 3）+ blocked 优先排序（herdr 评测佐证）+ 一条不变量：**宁可显示 unknown 不可显示假 idle**（ccmanager #227 教训）；我们的事件源使该不变量近乎免费。
- **注意力路由**：push（失焦 OSC/bell/native，crush 的 OSC 过 SSH）+ pull（聚焦即会话摘要）+ blocked/done 徽章上卷到会话列表与（未来）机群视图；通知真实性当产品核心做。
- **机群策略层（市场空白，§3.3 行 3）**：一个 auto-resolving 策略装饰器服务全部并发会话（scoped-rules open item 的自然泛化）；Stoneforge"默认无门、评审在合并"是反面极端，我们走中间：L0–L3 分层 + 规则 + 可选分类器（Claude `auto` 佐证分类器有效，但定位是舒适性不是安全机制——§7.1.1 纪律不变）。
- **worktree 工作流**：worktree-per-task + `.worktreeinclude` 惯例（ccmanager）+ **永不静默删除未合并工作**（Windsurf LRU 反例）。
- **best-of-n 扇出 + 并排 diff 比较**：被评测单点称道的编排动作（§3.4 正面清单）；候选，需 persona/subagent ADR 先行。
- **会话恢复纪律**：恢复=重放自有轨迹，永不依赖被管程序的外部 `--resume`（herdr 模式的脆弱性实证）。

### 4.4 评审层指导

- **评论=共享可寻址对象**（Conductor 模式），不拼文本 prompt（Orca/VK 模式是无共享事件流时的代偿，我们不需要）：评论对象入事件流（文件/行区间/侧别/线程/resolve 状态），agent 经工具读同一对象、可 resolve——评审队列同时是 orchestrator 可读队列。
- **评审即进化信号**：评论与拒绝沿 ADR-0011 条目 3 先例（rejection comment 进轨迹）进入进化环——评审层是自进化的人类反馈入口，这是分家世界结构上做不到的数据通路。
- **直改与评论分工**：typo 级人直接改 diff（零延迟），意图级走评论；**直改也作为事件入流**（Conductor "invisible to agent" 教训）。
- **合并门=PR gate**；冲突移交 agent（prompt 级即可，不承诺语义合并引擎）。

### 4.5 进化投影：全球独有的第四投影

【基于证据的推断】本调研确认：市场上没有任何 agent 或编排器有自进化 UX（skill/memory 的 delta、门禁、回滚的可视化）。ADR-0011 条目 4 已定"差异化必须可见"与复用审批机械；本条补设计原则：进化工作后台化永不阻塞交互（已定）；评审 UI 复用审批/diff 机械（已定）；**谱系可视化**（哪条 skill 从哪批轨迹长出、gate 结果与计数器的时间序列）是独有亮点——它把 §3.3"看不见 agent 内部"的行业痛点推到反面极致：我们的编排层连 agent 的**进化**都可见。

### 4.6 明确不做（含证据支撑）

语音输入、多人 multiplayer、插件/marketplace 生态、移动/web 前端（ADR-0011 既定不追；ACP + phase-5 远程 attach 覆盖其大半价值）；语义合并引擎（无人做到，单人尺度不追，§2.2.5）；WASM 插件运行时（ADR-0012 既定）；自然语言模式探测（ADR-0012 既定）。

## 5. 终端风格 Rust GUI 的技术路径（2026-09）

### 5.1 GPUI 三段式更正

【已验证事实】许可证：`gpui`=Apache-2.0（crates.io API，0.2.2），GPL 仅 Zed 应用层 crate（`terminal` 依赖 `alacritty_terminal`+`vte`、`ui` 组件库）——ADR-0011/0012/0013 修订的"GPL→独立仓库"前提不成立，仓库分离回落为发布/范围决策。【已验证事实】停更：crates.io 全部版本集中于 2025-10（0.1.0-test→0.2.2），0.2.2（2025-10-22）后至基线 2026-09-11 无新版——证实维护者"已停更"判断。【维护者实测，2026-09-11】git 版本依赖树复杂、使用有问题；佐证：macOS 可选拉 Zed font-kit fork 的 git 依赖（cargo-deny 视点）。【基于证据的推断】结论：许可证障碍消失，可用性障碍浮出——GPUI 实质路径只剩 git pin + 自行管理其依赖 churn，成本须与 egui/iced（成熟 crate）、Tauri+webview（Orca 实证路径）、自绘（tui-term 方向）同台权衡；此项留给 GUI ADR，本报告不预选。

### 5.2 可嵌入终端核心与 PTY 层

【已验证事实】`alacritty_terminal`（Apache-2.0，crates.io 有发布，网格/scrollback+`vte`+自带 tty PTY 模块）是 Rust 事实默认（Zed 内建终端即它，经 GPL 胶水）；`libghostty-vt`（MIT，C ABI，功能稳定但 API 在变动、未版本化、无官方 Rust 绑定）；wezterm `portable-pty` 0.9.0（跨平台 PTY 默认）；`vt100`（MIT，纯解析+内存屏幕）；`tui-term` 0.3.4（MIT，vt100 屏幕画成 ratatui widget，自承 WIP）。tokio 兼容的标准模式=阻塞读线程+channel 入异步运行时（Zed 与 tui-term 异步示例皆然）。

### 5.3 架构选项表

【已验证事实】（许可证列当日核实，注明者除外；GPUI 行按 §5.1 更新）

| 架构 | 许可证 | 成熟度 | 嵌终端 | markdown/diff | 成本 | 备注 |
|---|---|---|---|---|---|---|
| GPUI(git) + alacritty_terminal | 全 Apache/MIT | crate 停更，git churn 大，依赖复杂【维护者实测】 | 核心最佳，胶水自写 | 自建（Zed `ui` 是 GPL） | 高 | Rust 原生最大控制力 |
| egui/eframe + alacritty_terminal | 全宽松 | 较成熟、自承 WIP | epaint 画网格 | 富文本弱，自建 | 中高 | 最简单 native Rust；附赠 web |
| GUI=ratatui buffer 画到 GPU（tui-term 方向） | 全宽松 | tui-term WIP | vt100+portable-pty | 仅单元格网格美学 | 中 | **单渲染器打法**：一棵 widget 树两个后端 |
| Tauri + xterm.js + Rust 核心 | 全宽松（xterm.js MIT【未核实】） | 业界最成熟嵌终端路径 | xterm.js+portable-pty 过 IPC | 商品化（marked/Monaco） | UI 成本最低 | Orca/Wave 实证；双语言 |
| iced + alacritty_terminal | 宽松 | 实验性 | 网格上自写 widget | 自建；Elm 契合 block UI | 中高 | 终端先例最少 |

### 5.4 决策输入（事实，不含决定）

- 双渲染器维护是真正成本中心；tui-term 方向保单一 widget 树，天花板是单元格外观（无比例字体/真 markdown 排版/像素平滑滚动）。
- 富 markdown/diff 渲染要紧时：native Rust 栈全需自建，webview 侧商品化——Orca 与 Wave 尽管有 Rust/Go 核心仍落 Electron 类栈的原因。
- 嵌终端每条路径均已解：`alacritty_terminal` 默认核心、`portable-pty` 默认 PTY；`libghostty-vt` 有前途但未版本化。
- gpui 与 egui 原生覆盖 macOS/Linux/Windows；alacritty_terminal/portable-pty 已处理 ConPTY。
- 任何选项都是 adding-dependencies skill / 先问后动候选。

## 6. 排序问题：TUI 先行 vs 直取 GUI

【基于证据的推断】维持 TUI 先行（ADR-0011/0012 排序不变），五条理由：其一，生态方向反着走——编排器编排 TUI，跳过 TUI 等于自绝于 Orca/Superset/Zed/herdr 的免费编排与分发（§3.4 事后教训 3：厂商从下方吃掉槽位，反过来说，被编排物永有位置）。其二，ADR-0013 协议须由渲染范围最小的客户端先摇出缺陷；先 GUI 等于在协议未证实时同时背 markdown 排版、diff 渲染、嵌终端、窗口管理四份工作。其三，GUI 独占层（机群、注意力、评审往返）以并发会话为前提，对应 phase 5 前后。其四，daily-driver→轨迹→进化环要求最短路径到"可日用"。其五，"终端风格 GUI"仍须先嵌终端核心+块渲染，严格是 TUI 工作的超集。ACP 适配层是"GUI 之前的 GUI"（§3.5），建议升格排期。

## 7. 采纳候选清单（每条注明消费者）

消费纪律同 open-items.md：采纳或驳回后即删。

| # | 候选 | 出处 | 消费者 |
|---|---|---|---|
| 1 | 边界注入为默认转向语义；绑定刻意选择 | §2.1.2 | phase-1 TUI 地板重基线（ADR-0011 条目 3 修订） |
| 2 | 审批叠加按工具输入匹配 + turn/session 作用域；模式枚举保留为糖 | §2.1.3 | 配置层实现（scoped-rules open item） |
| 3 | rewind 四动作（restore/fork/summarize/re-decide）分阶段 | §2.1.4 | 检查点/rewind 实现 |
| 4 | 检查点存储用影子 git 仓库 | §2.1.4 | 检查点/rewind 实现 |
| 5 | 子代理/后台任务面板入地板 | §2.1.5、§3.3 行 2 | persona/subagent ADR + TUI 地板 |
| 6 | 轮内设置转向 | §2.1.2 | TUI 地板重基线 |
| 7 | 可组合状态行（挑选-排序式） | §2.1.5 相邻事实 | TUI 地板重基线 |
| 8 | 通知拉推双模（失焦推送+聚焦摘要）；OSC 过 SSH | §2.2.3 | TUI 地板重基线 |
| 9 | alt-screen 触发策略复审（"空间够就升舱"） | §2.1.1 | TUI 地板重基线（ADR-0012 条目 2） |
| 10 | 状态真实性不变量：宁 unknown 不假 idle | §2.2.3 | TUI 地板 + 编排层 ADR |
| 11 | 机群策略层：策略装饰器服务 N 会话 | §4.3 | 编排层 ADR（phase 5 前后） |
| 12 | worktree 工作流：.worktreeinclude + 永不静默删未合并工作 | §4.3 | 编排层 ADR |
| 13 | 评论=共享可寻址对象入事件流；直改亦入流 | §4.4 | 评审层/编排层 ADR |
| 14 | ACP 适配层排期 | §3.5 | open-items ACP 条目 → ADR |
| 15 | GPUI 前提修订（Apache-2.0 但停更+git 复杂；重锚分离理由） | §5.1 | GUI ADR / ADR-0013 条目 8 修订 |
| 16 | 编排层边界正式化（单用户多会话投影面；ADR-0002 排除项不变） | §4.0 | 编排层 ADR |

## 8. 来源清单

均为 2026-09-11 当日抓取核实；【未核实】条目正文已逐处注明。

- Claude Code：docs.anthropic.com/en/docs/claude-code/interactive-mode、/checkpointing；code.claude.com/docs/en/headless、/hooks、/agent-sdk（user-input、sessions）、/sub-agents
- Codex CLI：github.com/openai/codex（codex-rs/app-server）；developers.openai.com/codex/app-server、/exec-policy、/noninteractive、/cli、/cloud
- Gemini CLI：github.com/google-gemini/gemini-cli（docs/cli/checkpointing.md、themes.md、headless.md）
- Aider：aider.chat/docs/usage.html
- OpenCode：opencode.ai/docs（/server、/tui、/permissions）；github.com/sst/opencode
- Crush：github.com/charmbracelet/crush
- Goose：github.com/aaif-goose/goose；Amp：ampcode.com/docs；Cursor CLI：cursor.com/docs/cli/overview
- herdr：github.com/herdrdev/herdr；herdr.dev/docs（agents、integrations、socket-api、session-state、install）；herdr.dev/agent-guide.md
- ccmanager：github.com/kbwo/ccmanager（docs/status-hooks.md、gemini-support.md、auto-approval.md；issues #194、#196、#227）
- claude-squad：github.com/smtg-ai/claude-squad（session/tmux/tmux.go；issues #266、#325）
- Orca：github.com/stablyai/orca；www.onorca.dev（/docs/agents/claude-code、hooks-memory、usage-tracking、supported；/docs/review/annotate-ai-diff、commit-push；/docs/cli/orchestration）
- Warp：docs.warp.dev（/agents/cli-agents/overview、claude-code、agent-notifications、rich-input、remote-control；/terminal/blocks）；github.com/warpdotdev/warp
- Wave：github.com/wavetermdev/waveterm；Conductor：conductor.build（/docs、/docs/reference/harnesses/claude-code、agent-behavior、/changelog）
- Superset：docs.superset.sh（agent-status、mcp-server、usage、llms.txt 及 diff-viewer、pull-requests、use-with-ide）；github.com/superset-sh/superset（issues #7395、#7416、#7417、#7426）
- opcode：github.com/getAsterisk/opcode；Vibe Kanban：github.com/BloopAI/vibe-kanban；www.vibekanban.com/blog/shutdown
- Zed/ACP：zed.dev/docs/ai；agentclientprotocol.com（protocol/v1/overview、session-setup、prompt-turn、tool-calls、session-modes、terminals）；github.com/zed-industries/zed（crates/gpui、terminal、ui 的 Cargo.toml）
- GPUI crate：crates.io/api/v1/crates/gpui（版本时间戳与许可证核实）
- 编辑器与云 agent：cursor.com/docs/agent/overview；code.visualstudio.com/docs/copilot/chat/chat-agent-mode；docs.devin.ai；docs.windsurf.com/windsurf/cascade/worktrees；jules.google/docs；docs.github.com/en/copilot/concepts/coding-agent；devin.ai/desktop
- 终端核心：github.com/alacritty/alacritty（alacritty_terminal）；github.com/ghostty-org/ghostty；github.com/wezterm/wezterm（term/、pty/）；github.com/doy/vt100-rust；github.com/a-kenji/tui-term
- GUI 栈：github.com/emilk/egui；github.com/iced-rs/iced；github.com/makepad/makepad；github.com/tauri-apps/tauri；github.com/DioxusLabs/dioxus
- 痛点与事后分析：news.ycombinator.com/item?id=49195468、46690907、47267105、47599771、46368739、47324912、46027947、46924871、48804797、47388646、49586386、47341351、48832797、47104424；andrew.ooo/posts/orca-stablyai-parallel-coding-agents-ide-review、herdr-agent-multiplexer-terminal-review；github.com/rookedsysc/kanvibe
