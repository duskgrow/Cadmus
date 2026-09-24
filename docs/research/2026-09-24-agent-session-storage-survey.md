# Agent 会话存储模型调研：会话粒度、append-only 日志与 resume/fork/compaction 机制——2026-09-24 基线

> **Frozen research snapshot (baseline 2026-09-24).** Exhibit material for the
> ADRs in `docs/decisions/` — this is *not* living documentation and is never
> edited; where this document and the ADRs disagree, the ADRs win. Version,
> vendor and maintenance-status claims require re-verification at the start of
> the phase that depends on them (freshness policy: report §1.2.2). Current
> phase status lives in `docs/roadmap.md`.

调研基线：2026-09-24；语言：中文。

## 调研触发与问题

触发：TUI 的当前实现为**每个 prompt run 铸一条新 trace**（`TuiDriver::start`
每调用铸新 `trace_id`），每条 trace 的 `StartRun` 内嵌**此前的完整消息历史**——
存储字节量随对话长度平方增长，文件数随轮次线性增长，且 trace 间无谱系链接。
ADR-0009 item 4 假设的模型是"一个 session 一条持续增长的 trace"（resume =
重放事件前缀；fork = 复制前缀 + 记录谱系）。ADR-0022 把 resume/fork 定为
phase-1 closeout 集，会话模型必须在动工前定案。本调研为主流证据轮。

核心问题：主流 agent CLI 的会话持久化**粒度**是什么（每会话/每轮一个存储单元）、
写入模式（append-only 事件流 / 全量重写 / 数据库）、resume/fork/compaction 各如何
与落盘格式交互、历史是否被跨文件复制。

方法：三条并行调研线（Codex CLI、Gemini CLI、Claude Code + 其他），全部当日
（2026-09-24）对**源码或官方文档**核验；无法核验的条目保留 UNVERIFIED 标记（§5）。

## 逐工具发现

### Codex CLI（openai/codex，Rust；证据最详尽，与 Cadmus 最同构）

核验基准：`main` @ 7dae8c53（2026-09-24 当日提交）。

- **粒度**：一个会话（thread）一个文件
  `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<thread_id>.jsonl`。
  绝无每轮一文件；同 thread 出现第二个文件的唯一情形是 revert（见下）。
- **写入模式**：append-only JSONL 事件流（每行 `{timestamp, ordinal, ...item}`），
  后台 writer task + mpsc 通道落盘。记录类型：`SessionMeta`（仅首行）、
  `ResponseItem`、`EventMsg`（TurnStarted/TurnComplete/TokenCount/…）、
  `Compacted`、`TurnContext` 等；持久化有显式过滤策略（流式 delta、审批请求等
  瞬态**不**落盘，`codex-rs/rollout/src/policy.rs`）。
- **Resume**：`codex resume` 以 append 模式打开**同一文件**继续写
  （`open_rollout_for_append`，ordinal 从扫描续起；不写新 `SessionMeta`）；
  重放整文件重建历史，compaction 记录充当重放边界。
- **Fork**：一等子命令 `codex fork`，谱系记入子文件首行 `SessionMeta`
  （`forked_from_id`、`forked_from_ordinal_exclusive`、`parent_thread_id`）。
  两种物理模式：**Copied**（父前缀字节拷入子文件）与 **Referenced**（不拷贝，
  按 `(rollout id, ordinal, byte offset)` 引用父的不可变前缀，读取侧走多段链）。
- **Revert（undo）**：写一个**新的不可变** rollout 文件引用保留前缀，旧文件不动，
  唯一可变步骤是 SQLite 里 rollout 路径指针的 CAS 交换。
- **Compaction**：向同一文件**追加** `Compacted` 记录（内嵌摘要 +
  `replacement_history` 完整快照 + token 用量），压缩前历史**原样保留在日志里**；
  重放时以最新 compaction 为边界，只重放其快照 + 后缀。
- **重复策略**：事件 exactly-once；仅两处有意快照（compaction 内嵌快照、Copied
  fork 的前缀拷贝）。**每轮一文件 + 内嵌全量历史在这里没有对应物。**
- **索引与 SSOT 分层**：SQLite sidecar（`state_5.sqlite` 等）只做列表/搜索**索引**，
  JSONL 是 SSOT——列表 filesystem-first、DB fallback + read-repair。
  这正是 ADR-0005 "JSONL SSOT，SQL 为 phase-2 投影" 的外部同构验证。
- **保留**：无 TTL 删除；>7 天的 rollout 后台 zstd 压缩为 `.jsonl.zst`（追加前
  解压回明文）；显式 `codex archive` / `codex delete` 生命周期操作。
  注：其 git ghost-commit 检查点特性**已被移除**（`Feature::GhostCommit` =
  Removed），undo 改由 revert 新文件承担。

