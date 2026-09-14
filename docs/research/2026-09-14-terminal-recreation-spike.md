# Terminal 重建 Spike：stock ratatui 上动态带高的可行性——2026-09-14 基线

> **Frozen research snapshot (baseline 2026-09-14).** Exhibit material for the
> ADRs in `docs/decisions/` — this is *not* living documentation and is never
> edited; where this document and the ADRs disagree, the ADRs win. Version,
> vendor and maintenance-status claims require re-verification at the start of
> the phase that depends on them (freshness policy: report §1.2.2). Current
> phase status lives in `docs/roadmap.md`.

调研对象：ratatui-core 0.1.2（本地 registry 源码直读）+ vt100 0.16.2 仿真行为。
调研范围：ADR-0018 spike 事实 F1 留下的唯一未测试逃生口——高度变更时重建
`Terminal` 是否能在 stock ratatui（无 fork）上交付动态带高。
方法：进程内 vt100 确定性探针（`crates/cadmus-tui/tests/dynamic_height_spike.rs`，
8 场景全绿）+ 真机 harness 扩展（`examples/inline_spike.rs` 的 g/G/s 键）。
置信度标注：【已验证事实】= 源码直读或探针断言；【基于证据的推断】= 由已验证
事实经明示推理链得出。
消费者：ADR-0018 修订案（定高 vs 动态高度的裁决）；若采纳动态高度，PR 1 的
内联外壳。

## 1. 为什么做这个 spike

ADR-0018 修订案 item 1 把布局钉为定高，并明示：若动态高度变成硬需求，第一个
要 spike 的就是 F1 的逃生口（重建 `Terminal`，不 fork）。定高 vs 动态高度的
讨论（maintainer，2026-09-14）需要实测数据续谈。本 spike 的判决标准（maintainer
设定的 UX 守卫）：**零历史丢失/重复、残留有界、composer 光标稳定**；实现工作
交由后续 agent，本文件只交付可行性证据与协议。

## 2. Rig 设计（可复用，PR 1 item 9 的雏形）

进程内仿真：一个实现 ratatui `Backend` 的 `VtBackend`，把 ratatui-crossterm
实际发射的转义序列（逐 cell CUP+符号、`\n`×n 的 append_lines、ED 清屏、CUP
光标）喂给 `vt100::Parser`，由 vt100 施加真实终端语义（滚屏、scrollback、
延迟折行），而不是自己再推导一遍。光标位置查询（CPR）由仿真屏直接应答——
与真终端的应答路径等价。SGR 样式字节被有意省略：样式从不移动行，而本探针
判决的是行账。【已验证事实】

不能判决的：**合成层面**（clear→recreate→draw 之间的闪烁）。2026h 守卫只影响
合成，不进 vt100 的屏幕模型——留给人肉终端矩阵（harness 已带 g/G/s 键）。

## 3. 机制发现（源码级，全部【已验证事实】）

`compute_inline_size`（init 与 resize 共用）的真实行为，决定了朴素重建必然
留残影：

1. 先查询当前光标位置（CPR）；
2. **无条件** `append_lines(height−1−offset)`——光标靠近底边时终端滚屏，把
   旧带图像上推；
3. 再由"追加后可用的行数"倒推 viewport 原点。

推论：直接重建（无任何编排）时，旧带图像被滚屏上推，新带只覆盖其中一部分，
**上方残留旧带顶部若干行**——探针场景 `naive_recreation_leaves_stale_band_rows`
断言屏幕上出现两代 STATUS 行，坐实这一类残留（即 F4 的"旧帧变 scrollback
残留"在重建路径上的复现）。

## 4. 协议（探针锁定，【已验证事实】）

**增长 h→h′（Δ 行）**：

1. `insert_before(Δ 空行)`——历史整体上推 Δ（顶部 Δ 行进入 scrollback，本是
   其归宿），带位置不变，带正上方出现 Δ 个空行；
