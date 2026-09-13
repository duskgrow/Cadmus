# 设计 token 工程调研:渲染器无关的 token 规范 v0——2026-09-13 基线

> 调研范围:GUI(终态主力,终端风格)与 TUI(第一个渲染器)共享设计 SSOT 的**方法论与
> 工程结构**——调色板构造、token 分层与语义槽位、字体排印/间距/动效刻度、图标系统、
> 主题文件格式与 Rust 加载。不依赖具体色值(Linear 事实清单由另一路调研产出);与
> `2026-09-13-tui-aesthetic-style-research.md`(终端侧先例:探测链、降级、glyph 纪律)
> 互补,交叉引用而不重复。消费者:设计 token ADR(GUI ADR 与 TUI 实现的共同前置)。
>
> 置信度标注:【已验证事实】= 2026-09-13 当日一手来源(官方文档/源码/crates.io API,
> 附 URL);【基于证据的推断】= 从已验证事实外推;【未核实】= 当日未能取得一手来源。

## 摘要

1. **调色板方法论收敛于"感知均匀色空间 + 12 级三轨色阶 + 语义别名"**:Radix 的 12 级
   每级语义(背景 1–2 / 组件 3–5 / 边框 6–8 / 实心 9–10 / 文本 11–12)是目前文档化最
   完整的可复现流程;OKLCH 是构造空间的行业选择;Material HCT 无 Rust 实现,不采用。
2. **token 三层结构(scale → semantic → component)是跨系共识**,分歧只在配置面大小;
   最小完备语义槽位以 Gemini CLI ~15 键为锚,评审后 v0 定稿 **18 键**(新增 bg、
   bg-subtle、on-accent、mark、diff-*-bg,论证见 §2.3)。
3. **WCAG 数值底座**:正文 4.5:1、大字(≥18pt/≥14pt bold ≈ 24px/18.5px)3:1、UI
   组件与图形对象 3:1;阈值不可四舍五入。终端 glyph 当作符号用时**属于非文本内容**,
   同样适用 3:1。
4. **动效/图标/字号的终端可迁移子集很小**:动效只剩 spinner 帧与状态落定变色;图标
   需要"语义名 → SVG/Unicode/ASCII 三档"映射表;字号梯度在 TUI 由 bold/dim/inverse
   承载。规范按"渲染器能力档案(capability profile)"分层,而非按渲染器写两套 token。
5. **工程结构核心决策:OKLCH 只存在于离线生成器,运行时色值一律 sRGB hex**;热重载
   用 mtime 轮询(`notify` 是 CC0-1.0,**不在许可证白名单**,当日已核实);主题文件
   单文件 TOML/JSON,`base` 预设 + `overrides` 语义槽位子集,未知键容错。

---

## 1. 色彩科学与调色板构造

### 1.1 OKLCH:构造空间,不是存储格式

