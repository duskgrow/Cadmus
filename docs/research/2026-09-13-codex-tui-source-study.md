# Codex CLI TUI 源码调研:inline 渲染、流式 markdown、事件循环与测试栈——2026-09-13 基线

> **Frozen research snapshot (baseline 2026-09-13).** Exhibit material for the
> ADRs in `docs/decisions/` — this is *not* living documentation and is never
> edited; where this document and the ADRs disagree, the ADRs win. Version,
> vendor and maintenance-status claims require re-verification at the start of
> the phase that depends on them (freshness policy: report §1.2.2). Current
> phase status lives in `docs/roadmap.md`.

调研对象:openai/codex,commit `dfaf451426868c22e6859f5494150fd6338c3257`(2026-09-13 04:59 UTC,当日最新)。
调研范围:`codex-rs/tui/`、`codex-rs/file-search/`、`codex-rs/utils/fuzzy-match/`、workspace 根 `Cargo.toml`/`Cargo.lock`。
方法:git clone --depth 1 后直读源码;所有版本号以 `Cargo.toml`/`Cargo.lock` 为准。
置信度标注:【已验证事实】= 源码直读;【基于证据的推断】= 由已验证事实经明示推理链得出。
消费者:ADR-0018(TUI 实现架构)。

## 0. 第三方依赖清单(TUI 相关)

| crate | 版本 | 来源 | 用途 | 白名单风险 |
|---|---|---|---|---|
| ratatui | 0.30.2 | crates.io | 渲染框架。workspace features: `crossterm, layout-cache, underline-color`;tui crate 追加 `scrolling-regions, unstable-backend-writer, unstable-rendered-line-info, unstable-widget-ref` | MIT,无风险 |
| crossterm | 0.29.0 | **git patch**(`openai-oss-forks/crossterm` rev `45fecb9`,`[patch.crates-io]`) | 终端 IO、事件流 | MIT,但 **git 依赖违反"只用 crates.io"约束** |
| pulldown-cmark | 0.10.3 | crates.io(`default-features = false`,tui 加 `html` feature) | markdown 解析 | MIT,无风险 |
| syntect | 5.3.0 | crates.io | 语法高亮 | MIT,无风险 |
| two-face | 0.5.1 | crates.io(`default-features = false` + `syntect-default-onig`) | syntect 语法/主题包 | 【基于证据的推断】MIT/Apache-2.0,需 cargo-deny 核实 |
| diffy | 0.4.2 | crates.io | unified diff 解析 | MIT/Apache-2.0,无风险 |
| nucleo | 0.5.0 | **git 依赖**(`helix-editor/nucleo` rev `4253de9`) | @ 文件模糊匹配(仅 `codex-file-search` 用) | **双重风险:git 依赖 + 【推断】MPL-2.0 许可证不在白名单** |
| textwrap | 0.16.2(lock 里实际有 0.11.0 与 0.16.2 两份,tui 用 workspace 的 0.16.2) | crates.io | 折行辅助 | MIT |
| unicode-width / unicode-segmentation | 0.2 / 1.12.0 | crates.io | 宽度/grapheme | MIT/Apache-2.0、Unicode-3.0 系 |
| insta / vt100 / pretty_assertions | 1.46.3 / 0.16.2 / - | crates.io(dev-dependencies) | 测试 | Apache-2.0 / MIT,无风险 |
| tokio | 1 | crates.io | 异步运行时 | MIT |

**最重要的两个发现**:Codex 为了拿到 `crossterm::event::discard_buffered_input()` 和 `InputDiscardStatus`(见 `tui/input_boundary.rs:30,48`,标准 crossterm 无此 API)而维护了 crossterm fork;nucleo 直接 pin 在 helix-editor 的 git 仓库。Cadmus 的"只用 crates.io + 许可证白名单"约束下,这两处都需要替代方案(详见 §10)。

## 1. inline 渲染机制

【已验证事实】Codex 的 TUI **默认 inline,不用 ratatui 的 `Viewport::Inline`**。全仓库 grep 不到任何 `Viewport::` 引用。做法是 **fork 了 ratatui 的 `Terminal`**:`custom_terminal.rs` 文件头保留 ratatui MIT 许可证声明("This is derived from `ratatui::Terminal`"),自管 `viewport_area: Rect`(屏幕上的一块矩形,默认贴底)与 `last_known_screen_size`。

