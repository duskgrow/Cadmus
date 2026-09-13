# TUI 审美风格调研：用色、glyph、密度、样式、状态、动画与主题架构——2026-09-13 基线

> 调研范围：ADR-0012 已定交互骨架(原子 block、活 keymap 提示栏、fuzzy picker、编辑器级
> composer、inline 渲染、失焦才通知、可访问行模式)之外的**未定项**——用色语义与调色板策略、
> 字体样式纪律、glyph 集、边框/密度/留白、动画、状态色语义、明暗背景适配、主题架构。
> 所有来源均为 2026-09-13 当日拉取的官方文档/源码/README。
>
> 置信度标注:【已验证事实】= 当日一手来源(官方文档/源码/README,附 URL);
> 【基于证据的推断】= 从已验证事实外推;【未核实】= 当日未能取得一手来源。

## 摘要

1. 行业正在收敛到**语义色槽位 + 明暗双值 + 终端背景自动探测**的主题架构(Gemini CLI、
   Claude Code、OpenCode、bat、delta、zellij、yazi 全部如此),分歧只在配置面大小。
2. **ANSI 4-bit 16 色是被低估的高级路线**:GitHub CLI 主动把调色板对齐 16 色以换取用户可
   自定义性与对比度安全;Codex 源码实测以 ANSI 命名色为主、RGB 仅 26 处且按需降级。
3. Nerd Font 依赖是一个**可选开关**而非默认前提(lazygit/eza/k9s 均可关);纯
   Unicode(box-drawing/braille)已有完整降级链先例(btop 三档 graph symbol)。
4. 动画的得体边界已由 GitHub 用 6000 行代码买出答案:**opt-in、屏幕阅读器模式跳过、
   语义色角色、不阻塞启动**;终端界不存在 reduced-motion 环境变量惯例。
5. 推荐方向「终端公民」:语义色槽位 ANSI 优先、truecolor 为增强,纯 Unicode 三档 glyph,
   Gemini 级小配置面主题文件,动画仅 spinner 一档。理由见 §9。

---

## 1. 用色纪律

### 1.1 各家实际用色量