2. 光标停到"新带顶"（插入后 viewport.y − Δ）；
3. 以 `Viewport::Inline(h′)` 重建 `Terminal`——重锚的 append 恰好落在底行，
   **零滚屏**；首帧全量重绘覆盖的只有自己刚插入的空行与旧带位置。

**收缩 h→h′（Δ 行）**：

1. `Terminal::clear()`（inline 语义：从 viewport 原点清到屏尾，恰为旧带）；
2. 光标停到旧带顶 + Δ 行；
3. 重建更矮实例。腾出的 Δ 行成为历史与带之间的空白——**有界（恰 Δ 行）、
   不丑、随后续放行自然被消费**（终端行不可"向上塌陷"，那是我们拒绝的
   DEC 行删除技巧）。

**高度策略作用在有效高度上**：请求高度先按屏高钳制，与当前相等则整个操作
no-op——否则带已占满全屏时锚点数学下溢（探针场景
`grow_to_full_screen_height_clamps_and_stays_exact`）。

## 5. 场景矩阵与结果（8/8 绿）

| 场景 | 断言要点 | 结果 |
|---|---|---|
| rig  sanity（定高锚定 + insert 语义） | 全序列相等、光标 | ✅ |
| 朴素重建（反证基线） | 屏幕出现两代 STATUS（残留坐实） | ✅（如期复现） |
| 增长协议（底部锚定） | 历史零丢失、零残留、随后流式不受扰 | ✅ |
| 收缩协议 | 残留恰为 Δ 空行、带重新贴底、历史完整 | ✅ |
| 增长/收缩搅动 ×6 轮（穿插放行） | 每步全序列相等，无累积 | ✅ |
| 会话顶部增长（无历史、带悬空） | shell 提示行完整、无下溢 | ✅ |
| composer 光标在带中部时增长 | 光标恢复原位 | ✅ |
| 增长到全屏高及超屏请求 | 钳制为屏高、no-op、序列仍精确 | ✅ |

全序列断言（scrollback+屏幕逐行等于已发射历史+当前带）是最强不变量：任何
丢失、重复、残影都会打破序列。附注：探针开发中捕获的读侧分页 bug（vt100
scrollback 视图单屏高，需分页读取）已注释于测试文件——观测工具自身也要被
测试，这是本 spike 的方法论记录。

## 6. UX 守卫评估

- **零丢失/零重复**：✅ 全部场景（含搅动与边角）。
- **残留有界**：✅ 增长零残留；收缩恰 Δ 空行且自愈。
- **光标稳定**：✅ 每次高度变更后 composer 光标逐位断言（含带中部光标）。
- **闪烁**：❓ 仿真不能判决——真机矩阵项（见 §7）。
- **成本**：每次变更 = 一次 CPR 往返 + 一次带内全量重绘（2026h 包裹
  insert+recreate+draw 为一个原子单元）。变更频率是事件驱动的（composer
  跨行、held 块落定、resize），每 turn 数次，非每帧。

## 7. 残余风险与真机矩阵清单

- 【已验证事实】上游 issue #2640（未决）：inline 构造与 clear 的 CPR 查询
  与 stdin 竞争。我们的 input broker（拥有 event stream、可 drop/recreate，
  PR 1 切片内）是隔离该竞争的缝；矩阵需观察重建瞬间是否有按键被吞。
- 真机矩阵（Zed 终端、tmux、Windows Terminal、Zellij——此前未装，仍缺）
  已改为**全机械证据管线**，不依赖目击者描述：harness 把原始输出 tee 到
  `target/inline-spike/capture-<ts>.bin` + `.txt` 侧车（初始尺寸、
  identity、resize 事件含流偏移、完整按键日志、会话末预期行序列）；
  `inline_spike_replay` 用 vt100 重放字节流并与侧车模型逐行 diff，外加
  2026h 守卫配对检查与哨兵键检查（每次 g/G/s/f/F 前后按 `x`，CPR 竞争
  吞键即现形）。判负信号仍配失败参照：`G` 键制造残影、`F` 键制造闪烁，
  diff 中的残影行可归因到侧车 stats 的 naive_grows 计数。分析器自身由
  3 个夹具测试锁定（干净捕获判等、缺帧检出、哨兵缺位检出）。
  仿真判不了的只剩合成层面（闪烁），f/F 对照一瞥即可，为可选确认项。
