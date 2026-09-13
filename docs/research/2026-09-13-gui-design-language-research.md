# GUI 设计语言调研:Linear 主参照系与候选风格方向——2026-09-13 基线

> 调研范围:终态主力界面(终端风格 GUI:文本密集、键盘优先)的**设计语言事实清单与
> 风格方向候选**——以 Linear 为主参照系,补充参照覆盖 agent 特异场景(流式正文、diff、
> 审批 prompt、多状态列表、命令面板)。不碰色彩科学(OKLCH/Radix 12 级调色板构造由
> `2026-09-13-design-token-engineering.md` 覆盖,本报告消费其方法论、只取 Linear 的
> 色值倾向);与 `2026-09-13-tui-aesthetic-style-research.md`(终端侧先例)、
> `2026-09-11-agent-uiux-landscape.md`(agent 界面全景)互补,交叉引用而不重复。
> 已有结论(不重研):主题架构 = 语义槽位 + 明暗双值 + 渲染层降级。
> 消费者:GUI 设计语言 ADR(与 token ADR 并列的前置)。
>
> 置信度标注:【已验证事实】= 2026-09-13 当日一手来源(官方文档/官网页面,附 URL);
> 【基于证据的推断】= 从已验证事实外推;【未核实】= 当日未能取得一手来源。
> 色值、字号等具体数字以一手来源为准;拿不到即标【未核实】,不编。

## 摘要

1. **Linear 主题工程真相(§1)**:LCH 感知均匀空间,明暗主题同源生成,每主题仅
   base/accent/contrast 三变量;accent 蓝紫但刻意限制色度求中性;Inter Display 标题 +
   Inter 正文;文本 4 级层级;状态图标用填充比例而非颜色;动效为 ~160ms ease-out 量级。
   (一手:linear.app redesign part Ⅱ + 官网 HTML/CSS/SVG,2026-09-13)
2. **终端适配结论(§2)**:骨架(灰阶、accent 节制、密度、键盘结构、状态几何)可直接
   迁移;皮肤(elevation 多层、双字体、平滑动画、hover)走降级;伤筋动骨仅字号梯度与
   社交头像两项,均可代偿。Linear 主参照系成立,参照的是结构纪律而非像素表象。
3. **agent 缺口补参照(§3)**:Zed Agent Panel 给出流式工具指示行、checkpoint 按钮、
   hunk 级 diff 审查、工具权限三态;Raycast 给出 List/Grid/Detail/Form 四组件 +
   ActionPanel 全快捷键的键盘优先结构;Linear Inbox 给出类型中心的多状态列表。
4. **候选方向(§4)**:A 高纯度 Linear 移植 / B Linear 骨+对话流肉(推荐)/ C 终端原生
   调味。推荐 B:Linear 可迁移核心全保留,agent 场景从补丁升级为骨架,TUI 降级路径最短。
5. **未核实项**:Linear 具体色值/字号/间距刻度、边框纪律、app 内动效体系;Raycast
   审批 prompt 视觉形态与 List accessories 细节——均因一手来源当日不可得或预算耗尽,
   已在对应小节标注,建议消费本报告前由 ADR 阶段补充或明确接受为设计自由度。

---

## 1. P0 — Linear 设计语言事实清单

来源预算:4–5 个一手来源(linear.app 官方 blog/method/changelog 设计文章、品牌页),
高质量第三方拆解可补。

### 1.1 灰阶层级与色值倾向

- 官网 theme-color = `#08090a`:极暗近黑、几乎中性(蓝色分量仅 +2)。
  (【已验证事实】,linear.app 页面 HTML meta,2026-09-13)
- 官方 CSS 变量存在 `--color-text-quaternary`,证实**文本至少分 4 级**
  (primary/secondary/tertiary/quaternary);另有 `--font-weight-normal`、
  `--text-regular-size/-line-height/-letter-spacing` 等排版变量。(【已验证事实】,同上)
- elevation 层级通过黑白透明度叠加探索后回落到生成系统:surfaces 分
  background/foreground/panels/dialogs/modals。(【已验证事实】,redesign part Ⅱ,URL 见 §1.2)