关键结构:

- **`Tui::draw(height, draw_fn)`**(`tui.rs:990`):每帧先用 `desired_height(width)` 算出 chat widget 想要的高度,viewport 高度 = `height.min(screen_size.height)`。若 viewport 底部超出屏幕,调用 `ScrollbackStrategy::grow_viewport` 把现有内容上滚腾位。整个 draw 包在 `stdout().sync_update(...)`(synchronized update,2026h 模式)里防撕裂(`tui.rs:1003`)。
- **历史写入 scrollback 是 escape-sequence 操作,不是 ratatui 渲染**:`insert_history.rs` 文件头原话 "Codex uses the terminal scrollback itself for finalized chat history, so inserting a history cell is an escape-sequence operation rather than a normal ratatui render"。完成的 cell 行先入 `pending_history_lines` 缓冲,下一帧 `flush_pending_history_lines` 时,用 DEC scroll region(`SetScrollRegion(1..viewport.top())` + 逐行 `\r\n` + `ResetScrollRegion`)把行写进 viewport 上方的 scrollback 区,光标位置全程保持中性(`insert_history.rs:106-228`,Standard 模式)。写入的行数是预先按当前宽度 wrap 好的(`wrap_history_hyperlink_lines`),URL 行不切分以保持可点击。
- **终端差异用策略枚举收口**:`tui/scrollback.rs` 的 `ScrollbackStrategy::{Standard, Zellij, FullScreen}`,按 `codex_terminal_detection` 探测结果选择。FullScreen(Windows Terminal)的注释说明了原因:局部 DEC 滚动区会把行丢弃而不是推进 scrollback,所以改为整屏滚动(`scrollback.rs:50-79`)。
- **历史/活跃区边界**:`Terminal::note_history_rows_inserted`(`custom_terminal.rs:571`)记账写入的行数;`viewport_area.y` 随写入量下移。边界就是一个 `Rect`,没有任何 widget 树持有历史——写出去的行归终端所有。
- **resize**:`transcript_reflow.rs` 文件头给出核心原则——"Terminal scrollback is not a retained widget tree",宽度变化时以内存中的 transcript cells 为 SSOT,**清掉 Codex 拥有的 scrollback 行并按新宽度重放**;75ms 去抖(`TRANSCRIPT_REFLOW_DEBOUNCE`);`tui.rs:1130 draw_with_resize_reflow` + `app/resize_reflow*` 执行。流式中的 cell 用 `StreamCore::set_width` 重渲重建(`streaming/controller.rs:273`)。
- **alt-screen 只用于临时 overlay**:`enter_alt_screen`(`tui.rs:844`)保存 inline viewport,离开时恢复(`alt_saved_viewport`),transcript 全屏查看(Ctrl+T)等用;可被 `set_alt_screen_enabled(false)` 完全禁用。

【基于证据的推断】fork ratatui Terminal 的动机是 `Viewport::Inline` 无法表达"动态高度 + 历史逃逸序列写入 + 滚动区策略分派 + 游标状态失效控制(`invalidate_cursor_state`/`invalidate_viewport`)"这一组合。Cadmus 若走 inline,大概率也需要这层薄 fork,而不是直接用 `Viewport::Inline`。

## 2. 流式 markdown

【已验证事实】管线分层(全部在 `tui/src/`):