- WT/Zellij 的已知 quirk 针对 DEC scroll region；便携路径（含本协议）从不
  发射 DEC region（F2），【基于证据的推断】该 quirk 类继续被结构性规避，
  由矩阵确认。
- ratatui 下一版本（0.30.3+）在本区域有已合入未发布改动（#2670/#2731/
  #2666/#2527，见 open-items 对应条目）：bump 时本探针全套场景需重跑。

## 7.5 真机矩阵结果（2026-09-14 晚，全部机械证据）

| 终端 | 行级正确性 | 守卫配对 | 哨兵 | 备注 |
|---|---|---|---|---|
| Zed（xterm 类） | ✅ 零丢失/零残留 | 18/18（重跑） | ✅ | 人工跑，分析器判决 |
| Windows Terminal | ✅ | 25/25 | ✅ | 跨编译 gnu 二进制（`just spike-windows`），捕获文件回传后分析器判决 |
| tmux 3.7b | ✅（310 turn / 22968 行浸泡） | 314/314 | ✅ | **全自动脚本腿**（send-keys 驱动 + capture-pane + 分析器），零人工观察 |

三台终端上朴素 G 对照组均精确现形且可逐行归因（tmux 例：extra 恰为旧
12 行带图像减被覆盖的末行 = 11 行，含当时计数器快照与折行尾行），
证明残影防护既必要又充分。分析器在矩阵中自修正两处（scrollback 容量
按侧车预期行数定容；图例行压 ≤72 列防折行假阳性）——观测工具持续被
观测。

残余项（均不阻塞可行性）：合成层面（闪烁）为有界外观类——忽视 2026h
的终端上高度变更瞬闪一帧，该暴露面对包括 Codex fork 在内的一切实现
相同；tmux 面板模型非原子（capture-pane 实证），外跳不可观测。Zellij
未测（未安装；便携路径不发射其 quirk 针对的 DEC region，结构性规避）。

## 8. 结论

**动态高度在 stock ratatui 上可行，无需 fork。** 重建协议先在 vt100 确定性
仿真下满足全部 UX 守卫（§5），后在三台真终端上经全机械证据管线复核：
行级零丢失/零残留、守卫配对平衡、无吞键、对照组逐行可归因（§7.5）。
thin fork（~1.4k 行 + DEC 不可移植类 + 逐终端策略枚举）维持搁置；仅当
 Zellij 或后续终端在便携路径下出现异常时重开。

对定高 vs 动态高度裁决的建议（供 maintainer 参考，非决定）：动态高度的
机制成本已从"fork"降为"外壳内一处策略 + 每次变更一次 CPR 往返"；但注意
形状/样式不变量放行规则在两条路线下都需要（Codex 有动态高度照样要 flush
完成的 cell）——高度策略与放行策略是正交轴，采纳动态高度不淘汰 ADR-0018
item 4 的任何内容。

## 9. 产物清单

- `crates/cadmus-tui/tests/dynamic_height_spike.rs` — vt100 探针（8 场景，
  CI 内运行；rig 即 PR 1 item 9 的 vt100 骨架雏形）。
- `crates/cadmus-tui/examples/inline_spike.rs` — 真机 harness：g/G/s/f/F
  键 + 失败对照组 + tee 捕获/侧车/哨兵键（仍是 quirk 回归工具；PR 1
  shell 落地后按 open-items 条目收敛为驱动器）。
- `crates/cadmus-tui/examples/inline_spike_replay.rs` — 捕获重放分析器
  （含 3 个夹具测试），矩阵 verdict 的计算端。