- 具体灰阶各级色值:【未核实】(一手文章未给数值;要拿需解析 app 内 CSS,超出当日预算)。

### 1.2 accent 色相与饱和度策略

- 品牌 accent 为蓝紫(文中称 "blue",品牌页/官网一贯为靛蓝紫系),但 2024 重设计
  **刻意限制 accent 色度参与全局色彩计算**:"return to a more neutral and timeless
  appearance … achieved by limiting how much chrome (blue in our case) was used in the
  calculations applied to our color system"。即 accent 只点在最少量关键控件上,灰阶
  近乎纯中性。(【已验证事实】,
  https://linear.app/blog/how-we-redesigned-the-linear-ui)
- 具体 accent 色值(OKLCH/hex):【未核实】——官方文章未给出一手数值,不编。

### 1.3 明暗主题关系

- **同一套生成系统产出明暗双主题**:LCH 色彩空间(perceptually uniform)做主题生成,
  每个主题只定义 **3 个变量:base color、accent color、contrast**,派生出 surfaces
  (background/foreground/panels/dialogs/modals 等 elevation 层级)、texts、icons、
  controls 的别名。contrast 变量还可生成高对比无障碍主题。(【已验证事实】,同上)
- 探索阶段 Karri 用黑白透明度叠加表达 elevation/hierarchy 关系,再映射回变量系统。
  (【已验证事实】,同上)
- 2024 重设计提高对比:light mode 文本与中性图标更深,dark mode 更浅。(【已验证事实】,同上)
- 与 Cadmus 已有结论的关系:【基于证据的推断】"3 变量生成全主题"印证语义槽位+
  渲染层降级架构可行,且比手工维护 ~98 个变量更可持续;Cadmus 的语义槽位方案
  (18 键,见 design-token-engineering §摘要)介于两者之间,更贴合终端。

### 1.4 字体族 / 字号梯度 / 字重纪律

- 字体族:**Inter Display 用于标题**(增加表达力),**常规 Inter 用于其余所有文本**。
  (【已验证事实】,同上)
- 具体字号梯度 / 字重数值:【未核实】——一手文章未列;第三方拆解常见 11–13px 正文、
  400/500/600 字重的说法,当日未取得可引用的一手来源。

### 1.5 密度与间距哲学

- 重设计目标原文:"reduce visual noise, maintain visual alignment, and increase the
  hierarchy and **density** of navigation elements"——密度是导航层级的手段,chrome
  收紧为内容让位。(【已验证事实】,同上)
- 具体间距刻度(4pt grid 等):【未核实】。

### 1.6 边框与分隔线纪律

- 【未核实】一手文章未直接陈述边框纪律;redesign part Ⅱ 只说 sidebar/tabs/headers/panels
  "reduce visual noise, maintain visual alignment"。第三方常见观察(1px 半透明 hairline、
  圆角 6–8px)当日未拿到可引用来源,不编。

### 1.7 动效用法(什么动 / 什么不动)

- 官网第一方 CSS 出现 `transition:160ms var(--ease-out-quad)`(图标 morph),
  证实动效节奏是**短时长(~160ms)+ ease-out 曲线**的量级。(【已验证事实】,linear.app
  页面内联样式,2026-09-13;注:仅营销站证据,app 内体系未核实)
- redesign part Ⅱ 全文未提动画/过渡设计语言——【基于证据的推断】动效在 Linear 是
  隐性层(手感而非卖点),服务于状态反馈而非装饰。

### 1.8 图标风格

- 官网 sprite 证实:自定义图标集,**16×16 网格**,实心(fill)与描边(stroke,
  圆角 linecap)并存,状态图标用几何原语组合(IssueStatus 系列:backlog 虚线圆、
  todo 空心圆、started 半填、review 四分之三填、done 实心勾)——**用填充比例表达进度**,
  而非依赖颜色。(【已验证事实】,linear.app 内联 SVG sprite,2026-09-13)

---

## 2. P1 — 终端适配性结论

### 2.1 可直接迁移的元素

【基于证据的推断】(依据:§1 事实 + tui-aesthetic-style-research 的降级先例)

- **近中性灰阶 + 单一 accent 的节制策略**:Linear 灰阶近中性、accent 只点关键控件
  (§1.1/§1.2)。终端 16 色/256 色同样适合"灰阶为主 + 一个 accent"的纪律,语义槽位
  直接映射。
- **文本 4 级层级**(§1.1 `--color-text-quaternary`):映射为 fg / fg-dim /
  fg-faint(dim 属性)/ fg-inverse,与 token 报告 §摘要的 TUI 字号梯度由 bold/dim/
  inverse 承载一致。
- **密度即层级**(§1.5):行距、留白、对齐纪律是纯布局属性,不依赖像素渲染。
- **图标填充比例表达状态**(§1.8 IssueStatus):Unicode 圆符(○◔◑◕●)可近乎等义
  承载 backlog→done 的进度语义。
- **键盘优先 = 结构而非装饰**(§3.5):ActionPanel 式命令面板在终端天然成立。

### 2.2 只能降级的元素

【基于证据的推断】

- **elevation 分层**(§1.3 的 background/foreground/panels/dialogs):GUI 靠多层表面
  色;TUI 降级为 2–3 级背景 + 边框,深层模态只能靠 box-drawing 边框区隔。
- **细边框 hairline → box-drawing 字符**(─│┌┐):视觉权重上升,需用 dim 色补偿。
- **160ms ease-out 过渡**(§1.7):终端只剩离散帧;降级为 spinner 帧动画与状态落定
  变色(与 token 报告 §摘要 4 一致)。
- **hover 反馈**:终端无 hover(或不可靠);焦点态必须独立于 hover 存在。

### 2.3 会丢失且伤筋动骨的元素

【基于证据的推断】

- **字号梯度**(Inter Display vs Inter 的标题区分,§1.4):终端单一字号,标题层级只能
  靠 bold/大写/间距代偿——这是 Linear 排版表现力的主要损失。
- **头像/面孔**(§3.4 Linear 通知 "emphasized the faces"):终端无法渲染头像,
  agent/人类参与者区分只能靠图标与颜色,社交线索丢失。
- **亚像素对齐与视觉对齐微调**(§1.5 "visual alignment"):字符网格强制对齐,
  反而简化问题,不算伤筋动骨——记录为中性损失。

### 2.4 适配度总结论

【基于证据的推断】**Linear 风格的"骨架"(中性灰阶、节制 accent、密度、键盘结构、
状态几何语义)终端适配度高,可视为可迁移核心;"皮肤"(elevation 多层表面、字体
双族、平滑动效、hover)全部走降级路径;真正伤筋动骨的只有字号梯度与社交头像两项,
均可用排版/图标代偿。** 结论:Linear 作为主参照系成立,但参照的是其结构与纪律,
不是其像素表象。

---

## 3. P2 — agent 场景缺口与补充参照

Linear 不是 agent 软件;以下场景各取一个补充参照的最优解(每参照最多 1 个来源)。

### 3.1 流式正文 / 代码块(Zed)

- 响应流式进入,**用指示器展示模型正在调用哪些工具**(tool-use indicators 内联在流里)。
  (【已验证事实】,https://zed.dev/docs/ai/agent-panel ,2026-09-13)
- 用户消息是可点击的卡片(点击可编辑重发);长对话用底部滚动箭头跳到最近 prompt,
  键盘可 Shift+PageUp/Down 在消息间跳转。流式正文本身不做花哨渲染,导航与可操作性
  才是设计重点。(【已验证事实】,同上)
- 【基于证据的推断】Cadmus 可借鉴:流式区的"进度感"靠工具调用指示行 + 消息边界,
  而不是靠动画;正文 Markdown 渲染保持朴素,交互密度放在消息级操作。

### 3.2 diff 视图(Zed)

- 编辑汇总条(accordion,位于输入框上方)报告改了哪些文件、多少行;"Review Changes"
  (⇧⌃R) 打开 multi-buffer 审查页,**hunk 级 keep/reject**,或整体接受/拒绝;也可
  `agent.single_file_review` 让 diff 内联进单文件并暂时覆盖 git diff 显示。
  (【已验证事实】,同上)
- 每次模型执行编辑,消息顶部出现 "Restore Checkpoint" 按钮——中断到一半的编辑也
  能回滚。(【已验证事实】,同上)
- 【基于证据的推断】Cadmus 借鉴:diff 审查 = 汇总条 + 全量审查视图两层;hunk 是
  最小操作单元;checkpoint 按钮钉在产生变更的消息上,位置语义 = "回到这条消息之前"。

### 3.3 审批 / 确认 prompt(Zed 工具权限 + Raycast【未核实】)

- Zed:Tool Permissions 三态——**allowed / denied / confirmed**,按 profile 配置工具集。
  (【已验证事实】,同上)确认交互的具体视觉形态文档未描述。【未核实】
- Raycast 审批 prompt 形态:【未核实】(当日未取到一手页面;预算转向其他条目)。

### 3.4 多状态列表(Linear 通知 + Raycast List)

- Linear 2024 重设计把 Inbox 通知改为**以通知类型为中心**组织、突出队友头像,
  并简化 header 与 filter。(【已验证事实】,redesign part Ⅱ,URL 见 §1.2)
- Raycast:多状态同类项归 **List** 组件管(官方定位 "show multiple similar items");
  加载态用顶层 `isLoading` 指示条(顶部),而非骨架屏。(【已验证事实】,
  https://developers.raycast.com/api-reference/user-interface ,2026-09-13)
- List 行内状态附件(accessories/tag 细节):【未核实】。

### 3.5 键盘优先命令面板美学(Raycast)

- 设计系统只有 **4 个高层组件:List / Grid / Detail / Form**——用极小的词汇表
  保证一致性("Think of it as a design system")。(【已验证事实】,同上)
- **ActionPanel 承载 Action 列表,每个 Action 可绑快捷键**;官方原话 "Shortcuts allow
  users to use Raycast without using their mouse"。键盘优先不是主题,是组件结构的一部分。
  (【已验证事实】,同上)
- 响应性纪律:"render something as quickly as possible",数据未到先给顶部 loading 指示。
  (【已验证事实】,同上)
- 【基于证据的推断】Cadmus 借鉴:agent GUI 的键盘优先应落成结构性规则——一切动作进
  ActionPanel 式命令面板并带快捷键;主词汇表保持极小(列表/详情/表单/网格四件套)。

---

## 4. P3 — 候选风格方向

### 4.1 方向 A:「Linear 直系」——高纯度移植

- 定位:把 Linear 的结构纪律(中性灰阶 + 蓝紫 accent + 密度 + 状态几何)原样设为
  Cadmus 基调,agent 场景缺口全部按 §3 补参照缝入。
- 色:近中性灰阶,accent 用蓝紫系单色、极低出现率;明暗同构生成(base/accent/
  contrast 三变量思路,生成方法交 token 报告)。字:Inter 或等效人文无衬线,
  标题/正文双族。距:高密度导航 + 内容区克制留白,对齐纪律优先于装饰。
  框:细 hairline、圆角小,分隔线优先于卡片。动:~160ms ease-out 只做状态反馈。
  图标:16px 网格自定义集,状态用填充比例原语。
- TUI 降级映射:灰阶→fg 三档 + bg 两档;accent→唯一品牌色槽;hairline→dim 色
  box-drawing;动效→spinner + 落定变色;图标→Unicode 三档(◔◑◕●)。
- 风险:Linear 是项目管理工具,其"导航密度"审美不天然适配对话流主界面;照搬到
  agent 场景有语境错位风险,需 §3 补丁面积大。【基于证据的推断】

### 4.2 方向 B:「Linear 骨 + 终端肉」——中纯度,对话为主

- 定位:保留 Linear 的灰阶/accent/边框/图标纪律,但布局骨架以 **agent 对话流为中心**
  (Zed Agent Panel 式:流式正文 + 工具指示行 + hunk 审查),导航 chrome 减到最少。
- 色:同 A 的灰阶纪律,accent 同时承担"agent 正在工作"的状态色。字:单族等宽为体,
  正文用 UI 字体、代码用等宽的双轨。距:消息卡片间距 > 导航密度;正文行距宽松。
  框:只有 diff、审批 prompt、代码块三类容器有框,其余靠留白。动:流式期间只有
  工具指示行动,落定后静。图标:Linear 式状态原语 + 工具类型小图标。
- TUI 降级映射:对话流本身就是文本,几乎无损耗;diff 用 +/- 行与 hunk 分隔线;
  审批 prompt 用边框 + 快捷键提示(Raycast Action 式 `⌘K` 提示降级为底部 hint 行)。
- 风险:双字体轨在纯 TUI 无法成立,需接受 TUI 全等宽;对话居中布局在大屏上的
  宽度纪律需额外定义。【基于证据的推断】

### 4.3 方向 C:「终端原生 + Linear 调味」——低纯度

- 定位:以终端惯例为体(prompt 行、命令回显、纯文本流),只借 Linear 的三样东西:
  灰阶克制、accent 单色、状态几何图标。
- 色:终端 16 色语义重映射为主,GUI 才解锁全色域。字:全等宽。距:行距即排版。
  框:box-drawing 为主,GUI 渲染细线等价物。动:仅 spinner。图标:Unicode 优先。
- TUI 降级映射:无需降级——TUI 即原生形态,GUI 是增强路径。
- 风险:终态是 GUI 主力,方向 C 可能让 GUI 看起来"只是个好看的 TUI",浪费 GUI 的
  排版与密度能力;维护者偏好 Linear 风格,C 的 Linear 含量可能低于期望。
  【基于证据的推断】

### 4.4 推荐与理由

**推荐方向 B。** 理由:(1) 终态主力是 GUI 且维护者偏好 Linear——A 的 Linear 纯度
满足偏好,但 A 的布局骨架(导航密度)与 agent 对话主流不匹配,补丁面最大;
(2) B 把 Linear 的可迁移核心(§2.1 全部条目)留下,把 agent 场景(Zed 对话流、
审批、diff)作为一等布局,补丁变成骨架本身;(3) B 的 TUI 降级路径最短(对话流
原生是文本),符合"TUI 第一渲染器、同一 token 降级"架构;(4) C 与终态 GUI 定位冲突。
A 作为 B 的风格锚点保留:B 的色/框/图标规则即 A 的规则。【基于证据的推断】

---

## 5. 来源清单

1. https://linear.app/blog/how-we-redesigned-the-linear-ui — Linear 官方,2024-03-28,
   redesign part Ⅱ(主题生成系统、LCH、三变量、Inter Display、中性化、密度)。当日已验证。
2. https://linear.app/ (含 404 页 HTML/CSS/SVG sprite)— 一手工程产物:theme-color
   #08090a、InterVariable、--color-text-quaternary、160ms ease-out-quad、16×16 图标集。
   当日已验证。part Ⅰ 链接当日 404,内容未获取。【未核实】
3. https://linear.app/method 与 https://linear.app/quality — 已抓取,设计细节含量低,
   仅确认存在,未引用实质内容。
4. https://zed.dev/docs/ai/agent-panel — Zed 官方文档:流式工具指示、checkpoint、
   Review Changes/hunk keep-reject、Tool Permissions 三态。当日已验证。
5. https://developers.raycast.com/api-reference/user-interface — Raycast 官方:
   List/Grid/Detail/Form、ActionPanel + 快捷键、isLoading 顶部指示。当日已验证。
6. https://manual.raycast.com/ 与 https://developers.raycast.com/ — 已抓取,为目录页,
   仅用于定位来源 5。

来源预算:8/10 已用。剩余预算未用——P0 的色值/间距数值需解析 app 内 CSS 或设计
规格,性价比低,按纪律标注【未核实】收手。