来源：`codex-rs/rollout/src/{recorder,policy,compression,maintenance}.rs`、
`codex-rs/history/src/lib.rs`、`codex-rs/thread-store/src/local/{rollout_lineage,revert_thread}.rs`、
`codex-rs/core/src/session/{mod,rollout_reconstruction}.rs`、
`codex-rs/protocol/src/protocol.rs`（均在 github.com/openai/codex `main`）。

### Gemini CLI（google-gemini/gemini-cli，TypeScript）

核验基准：`main`（2026-09-24）。

- **粒度**：一个会话一个文件
  `~/.gemini/tmp/<project-id>/chats/session-<ISO分钟>-<sessionId前8位>.jsonl`；
  子 agent 会话按父 id 嵌套目录。
- **写入模式**：append-only JSONL（`fs.appendFileSync`）。行类型：元数据首行、
  消息行（带 uuid；更新 = 追加同 id 全量副本，重放时 last-wins）、
  元数据补丁 `{$set: …}`、rewind 标记 `{$rewindTo: <messageId>}`（字节不删，
  重放时截断）。**曾从"单 JSON 文档全量重写"迁移到 append-only JSONL**——
  loader 保留 legacy 回退并在 resume 旧文件时改写为新格式。
  即：他们正是从"类全量"模型迁往 append-only 的。
- **Resume**：`--resume` / 会话浏览器加载后**继续追加同一文件**
  （`ChatRecordingService.initialize` 的 resumed 分支沿用原路径）。
- **Fork**：`/chat save <tag>` 写全量历史快照 `checkpoint-<tag>.json`
  （不可变，官方称"named branch points"）；resume 该 tag = 把副本导入当前会话，
  快照文件不动——拷贝式分支，非破坏性续写。
- **Compaction**：压缩后向同一文件追加 `$set:{messages: [...]}` 全量快照记录
  （loader 视为"清空重建"），压缩前的消息行**保留在日志里**；
  `/rewind` 跨压缩点工作（文档明示）。
- **文件检查点**（默认关闭）：shadow git 仓（`~/.gemini/history/<project-id>/`，
  与用户 git 完全隔离）+ 每工具一个 checkpoint JSON（内嵌全量历史副本）；
  `/restore` = `git restore --source <hash> .` + `git clean -fd` 后重新提议原工具调用。
- **保留**：`general.sessionRetention` 默认 30 天（会话）；checkpoint 未见保留
  策略（UNVERIFIED）。

来源：`packages/core/src/services/{chatRecordingService,chatRecordingTypes,gitService}.ts`、
`packages/core/src/core/{client,geminiChat,logger}.ts`、`packages/core/src/commands/restore.ts`、
`docs/cli/{session-management,checkpointing,rewind}.md`（均在 github.com/google-gemini/gemini-cli `main`）。

### Claude Code（闭源；官方文档 + 社区格式分析）

核验基准：docs.claude.com 当日页面 + claude-code-log 的逐字段格式模型。

- **粒度**：一个会话一个文件
  `~/.claude/projects/<目录slug>/<session-uuid>.jsonl`；子 agent 的转录在
  `<session-id>/subagents/` 下的独立文件。
- **写入模式**：append 式 JSONL 事件流（每行一个消息/工具/元数据条目；
  条目间以 `uuid`/`parentUuid` 构成 DAG；官方称"saved continuously…as you
  work"，且两个终端同时 resume 同一会话会"interleave into one transcript"——
  等价于承认同一文件持续追加）。
- **Resume**：`--continue` / `--resume` **续用同一 session id、追加同一文件**；
  `--fork-session` 才铸新 id。
- **Fork**：`--fork-session` 与 `/branch`（拷贝转录到分支点、切换写入目标，
  原文件不动）；谱系是结构性的（共享 uuid 前缀 + 新 session id），无显式
  forked-from 字段。
- **Compaction**：`/compact` 与 auto-compact 向同一文件追加 `type:"summary"`
  条目与 `compact_boundary` 系统条目（含 preTokens/postTokens/trigger 等元数据），
  **压缩前的行全部保留**——社区工具因此能渲染完整压缩前历史。
- **保留**：默认 30 天清扫（`cleanupPeriodDays`）；可用
  `--no-session-persistence` 抑制。

来源：docs.claude.com/en/docs/claude-code/{sessions,cli-reference,hooks,context-window}；
github.com/daaain/claude-code-log `models.py`。

### Aider / OpenCode / Goose（简表）