- **Codex CLI**:TUI 源码中 `Color::` 用量统计——命名 ANSI 色占绝对多数(Red 37、Cyan 36、
  Green 32、Magenta 28、Yellow 14、DarkGray 12、Blue 12 等),`Rgb` 仅 26 处、`Indexed`
  仅 6 处。即:以终端可调色的 16 色为主,RGB 只用于精确适配场景。(【已验证事实】,实测
  `openai/codex` @main `codex-rs/tui/src`,grep 统计,2026-09-13;
  https://github.com/openai/codex/tree/main/codex-rs/tui/src)
- **GitHub CLI**:官方工程博客明确「调色板向 4-bit ANSI 16 色对齐」,因为绝大多数终端只允
  许用户自定义这 16 色,从而让可访问性用户能在终端偏好里完全接管配色;色选基于 Primer 无障
  碍基线。(【已验证事实】,
  https://github.blog/open-source/maintainers/building-a-more-accessible-github-cli/ ,2025-05-02)
- **GitHub Copilot CLI 横幅**:把颜色当*语义角色*系统而非字面色值——`eyes/goggles/border/
  shine/text` 等角色映射到 ANSI 4-bit 槽位,明/暗两套映射,保证高对比主题与用户覆盖下可辨。
  (【已验证事实】,
  https://github.blog/ai-and-ml/github-copilot/from-pixels-to-characters-the-engineering-behind-github-copilot-clis-animated-ascii-banner/ ,2026-01-28)
- **crush**:charmtone 命名调色板,明暗分级做得最细——前景 4 级(`fgBase/fgMoreSubtle/
  fgSubtle/fgMostSubtle`)、背景 4 级、语义集含 `destructive/error/warningSubtle/warning/
  attention/busy/info×3/success×3`,另附一套 ANSI 16 色重映射用于驯化裸 shell 输出。
  (【已验证事实】,
  https://github.com/charmbracelet/crush/blob/main/internal/ui/styles/themes.go)
- **lazygit**:默认只用约 5 个语义色(`activeBorderColor: green+bold`、`inactiveBorderColor:
  default`、`searchingActiveBorderColor: cyan+bold`、`optionsTextColor: blue`、`selectedLineBgColor`),
  其余全靠默认前景色。(【已验证事实】,
  https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md)
- **fzf**:内置 `dark`/`light`/`16`(base16)/`bw`(无色)四套预设,另开放约 40 个命名槽位
  (fg/bg/hl/current-*/border/label/prompt/pointer/marker/spinner/header/gutter…)。
  (【已验证事实】,https://github.com/junegunn/fzf/blob/master/man/man1/fzf.1)

### 1.2 语义色分配惯例

Gemini CLI 的主题 schema 是目前最小且完整的语义面:`text.primary/secondary/link/accent/
response`、`background.primary/diff.added/diff.removed`、`border.default/focused`、
`status.success/warning/error`、`ui.comment/symbol/gradient`——约 15 键。
(【已验证事实】,
https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/themes.md)

Claude Code 的 token 面更大但组织清晰:品牌色(`claude`,spinner 与 assistant 标签共用)、
文本梯度(`text/inactive/subtle`)、状态(`success/error/warning/merged`)、**模式色直接染输
入框边框**(`promptBorder/planMode/autoAccept/bashBorder`)、diff 六键(含 dimmed 变体与
word 级高亮)。(【已验证事实】,
https://docs.anthropic.com/en/docs/claude-code/terminal-config)

### 1.3 明暗适配与低色降级

- 终端背景探测的事实标准是 **OSC 10/11 查询 + 超时 + 回退链**。Rust 侧 `termbg` crate:OSC
  查询(100 ms 超时)→ Win32 API → `COLORFGBG` 环境变量;RGB 转 YCbCr,Y>0.5 判为浅色。
  明确列出 **Windows Terminal、ConEmu、PuTTY 不支持**该查询(microsoft/terminal#3718)。
  (【已验证事实】,https://github.com/dalance/termbg/blob/master/README.md)
- Codex 的 `terminal_palette.rs` 实现了同款机制:启动时做有界探测、结果缓存,查询失败与不
  支持统一走同一条 fallback;配 `is_light()`(亮度公式 0.299R+0.587G+0.114B)与 CIE76 感知
  距离做颜色适配。(【已验证事实】,
  https://github.com/openai/codex/blob/main/codex-rs/tui/src/terminal_palette.rs 与
  color.rs)
- bat 按终端背景自动挑选 dark/light 主题,可用 `--theme-dark/--theme-light` 或
  `BAT_THEME_DARK/BAT_THEME_LIGHT` 覆盖,另有 `--theme auto:system` 走 OS 级明暗;并提供
  `ansi`/`base16`/`base16-256` 三个只用 8-bit 色的主题。(【已验证事实】,
  https://github.com/sharkdp/bat/blob/master/README.md 「Highlighting theme」节)
- delta 把「自动检测明/暗终端背景」列为头部特性,配 `--dark/--light` 与 bat 主题生态。
  (【已验证事实】,https://github.com/dandavison/delta/blob/master/README.md)
- OpenCode 的 `system` 主题:按终端背景色生成灰阶、语法与 UI 用 ANSI 0–15、文本/背景可用
  `"none"` 透传终端默认色——是「住在用户终端配色里」的最激进实现。(【已验证事实】,
  https://opencode.ai/docs/themes/ ,页面更新于 2026-09-11)
- **降级不是框架给的**:ratatui 文档明确 `Rgb` 在非 truecolor 终端下 crossterm/termion 后
  端行为「不可预测(可能出现闪烁花屏)」——映射必须应用层自己做(Codex `best_color()` 即
  按 supports-color 层级把 RGB 收到 256/16)。(【已验证事实】,
  https://docs.rs/ratatui/0.30.2/ratatui/style/enum.Color.html)
- 能力分层事实:termenv 四级 `Ascii / ANSI16 / ANSI256 / TrueColor`,`EnvColorProfile` 尊重
  `NO_COLOR` 与 `CLICOLOR_FORCE`;其终端矩阵显示 tmux/screen **无法**做色彩方案查询(复用
  器可接多个不同配色终端),Windows Terminal 同样不在支持列。(【已验证事实】,
  https://github.com/muesli/termenv/blob/master/README.md)

### 1.4 「几乎不用色也很高级」的先例

fzf `bw` 预设完全无色;Claude Code classic 渲染器以 default/dim 为主、彩色只给状态与品牌
点;gh 把 16 色对齐本身当作可访问性特性宣传。(【基于证据的推断】——三者均已验证,但「高
级感来自克制」是对证据的解读。)

## 2. glyph 与图标

- **Nerd Font 是开关不是前提**:lazygit `gui.nerdFontsVersion`("2"/"3",默认关);k9s
  `noIcons`(注释原文:「not all terminal support these chars」);eza `--icons=auto` 只在
  tty 显示图标。(全部【已验证事实】,来源见 §1 与
  https://github.com/derailed/k9s/blob/master/README.md 、
  https://github.com/eza-community/eza/blob/main/man/eza.1.md)
- **但 eza/yazi 的默认图标集是 Nerd Font 私用区码点**:eza `icons.rs` 常量全部落在
  U+E000–U+F8FF 及增补私用区(如 `AUDIO='\u{f001}'`);yazi 官方安装文档把
  `font-symbols-only-nerd-font` 列入依赖。无 Nerd Font 即豆腐块。(【已验证事实】,
  https://github.com/eza-community/eza/blob/main/src/output/icons.rs 、
  https://yazi-rs.github.io/docs/installation)
- **纯 Unicode 降级链的完整先例是 btop**:要求 UTF-8 locale 与含 Braille Patterns、
  Geometric Shapes、Box Drawing 的字体;`graph_symbol` 有 braille/block/tty 三档,真 TTY
  自动激活 16 色 TTY 模式,`-lc/--low-color` 把 truecolor 收到 256。(【已验证事实】,
  https://github.com/aristocratos/btop/blob/main/README.md)
- Codex TUI 源码零 Nerd Font 引用(grep `nerd` 无真实命中)。(【已验证事实】,同 §1.1)
- **ASCII 全集先例**:charmbracelet/glamour 自带 `ascii.json` 与 `notty.json` 样式(后者是
  无样式兜底)。(【已验证事实】,
  https://github.com/charmbracelet/glamour/tree/master/styles)
- **CJK/宽字符坑**:unicode-width 0.2.2 文档——UAX#11 下 Fullwidth/Wide 恒为宽 2;
  **Ambiguous 字符在东亚上下文宽 2、其他上下文宽 1**(crate 用 `cjk` feature 区分),即同
  一字符串在不同 locale 的终端里列宽不同;emoji ZWJ 序列宽 2;eza 为此开放
  `EZA_ICON_SPACING` 让用户按终端实调图标间距(「no standard number of spaces」)。
  (【已验证事实】,https://docs.rs/unicode-width/0.2.2/unicode_width/ 与 eza man)
- OpenCode 是否硬性要求 Nerd Font:当日未取得一手确认。(【未核实】)

## 3. 密度与留白

光谱两端都有成功先例,**会话界面应站极简端**:

- 满屏仪表盘端:btop(圆角默认开、`rounded_corners` 在 TTY 模式被忽略);k9s(密集表格 +
  每 context skin)。(【已验证事实】,btop `btop.conf` 源码注释:
  https://github.com/aristocratos/btop/blob/main/btop.conf; k9s README 同前)
- 多面板中端:lazygit(border 默认 `rounded`,可选 single/double/hidden/bold;选中行用背景
  色,可退回 bold 或 reverse)。(【已验证事实】,lazygit Config.md)
- 极简端:Claude Code classic——无面板边框,只有 prompt 输入框有边框(且边框颜色是模式语
  义载体),会话区靠缩进、dim、留白分层;fzf——单列表 + 可选 preview,border 可 none。
  (【已验证事实】,Claude Code 文档 token 表;fzf man)
- 边框纪律:zellij 连 multiplexer 都默认 `pane_frame_style "titles"`(只画标题条不画全框),
  `full`/`none` 可选。(【已验证事实】,
  https://github.com/zellij-org/zellij/blob/main/zellij-utils/assets/config/default.kdl)
- 结论(【基于证据的推断】):agent 会话是*阅读流*不是*仪表盘*,密度应向 Claude Code/fzf
  看齐——单栏为主、边框只用于「可交互/有模式语义」的元素(composer、审批、picker);面板化
  只留给真正的 modal 子应用(会话列表、fleet 视图),与 ADR-0012 的 inline 默认一致。

## 4. 字体样式

- helix 主题 modifier 全集:`bold/dim/italic/underlined/slow_blink/rapid_blink/reversed/
  hidden/crossed_out` + underline style(line/curl/dashed/dotted/double),并明确标注
  「provided they are supported by your terminal emulator」。(【已验证事实】,
  https://docs.helix-editor.com/themes.html)
- italic 支持实测(本机 terminfo 快照,2026-09-13):`xterm-256color`、`tmux-256color` 有
  `sitm/ritm`;`screen-256color`、`linux`、`dumb` **没有**。即 italic 在 screen 系与 Linux
  控制台会丢。(【已验证事实】,本机 `infocmp` 输出;作为跨机普遍性的外推属
  【基于证据的推断】)
- `NO_COLOR` 官方规范明确:**只抑制颜色,不抑制 bold/underline/italic**;且用户级配置与命
  令行参数应能覆盖 NO_COLOR。(【已验证事实】,https://no-color.org/ ,页面更新 2026-09-04)
- 业界用法收敛:dim/faint 给次级信息(时间戳、提示);bold 给标题与强调;italic 用于
  blockquote 等点缀(Claude Code 2026 年把深色主题 blockquote 从 dim 改成 *italic+左竖线*,
  因为 dim 在深色背景可读性差);reverse/inverse 用于选中态(lazygit 允许用 `reverse` 替代
  选中行背景色);blink 基本没人用。(【已验证事实】,Claude Code CHANGELOG
  https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md ;lazygit Config.md)
- strikethrough 在 lipgloss/termenv 均支持,但终端覆盖率低;建议仅作装饰、永不承载语义。
  (【基于证据的推断】)

## 5. 状态可视化

任务骨架的会话状态机(idle/working/blocked/done)来自 herdr(ADR-0012)。同业视觉语言:

- **Claude Code**:品牌 accent 色驱动 spinner,每个可动画语义色配一个 `*Shimmer` 变体供渐
  变动画使用;模式(plan/auto-accept/bash)直接换输入框边框色;审批对话框用独立
  `permission` 色;diff 有 approved/dimmed(被拒后)双态。token 化使整套状态语言可被主题覆
  盖。(【已验证事实】,terminal-config 文档)
- **crush**:状态语义粒度全场最细——`busy`(Citron 黄绿)、`attention`(Tang 橙)、
  `warning` 两级、`error`/`destructive`、`success` 三级、`info` 三级;另有
  `borderFocused/Blurred` 表达焦点。(【已验证事实】,themes.go)
- **gh CLI 树立了可访问性标杆**:屏幕阅读器模式下把 braille spinner 替换为**静态文本进度指
  示**(尽量带上下文案,兜底「Working…」);交互 prompt 换成 huh 的可访问模式。(【已验证
  事实】,github.blog a11y 文,gh ≥ v2.72.0 提供 `gh a11y` 命令)
- OpenCode 主题键族把状态面摊开:`error/warning/success/info` + `borderActive` + diff 八键
  + markdown 十三键。(【已验证事实】,opencode.ai/docs/themes/)
- 评价(【基于证据的推断】):单点最强是 Claude Code 的「模式=边框色 + shimmer 对」(状态
  即位置即颜色,且不增加用色数);语义完备性是 crush;降级范式是 gh。Cadmus 应取三者交集:
  状态色四语义(working=accent/blocked=warning/done=success/error=error)+ 位置编码(block
  状态条/composer 边框/会话列表行)+ 行模式下的纯文字等价物。

## 6. 动画

- **得体集**:spinner(codex 用 36 帧编译期内嵌的 ASCII 帧序列,多主题变体;
  https://github.com/openai/codex/blob/main/codex-rs/tui/src/frames.rs);spinner 渐变
  (Claude Code shimmer 对,lazygit 无边框动画);彩蛋动画做成可关配置(lazygit
  `animateExplosion` 默认 true 可关);闪烁抑制(btop `terminal_sync`,用终端同步输出序列)。
  (全部【已验证事实】)
- **代价标尺**:GitHub 为 3 秒、约 20 帧的开屏横幅写了约 6000 行 TypeScript,绝大多数用于
  处理终端差异与可访问性;最终纪律是——动画 opt-in(默认关)、屏幕阅读器模式自动跳过、颜色
  走 4-bit 语义角色、不阻塞启动、不超 3 秒。(【已验证事实】,github.blog Copilot CLI 横幅文)
- **reduced-motion 等价物**:终端界不存在 NO_COLOR 式的 `REDUCE_MOTION` 环境变量惯例(当日
  检索无结果);现实等价物是三条:管道时无动画(ADR-0012 item 1 已定,clig.dev 地板)、
  屏幕阅读器/行模式无动画(gh 先例)、配置项一键关动画。(【未核实】——否定性结论,无法
  穷举证明不存在;建议按「不存在」设计。)
- 结论(【基于证据的推断】):Cadmus 的动画白名单 = spinner 帧动画(≤2 档速率)+ 完成时
  的一次性状态色落定;不做渐变 shimmer 在正文文本上;不进 alt-screen 就不做任何全屏过渡动
  画。

## 7. 主题系统架构

| 架构 | 格式 | 明暗策略 | 配置面 | 备注 |
|---|---|---|---|---|
| Gemini CLI | settings.json `customThemes` 或单文件 JSON(拒绝 home 目录外主题文件) | 内置 10 dark + 7 light + ANSI/ANSI Light 兜底主题 | ~15 语义键 | 预览式 `/theme` 选择器 |
| Claude Code | `~/.claude/themes/*.json`:`base`(六预设含 daltonized 色盲对)+ `overrides`;热重载;未知 token/非法值忽略 | `Auto (match terminal)` 探测明暗 | ~40 token | `dark-ansi/light-ansi` 预设即 16 色路线 |
| OpenCode | JSON,`defs` 引用 + 每键 `{dark, light}` 双值 + `"none"` 透传;内置→用户→项目→cwd 四级目录覆盖 | `system` 主题直接住进终端配色 | ~60 键(含 markdown/syntax/diff 全家) | 要求 truecolor 才有全彩 |
| bat | 内置主题 + `--theme-dark/--theme-light`/`BAT_THEME_DARK/LIGHT` + `auto:system` | OSC 10/11 探测自动挑主题 | 每个主题是一整套 syntect 高亮 | `ansi/base16` 8-bit 主题 |
| zellij | KDL,`theme_dark`/`theme_light` **成对**才自动切换,未探测到前用 dark | 主机终端报告 | 每主题 ~10 元素 | 复用器内探测有结构性限制 |
| k9s | YAML skin,按 context 可换 + `K9S_SKIN`;`invert` 深浅互转保色相 | invert 翻转 | 大 | 配置面失控被 lazygit 自承为反例 |
| yazi | flavor 目录(`flavor.toml` + 配套 `tmtheme.xml` 语法高亮)+ 用户 `theme.toml` 合并覆盖 | `[flavor] dark=/light=` 双槽随终端切 | 中 | 语法高亮与 UI 主题同源是好主意 |
| helix | TOML + `[palette]` 命名色 + `inherits` 继承 | 内置多主题 | scopes 大 | 默认 palette 即终端 16 色名 |
| glamour/glow | JSON style;`dark`/`light`/`ascii`/`notty` 多文件,auto 按终端背景选 | 自动 | 中 | markdown 渲染专用 |

(全部【已验证事实】,URL 见 §1–§5 各处与
https://zellij.dev/documentation/configuration 、
https://yazi-rs.github.io/docs/flavors/overview 、
https://docs.helix-editor.com/themes.html)

**给「配置面要小」的结论**(【基于证据的推断】):Gemini 的 ~15 语义键面 + Claude Code 的
`base + overrides` 两层结构 + bat/zellij/yazi 的 dark/light 双槽 + termbg 式带超时的探测回
退链,四者拼接即为最小完备架构。关键设计点:主题文件给的是*语义槽位值*而非组件样式;探测
失败一律落到 dark ANSI 安全预设;降级(色深/glyph)是渲染层函数,不是主题作者的负担。

---

## 8. 三个候选风格方向

### 方向 A:终端公民(Terminal Citizen)

- **一句话定位**:把用户的终端配色当宿主,语义色走 ANSI 16 槽位,truecolor 只是增强层。
- 用色:全部 UI 色从 12 个语义槽位出(fg/fg-subtle/accent/success/warning/error/info/
  border/border-active/diff-add/diff-remove/selection),槽位默认值 = ANSI 命名色;truecolor
  主题只是把槽位值换成 hex,渲染层负责向 256/16 降级(Codex `best_color` 模式)。
- glyph:纯 Unicode(box drawing + braille + ●○◆✓✗),三档:unicode(默认)/ascii(`| - + > x`)
  /无(行模式);永不引用 Nerd Font 私用区。
- 密度:会话流单栏无边框;边框只给 composer/picker/审批,`titles` 式细框(zellij 先例),
  圆角仅 truecolor+unicode 档默认。
- 动画:仅 spinner(帧动画,速率两档),`--no-color`/管道/行模式/TERM=dumb 全部静止。
- 参考先例:gh CLI(4-bit 对齐)、bat `ansi` 主题、OpenCode `system` 主题、Codex、fzf `16`。
- 风险:品牌辨识度低;ANSI 色在用户终端里色相不可控(红可能偏橘),语义对比要靠「明度差
  + 位置/符号双编码」兜底;OSC 11 探测在 Windows Terminal/tmux 会失败,必须接受 dark 默认
  的误判成本。

### 方向 B:精致暗室(Refined Darkroom)

- **一句话定位**:charm 系美学——命名调色板 + 4 级灰阶 + 柔和圆角 + shimmer,默认假设现代
  truecolor 终端。
- 用色:charmtone 式命名 hex 调色板(≈20 色),前景/背景各 4 级 subtlety,语义集含
  busy/attention 细分;256 终端由渲染层找最近色,16 色终端落到 ANSI 安全预设。
- glyph:Unicode 全集 + 可选 Nerd Font 图标开关(lazygit 模式);ascii 降级集。
- 密度:中密度;圆角细框给 block 与 composer;dim 大量用于元信息。
- 动画:spinner + 品牌 shimmer 渐变(Claude Code 式),全部可关。
- 参考先例:crush、Claude Code dark 默认、btop(圆角)、glamour dark。
- 风险:浅色终端等于再设计一套(明暗双主题维护成本翻倍);低色终端视觉质量损失大,与
  「公民性」约束有张力;渐变动画对慢 SSH/老终端不友好。

### 方向 C:排印极简(Typographic Minimal)

- **一句话定位**:颜色接近零,层级全靠排版——bold/dim/inverse/缩进/留白,颜色只给四态。
- 用色:默认主题 = fzf `bw` + 一个 accent + 状态三色;全部样式在 16 色内且可关到无色。
- glyph:同方向 A 的三档,但默认更克制(连 braille 都不用)。
- 密度:最低;无任何常驻边框,composer 用缩进 + 提示符区分;分隔靠空行与 `───`。
- 动画:仅 spinner,且 spinner 是纯文本符号轮换。
- 参考先例:fzf `bw`、helix(无图标、ui.window 仅分隔线)、Claude Code classic 的克制面。
- 风险:diff 与审批的可读性压力全压在排版上,设计失败即「简陋」而非「高级」;状态可扫性
  弱,对多会话注意力路由(ADR-0015 方向)不利。

### 推荐:方向 A,吸收 C 的密度纪律与 B 的语义结构

理由:(1) 公民性是已接受的硬约束(ADR-0012),A 是唯一天然满足 NO_COLOR/TERM=dumb/低色
终端/用户终端配色接管的方向,且有 gh、Codex、bat、OpenCode `system` 四个当日可验证的头部
先例;(2) Cadmus 的差异化在事件流与进化投影,不在皮肤——小配置面主题(Gemini 级 ~15 键)
与项目的「配置面要小」原则一致;(3) C 的密度/边框纪律应作为 A 的默认审美(无边框会话流),
B 的语义色板结构(busy/attention 细分、shimmer 对)留作 truecolor 增强主题的挂载点,不进入
默认渲染路径。

## 9. 《Cadmus TUI 风格指南》骨架(v0 草案,可进 ADR)

> 说明:本骨架按仓库惯例先行中文版供评审;随 ADR 落地时需出英文版(English-first)。

1. **语义槽位**:一切颜色出自命名语义槽位;组件不得硬编码色值。槽位清单(v0):
   `text / text-subtle / accent / success / warning / error / info / border / border-active /
   diff-added / diff-removed / selection`。
2. **调色板策略**:内置 `dark` / `light` / `dark-ansi` / `light-ansi` 四预设;默认
   `auto`(OSC 10/11 探测,100ms 超时,失败落 dark-ansi);ansi 预设只用 16 命名色,
   是低色终端与用户自定义的最终落点。
3. **降级在渲染层**:色深(truecolor→256→16→none)与 glyph(unicode→ascii)都是渲染层
   函数,主题文件不感知;探测链 = COLORTERM/supports-color → OSC 10/11 → COLORFGBG →
   dark-ansi 兜底。
4. **明暗**:主题每槽位可给 `{dark, light}` 双值;禁止任何「假设背景是深色」的硬编码;
   浅色终端快照进 `just snapshot-review` 固定。
5. **字体样式纪律**:bold=标题/强调;dim=元信息/次级;italic=引用/点缀;inverse=选中态;
   blink/strikethrough 不承载语义;TERM=dumb 与行模式下全部样式归零(NO_COLOR 只去色,
   不去样式——但我们提供 `--no-color` 与行模式两个更狠的开关)。
6. **glyph 集**:默认 Unicode 保守子集(box drawing、●○、✓✗、▲▼、…);`--ascii` 或
   检测失败时切 ASCII 集(`- | + > x !`);禁止 Nerd Font 私用区码点;所有宽度计算经
   unicode-width(CJK/emoji 按 UAX#11),图标后间距按终端实测可配。
7. **边框与密度**:会话流无边框;边框仅用于 composer、picker、审批对话框等有模式语义的
   可交互元素,样式为细线 titles 式,圆角仅在 unicode 档默认;间距优先于边框。
8. **状态语言**:working=accent+spinner、blocked=warning+「需决策」符号、done=success 直
   至被看见、error=error 色;状态永远双编码(颜色 + 符号/位置),行模式有纯文字等价物
   (gh「Working…」先例)。
9. **动画白名单**:仅 spinner(两档速率)与状态落定的一次性变色;管道/行模式/TERM=dumb/
   配置关闭时零动画;不做开屏动画、不做正文渐变。
10. **主题文件**:单文件 JSON/TOML,`base`(四预设之一)+ `overrides`(语义槽位子集),
    未知键与非法值忽略不报错;`customThemes` 不入主配置面;主题目录限 `$XDG_CONFIG_HOME/
    cadmus/themes`,热重载。
11. **可测试性**:每个槽位在每个预设下都有 snapshot;明暗两背景 + 16 色 + 无色 + ASCII 五
    套渲染矩阵进 CI(golden.ascii + insta)。

## 10. 来源清单(全部 2026-09-13 当日访问)

- Gemini CLI themes:https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/themes.md
- Claude Code terminal-config:https://docs.anthropic.com/en/docs/claude-code/terminal-config
- Claude Code CHANGELOG:https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md
- Codex TUI 源码(color.rs / terminal_palette.rs / frames.rs,@main):
  https://github.com/openai/codex/tree/main/codex-rs/tui/src
- OpenCode themes:https://opencode.ai/docs/themes/
- crush themes.go:https://github.com/charmbracelet/crush/blob/main/internal/ui/styles/themes.go
- crush README:https://github.com/charmbracelet/crush/blob/main/README.md
- lazygit Config.md:https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md
- k9s README:https://github.com/derailed/k9s/blob/master/README.md
- helix themes:https://docs.helix-editor.com/themes.html
- btop README / btop.conf:https://github.com/aristocratos/btop
- fzf.1:https://github.com/junegunn/fzf/blob/master/man/man1/fzf.1
- gh a11y:https://github.blog/open-source/maintainers/building-a-more-accessible-github-cli/
- Copilot CLI 横幅:https://github.blog/ai-and-ml/github-copilot/from-pixels-to-characters-the-engineering-behind-github-copilot-clis-animated-ascii-banner/
- lipgloss README:https://github.com/charmbracelet/lipgloss/blob/master/README.md
- termenv README:https://github.com/muesli/termenv/blob/master/README.md
- glamour styles:https://github.com/charmbracelet/glamour/tree/master/styles
- delta README:https://github.com/dandavison/delta/blob/master/README.md
- bat README / CHANGELOG:https://github.com/sharkdp/bat
- eza(man/theme.yml/icons.rs):https://github.com/eza-community/eza
- zellij default.kdl:https://github.com/zellij-org/zellij/blob/main/zellij-utils/assets/config/default.kdl
- yazi flavors / installation:https://yazi-rs.github.io/docs/flavors/overview
- NO_COLOR:https://no-color.org/
- termbg:https://github.com/dalance/termbg/blob/master/README.md
- unicode-width:https://docs.rs/unicode-width/0.2.2/unicode_width/
- ratatui Color:https://docs.rs/ratatui/0.30.2/ratatui/style/enum.Color.html
- 本机 terminfo 快照(infocmp xterm-256color/tmux-256color/screen-256color/linux/dumb)