1. **源收集(不解析)**:`markdown_stream.rs` 的 `MarkdownStreamCollector` 是 **newline-gated** 累积器。`push_delta` 只追加原始文本;`commit_complete_source()` 只提交到最后一个 `\n` 为止的字节区间,尾部不完整行留在 buffer——"prevents the live stream from rendering incomplete markdown blocks that may change meaning"(`markdown_stream.rs:78-93`)。未闭合代码块、半截表格这类不完整 markdown 就**天然不进入渲染**,不存在"修半个标签"的逻辑。
2. **增量渲染**:`streaming/render.rs` 的 `StreamingRender` 把已渲染结果按"顶层 block 边界"切分:`last_top_level_block_start` 之前的内容只渲一次永久保留,只有最后一个顶层 block 随新 committed source 重渲(因为 list tightness、setext 标题、fence、表格都可能被后续行改变语义)。两个全文级例外:出现 reference-style link definition 或 inline-visualization 指令时回退全量重渲。
3. **开放代码 fence 快速路径**:`streaming/code_fence.rs` 的 `OpenCodeFence` 保守识别"一个开放的、顶层的、带语言标注的 fence",用 `StreamingCodeHighlighter`(`render/highlight_streaming.rs`)持有 syntect 的 `(HighlightState, ParseState)`,对**每个新到完整行**追加高亮,避免重跑整个 fence。任何"可能是闭合行"的行(保守误判也接受)立即回退到全量 CommonMark 路径。
4. **解析与渲染**:pulldown-cmark `Parser::new_ext` + `ENABLE_STRIKETHROUGH | ENABLE_TABLES` + `into_offset_iter()`(`markdown_render.rs:338-341`),事件流经自研 writer(`MarkdownWriter`,2700+ 行)产出 ratatui `Line`。表格是重头戏:累计进 `TableState` 后跑五步管线(溢出过滤→列数归一→内容感知列宽分配→展示选择→溢出追加);列分 Narrative/TokenHeavy/Compact 三类分配宽度,放不下时**转置为 key/value 记录**(`markdown_render.rs` 文件头)。流式期间表格有专门 holdback:`table_holdback.rs` 检测到 pipe-table 头部后,从表头起全部保持为可变 tail 直到流结束(`streaming/controller.rs` 文件头 "Table holdback" 节)。
5. **语法高亮**:syntect 5.3.0 + two-face 0.5.1(`render/highlight.rs:57-70`,`SYNTAX_SET: OnceLock<SyntaxSet>` 用 `two_face::syntax::extra_newlines` 初始化,主题 RwLock 可热切换,带 `theme_revision` 失效机制;超长行/超大输入有 plain-text 降级)。diff 高亮按 hunk 整块拼接以保持 parser state(`diff_render.rs` 文件头)。
6. **两区域显示模型**(`streaming/controller.rs` 文件头):stable region(commit 进 scrollback,带动画队列)+ tail region(可变,渲染在活跃 cell 槽位)。`finalize` 时以 **item completion 的完整文本为准**做 consolidation——注释原话 "Item completion is authoritative. Use it for consolidation so any deltas dropped by a saturated transport cannot truncate the transcript"(`chatwidget/streaming.rs:172-175`)。

【基于证据的推断】这套设计的核心取舍是"原始源是 SSOT,渲染是派生"——resize、主题切换、finalize 全部通过重渲解决,不做字节级 remap。这与 Cadmus 事件溯源架构天然契合(事件 = 源,渲染 = 物化视图)。

## 3. 事件循环架构

【已验证事实】

- **接线**:crossterm `EventStream`(feature `event-stream`)被 `EventBroker`(`tui/event_stream.rs`)单例持有,可 **drop/recreate**(pause/resume)——文件头注释解释了原因:不 drop 的话 crossterm 的 reader 线程会持续读 stdin,会偷走外部编辑器(vim)的输入和终端查询响应。`TuiEventStream` 把三路合并:`draw_stream`(broadcast,帧通知)+ crossterm 事件 + `resume_stream`(watch),poll 时 **round-robin 交替** draw 与输入,"approximate fairness + no starvation"(`event_stream.rs:316-340`)。另有 tmux/Windows 用的 `SizeMonitor` 轮询尺寸变化注入 Resize。
- **主循环**(`app/startup.rs:1062` 的 `select!`):四个分支——`app_event_rx`(AppEvent,来自 app-server 与内部任务)、`active_thread_rx`、`tui_events.next()`(终端事件)、重连 future。终端输入分支带 `if` 条件守卫:有 pending 启动事件时**暂缓处理键盘输入**(事件仍在流里排队,不丢)。
- **帧调度(每事件重绘 vs 节流)**:**节流 + 合并**。`FrameRequester`/`FrameScheduler`(`tui/frame_requester.rs`)是 actor 模式(文件头引用了 ryhl.io 的 Actors with Tokio):任何 widget/后台任务持 `FrameRequester` 句柄发 `Instant` 请求,scheduler 任务合并请求、经 `FrameRateLimiter` **钳制到 120 FPS**(`frame_rate_limiter.rs`,`MIN_FRAME_INTERVAL`),到点往 broadcast 发一个 Draw。测试证明三次连续 `schedule_frame()` 只产生一次 draw。
- **LLM chunk 路径上的批处理**:`on_agent_message_delta` → `StreamController::push(delta)`(newline-gated 提交进队列)→ `sync_active_stream_tail` + `request_redraw` + `AppEvent::StartCommitAnimation`(`chatwidget/streaming.rs:182-214`)。然后 **commit tick 以帧节奏**(120 FPS,`COMMIT_ANIMATION_TICK = TARGET_FRAME_INTERVAL`,`app.rs:432`)驱动 `AdaptiveChunkingPolicy`(`streaming/chunking.rs`):双档位 + 滞回——Smooth 档每 tick 只放出 1 行(制造打字机效果),CatchUp 档在队列深度/最老行龄超阈值时一次性排空积压,退出阈值更低且有 hold 窗口防抖。文件头甚至附了调参指南("lag starts too late: lower enter thresholds")。
- **"输入永不阻塞"**:【已验证事实】键盘事件路径(`App::handle_tui_event` → `handle_key_event` → composer)是同步内存操作,全程无 await 网络;绘制被帧率钳制;输入与 draw round-robin 公平调度。另有防御性设计:`input_boundary.rs` 在安全敏感画面(审批)出现前丢弃缓冲输入,避免预输入误触审批选项。