| 工具 | 粒度 | 写入模式 | Resume | Fork | Compaction 与日志 |
|---|---|---|---|---|---|
| Aider | 无会话概念；每仓库一份滚动文件 | append-only **Markdown** 展示日志（`.aider.chat.history.md`），非事件日志 | `--restore-chat-history`（默认关）续写同一文件 | 无（`/save`/`/load` 只还原文件集） | 仅内存压缩；日志永不截断 |
| OpenCode | 一个会话一条记录 | **已从每实体一个 JSON（全量重写）迁移到单一 SQLite** `opencode.db` | 续用同 id，新轮次写新行 | `--fork` 铸新 id + 拷贝消息（可到指定 messageID），无显式谱系字段 | 追加 compaction 摘要记录并**隐藏**旧消息（不删除）；旧工具输出打压缩标记 |
| Goose | 一个会话一条记录 | **v1.10.0 起从每会话一 JSONL 迁到单一 SQLite** `sessions.db` | 续用同 id | `--fork` / Duplicate / 消息编辑派生，均为拷贝 | 官方明示"此前对话保持可见"，仅摘要进上下文 |

## 横切结论

1. **一致性结论**：六家无一采用"每轮一文件"，无一在正常路径把全量历史嵌入
   每条新记录。全部保持**一个会话一个持久单元，新事件原地追加**（文件 append
   或 DB 插行）。Cadmus 当前的 per-turn trace + 内嵌全量历史在主流中**没有对
   应物**——Gemini 的旧全量 JSON 模型是最接近的先例，而他们已经迁走。
2. **Resume 的主流语义 = 同一存储单元上继续追加**（Codex/Gemini/Claude 均如此；
   DB 系续用同 id）。重放边界由 compaction 记录/快照承担，原始日志永不重写。
3. **Fork 的谱系记录分三级**：Codex 的显式元数据（`forked_from_id` + 位置，
   还支持零拷贝的 referenced fork）> Claude Code 的结构性子串（共享 uuid 前缀）
   > Gemini/Goose/OpenCode 的纯拷贝（无字段）。ADR-0009 已选"谱系记入
   start_run attributes"，与最强者（Codex）同向。
4. **Compaction 与 append-only 不冲突**：三家的做法同为"追加 marker + 内嵌
   快照，重放时以 marker 为边界"——为 ADR-0007 的分层压缩落地提供了现成模式
   （压缩后重放从快照起，日志全文保留供轨迹分析）。
5. **Rewind/检查点**：Gemini 的 `$rewindTo` 日志标记 + shadow git 快照、Codex 的
   revert-新不可变文件 + 指针 CAS，均与 closeout 集的 checkpoint/rewind 同构；
   Codex 移除 git ghost-commit 检查点的教训（改由 revert 文件承担）值得在
   closeout ADR 中正面回应。
6. **规模治理**：文件数问题的主流答案不是"更少文件的更大单元"（他们早已是
   一会话一文件），而是压缩与清扫——Codex 的 >7 天 zstd（追加前解压）、
   Claude Code 与 Gemini 的 30 天保留窗。Cadmus 的日分片 + 读写根分层已具备
   接入同类策略的接缝。
7. **JSONL SSOT + SQLite 索引（Codex）与"文件 → 单库"迁移（OpenCode、Goose）
   并存**：两条路线都被验证可行；Codex 的分层（JSONL 为 SSOT、SQLite 可重建
   索引 + read-repair）与 ADR-0005 的既定方向一致，OpenCode/Goose 的单库路线
   则是 phase-2 SQL 仓的备选参照。

## 对本项目会话模型决策的意义

- 调研一致支持**方向 1（一 session 一 trace，轮次追加进同一 trace）**：
  它是全部六家的实际形态，且与 ADR-0009 的既定心智模型吻合。
- 方向 2（per-turn trace + 谱系字段）在主流中没有先例；其唯一卖点（每条
  trace 可独立重放）被三家的 compaction-marker 模式以更廉价的方式覆盖
  （重放边界 = 快照记录，而非文件边界）。
- 落地注意点（写 closeout ADR 时逐条回应）：loop 需要"run 结束后待命、
  continue 命令追加新 turn"的状态机；compaction 采用 marker+快照模式；
  fork 谱系沿用 ADR-0009 的 attributes 方案（Codex 级别）；rewind 参照
  Gemini `$rewindTo`（字节保留、重放截断）与 Codex revert（不可变新文件）
  二选一或分层；保留策略挂到既有分片/分层接缝上。

## 局限与 UNVERIFIED 清单

- Claude Code 为闭源，写入模式与 fork 谱系字段依赖官方文档措辞与社区格式
  分析，无字节级承诺（官方最接近的表述：双终端 resume 会交织进同一转录）。
- Gemini CLI：checkpoint JSON 的精确写入调用点、`/rewind` 的 UI 侧挂接、
  `--resume` 的最后一层胶水代码未逐行核验（链路其余环节均有源码）。
- OpenCode 从 JSON 迁到 SQLite 的具体版本未核实；Goose 旧 JSONL 的写入模式
  （append vs 重写）未核实（仅存读路径源码）。
- Codex 官方文档站当日对抓取返回 403，其结论全部基于源码而非文档散文。