- OKLCH/Oklab 由 Björn Ottosson 于 2020 年提出,修复 CIE LCH 在蓝色区间(色相
  270–330)的色相漂移;CSS Color 4 于 2022-07-05 进入候选推荐并引入 `oklch()`;
  截至 2025 年秋,所有现代浏览器可用。轴语义:`L` 感知亮度 0–1、`C` 色度(实践上限
  <0.37,随色相/色域而变)、`H` 色相角(参考点:红 ≈20、黄 ≈90、绿 ≈140、蓝 ≈220、
  紫 ≈320)。(【已验证事实】,
  https://evilmartians.com/chronicles/oklch-in-css-why-quit-rgb-hsl ,2025-09-17 更新)
- 对调色板生成的价值:**L 轴跨色相一致**,固定 L 改 H 得到同亮度异色(语义色阶的
  基石);色修改可预测,无 HSL `darken()` 的意外;超色域颜色应按"降色度保色相"做
  gamut mapping(CSS Color 4 要求浏览器用 OKLCH 法,但 Chrome/Safari 当时仍用裁剪法,
  因此生成期手工映射更稳)。(【已验证事实】,同上)
- 该文 changelog 明确:**OKLCH 本身不等于对比度合格**,对比度检测应使用 APCA 等专用
  度量。(【已验证事实】,同上 changelog 2023-02-05)
- Material 3 的 HCT 是另一路:CAM16 的 hue/chroma + CIE L* 的 tone,tonal palette =
  仅 tone 变化的色列;官方 material-color-utilities 仅提供 C++/Dart/Java/Swift/
  TypeScript/Kotlin,**无 Rust 移植**。(【已验证事实】,
  https://github.com/material-foundation/material-color-utilities/blob/main/README.md)

**对 Cadmus 的落地**:构造空间选 OKLCH(Rust 侧有白名单内实现,见 §7.3);HCT 因无
Rust 实现且其" viewing conditions 自适应"收益对两套固定主题边际,不采用。OKLCH 不出
现在运行时:生成器(xtask 或独立脚本)在 OKLCH 中构造 → gamut map → 烘焙为 sRGB hex
写进四个内置预设;运行时只做 hex 解析与 256/16 降级映射。(【基于证据的推断】)

### 1.2 Radix 12 级色阶:三轨语义的可复现模板

当日核实的每级用途(官方"Understanding the scale"):

| 级 | 语义用途 |
|---|---|
| 1 | App 背景 |
| 2 | 次级背景(卡片、侧栏、代码块) |
| 3 | 组件背景(常态) |
| 4 | 组件背景(hover) |
| 5 | 组件背景(pressed/selected) |
| 6 | 非交互边框/分隔线 |
| 7 | 交互组件边框、focus ring |
| 8 | 交互组件边框 hover 态、更强的 focus ring |
| 9 | 实心背景(全色阶彩度最高的一级;品牌/主按钮) |
| 10 | 实心背景 hover |
| 11 | 低对比文本 |
| 12 | 高对比文本 |

补充保证:11/12 级在同色阶 2 级背景上保证 APCA Lc 60 / Lc 90;多数色阶的 9 级配白
字,Sky/Mint/Lime/Yellow/Amber 的 9/10 级配深字。每条色阶有四个变体:light、light
alpha、dark、dark alpha;另有跨明暗不变的 black alpha / white alpha 遮罩阶。
(【已验证事实】,
https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale 与
.../palette-composition/scales)

别名机制(官方"Aliasing"页):尺度名(blue-1…12)→ 语义别名(accent/success/
warning/danger)→ 用途别名(accent-bg-subtle/accent-border/accent-solid/accent-text…)
可多层并存;明暗双模式靠 **mutable alias**(如 `--panel` 在 light 映 white、dark 映
slate-2),并明确**避免组件名命名**(不用 `CardBg`,因为一个变量会服务多个组件)。
(【已验证事实】,https://www.radix-ui.com/colors/docs/overview/aliasing)

**对 Cadmus 的落地**:12 级三轨是**生成器的输出结构**与**预设内部的组织方式**,不进
运行时 API——运行时只见语义槽位(§2)。色阶生成时按上表自检:文本轨(11/12)对背景
轨(1/2)过对比度门(§1.4),边框轨(6–8)对相邻背景过 3:1。(【基于证据的推断】)

### 1.3 WCAG 对比度数值底座(规范引用层)

当日核实(WCAG 2.2 Understanding,2026-06 更新):

- **SC 1.4.3(AA)**:正文 ≥ 4.5:1;大字(≥18pt 常规或 ≥14pt bold,约 24px/18.5px)
  ≥ 3:1;比值不四舍五入(4.499 不合格)。https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html
- **SC 1.4.6(AAA)**:正文 ≥ 7:1,大字 ≥ 4.5:1(同页 rationale 与 "See also")。
- **SC 1.4.11(AA)**:识别 UI 组件/状态所需的视觉信息、以及理解内容所需的图形对象,
  对相邻色 ≥ 3:1。**当作符号用的文本字符(如关闭按钮的 "X"、箭头的 ">")计入非文
  本内容**——这直接覆盖 TUI 的 glyph 状态符号。https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html
- 豁免:非活动组件、纯装饰、logotype;hover 附加效果本身不要求 3:1(指针位置已是
  指示),但不得让组件丢失既有对比度。(均【已验证事实】,同上两页)

**对 Cadmus 的落地**:对比度预算写进构造流程(§1.5)与一致性测试:文本槽位对 bg 过
4.5:1(text-subtle 过 3:1 并在规范中限定其用途为"次级/元信息",不走 AAA 路径);
border-active、状态 glyph 对相邻背景过 3:1。TUI 的 16 色档由 gh 先例兜底(用户可在
终端侧接管配色),但 truecolor/256 档的内置预设必须自检。(【基于证据的推断】)

### 1.4 暗色主题:独立色阶,不是反相

- Radix 的每条色阶有独立设计的 dark 变体(不是算法反相);alpha 遮罩阶跨明暗不变。
  (【已验证事实】,§1.2 来源)
- shadcn/ui 默认主题:dark 模式覆盖同名语义 token;边框在 dark 下用白透明
  (`--border: oklch(1 0 0 / 10%)`)而非灰阶色值——暗色边框/分隔用 alpha 叠加表达,
  这是"elevation/分隔靠叠加"的现行主流实现。(【已验证事实】,
  https://ui.shadcn.com/docs/theming)
- Material 的 dynamiccolor 组件按"暗色主题、样式、对比度需求"等状态调整角色色;其
  tonal palette 以 tone 为唯一变量,暗色方案 = 角色映射到不同 tone。(【已验证事实】,
  §1.1 MCU 来源)M2 的"elevation 越高、白色 overlay 越浓"具体配方当日未能取得一手
  页面(m2.material.io 需 JS)。(【未核实】,
  https://m2.material.io/design/color/dark-theme.html)

**结论**(【基于证据的推断】):明暗两套 12 级色阶**分别构造、共享参数族**(同一
L 曲线族、同一 H、同一 C 曲线族,端点与曲线参数各自调),语义槽位映射不变;暗色下
的 border/selection/diff-bg 允许用 white-alpha 叠加值表达(GUI 原生支持;TUI 渲染层
把 alpha 合成到已解析的背景色上再降级)。

### 1.5 "muted 灰阶 + 单 accent"可复现构造流程

输入:accent 色相 `H_a`(0–360)、可选 accent 彩度上限 `C_a`(默认 0.15)、可选灰阶
色相偏移 `H_g`(默认 = `H_a`,即灰阶向 accent 微微染色——Radix Slate/Mauve 即染色
灰先例,§1.1 来源的 scales 页)。输出:明/暗两套 12 级灰阶 + 12 级 accent 阶 + 语义
槽位映射表。(【基于证据的推断】,参数需经 §1.3 对比度门与快照评审校准)

1. **定 L 锚点**:以"背景 → 文本"的目标对比度反推。light:step1 ≈ L 0.99(近白)、
   step12 需对 step1 ≥ 16:1(取 L ≤ 0.21 量级);dark:step1 ≈ L 0.15–0.20(避免纯黑,
   留出"更黑"的余量给 overlay),step12 ≈ L 0.93+。
2. **铺 L 曲线**:12 级按"感知等距 + 两端加密"分布——背景轨(1–5)级差小(同亮度区
   微调),文本轨(11–12)拉开。同一函数族生成明暗两套,参数不同(§1.4)。
3. **铺 C 曲线**:灰阶 C ≤ 0.02(muted 的来源;近中性但带 H_g 染色);accent 阶 C 在
   step 9 达峰(Radix 9 级"全阶彩度最高"原则),背景/文本轨低彩度(文本轨 C 约峰值
   的 50–70%,保证可读)。
4. **H 固定**:全阶恒 H(灰阶 H_g、accent H_a);OKLCH 下恒 H 即恒感知色相。
5. **Gamut map**:每级先查 sRGB 色域内;越界则降 C 至边界(保 H、保 L),不裁剪 RGB。
6. **对比度门(机械断言,不过则生成失败)**:step11/step12 对 step1/step2 ≥ 4.5:1;
   step7(border-active 轨)对 step1 ≥ 3:1;accent step9 对 on-accent 字色 ≥ 4.5:1;
   diff-bg 轨对文本轨不退化正文对比度至 4.5:1 以下。附检:11/12 对 2 级背景达 APCA
   Lc 60/90(Radix 同标准)。
7. **烘焙**:输出 sRGB hex ×2(明暗)×3(灰阶、accent 阶、overlay 阶),写进预设文
   件;语义槽位映射是固定表(§8.3),不随主题变。
8. **快照**:明暗 × 真彩/256/16/无色 × unicode/ascii 渲染矩阵进 `just snapshot-review`
   (沿用风格指南骨架 §9.11 的矩阵)。

## 2. token 命名架构

### 2.1 三层先例的事实核验

- **SLDS**:最早的大型 token 系统,当日页面(Version 1.2.55,2026-09-06 更新)可见
  分类 Colors/Background/Text/Border/Font/FontSize/Opacity/LineHeight/Spacing/Radius/
  Sizing/Shadow/Time/Touch/MediaQuery/Z-index;raw 层为调色板(gray 13 级、各彩色
  10–95 阶),语义层按用途硬切分("Use these tokens for text colors only. Do not use
  these for border colors or background colors.")。(【已验证事实】,
  https://www.lightningdesignsystem.com/design-tokens/)
- **Radix**:尺度 → 语义别名 → 用途别名 + 明暗 mutable alias(§1.2,已验证)。
- **shadcn/ui**:语义层 = background/foreground 成对约定(`primary`/`primary-foreground`),
  dark 模式 = `.dark` 下覆盖同名 token;radius 刻度从单一 `--radius` 派生。全部以
  OKLCH 书写。(【已验证事实】,https://ui.shadcn.com/docs/theming)
- **W3C DTCG 格式模块 2025.10**:JSON 交换格式,`$value`/`$type`/`$description`/
  `$extensions` 结构,`{group.token}` 别名语法,类型含 color/dimension/fontFamily/
  fontWeight/duration/cubicBezier/number 与 shadow/border/transition/gradient/
  typography 复合类型。**状态:Draft Community Group Report,非 W3C 标准**;预览页
  明确"不要实现此版本"。(【已验证事实】,https://tr.designtokens.org/format/ ,
  最新发布版 https://www.designtokens.org/TR/2025.10/format/)
- Material 3 的 ref/sys/comp 三层命名(md.ref.*/md.sys.*/md.comp.*)当日无法取得一手
  页面(m3.material.io 需 JS)。(【未核实】,https://m3.material.io/foundations/design-tokens/overview)

**对 Cadmus 的落地**(【基于证据的推断】):

- 采用三层:**L0 scale**(12 级色阶,仅存在于预设与生成器)→ **L1 semantic**(语义
  槽位,运行时唯一契约)→ **L2 component**(组件级别名,默认空集,按需逐个引入且
  必须能回指 L1)。组件不直接引用 L0(SLDS 的用途硬切分纪律)。
- DTCG 借概念不借格式:它是工具间交换格式且未定稿;Cadmus 主题文件是**人写的配置**
  (TOML/JSON,§7),别名/mutable alias 思想用 `{dark, light}` 双值表达即可。
- 命名规则:kebab-case;`family[.variant]`;语义层禁止组件名(Radix `CardBg` 教训);
  明暗差异只走槽位双值,不出现在名字里(没有 `*-dark` 键名)。

### 2.2 最小配置面的锚点

- Gemini CLI:~15 语义键(`text.primary/secondary/link/accent/response`、
  `background.primary/diff.added/diff.removed`、`border.default/focused`、
  `status.success/warning/error`、`ui.comment/symbol/gradient`)。(【已验证事实】,
  同日 TUI 调研 §1.2;https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/themes.md)
- Claude Code:~40 token,含模式边框色与 diff 六键。(【已验证事实】,同日 TUI 调研)
- crush:前景 4 级、语义含 busy/attention 细分。(【已验证事实】,同日 TUI 调研)

### 2.3 槽位清单评审:12 键草案 → v0 定稿 18 键

对草案 `text / text-subtle / accent / success / warning / error / info / border /
border-active / diff-added / diff-removed / selection` 的逐项评审:

| 变更 | 槽位 | 理由 |
|---|---|---|
| 保留 | text, text-subtle | 文本两档足够(v0 不引第三档;crush 的 4 级是配置面膨胀先例)。text-subtle 用途限定为元信息/次级,对比度门 3:1 |
| 保留 | accent, success, warning, error, info | 四态 + 单 accent 是状态语言最小集(同日 TUI 调研 §5 结论) |
| 保留 | border, border-active | 对应 Radix 6 与 7/8 两轨;border-active 承载 focus/active/模式染色(Claude Code promptBorder 先例),不改名 |
| 保留 | selection | 用户选区/选中行 |
| 修订 | diff-added, diff-removed | 保留,但各配一个 `-bg` 变体(见下) |
| **新增** | **bg** | 一切对比度预算与 GUI 绘制的锚点;Gemini `background.primary`、shadcn `background` 均有。TUI 真彩档必须显式绘制背景,否则用户终端配色会把精心调的对比度打散(同日 TUI 调研:OpenCode `system` 主题选择住进终端配色是另一极,Cadmus 不取) |
| **新增** | **bg-subtle** | Radix step 2 轨:面板、picker、状态条、代码块的次级背景;会话流与面板的分层全靠它(无边框纪律下尤其如此) |
| **新增** | **on-accent** | accent 实心表面(主按钮、active 项)上的前景;Radix"9 级配白字/深字"与 shadcn `-foreground` 对约定都要求它存在,否则 accent 实心面不可用 |
| **新增** | **diff-added-bg, diff-removed-bg** | diff 行背景与字级高亮是两个渲染器都需要的形态(delta/Claude Code diff 键族先例);v0 只到行背景粒度,字级高亮留扩展点 |
| **新增** | **mark** | 搜索/匹配命中高亮(fzf `hl` 先例),picker 是核心界面(ADR-0012),命中高亮 ≠ 用户选区,不能与 selection 合并 |
| 不引入 | text-disabled | 非活动态 v0 用 text-subtle 承载;引入触发器:第一个真 disabled 交互组件落地 |
| 不引入 | link | v0 用 accent 承载 URL;引入触发器:accent 与链接同屏歧义的真实投诉 |
| 不引入 | overlay | 仅 GUI modal 需要;GUI ADR 落地时以 white/black-alpha 阶引入 |

v0 定稿(18 键,五族):**surface**: `bg, bg-subtle`;**text**: `text, text-subtle`;
**accent**: `accent, on-accent`;**status**: `success, warning, error, info`;
**border**: `border, border-active`;**special**: `selection, mark`;
**diff**: `diff-added, diff-added-bg, diff-removed, diff-removed-bg`。

防膨胀闸门(进规范):新增槽位必须引用一个已落地的渲染消费者;无消费者的需求进
open-items,不进槽位表。

## 3. 字体排印刻度

### 3.1 先例核验

- **Carbon**:IBM 刻度有单一方程 `Xn = Xn-1 + {INT[(n-2)/4]+1} * 2`(y₀=12px),即
  12/14/16/18/20/24/28/32/36/42/48…;productive(产品内,紧凑)与 expressive(营销/
  长文)双集;字重纪律 = Light/Regular/SemiBold 三档,"SemiBold 适合节标题,不用
  于长文";mono 栈 `IBM Plex Mono, Menlo, DejaVu Sans Mono, …, monospace`。
  (【已验证事实】,https://carbondesignsystem.com/elements/typography/overview/ ,
  页面更新 2026-09-09)
- **SLDS**:font-size 刻度 10/12/13/14/16/18/20/24/28/32/42px;line-height 三个无单
  位值 1 / 1.25 / 1.5;默认栈 `-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto,
  …`;mono 栈 `Consolas, Menlo, Monaco, Courier, monospace`。(【已验证事实】,同上
  SLDS 页)
- **shadcn/ui**:radius 从单基值派生(×0.6/0.8/1/1.4/…)——"单基值派生刻度"模式同
  样适用于字号。(【已验证事实】,§2.1 来源)
- WCAG 大字阈值 18pt/14pt bold(§1.3)——字号刻度与对比度义务的接口。
- 开发者工具的正文密度收敛在 12–14px(Carbon productive 以 14 为正文体、VS Code/
  JetBrains 默认皆在此带)。具体编辑器默认值当日未逐一核实。(【基于证据的推断】/
  【未核实】)

### 3.2 对 Cadmus 的落地

- **GUI type scale**(px,基准 13):`12 / 13 / 14 / 16 / 20 / 24 / 28`——比率 ≈1.125–
  1.25 混合,低端加密(12/13/14 三档承载密度差异,开发者工具惯例),高端只到 28
  (会话界面无 display 需求;更大的标题是营销场景,不进产品内刻度)。
- **角色而非裸数值**:`body`(13)、`body-strong`(13+600)、`caption`(12)、
  `title`(16/600)、`heading`(20/600)、`code`(`body` 同尺寸换 mono)。组件只引用
  角色。
- **字重纪律**:400/500/600 三档;<16px 不用 300(细字重 + 抗锯齿在低对比下劣化,
  WCAG 1.4.3 intent 节明示);600 只给标题/强调,不进长文(Carbon 纪律)。
- **行高**:无单位倍数——正文 1.5、标题 1.25、紧凑列表 1(SLDS 三值先例);等宽
  代码行高与正文行高对齐,保证混排基线。
- **字体栈**:UI = 系统栈(SLDS 同款 `-apple-system…` 系)或 Inter;代码 = 等宽栈
  (JetBrains Mono 优先,fallback `SF Mono/Consolas/Menlo/monospace`);GUI 不内置字体
  文件 v0(渲染器各端用系统字体,避免二进制膨胀;Nerd Font 私用区禁用不变)。
- **TUI 映射**(字号梯度不存在,层级由样式 + 位置承载):

  | 角色 | TUI 样式 |
  |---|---|
  | title/heading | bold |
  | body | 默认 |
  | body-strong | bold |
  | caption/元信息 | dim |
  | 引用/点缀 | italic(screen 系与 linux 控制台丢失,见同日 TUI 调研 §4——故 italic 永不承载独占语义) |
  | 选中态 | inverse 或 selection 背景 |
  | 状态符号 | 颜色 + glyph 双编码(§8.6) |

  NO_COLOR 只去色不去样式(【已验证事实】,no-color.org);TERM=dumb/行模式全部归零
  (同日 TUI 调研 §9.5)。

## 4. 间距刻度

- **先例**:Carbon `$spacing-01…13` = 2/4/8/12/16/24/32/40/48/64/80/96/160px(2/4/8
  倍数族,13 级);SLDS t-shirt `xxx-small…xx-large` = 2/4/8/12/16/24/32/48px。两家
  都落在 **4px 基网**上。(【已验证事实】,
  https://carbondesignsystem.com/elements/spacing/overview/ 与 SLDS 页,均为当日)
- **Cadmus 刻度**:base-4,9 级 `space-0…8` = `0/4/8/12/16/24/32/48/64px`;≥16 后
  走 ×1.5/×2 的感知等比(Carbon 高端同理)。命名用序号(Carbon)而非 t-shirt(SLDS):
  序号可无损扩展,t-shirt 会在 `xxx-` 处失控。组件内间距与组件间布局间距同源(Carbon
  允许同刻度两用的先例)。
- **语义别名**(可选层):`gap-inline`(行内元素间)= space-2、`gap-block`(block 间)
  = space-4、`inset-pane`(面板内边距)= space-4。别名只命名"关系",不新增数值。
- **TUI 映射**:终端网格单位是行列(cell),宽高比 ≈1:2。纪律:
  - 水平间距以列为单位,允许值 0/1/2/4 列(≈ space-0/2/4/8);
  - 垂直间距以行为单位,允许值 0/1/2 行;
  - 禁止任何 sub-cell 概念;一切对齐到字符网格;缩进 = 列数,与刻度同源。
  (【基于证据的推断】;与同日 TUI 调研 §3 的"间距优先于边框"一致)

## 5. 动效刻度

### 5.1 先例核验

- **Carbon 时长 token**(静态六档):`fast-01 70 / fast-02 110 / moderate-01 150 /
  moderate-02 240 / slow-01 400 / slow-02 700 ms`;微交互纪律 90–120ms;时长随位移量
  非线性增长。
- **Carbon 缓动**(productive/expressive × standard/entrance/exit 三组 cubic-bezier):
  productive standard `(0.2, 0, 0.38, 0.9)`、entrance `(0, 0, 0.38, 0.9)`、exit
  `(0.2, 0, 1, 0.9)`;禁用 bounce/stretch/急停曲线。
- **Carbon reduced-motion 立场**:永远提供静态等价物("Make sure there is always a
  way to communicate similar messages statically")。
  (以上三项【已验证事实】,https://carbondesignsystem.com/elements/motion/overview/ ,
  页面更新 2026-09-09)
- **SLDS 时长 token**:0 / 0.05 / 0.1 / 0.2 / 0.4 / 3.2 s 六档,附帧数注释。
  (【已验证事实】,SLDS 页 Time 节)
- **DTCG**:duration(ms/s)与 cubicBezier 是一等类型,transition 是复合类型
  (duration+delay+timingFunction)。(【已验证事实】,§2.1 来源)
- **Material 3 动效 token**(md.sys.motion 的 duration/easing 分档):当日未能取得一手
  页面(需 JS)。(【未核实】,https://m3.material.io/styles/motion/overview)
- **prefers-reduced-motion**:Baseline,2020-01 起全浏览器可用;值 `no-preference`/
  `reduce`;Media Queries Level 5。(【已验证事实】,https://developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-reduced-motion ,页面更新 2026-06-10)
- 终端界**不存在** REDUCE_MOTION 式环境变量惯例;现实等价物 = 管道时无动画(ADR-0012
  已定)+ 行模式/屏幕阅读器无动画(gh 先例)+ 配置项一键关。(【未核实】否定性结论,
  按"不存在"设计;同日 TUI 调研 §6)

### 5.2 对 Cadmus 的落地:能力档案分层

动效 token 分两层定义,渲染器按能力档案消费:

- **可迁移子集(TUI + GUI)**:`spinner-interval`(帧间隔,两档:80ms/160ms)、
  `state-settle`(任务完成的"一次性状态色落定",时长 = duration-moderate-01,
  TUI 实现为瞬时切色)。其余一切动效是 GUI 专有。
- **GUI 专有序列**:时长刻度 = `instant 0 / fast-01 70 / fast-02 110 / moderate-01
  150 / moderate-02 240 / slow-01 400 / slow-02 700 ms`(Carbon productive 六档 +
  instant);缓动 = `standard / entrance / exit` 三条 cubic-bezier,值取 Carbon
  productive 三曲线;不用 expressive 系列(终端风格审美)。
- **reduced-motion 的 token 表达**:不在每个 token 上做文章,而是定义**动效能力档
  位** `motion = full | reduced | none`:
  - `full`:全部允许;
  - `reduced`:位移/缩放类归零,保留透明度渐变与状态变色,spinner 保留但可降为静
    态文本;
  - `none`:一切静止,spinner 替换为静态文本指示(gh 屏幕阅读器先例)。
  解析顺序:GUI = OS 设置(prefers-reduced-motion 等价物)> 配置项;TUI = 管道/
  行模式/TERM=dumb 强制 `none`(ADR-0012)> 配置项。配置项 `ui.motion` 三档,默认
  `full`(GUI 跟随 OS)/ TUI 探测。

## 6. 图标系统

### 6.1 三库对比(全部当日核实)

| 维度 | Lucide | Phosphor | Material Symbols |
|---|---|---|---|
| 许可证 | **ISC**(白名单内) | **MIT**(白名单内) | **Apache-2.0**(白名单内) |
| 数量 | 1,837(v1.45.0) | ~1,247 唯一名 × 6 粗细 ≈ 7.5k 资产 | 官方指南称 2,500+,同页字体载荷节称 3,800+ |
| 风格 | 线性 stroke,24px 网格,默认 2px 描边,严格一致性规则;可调 stroke width | 同族 6 粗细(thin/light/regular/bold/fill/duotone) | 3 风格(outlined/rounded/sharp)× 可变轴 FILL/wght 100–700/GRAD/opsz 20–48 |
| 形态 | 独立 SVG,官方宣称 tree-shakable | 独立 SVG(assets/<weight>/)+ 字体 | 可变字体为主;逐图标 SVG 在 git 仓库;字体可按 `icon_names` 子集化 |
| Rust 适配 | 逐图标内嵌即可 | 社区有 egui/leptos 端口 | 字体方案与"不内置字体"冲突 |
| 来源 | https://lucide.dev | https://github.com/phosphor-icons/core | https://developers.google.com/fonts/docs/material_symbols(页面 2024-09-26,注意时效) |

**选择**(【基于证据的推断】):GUI 图标源用 **Lucide**——stroke 线性风与终端风格同
源,数量够用,ISC 在白名单,逐图标 SVG 内嵌(`include_str!` 或编译期常量)天然
tree-shake,零运行时字体依赖。Phosphor 是可接受替代(粗细变体更富)。Material
Symbols 的字体分发模式与 Cadmus 的分发纪律不合,不采用。**只内嵌用到的图标**,图标
清单即 §8.6 的注册表,新增图标 = 新增注册表行 + 审查。

### 6.2 语义图标名 → 三档输出的映射表设计

每个语义图标一行:**语义名**(渲染器无关契约)→ **gui**:Lucide 图标名(SVG)→
**unicode**:保守 Unicode 码点(TUI 默认档)→ **ascii**:ASCII 串(降级档)。
规则:

- 禁止 Nerd Font 私用区(既定约束;同日 TUI 调研 §2:eza/yazi 默认图标集即私用区码
  点,无 Nerd Font 即豆腐块)。
- Unicode 档只选**默认文本呈现**的码点;若码点有 emoji 变体,追加 VS15(U+FE0E)强
  制文本呈现。当日核实:Geometric Shapes 块(U+25A0–25FF)中 U+25AA/25AB/25B6/25C0/
  U+25FB–25FE 有 emoji 变体;Dingbats 块中 ✅(U+2705)、❌(U+274C)、❓(U+2753)、
  ❗(U+2757)等默认 emoji 呈现——**避开**。(【已验证事实】,
  https://en.wikipedia.org/wiki/Geometric_Shapes_(Unicode_block) 与
  https://en.wikipedia.org/wiki/Dingbats_(Unicode_block) ,以 Unicode 17.0 图表为准;
  权威源是 unicode.org 图表,实现时以 icon-audit 快照测试钉死)
- 宽度一律经 unicode-width(UAX#11;CJK 上下文 Ambiguous 宽 2,同日 TUI 调研 §2);
  图标后间距按终端实测可配(eza `EZA_ICON_SPACING` 先例)。
- 语义图标对相邻背景过 3:1(WCAG 1.4.11,符号字符计入非文本,§1.3)。

### 6.3 v0 语义图标注册表(候选)

| 语义名 | gui (Lucide) | unicode | ascii | 核验状态 |
|---|---|---|---|---|
| success | check | ✓ U+2713 | `v` | 码点【已验证事实】(Dingbats 图表,默认文本呈现) |
| error | x | ✗ U+2717 | `x` | 同上 |
| warning | triangle-alert | ⚠ U+26A0 | `!` | 码点当日未取一手图表【未核实】 |
| info | info | ℹ U+2139 | `i` | 同上 |
| running(working) | loader-circle | braille 帧序列(⠋⠙⠹…) | `- \ | /` 轮换 | braille 可用性【已验证事实】(btop 三档先例);逐帧码点【未核实】 |
| blocked | circle-alert | ◆ U+25C6 | `*` | 码点【已验证事实】(Geometric 图表) |
| selected | check / 行底色 | ● U+25CF | `>` | 同上 |
| collapsed/expanded | chevron-right/down | ▸ U+25B8 / ▼ U+25BC | `>` / `v` | 同上(▸ 无 emoji 变体;▼ 同上) |
| git-branch | git-branch | 无安全候选(⑂ U+2442 OCR 字符覆盖率未核实) | 文本 `branch:` | 【未核实】 |
| file | file | 无(emoji 码点宽度危险) | 无标记 | 【基于证据的推断】 |
| folder | folder | 目录以尾随 `/` 表达(`ls -F` 惯例) | `/` | 同上 |
| link | link | 无安全候选 | 文本 | 【未核实】 |
| search | search | 无安全候选 | `/` 提示符 | 同上 |
| spinner 帧 | (GUI 用动画) | braille/block 两档(btop 先例) | 上同 | 【已验证事实】先例 |

表格式即契约:**新增语义图标 = 新增一行 + icon-audit 测试钉住三档输出**;语义名不
含组件名与渲染器名。

## 7. 主题文件格式与 Rust 加载

### 7.1 格式与结构

- 单文件;`base`(内置四预设之一)+ `overrides`(语义槽位子集,每槽位可给
  `{dark, light}` 双值或单值双用)是"Gemini 小配置面 + Claude Code base+overrides +
  OpenCode/yazi 双槽"的拼接,同日 TUI 调研 §7 已收敛此结论。
- **未知键容错**:解析时忽略并收集警告(不报错);非法值回退 base 对应槽位并警告
  (Claude Code "未知 token/非法值忽略"先例,同日 TUI 调研 §7)。容错面的边界:结构
  性错误(非表、非字符串色值)仍报 miette 三段式错误——静默吞掉会让用户以为主题生
  效了。
- TOML vs JSON:TOML 支持注释,适合人写;JSON 零新依赖(serde_json 已在 workspace)。
  `toml` crate 当日核实:1.1.6+spec-1.1.0(2026-09-10 发布),MIT OR Apache-2.0,活
  跃维护。(【已验证事实】,https://docs.rs/crate/toml/latest)引入需走
  `adding-dependencies` 流程(ASK first)。

### 7.2 色值语法与 OKLCH 的工程位置

色值文法(v0):`#rgb` / `#rrggbb` / `#rrggbbaa` + ANSI 16 命名色(`black`…`white`
+ `bright-*`)+ `default`(渲染器默认前景/背景)。**不接受 `oklch()`**——OKLCH 只
存在于离线生成器(§1.5),运行时零色彩学依赖;主题作者要自定义色阶,用生成器产出
hex,而不是手写 OKLCH。(【基于证据的推断】;若未来要在加载期接受 OKLCH,候选实现
当日已核实,见 §7.3,届时走 adding-dependencies 流程)

### 7.3 依赖事实表(当日 crates.io API 核实)

| crate | 版本 | 许可证 | MSRV | 维护状态 | 结论 |
|---|---|---|---|---|---|
| palette | 0.7.7(2026-08-02) | MIT OR Apache-2.0 | 1.71 | 活跃 | 白名单内;若引入,用于**生成器**(xtask)的 OKLCH 构造与 gamut map |
| color(linebender) | 0.3.3(2026-05-05) | Apache-2.0 OR MIT | 1.82 | 活跃 | 白名单内,更轻(约 5k 行);备选 |
| toml | 1.1.6+spec-1.1.0(2026-09-10) | MIT OR Apache-2.0 | —(随 serde 生态) | 活跃 | 主题文件若选 TOML |
| notify | 8.2.0 / 9.0.0-rc.5 | **CC0-1.0** | 1.77 / 1.88 | 活跃 | **不在白名单(deny.toml allow 列表无 CC0),热重载不得使用** |

(【已验证事实】:https://crates.io/api/v1/crates/palette 、…/color 、…/notify ;
白名单:Cadmus/deny.toml `[licenses] allow` = MIT / Apache-2.0 / Apache-2.0 WITH
LLVM-exception / ISC / Unicode-3.0 / 0BSD)

### 7.4 热重载

`notify` 不可用(§7.3),方案:**UI 事件循环上的 mtime 轮询**(间隔 1s,time/IO 按
AGENTS.md 惯例以构造参数注入),发现变更 → 重读 → 校验 → 原子换入;失败保留旧主题并
警告。TUI 主循环本就有 tick;GUI 端同理可用框架定时器。零新依赖,语义与 watch 等价
(主题文件是低频小文件)。(【基于证据的推断】)

---

## 8. 《Cadmus 设计 token 规范 v0》草案(可进 ADR)

> 说明:按仓库惯例先行中文版供评审;随 ADR 落地时需出英文版(English-first)。

### 8.0 范围与原则

1. 本规范是 GUI 与 TUI 共享的设计 SSOT。token 分三层:**L0 scale**(12 级色阶,仅存
   在于生成器与内置预设)、**L1 semantic**(语义槽位,运行时唯一契约)、
   **L2 component**(组件别名,默认空集)。组件只引用 L1/L2,禁止硬编码色值、字号、
   间距、时长。
2. 渲染器无关:token 不含渲染器名与渲染器专用单位;渲染差异由**能力档案**(§8.7)
   表达。
3. 防膨胀闸门:新增槽位/刻度/图标必须引用一个已落地的渲染消费者。

### 8.1 token 分层结构

```
L0 scale      gray-1…12, accent-1…12, overlay-black/white-alpha-1…12   (明暗各一套)
L1 semantic   §8.2 的 18 槽位,每槽位 = {dark 值, light 值}
L2 component  默认空;新增需 ADR 或 PR 评审(引用 L1,不引 L0)
```

L0 → L1 的映射是固定表(预设生成时烘焙),不暴露给主题作者的配置面(作者只覆写 L1)。

### 8.2 语义槽位定稿(18 键)与默认映射

| 槽位 | 族 | L0 默认映射(light / dark) | 对比度门(对 bg) | ansi 预设值(light / dark) |
|---|---|---|---|---|
| bg | surface | gray-1 / gray-1 | — | white / black |
| bg-subtle | surface | gray-2 / gray-2 | — | white / black |
| text | text | gray-12 / gray-12 | ≥ 4.5:1 | black / white |
| text-subtle | text | gray-11 / gray-11 | ≥ 3:1,仅元信息/次级 | bright-black / bright-black |
| accent | accent | accent-9 / accent-9 | 实心面配合 on-accent | blue / blue |
| on-accent | accent | 白 / 深灰(按 accent-9 亮度判定,生成器断言 ≥4.5:1) | ≥ 4.5:1 | white / black |
| success | status | green 系 11 / 11 | ≥ 4.5:1(文本用法)/ ≥3:1(glyph) | green / green |
| warning | status | 同构 | 同上 | yellow / yellow |
| error | status | 同构 | 同上 | red / red |
| info | status | 同构 | 同上 | cyan / cyan |
| border | border | gray-6 / gray-6 | 无强制(分隔);交互边界 ≥3:1 | bright-black / bright-black |
| border-active | border | accent-7 / accent-7 或 gray-7(focus) | ≥ 3:1 | blue / blue |
| selection | special | accent-3 / accent-3 | 选区内 text 仍 ≥4.5:1 | reverse(反色,无语义色) |
| mark | special | accent-4 / accent-4(与 selection 区分的搜索命中) | 同上 | reverse + bold |
| diff-added | diff | green 系 11 / 11 | ≥ 4.5:1 | green / green |
| diff-added-bg | diff | green 系 3 / 3 | 其上文本 ≥4.5:1 | 默认背景(无语义色) |
| diff-removed | diff | red 系 11 / 11 | ≥ 4.5:1 | red / red |
| diff-removed-bg | diff | red 系 3 / 3 | 其上文本 ≥4.5:1 | 默认背景(无语义色) |

语义色(success/warning/error/info)在 L0 各是一条 12 级彩阶(由 accent 构造流程的
H 替换生成);v0 主题作者不可改其色相,只能整槽覆写。状态永远双编码(颜色 +
glyph/位置),行模式有纯文字等价物(同日 TUI 调研 §8/§9.8)。

### 8.3 调色板构造流程(生成器规格)

输入 `H_a`(accent 色相)、可选 `C_a`、`H_g`;流程 = §1.5 的 8 步(L 锚点 → L 曲线
→ C 曲线 → 恒 H → gamut map → 对比度门断言 → 烘焙 hex → 快照矩阵)。生成器是离线
工具(xtask 子命令),可引 `palette`(白名单内,§7.3);输出 = 四个内置预设
(`dark / light / dark-ansi / light-ansi`)的 Rust 源或资源文件;产物进版本库,diff
可评审。对比度门不过 = 生成失败,不允许手工调值绕过(改参数,不改结果)。

### 8.4 type 刻度表

| token | GUI 值 | TUI 映射 |
|---|---|---|
| font.ui | 系统栈(SLDS 式)或 Inter | 终端字体(不控制) |
| font.mono | JetBrains Mono → SF Mono/Consolas/Menlo → monospace | 同上 |
| text.caption | 12px / 400 / lh 1.25 | dim |
| text.body | 13px / 400 / lh 1.5 | 默认 |
| text.body-strong | 13px / 600 / lh 1.5 | bold |
| text.title | 16px / 600 / lh 1.25 | bold |
| text.heading | 20px / 600 / lh 1.25 | bold |
| text.code | = text.body,font.mono | 默认(等宽是终端天然) |

刻度延伸档 24/28 保留给 GUI 面板标题,不进 v0 角色表。字重只用 400/500/600;<16px
禁用 ≤300;italic 仅点缀,不承载独占语义。

### 8.5 间距刻度表

| token | GUI(px) | TUI |
|---|---|---|
| space-0…8 | 0 / 4 / 8 / 12 / 16 / 24 / 32 / 48 / 64 | 0 / — / 1 列 / — / 2 列 / — / 4 列 / — / —(垂直:0/1/2 行) |
| gap-inline | = space-2(8) | 1 列 |
| gap-block | = space-4(16) | 1 行 |
| inset-pane | = space-4(16) | 1 列 + 0 行(面板内边距) |

禁止刻度外数值;TUI 禁止 sub-cell;间距优先于边框(无边框会话流纪律)。

### 8.6 动效刻度表

| token | 值 | 说明 |
|---|---|---|
| duration.instant | 0ms | 无过渡 |
| duration.fast-01 / fast-02 | 70 / 110ms | 微交互(hover、按键反馈) |
| duration.moderate-01 / moderate-02 | 150 / 240ms | 展开、picker 出现、toast |
| duration.slow-01 / slow-02 | 400 / 700ms | 大展开、背景压暗 |
| easing.standard | cubic-bezier(0.2, 0, 0.38, 0.9) | 始终可见元素 |
| easing.entrance | cubic-bezier(0, 0, 0.38, 0.9) | 进入 |
| easing.exit | cubic-bezier(0.2, 0, 1, 0.9) | 离开 |
| spinner.interval | 80 / 160ms 两档 | 唯一可迁移动画 |
| motion | full / reduced / none | 能力档位,解析顺序见 §5.2;reduced = 位移缩放归零、保留透明与变色;none = 全静止 + 静态文本指示 |

TUI 消费子集:`spinner.interval`、`state-settle`(= moderate-01 的瞬时切色)、
`motion`(管道/行模式/TERM=dumb 强制 none)。禁用 bounce/elastic 曲线。

### 8.7 渲染器能力档案(降级契约)

| 能力轴 | 档位(强 → 弱) | 消费规则 |
|---|---|---|
| color | truecolor → 256 → 16 → none | 渲染层函数:hex → 最近 256 → ANSI 16 槽位 → 去色(NO_COLOR);主题文件不感知 |
| glyph | unicode → ascii → none(行模式) | 图标注册表三档输出;禁 Nerd Font 私用区 |
| motion | full → reduced → none | §8.6 |
| font-style | full → no-italic → none | italic 丢失时语义已由位置/双编码兜底;TERM=dumb 全归零 |

探测链沿用已定结论:COLORTERM → OSC 10/11(100ms 超时)→ COLORFGBG → dark-ansi 兜底
(同日 TUI 调研 §1.3/§7;Windows Terminal/tmux 结构性不可探测,接受 dark 误判成本)。

### 8.8 图标映射表格式

见 §6.2/§6.3:语义名 → `{ gui: lucide 名, unicode: 码点(可含 VS15), ascii: 串 }`;
三档由渲染器按 glyph 能力轴选择;新增 = 注册表加行 + icon-audit 快照测试。

### 8.9 主题文件 schema(v0)

```toml
# $XDG_CONFIG_HOME/cadmus/themes/<name>.toml — 单文件主题
# meta 必填;base 默认 "auto"(探测决定 dark-ansi/light-ansi 系)
[meta]
name = "example"
author = "…"
variant = "dual"        # dual | dark | light —— dual 主题必须提供两套值或依赖 base 双值
version = 1             # schema 版本,未知主版本拒绝加载并报错

base = "dark"           # dark | light | dark-ansi | light-ansi;未设置的槽位从 base 继承

# overrides:只允许 §8.2 的 18 个键;未知键忽略并警告
[overrides.dark]
accent = "#7aa2f7"
text-subtle = "#9aa0a6"

[overrides.light]
accent = "#3b5bdb"

# 单值简写(明暗同用)——写在 [overrides] 顶层
[overrides]
border-active = "#7aa2f7"
```

加载语义:

1. 解析(TOML;JSON 等价结构同样接受)。结构错误 → miette 三段式报错(`code` +
   `help`),不启动主题加载降级链之外的静默兜底。
2. 未知键:收集为警告,逐键列出,不影响加载。非法色值:回退 base 对应槽位 + 警告。
3. 槽位解析顺序:`overrides.{dark,light}` > `overrides`(单值)> base 预设。
4. 色值文法:§7.2(hex / ANSI 16 名 / `default`)。`default` 意为渲染器默认色
   (TUI = 终端默认前景/背景;GUI = base 值)。
5. 热重载:mtime 轮询(1s,time/IO 注入);变更加载失败保留旧主题并警告。
6. 主题目录限 `$XDG_CONFIG_HOME/cadmus/themes`(同日 TUI 调研 §9.10);内置预设随二
   进制分发,不可被同名文件遮蔽(避免影子主题的排障噩梦)。

### 8.10 一致性与测试

- 槽位完整性测试:18 键 × 4 预设全有值;对比度门作为单元测试重放(生成器输出进
  库,门断言进 CI)。
- 渲染矩阵快照:明暗 × 色深四档 × glyph 三档,进 `just snapshot-review`。
- icon-audit:注册表每行的 unicode 码点断言(块归属、无 emoji 默认呈现、宽度)。
- schema 容错测试:未知键/非法值/结构错误三类 fixture。

## 9. 来源清单(全部 2026-09-13 当日访问,另注明的除外)

- Evil Martians, OKLCH in CSS(2025-09-17 更新):https://evilmartians.com/chronicles/oklch-in-css-why-quit-rgb-hsl
- Radix Colors, Understanding the scale / Scales / Aliasing:
  https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale 、
  https://www.radix-ui.com/colors/docs/palette-composition/scales 、
  https://www.radix-ui.com/colors/docs/overview/aliasing
- material-color-utilities README(HCT/tonal palette/语言矩阵):
  https://github.com/material-foundation/material-color-utilities/blob/main/README.md
- WCAG 2.2 Understanding SC 1.4.3 / 1.4.11(2026-06 更新):
  https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html 、
  https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html
- W3C DTCG Design Tokens Format Module 2025.10(Draft CG Report):
  https://tr.designtokens.org/format/ 、https://www.designtokens.org/TR/2025.10/format/
- shadcn/ui Theming:https://ui.shadcn.com/docs/theming
- Salesforce Lightning Design Tokens(Site v1.2.55,2026-09-06):
  https://www.lightningdesignsystem.com/design-tokens/
- Carbon Typography / Spacing / Motion(页面更新 2026-09-09):
  https://carbondesignsystem.com/elements/typography/overview/ 、
  https://carbondesignsystem.com/elements/spacing/overview/ 、
  https://carbondesignsystem.com/elements/motion/overview/
- MDN prefers-reduced-motion(2026-06-10 更新):
  https://developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-reduced-motion
- Lucide(v1.45.0,ISC,1,837 icons):https://lucide.dev
- Phosphor core(MIT):https://github.com/phosphor-icons/core
- Material Symbols 指南(Apache-2.0;页面 2024-09-26,注意时效):
  https://developers.google.com/fonts/docs/material_symbols
- Wikipedia:Geometric Shapes / Dingbats Unicode 块(以 Unicode 17.0 图表为准):
  https://en.wikipedia.org/wiki/Geometric_Shapes_(Unicode_block) 、
  https://en.wikipedia.org/wiki/Dingbats_(Unicode_block)
- crates.io API:palette 0.7.7 / color 0.3.3 / notify 8.2.0 / toml(docs.rs):
  https://crates.io/api/v1/crates/palette 、https://crates.io/api/v1/crates/color 、
  https://crates.io/api/v1/crates/notify 、https://docs.rs/crate/toml/latest
- 仓内:Cadmus/deny.toml(许可证白名单);
  docs/research/2026-09-13-tui-aesthetic-style-research.md(同日,终端侧先例:探测链、
  降级、glyph 纪律、主题架构先例、状态双编码、动画白名单);
  Gemini CLI themes / Claude Code terminal-config(经同日 TUI 调研转引)
- 当日未能取得一手来源(标记【未核实】处):m2.material.io dark-theme、m3.material.io
  motion 与 design-tokens 页(均需 JS);U+26A0/U+2139/U+2442 的 Unicode 一手图表;
  braille spinner 逐帧码点;VS Code/JetBrains 默认字号。