【基于证据的推断】120 FPS 上限比常见的 30/60 FPS 激进,实际渲染成本靠 ratatui 双缓冲 diff(`custom_terminal.rs:602 diff_buffers`,逐 cell 比对)控制;Cadmus 可先从 60 FPS 起步。

## 4. composer

【已验证事实】**完全自研**(`bottom_pane/textarea.rs` 4619 行 + `textarea/` 子目录 + `chat_composer.rs` 13048 行)。tui-textarea **不是依赖**(Cargo.toml 无此 crate;`app.rs:927` 的注释仅引用其 CRLF 处理作文档)。结构:

- `TextArea`(`textarea.rs:134`):`text: String` + `cursor_pos` + `wrap_cache: RefCell<Option<WrapCache>>` + `elements: Vec<TextElement>` + 单条目 `kill_buffer` + Vim 状态机。grapheme 级移动(unicode-segmentation),词操作基于 `split_word_bound_indices` 加自有 `is_word_separator` 细分。
- **TextElement 是核心抽象**:@提及/图片占位符等"原子元素"带 `id + range`,每次编辑通过 `shift_elements`(`textarea.rs:1840`)原子地平移/删除,渲染成带背景色的标签,删除时整体消失。
- **选择(selection)**:【已验证事实】**没有通用的 Shift 选择系统**(grep 无 selection 字段);剪贴交互走 kill buffer(Ctrl+K 杀 / Ctrl+Y 取,单条目、区分 characterwise/linewise,可跨 composer 快照传递)。
- **撤销/重做**:`chat_composer/vim_history.rs` 的 `VimHistory { undo, redo, pending }`——**快照式**(snapshot 整个 `ComposerDraft`:文本 + elements + 图片附件 + mention bindings + pending pastes),而非增量 diff;有界:64 步 / 1MB(`MAX_VIM_UNDO_STEPS`/`MAX_VIM_UNDO_BYTES`);Vim 事务分组(`begin_vim_edit`/`finish_vim_edit`),取消命令不驱逐已提交历史。绑定在 Vim normal 模式 `u` / Ctrl+R(keymap.rs:1714);insert 模式 Ctrl+R 留给历史搜索。
- **多行与折行**:自有软折行(`wrapping.rs`+`wrap_cache`,满行保留续行插入点、尾随空格也折行、URL 跨折行保留 OSC8 超链接目标)。
- **大块粘贴**:双通道——有 bracketed-paste 的终端走 crossterm `Event::Paste`(CRLF→LF 归一化);**没有的(如 Windows)**走 `paste_burst.rs` 的 `PasteBurst` 纯状态机:计时/计数启发识别粘贴式字符流,缓冲成单个 Paste 字符串,期间 Enter 解释为换行而非提交,首个 ASCII 字符短暂扣留防闪烁。
- Vim 模式完整:normal/operator-pending/text-object/search(`/`),`vim_commands.rs` 等。

## 5. picker / 补全

【已验证事实】**两套并存**:

- **@ 文件补全**:`codex-file-search` crate 用 **nucleo**(git pin)+ `ignore` 遍历。`create_session` 起两个 OS 线程——walker_worker(遍历注入)与 matcher_worker(nucleo 匹配);TUI 侧 `FileSearchPopup` 收 `FileSearchResult` 刷新,match indices 用于高亮命中字符。即"后台索引 + 增量查询"的完整 nucleo 用法。
- **斜杠命令/通用过滤**:`codex-utils-fuzzy-match` 是**约 60 行自研**的大小写不敏感子序列匹配器(返回命中字符索引 + 分数,显式处理 lowercase 扩张字符如 ß→ss 的索引映射),用于 `slash_commands.rs` 等列表过滤。
- **命令面板/选择弹窗**:`selection_popup_common.rs` 的 `GenericDisplayRow`(含 `match_indices`、`wrap_indent`),手动行布局渲染。
- **picker + preview**:resume picker(`resume_picker.rs` + `resume_picker_transcript_preview.rs`)最接近"fuzzy+preview 万能 picker"的参照:列表 + 后台异步加载的会话 transcript 预览(`PickerLoadRequest::Preview` 经 `bg_tx` 回传,按 thread_id 缓存 `TranscriptPreviewState`)。

【基于证据的推断】Codex 没有统一的"万能 picker"抽象——文件、命令、会话恢复、主题各是独立 popup。Cadmus 的"fuzzy+preview 万能 picker"目标反而更收敛,可以用一套 nucleo 风格(或 skim/fuzzy-matcher)核心 + 统一 preview 槽位覆盖所有场景。

## 6. diff 渲染

【已验证事实】

- **生成**:diff 来源不是 TUI 算的——审批里的 patch 文本来自 core/app-server(`FileChange { unified_diff }`,`diff_model.rs`);`/diff` 斜杠命令则直接执行真实 `git diff`(30s 超时,禁用 hooks,`get_git_diff.rs`)。
- **解析**:`diffy`(`diff_render.rs:558 diffy::Patch::from_str`,`Hunk`、`Line::{Insert,Delete,Context}`)。
- **着色/渲染**:全自研——右对齐行号 + gutter 符号(`+`/`-`/空格)+ 按文件扩展名走 syntect 高亮(hunk 整块拼接保 parser state,跨 hunk 不续状态);**主题感知**:`DiffTheme` 按终端背景明暗选 dark 绿/红 tint 或 GitHub 浅色 pastel,并按 truecolor/256/16 色量化;若语法主题为 `markup.inserted`/`markup.deleted` scope 定义了背景色则优先采用;长行硬折行且样式跨折行保留。
- **审批内嵌**:`approval_overlay.rs` 把 `FileChange` 渲染进 ListSelectionView 的内容区(选项路由决策事件回 app;注释强调它只做呈现不做安全判断)。

## 7. 状态行

【已验证事实】正好是"pick-and-reorder"模型:

- **数据结构**:`StatusLineItem` enum(`status_line_setup.rs:56`,约 25 个变体:ModelName/GitBranch/ContextRemaining/FiveHourLimit/CodexVersion…),strum kebab-case 序列化进配置;配置即**有序列表**(`status_line_items: Vec<String>`)。
- **解析**:`ChatWidget::status_line_value(item) -> Option<String>`(`chatwidget/status_surfaces.rs:707`),`None` 语义是"暂时无数据,省略"而非错误——保证部分数据未就绪时状态行仍可读。
- **组装**:`refresh_status_line_from_selections`(同文件 202 行)按配置顺序过滤出可用段,`status_line_from_segments` 拼成单个 `Line<'static>` 存进 footer 的 `status_line_value` 缓存,帧渲染时直接画。
- **配置 UI**:`status_line_setup.rs` 用 `MultiSelectPicker`:空格勾选、**左右箭头重排**、底部实时预览。
- 另有独立的 `status_indicator_widget`(转圈动画)和 `effort_status_line`,与可组合状态行是分离机制。

## 8. 测试

【已验证事实】

- **主力不是 ratatui TestBackend,而是 vt100 全模拟**:`test_backend.rs` 的 `VT100Backend = CrosstermBackend<vt100::Parser>`——escape 序列写进真实终端模拟器,可断言最终屏幕内容**和 scrollback**(`vt100::Parser::new(h, w, scrollback_len)`),这对 inline 架构是刚需(TestBackend 没有 scrollback 概念)。
- **insta snapshot 极其重度**:全仓库 `.snap` 文件 **990 个**,161 个源文件引用 `insta::`;快照目录就放在被测模块旁(`chatwidget/snapshots/`、`streaming/snapshots/` 等)。
- **单一集成测试二进制**:`tests/all.rs` 聚合 `tests/suite/`(`vt100_history.rs`、`vt100_live_commit.rs`、`resize_reflow.rs`、`reconnect.rs` 等),减少链接开销。
- **测试与实现同文件/邻接**:几乎每个模块有同名 `_tests.rs`(tui/src 下约 60 个),流式、折行、keymap、paste burst 全是纯状态机 + 大量单测(如 frame_requester 用 `tokio::time::pause` 做确定性时间测试)。
- 未见"黄金录像"(录制会话回放)类测试;最接近的是 vt100 套件 + snapshot。

## 9. 值得抄的与值得警惕的

**值得抄**【已验证事实,除标注外】:

1. **"scrollback 是 escape-sequence 操作,渲染只发生在 viewport"** 的分层(`insert_history.rs` 头注释),配合 `HistoryLineWrapPolicy::{PreWrap, Terminal}` 区分预折行/终端软折行。
2. **newline-gated markdown 提交** + 顶层 block 边界的增量渲染 + 开放 fence 的 syntect 状态续跑——三层保守降级,永不出错半解析。
3. **FrameRequester actor + 帧率钳制 + 双缓冲 cell diff**,帧调度与事件处理彻底解耦,widget 随处可请求重绘。
4. **commit-tick 双档位滞回**(Smooth 打字机 / CatchUp 排空),并把调参指南写进文件头。
5. **EventBroker drop/recreate** 解决 stdin 所有权(跑外部编辑器),这是 ratatui 社区已知痛点的干净方案。
6. **VT100Backend(vt100 模拟器)测 inline 输出**,比 TestBackend 高一个保真度。
7. **快照式 undo 快照整个 draft**(含附件/mention 绑定),比增量 diff 简单且恢复语义正确;64 步/1MB 有界。
8. **`ScrollbackStrategy` 把终端怪癖(Zellij/Windows Terminal)收口在一个枚举里**,注释写明为什么。
9. **Item completion 权威于 delta 流**——传输丢 delta 不会截断最终 transcript。【推断】这对 Cadmus 的事件溯源(可能重放/补事件)是重要原则。

**值得警惕**:

1. **规模失控**:tui/src 共 **345k 行 Rust**;chatwidget 一处 75.6k 行、91 个子模块;`App` struct 约 91 个字段;`chat_composer.rs` 单文件 13k 行;`keymap.rs` 3966 行。【推断】这是多年有机增长 + 功能直接长在 ChatWidget 上的结果;Cadmus 的 wiring 层定位若失守(业务逻辑渗进 TUI),会重蹈覆辙。
2. **crossterm fork 与 nucleo git pin**——两个非 crates.io 依赖(见 §0);`input_boundary.rs` 的 `discard_buffered_input` 依赖 fork API,Cadmus 若上游没有该 API 需换实现(如启动时 poll+read 排空,已有跨平台 fallback 路径可参照)。
3. **快照式 undo 的内存成本**被字节预算兜住,但快照整个 draft(含图片路径等)在 Cadmus 若 composer 持有更大对象,需要重新评估预算。
4. **两套 fuzzy 并存**(nucleo + 60 行自研)说明 nucleo 的引入成本高到他们不愿在非文件场景复用。【推断】nucleo 的线程模型对小型内存列表是杀鸡用牛刀。
5. **粘贴启发式**(PasteBurst)本质是在没有 bracketed-paste 的终端上猜,注释坦承有 flicker 抑制等折衷;这是必要之恶但要预期边缘 case。

## 10. 对 Cadmus 的直接建议

结合我们已定设计(inline 默认、原子 block、事件溯源核心 + 客户端协议、crates.io-only、白名单许可证、MSRV 1.88):

**可照搬的机制**:

- **inline 架构三件套**:薄 fork/包装 ratatui Terminal 自管 `viewport_area` + 历史行 escape-sequence 写入(DEC scroll region)+ `ScrollbackStrategy` 终端策略枚举。Codex 的 `custom_terminal.rs` 保留了 ratatui MIT 头,这条路合规;但要预算 ~1.4k 行的维护成本,并先验证 ratatui 0.30 的 `Viewport::Inline` 是否真的不够(他们 fork 的历史原因可能部分已被上游吸收——【推断】需读 ratatui 0.30 的 inline viewport 源码后决定)。
- **流式管线骨架**:`MarkdownStreamCollector`(newline-gated)→ `StreamingRender`(顶层 block 稳定前缀 + 可变尾块)→ `OpenCodeFence` 快速路径 → pulldown-cmark + syntect/two-face。全部是 crates.io 可得组合,与我们"流式增量 markdown + 语法高亮"完全对应。
- **帧调度**:FrameRequester/FrameScheduler/FrameRateLimiter 三件套(~150 行,去掉他们的测试)直接可搬,建议 60 FPS 起步。
- **commit-tick 滞回策略**:把"打字机动画 vs 追平积压"做成纯函数策略(输入 QueueSnapshot 输出 DrainPlan),与我们的事件溯源"物化视图追赶事件流"语义同构——可以把 queue depth 换成"未物化事件数/最早事件龄"。
- **状态行模型**:strum kebab-case enum + `item -> Option<String>` + 配置即有序列表 + MultiSelectPicker 重排,几乎一一对应我们的"可组合状态行"。
- **测试栈**:insta + vt100 backend + 单一集成测试二进制 + 纯状态机单测,与我们的 nextest + insta 既定路线兼容(注意我们的 snapshot 纪律:`just snapshot-review` 逐个批准)。

**需要因事件溯源架构调整的**:

- **TUI ↔ core 边界**:Codex 的 TUI 是 app-server 的 JSON-RPC 客户端(不链 codex-core crate,只链 `codex-app-server-client/protocol`),AppEvent 枚举是内部"上帝消息"(1100+ 行枚举文件,上百变体)。我们的客户端协议更干净,但**要警惕 AppEvent 式膨胀**:建议 UI 内部事件与协议事件严格分层,协议事件 → 视图模型物化 → UI 只读物化状态。
- **transcript reflow 与我们的契合度更高**:Codex 的 resize 重放以"内存中 transcript cells"为 SSOT;我们的 SSOT 是事件流本身,resize reflow 可以直接从事件物化重建,比 Codex 更理直气壮——但要照搬他们的不变式:流式中或待 consolidate 时的 reflow 请求必须在流成为 source-backed 后补一次最终 reflow(`transcript_reflow.rs` 头注释)。
- **审批内嵌 diff**:他们的 diff 文本来自 core;我们的审批 prompt 应消费事件里的 `FileChange` 载荷,渲染层照搬 diffy + 主题感知 DiffTheme 即可。
- **composer undo**:Codex 的快照式 undo 可搬;若未来 composer 草稿本身入事件溯源(跨会话草稿恢复),快照与事件可以统一为"draft 事件 + 物化快照"。

**依赖层面的落地建议**:

- 可直接用:ratatui 0.30.2、 pulldown-cmark 0.10.3、syntect 5.3.0、two-face 0.5.1、diffy 0.4.2、textwrap 0.16、unicode-width 0.2、unicode-segmentation 1.12、insta 1.46、vt100 0.16(dev)。版本与 MSRV 1.88 兼容性需 cargo-deny/构建验证(Codex 自身 rust-toolchain 未在本次核对范围内)。
- **crossterm**:必须用 crates.io 0.29 原版,意味着 `discard_buffered_input`/`InputDiscardStatus` 不可用——启动输入排空需自实现(poll(0)+read 循环,他们代码里 Unix 分支其实就是这么做的,fork API 只是增强)。【推断】这是可接受的替代。
- **nucleo**:不可用(git 依赖 + 疑似 MPL-2.0)。替代:crates.io 的 `nucleo-matcher`(0.3+,仅匹配器无索引器,MIT)或 `fuzzy-matcher`(MIT)+ 自起遍历线程(ignore crate,MIT/Apache-2.0)。走 `adding-dependencies` 流程评估。
- **避免**:two-face 若因 any 原因不符白名单,降级方案是 syntect 自带 default-onig(他们正是用 two-face 扩展语法集)。

---

**遗留未读项**(如需后续深挖):`codex-app-server-protocol` 的事件协议 schema(与我们的客户端协议设计对照)、`exec_cell/` 的"一轮对话原子 block"渲染细节(状态着色具体实现)、`keymap.rs` 的 TOML 用户自定义 keymap 机制(Codex 支持全量键位重映射,我们可能不需要这个复杂度)。
