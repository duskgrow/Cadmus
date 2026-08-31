> **This file is the design basis of the rust-template template** (the template's own spec document, kept in Chinese as the original research report).
> Projects generated via `just init` remove this file — it belongs to the template, not to generated projects.
> For the trade-offs behind each tooling choice, see the ADRs under `docs/decisions/`.
>
> 本文件是 rust-template 模板的设计依据（规范调研报告原文，中文）。`just init` 生成项目时会移除此文件。模板选型的权衡见 `docs/decisions/`。

# 可复用工程规范手册：从 DeepSeek Harness、Zed 与标杆开源项目中提炼

> 研究基线：2026-08。全书结论均以该时间点前后的公开一手来源（官方仓库、官方文档、论文、标准组织）为依据。


## 1. 总论：贯穿性元原则

本章是全报告的纲领。对 Zed、rust-analyzer、DeepSeek Harness（dsh）、LLVM、uv 等标杆开源仓库十个工程维度的调研表明：那些看似分散的最佳实践——锁文件、代码生成、薄编排层、快照测试、AGENTS.md——可以被少数几条元原则统一解释，且这些原则在彼此独立的生态中被反复、无意识地重新发现。本章给出四条元原则：SSOT 的拓扑结构、自动化的五层栈与"机器准备、人类点闸"的人机分工、架构约束的机械化及其与可测试性的共生、人机双读的仓库观。后续各章（第 2–9 章）是这些原则在仓库组织、环境、自动化、测试、文档、发布、Agent 接入与用户交互等维度上的具体展开；第 10 章将其压缩为三语言速查表与冲突区决策，第 11 章给出按采纳成本排序的落地路线图。

### 1.1 单一事实来源（SSOT）是一种拓扑而非口号

#### 1.1.1 SSOT 三要素：权威位置 + 派生方式 + 机械校验器

"每条信息只在一个地方维护"作为口号几乎无人反对，但落地失败的项目无一例外缺了后两个要素。对十个维度的交叉分析显示，成熟项目的反重复手段收敛为且仅为两种合法形态：**生成**（derive）与**引用**（reference）。生成的实例包括：Zed 由 `cargo xtask workflows` 生成全部 CI YAML[^1^]、rust-analyzer 从语法定义生成代码[^2^]、git-cliff 从 Conventional Commits 生成 changelog[^3^]、clap 从命令定义生成 help 与 man page[^4^]；引用的实例包括：CLAUDE.md 只写一行 `@AGENTS.md`[^5^]、flake 直接读取 `rust-toolchain.toml` 而非复述版本号[^6^]、ADR 在别处只以编号被引用[^7^]。没有任何一个成熟项目靠"维护两份手写拷贝加人肉同步"存活。

关键在于，两种形态都必须配上**机械校验器**才构成闭环：生成物需要 drift check（Zed 用 `git diff --exit-code .github` 校验生成的 workflow 与源一致[^1^]），引用需要存活检查（文档中的命令需要 doctest 式的执行验证[^8^]）。因此 SSOT 的完整定义是一个三元组——**权威位置（authoritative location）+ 派生方式（生成或引用）+ 机械校验器**。缺了校验器，单源声明只是暂时未漂移的双源。

由此可得一个可执行的工作法：**事实登记表**——为仓库枚举每条事实（工具链版本、命令入口、API 契约、架构约束），逐条指定权威位置、派生形态与校验机制。评审任何 PR 时只问两个问题：这次变更的权威位置更新了吗？它的派生物会漂移吗？

#### 1.1.2 "生成物入库还是不入库"的判定规则

生成物是否提交 git 是经典争论，两派各有权威背书："仓库只应包含原始材料而非构建输出"[^9^]；而 Zed 生成的 workflow YAML、insta 快照、CHANGELOG 都明确要求入库[^1^][^10^]。交叉验证给出的判据只有一条：**消费者是否需要不构建即可读**。CI YAML 要被 GitHub 直接解析、快照要在 PR diff 中被人评审、CHANGELOG 要被用户在网页上阅读——这类生成物入库，但必须配 CI drift check；API 文档站点、man page 等有构建管线消费的生成物则不入库。两派共享的底线完全一致：生成物绝不手改。

#### 1.1.3 反重复的完整定义：三种同构的重复

DRY 常被窄化为"不复制粘贴代码"。调研显示成熟项目治理的重复有三种同构形态：其一，**代码重复实现**，经典形态；其二，**文档与代码重复**，如 README 复述命令行参数、文档硬编码快捷键——Zed 的对策是 docs_preprocessor 让文档中的快捷键成为生成物，从机制上禁止硬编码[^11^]；其三，**文档超出职责范围**，即文档手写复述了代码本可自证的事实性内容（what is/how works）。第三种的治理规则是：事实性内容一律生成或引用，手写散文只保留代码无法自证的三件事——意图（why）、决策理由（ADR）、入口（how to run，且写成可执行命令）。三问过滤器——"代码能自证吗？回答的是 why 吗？是入口吗？"——可统辖全部文档决策（展开见第 6 章）。

### 1.2 自动化收敛为五层栈

#### 1.2.1 五层架构与"只消费下层 SSOT"的健康判据

把环境、任务、钩子、CI、发布五个维度分别独立得出的架构拼在一起，出现一个跨语言通用的五层栈，且各维度对接口契约的描述逐字一致：上层不含业务逻辑，只是下层的调用方——"one runnable source of truth, many callers"[^12^]。

| 层 | 职责 | 载体示例 | 不该出现的东西 |
|---|---|---|---|
| ① 工具版本层 | 钉死一切工具与语言版本 | `flake.lock`、`rust-toolchain.toml`、`.python-version` | 任务逻辑、业务命令 |
| ② 任务层 | 自动化逻辑的唯一实现地 | `justfile`、`cargo xtask`、`uv run`、CMake preset | 版本号（须引用下层） |
| ③ 钩子层 | 提交前快速质量门 | `.pre-commit-config.yaml` | 测试矩阵、慢检查 |
| ④ CI 薄编排层 | 触发器、矩阵、权限、缓存 | GitHub Actions YAML（只调单行入口） | 多行 `run:` 业务逻辑 |
| ⑤ 发布层 | 制品产出与投递 | tag 触发 + 任务层 dist + OIDC Trusted Publishing | 长期凭据、手改 changelog |

这个栈的健康判据是拓扑性的：**每层只消费下层的 SSOT，不被上层重写**。CI 层只跑 `nix develop -c <cmd>`[^13^] 或 `just ci`[^14^]，发布层只调 `cargo xtask dist`[^15^]；于是"CI YAML 里出现多行 `run:`"不再是口味问题，而是可机械判定的层次污染——它意味着任务层存在一份未被消费的影子拷贝。推论同样实用：本地绿 ≡ CI 绿（矩阵维度除外），因为两边跑的是同一份任务定义。新增任何自动化时先问"它属于哪一层"，答案直接决定代码写在哪、被谁调用（各层落地见第 3、4、7 章）。

#### 1.2.2 "机器准备、人类点闸"：五处独立涌现的人机平衡点

自动化不等于零人环。同一平衡模式在五个互不相关的场景中独立涌现：发布工程的 Release PR（机器算版本、写 changelog，人点 merge[^16^][^17^][^18^]）；merge queue（机器做投机合并验证，人只负责入队[^19^]）；Zed 的 LLM 文档建议流水线（机器分析 diff 攒建议，批量后人工 PR 复核[^20^]）；Renovate 依赖仪表盘的审批制[^21^]；dsh/goose 的 agent 审批门（机器执行、人批准高风险动作）。共同结构是：**机器负责生成候选与验证，人负责不可逆决策的批准，且批准成本被压到一次点击**。这是一个可复用的设计模板：对任何自动化流程，先找出不可逆/高风险步骤保留人闸，再把人闸之前的一切准备（diff、影响分析、校验）全部自动化。它同时也是 agent 接入仓库的安全姿势：agent 可以跑到 PR 门口，merge 键在人手里（展开见第 8、9 章）。

### 1.3 高内聚低耦合与可测试性共生

#### 1.3.1 架构约束编译进工具链

四个维度的生态独立到达同一结论：**凡是靠人约定"不要这样做"的架构规则，成熟项目都找到了对应的机械强制点**。Zed 用 Dylint 把 GPUI 渲染线程纪律写成编译期 lint[^22^]；rust-analyzer 给每层标注 Architecture Invariant，用 façade crate 与 POD 边界类型固化分层[^23^]；LLVM 用"假想 Unix 链接器"判定分层合法性，Google 用 Bazel 的严格传递依赖检查批量抓 layering violation[^24^][^25^]；CMake 用 target 的 PUBLIC/PRIVATE/INTERFACE 属性把依赖图变成显式数据[^26^]；dsh 把"配置错误加载期 fail loud""包边界显式 resolve"写成铁律[^27^]。反方向的证据同样存在：文档侧的 context rot 研究表明，找不到强制点的规则会以可测量的速度腐化——约 23% 仓库的指令文件含腐烂引用[^28^]。由此得到评审纪律的重新分工：机器能管的全部交给机器，人类评审只处理机器管不了的部分。对每条架构约束问一句"它能被机器检查吗"——不能，就造一个强制点，或接受它将腐化。

#### 1.3.2 seam 天然是最干净的模块边界

可测试性与低耦合是同一枚硬币。Michael Feathers 的 seam（接缝，测试注入点）理论指出：把时间、随机、IO 全部当作依赖注入，注入点既是测试 seam 也是模块边界[^29^]。各维度提供了无意识但同构的实例：GPUI 的 TestAppContext 本质是注入的确定性调度器加平台输入 seam[^30^]；loom 要求并发原语在 `#[cfg(loom)]` 下可整体替换[^31^]；proptest 要求种子可注入[^32^]；dsh 的 record/replay 要求会话日志为唯一事实源、一切派生可重放[^33^]。逆命题也成立："legacy code is simply code without tests"——不可测的代码几乎必然藏有隐藏依赖（全局单例、硬编码的 `now()`/`rand()`）。这意味着**测试基建的投资同时就是架构解耦的投资，一份成本两份回报**：一条"禁止隐藏调用 `now()`/`rand()`/全局状态，一律构造参数注入"的开发纪律，同时交付确定性可复现的测试与显式的模块依赖，无需额外架构文档（展开见第 5 章）。

### 1.4 Agent 时代的仓库观

#### 1.4.1 仓库成为人机双读制品

AGENTS.md 已成为"给 agent 的 README"事实标准——60,000+ 仓库采用、由 Linux 基金会旗下 AAIF 作为创始项目托管[^34^][^35^][^36^]。但单看"AGENTS.md 怎么写"会错过更大的转变：仓库正在从"人类读物"变为**人机双读制品**，由三条原则重塑。其一，**规范要机器可读**：AGENTS.md、`.agents/skills/`[^37^]、公开 JSON Schema 的配置文件[^38^]、从 CLI 定义生成的文档[^4^]。其二，**验收要可执行**：AGENTS.md 里的命令 agent 会当真去跑，因此"完成定义"写成命令序列是最高杠杆的条目[^34^]；dsh 甚至把"能否被宿主加载"做成 CI 冒烟测试[^39^]。其三，**内容要增量**：ETH 的实证研究表明，复述仓库已有信息的 context file 平均反而降低 agent 成功率并增加 20% 以上成本，"代码库概览"章节无效[^40^]——价值等于增量信息密度，不是长度。

三条原则叠加的含义是：为人类写的文档与为 agent 写的指令**共享同一套 SSOT 与防漂移机制**。Agent 不是文档的新读者类型，而是文档工程纪律的放大器——违反 DRY 的代价过去以"维护负担"的形式缓慢显现，现在以 token 成本与失败率的形式立刻显性化。对信奉 SSOT 的开发者而言，这不是新要求，而是老纪律终于等来了即时反馈回路（展开见第 8 章）。

## 2. 仓库组织与架构边界

第 1 章的元原则——每条事实有唯一权威位置（SSOT）、架构约束尽量编译进工具链——在本章落到两个具体问题：代码放在哪（crate/包/库的粒度与布局），以及模块边界靠什么维持。标杆项目的共同答案是：粒度服务于编译并行与边界显式，边界靠机器强制而非评审自觉。

### 2.1 Monorepo 与 workspace 划分

#### 2.1.1 Zed 243 crate 单 workspace 与 rust-analyzer 扁平 crates/ 布局的启示：crate/包粒度决策依据

Zed 是一个单一 Cargo workspace 的 monorepo，根 `Cargo.toml` 实测列出 **243 个 member crate**，全部平铺在 `crates/` 目录下，命名即领域（`gpui`、`editor`、`language`、`collab`、`agent_ui`……），主二进制 `zed` 只是 `default-members` 之一[^41^]。粒度可以细到 `activity_indicator`、`clock` 这样的小组件也独立成 crate，换来的收益是编译并行、依赖必须在 manifest 中显式声明、测试天然隔离；核心数据结构 SumTree/Rope 单独成 crate 后被文本、诊断、git blame、文件树等 20 余处复用[^42^]。

rust-analyzer 则从方法论侧给出了操作规则。matklad 的经验是：对 1 万至 100 万行规模的项目，扁平布局优于树状布局——Cargo 的 crate 命名空间本身扁平，树状目录会引入第二套可能不一致的层级，而扁平结构"不需要维护"，加 crate、拆 crate 的成本为零[^43^]。配套规则包括：根目录用虚拟清单（virtual manifest）避免"主 crate 放根目录"这个例外；目录名与 crate 名严格一致以便导航与重命名；要发布、需严肃对待 semver 的 crate 单独放顶层 `libs/`，从而可以机械地检查"libs/ 不依赖 crates/"，内部 crate 则统一 `version = "0.0.0"` 免除 semver 负担[^43^]。

粒度决策的依据不是"多大算大"，而是三条可操作的判据。其一，**按架构层拆分**：rust-analyzer 显式区分"API 边界 crate"与"实现 crate"——底层 `syntax` 对 salsa 与 LSP 零感知，`hir` 是对外的 façade（外观）crate，公共 API 由带公有字段的 POD（Plain Old Data，纯数据）类型构成，且不面向当前唯一消费者设计[^23^]。其二，**按可独立复用的能力单元拆分**：`regex` 只是薄封装，解析在 `regex-syntax`、引擎在 `regex-automata`[^44^]。其三，**拆分必须带来机制收益**：拆出的模块若没有独立的编译单元、依赖声明或测试边界，拆分只是制造导航成本。

Python 侧的对应物是 uv workspace：根 `pyproject.toml` 声明 `[tool.uv.workspace] members = ["packages/*"]`，成员间依赖用 `[tool.uv.sources] foo = { workspace = true }` 声明且自动为 editable[^45^]。但官方文档明示了一个关键差异："Python does not provide dependency isolation"——workspace 不提供依赖隔离，成员可能 import 到其他成员声明的依赖[^45^]。即 Rust/C++ 由构建系统强制的边界，在 Python 侧必须另行工具化（如 import-linter 类检查），这正是 2.2 节的主题。

#### 2.1.2 依赖版本统一声明：workspace.dependencies / uv workspace / CMakePresets，消灭版本号多处维护

三语言各自收敛出了"版本号只写一次"的机制，但统一的粒度与锁定物不同。

Rust 自 Cargo 1.64 起支持依赖继承：根 `[workspace.dependencies]` 声明一次，成员以 `{ workspace = true }` 继承；`[workspace.package]` 与 `[workspace.lints]` 同理，把版本、edition、license、lint 政策全部单点化[^2^][^15^]。继承有两条边界：成员只能追加 `optional` 与 `features`，不能覆盖 `version`；且 Cargo 至今没有内建 lint 检查"应继承而未继承"，需用 cargo-autoinherit 等工具把重复声明收敛回 workspace[^46^][^47^]。

Python 侧，uv workspace 的根 `tool.uv.sources` 默认对所有成员生效、可被成员覆盖；锁定物 `uv.lock` 的 universal lockfile 语义详见 3.2.2 节[^45^][^48^]。

C++ 没有统一的官方机制，实践是分层的：`vcpkg.json` 声明依赖、`vcpkg-configuration.json` 用 baseline 锁定整个 registry 的 commit[^49^][^50^]；CPM.cmake 在 `CPMAddPackage` 中钉 git tag/commit 加 `URL_HASH` 完整性校验[^51^]；而 `CMakePresets.json` 承担的是另一维度的 SSOT——把 configure/build/test/workflow 配置全部收敛到一个检入仓库的文件，用 hidden 基座 preset 加 `inherits` 消除重复，`condition` 字段按 `${hostSystemName}` 安置平台差异，README 与 CI 只剩一句 `--preset`[^52^]。

| 生态 | 统一机制 | SSOT 文件 | 统一的对象 | 锁定物 | 已知短板 |
|---|---|---|---|---|---|
| Rust | `[workspace.dependencies]` 继承（Cargo ≥1.64） | 根 `Cargo.toml` | 第三方版本、内部 crate 路径、lint 与 package 元数据 | `Cargo.lock` | 无内建"未继承"检查，需 cargo-autoinherit 补齐 |
| Python | uv workspace + `tool.uv.sources` | 根 `pyproject.toml` | 成员间 editable 依赖、统一 source 覆盖 | `uv.lock`（跨平台单文件） | 不提供依赖隔离，边界需另立工具 |
| C++ | vcpkg baseline / CPM `GIT_TAG`+`URL_HASH` | `vcpkg-configuration.json` / `CMakeLists.txt` | registry commit 或源码版本 | baseline commit / URL 哈希 | 无官方统一机制，方案需按规模自选 |
| C++（配置维度） | `CMakePresets.json`（hidden preset + inherits） | 根 `CMakePresets.json` | 编译器、cache 变量、平台条件 | 文件本身检入 git | 只管配置不管依赖版本 |

**解读**：四个机制共享同一拓扑——声明集中在一处、消费方只允许"引用"形态、解析结果由锁定文件钉死；差异在于统一的完备性。Rust 把依赖、元数据、lint 三维全部纳入继承体系，是三语言中最彻底的；uv 锁定了版本但放弃了隔离，意味着"版本 SSOT"与"边界强制"在 Python 生态是两个独立问题；C++ 的分层现状要求用户自己做出裁决：依赖版本 SSOT（vcpkg baseline 或 CPM）与配置 SSOT（CMakePresets）各选其一并写成团队规则，绝不同时维护两份同构清单。对混合语言仓库，落地的检查项是：任何一个第三方依赖的版本号，在全仓 `grep` 只应命中一个权威位置（manifest），其余命中只能是 lockfile 这类生成物。

### 2.2 模块边界与低耦合的工程强制

#### 2.2.1 LLVM 库分层、IWYU、严格传递依赖、Dylint 自定义 lint：边界靠工具而非自觉

"高内聚低耦合"若停留在 code review 清单上，必然随时间腐化；标杆项目的做法是给每条架构规则找到机械强制点。

**依赖图层的强制**。LLVM Coding Standards 的 "Library Layering" 是经典文本："A directory of header files defines a library. One library should only use things from the libraries listed in its dependencies"——依赖关系是显式声明的数据（历史上载于 `LLVMBuild.txt`，现由 CMake target 依赖承接），不是隐式事实[^24^][^53^]。它还给出一个可操作的自测法："假想 Unix 链接器"判据——把所有 inline 函数改为 out-of-line 后，链接器按任意依赖顺序都应链接成功，循环依赖因此结构上不可能存在[^24^]。即便如此，LLVM 社区自己承认 LLVMCore 会意外拖入 BinaryFormat、Remarks 等库；Google 靠 Bazel 的严格传递依赖检查（target 引用未直接依赖的符号即报错）批量发现并 revert 掉 layering violation，MLIR 侧则用"最小可链接"示例二进制把组件的预期依赖集钉成测试[^25^][^54^]。

**源码层的强制**。include 依赖是 C++ 耦合的物理载体。LLVM 的现行方向是把模糊的头文件政策改为两条可工具化规则：头文件自足（self-contained，可单独编译）与 IWYU（Include-What-You-Use）洁净——只用到的符号才 include，删除的符号同步删 include；执行工具是 clang-tidy 的 `misc-include-cleaner`，以 compile_commands.json 为输入进入 presubmit[^24^][^55^]。

**语义层的强制**。构建系统与 include 检查管不到的架构纪律，要写成自定义 lint。Zed 的 `tooling/lints` 是基于 Dylint 的自研 lint 库（`blocking_io_on_foreground`、`entity_update_in_render` 等），把"GPUI 渲染线程上不得做阻塞 IO"这类项目特有纪律固化为编译期检查，通过 `[workspace.metadata.dylint]` 接入[^41^][^22^]。rust-analyzer 则给每个架构层显式标注 "Architecture Invariant"（如"在函数体内键入永不使全局派生数据失效"），并用 façade crate 加 POD 边界类型把不变式落实为类型系统约束[^23^]。这些机制的共同模式是：凡"人约定不要这样做"的规则，成熟项目都找到了 lint、链接器、类型系统或依赖检查中的强制点；找不到强制点的规则，应视为注定腐化。

#### 2.2.2 内部 API 边界纪律：thiserror/anyhow 按调用者意图分工、semver-checks 防 API 意外破坏

模块边界不仅是依赖方向，还包括穿越边界的错误类型与 API 演化纪律。

错误类型的分工，生态通行口诀是"库用 thiserror、应用用 anyhow"，但 Luca Palmieri 给出了更精确的判据——按**调用者意图**而非"库/应用"身份一刀切："Do you expect the caller to behave differently based on the failure mode? Use an error enumeration... Do you expect the caller to just give up and report? Use an opaque error."[^56^][^57^] 配套纪律包括：错误类型按其抽象层说话（HTTP 层的调用者只需区分 400 与 500，不应暴露 `sqlx::Error` 的细节）；错误服务控制流（机器消费枚举变体）与报告（人消费上下文）两个目的；向上传播的函数不 log，只在处理点统一记录[^56^]。这套纪律的直接收益是边界两侧的解耦：内部实现换依赖（换数据库驱动）不波及调用者的错误匹配逻辑。

API 演化的纪律则靠机械兜底。cargo-semver-checks 基于 rustdoc JSON 提取公共 API，用 Trustfall 查询引擎把当前版本与 crates.io 基线比对，把 semver 违规变成可查询 lint——仅 2025 年一年就新增 122 条（覆盖泛型参数变化、`&self`→`&mut self`、repr、sealed trait 等场景），且已并入 cargo 官方路线图[^58^]。它与 2.1.1 的 `libs/`/`crates/` 隔离配合使用：对外发布的 crate 进 `libs/` 并在 CI 跑 semver-checks，内部 crate 保持 `0.0.0` 免于 semver 负担[^43^][^58^]。C++ 侧的对应纪律是 abseil 的 API/ABI 双轨声明及其配套约束，详见 7.3.1 节[^59^]。两种语言共同的落地形态：把"API 是契约"变成 CI 里一条会失败的检查。

### 2.3 Agent harness 的架构启发

#### 2.3.1 DeepSeek Harness"一切皆插件"与 append-only 会话日志 SSOT、"模型可见⟺已落日志"运行时不变量

DeepSeek Harness（dsh，DeepSeek 2026 年 8 月开源的 agent 运行时，TypeScript，基于 Cordis 插件框架）把插件化推到极限："every part of the product is a plugin, including the model adapter, the tool registry, the session log, and the agent loop itself... There is no privileged core to patch"——连 agent loop 都只是配置文件中的一行，可被替换[^33^][^60^]。支撑这种激进可替换性的是两条机制纪律：其一，**注册即 effect**——一切注册走 `ctx.effect()` 并返回 disposer，卸载自动清理，且每个 registry 必须有 HMR-safety 测试（dispose 后断言清理干净）[^27^][^61^]；其二，**能力接缝三角色**——一个可替换能力必须同时设计接口（Service Definition）、实现（Provider）与消费方（Consumer），"只做一个角色不构成 seam"[^33^]。

更值得普通项目借鉴的是其状态架构。dsh 的会话状态是 append-only 的 `SessionEvent` 日志（JSONL/SQLite 后端），架构文档明言 "The session log is the source of the context the model sees"——`deriveMessages()` 从日志投影出模型历史，fork、resume、transcript、telemetry 全部从同一事件流派生[^33^]。其铁律是运行时不变量 **"Model-visible means logged"**：任何进入模型请求的内容必须能从日志重建，并有运行时断言强制[^33^]。这是 SSOT 原则从静态文件域扩展到运行时状态域的范例：单一事实来源（append-only 日志）+ 派生物（消息历史、fork、回放）+ 校验器（运行时断言），三要素齐备。任何需要审计、回放、断点续传的系统（不只是 agent）都可套用。dsh 的另两条边界纪律同样可移植："Misconfiguration fails loud"——配置错误在加载期即报错，绝不静默跳过；"Explicit > implicit at package boundaries"——跨包默认值必须是显式 `resolve(request)` 步骤，禁止在 `run()` 里藏 `?? default`[^27^]。

#### 2.3.2 开放协议化解 N×M 集成：ACP 类比 LSP、WIT 版本快照管理 ABI——协议优先于实现

当模块边界跨越组织或进程时，标杆项目的选择是把集成问题抽象为开放协议。Zed 主导的 ACP（Agent Client Protocol，Apache 2.0）定位类比 LSP：编辑器以子进程拉起 agent，JSON-RPC 2.0 over stdio、换行分隔、stdout 只准协议消息、stderr 走日志[^62^]。生态策略是"协议先行、自家产品只是第一个 client"：先接入 Gemini CLI，再自研桥接包装 Claude Code 证明通用性，随后 JetBrains 官宣共建，并上线 ACP Registry——"implement once, work everywhere"[^63^][^64^]。N 个编辑器与 M 个 agent 的 N×M 集成由此降为 N+M。

二进制级边界的版本治理体现在 Zed 扩展体系：扩展编译到 `wasm32-wasip2` 沙箱执行，宿主与扩展间的 ABI 用 WIT（WebAssembly Interface Types）按版本快照管理——`wit/` 下 `since_v0.0.1/` 至 `since_v0.8.0/` 每版一份完整接口快照，`PENDING_CHANGES.md` 跟踪未发布变更，宿主机因此能为不同时代的扩展选择对应 ABI[^65^]。

这两条对非 agent 项目同样成立。其一，凡是预期有多个实现方/消费方的内部接口（插件点、扩展 ABI、跨进程调用），先定协议或 schema 再写实现：配置文件公开 JSON Schema 换 IDE 补全与 CI 可校验性，接口用版本快照目录管理演化，都是"协议优先于实现"的低成本形态[^38^][^66^]。其二，版本快照目录法（`since_vX.Y.Z/`）可直接移植到任何需要长期兼容的接口治理——每版一份完整快照、变更先入 pending 文件，兼容性从"靠记忆承诺"变为"靠目录结构可审计"。

## 3. 可复现环境与依赖治理

第 2 章划定了代码的存放位置与架构边界，本章回答下一个问题：这些代码在什么环境中被构建、依赖从何处来。可复现性的第一性原理是：**所有外部输入都被 hash/commit/lock 钉住，且锁定文件全部入库**。对"Rust + C++ + Python、跨三平台、flake + direnv"的场景，本章给出一套已被 Helix、Hyprland 等标杆项目验证的拓扑：flake.lock 锁工具链与系统依赖，Cargo.lock/uv.lock 锁语言依赖，rust-toolchain.toml/.python-version 锁语言版本——每条事实恰有一个权威位置，其余位置只引用、不重述。

### 3.1 flake + direnv 标准模式

#### 3.1.1 flake.nix + nix-direnv `use flake`：flake.lock 入库即环境 SSOT

标准形态只有三个文件：仓库根的 `flake.nix` 声明 inputs（nixpkgs、rust-overlay 等）与 `devShells`；`.envrc` 一行 `use flake`；首次 `nix develop` 生成的 `flake.lock` 把全部 input 钉到具体 commit 与 hash。**flake.lock 必须提交 git**——它是整个环境可复现性的载体，Libation 项目的文档表述得很直接："Keep your `flake.lock` file committed to ensure builds are reproducible for all collaborators."[^67^] 升级纪律对应地也只有一条：`nix flake update`（或 `nix flake lock --update-input nixpkgs` 单 input 升级），review diff 后提交；可用 `DeterminateSystems/update-flake-lock` Action 定期自动提 PR[^68^]。

`.envrc` 一行背后应使用 nix-direnv 而非 direnv 自带的 `use flake`，理由是一手 README 列出的三个机制差异[^69^]：缓存 nix-shell 求值结果（首次之后进入目录不再重新求值 nixpkgs）；把 shell derivation 链接进用户 gcroots 防止垃圾回收（离线可用）；自动 watch `flake.nix`/`flake.lock`，文件变更即重载。相比 lorri 方案它无需外部 daemon[^69^]。注意 macOS 自带 bash 3.2 过旧，direnv 需经 Nix 或 Homebrew 安装[^69^]。

最小可落地骨架（多系统遍历用 flake-utils 的 `eachDefaultSystem`）：

```nix
{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";  # 消除 lock 中重复的 nixpkgs
    };
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ rustToolchain uv cmake ninja pkg-config openssl just ];
        };
      });
}
```

```bash
# .envrc —— 全部内容就这一行
use flake
```

`inputs.nixpkgs.follows = "nixpkgs"` 这一行值得单独强调：它强制 overlay 跟随主 nixpkgs，避免 flake.lock 里出现多个 nixpkgs 副本导致版本分裂，Hyprland 的 flake 大量使用此模式[^70^]。devShell 的组织另有三条社区惯例：多语言 monorepo 可提供 `devShells.rust`/`devShells.python` 等并按 `use flake .#rust` 选择，单一默认 shell 通常已够用；`shellHook` 只做导出环境变量、打印提示等轻量事，慢速初始化会让每次进入目录都付出可感知的等待；darwin 分支按需追加 apple_sdk framework 依赖[^69^][^70^]。

#### 3.1.2 职责边界：flake 管工具链/系统依赖，cargo/uv 管语言依赖；开发期不用 2nix 工具避免双重描述

这套模式不引入重复的关键，是一条严格的职责边界：flake 只负责"工具链 + 系统依赖 + 非语言生态工具"（rustc/cargo、uv 本身、gcc/clang/cmake/ninja、openssl/zlib/pkg-config、LLDB、protoc）；语言内包依赖（crates、PyPI wheels）完全交给 cargo 与 uv，SSOT 分别是 Cargo.lock 与 uv.lock。pydevtools 手册对 uv 的表述同样适用于 cargo："Reach for uv2nix when you need bit-identical builds across machines or when you're packaging… Skip it for day-to-day development; the patterns above let uv's lockfile stay the source of truth."[^71^]

推论是：**开发期不要在 flake 里使用 crane/naersk/cargo2nix/uv2nix 等 2nix 工具重述语言依赖**。NixOS Wiki 把 8 种 Rust 打包方案全部列在"Packaging Rust projects with nix"一节而非开发环境一节[^72^]；devenv 也承认开发者"不想比较 crate2nix vs cargo2nix vs naersk vs crane"，2nix 的正当定位是发布产物（`packages.default`）的打包层[^73^]。uv2nix 作者 2025 年 1 月宣布去掉 experimental 标签后能力已完备（纯 Nix 求值、逐包构建、editable 支持、交叉编译）[^74^]，但它消费的是 uv.lock 做翻译而非另写清单——触发条件只有一个：你想 `nix build` 出 bit 级复现的发布产物。深浅集成不矛盾，是"开发期浅、发布产物深"的场景分层。

另一个反重复机制是 `inputsFrom`：若 flake 已有打包 derivation，devShell 通过 `inputsFrom` 继承其构建依赖而非复制 buildInputs 列表——Helix 与 Hyprland 均如此，构建依赖只描述一次[^6^][^70^]。混用两套体系的风险有实证警示：naersk 的 README 明确声明 "Naersk by default ignores the rust-toolchain file"[^75^]——2nix 工具与 rustup 工具链文件本是两套世界，开发期混用等于引入第二个版本来源。

### 3.2 工具链版本单一来源

#### 3.2.1 Rust：rust-toolchain.toml 为 SSOT，flake 用 rust-bin.fromRustupToolchainFile 直接消费（Helix 实证）

Rust 生态的版本 SSOT 是仓库根的 `rust-toolchain.toml`：

```toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy", "rust-analyzer"]
```

要点是 pin 具体版本并预声明 components 与 targets；不要写浮动的 `"stable"`——"stable floats to the latest stable release—fine for personal projects, surprising in CI when a new release changes a lint or codegen behavior"[^76^]。Zed 仓库即用此文件固定 `channel = "1.97.1"` 并预声明交叉编译 targets[^77^]。

这份文件有两个消费方：rustup 用户进目录自动按它解析工具链；Nix 用户在 flake 里写 `pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml`（rust-overlay 提供），版本号零重复。Helix 编辑器的生产 flake.nix 正是此用法[^6^]。这也是"一份 SSOT 文件被多条环境路线消费"的范式：同一份文件同时服务 rustup 用户、Nix 用户与 CI 原生 Windows job。overlay 选型上社区 2025 年共识倾向 rust-overlay——devenv 因维护质量从 fenix 切换到 rust-overlay[^73^]。

#### 3.2.2 Python：.python-version + requires-python 为 SSOT，flake 只提供 uv

Python 侧的对偶结构是 `.python-version` + pyproject.toml 的 `requires-python`。uv 官方文档明确了优先级：`uv python pin 3.12` 写 `.python-version`，uv 在工作目录及各级父目录搜索该文件；`requires-python` 声明项目支持的版本区间；两者冲突时 uv 直接报错[^78^][^79^]。语言依赖的 SSOT 则是 uv.lock——它是 universal lockfile，单文件跨操作系统、架构与 Python 版本解析，必须提交版本库且绝不手改[^80^]。

与 flake 的统一策略推荐**浅集成**：flake 只提供 `pkgs.uv`（NixOS 上另配 nix-ld 环境），解释器版本完全由 `.python-version` 决定，uv 自动下载对应 CPython，flake 中不出现任何 Python 版本号[^71^]。一个必须规避的坑：flake 中应设 `NIX_LD`/`NIX_LD_LIBRARY_PATH` 而非 `LD_LIBRARY_PATH`——后者在非 NixOS Linux 上会迫使宿主动态链接器加载 Nix 的 glibc 导致 Python 崩溃[^71^]。若确需 flake 提供 nixpkgs 解释器（中集成），可用 `builtins.readFile ./.python-version` 解析后映射到对应 `pkgs.python3xx` 消除版本号重复[^81^]。

#### 3.2.3 跨平台现实：Nix 无原生 Windows 支持，Windows 走 WSL2 或语言原生工具链

必须如实告知团队的事实：Nix 没有原生 Windows 支持，"Running Nix directly on Windows without WSL is not practical for real development work"[^82^]。现实路径是 WSL2（WSL1 的进程隔离不完整，Nix 构建会失败）内装 Nix，项目文件放 Linux 文件系统（`~/projects`，勿用 `/mnt/c`，跨文件系统 IO 极慢），编辑器用 VS Code Remote-WSL[^82^][^83^]。devcontainer 不是替代答案——它在 Windows 上同样只支持 Linux 容器[^84^]。

但这不构成 SSOT 的破坏，恰是本模式设计的精巧处：原生 Windows 验证交给 CI 的 windows-latest job，走 rustup + uv 原生路线，消费的仍是同一份 rust-toolchain.toml 与 .python-version——版本一致性由文件共享保证，而非环境描述复制[^82^]。若团队有"不愿碰 Nix 且必须原生 Windows"的成员，mise 是少数原生支持 Windows 的降级通道，它与 Nix 是互补而非竞争关系[^85^]。

### 3.3 C++ 依赖方案选型

#### 3.3.1 nixpkgs 首选 / vcpkg manifest 原生 Windows 备选 / vendored+pinned 是大团队特权：库依赖 SSOT 只选一个

C++ 没有官方包管理器，也没有 rust-toolchain.toml 的等价物——编译器与标准库版本由 nixpkgs revision 承载（即 flake.lock），需要特定编译器时在 mkShell 中指定 `stdenv = pkgs.gcc13Stdenv`，或像 Hyprland 那样 `mkShell.override { inherit (pkg) stdenv; }`[^70^]。库依赖则有三条哲学路线，对照如下：

| 方案 | 依赖清单 SSOT | 版本锁定机制 | 原生 Windows | 与 flake 路线的关系 | 适用场景 |
|---|---|---|---|---|---|
| nixpkgs（flake buildInputs） | flake.nix | flake.lock（nixpkgs revision） | 无（走 WSL2） | 原生一体，dev/CI 零差异 | Nix 为主力环境的全员 |
| vcpkg manifest | vcpkg.json + vcpkg-configuration.json | baseline 锁定 registry commit + overrides | 一等公民（MS 官方） | flake 退为仅供编译器/工具 | 必须原生 Windows 构建 |
| Conan 2 | conanfile.py/.txt + profile | `conan lock` lockfile | 一等公民 | 同上；可生成 CMakePresets | 跨编译配置矩阵复杂的项目 |
| CPM.cmake | CMakeLists.txt 内 CPMAddPackage | git tag/commit + URL_HASH | 依赖 CMake 本身 | 与 flake 并存但职责重叠 | 小项目、零外部工具诉求 |
| vendored + pinned（LLVM/Chromium 式） | third_party/ + DEPS 类清单 | 精确 commit hash 检出 | 全平台 | 与 flake 并存 | 超大型项目、需私有补丁 |

这张表的读法比各行内容更重要。**nixpkgs 是 C/C++ 事实上的最大包仓库**，"Nix is to C++ what uv/poetry is to Python"——依赖以 `buildInputs = [ fmt spdlog openssl … ]` 进入 mkShell，cmake 经 find_package/pkg-config 发现它们，dev 与 CI 消费同一份 flake.lock，可复现性最强[^86^]。因此 Nix 为主力环境时首选 nixpkgs；只有需要服务原生 Windows 非 Nix 用户时，才把库依赖清单迁往 vcpkg.json（微软官方推荐 manifest mode，"For most users, we recommend manifest mode"，版本钉定靠 vcpkg-configuration.json 的 baseline commit）[^49^][^50^] 或 conanfile。**铁律是两者取一作为库依赖 SSOT，绝不同时维护 nix 表达式和 conanfile 两份同构清单**——除非受众明确分裂且接受同步成本。表中未展开的两个选项也值得一句定位：Conan 2 的一个生态收敛信号是其输出物选择了 CMake 官方格式——`CMakeToolchain` 直接生成 CMakePresets.json，包管理层与配置层由此共享同一 SSOT[^87^]；CPM.cmake 与 FetchContent 属"源码级获取"路线，单脚本检入、CI 友好，但全源码构建且无版本求解，适合依赖少而简单的小项目[^51^]。至于 vendored+pinned：LLVM 刻意保持零外部 C++ 库依赖、第三方代码 vendor 进 third-party/[^88^][^89^]，Chromium 用 DEPS 按精确 commit 检出并靠自动化 roller 摊薄 deps rolls 成本[^90^]——"规模即能力"，这是大团队特权，中小项目应把同样的钉版本思想降级为 lockfile/baseline/URL_HASH。

### 3.4 CI 与环境一致

#### 3.4.1 nix-installer-action + magic-nix-cache + `nix develop -c`：CI 零环境重复定义

环境治理的最后一环是让 CI 不描述环境。GitHub Actions 标准三段式[^13^]：

```yaml
permissions: { id-token: write, contents: read }
steps:
  - uses: actions/checkout@v4
  - uses: DeterminateSystems/nix-installer-action@main
  - uses: DeterminateSystems/magic-nix-cache-action@main
  - run: nix develop -c just ci    # 或 nix flake check
```

（示例沿用两个 action 官方 README 的 `@main` 写法；生产仓库应按 4.3.2 节口径把全部 action 钉到完整 commit SHA。）`nix-installer-action` 安装 Nix；`magic-nix-cache-action` 用 GitHub Actions cache 做零配置二进制缓存，官方口径可节省 30–50% CI 时间，但注意它只在 CI 内可用，且 2025 年复活版基于对 GitHub 缓存 API 的逆向、"the API could change at any time"，认真使用应选 FlakeHub Cache 或 Cachix[^13^][^91^][^92^]。对照组是"setup-rust + setup-python + apt install"式 yaml——那是三份独立演化、必然漂移的环境描述；而 `nix develop -c` 之后，工具链版本全部来自入库的 flake.lock，CI 与本地消费同一 SSOT，"本地绿 ≡ CI 绿"成立。Helix 与 Hyprland 均以 `nix flake check` 驱动 CI 检查，把测试做成 flake 的 `checks` 输出，本地与 CI 跑同一命令[^6^][^70^]。配套卫生措施：`flake-checker-action` 体检 flake.lock 中 nixpkgs 是否过旧，`update-flake-lock` 定期自动提升级 PR[^68^]。跨平台矩阵上，`nix develop` 在 ubuntu 与 macos runner 直接可用；windows-latest 一格走 rustup + uv 原生路线，消费 3.2 节的同一份 SSOT 文件，不为 Windows 维护平行环境描述[^82^]。

至此可以给出本章的卫生纪律收口：**工具链升级 = 改 SSOT 文件一处**（rust-toolchain.toml / .python-version / `nix flake update`），跑全套检查再合并；禁止在 README、CI yaml、Dockerfile 中手写任何工具链版本号——需要版本信息的位置一律改为消费上述文件（如 CI 原生 job 由 rustup/uv 自动读取，容器构建 `COPY .python-version` 后 `uv sync --frozen`）[^71^][^80^]。这条纪律把"环境可复现"从一次性配置变成可持续维护的拓扑：每条事实一个家，其余位置只有引用。

## 4. 自动化任务层

第 3 章解决了"环境里有什么工具"，本章解决"任务逻辑写在哪"。对厌恶重复的维护者而言，这是全手册杠杆最大的一层决策：同一段构建、检查、发布逻辑被实现几次，漂移源就有几个。成熟开源项目给出的答案高度收敛——**设立一个"任务层"作为所有可重复逻辑的唯一实现地，本地开发者、git 钩子、CI、文档都退化为它的调用方**；CI YAML 收缩为只含触发器、矩阵、权限、缓存的薄编排层。[^12^]

### 4.1 任务层即唯一实现地

#### 4.1.1 "本地/钩子/CI/文档都是调用方"：CI 只调用任务入口，不内联逻辑

这一原则最精炼的表述来自 Galaxy 项目的工程博客，题为"One runnable source of truth, many callers"：

> 把每个工作流的逻辑放进一个可运行脚本，让每个消费方——CI、git 钩子、任务运行器、AI agent 的 skill——都来调用它。包清单与检查项只存在于一个地方；新增或重命名包时没有任何东西会悄悄失步，因为除那一个脚本外根本没有需要保持同步的东西。[^12^]

Galaxy 的同一个质量门脚本被四处调用：GitHub Actions（PR 触发 + Python 3.10–3.13 矩阵）、git pre-push 钩子、`make qa-gate` 目标、AI agent 的 pre-PR skill。[^12^] 同构的实例在 Rust 生态早已存在：rust-analyzer 的发布流程是本地执行 `cargo xtask release` 推送标签，标签触发的 GitHub Actions 只做一件事——调用 `cargo xtask dist` 打包二进制与 VS Code 扩展并发布，CI 中没有任何内联的打包逻辑。[^93^] 也就是说，"本地能复现 CI 的每一次失败"不是靠纪律维持的口号，而是架构的直接推论：CI 执行的命令与本地逐字相同，差异只剩矩阵维度（操作系统、工具链版本）这类声明式变量。

#### 4.1.2 反模式：Makefile+shell+CI yaml 三处重复逻辑、文档命令与脚本不一致

违反该原则的代价有真实案例可查。databeak 项目的复盘 issue #125 记录了一个典型事故：Ruff 与 MyPy 的检查在 `.pre-commit-config.yaml` 和 CI workflow 中被重复定义，两处参数逐渐漂移，团队不得不专门立项去重。[^94^] 这揭示了三处重复（Makefile 一份、shell 脚本一份、CI YAML 一份）的必然结局：改一处忘改另一处，本地过而 CI 挂。第二类反模式是文档与脚本分离——README 里贴的命令与脚本实际行为不一致，新用户照文档操作即失败。dotCMS 把 justfile 定位为"活文档"正是针对这一点：justfile 随分支同步演化，"命令自动与代码库保持同步"[^95^]；rust-analyzer 更彻底，用代码生成（codegen）从语法定义直接产出文档，手工同步的环节被整个删除。[^96^] 第三类反模式是在 workflow 的 `run:` 字段里写多行业务脚本：逻辑无法本地复现，调试循环退化为"push—等 CI—看日志—再 push"。[^12^]

### 4.2 实现选型

任务层的载体选择是本章唯一的实质分歧点（调研中标记为冲突区 C1）：Rust 生态倾向 xtask，多语言仓库倾向 just。二者不是互斥关系，而是按逻辑复杂度分层。

#### 4.2.1 xtask 模式（matklad 2018）：用宿主语言写自动化

xtask 模式由 rust-analyzer 作者 Aleksey Kladov（matklad）在 2018 年的博客《Make your own make》中提出，动机是对 shell 与 make 的双重不满："shell 脚本本质上是平台相关的；make 也一样，带着它那些恼人地相似的众多方言。"[^97^] 方案本身不引入任何新工具——它只是"一种特定的 cargo 项目配置"：在 workspace 中加一个 `xtask` 成员，再在 `.cargo/config.toml` 注册别名：

```toml
[alias]
xtask = "run --package xtask --"
```

此后 `cargo xtask <task>` 即可运行。cargo-xtask 规范仓库自述其两个决定性特征：除 `cargo` 与 `rustc` 外不需要任何其他二进制，完全自举；不经过 shell，因此比 bash 更容易做到跨平台。[^98^] 需要说明的是其措辞克制："cargo-xtask 并非官方推荐的工作流，但它是生态中相当常见的模式——Cargo 自己就在用 xtask。"[^98^] 官方列出的采用者还包括 rust-analyzer（发布、性能指标）、helix-editor（校验内嵌查询文件、生成文档）、containers/bootc（生成 RPM）等。[^98^]

用宿主语言写自动化的收益是复合的：任务代码享受编译器类型检查、IDE 补全与重构；可以写单元测试——rust-analyzer 直接用 `cargo test -p xtask` 测试其代码生成逻辑[^99^]；团队不必维护 bash/make 方言这第二种语言的专业知识。[^97^] 局限同样明确：xtask 不接入 Cargo 生命周期（不能拦截原生 `cargo build`），且是项目本地的、无法从依赖中复用，通用逻辑需另发为 crates.io 库。[^98^]

#### 4.2.2 just 作为语言无关总入口：多语言混合仓的门面

just 是 Casey Rodarmor 用 Rust 编写的单二进制命令运行器，官方定位清晰："just 是命令运行器，不是构建系统，因此避开了 make 的大部分复杂性与怪癖——不需要 `.PHONY`。"[^100^] 与 make 的本质差异在于：make 追踪文件时间戳做增量重建，just 不追踪任何文件、每次原样执行 recipe——用可预测性换掉了隐式行为。[^101^] 它对混合语言仓库的价值有三：`just --list` 自带出全部 recipe 的活文档；recipe 可用任意 shebang 语言编写；支持 modules/imports 拆分多文件。[^100^] Baserow 的多语言 monorepo 是标杆用法：根、backend（Python/Django + uv）、frontend（Node/Nuxt）三个 justfile 分层组织，CI 与本地共用 `just lint` / `just test` 入口。[^102^]

| 维度 | cargo xtask | just |
|---|---|---|
| 定位 | 项目内自动化的完整编程环境 | 语言无关的命令路由与编排门面 |
| 载体语言 | Rust（宿主语言，类型安全） | justfile DSL + 任意 shebang 语言 |
| 额外依赖 | 零（仅需 cargo + rustc，自举）[^98^] | 需安装 just 单二进制[^101^] |
| Windows 支持 | 天然跨平台，不经 shell[^98^] | 需 `set shell := ["powershell.exe", "-c"]` 或 shebang recipe[^103^] |
| 表达力 | 完整语言：条件、循环、解析、代码生成 | 命令聚合为主；复杂逻辑需下沉 |
| 自文档 | 依赖 clap 帮助文本 | `just --list` 内建[^100^] |
| 复用性 | 项目本地，通用逻辑需发库[^98^] | 可 import/module 拆分复用[^100^] |
| 典型采用者 | rust-analyzer、Cargo、helix、Zed[^98^] | Baserow、dotCMS、Codex[^102^][^95^] |

对比可见两者的能力曲线在"逻辑复杂度"轴上互补：xtask 在需要条件分支、文件解析、代码生成时优势明显，但为一行命令写一个 Rust 子命令属于过度工程；just 把"谁该被调用"表达得极好，却无法体面地承载重逻辑，且 Windows 下默认 `sh -c` 会失败，必须显式换 shell 或改用 shebang recipe。[^103^] 因此调研给出的裁决原则是分层而非二选一：**justfile 做全仓统一入口（对 Rust/C++/Python 混合仓而言它是唯一语言无关的门面），recipe 体内只调 `cargo xtask …`、`uv run …`、`cmake --preset …`；能用 just 一行表达的别上 xtask，需要条件逻辑或生成能力的别塞进 justfile**。最小落地形态如下：

```just
set shell := ["powershell.exe", "-c"]  # 跨平台仓按需启用

lint:
    cargo xtask lint
    uv run ruff check .

ci: lint
    cargo nextest run --workspace
    uv run pytest
```

recipe 体不含任何 bashism 与参数重复——参数的唯一存放地是下层工具自己的配置文件。

### 4.3 钩子层与 CI 的一致性

#### 4.3.1 .pre-commit-config.yaml 单一来源，CI 跑 `prek run --all-files`

pre-commit 框架的官方定位是"多语言 pre-commit 钩子的包管理器"：单一 `.pre-commit-config.yaml` 声明钩子仓库、版本（`rev`）与钩子 id，框架负责下载、隔离安装、按 git 阶段运行。[^104^] 其关键设计是把同一份配置同时用作 CI 工具：官方文档明确建议"添加 `pre-commit run --all-files` 作为 CI 步骤"，本地 commit 时跑增量（staged files），CI 跑全量——**CI 不重写检查逻辑，只是换文件范围**。[^104^]

执行器可选用 prek：需核实归属的是，prek **不是 Astral 的产品**，而是 j178（Jo）的个人项目，历经 pre-commit-rs → prefligit → prek 两次改名，项目自述"深受原版 pre-commit 与 Astral 的 uv 启发"但与 Astral 无隶属关系。[^105^][^106^] 它是无依赖单二进制，与 pre-commit 的配置和钩子完全兼容，速度优势来自钩子环境跨仓库共享、并行安装与执行、用 uv 建虚拟环境、常见钩子的 Rust 内置实现。[^105^] ruff 仓库的 CI 即采用 `uvx prek run --all-files --show-diff-on-failure`，并按 `.pre-commit-config.yaml` 的哈希缓存 `~/.cache/prek`。[^14^]

这一节必须直面"双 pin 漂移"质疑（冲突区 C8）：钩子配置里的 `rev` 与 mise/uv 管理的工具版本是两份 pin，会各自漂移；且本地钩子常用 `--fix` 写模式而 CI 应只读。[^94^] 收敛解法是三点：其一，CI 以只读方式跑同一份配置；其二，工具版本尽量只 pin 一次——钩子用 `repo: local` + `language: system`（或 `unsupported`）直接调 PATH 中由 mise/flake 提供并锁版本的二进制，而非让 pre-commit 再装一份；其三，测试矩阵与多版本检查不属于钩子层，留在 CI 专有 job。[^94^] 最小配置示例：

```yaml
repos:
  - repo: local
    hooks:
      - id: lint
        name: just lint
        entry: just lint
        language: unsupported   # 不再装第二份工具，版本由 mise/flake 管
        pass_filenames: false
```

#### 4.3.2 GitHub Actions 薄编排：reusable workflows、matrix、merge queue、action 按 SHA 钉死

薄编排层允许保留的只有四样：触发器、矩阵、权限、缓存。超出这个边界的复用诉求交给两个官方机制：reusable workflow 是完整 YAML（`workflow_call` 触发），在 **job 级**用 `uses:` 调用，可含多个 job、可传递 secrets、每步独立日志；composite action 是步骤包，在 **step 级**调用，不能含 job、不能用 secrets。[^107^] 经验划分是：封装"重复步骤"（装工具链 + 缓存）用 composite action，标准化"整条流水线"用 reusable workflow，跨仓库共享时放中央仓库并按 tag 或 SHA 版本化引用。[^107^][^108^] 跨平台构建用 matrix 驱动 `runs-on` 与 target 三元组而非复制 job——同一份步骤服务全部平台，`include`/`exclude` 裁剪组合。[^109^] 高流量分支启用 merge queue 防止"分别绿、合并红"，但 workflow **必须**增加 `merge_group` 触发器，否则入队后必需的 status check 永不触发、合并卡死。[^19^]

安全加固是 CI 层的必修课而非选修：GitHub 官方明确"将 action 钉到完整长度的 commit SHA 是把 action 当作不可变发布使用的唯一方式"，并建议在仓库或组织级策略中强制。[^110^] 配套做法是顶层 `permissions: contents: read` 按 job 放宽、Dependabot 跟踪 `github-actions` 生态维持 SHA 新鲜度；ruff 的 workflow 是可直接照抄的范本（`actions/checkout@<full-sha> # v6.0.2` 的注释式钉法）。[^14^][^110^] 薄编排层的 `run:` 字段因此只剩单行入口：

```yaml
- name: quality gate
  run: uvx prek run --all-files --show-diff-on-failure
- name: build & test
  run: just ci
```

### 4.4 Zed 的元自动化

#### 4.4.1 用 Rust xtask 生成 CI YAML 并加一致性门禁：连 CI 配置本身也是生成物

把本章原则推到极限的是 Zed：`.github/workflows/` 下的 YAML 不是手写文件，而是 `cargo xtask workflows` 的生成物，权威源是 `tooling/xtask/src/tasks/workflows.rs` 中的类型安全 Rust 代码。[^1^] 同一段"checkout + sccache + nextest"逻辑在 Rust 里是一个函数，在多平台 job 间复用，YAML 层的重复与漂移在源头上被消灭。[^1^] 生成物入库（GitHub 必须能直接读取 workflow 文件），但 CI 中设有一致性门禁——`check_scripts` job 重新运行生成命令后执行 `git diff --exit-code .github`，任何手改 YAML 或忘记重新生成的提交都会在此失败。[^1^] 这正是 SSOT 三要素的完整闭环：权威位置（workflows.rs）+ 派生方式（xtask 生成）+ 校验器（drift check）。

Zed 同时提供了反面参照：其 `script/` 目录约 90 个 shell 脚本配有 `.ps1` 孪生以覆盖 Windows，双份实现的维护负担恰好印证了 matklad 对 shell 脚本"本质平台相关"的批评[^97^][^111^]——任务逻辑用 Rust（xtask）或 Python（`uv run` 脚本）编写可天然免除这类孪生。对多数项目，Zed 模式的借鉴不必一步到位：第一级是 4.1 节的"CI 只调任务入口"；第二级是本章的钩子同源与薄编排；第三级才是"CI 配置本身也是生成物"。但判据从第一天就成立——**每当一个命令、一段逻辑、一份配置出现第二份手写拷贝，立即把它下沉到任务层或改为生成物，并配一个机械校验器守住它**。

## 5. 测试工程

第 4 章解决了"任务逻辑写在哪、命令从哪进入仓库"，本章接过其中权重最高的一类任务——测试。测试投入的现代分配方式，不再是"单元测试占几成"的比例之争，而是按"缺陷藏在哪里"选择武器：逻辑错误靠单元测试，行为回归靠快照，输入空间爆炸靠属性测试，并发交错靠确定性调度，内存与未定义行为靠动态检查器。本章按形状、断言、确定性基建、CI 矩阵四层展开，落脚点是三语言对照的武器库清单。

### 5.1 测试形状与投入分布

#### 5.1.1 形状是架构的函数：纯逻辑库→金字塔、应用→trophy、微服务→honeycomb；覆盖率门的库/应用二分

经典测试金字塔（Mike Cohn，约 70% 单元 / 20% 集成 / 10% E2E）的前提假设是"高层测试慢、贵、脆"，而现代工具链（进程级并行 runner、容器化服务、快照断言）已显著削弱这一假设[^112^]。Kent C. Dodds 的 Testing Trophy 把集成测试放为最宽层，核心命题是"测试越贴近软件被使用的方式，给出的信心越高"，并批评过度 mock 的单元测试"最难维护、却给不了坚实信心"；但该模型自限为单体应用设计[^113^]。Spotify 的 honeycomb 模型走向另一端：微服务架构下"bug 活在服务边界上"，集成测试膨胀、单元与 E2E 收缩[^114^]。三者的调和结论是**形状是架构的函数而非信仰**：纯逻辑库（解析器、编解码、算法）缺陷藏在函数内部，金字塔仍然成立；胶水与组件代码为主的应用取 trophy；API 密集的服务取 honeycomb[^112^][^115^]。无论哪种形状，静态分析（clippy、ruff+mypy、clang-tidy）都是"零运行时成本的测试"，应作为所有形状的公共底座[^112^]；测试要写在"能捕获该 bug 的最低层"，flaky 测试比没有测试更糟——它训练团队忽略失败[^115^]。

覆盖率门禁（coverage gate）同样应按库/应用二分。DeepSeek Harness（dsh）对 `packages/*/*/src` 执行**逐文件 100% 行覆盖**，理由是"未覆盖行常常是该删的死代码，而不是缺测试"——这把覆盖率门从质量指标改造成死代码探测器，在严格插件化架构下方可行[^61^]。Python 生态的惯例现实得多：`fail_under = 80` 配合 `branch = true` 分支覆盖，且阈值只写 `pyproject.toml` 的 `[tool.coverage.report]`，CLI 与 CI 不得重复传参[^116^][^117^]。落地建议：库代码取 dsh 精神（未覆盖即删或即测），应用代码取 80% 派（阈值是防退化底线而非荣誉勋章）；无论选哪派，阈值单一来源、禁止第二处书写。

### 5.2 现代断言形态

#### 5.2.1 快照测试成为大输出主流断言：快照入库评审、CI 只读、不确定字段 redact

当期望值短且稳定时，`assert_eq!(result, "...")` 是好断言；但当输出是多行 JSON、编译器诊断或 CLI 输出且频繁演进时，手写期望值等于在测试里重复书写程序的输出规范——这正是 DRY 原则在断言层的违反。快照测试（snapshot testing，谱系上承 approval testing）翻转了心智模型：**运行一次 → 捕获输出 → 人工评审 diff → 批准入库**，期望值从代码字符串变成版本化的 `.snap` 数据文件[^118^][^119^]；insta 文档的适用判据直截了当："快照测试在参考值很大或经常变化时特别有用"[^10^]。

三个生态的实践已收敛为同一套纪律。Rust 侧 insta 提供 `assert_snapshot!` 宏与一等交互式评审工作流 `cargo insta review`，CI 中默认绝不自动写新快照[^10^][^10^]；Python 侧 syrupy（零依赖 pytest 插件）把 Soundness 列为设计原则——快照缺失也判失败而非仅差异失败，`__snapshots__/` 必须随代码提交[^120^][^121^]。标杆实证是 uv：测试套件重度使用 insta，典型形态为 `uv_snapshot!(context.filters(), ...)`——`context.filters()` 即 redaction 的实例化，先把路径、版本号等不确定内容过滤再快照[^122^][^123^]；Foundry 则把"配置序列化表面变更必须同步更新快照"写进仓库 AGENTS.md[^124^]。可复用规则四条：CLI 输出、诊断、序列化格式、配置表面首选快照，纯函数关键边界值仍用手写断言（意图更显式）；**时间戳、UUID、绝对路径等不确定字段必须 redact/filter**，否则快照退化为 flaky 测试[^122^][^120^]；快照文件入库并参与 code review——snapshot diff 即行为变更的可审阅证据[^121^]；CI 设只读模式，防止 CI 自动批准回归[^119^]。

#### 5.2.2 属性测试精确分工：round-trip、oracle、不变量三场景

属性测试（Property-Based Testing，PBT）沿 Haskell QuickCheck → Python Hypothesis → Rust proptest 的谱系演进，现代实现的共同机制是三件套：**策略**（strategy，按"每个值"而非"每个类型"定义输入分布）、**收缩**（shrinking，失败时自动把反例最小化，如大列表收缩为 `[0]`）、**失败持久化**（proptest 将失败用例写入 `proptest-regressions/` 并应提交 git，Hypothesis 维护失败数据库优先重测最小失败）[^125^][^32^][^126^]。PBT 不替代示例测试，其价值在"用更少代码覆盖更大输入空间"；对 7 个知名 Python 仓库（cpython、pandas、numpy、jax 等）87 条 Hypothesis 测试的实证研究发现，**round-trip、test oracle、系统等价三类属性占绝对主导**，因为定义直观、成本低[^127^]。

落地按三个场景精确分工。**round-trip**：`decode(encode(x)) == x`，适用一切编解码、序列化、格式化/解析对；**oracle 对比**：优化实现与朴素参考实现输出一致，适用重写优化后的回归保护；**不变量**：排序保长度、转账保总额、归一化幂等、解析器"never panics"[^128^][^129^][^128^]。反模式同样明确：`.gitignore` 掉回归文件；用重 `filter` 而非窄策略（大量生成值被丢弃，覆盖空心化）；CI 用未固定的随机种子；在 property 内 mock 外部服务[^32^]。判断准则一句话：输入域结构化且存在可陈述的不变量，就上 PBT；示例测试留作文档化的具体案例。

### 5.3 确定性测试基础设施

#### 5.3.1 种子可注入、失败必打印复现指令：Zed GPUI 种子化运行时、随机化测试+自动最小化

消除不确定性的第一条战线是调度与随机。Zed 的 GPUI 框架提供 `#[gpui::test]` 宏与 `TestAppContext`：测试运行在**确定性、种子化的调度器**（`TestDispatcher`）之上，宏参数 `iterations = 100` 按种子 0..100 重复执行以暴露竞态，`seed = N` 支持复现；多客户端协作测试注入多个共享同一 executor 的 `TestAppContext`，使异步任务确定性交错[^30^]。在此之上，Zed 协作系统（CRDT/多客户端）用**随机化测试计划**对抗状态空间爆炸：`TestPlan` 以 `SEED` 驱动 `StdRng` 随机生成操作序列（编辑、断连/重连、服务端重启），CI 侧脚本形成完整闭环——随机种子跑 5 万次迭代 → 失败则**自动最小化失败计划**（delta-debugging 式 shrink）→ 最小 plan 连同 commit 上报建档，用 `LOAD_PLAN` 原地复现[^130^][^131^]。

由此可提炼语言无关的种子三规则：所有随机测试的种子必须 **(a) 可注入、(b) 失败时打印、(c) 可用环境变量复现**（如 `PROPTEST_SEED`、`SEED=N cargo test`）[^32^][^30^]。底线是任何失败输出都带"复现指令"——一条能在本地精确重演失败的命令，而不是一句"CI 上偶尔红"。

#### 5.3.2 假时钟/假 IO/record-replay：DeepSeek Harness keyless 快照回放；seam 倒逼解耦

第二条战线是时间、IO 与外部服务。Feathers 的 seam 理论及其"时间、随机、IO 一律构造参数注入"的开发纪律已在 1.3.2 节引入[^29^][^132^]，本节只补其在测试侧的落地增量。第一是假时钟：Go 1.25 标准库的 `testing/synctest` 让测试在 "bubble" 内使用假时钟，仅当所有 goroutine 持久阻塞时推进，"sleep 一小时"的测试微秒级完成；Rust 用 `tokio::time::pause()`，Python 用 freezegun[^133^]。真实 sleep 出现在测试里即坏味道。

外部服务依赖的现代解法是 record/replay。dsh 的 keyless snapshot 体系是完整范本：`pnpm run test:snapshot` 无需 API key——启动真实 automation-server 示例，**回放录制的 session，diff 归一化后的 JSON-RPC 与重新持久化的日志**；系统提示与工具 schema 被 tokenize，使一次编辑只扰动一行 diff；测试政策规定"模型或用户可见行为的每个非琐碎变更，同 PR 更新 keyless 快照"，带 key 的真实 API 测试在无凭据时自动 self-skip，保证 keyless CI 常绿[^27^][^61^]。对已发生的间歇性并发 bug，Linux 上的终局武器是 Mozilla **rr**：录制一次失败即可任意次数确定性回放（拦截系统调用、时钟、线程调度等非确定输入），`--chaos` 模式随机化调度主动暴露竞态；其限制是仅支持 Linux x86-64，Windows 侧对应物为 WinDbg TTD[^134^][^135^]。

### 5.4 CI 测试矩阵

#### 5.4.1 矩阵降维：组合爆炸控制、变更感知测试、缓存按维度隔离

矩阵设计的艺术是用最少组合买到最大信息。组合爆炸是现实威胁：4 OS × 5 运行时 × 3 数据库 × 2 缓存方案 = 120 个 job，单次 CI 45 分钟[^136^]。标准对策分三层。**分层拓扑**：PR 层只跑关键组合（如 ubuntu + 最新工具链），merge queue 层跑完整必过集，nightly 层穷举矩阵与长跑测试[^136^][^19^]。**声明式裁剪**：用 `exclude` 删除已知无效组合、`include` 追加特殊组合，或由前一 job 按变更内容动态生成矩阵[^137^][^138^]；每加一维先问"这一维抓到过真实 bug 吗"。**变更感知测试**（affected tests）：Zed 的 `run_tests.yml` 首设 orchestrate job，用 `git diff` + `cargo metadata` 计算变更 crate，再用 cargo-nextest 的 `rdeps()` filterset 只跑受影响 crate 及其反向依赖；toolchain 或根 Cargo 文件变更则回退全量[^1^]。Nx 的 `nx affected` 与 Bazel 的 action 级缓存是同一思想的不同实现——**依赖图 + 内容哈希 = 测试选择的 SSOT**，基于路径的变更触发是无法理解传递依赖的退化方案[^139^][^140^][^141^]。

两个细节决定矩阵实际成本。其一，merge queue 除 4.3.2 节所述的 `merge_group:` 触发器陷阱外，还有两个测试侧细节：required check 的名称在 `pull_request` 与 `merge_group` 两上下文必须保持一致，否则入队后永不报告；flaky 测试在 merge queue 下被放大为全队阻塞，上线前先治理[^19^][^142^]。其二，**缓存按矩阵维度隔离**：ccache/sccache 的 key 前缀必须包含 OS 与编译器维度，不同 matrix cell 共享缓存会相互污染[^143^]；Rust 侧 `Swatinem/rust-cache` 按 Cargo.lock、toolchain、target 自动构造 key[^144^]。

#### 5.4.2 语言武器库对照：nextest/trybuild/miri、pytest/hypothesis、GTest/sanitizer/fuzzing 矩阵

下表按"缺陷藏身之处"对照三语言的标准件，按模块特征取用，避免为不需要的维度过度投资。

| 缺陷维度 | Rust | Python | C++ |
|---|---|---|---|
| 测试运行器 | cargo-nextest（process-per-test、分片、archive 复用构建产物） | pytest（conftest 注入、parametrize 堆叠） | CTest + `gtest_discover_tests` / `catch_discover_tests` |
| 单元/断言框架 | 内置 `#[test]` + `assert!` 族 | pytest 断言重写 | GoogleTest（大项目）/ Catch2（中小）/ doctest（编译时间敏感） |
| 快照断言 | insta（`cargo insta review`） | syrupy（`__snapshots__/` 入库） | 无统治性标准件，golden file 自管或 ApprovalTests |
| 属性测试 | proptest（`PROPTEST_SEED`、回归文件入库） | hypothesis（失败数据库） | RapidCheck |
| 类型/编译期契约 | trybuild（compile-fail + `.stderr` 期望） | mypy strict / pyright 进 CI | `static_assert` / concepts + clang-tidy presubmit |
| 内存/UB 动态检查 | miri（含 `unsafe` 的 crate 开 nightly job） | 解释执行天然免疫；C 扩展走右列方案 | ASan+UBSan（PR lane）、TSan（独立 lane）、MSVC `/fsanitize=address` |
| 并发交错 | loom（`#[cfg(loom)]` 排列模型检查） | 假时钟 freezegun + 真服务集成测试 | TSan lane 多轮重复 + `rr --chaos` 录制 |
| 不可信输入 fuzz | cargo-fuzz（libFuzzer）+ OSS-Fuzz | atheris + python/library-fuzzers 独立仓 | libFuzzer `LLVMFuzzerTestOneInput` + OSS-Fuzz/ClusterFuzzLite |

表格呈现的不是三份孤立清单，而是同一套缺陷分类学在三个生态的投影：每一行回答"这类 bug 用什么抓"。横向可见三条取舍规律。其一，**Rust 的差异化优势在编译期与执行模型**——nextest 的 process-per-test 隔离让单个 panic 不拖垮全套件并更好吸收长尾测试，在 Windows 上收益尤其明显[^145^][^146^]；trybuild 把"应当编译失败"变成可回归断言，是 proc-macro 与类型级不变量的独有武器[^147^]。其二，**C++ 的重型武器集中在动态检查**：sanitizer 矩阵分工（PR lane 跑 ASan+UBSan，TSan 独立 job，禁巨型单 job）是对内存安全缺口的结构性补偿[^148^]，fuzz 契约也极小——一个 `LLVMFuzzerTestOneInput` 函数即接入覆盖率引导 fuzz，纪律是"fuzz 一切处理不可信输入的边界"[^149^][^150^]。其三，**Python 走轻资产路线**——解释执行免疫整类内存 bug，投资应集中在 pytest 组织力（fixture 永不 import、靠 conftest 发现注入）与 hypothesis/syrupy 的断言现代化上[^151^][^152^][^120^]。跨语言 monorepo 的落地顺序：先统一变更感知与三层 CI 拓扑（语言无关收益最大），再按上表逐语言补齐快照与属性测试两格，最后按模块风险画像（unsafe、解析器、并发原语）按需引入 miri/loom/fuzz 等重型件。

## 6. 文档工程与文档-代码一致性

第 1 章的"生成或引用"判据在前几章治理了代码、环境与配置，本章把它应用到漂移最高发的领域——文档。文档问题的根源很少是"写得不够"，而是**写错了位置**：把代码本可自证的事实性内容（签名、参数、版本号、快捷键、命令）手抄成散文，于是同一条事实出现两份表述，其中一份必然腐烂。Docs as Code 社区给出的定义把文档拉到与代码同一工程平面：issue tracker、版本控制、纯文本标记、代码评审、自动化测试五件套[^153^]；其中最契合防漂移诉求的一条是"功能 PR 不含文档更新即不允许合并"[^153^]。本章沿"职责边界 → 生成链 → 决策记录 → 机械保障"推进，目标是把文档压缩到代码无法自证的最小集合，其余一切交给生成与引用。

### 6.1 文档的职责边界

#### 6.1.1 Diátaxis 四分法：每种文档写什么、不写什么

Diátaxis（diataxis.fr，Daniele Procida 创立）是目前最成熟的文档职责划分框架：它识别四类用户需求与对应的四种文档形态——tutorials（教程，面向学习）、how-to guides（操作指南，面向目标）、reference（参考，面向信息）、explanation（解释，面向理解），主张整个文档体系围绕这四种需求组织[^154^]。它的反重复价值在于**每类文档只回答一类问题**：reference 描述机器本身（what is），不含教学步骤；tutorial 带新手完成一次体验，不堆砌完整参数表；explanation 讲背景、理由与权衡（why），不重复 reference 的签名细节。一份文档越界，就同时制造了"同一内容两种表述"的温床。该框架已被 Python、Django、Cloudflare 等数百个文档项目采用[^154^]。

| 文档形态 | 回答的问题 | 写什么（职责内） | 不写什么（职责外） | 防漂移机制 |
|---|---|---|---|---|
| README / 门户 | what / why 一句话 / 怎么跑 | 项目定位、安装运行命令、指向各层文档的链接 | 完整参数表、实现细节、设计史 | 示例命令纳入 doctest；版本号注入 |
| tutorial / how-to | 如何完成一个任务 | 完成特定任务的最短路径 | 穷举参数（引用 reference 即可） | 示例代码进 CI 测试 |
| reference（API 参考） | 契约是什么 | 签名、类型、参数、错误、行为契约 | 教程、设计理由 | 全部由代码生成，禁止手写 |
| explanation / 架构文档 | 为什么是这样 | 系统结构、组件关系、关键约束 | 逐函数说明、可从代码读出的现状描述 | 文本图示随代码版本化 |
| ADR | 当时为什么这样决定 | 单个决策的 context/decision/consequences | 当前状态描述（归架构文档） | 顺序编号、supersede 不删除 |
| CHANGELOG | 这版变了什么 | 面向用户的逐版本 notable changes | 提交流水账 | 从 conventional commits 生成 |

这张表的用法是评审判据而非分类学练习：评审任何一段文档时先问"它回答哪类问题"，再问"这个事实的权威位置在哪"。若一段文字回答的是"how is it implemented"，它属于代码与代码注释，不属于任何独立文档——实现细节写进散文就是预定了未来的漂移。判据的另一半是机制列：每种文档形态都配了对应的机械化同步手段，没有任何一格依赖"记得手动更新"。越界即删不是洁癖，而是成本核算——大文档从不被维护，小而职责单一的文档才有被更新的可能。

#### 6.1.2 文档只剩三件事：意图、理由、入口

把四分法再压缩一步，手写文档的合法内容只剩三类：**意图**（spec/explanation，回答 why）、**理由**（ADR，回答 why this way）、**入口**（README/how-to，回答 how to run，且写成可执行命令）。所谓"文档超出职责范围"，本质都是把第四类内容——事实性的 what is——手写成了散文。这一划分在 agent 时代得到独立印证：ETH 的实证研究发现 context 文件中重复代码已有信息是负收益[^40^]，而 Zed 把文档中的快捷键这类易腐事实直接改为生成物（见 6.2.2）[^11^]。落地时可用一个三问过滤器统辖全部文档决策：这段内容代码能自证吗？（能→删除或改为生成物）它回答的是 why 吗？（是→ADR/explanation，只写一次）它是入口吗？（是→写成命令并纳入测试）。

### 6.2 SSOT 生成链

#### 6.2.1 参考文档从代码生成

单一事实来源（SSOT）的工程含义是：每条信息只在一个权威位置维护，其他所有出现位置都是生成物或引用，手写第二份拷贝即引入漂移源。API 参考是这一原则最无争议的适用域——签名、类型、参数永远从代码读，四个生态各有标准链：Rust 的 rustdoc 把 `///` 文档注释与代码同文件维护，`cargo doc` 生成、docs.rs 自动托管[^8^]；Python 的 mkdocstrings 构建期导入包，由 Griffe 收集类型注解并渲染参数与返回类型，"签名改了文档自动变"[^155^]；C++ 走 Doxygen 解析注释生成 XML、Breathe 桥接、Sphinx 渲染的三段管线[^156^]；HTTP API 则以 OpenAPI spec 为契约，"OpenAPI serves as both documentation and as a contract… one source of truth"[^157^]。

OpenAPI 域存在 spec-first 与 code-first 两种拓扑：前者以 spec 为 SSOT，handler、SDK、文档由它生成或对照它校验，"the spec cannot lie by construction"；后者从类型注解生成 spec（FastAPI 模式），弱点是"spec 只在注解 accurate 时 accurate"，需契约测试兜底[^158^]。两派共享同一条防重复戒律："不要在代码注解和独立 YAML 两处重复 API 信息，那是漂移和 bug 的温床"[^159^]。取舍规则：对外契约（HTTP API、插件 ABI）偏 spec-first，内部模块偏 code-first。

README 中的代码示例同样纳入生成链的校验端：rustdoc 官方书给出 `#[doc = include_str!("../README.md")]` 的做法，把 README 作为 crate 文档包含进来，`cargo test` 时 README 里的示例一并编译运行[^8^]。README 示例从此不可能与 API 漂移而不被 CI 发现——这是把"文档"改写成"测试资产"的最小改动。

#### 6.2.2 版本号/命令/快捷键注入：文档不许手抄代码事实

生成原则应推广到一切"代码里已存在的事实"。Zed 的文档是标杆实例：其 `docs/` 为标准 mdBook，但叠加了自研预处理器 `docs_preprocessor`，把 `{#kb agent::ToggleFocus}`、`{#action agent::OpenSettings}` 等指令在构建期展开为当前真实的 keybinding 与 action 引用——文档中的快捷键永远与代码同步，`docs/AGENTS.md` 明令"Always use preprocessor syntax for keybindings instead of hardcoding"[^11^][^160^]。同一思路的轻量变体早已普及：Kubeflow 用 Hugo shortcode 把"最低 Kubernetes 版本"定义在单一文件、全站引用，避免"a maintenance nightmare"[^161^]；mdBook 的 `{{#include}}`、MkDocs macros、AsciDoc attributes 都是同构机制。版本号的纪律是单一存储点：Rust 只在 `Cargo.toml`（代码经 `env!("CARGO_PKG_VERSION")` 读取）、Python 只在 `pyproject.toml`，README 与 badge 中的版本由 CI 注入或干脆不写死。给混合语言项目的可执行规则：**枚举仓库里每条会出现在文档中的事实（版本号、命令、快捷键、配置项、默认值），逐条指定权威位置与注入机制；发现任何手抄，即视为待修复的漂移源。**

### 6.3 决策与变更记录

#### 6.3.1 ADR/MADR：决策理由只写一次

"为什么这样做"是最容易被重复且互相矛盾的一类内容——散落在 PR 描述、代码注释、wiki 和口口相传中。ADR（Architecture Decision Record，架构决策记录）由 Michael Nygard 2011 年的奠基文章提出，动机正是"项目生命周期中最难追踪的就是某些决策背后的动机"，新人面对历史决策只有盲目接受或盲目推翻两条坏路[^7^]。核心约定：只记录"architecturally significant"的决策；存于仓库内；顺序编号、单调递增、编号不复用；决策被推翻时**不删除旧记录，标记 superseded**——"它仍是曾经的决策，只是不再是现行决策"；全文五段（Title/Context/Decision/Status/Consequences），一两页为限[^7^]。社区标准化的 MADR 模板（当前 4.0.0）将其固化为 `docs/decisions/NNNN-title.md` + frontmatter 状态机（proposed/accepted/deprecated/superseded by ADR-NNNN），并增设 Confirmation 段描述如何用评审或测试确认决策被遵守——这把 ADR 也接进了机械验证回路[^162^]。

ADR 防重复的关键纪律是**引用而非复述**：理由只存在于 ADR 一处，代码注释、架构文档、PR 描述一律只写"see ADR-0007"；决策演化通过新增 ADR + supersede 链接表达，而不是回头改写多份文档。于是决策史成为一串不可变、互相链接的小记录——"一个 ADR 的 consequences 很可能成为下一个 ADR 的 context"[^7^]。

#### 6.3.2 CHANGELOG 自动生成

手写 changelog 是典型的"同一信息写两遍"：commit 写一遍、changelog 再抄一遍，且它还是 merge queue 下最高频的冲突源[^163^]。Keep a Changelog 规范定义了产物的形态：changelog 是"面向人类的、按时间排序的 notable changes 策展列表"，固定六类分组（Added/Changed/Deprecated/Removed/Fixed/Security），顶部保留 Unreleased 节，并明确反对把 commit log 当 changelog——"它们充满噪声"[^164^]。Conventional Commits 则把输入端结构化：`<type>[scope]: <description>`，`fix`→PATCH、`feat`→MINOR、`BREAKING CHANGE`/`!`→MAJOR，官方列出的首要用途即"自动生成 CHANGELOG"与"自动确定语义化版本号"[^165^]。生成器一侧，git-cliff 以 `cliff.toml` 的正则 parser 把提交分组进 Keep a Changelog 章节，单 Rust 二进制可嵌入任何语言项目的 CI，`--bump` 直接推导下一版本号[^3^]；Rust 生态的 release-plz 进一步把 Release PR 作为人工闸门——机器算版本、生成 changelog，人只点 merge[^166^]。分工就此清晰：commit message 是结构化、机器可读的 SSOT；CHANGELOG.md 是策展过的、人读的生成物；日常条目不手写，仅允许对基线条目做人工润色[^167^]。

### 6.4 防漂移的机械保障

#### 6.4.1 CI 门禁清单

原则只有一条：任何能在 CI 里机械验证的一致性，就不靠人肉纪律维持。一份最小门禁清单按四组配置——**示例可执行**：Rust `cargo test --doc`（含 `include_str!` 纳入的 README）[^8^]，Python 用 pytest 驱动标准库 doctest 在 CI 中自动测试文档片段[^168^]，C++ 的等效做法是文档示例经 Doxygen `\snippet`/`\include` 引用真实可编译的示例源文件；**生成物再生成 diff**：凡由代码或 spec 生成的文档，CI 重新生成并 `git diff --exit-code`，手改生成物或与源不一致即失败——Zed 对生成的 CI YAML 用同一招[^1^]；**链接检查**：lychee 官方 action 在 PR 上 `fail: true` 拦截死链，外部链接改定时任务并自动开 issue[^169^]；**散文 lint**：Vale 以可版本化的 `.vale.ini` + 可继承的 Google/Microsoft 样式规则检查术语与语气，规则级别设为 `error` 即阻断 CI，实操上先以 annotation 非阻塞运行、误报收敛后再转为 gating[^170^]。文档站构建本身（`mkdocs build --strict` 等）是第四道兜底检查[^153^]。Stripe 的 Markdoc 管线展示了这一清单的完整形态：文档解析为 AST 后按 schema 校验、链接路由全部验证，且"we run the Markdoc validator in our continuous integration system"[^171^]。

#### 6.4.2 同 PR 共演化与 AI 时代的 SSOT 疆域

机械保障之上还需一条流程纪律：文档与代码在同一个 PR 里原子化演化，"功能 PR 不含文档不合并"[^153^]。Zed 把这条纪律延伸到了 LLM 时代：`docs_suggestions.yml` 监听合入 main 的代码变更，调用 LLM 分析 diff 产出文档更新建议，批量累积后由脚本开 PR 交人工复核——机器准备、人始终在环[^20^]。

更根本的问题是 AI 编码带宽下"什么才是 SSOT"。两股观点看似对立：GitHub Spec Kit 主张从"code is the source of truth"转向"intent is the source of truth"，spec 成为可执行的活文档[^172^][^173^]；AI-native 工程组织则坚持代码为 SSOT、把 spec 入库为可被机器校验的资产[^174^]。spec-kit 讨论区的裁决式区分收敛了这场争论：代码是"实际做了什么"的 SSOT，spec/ADR 是"意图与理由"的 SSOT，二者各有疆域、不互相重复，靠入库加机械校验（diff against spec、doctest、契约测试）防漂移[^175^]。这没有推翻"文档写 why/what、代码表达 how"的老信条，反而强化了它——how 的信息密度已高到只有代码与测试能承载，文档收缩到意图、理由、入口三件事，正是为了让剩下的每一份手写内容都值得维护。

## 7. 发布工程

至此，从环境、任务、测试到文档的 SSOT 链已经闭合，本章处理交付前的最后一环。发布工程要同时回答三个问题：版本号如何只维护一次、发布如何全自动但保留人工闸门、制品如何在被篡改的环境下仍然可信。跨语言标杆项目给出的收敛答案是：**git tag 是版本事件的唯一触发源，commit message 是版本意图的唯一来源，可信凭据只在 CI 运行时分分钟级存在**。任何手写版本号、手写 changelog、长期发布 token，都是会腐烂或被窃的重复与风险。本章按"版本 SSOT → 可信发布 → 跨平台分发 → 节奏与人机分工"展开。

### 7.1 版本号单一来源

#### 7.1.1 git tag 为唯一版本源：从 tag 推导，杜绝两处维护

典型反模式是 `__version__`、`pyproject.toml` 的 `version` 字段、git tag 三处手工同步——三份拷贝必然漂移，且漂移在发布当天才暴露。主流解法是让构建系统从 git tag 推导版本，源码与清单里不留第二份手写拷贝：

- **Python**：hatch-vcs 通过 `[tool.hatch.version] source = "vcs"` 在构建时从 tag 读版本，并可生成 `_version.py` 供运行时引用；uv 项目对应方案是 uv-dynamic-versioning[^176^][^177^]。一个关键限制：uv 内置的 `uv_build` 后端要求静态版本字段、拒绝 `dynamic = ["version"]`，因此 tag 推导需改用 hatchling 后端，或保留静态版本交给 release-please 机械改写[^177^]。
- **Rust**：cargo-dist 的模式是典范——`git tag v1.0.0 && git push --tags` 即触发 plan（按 tag 选定发布的包与版本）→ build → publish → announce 的完整管线，tag 格式本身编码发布范围（`v{VERSION}` 统一发布 / `{PKG}-v{VERSION}` 单独发布）[^178^]。
- **C++**：版本号写在 CMake `project(... VERSION x.y.z)` 一处，头文件中的版本宏由 `configure_file` 生成，不在源码中手写第二处。

可复用规则：版本号单一存储点（Rust 为 `Cargo.toml`、Python 为 tag 或 `pyproject.toml`、C++ 为 `project(VERSION)`），二进制内嵌版本由构建系统注入（如 Rust 的 `env!("CARGO_PKG_VERSION")`）；发布动作就是推 tag，tag 与包内版本的一致性由 CI 校验[^178^]。

#### 7.1.2 Conventional Commits = 版本意图 SSOT：Release PR 作人工闸门

tag 回答"何时发"，commit message 回答"发多大"。Conventional Commits v1.0.0 把提交前缀定义为机器可解析的结构化数据：`fix:` → PATCH，`feat:` → MINOR，`feat!:` 或 footer `BREAKING CHANGE:` → MAJOR[^165^]。由此版本号推导与 changelog 生成共用同一份输入，纪律则靠 commit-msg hook（commitlint、Lefthook 等）强制——"纪律只存活于工具中"[^167^]。如 6.3.2 节所述，手写 changelog 是 merge queue 下最高频的冲突源，应整体交给工具生成[^163^]。

围绕这份"意图 SSOT"，各生态收敛出五个代表工具，差异集中在意图来源与人工闸门两个维度：

| 工具 | 生态 | 版本意图来源 | 人工闸门 | Changelog | 适配场景 |
|---|---|---|---|---|---|
| release-please（Google） | 多语言 | Conventional Commits | Release PR（合并才发版） | 自动生成 | 多语言 monorepo，按 package 配多组件[^16^] |
| release-plz | Rust | Conventional Commits | Release PR | git-cliff 驱动 | 集成 cargo-semver-checks 标注 API 破坏；内置 crates.io Trusted Publishing[^166^][^179^] |
| changesets | JS（思想可移植） | 显式 changeset 文件（人写） | "Version Packages" PR | 人写条目，质量最高 | monorepo 多包协调；追求"版本 bump 永不意外"[^180^] |
| semantic-release | JS | Conventional Commits | 无（全自动） | 自动生成 | 迭代极快、下游容忍度高的工具链[^181^] |
| git-cliff | 语言无关 CLI | Conventional Commits + 自定义 parser | 由调用方决定 | 模板高度可定制 | 只需 changelog/版本推导的单 Rust 二进制，可嵌入任意 CI[^3^] |

这张表呈现的是一条哲学光谱而非优劣排名。左端 semantic-release 追求零摩擦全自动，代价是放弃可审计的发布决策点；右端 changesets 用一份手写意图文件换取 changelog 质量与"版本 bump 永不意外"。多方独立收敛的平衡点是 **Release PR**：机器准备好版本号、changelog、tag 计划，人类只点 merge——自动化与可审计兼得，这正是"全自动 vs 人闸 vs 本地命令式"之争（semantic-release / release-plz / cargo-release 三派）在实践中被多数项目采纳的折中[^16^][^166^]。对混合语言项目的落地建议：Rust 侧用 release-plz（顺带获得 cargo-semver-checks 对公共 API 破坏的机械校验，245+ 条 lint 且在并入 cargo 官方的路线图上[^182^][^183^]），Python/C++ 侧用 release-please 的多组件模式，changelog 模板统一用 git-cliff 格式。

### 7.2 可信发布与供应链安全

#### 7.2.1 OIDC Trusted Publishing 取代长期 token

长期 API token 是发布链上最薄弱的一环。2025-03 的 litellm 事件提供了根因样本：PyPI token 经被攻陷的 CI action 外泄，攻击者直接发布恶意版本——"任何把 PyPI token 存为 GitHub secret 的 workflow 都暴露在这一类攻击下"[^184^]。

OIDC Trusted Publishing 消除了这个静态秘密：CI 运行时向 GitHub 请求携带 repo/workflow/ref 声明的短期 OIDC token → 注册表验证签名与预先注册的 trusted publisher 配置 → 颁发分钟级、限定 scope 的临时令牌。"CI 中不存任何 secret，workflow 运行之间不存在任何 token"[^185^]。生态落地时间线：PyPI 自 2023 年起支持（换取的发布令牌 15 分钟内过期，已有超过 50,000 个项目启用、占全部上传量的 20% 以上[^186^]），crates.io 于 2025-07 GA（`rust-lang/crates-io-auth-action` 换取约 30 分钟令牌，release-plz 已内置支持[^187^][^188^][^179^]），npm 与 NuGet 随后跟进[^189^][^190^]。

三个落地细节：其一，**crates.io 与 npm 首发必须手动**——trusted publisher 只能绑定已存在的包，首次发布仍需 `cargo login`/npm 发布后吊销 token（PyPI 是例外：pending publisher 允许为尚不存在的项目名预注册，首发即可走 OIDC）[^187^]；其二，构建与发布拆成**双 job**，仅发布 job 持有 `id-token: write` 权限，收缩供应链攻击面[^191^]；其三，token 消失后**真正的发布闸门前移到 workflow 权限、分支保护与 tag 策略**——crates.io 已移除对 `pull_request_target`/`workflow_run` 触发器的 TP 支持，并可开启 TP-only 模式彻底禁用 token 发布，这些配置必须与代码同级审计[^187^][^188^]。

#### 7.2.2 sigstore/SLSA/SBOM/attestation：信任从"凭据保密"迁移为"可验证短链"

OIDC 解决"谁有权发"，attestation 解决"发出来的东西怎么来的"。四件套分工：

- **Sigstore keyless 签名**：OIDC 身份 → Fulcio 颁发分钟级证书 → cosign 签名 → Rekor 公共透明日志存证。没有私钥可偷、可轮换、可丢失——"等攻击者摸到 runner 时，证书早已不存在"[^192^]。
- **SLSA 分级**：L1 有 provenance（可伪造）→ L2 托管构建平台 + 签名 provenance → L3 强化隔离构建 + 不可伪造 provenance。GitHub Artifact Attestations（`actions/attest-build-provenance`）开箱即达 L2，用可复用工作流隔离构建与签名可达 L3[^193^][^194^]。
- **SBOM**（CycloneDX/SPDX，Syft 生成）：provenance 说明制品"如何构建"，SBOM 说明"内含什么"，两者互补，均可用 `actions/attest-sbom` 签名绑定到制品[^194^]。
- **消费者侧验证**：`gh attestation verify <artifact> --repo org/repo` 一行命令完成校验。这条证明链回答的正是 xz 后门式问题——"源码干净但分发的二进制被下毒"[^194^][^195^]。

由此得到发布流水线的标准顺序：Release PR → tag → 矩阵构建 → attest provenance + SBOM → OIDC Trusted Publishing → GitHub Release；同时删除仓库里所有长期发布 token、第三方 action 按完整 SHA 固定，并把消费者验证命令写进 README[^185^][^194^]。

### 7.3 跨平台分发

#### 7.3.1 cargo-dist 跨平台二进制与产物对账：Zed 周三例行实证

对需要分发原生二进制的项目（Rust CLI、C++ 应用），cargo-dist（现名 dist）把"跨平台矩阵构建 + 安装器生成"收敛为自生成的 CI：`dist init` 生成 `release.yml`，推 `v*` tag 即完成 plan → build（各平台 tarball + 安装器）→ host（GitHub Release + checksum）→ publish（npm/Homebrew formula）→ announce，产物包括 shell/PowerShell 一键安装脚本、MSI、npm 包与 Homebrew formula[^196^]。其两条工程纪律值得直接照搬：**CI 里跑的那一条 dist 命令，本地能原样复现**；**发布管线也要进 PR CI**（至少跑 plan 步），避免"发版当天才发现管线坏了"[^196^]。

规模化的实证来自 Zed 的周三例行发布：tag 触发 `release.yml` 后，三平台先跑完整测试套件，再构建 14 种产物（macOS `.dmg` ×2、Linux `.tar.gz` ×2、Windows `.exe` ×2、remote-server ×6 等），随后 `validate_release_assets` 用 jq 将预期产物清单与实际上传产物**对账，缺一即失败**——发布产物先声明后对账，是防止"静默缺平台"的机械兜底[^197^][^111^]。

C++ 库的分发另有一套 API/ABI 双轨纪律：abseil 明确"承诺 API 兼容但不承诺 ABI"，影响 ABI 的编译选项（`-std=`、`-fexceptions` 等）必须全局统一，消费者应全源码同构构建[^59^]；fmt 则以 `fmt::fmt` 与 `fmt::fmt-header-only` 双 target 发布，把 header-only（无 ABI 匹配负担）与 compiled（编译更快）的选择权交给链接 target[^198^]。库作者默认照抄 fmt 模式，应用侧遵守 abseil 纪律。

### 7.4 发布节奏与人机分工

#### 7.4.1 通道制与例行发布日：失败回滚预案

版本与供应链解决"单次发布"的正确性，通道制解决"持续发布"的风险摊平。Zed 的四通道模型是完整范本：Dev / Nightly / Preview / Stable，通道由构建期文件 `crates/zed/RELEASE_CHANNEL`（内容为单个词）决定；发布日从 `main` 切出 preview 分支、再从 preview 切出 stable 分支，tag 与通道严格对应（`stable → v{version}`、`preview → v{version}-pre`），`script/determine-release-channel` 在 CI 中校验 tag 与通道匹配，不匹配直接失败[^199^]。三个配套机制：

- **节奏固定**：发布日固定在周三（脚本内部戏称 "Zednesdays"），preview 始终领先 stable 一周——这一周的时差本身就是稳定通道的回滚缓冲与熔断窗口，preview 暴露的回归可在到达 stable 前被 cherry-pick 修复拦截[^199^][^200^]。
- **例行全脚本化**：版本 bump 由 `script/bump-zed-version` 触发 workflow 完成，但**非周三执行会要求人工确认**——例行路径零人工，例外路径强制人闸[^200^]。
- **Nightly 滚动**：`release_nightly.yml` 每 4 小时 cron 检查，若 `nightly` tag 已指向当前 commit 则跳过，否则重建发布，尝鲜用户永远有可用的最新构建[^201^]。

失败回滚预案由此分层：通道制提供时间维度的缓冲（问题先污染 preview，不污染 stable）；tag 不可变语义保证回滚动作是"发一个新的修复版"而非改写历史；对已流入注册表的坏版本，crates.io/PyPI 的 yank 机制可将其标记为不可新装而不破坏既有 lockfile。配合 7.2 的 attestation，消费者始终可以验证自己拿到的是哪个通道、哪次构建的产物。至此人机分工的全貌闭合：**机器负责推导、构建、签名、对账与例行节奏，人负责 Release PR 的合并、非例行发布的确认、以及回滚决策**——批准成本被压到一次点击，但不可逆动作的闸门始终在人手里。

## 8. Agent 开发规范

前七章讨论的工程纪律——SSOT、单一命令入口、机器强制、drift check——在引入 AI 编码 agent 之后并不会被取代，反而被放大：agent 是仓库里最诚实也最昂贵的读者，它逐字消费指令文件、当真执行里面的每一条命令，并以 token 成本与任务失败率的形式，立刻为文档工程纪律的缺失开出账单。本章回答三个问题：指令文件放在哪、写什么才有正收益、以及仓库本身要做哪些改造才能被 agent 高效执行。

### 8.1 AGENTS.md 事实标准

#### 8.1.1 开放标准现状：纯 Markdown、无 schema、就近优先

AGENTS.md 是仓库根目录下的纯 Markdown 文件，官方定位为 "README for agents"——为编码 agent 提供构建、测试与规范上下文的可预测位置。其规范刻意保持最小：无必填字段、无 schema，agent 直接解析文本；官方推荐章节包括项目概览、构建与测试命令、代码风格、测试指令与安全考量[^34^]。两个机制特征决定了它的工程行为：其一，**就近优先**——monorepo 子目录可放嵌套 AGENTS.md，agent 自动读取目录树中离被编辑文件最近的一份，最近者优先，会话中的显式用户指令又覆盖一切文件指令[^34^]；其二，**可执行性**——agent 会主动执行 AGENTS.md 中列出的检查命令并尝试修复失败，这是它区别于普通文档的根本属性：写进去的命令会被当真跑[^34^]。

采用与治理层面，该格式 2025 年 8 月由 OpenAI 联合 Amp、Google Jules、Cursor、Factory 等共同发布，官方口径已有 60,000+ 开源项目采用，OpenAI 主仓库内含 88 个嵌套 AGENTS.md 文件[^34^]。2025-12-09，Linux 基金会宣布成立 Agentic AI Foundation（AAIF），AGENTS.md 与 Anthropic 捐出的 MCP、Block 捐出的 goose 并列为三个创始项目[^35^][^36^][^202^]；对 2,853 个 GitHub 仓库的实证研究亦确认 context file 占据 agent 配置机制的绝对主导，AGENTS.md 正成为跨工具互操作标准[^203^]。对实践者的含义很直接：把 AGENTS.md 当作与代码同级、被版本化与评审的软件制品——采用率研究发现 50% 的文件创建后从未修改，而健康的变更模式是随代码共演化的细粒度指令调优[^204^]。

#### 8.1.2 互操作防重复：指令文件也遵守 SSOT

各工具的原生指令文件尚未完全统一到 AGENTS.md，多工具并存的项目面临的第一性问题不是"用哪个"，而是"事实写在哪一份"。现状与桥接方式如下：

| 工具 | 原生指令文件 | 对 AGENTS.md 的支持 | 推荐桥接方式 |
|---|---|---|---|
| OpenAI Codex | AGENTS.md / AGENTS.override.md | 原生 | 无需桥接[^34^] |
| GitHub Copilot | .github/copilot-instructions.md 及 .github/instructions/*.instructions.md | 官方文档明确支持，目录树就近优先 | symlink 或一行引用指向 AGENTS.md[^205^][^206^][^207^] |
| Cursor | .cursor/rules/*.mdc | 可读；.cursorrules 已废弃，官方建议迁往 AGENTS.md | 直接采用 AGENTS.md，路径规则留 .mdc[^203^][^208^] |
| Claude Code | CLAUDE.md | 不原生读取 | CLAUDE.md 内一行 `@AGENTS.md` import，或 symlink[^5^][^209^] |
| Gemini CLI | GEMINI.md | 需配置 `"context": {"fileName": "AGENTS.md"}` | 配置指向同一文件[^34^] |
| Aider | 无默认 | `.aider.conf.yml` 加 `read: AGENTS.md` | 配置指向同一文件[^34^] |

表中三种桥接形态——import、symlink、配置指向——共享同一拓扑：AGENTS.md 是唯一权威位置，其余文件只是引用，不存在第二份内容拷贝，漂移在结构上不可能发生[^5^][^210^]。该模式已被顶级项目验证：Next.js 的 AGENTS.md 开篇即声明 "`CLAUDE.md` is a symlink to `AGENTS.md`. They are the same file."[^211^]；dsh（DeepSeek Harness）同样将 CLAUDE.md symlink 到根 AGENTS.md[^27^]。反面教训同样明确：SSW 把"CLAUDE.md 手工维护为 AGENTS.md 副本"列为反面示例，因为手工同步的副本必然漂移并导致 agent 行为不一致[^212^]。落地结论只有一条：symlink 在 Windows 上需 `core.symlinks=true` 或开发者模式[^5^][^212^]，团队有原生 Windows 成员时退而用 `@AGENTS.md` import 或一行引用——三种形态任选，唯独不许复制内容。对真实仓库的观察研究确认，成熟仓库的 context file 之间普遍采用"仅引用"写法（直接指针、短引用加上下文、摘要加引用三式）[^203^]。

### 8.2 写什么、不写什么

#### 8.2.1 实证研究结论：LLM 生成降成功率，人写精简才有效

指令文件"该写多少"曾有两种相反证据，交叉验证后可调和为一个判据。ETH Zürich 的研究（SWE-bench Lite + 自建 AGENTbench，4 个 agent × 多模型）给出反直觉的结论：context file 平均而言**降低**任务成功率，同时使推理成本上升超过 20%；细分后，LLM 自动生成的平均降低成功率约 3%，人工编写的平均提升约 4%（但仍增加成本，≤19%）[^40^]。更关键的机制性发现有三：其一，指令确实被遵守——提到 `uv` 时 agent 平均每实例调用 1.6 次，未提到时不足 0.01 次，效果不佳不是指令遵循问题，而是内容本身的问题[^40^]；其二，"代码库概览"类章节无效——agent 自行探索仓库同样快，概览只增加 token[^40^]；其三，删除仓库内全部文档后，LLM 生成的 context file 反而转为正收益（+2.7%）——这直接证明 **context file 的价值等于增量信息，而非已有文档的重复**[^40^]。方向相反的证据来自 Lulla 等对真实 PR 的对照研究：根 AGENTS.md 的存在与 token 用量下降、任务完成更快相关[^213^]。两项研究的可调和读法是：精简、人工维护、只含增量信息的文件提效，臃肿或自动生成后不裁剪的拖慢。实践判据由此成形——对每条候选规则问"agent 不看这条会犯错吗"，答否则删；从最小集起步，按 agent 实际犯的错迭代增补，不用 `/init` 一键生成后原样入库[^40^][^214^]。

#### 8.2.2 架构约束有效、架构概览无效、安全护栏普遍缺失

内容选择的精确分界在"约束"与"概览"之间。**架构约束**（禁令 + 理由 + 机器强制出处）必须成文，因为"为什么禁止"无法从代码推断：Kibana 的 AGENTS.md 规定 server 入口不得静态 import `./plugin`，解释原因（被禁用的插件仍会被 Node 解析执行），并指向强制该规则的 ESLint 规则与原始 PR/issue——规则、机器强制、出处三位一体[^215^]。这与第 1 章"架构约束编译进工具链"（1.3.1 节）同构：AGENTS.md 陈述 why 并指路，lint/类型系统负责强制。**架构概览**（目录树导览、现状描述）则相反：实证证明无效，且 23.0% 的代表性仓库已出现腐烂引用（指向已删除的函数/模块）——"模型引用了一个不存在的函数，或强制执行团队数月前已放弃的约定"，且这种退化不产生任何可见错误[^28^]。治理研究还揭示两个普遍缺口：七原则评估中 94%（32/34）的真实 AGENTS.md 结构完整性不达标，时效性原则全体得 0 分——没有一份文件声明失效触发器[^216^][^217^]；2,303 份 context file 中安全主题仅占 14.8%、性能占 14.5%，即约 85% 的仓库只教 agent "让代码能跑"，未提供安全与性能护栏[^218^]。差异化改进点因此清晰：写安全护栏（密钥不入库、依赖变更与迁移须先询问、不碰生成物），为每节声明失效触发器（如"本节随 justfile 变更而更新"），在改动代码的同一 PR 同步更新指令[^204^][^217^]。

综合以上实证，一个面向 Rust/C++/Python 混合仓库的最小 AGENTS.md 骨架如下——每条都是 agent 无法从代码推断的增量信息：

```markdown
# AGENTS.md
<!-- 时效声明：命令节随 justfile 变更而更新；最近审查 2026-08 -->

## 命令（唯一入口：just，禁止绕过）
- 全量检查：`just ci`（本地绿 ≡ CI 绿）
- 单测试：Rust `cargo nextest run -p <crate> <name>`；Python `uv run pytest path::test -xvs`；C++ `ctest --preset dev -R <name>`
- Lint/类型：`just lint`（clippy + ruff + clang-tidy，<15s 增量）

## 环境
- 工具链由 flake.nix 提供；先 `direnv allow`。沙箱内网络禁用，凡需网络的测试已标记早退，勿"修复"它们。

## 禁令（NEVER）
- NEVER 直接调 pytest/cargo/cmake，一律经 just 入口（原因：入口注入种子与超时参数，绕过则测试不可复现；由 CI `just ci` 强制）。
- NEVER 手改 uv.lock / Cargo.lock / 生成物（`git diff --exit-code` 门禁）。
- ASK 后再动：新增依赖、数据库迁移、`.github/` 下任何文件。

## 风格（lint 管不到的部分）
- 时间/随机/IO 一律构造参数注入，禁止隐藏调用 `now()`/`rand()`（原因：测试确定性；loom/proptest 基建依赖此 seam）。
```

该示例刻意不写目录结构与模块导览（agent 可自行探索），不写 README 已有的安装说明（应链接而非复制），每条禁令附理由与强制出处，总长约 30 行——低于社区经验的 200 行常驻预算[^219^][^209^]。

### 8.3 分层指令机制

#### 8.3.1 SKILL.md 三层渐进披露与 path-scoped rules：避免单文件膨胀

常驻指令文件每次会话全量加载，其膨胀直接转化为每次请求的固定 token 成本——即 context bloat，而治理研究观察到的"一切塞进单个 always-on 文件"正是常见反模式[^220^]。架构级答案是把指令按激活方式分层：常驻层（根 AGENTS.md，必须精简）→ 路径作用域层（按文件 glob 按需加载）→ 技能层（按任务相关性由 agent 自主激活）→ 外部知识层（docs/ 与 CONTRIBUTING，只链接不复制）[^203^][^220^]。

技能层的载体 SKILL.md 于 2025-12-18 由 Anthropic 发布为开放标准，已被 Cursor、GitHub Copilot、Gemini CLI、Codex、OpenCode 等 26+ 平台采用[^221^]。其结构为"一个目录 = 一个 skill"：唯一必需文件是 SKILL.md，由 YAML frontmatter（必填仅 `name` 与 `description`，name 须等于目录名，description ≤1024 字符且须写明"做什么 + 何时用"）加 Markdown 正文构成，可选 scripts/、references/、assets/ 子目录[^221^]。核心机制是**三层渐进披露（progressive disclosure）**：Level 1 仅元数据常驻（每 skill 约 30–100 token）；Level 2 激活时加载正文（建议 <5,000 token）；Level 3 资源文件按需加载[^221^]。这意味着知识总量不设上限，而常驻成本有界——单文件膨胀问题被转化为"何时激活"的元数据设计问题。跨工具位置约定为项目级 `.agents/skills/` 与个人级 `~/.agents/skills/`，Claude 侧用 symlink `.claude/skills → ../.agents/skills` 保持单一事实来源[^212^][^221^]。

路径作用域层在各工具中的形态：Cursor 的 `.cursor/rules/*.mdc` 以 frontmatter 决定激活方式（`alwaysApply: true` 常驻 / 仅 `description` 由 agent 判断相关性 / `globs` 文件匹配自动附加），官方建议单规则 <500 行且 always-on 规则极短[^208^]；Copilot 的 `.github/instructions/*.instructions.md` 按路径生效[^205^][^206^]；Claude 的 `.claude/rules/*.md` 支持 `paths` frontmatter[^209^]；AGENTS.md 嵌套则是最通用的就近优先形式[^34^]。分工判据收敛为一句话：**约束放 AGENTS.md/rules，流程放 skills，参考知识放 docs/ 并只留链接**[^220^]。这与第 6 章文档职责划分同构——流程是"how"，约束是"why 的禁令形式"，二者混进同一个常驻文件，正是 context bloat 与 context rot 的共同温床。

### 8.4 Agent 可执行的仓库

#### 8.4.1 单一命令入口 + 快速反馈环 + 明确测试指令

实证与标杆实践共同表明：agent 的效率是仓库工程质量的函数，而非指令文本书写技巧的函数。三个改造点按杠杆排序。其一，**单一命令入口并在指令中钦定禁止绕过**：Airflow 规定 "Never run pytest, python, or airflow commands directly on the host — always use breeze"，并配一段工具自动生成的命令表（`START generated-commands` 标记区段，手写与生成分离）[^222^]；OpenAI Codex 仓库用 just 统一任务入口，要求 agent 先装齐 `just`/`rg`/`cargo-insta`[^223^]。对混合语言仓库，第 4 章的 justfile 总入口直接复用为 agent 入口。其二，**快速反馈环写成默认行为**：Next.js 把 watch 构建（约 1–2s 对比全量约 60s）设为 agent 默认规则，改动前先启动开发服务器，跳过必须显式说明理由[^211^]。必要性有实证支撑：context file 会让 agent 跑更多测试（步骤 +2.4~3.9，成本 +20%），不给廉价的测试粒度（单测试/单文件命令），成本必然失控[^40^]。其三，**把"完成定义"写成命令序列**：官方 FAQ 确认 agent 会主动执行 AGENTS.md 中的检查并修复失败[^34^]，ETH 研究确认工具名被提及即几乎必然被调用[^40^]——因此 build → test → lint → typecheck 的命令序列是杠杆最高的一节，且应与 CI 对齐，使"agent 本地绿 ≡ CI 绿"。此外两个低成本高收益项：环境自我描述（告诉 agent 沙箱能力边界，如 Codex 仓库明示网络禁用变量与早退测试，防止它误改为绕过沙箱而设计的代码）[^223^]；脚手架命令入库（如 `pnpm new-test --args`，agent 便不会手写偏离规范的新文件）[^211^]。

#### 8.4.2 Harness 设计模式迁移：策略与强制分离、审批分级、配置 Schema 化

普通工程项目无需自建 agent 运行时，但六个开源 harness（dsh、opencode、aider、Codex CLI、goose、crush）的横评中有两个模式可直接迁移。第一，**权限与审批机制的共识可直接借用**：这些项目收敛出的共同原则是策略（审批门控）与强制（OS 级沙箱）分离、危险全开模式刻意起吓人的名字，粒度取舍取决于团队对打扰的耐受度[^224^][^225^][^226^][^227^][^228^]；四种权限粒度方案的具体形态详见 9.3.1 节。对普通仓库真正可迁移的是其边界观：AGENTS.md 里写"never modify production config"只是请求而非边界，真正的保证必须落在分支保护、scoped 凭据、egress 限制或工具级 hook 里；安全团队已演示通过 AGENTS.md 注入窃取凭据的攻击，指令文件应作为一等供应链制品纳入评审[^229^][^230^]。第二，**配置 JSON Schema 化**：opencode、crush、Codex 的配置文件均公开 JSON Schema（`"$schema"` 字段），换得 IDE 补全与 CI 可校验[^38^][^66^][^225^]；dsh 另提供 `--dump-config` 导出运行时实际生效的整棵配置树，消除"配置到底谁赢了"的猜测[^33^]。迁移含义：凡供 agent 消费的非 Markdown 配置（权限、工具白名单、模型路由）都应带 Schema 并可检视生效值——agent 读不懂隐含约定，配置错误应在加载期 fail loud 而非静默跳过[^27^]。

最后一条治理纪律回指本章开头：agent 在会话中犯的重复性错误应回写为 AGENTS.md 或 rules 中的永久规则（Claude Code 的 `#` 快捷键即内建实现），而非在聊天中反复纠正[^219^][^209^]；指令文件与代码同 PR 共演化、生成区段由工具刷新——SSOT 三要素（权威位置、派生方式、校验器）在 agent 时代一条不减[^222^][^217^]。

## 9. 用户交互工程

第 8 章讨论了仓库如何被 agent 消费，本章转向程序朝外的另一面：命令行是程序的 UI。本章把 CLI 规范、错误信息与 agent 交互界面放在同一工程视角下考察：三者共享同一条主线——**交互表面也是工程制品，应由定义生成、由测试守护、按风险分级**，而不是散落在代码里的临时 `print`。对同时维护 CLI/TUI 工具与 agent 应用的开发者，本章给出的核心结论是：clig.dev 提供了语言无关的权威交互规范[^231^]；help 文本与手册页应当从命令定义单链生成，杜绝第二份手写文档[^4^][^124^]；错误信息按"说什么、为什么、怎么办"三段式设计并携带可追踪错误码[^232^]；agent 界面在 CLI 规范之上新增权限分级与危险操作呈现两个维度[^224^][^227^]。

### 9.1 CLI 用户体验规范

#### 9.1.1 clig.dev：stdout/stderr 分工、机器可读输出、退出码纪律、--help 自文档化

clig.dev（Command Line Interface Guidelines）是社区维护的开源 CLI 设计指南，其原则不绑定任何语言或框架，已被广泛引用为事实标准[^231^]。与本章相关的核心条目可归纳为六组：

| 维度 | 原则 | 落地要点 |
|---|---|---|
| 输出分工 | 程序输出走 stdout，诊断与进度消息走 stderr | stdout 内容可被管道消费；重定向后 stderr 仍对人可读[^231^] |
| 退出码 | 成功返回 0，失败非 0 | 进阶做法是给退出码分段赋语义，如 Square 的 80–99 表用户错误、100–119 表软件内部错误，使告警系统可按码区分责任方[^233^] |
| 机器可读 | 人类可读为默认，`--json` 提供结构化输出 | 非 TTY 时禁用颜色与动画，尊重 `NO_COLOR`；一切人类输出先考虑管道场景[^231^] |
| Help 自文档化 | 无参数、`-h`/`--help` 必须显示帮助；以示例开头 | 期待管道输入但 stdin 是 TTY 时直接显示帮助；猜测并提示用户可能想输入的命令[^231^] |
| 错误 | 捕获底层错误并重写为面向人类的信息 | 信噪比至上；不可解释的错误给调试信息与提交 bug 的指引[^231^][^232^] |
| 演进与健壮 | 变更尽量 additive，破坏前警告 | 危险操作需确认；secret 不从 flag 读（会进 shell 历史）；长任务显示进度[^231^] |

这六组原则的共同逻辑是"把 CLI 当作有消费者的 API"：stdout 是给下游程序的数据通道，stderr 与退出码是给人类和自动化系统的控制通道，两者混用会同时破坏两种消费者。退出码分段是其中最容易被低估的一条——当工具进入 CI 或脚本后，"失败"不再是二元事件，调用方需要区分"用户输错参数"与"工具自身崩溃"以决定是否重试、告警谁。`--json` 与 `NO_COLOR` 则承认 CLI 的运行环境是不可控的：同一命令可能运行在个人终端、CI 日志、IDE 内嵌终端中，输出格式必须由环境探测与显式 flag 共同决定，而非硬编码。

#### 9.1.2 help 文本从定义生成：help 即文档 SSOT

命令定义应当同时是参数解析器、help 文本与手册页的单一事实来源（SSOT）。Rust 生态的 clap 模式是标杆：derive 类型承载全部参数定义与文档注释，`clap_mangen` 在构建期从同一定义生成 man page，help 文本与手册页因此永不漂移[^4^]。Foundry 仓库将这一原则写进贡献规范："Keep CLI option text in the Rust clap definition; the book's CLI reference is generated from command help and should not be edited by hand"[^124^]。同一模式在各语言均有对应物：Python 的 click/typer 从装饰器定义生成 help，C++ 的 CLI11 从链式定义生成，Go 的 cobra 可生成 man page 与 markdown 文档[^4^]。

对厌恶重复维护的开发者，落地链只有一条：**定义 → help → man/web 文档，单链生成，零手写副本**。生成之外还需要机械校验闭环，两个已被实证的做法是：其一，用快照测试守护 help 文本——对每条命令的 `--help` 输出做快照（如 insta），参数或措辞变更在 code review 中以 diff 形式显式呈现；其二，把 clig.dev 合规性写成自动化测试——遍历命令树检查每条命令具备描述、示例与退出码文档，由 CI 强制[^234^]。这与前文各章的 SSOT 三要素一致：权威位置（命令定义）、派生方式（生成器）、校验器（快照与合规测试）。

### 9.2 错误信息设计

#### 9.2.1 错误即面向用户的文档：三段式与 miette/annotate-snippets

错误信息是用户在最沮丧时刻读到的文档，其质量标准应当不低于 README。可操作的设计框架是三段式：**发生了什么**（用人话而非异常类名）、**为什么**（可理解的直接原因）、**怎么办**（可执行的修复建议）。Node.js CLI 最佳实践指南将其凝练为两条规则：Trackable——错误携带可查文档的错误码（如 `Error (E4002): ...`），使用户能搜索、使维护者能定位；Actionable——"失败的信息应当告诉用户需要什么修复，而不是抱怨出错了"[^232^]。

Rust 生态已把三段式机制化为类型系统级的协议。miette 定义了与 `std::error::Error` 兼容的 `Diagnostic` trait：通过 derive 宏为每个错误声明结构化错误码（`code(my_app::my_error)`）、`help(...)` 修复建议、指向文档的 `url`（支持终端内渲染为可点击链接），以及 `#[source_code]` + `#[label]` 标注的源码片段与行内高亮[^235^]。应用层用 `Report` + `IntoDiagnostic` 转换外部错误；`fancy` feature 提供图形化输出，且官方明确该 feature 只应作用于顶层 crate，库不应把渲染依赖传染给下游[^235^]。其默认 ReportHandler 还处理了两个常被忽视的可访问性细节：错误码在支持的终端转为超链接、`NO_COLOR` 等启发条件下自动切换为屏幕阅读器友好的纯文本叙述输出[^235^]。若不需要 miette 的完整协议，rustc 同款的 annotate-snippets 是更底层的渲染库——cargo 官方正在迁移到它，以统一与 rustc 一致的诊断渲染风格[^236^]，这本身即是对"错误信息质量属于工程质量"的最强背书：连构建工具都把诊断渲染当作一等工程问题。

跨语言落地时，三段式的载体不同但结构不变：Python 用自定义异常携带错误码属性并在顶层 `except` 统一渲染，C++ 在错误类型中携带 code/hint 字段。两条纪律是语言无关的：其一，错误码一旦发布即为公共接口，删改码号视同 SemVer 破坏性变更；其二，调试信息（backtrace、环境快照）默认折叠，通过 `--verbose` 或环境变量展开——面向人类的输出与面向调试的输出分层，对应 clig.dev 的信噪比原则[^231^]。

### 9.3 Agent 交互界面

#### 9.3.1 TUI 流式输出、权限确认分级、危险操作"吓人命名"

Agent 工具在 CLI 规范之上引入了新问题：程序不再一次性输出结果，而是持续执行动作序列，交互界面必须同时回答"它在做什么""我如何打断""哪些动作需要我批准"。对六个主流开源 agent（dsh、opencode、aider、Codex CLI、goose、crush）的调研可提炼出三组已被实证的交互模式[^33^][^237^][^224^][^238^][^239^][^240^]。

**流式输出必须可中断、可回溯。** opencode 在流式输出中支持 `esc` 即时中断，`/undo`、`/redo` 可回滚整个回合（含文件改动）[^237^]；dsh 提供 Trajectory 视图，可逐条回看系统提示、工具调用与决策链，并支持会话分叉与恢复[^241^]。这两条合起来构成 agent 界面的信任基础：用户能随时叫停，且事后能审计 agent 做过什么——对应 dsh 的运行时不变量"模型可见的内容必须已落日志"，fork、resume、telemetry 全部从同一事件流派生[^33^]。

**权限确认按风险分级，而非一刀切。** 六个项目收敛出的共识是**策略与强制分离**：审批门控（approval）是策略层，OS 级沙箱才是执行层强制，两者各有独立旋钮[^224^]。粒度上存在四种已实证的方案：Codex 的双旋钮模型——`sandbox_mode`（read-only / workspace-write / danger-full-access）× `approval_policy`，且 workspace-write 下网络默认关闭[^224^]；opencode 的声明式权限——15 个权限键 × allow/ask/deny 三态 × bash 命令的 glob 模式映射（如 `"git push *": "deny"`），可按 agent 覆盖、最后匹配优先[^226^][^227^]；goose 的 smart_approve——直接利用 MCP 工具注解中的 `read_only_hint` 做风险分类，只读工具自动放行、写操作才打扰人[^238^][^228^]；crush 社区进一步提出 context-token 模型，把复合命令解析为命令与路径令牌分别记忆授权，含 `$()`、重定向、`eval` 的结构一律 fail-closed[^242^]。粒度的选择取决于用户对打扰的耐受度：逐次确认最安全但打断心流，smart_approve 式"用现成元数据做风险分类"是减少打扰的折中。另一个降低理解成本的模式是 opencode 的 plan/build 双 agent 切换：只读规划与可写执行分离，`Tab` 一键切换，比细读权限配置更符合直觉[^237^]。

**危险操作的呈现三件套与"吓人命名"。** 对不可自动放行的操作，实证的呈现组合是：内联确认弹窗 + 变更 diff 预览（crush 在编辑前先展示 diff 再应用）+ 会话级或规则级的授权记忆[^227^][^239^]；aider 则把回滚外包给 git——每次编辑自动提交，`/undo` 即 revert，零额外存储换来完整审计[^240^]。而对"移除一切护栏"的逃生门，多个项目刻意使用带警告意味的命名：Codex 的 `--yolo` 只是 `--dangerously-bypass-approvals-and-sandbox` 的别名[^224^]，crush 的 `--yolo` 在文档中被连用两个 "very" 警告[^239^]。这一命名纪律的工程含义是：危险能力必须存在（自动化场景需要），但其名称本身即文档——出现在脚本、命令历史与 code review 中时，任何人都能一眼识别风险等级，无需查阅手册。

## 10. 三语言落地速查与决策指南

前九章分别论证了"为什么"与"是什么"，本章把它们压缩成可以直接照做的执行层。三张语言速查表按统一四元组组织——**工程维度 → 推荐工具/机制 → SSOT 位置（该事实唯一存放的文件）→ 详见章节**——其中"SSOT 位置"一列是全章的灵魂：任何一条事实若需要写到第二处，就先回到第 1 章的"生成或引用"判定规则处理。10.4 的决策表则处理速查表无法覆盖的部分：12 个存在真实分歧的选型点（Conflict Zone），按项目场景给出裁决依据而非唯一答案。三张速查表之间有意识保持了同构——每个语言栈都覆盖"骨架/依赖/lint/测试/矩阵/发布/可信凭据"同一组维度，混合仓读者可以逐行横向对齐，发现三栈的 SSOT 文件链各自闭合、职责零重叠。

### 10.1 Rust 落地清单

#### 10.1.1 workspace.dependencies + workspace.lints + nextest + insta + release-plz + cargo-dist 工具栈速查表

| 工程维度 | 推荐工具/机制 | SSOT 位置 | 详见 |
|---|---|---|---|
| 骨架与布局 | 根虚拟清单 + `crates/*` 扁平布局，目录名 == crate 名；要发布的 crate 单独放 `libs/`[^43^] | 根 `Cargo.toml` | §2.1 |
| 依赖版本统一 | `[workspace.dependencies]` 声明一次，成员 `workspace = true` 继承[^2^][^15^] | 根 `Cargo.toml` | §2.1 |
| lint 政策 | `[workspace.lints]` 统一 clippy/rust lint，成员 `[lints] workspace = true`；CI `clippy --all-targets -- -D warnings`[^243^] | 根 `Cargo.toml` | §2.1 |
| 工具链版本 | rustup 与 flake 双消费同一文件（rust-bin.fromRustupToolchainFile）[^6^] | `rust-toolchain.toml` | §3.2 |
| 任务层 | 自动化全部写进 `cargo xtask`，消灭零散 shell/Makefile[^97^] | `xtask/` crate | §4.2 |
| 测试运行器 | cargo-nextest（process-per-test，Windows 收益尤其大；分片 `--partition slice:m/n`、归档复用构建产物）[^145^][^146^][^244^] | `.config/nextest.toml` | §5.4 |
| 大输出断言 | insta 快照 + `cargo insta review` 人工批准；CI 只读[^10^] | `*.snap`（入库评审） | §5.2 |
| 编译期/UB 兜底 | trybuild（compile-fail）、miri（unsafe crate 开 nightly job）、loom（并发原语）[^147^][^245^][^31^] | `tests/ui/`、CI job | §5.4 |
| 依赖政策 | cargo-deny 四检查：advisories/licenses/bans/sources[^246^] | `deny.toml` | — |
| API 破坏防线 | cargo-semver-checks（仅 2025 年就新增 122 条 lint，发布前必跑）[^58^] | CI 配置 | §2.2 |
| 发布自动化 | release-plz 开 Release PR（人点 merge），二进制配 cargo-dist 出跨平台安装器[^17^][^196^] | `release-plz.toml`、`dist-workspace.toml` | §7.1 |
| 可信凭据 | crates.io Trusted Publishing（OIDC），删除长期 token，可开 TP-only 模式[^187^][^188^] | workflow `permissions` | §7.2 |
| CI 缓存 | Swatinem/rust-cache，key 按 lockfile/toolchain/target 自动隔离[^144^] | workflow | §5.4 |

这张表的主线是**根 `Cargo.toml` 一个文件承担三种 SSOT 角色**：依赖版本、包元数据、lint 政策全部以 workspace 级表声明、成员继承的方式收敛，继承语义只允许追加 `features`/`optional`、不允许覆盖版本，从机制上杜绝了"成员私自改版本"的漂移路径[^15^]。测试侧的选型逻辑是"执行模型优先"：nextest 的 process-per-test 不只是提速，更带来故障隔离与 CI 分片/归档两项结构化能力，这是它取代 `cargo test` 成为默认的真正原因[^146^][^244^]。发布侧三件套（release-plz + cargo-dist + Trusted Publishing）共享同一条纪律——机器准备、人点闸门、凭据分钟级存活；首次发布必须手动（Trusted Publisher 只能绑定已存在 crate），这是唯一允许的人工例外[^187^]。cargo-hakari（workspace-hack 统一 feature 解析，累计提速约 1.7x）未列入默认栈：只有 workspace 膨胀到构建时间成为瓶颈时才值得引入，且发布到 crates.io 需特殊处理[^247^]。

### 10.2 Python 落地清单

#### 10.2.1 pyproject.toml SSOT + uv workspace + ruff + pytest + Trusted Publishing 工具栈速查表

| 工程维度 | 推荐工具/机制 | SSOT 位置 | 详见 |
|---|---|---|---|
| 骨架与配置 | PEP 621 `[project]` + 全部 `[tool.*]` 集中；src layout + `tests/` 在 src 之外[^248^][^249^][^250^] | `pyproject.toml` | §2.1 |
| monorepo | uv workspace + `[tool.uv.sources]`，成员间依赖自动 editable；requires-python 取交集、无依赖隔离[^45^] | 根 `pyproject.toml` | §2.1 |
| 依赖锁定 | uv.lock 为 universal lockfile（单文件跨平台），必须提交、绝不手改；CI `uv sync --locked` 防漂移，部署用 `--frozen`[^80^] | `uv.lock` | §3.1 |
| 统一执行器 | 一切命令经 `uv run`（本地/CI/任务文件同一入口）[^251^][^252^] | justfile | §4.2 |
| 依赖分组 | 运行时 extras → `[project.optional-dependencies]`；开发依赖 → PEP 735 `[dependency-groups]`，同一依赖只出现一次[^253^] | `pyproject.toml` | — |
| lint/格式化 | ruff 一家替代 black+isort+flake8+pyupgrade；`select` 显式声明规则集[^250^] | `[tool.ruff]` | §4.3 |
| 类型检查 | 编辑器用 ty（快 10–60x），CI 用 mypy `strict = true` 或 pyright（符合度：pyright 96.8%、pyrefly 97.9%，均高于 ty 的 86.5%）[^254^][^255^][^256^] | `[tool.mypy]` 等 | §5.4 |
| 测试框架 | pytest：fixture 只放 conftest 靠发现注入、永不 import fixture；parametrize 堆叠代替重复函数[^151^][^152^] | `[tool.pytest.ini_options]` | §5.4 |
| 断言补强 | syrupy 快照（`__snapshots__/` 必提交）+ hypothesis 属性测试（roundtrip/不变量/幂等）[^128^][^120^] | `__snapshots__/` | §5.2 |
| 覆盖率门 | `[tool.coverage.run] branch = true` + `fail_under` 只写 pyproject，CLI 不重复传阈值[^116^][^117^] | `[tool.coverage.*]` | §5.1 |
| 版本矩阵 | nox + `nox.project.python_versions()` 从 pyproject 读矩阵，或纯 CI matrix——只定义一次[^257^] | `noxfile.py` 或 workflow | — |
| 发布自动化 | conventional commits → release-please 管 CHANGELOG/tag；注意 uv_build 后端不支持 dynamic version，tag 推导需换 hatchling + uv-dynamic-versioning[^177^][^18^][^258^] | `release-please-config.json` | §7.1 |
| 可信凭据 | tag 触发 + 双 job（build 无凭据 / publish `id-token: write`），PyPI Trusted Publishing 零 token[^191^][^259^] | workflow `permissions` | §7.2 |

Python 栈的收敛度是三语言中最高的：`pyproject.toml` 同时是包元数据、依赖声明与多家工具的配置中心，SSOT 纪律具体化为两条可执行规则——覆盖率阈值只写 `[tool.coverage.report]` 不在 CLI 重复[^117^]，同一依赖不在 extras 与 dependency-groups 出现两次（canvas-mcp 项目曾因两处定义 dev 依赖且版本偏移而付出调试成本，这是"重复即腐烂"的直接实证）[^260^]。类型检查是表中唯一的"双工具"格：性能与规范符合度在 2025–2026 年呈反向分布（ty 快 10–60 倍但符合度 86.5%，pyright 96.8%），因此编辑器/CI 分工是当前最优折中，但各家抑制注释语法不同，多检查器并行要付出维护成本，beta 格局稳定后应回到单一检查器[^254^]。发布侧的陷阱在构建后端：`uv_build` 要求静态版本、拒绝 `dynamic = ["version"]`，要 tag 推导版本就必须换 hatchling——这个限制决定了"版本 SSOT 是 tag 还是 pyproject"的架构选择题[^177^]。

### 10.3 C++ 落地清单

#### 10.3.1 CMakePresets.json SSOT + nixpkgs/vcpkg + GTest + sanitizer 矩阵 + clang-tidy 检入工具栈速查表

| 工程维度 | 推荐工具/机制 | SSOT 位置 | 详见 |
|---|---|---|---|
| 配置中心 | CMakePresets.json 检入为 SSOT（hidden 基座 + inherits 消重 + condition 分平台）；CMakeUserPresets.json 进 .gitignore；CI 每格只调一个 `--preset`[^52^] | `CMakePresets.json` | §2.1 |
| 构建纪律 | 一切 target 化并显式 PUBLIC/PRIVATE/INTERFACE；禁全局命令；影响 ABI 的选项全局统一[^26^][^59^] | `CMakeLists.txt` | §2.2 |
| 依赖管理 | flake 环境首选 nixpkgs；需服务原生 Windows 非 Nix 用户时迁 vcpkg manifest（baseline 锁 registry commit）；中小项目备选 CPM（GIT_TAG + URL_HASH）[^49^][^50^][^51^] | `flake.nix` 或 `vcpkg.json`（二选一） | §3.3 |
| 测试框架 | 大项目 GoogleTest、中小 Catch2/doctest；`gtest/catch_discover_tests` 接入 CTest，交叉编译设 `DISCOVERY_MODE PRE_TEST`[^261^][^262^][^263^] | `CMakeLists.txt` + test preset | §5.4 |
| 内存/UB 防线 | sanitizer 矩阵写进 presets：PR lane = ASan+UBSan、独立 TSan lane、禁巨型单 job[^148^] | `CMakePresets.json` | §5.4 |
| 不可信输入 | libFuzzer harness（`LLVMFuzzerTestOneInput`）；够格接 OSS-Fuzz，否则 ClusterFuzzLite 进自家 CI[^149^][^150^][^264^] | `fuzz/` + CI | §5.4 |
| 代码规范 | `.clang-format`/`.clang-tidy` 检入仓库根；CI 用 clang-tidy-diff 只查增量，新 check 渐进启用[^265^][^266^] | `.clang-format`、`.clang-tidy` | §4.3 |
| 编译缓存 | ccache/sccache 经 `CMAKE_*_COMPILER_LAUNCHER` 接线，缓存 key 按 matrix 维度隔离；Windows 优先 sccache[^143^][^267^] | preset `cacheVariables` | §5.4 |
| 工具链枢纽 | `CMAKE_EXPORT_COMPILE_COMMANDS=ON` 写入 preset；compile_commands.json 喂 clangd/clang-tidy/IWYU/include-cleaner[^268^] | preset `cacheVariables` | §2.2 |
| 模块边界 | 目录即库、"假想 Unix 链接器"自测分层；clang-tidy `misc-include-cleaner` 进 presubmit[^24^][^55^] | `CMakeLists.txt` 依赖声明 | §2.2 |
| 版本政策 | API/ABI 双轨明文化：abseil 式"不承诺 ABI"或 soname/SOVERSION 纪律；依赖版本全局唯一防菱形 ODR[^59^][^269^] | `project(VERSION)` + 文档 | §7.1 |
| 库分发 | 编译版 + header-only 双 target（fmt 模式）；先做好 `find_package` 包配置，vcpkg/Conan/CPM 五路自然可用[^198^] | `CMakeLists.txt` | §7.3 |

C++ 栈与其他两栈的最大差异是**不存在官方包管理器**，因此"SSOT 文件链"在这里是多段拼接：flake.nix 管工具链供给、CMakePresets.json 管项目配置、vcpkg.json（或 nixpkgs）管库依赖、`.clang-*` 管代码规范——四者各为单一事实源、职责正交零重叠，这正是 LLVM 与 Conan 2 都把 CMakePresets 选作交接面的原因[^52^][^87^]。依赖一格的"二选一"不是偷懒：跨维度交叉验证的裁决明确"两者取一作为库依赖 SSOT，绝不同时维护两份同构清单"，Nix 主力路线用 nixpkgs，原生 Windows 刚需才迁 vcpkg[^49^][^51^]。sanitizer 矩阵的分 lane 设计是对"慢而被忽略"这一失败模式的直接回应——一个又慢又 flaky 的巨型 sanitized job 等于没有 sanitizer[^148^]。ABI 一栏提醒：C++ 的 semver 必须拆成 API 与 ABI 两个维度，选 abseil 式源码同构还是 soname 式 ABI 版本纪律，必须在文档里写死，否则菱形依赖会在链接期以 ODR 违反的形式讨债[^59^]。

### 10.4 冲突区决策树

#### 10.4.1 12 个 Conflict Zone 的选型决策：按场景查表，而非选边站队

速查表给的是无争议的默认栈；以下 12 个选择点在标杆项目间存在真实分歧，跨维度交叉验证的结论是一致的——**分歧几乎都可通过"场景分层"调和**，下表按触发场景给出裁决：

| # | 冲突区 | 各方主张 | 按场景的裁决 | 详见 |
|---|---|---|---|---|
| C1 | 任务层载体 | xtask 派（类型安全、消 YAML 重复）[^97^]；just 派（语言无关、`--list` 即活文档）[^223^] | 分层不互斥：混合仓 justfile 做总入口，需条件逻辑/生成/解析的下沉 xtask；纯 Rust 单仓 xtask 自足；Windows 重逻辑避开 shell 用 xtask | §4.2 |
| C2 | 测试形状 | Trophy/集成优先[^112^]；金字塔单元基座[^115^]；覆盖率门（100% 死代码探测 vs fail_under=80 防退化）[^61^][^117^] | 形状是架构的函数：纯逻辑→金字塔、应用→trophy、微服务→honeycomb；库代码取"未覆盖即删"、应用取 80% 底线 | §5.1 |
| C3 | C++ 依赖 | vendored/pinned（LLVM/Chromium）；包管理器（vcpkg/Conan/CPM）；nixpkgs[^88^][^90^][^49^] | Nix 环境首选 nixpkgs；原生 Windows 刚需迁 vcpkg.json 为 SSOT；deps rolls 是大团队特权，中小项目不模仿形式只学"钉版本"思想 | §3.3 |
| C4 | AGENTS.md 详略 | ETH 实证：context file 平均降成功率、增成本 >20%[^40^]；重度派：dsh/Zed 数百行[^27^][^37^] | 价值 = 增量信息密度而非长度；每条规则问"agent 不看这条会犯错吗"，答否则删；LLM 自动生成不裁剪有害 | §8.2 |
| C5 | AGENTS.md 内容 | 写架构派（规范/结构高频主题）[^204^]；不写概览派（概览章节实证无效）[^40^] | 区分"架构概览"（描述现状，删）与"架构约束"（禁令+理由+强制它的 lint 出处，必须写）；概览交给 agent 探索 | §8.2 |
| C6 | 发布自动化 | 全自动 semantic-release[^181^]；Release PR 人闸[^17^]；本地命令式 cargo-release[^270^] | 默认 Release PR（机器准备+可审计）；全自动限迭代极快且下游容忍度高的工具；低频发布用本地命令式 | §7.1 |
| C7 | spec-first vs code-first | spec 为意图 SSOT[^158^]；code 为 SSOT + 契约测试兜底[^174^] | 疆域划分：对外契约（HTTP API/扩展 ABI）偏 spec-first，内部模块偏 code-first；二者不互相重复，靠 diff/doctest/契约测试防漂移 | §6.4 |
| C8 | pre-commit 与 CI | 同源派（CI 跑同一配置）[^94^]；质疑派（hook rev 与工具版本双 pin 漂移）[^271^] | CI 只读模式跑同一份 `.pre-commit-config.yaml`；工具版本只 pin 一次（`language: system` 调 PATH 二进制，版本归 mise/uv/flake 管）；测试矩阵不进钩子层 | §4.3 |
| C9 | 生成物入库 | 不入库派（repo 只含原始材料）[^9^]；入库+校验派（快照/workflow YAML 入库）[^1^][^10^] | 判据 = 消费者是否需不构建即可读：CI YAML、快照、CHANGELOG 入库 + drift check；文档站点/man page 可不入库；两派共享"生成物绝不手改" | §1.1 |
| C10 | header-only vs compiled | fmt 双 target[^198^]；abseil 不承诺 ABI[^59^]；soname 派[^269^] | 库作者默认 fmt 模式（双 target 消费者自选）；应用侧守 abseil 纪律（ABI 选项全局统一）；只有 distro 级分发才需 soname 纪律 | §7.3 |
| C11 | Nix 深浅集成 | 浅集成：flake 只供工具链，语言 lockfile 为 SSOT[^71^]；深集成：uv2nix 翻译成 Nix 构建图[^74^] | 不矛盾，按产物分层：开发期一律浅集成；触发深集成的唯一条件是想 `nix build` 出 bit 级复现的发布产物 | §3.1 |
| C12 | agent 权限粒度 | Codex 双旋钮[^224^]；opencode 15 键三态[^226^]；goose 风险分类自动放行[^228^] | 共识大于分歧：策略与强制分离、危险模式起吓人的名字；粒度按团队被打扰耐受度选，"用元数据做风险分类"是减少打扰的最优折中 | §8.4 |

这张决策表的用法是"先定位场景，再抄裁决"，三个反复出现的元规律值得单独点出。其一，**分层调和占多数**：C1/C3/C11 的"之争"其实都是不同层级的职责划分——just 与 xtask 是门面与实现、nixpkgs 与 vcpkg 是环境路线与降级通道、浅/深 Nix 集成是开发期与发布期，选边站队在这些格子本身就是错误动作。其二，**实证研究修正实践直觉**：C4/C5 中 ETH 的负收益实证（context file 平均降低 agent 成功率、增成本逾 20%[^40^]）与 Lulla 的正收益实证（根 AGENTS.md 省 token[^213^]）方向相反，调和键是"增量信息密度"——人工精简、只写 agent 无法从代码推断的内容才有效，这把 AGENTS.md 写作从"写得全不全"的品味问题变成"每条规则能否通过犯错测试"的机械判据。其三，**判据优先于结论**：C9 的"消费者是否需不构建即可读"、C7 的"对外契约 vs 内部模块"、C2 的"库 vs 应用"，都是可复用的判定函数——记住判据比记住答案更重要，因为工具版本会变、场景特征不变。第 11 章的采纳路线图将把这些裁决按采纳成本排成可执行顺序。

## 11. 采纳路线图

前十章的规范全集对个人开发者与小团队而言体量可观，一次性落地几乎必然以"配置写了一半、纪律无人维持"收场。本章把全部落地项按采纳成本重排为三层，并为每层给出可机械检查的验收标志。排序依据两条原则：其一，**先建立单一事实来源（SSOT）文件，再建立消费它的自动化**——后续每层只是"多一个消费方"而非"多一份拷贝"；其二，**每层落地后仓库都处于可交付状态**，在任何一层停下，已落地部分都独立成立。

### 11.1 按采纳成本排序

#### 11.1.1 零成本层：pre-commit/prek、rust-toolchain.toml、.python-version、conventional commits

零成本层的判据是：每项投入半小时以内、纯增量、不改变现有工作流，共同形态是**向仓库根目录提交一份声明式文件，由现成工具自动消费**。Rust 项目写 `rust-toolchain.toml` 声明 channel 与组件，rustup 自动按它切换工具链；该文件后续可被 Nix flake 通过 `rust-bin.fromRustupToolchainFile` 直接读取，一份文件同时服务 rustup 用户与 Nix 用户[^6^][^70^]。Python 项目用 `uv python pin` 生成 `.python-version`，配合 pyproject 的 `requires-python` 构成版本 SSOT[^71^][^78^]。这两个文件是第 3 章环境路线的公共子集，先写不会沉没成本。C++ 没有语言级的版本钉死文件，对应的零成本动作是把 `CMakePresets.json` 与 `.clang-tidy` 检入仓库根，作为构建与诊断配置 SSOT（展开见第 10 章）。提交 `.pre-commit-config.yaml` 并用 prek 安装本地钩子，即获得提交前的格式化与基础检查拦截；prek 与原版 pre-commit 读同一份配置可随时互换，CPython、Apache Airflow、FastAPI 与 Astral 的 ruff 仓库均已采用[^105^][^14^]。同步把提交信息切换为 Conventional Commits（`fix`→PATCH、`feat`→MINOR、`!`→MAJOR），其官方首要用途即自动生成 changelog 与推导版本号[^165^]——零工具成本，却是第 7 章发布自动化的结构化输入，越早开始历史越完整。

#### 11.1.2 低成本层：just 总入口、CI 薄化、快照测试、AGENTS.md 初版、Diátaxis 文档分层

低成本层的判据是：每项半天到一天完成、完成后立刻降低日常摩擦，主题是把零成本层建立的 SSOT **接上调用方**。用 justfile 收敛命令入口：just 是单二进制的语言无关任务运行器，`just --list` 即活文档，Windows 上以 `set shell := ["powershell.exe", "-c"]` 或 shebang recipe 规避 bash 依赖[^100^][^101^]。随后把 CI workflow 内联的 `run:` 脚本替换为对任务层的单行调用（`just ci`、`uvx prek run --all-files`），CI YAML 退化为只做触发器、矩阵与权限的薄编排层[^12^]。测试侧为 CLI 输出、诊断信息等大输出断言引入快照测试：期望值成为入库评审的 `.snap` 数据文件而非代码中的字符串墙，CI 只读不自动更新[^119^][^10^]。文档侧做两件事：按 Diátaxis 四分法（教程/操作指南/参考/解释）给既有文档归位[^154^]；写第一版 AGENTS.md，只写 agent 无法从代码推断的增量信息（命令入口、禁令、非标准工具链约定）——实证研究表明人写且精简的 context file 提升任务成功率，LLM 自动生成的反而平均降低成功率[^40^][^213^]。

#### 11.1.3 投资层：flake+direnv 全量、xtask、release-plz+OIDC、确定性测试基建、架构 lint

投资层的判据是：单项需数天投入或持续的纪律维持，回报体现在环境一致性、发布可信度与架构防腐，适合在有第二个协作者或首次对外发布之前完成。环境与 CI 收敛到 flake 全量模式：`.envrc` 一行 `use flake`，`flake.lock` 入库即工具链与系统依赖 SSOT；CI 用 `nix-installer-action` + `magic-nix-cache-action` 后直接 `nix develop -c <cmd>`，环境描述零重复[^69^][^272^][^13^][^68^]。跨平台现实须正视：Nix 无原生 Windows 支持，需为 Windows 成员保留 rustup/uv 降级通道——因版本 SSOT 同时被两条路线消费，该通道不产生第二份版本声明[^6^][^71^]。C++ 侧则按第 3 章的裁决在 nixpkgs 与 vcpkg 清单之间二选一，一旦选定即把另一份依赖清单彻底移除，不允许双份并存。自动化逻辑超出 just recipe 表达能力时，用 xtask 把任务下沉为宿主语言代码：该模式由 matklad 于 2018 年提出、由 cargo-xtask 仓库固化为规范[^97^][^98^]。发布侧引入 release-plz 自动产出 Release PR 与 changelog[^17^]，并把包发布迁移到 OIDC Trusted Publishing：在 registry 端按仓库与 workflow 文件名注册 Trusted Publisher，CI 以 `id-token: write` 换取短期令牌（crates.io 约 30 分钟、PyPI 约 15 分钟），删除长期 token[^187^][^188^]。测试侧为含时间、随机、IO 的模块建立确定性基建：种子可注入、失败必打印、可凭环境变量复现[^133^][^273^]。架构侧把层级规则编译进工具：C++ 用 clang-tidy `misc-include-cleaner` 在 presubmit 强制 IWYU 式头文件纪律[^55^]，Rust 可用 Dylint 编写项目自有 lint[^22^]——找不到机械强制点的架构规则必然腐烂。

### 11.2 验收标准

每层落地不是"配置文件已提交"，而是**一组可被任意第三方（包括 agent）执行的检查全部通过**。下表汇总三层的采纳范围、成本量级与验收标志。

| 层级 | 落地项目 | 成本量级 | 验收标志 |
|---|---|---|---|
| 零成本层 | `rust-toolchain.toml`、`.python-version`、`CMakePresets.json` + `.clang-tidy` 检入（C++）、`.pre-commit-config.yaml`（prek）、Conventional Commits | 每项 ≤0.5 小时 | 干净机器 clone 后 rustup/uv 自动选中钉死版本；`prek run --all-files` 全绿；git log 最近提交全部符合 type 规范 |
| 低成本层 | justfile 总入口、CI 薄化、快照测试、AGENTS.md 初版、Diátaxis 归位 | 每项 0.5–1 天 | 本地 `just ci` 与 CI 跑同一入口且结果一致；快照随代码入库、CI 只读校验；AGENTS.md <200 行且只含增量信息 |
| 投资层 | flake+direnv 全量、xtask、release-plz+OIDC、确定性测试基建、架构 lint | 每项数天 + 持续纪律 | `nix develop -c just ci` 在 CI 与本地等价；发布流程无长期 token；随机测试失败可凭种子一键复现；越层依赖被 lint 在提交前拦截 |

读表需把握一条取舍主线：成本列按"首次落地"计量，真正的差异在维护侧——零成本层的文件几乎免维护，投资层每一项都要随项目演化持续投入（flake.lock 定期更新、lint 规则随架构调整、种子回归库随失败累积）。因此层的推进应以触发条件而非"有空就做"驱动：第二个协作者加入是 flake 的触发点，首次对外发布是 OIDC 的触发点，首次出现"越层依赖在 review 中漏网"是架构 lint 的触发点。对单人项目，长期停留在低成本层是合理的终态；投资层的价值在于协作规模与发布频率放大时，把隐性的人工纪律换成显性的机械强制。

#### 11.2.1 每层落地的可检查标志：本地绿≡CI 绿、clone 后三条命令跑通、文档零手抄事实

三条跨层通用的验收判据，各自对应本书的一条主线。**本地绿≡CI 绿**：同一份 `.pre-commit-config.yaml` 同时驱动本地钩子与 CI 检查[^14^]，CI 的 `run:` 只调用任务层入口[^12^]，"CI 挂了本地没法调"的调试循环由此在结构上被消灭；判定方法是把 CI 日志中的每条命令在本地逐条执行，结果必须逐条一致。**clone 后三条命令跑通**：干净机器 clone 后，应在三条命令以内（本模板实测两条：`direnv allow`、`just ci`——git 钩子由 devShell 进入时自动安装）完成从源码到全量检查通过；该判据同时是 agent 可用性测试——AGENTS.md 中列出的命令会被 agent 当真执行[^34^]，跑不通的命令对人和 agent 同为失修的文档。**文档零手抄事实**：文档中不出现可从代码生成的事实副本（版本号、命令清单、help 文本、API 签名）；判定方法是抽查文档中每条命令与数字并追问其权威位置，答不上来即违规。

放眼演进方向，这三条判据的分量只会增加。2025 年 12 月 Linux Foundation 宣布成立 Agentic AI Foundation，AGENTS.md 由 OpenAI 捐赠成为创始项目之一，agent 指令文件正从社区惯例走向基金会级开放标准[^35^][^36^][^202^]。当 agent 成为仓库的常驻协作者，仓库工程的评价标准将从"人能不能看懂"转向"人机双读、机器可执行验收"：规范写成 lint 与 schema 而非散文，验收写成命令序列而非口头约定，知识写成增量信息而非文档重复。按本路线图推进的仓库恰好站在这一方向的起点上——每层验收标志不仅是给人的完成定义，也是给 agent 的可执行契约。


# 参考文献

[1] zed-industries/zed. .github/workflows/run_tests.yml 及 workflow 目录[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/tree/main/.github/workflows
[2] The Cargo Book. Workspaces[EB/OL]. https://doc.rust-lang.org/cargo/reference/workspaces.html
[3] git-cliff. 官方站点[EB/OL]. (2026-08-26). https://git-cliff.org/
[4] "How to easily create a CLI in Rust using clap and clap_mangen"（dev.to，2025-09-09）[EB/OL]. https://dev.to/0xle0ne/how-to-easily-create-a-cli-in-rust-using-clap-and-clapmangen-37g1
[5] yurukusa gist. Does Claude Code read AGENTS.md? No（import/symlink 模式）[EB/OL]. (2026-08-22). https://gist.github.com/yurukusa/d36197848911f025add142abefcde685
[6] helix-editor/helix. flake.nix 原文（master 分支，访问于 2026-08；fromRustupToolchainFile、inputsFrom、nixConfig.extra-substituters）[EB/OL]. https://raw.githubusercontent.com/helix-editor/helix/master/flake.nix
[7] Michael Nygard. "Documenting Architecture Decisions"[EB/OL]. (2011-11-15). https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions
[8] The rustdoc book. "Documentation tests"[EB/OL]. (2026-08-26). https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html
[9] markdoc/markdoc Discussion #448. "Internal vs external documentation"（Stripe 工程师评论）[EB/OL]. (2023-09-25). https://github.com/markdoc/markdoc/discussions/448
[10] docs.rs. insta[EB/OL]. (2026). https://docs.rs/insta
[11] zed-industries/zed. docs/AGENTS.md[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/docs/AGENTS.md
[12] Galaxy Project, "A repository for both humans and AI agents", 2026-06-11[EB/OL]. https://galaxyproject.org/news/2026-06-11-a-repository-for-both-humans-and-ai-agents/
[13] DeterminateSystems/magic-nix-cache-action README（GitHub；30-50% CI 提速、零配置、仅 CI 可用）[EB/OL]. https://github.com/DeterminateSystems/magic-nix-cache-action
[14] astral-sh/ruff CI workflow（uvx prek run --all-files、prek 缓存、SHA pinning 实例）[EB/OL]. (2026-08). https://github.com/astral-sh/ruff/actions/runs/24015969375/workflow
[15] The Cargo Book. Specifying Dependencies(含 cargo#12162 讨论)[EB/OL]. https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
[16] "Automating Elixir Releases with Release Please"（Elixir School，2025-07-31）[EB/OL]. https://elixirschool.com/blog/managing-releases-with-release-please
[17] release-plz(仓库与文档;用法印证:fg-labs/redskull README)[EB/OL]. (2026). https://github.com/release-plz/release-plz
[18] Theodo blog. Release Please: automation of your GitHub releases[EB/OL]. (2026-01-20). https://www.theodo.com/blog/automate-your-github-releases-with-release-please
[19] GitHub Docs. "Managing a merge queue"（2026-03-10 访问）[EB/OL]. https://docs.github.com/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue
[20] zed-industries/zed. .github/workflows/docs_suggestions.yml, script/docs-suggest, script/docs-strip-preview-callouts[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/.github/workflows/docs_suggestions.yml
[21] zed-industries/zed. renovate.json[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/renovate.json
[22] zed-industries/zed. script/clippy, .cargo/ci-config.toml, tooling/lints[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/script/clippy
[23] rust-analyzer. Architecture (dev guide)[EB/OL]. (2026). https://github.com/rust-lang/rust-analyzer/blob/master/docs/dev/architecture.md
[24] LLVM Coding Standards. Library Layering[EB/OL]. (2026-02). https://llvm.org/docs/CodingStandards.html
[25] LLVM Discourse. MLIR Project Charter and Restructuring（layering violation 实证与最小可链接测试手段）[EB/OL]. (2024-11). https://discourse.llvm.org/t/rfc-mlir-project-charter-and-restructuring/82896
[26] Effective Modern CMake（mbinna gist，社区权威清单）[EB/OL]. (2026-02). https://gist.github.com/mbinna/c61dbb39bca0e4fb7d1f73b0d66a4fd1
[27] dsh AGENTS.md(仓库根)[EB/OL]. (2026-08-13). https://github.com/deepseek-ai/deepseek-harness/blob/master/AGENTS.md
[28] Baltes & Treude. Context Rot in AI-Assisted Software Development, arXiv:2606.09090[EB/OL]. (2026-06-08). https://arxiv.org/abs/2606.09090
[29] Martin Fowler. "Legacy Seam"（2024-01-04）[EB/OL]. https://martinfowler.com/bliki/LegacySeam.html
[30] zed-industries/zed. crates/gpui README (TestAppContext / #[gpui::test])[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/tree/main/crates/gpui
[31] tokio-rs/loom[EB/OL]. (2026). https://github.com/tokio-rs/loom
[32] proptest-testing SKILL.md（含 proptest 官方 README 摘录，2026-05-02 访问）[EB/OL]. https://github.com/testland/qa/blob/main/plugins/qa-property-based/skills/proptest-testing/SKILL.md
[33] dsh docs/architecture.md[EB/OL]. (2026-08-13). https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md
[34] AGENTS.md 官方站点（规范+FAQ+示例）[EB/OL]. (2026-08-26). https://agents.md/
[35] AAIF Projects 官方页[EB/OL]. (2026-08-26). https://aaif.io/projects
[36] Agentic AI Foundation GitHub org[EB/OL]. (2026-08-26). https://github.com/aaif
[37] zed-industries/zed. AGENTS.md, .agents/skills/, REVIEWERS.conl[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/AGENTS.md
[38] opencode Docs. Config[EB/OL]. (2026-08-26). https://opencode.ai/docs/config/
[39] dsh Discussion #1629(插件脚手架 RFC + CI 冒烟)[EB/OL]. (2026-08-15). https://github.com/deepseek-ai/deepseek-harness/discussions/1629
[40] Gloaguen et al. (ETH Zürich): Evaluating AGENTS.md. Are Repository-Level Context Files Helpful for Coding Agents?, arXiv:2602.11988[EB/OL]. (2026-02-12). https://arxiv.org/abs/2602.11988
[41] zed-industries/zed. Cargo.toml (workspace root)[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/Cargo.toml
[42] Zed Blog. Zed Decoded: Rope & SumTree (2024-04-23)[EB/OL]. (2026-08-26). https://zed.dev/blog/zed-decoded-rope-sumtree
[43] matklad. Large Rust Workspaces[EB/OL]. (2021-08-22). https://matklad.github.io/2021/08/22/large-rust-workspaces.html
[44] SYSTEMF @ EPFL. Adding lookbehinds to rust-lang/regex(regex crate 分层)[EB/OL]. (2025-07-15). https://systemf.epfl.ch/blog/rust-regex-lookbehinds/
[45] Astral Docs. Using workspaces | uv[EB/OL]. (2026-08-14). https://docs.astral.sh/uv/concepts/projects/workspaces/
[46] Mainmatter. cargo-autoinherit: DRY up your workspace dependencies[EB/OL]. (2024-03-18). https://mainmatter.com/blog/2024/03/18/cargo-autoinherit/
[47] RomarQ/cargo-workspace-inheritance-check(引 clippy#10306)[EB/OL]. (2026). https://github.com/RomarQ/cargo-workspace-inheritance-check
[48] Astral Docs. Migrating from pip to a uv project[EB/OL]. (2026-07-23). https://docs.astral.sh/uv/guides/migration/pip-to-project/
[49] Microsoft Learn. vcpkg Manifest mode[EB/OL]. (2024-07). https://learn.microsoft.com/en-us/vcpkg/concepts/manifest-mode
[50] Microsoft Learn. vcpkg.json Reference[EB/OL]. (2024-07). https://learn.microsoft.com/en-us/vcpkg/reference/vcpkg-json
[51] CPM.cmake GitHub README[EB/OL]. (2025-05). https://github.com/cpm-cmake/CPM.cmake
[52] CMake 官方手册 cmake-presets(7)[EB/OL]. (2026-02). https://cmake.org/cmake/help/latest/manual/cmake-presets.7.html
[53] LLVM Coding Standards（13.0.1 版，LLVMBuild.txt 依赖声明载体）[EB/OL]. (2022-02). https://releases.llvm.org/13.0.1/docs/CodingStandards.html
[54] Bazel 官方文档. Dependency Management（strict transitive dependency 经验）[EB/OL]. (2026-02). https://bazel.build/basics/dependencies
[55] LLVM Discourse RFC. Tweak header inclusion policy to promote IWYU（header standalone + clang-tidy include-cleaner）[EB/OL]. (2024-09). https://discourse.llvm.org/t/rfc-slight-tweak-to-header-inclusion-policy-to-promote-iwyu/81430
[56] Luca Palmieri. Error Handling In Rust: A Deep Dive[EB/OL]. (2021-05). https://lpalmieri.com/posts/error-handling-rust/
[57] Comprehensive Rust(Google)— thiserror and anyhow[EB/OL]. https://google.github.io/comprehensive-rust/error-handling/thiserror-and-anyhow.html
[58] Predrag Gruevski. cargo-semver-checks 2025 Year in Review[EB/OL]. (2026-01-11). https://predr.ag/blog/cargo-semver-checks-2025-year-in-review/
[59] abseil-cpp FAQ（ABI 与预编译库纪律）[EB/OL]. (2026-02). https://github.com/abseil/abseil-cpp/blob/master/FAQ.md
[60] DataCamp. DeepSeek Harness vs Claude Code[EB/OL]. (2026-08-24). https://www.datacamp.com/blog/deepseek-harness-vs-claude-code
[61] dsh docs/testing.md[EB/OL]. (2026-08-13). https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/testing.md
[62] Agent Client Protocol. Transports spec[EB/OL]. (2026-08-26). https://agentclientprotocol.com/protocol/v1/transports
[63] Zed Blog. The ACP Registry is Live (2026-01-28)[EB/OL]. (2026-08-26). https://zed.dev/blog/acp-registry
[64] JetBrains Blog. JetBrains × Zed: Open Interoperability for AI Coding Agents (2025-10-06)[EB/OL]. (2026-08-26). https://blog.jetbrains.com/ai/2025/10/jetbrains-zed-open-interoperability-for-ai-coding-agents-in-your-ide/
[65] zed-industries/zed. crates/extension_api (README, wit/, Cargo.toml)[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/tree/main/crates/extension_api
[66] Crush Docs. Permissions Configuration[EB/OL]. (2026-03-11). https://charmbracelet-crush.mintlify.app/configuration/permissions
[67] Libation 文档. "Development Environment Setup using Nix or Nix Flakes on Linux"（flake.nix/shell.nix/flake.lock 三文件模式、"Keep your flake.lock committed"）[EB/OL]. https://getlibation.com/docs/development/nix-linux-setup
[68] Determinate Systems 官方文档. "Determinate in GitHub Actions"（2025-11；determinate-nix-action / flakehub-cache-action / flake-checker-action / update-flake-lock 一览）[EB/OL]. https://docs.determinate.systems/guides/github-actions/
[69] nix-community/nix-direnv README. "A fast, persistent use_nix/use_flake implementation for direnv"（GitHub，访问于 2026-08，README 含缓存/gcroots/watch 文件清单/安装方式）[EB/OL]. https://github.com/nix-community/nix-direnv
[70] hyprwm/Hyprland. flake.nix 原文（main 分支，访问于 2026-08；follows 去重、mkShell.override stdenv、inputsFrom、nix-systems/default-linux）[EB/OL]. https://raw.githubusercontent.com/hyprwm/Hyprland/main/flake.nix
[71] Python DevTools Handbook. "How to use uv on NixOS"（2026-08；nix-ld 模式、flake 提供 uv 模式、LD_LIBRARY_PATH 警告、"Skip uv2nix for day-to-day development"）[EB/OL]. https://pydevtools.com/handbook/how-to/how-to-use-uv-on-nixos/
[72] NixOS Wiki. Rust（"no less than 8 different solutions for building Rust code with Nix" 对比表）[EB/OL]. https://wiki.nixos.org/wiki/Rust
[73] devenv blog. "Closing the Nix Gap: From Environments to Packaged Applications for Rust"（2025-08-22；fenix→rust-overlay 更换原因、crate2nix/uv2nix 封装）[EB/OL]. https://devenv.sh/blog/2025/08/22/closing-the-nix-gap-from-environments-to-packaged-applications-for-rust/
[74] NixOS Discourse. "Uv2nix - Build & develop Python projects using uv with Nix"（adisbladis，2025-01-09；去 experimental 标签、特性列表）[EB/OL]. https://discourse.nixos.org/t/uv2nix-build-develop-python-projects-using-uv-with-nix/58563
[75] nix-community/naersk README（GitHub；"Naersk by default ignores the rust-toolchain file"）[EB/OL]. https://github.com/nix-community/naersk
[76] OpenReplay blog. "5 Version Managers Every Developer Should Know"（2026-06；rust-toolchain.toml 格式示例、"stable floats… surprising in CI"）[EB/OL]. https://blog.openreplay.com/5-version-managers-developers/
[77] zed-industries/zed. rust-toolchain.toml[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/rust-toolchain.toml
[78] Astral uv 官方文档. Python versions（.python-version 发现规则、requires-python 优先级、uv python pin，访问于 2026-08）[EB/OL]. https://docs.astral.sh/uv/concepts/python-versions/
[79] uv CLI 文档. uv python pin（pin 与 requires-python 冲突报错原文；pyenv 兼容性建议）[EB/OL]. https://astral-sh-uv.mintlify.app/cli/python-pin
[80] pydevtools.com handbook. How to Use a uv Lockfile for Reproducible Python Environments[EB/OL]. (2026-08-19). https://pydevtools.com/handbook/how-to/how-to-use-a-uv-lockfile-for-reproducible-python-environments/
[81] FerranAD/simple-uv2nix（GitHub；.python-version + requires-python + uv lock 的版本变更流程）[EB/OL]. https://github.com/FerranAD/simple-uv2nix
[82] dev.to. "Using Nix on Windows (the Right Way)"（2026-01；WSL2 必需、"not practical without WSL"、项目放 Linux 文件系统）[EB/OL]. https://dev.to/jajera/using-nix-on-windows-the-right-way-14ki
[83] GitCode 博客. nix-installer 在 WSL 的兼容性问题解析（2025-06；WSL1 缺进程隔离导致构建失败，须 WSL2）[EB/OL]. https://blog.gitcode.com/c96e1cabc14b356bc7c5ba6c935b792f.html
[84] devcontainers Discussion #61. Windows support（GitHub，2023-08；"devcontainer only supports the Linux container variant"）[EB/OL]. https://github.com/orgs/devcontainers/discussions/61
[85] Obright. "Nix vs mise Deep Dive 2026"（2026-06；对比表、"not competitors—they are complements"、两层策略建议）[EB/OL]. https://www.oflight.co.jp/en/columns/nix-vs-mise-dev-env-comparison-2026
[86] twdev.blog. "Nix is to C++ what uv/poetry is to Python"（2025-07-15；cmake + nixpkgs 构建 C++ 示例）[EB/OL]. https://twdev.blog/2025/07/nix/
[87] DeepWiki. Conan CMake Integration（基于 conan-io/conan 源码 conan/tools/cmake/）[EB/OL]. (2026-02). https://deepwiki.com/conan-io/conan/3.1-cmake-integration
[88] LLVM 官方文档. Getting Started with the LLVM System（Requirements / Stand-alone Builds）[EB/OL]. (2026-02). https://llvm.org/docs/GettingStarted.html
[89] LLVM Developer Policy（vendored dependency 版权特例；经 ROCm 镜像确认同文）[EB/OL]. (2026-02). https://rocm.docs.amd.com/projects/llvm-project/en/latest/LLVM/llvm/html/DeveloperPolicy.html
[90] Chromium 官方文档. Using depot_tools（Pinned deps / DEPS 说明）[EB/OL]. (2026-02). https://www.chromium.org/developers/how-tos/depottools/
[91] GitHub Marketplace. Magic Nix Cache（"doesn't offer a publicly available cache… only usable in CI"）[EB/OL]. https://github.com/marketplace/actions/magic-nix-cache
[92] Determinate Systems blog. "Bringing back the Magic Nix Cache Action"（2025-06-13；GitHub 缓存 API 逆向风险声明、推荐 FlakeHub Cache）[EB/OL]. https://determinate.systems/blog/bringing-back-magic-nix-cache-action/
[93] rust-analyzer manual, "Contributing. Release Process"[EB/OL]. (2026-08). https://rust-analyzer.github.io/book/contributing/
[94] jonpspri/databeak #125. Evaluate redundancy between pre-commit hooks and GitHub CI/CD workflows[EB/OL]. (2025-10-06). https://github.com/jonpspri/databeak/issues/125
[95] dotCMS Blog, "Just Run It. Understanding and Using the justfile", 2025-07-21[EB/OL]. https://www.dotcms.com/blog/understanding-and-using-the-justfile
[96] matklad, "Generate All the Things", 2021-09-05[EB/OL]. https://matklad.github.io/2021/09/05/generate-all-the-things.html
[97] matklad (Aleksey Kladov), "Make your own make", 2018-01-03[EB/OL]. https://matklad.github.io/2018/01/03/make-your-own-make.html
[98] matklad/cargo-xtask (specification repo README)[EB/OL]. (2026-08). https://github.com/matklad/cargo-xtask
[99] rust-analyzer xtask API docs[EB/OL]. (2026-08). https://rust-lang.github.io/rust-analyzer/xtask/index.html
[100] Just Programmer's Manual, Introduction[EB/OL]. (2026-08). https://just.systems/man/en/
[101] casey/just GitHub repo[EB/OL]. (2026-08). https://github.com/casey/just
[102] Baserow Docs, "Justfile Command Reference"[EB/OL]. (2026-08). https://baserow.io/docs/development/justfile
[103] developerlife.com, "Use just to manage project specific commands", 2023-08-28[EB/OL]. https://developerlife.com/2023/08/28/justfile/
[104] pre-commit official documentation[EB/OL]. (2026-08). https://pre-commit.com/
[105] j178/prek GitHub README[EB/OL]. (2026-08). https://github.com/j178/prek
[106] prek changelog（v0.0.6 pre-commit-rs→prefligit；v0.0.23 prefligit→prek）[EB/OL]. (2026-08). https://prek.j178.dev/changelog/
[107] GitHub Docs, "Reusing workflow configurations"[EB/OL]. (2026-08). https://docs.github.com/en/actions/concepts/workflows-and-actions/reusing-workflow-configurations
[108] Kubermates, "GitHub Actions Composite vs Reusable Workflows", 2025-07-18[EB/OL]. https://kubermates.org/blog/github-actions-composite-vs-reusable-workflows-4bih/
[109] houseabsolute/actions-rust-cross README（跨平台 matrix 示例）[EB/OL]. (2026-08). https://github.com/houseabsolute/actions-rust-cross
[110] GitHub Docs, "Secure use reference. Using third-party actions"[EB/OL]. (2026-08). https://docs.github.com/en/actions/reference/security/secure-use
[111] zed-industries/zed. README.md (licensing 节）, script/ 目录清单[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed
[112] Kent C. Dodds. "Write tests. Not too many. Mostly integration."[EB/OL]. (2026-06-30). https://kentcdodds.com/talks/write-tests-not-too-many-mostly-integration
[113] Kent C. Dodds. Testing Trophy 论点（经 codersera.com "Software Testing: Complete Guide" 与 productengineer.info 综述转引其原文，2026-05/03 访问）[EB/OL]. https://codersera.com/blog/software-testing-complete-guide-2026/
[114] Spotify Engineering honeycomb 模型（经 Katalon "Software Testing Models: From V-Model to Test Pyramid" 综述，2026-02-05）[EB/OL]. https://katalon.com/resources-center/blog/software-testing-model-evolution
[115] test-strategy skill 汇总（tool.lu，2026-07-05 访问）[EB/OL]. https://tool.lu/skill/s/3Gp
[116] pydevtools.com handbook. How to Measure Code Coverage with pytest-cov[EB/OL]. (2026-08-19). https://pydevtools.com/handbook/how-to/how-to-measure-code-coverage-with-pytest-cov/
[117] IBM/mcp-context-forge #2625. [EPIC][TESTING]: Achieve 80%+ Code Coverage with CI/CD Enforcement[EB/OL]. (2026-02-01). https://github.com/IBM/mcp-context-forge/issues/2625
[118] insta-ruby README "Prior Art" 一节（GitHub，2026-07-14 访问）[EB/OL]. https://github.com/marcoroth/insta-ruby
[119] "Snapshot Testing Rust Code with cargo-insta"（mutorium.com，2025-12-09）[EB/OL]. https://www.mutorium.com/blog/cargo-insta-snapshot-testing/
[120] DEV.co. Syrupy: Pytest Snapshot Testing Plugin[EB/OL]. (2026-07-08). https://dev.co/testing/open-source/syrupy
[121] syrupy. GitHub README（2025-03-24 访问）[EB/OL]. https://github.com/syrupy-project/syrupy
[122] "How uv Works Under the Hood"（noos.blog，2026-08-04）[EB/OL]. https://noos.blog/posts/uv-how-it-works-under-the-hood/
[123] astral-sh/uv. CONTRIBUTING.md[EB/OL]. (2026). https://github.com/astral-sh/uv/blob/main/CONTRIBUTING.md
[124] Foundry AGENTS.md（foundry-rs/foundry，GitHub）[EB/OL]. https://github.com/foundry-rs/foundry/blob/master/AGENTS.md
[125] OOPSLA 2025 PBT 论文（UCSD）[EB/OL]. https://cseweb.ucsd.edu/~mcoblenz/assets/pdf/OOPSLA_2025_PBT.pdf
[126] "Property-Based Testing in Python with Hypothesis"（universopython.com，2026-07-31）[EB/OL]. https://universopython.com/en/blog/python-hypothesis-property-based-testing
[127] TU Delft 硕士论文：Hypothesis 在真实 Python 项目中的使用研究（87 tests / 7 repos）[EB/OL]. https://repository.tudelft.nl/
[128] "How to Build Property-Based Testing with Hypothesis"（OneUptime，2026-01-30）[EB/OL]. https://oneuptime.com/blog/post/2026-01-30-how-to-build-property-based-testing-with-hypothesis/view
[129] "Property-Based Testing in Rust with the proptest Crate"（Sling Academy，2025-02-04）[EB/OL]. https://www.slingacademy.com/article/property-based-testing-in-rust-with-the-proptest-crate/
[130] zed-industries/zed. script/randomized-test-ci[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/script/randomized-test-ci
[131] zed-industries/zed. crates/collab/tests/integration/randomized_test_helpers.rs[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/crates/collab/tests/integration/randomized_test_helpers.rs
[132] Mike Bland. "Legacy code, seams, and the most important design guideline"（2023-08-23）[EB/OL]. https://mike-bland.com/2023/08/23/legacy-code-seams-and-the-most-important-design-guideline.html
[133] Go `testing/synctest`（Go 1.25 标准库；综述：glukhov.org，2026-06-26）[EB/OL]. https://www.glukhov.org/app-architecture/testing-architecture/testing-concurrent-go-code-synctest/
[134] rr 官方站[EB/OL]. (2024-10-18). https://rr-project.org/
[135] awesome-ttd（time-travel debugging 工具清单，2025-11-02 访问）[EB/OL]. https://github.com/xusheng6/awesome-ttd
[136] "GitHub Actions Matrix Builds 实战指南"（eastondev.com，2026-07-30）[EB/OL]. https://eastondev.com/blog/en/posts/dev/20260428-github-actions-matrix/
[137] "How to Use Matrix Include/Exclude in GitHub Actions"（OneUptime，2025-12-20）[EB/OL]. https://oneuptime.com/blog/post/2025-12-20-github-actions-matrix-include-exclude/view
[138] "GitHub Actions Matrix strategy. Basics, tutorial & best practices"（Octopus，2026-08-11）[EB/OL]. https://octopus.com/devops/github-actions/github-actions-matrix/
[139] Nx 官方文档. "Run Only Tasks Affected by a PR"[EB/OL]. https://nx.dev/docs/features/ci-features/affected
[140] "Overcoming monorepo challenges with Bazel"（VirtusLab，2026-06-25）[EB/OL]. https://virtuslab.com/blog/backend/overcoming-monorepo-challenges/
[141] Harness CI Nx 支持 feature request（含 path-based vs graph-based 分析，2026-07-14 访问）[EB/OL]. https://ideas.harness.io/feature-request/p/native-nx-monorepo-support-in-harness-ci-affected-graph-aware-builds-and-tests
[142] "GitHub Merge Queue in 2026. How It Works & Handling Flaky Required Status Checks"（tenki.cloud，2026-04-16）[EB/OL]. https://tenki.cloud/blog/github-merge-queue-setup
[143] GitHub Marketplace. ccache-action（matrix 缓存隔离与 COMPILER_LAUNCHER 接法）[EB/OL]. (2026-02). https://github.com/marketplace/actions/ccache-action
[144] Swatinem/rust-cache[EB/OL]. (2026). https://github.com/Swatinem/rust-cache
[145] cargo-nextest. 官网[EB/OL]. (2026). https://nexte.st/
[146] nextest docs. Why process-per-test?[EB/OL]. (2026). https://nexte.st/docs/design/why-process-per-test/
[147] dtolnay/trybuild[EB/OL]. (2026). https://github.com/dtolnay/trybuild
[148] The Complete C/C++ Sanitizers Handbook（CI sanitizer 矩阵与 presets 落地，引用 Clang/CMake/MS 官方数据）[EB/OL]. (2026-07). https://gist.github.com/MangaD/3b46e4c5ef4c63e44a21bed39ae64093
[149] C Programming in the AI Era, Ch.20 Fuzzing（libFuzzer/AFL++/OSS-Fuzz 纪律综述）[EB/OL]. (2026-02). https://www.socratopia.app/library/c-programming-ai-era-en/chapter-20
[150] OSS-Fuzz 官方文档. Setting up a new project[EB/OL]. (2026-02). https://google.github.io/oss-fuzz/getting-started/new-project-guide/
[151] pytest Documentation 8.0 (Fixtures reference: conftest.py sharing, parametrize)[EB/OL]. https://docs.pytest.org/en/8.0.x/
[152] pytest-dev/pytest #13148. Improve documentation around conftest-files and importing of fixtures[EB/OL]. (2025-01-20). https://github.com/pytest-dev/pytest/issues/13148
[153] Write the Docs. "Docs as Code"[EB/OL]. (2026-08-26). https://www.writethedocs.org/guide/docs-as-code/
[154] Diátaxis. 官方站点[EB/OL]. (2026-08-26). https://diataxis.fr/
[155] mkdocstrings-python. PyPI 项目页（官方特性列表）[EB/OL]. https://pypi.org/project/mkdocstrings-python/
[156] Verilog-to-Routing. "Sphinx API Documentation for C/C++ Projects"[EB/OL]. (2026-08-26). https://docs.verilogtorouting.org/en/latest/dev/c_api_doc/
[157] IBM Think. "What Is OpenAPI?"[EB/OL]. (2026-02-24). https://www.ibm.com/think/topics/open-api
[158] Pontil. "OpenAPI spec drift: why your contract lies before your agents break"[EB/OL]. (2026-07-22). https://www.pontil.com/blog/openapi-spec-drift-why-your-contract-lies-before-your
[159] Dochia Blog. "OpenAPI as a Single Source of Truth for APIs"[EB/OL]. (2025-09-16). https://blog.dochia.dev/blog/openapi-single-source/
[160] zed-industries/zed. docs/book.toml[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/docs/book.toml
[161] Kubeflow website 仓库 README. "Using Hugo shortcodes"（版本号短代码注入）[EB/OL]. (2023-12-19). https://github.com/hbelmiro/kubeflow-website
[162] MADR. "Markdown Architectural Decision Records"[EB/OL]. (2024-09-17). https://adr.github.io/madr/
[163] kroxylicious merge queue issue（changelog 冲突分析，2026-04-09）[EB/OL]. https://github.com/kroxylicious/kroxylicious/issues/3666
[164] Keep a Changelog v1.1.0[EB/OL]. (2026-08-26). https://keepachangelog.com/en/1.1.0/
[165] Conventional Commits v1.0.0[EB/OL]. (2026-08-26). https://www.conventionalcommits.org/en/v1.0.0/
[166] release-plz 官方文档[EB/OL]. https://release-plz.dev/docs
[167] Scattercode. "A changelog you never write: Conventional Commits, Lefthook and git-cliff"[EB/OL]. (2026-07-17). https://scattercode.dev/2026/07/a-changelog-you-never-write-conventional-commits-lefthook-and-git-cliff/
[168] Acoular. "Documentation"（CI 中 pytest+doctest 测试文档片段）[EB/OL]. (2026-08-26). https://www.acoular.org/contributing/documentation.html
[169] "How to Set Up Automatic Broken Link Monitoring"（含 lychee-action PR/定时 workflow 示例）[EB/OL]. (2026-04-10). https://broken-links-checker.com/blog/automatic-broken-link-monitoring
[170] Vale. 官方文档[EB/OL]. (2026-08-26). https://vale.sh/docs
[171] Stripe Engineering Blog. "How Stripe builds interactive docs with Markdoc"[EB/OL]. (2022-09-13). https://stripe.com/blog/markdoc
[172] GitHub Blog. "Spec-driven development with AI: Get started with a new open source toolkit"[EB/OL]. (2025-09-02). https://github.blog/ai-and-ml/generative-ai/spec-driven-development-with-ai-get-started-with-a-new-open-source-toolkit/
[173] github/spec-kit. "spec-driven.md"[EB/OL]. (2026-08-26). https://github.com/github/spec-kit/blob/main/spec-driven.md
[174] Howardism. "Code as Source of Truth"（转述 Fiona Fung 的 AI-native 工程组织实践）[EB/OL]. (2026-05-23). https://www.howardism.dev/articles/code-as-source-of-truth
[175] github/spec-kit Discussion #152. "Evolving specs"[EB/OL]. (2026-08-10). https://github.com/github/spec-kit/discussions/152
[176] Python 模块、包与现代工程实践（hatch-vcs 配置示例）— 掘金[EB/OL]. (2026-06-18). https://juejin.cn/post/7652636952517509156
[177] pydevtools.com handbook. How to add dynamic versioning to uv projects[EB/OL]. (2026-08-19). https://pydevtools.com/handbook/how-to/how-to-add-dynamic-versioning-to-uv-projects/
[178] cargo-dist. GitHub README 与 CHANGELOG（announcement/tag 语义）[EB/OL]. https://github.com/axodotdev/cargo-dist
[179] release-plz GitHub Quickstart（trusted publishing 配置说明）[EB/OL]. https://release-plz.dev/docs/github/quickstart
[180] "Why I Chose Changesets over Semantic-Release"（xnok.github.io，2026-08-21）[EB/OL]. https://xnok.github.io/infra-bootstrap-tools/blog/intentional-releases-changesets/
[181] "What is Semantic Release?"（JFrog Learn，2026-07-03）[EB/OL]. https://jfrog.com/learn/sdlc/semantic-release/
[182] docs.rs. cargo-semver-checks[EB/OL]. https://docs.rs/crate/cargo-semver-checks/latest
[183] Rust Project Goals 2026：cargo-semver-checks 并入 cargo 路线图（2025-11-18）[EB/OL]. https://rust-lang.github.io/rust-project-goals/2026/cargo-semver-checks.html
[184] litellm issue #24542. 迁移 Trusted Publishers 的根因分析（2026-03-25 访问）[EB/OL]. https://github.com/BerriAI/litellm/issues/24542
[185] "Why Use Trusted Publishing for PyPI?"（pydevtools.com，2026-08-17）[EB/OL]. https://pydevtools.com/handbook/explanation/why-use-trusted-publishing-for-pypi/
[186] lirantal/pypi-security-best-practices. PyPI Security Best Practices (§11 Trusted Publishing OIDC)[EB/OL]. (2024-03-06). https://github.com/lirantal/pypi-security-best-practices
[187] Rust Blog. crates.io development update(Trusted Publishing 公告)[EB/OL]. (2025-07-11). https://blog.rust-lang.org/2025/07/11/crates-io-development-update-2025-07/
[188] crates.io. Trusted Publishing 文档[EB/OL]. (2026). https://crates.io/docs/trusted-publishing
[189] "Trusted Publishing to npm and PyPI with OIDC"（systemshardening.com，2026-05-02）[EB/OL]. https://www.systemshardening.com/articles/cicd/trusted-publishing-oidc/
[190] NuGet Trusted Publishing 综述（safeguard.sh，2026-05-12）[EB/OL]. https://safeguard.sh/resources/blog/nuget-trusted-publishing-signing-rollout-2026
[191] Astral Docs. Using uv in GitHub Actions | uv[EB/OL]. (2026-08-14). https://docs.astral.sh/uv/guides/integration/github/
[192] "Keyless image signing. Sigstore and cosign in CI"（hogin.pro，2026-06-12）[EB/OL]. https://hogin.pro/posts/sigstore-cosign-keyless-ci/
[193] "What is The SLSA Framework?"（Wiz Academy，2026-06-02）[EB/OL]. https://www.wiz.io/academy/application-security/slsa-framework
[194] "GitHub Actions Artifact Attestations. SLSA Provenance and Supply Chain Defaults"（tenki.cloud，2026-04-16）[EB/OL]. https://tenki.cloud/blog/github-actions-artifact-attestations-slsa
[195] ClashMetaForAndroid PR #769（xz 后门类比论证，2026-06-06 访问）[EB/OL]. https://github.com/MetaCubeX/ClashMetaForAndroid/pull/769
[196] cargo-dist (dist) Book. Introduction[EB/OL]. (2026). https://axodotdev.github.io/cargo-dist/book/introduction.html
[197] zed-industries/zed. .github/workflows/release.yml[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/.github/workflows/release.yml
[198] fmt 官方文档 Usage[EB/OL]. (2026-02). https://fmt.dev/10.2.0/usage.html
[199] Zed Blog. Introducing Zed's preview channel (2023-07-19)[EB/OL]. (2026-08-26). https://zed.dev/blog/preview-channel
[200] zed-industries/zed. script/bump-zed-version[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/script/bump-zed-version
[201] zed-industries/zed. .github/workflows/release_nightly.yml[EB/OL]. (2026-08-26). https://github.com/zed-industries/zed/blob/main/.github/workflows/release_nightly.yml
[202] Pure AI. Linux Foundation Launches Agentic AI Foundation[EB/OL]. (2025-12-22). https://pureai.com/articles/2025/12/22/linux-foundation-launches-agentic-stuff.aspx
[203] Baltes et al.. Harness Engineering for Agentic AI Coding Tools, arXiv:2602.14690[EB/OL]. (2026-02-16). https://arxiv.org/abs/2602.14690
[204] Mohsenimofidi et al.. Context Engineering for AI Agents in Open-Source Software, arXiv:2510.21413[EB/OL]. (2025-10-24). https://arxiv.org/abs/2510.21413
[205] GitHub Docs. Support for different types of custom instructions[EB/OL]. (2026-08-26). https://docs.github.com/en/copilot/reference/custom-instructions-support
[206] GitHub Docs. Adding repository custom instructions for Copilot[EB/OL]. (2026-08-26). https://docs.github.com/copilot/customizing-copilot/adding-custom-instructions-for-github-copilot
[207] kyenai. AGENTS.md vs CLAUDE.md vs Copilot Instructions 对比与证据矩阵[EB/OL]. (2026-06-14). https://www.kyenai.com/guides/agents-md-vs-claude-md-cursorrules-copilot-instructions
[208] Cursor Docs. Rules（.mdc frontmatter 与四种激活模式）[EB/OL]. (2026-08-26). https://cursor.com/docs/rules
[209] Claude Code memory 文档及层级实测总结（claude-howto 02-memory）[EB/OL]. (2026-08-04). https://code.claude.com/docs/en/memory ; https://github.com/luongnv89/claude-howto/blob/main/02-memory/README.md
[210] agyn.io: AGENTS.md vs CLAUDE.md. Does Claude Code or Codex Read Both?[EB/OL]. (2026-06-03). https://agyn.io/blog/claude-md-agents-md-compatibility
[211] vercel/next.js 仓库 AGENTS.md（原文）[EB/OL]. (2026-08-26). https://raw.githubusercontent.com/vercel/next.js/canary/AGENTS.md
[212] SSW Rules. Do you symlink your AGENTS.md to other tool-specific files?[EB/OL]. (2026-08-23). https://www.ssw.com.au/rules/symlink-agents-to-claude
[213] Lulla et al.. On the Impact of AGENTS.md Files on the Efficiency of AI Coding Agents, arXiv:2601.20404[EB/OL]. (2026-01). https://arxiv.org/pdf/2601.20404
[214] Matt Nigh (GitHub Blog): How to write a great agents.md. Lessons from over 2,500 repositories[EB/OL]. (2025-11-25). https://github.blog/ai-and-ml/github-copilot/how-to-write-a-great-agents-md-lessons-from-over-2500-repositories/
[215] elastic/kibana 仓库 AGENTS.md（原文）[EB/OL]. (2026-08-26). https://raw.githubusercontent.com/elastic/kibana/main/AGENTS.md
[216] Zietsman. Structural Quality Gaps in Practitioner AI Governance Prompts, arXiv:2604.21090[EB/OL]. (2026-04-22). https://arxiv.org/abs/2604.21090
[217] Zietsman. Fifty Years of Specification Completeness（七原则扩展，P7=0 发现）, arXiv:2606.25120[EB/OL]. (2026-06-23). https://arxiv.org/abs/2606.25120
[218] Chatlatanagulchai et al.: Agent READMEs. An Empirical Study of Context Files for Agentic Coding, arXiv:2511.12884[EB/OL]. (2025-11-17). https://arxiv.org/abs/2511.12884
[219] Anthropic Engineering. Claude Code Best Practices（CLAUDE.md 内容与迭代）[EB/OL]. (2025-04-18). https://www.anthropic.com/engineering/claude-code-best-practices
[220] mock-server monorepo. opencode building blocks（反模式表）[EB/OL]. (2026-08-26). https://github.com/mock-server/mockserver-monorepo/blob/master/docs/operations/opencode-building-blocks.md
[221] Agent Skills 开放规范（agentskills.io）及 SKILL.md Anatomy 指南[EB/OL]. (2026-06-09). https://agentskills.io/specification ; https://agentman.ai/blog/build-your-first-agent-skill-skillmd-anatomy
[222] apache/airflow 仓库 AGENTS.md（原文）[EB/OL]. (2026-08-26). https://raw.githubusercontent.com/apache/airflow/main/AGENTS.md
[223] openai/codex 仓库 AGENTS.md（原文）[EB/OL]. (2026-08-26). https://raw.githubusercontent.com/openai/codex/main/AGENTS.md
[224] InventiveHQ. Codex/Claude/Gemini CLI Sandbox & Approval Modes Compared[EB/OL]. (2026-08-13). https://inventivehq.com/blog/ai-coding-cli-sandbox-approval-modes-compared
[225] Anomity. OpenAI Codex Commands Cheat Sheet[EB/OL]. (2026-08-08). https://anomity.ai/blog/openai-codex-commands-cheat-sheet/
[226] opencode Docs. Agents[EB/OL]. (2026-08-26). https://opencode.ai/docs/agents/
[227] opencode Docs. Permissions[EB/OL]. (2026-08-26). https://opencode.ai/docs/permissions/
[228] wuu73. Goose Deep Dive(GooseMode 源码分析)[EB/OL]. (2026). https://wuu73.org/aiguide/infoblogs/coding_agents/goose.html
[229] InfraGap. AGENTS.md Explained[EB/OL]. (2026-07-28). https://infragap.com/agents-md/
[230] Backslash Security. AGENTS.md Injection in OpenAI Codex CLI[EB/OL]. (2026-07-06). https://www.backslash.security/blog/openai-codex-injection-in-agents-md-exfiltrating-credentials
[231] Command Line Interface Guidelines[EB/OL]. https://clig.dev/
[232] Node.js CLI Apps Best Practices（lirantal）[EB/OL]. https://github.com/lirantal/nodejs-cli-apps-best-practices
[233] Square Engineering. "Command Line Observability with Semantic Exit Codes"（2023-01）[EB/OL]. https://developer.squareup.com/blog/command-line-observability-with-semantic-exit-codes/
[234] "Applying clig.dev to a Go CLI. With an Automated Compliance Test"（dev.to，2026-04-14）[EB/OL]. https://dev.to/bala_paranj_059d338e44e7e/applying-cligdev-to-a-go-cli-with-an-automated-compliance-test-1oa2
[235] docs.rs. miette（Rust 诊断错误库）官方文档[EB/OL]. https://docs.rs/miette
[236] rust-lang/cargo Issue #15944. 迁移 annotate-snippets 统一诊断渲染[EB/OL]. https://github.com/rust-lang/cargo/issues/15944
[237] opencode Docs. Intro[EB/OL]. (2026-08-26). https://opencode.ai/docs/
[238] goose Docs. Permission Modes[EB/OL]. (2026-04-07). https://goose-docs.ai/docs/guides/goose-permissions/
[239] charmbracelet/crush (GitHub README)[EB/OL]. (2026-08). https://github.com/charmbracelet/crush
[240] Aider-AI/aider (GitHub README)[EB/OL]. (2026-08). https://github.com/Aider-AI/aider
[241] 博客园. DeepSeek Harness 教程:特性/运行模式/Trajectory/配置树[EB/OL]. (2026-08-15). https://www.cnblogs.com/hong2018/p/22494668
[242] crush Discussion #3077(context-token 权限提案)[EB/OL]. (2026-06-07). https://github.com/charmbracelet/crush/discussions/3077
[243] Rust Project Primer. Lints([lints]/[workspace.lints])[EB/OL]. (2026). https://rustprojectprimer.com/checks/lints.html
[244] nextest docs. Archiving and reusing builds[EB/OL]. (2026). https://github.com/nextest-rs/nextest/blob/main/site/src/docs/ci-features/archiving.md
[245] rust-lang/miri[EB/OL]. (2026). https://github.com/rust-lang/miri
[246] cargo-deny. README/Book(EmbarkStudios)[EB/OL]. (2026). https://embarkstudios.github.io/cargo-deny/
[247] cargo-hakari docs(guppy)[EB/OL]. (2026). https://docs.rs/cargo-hakari ; https://guppy-rs.github.io/guppy/rustdoc/cargo_hakari/about/index.html
[248] PEP 621 – Storing project metadata in pyproject.toml[EB/OL]. (2026-07-13). https://peps.python.org/pep-0621/
[249] Python Packaging User Guide. src layout vs flat layout[EB/OL]. https://packaging.python.org/en/latest/discussions/src-layout-vs-flat-layout/
[250] pydevtools.com handbook. Ruff: Complete Guide[EB/OL]. (2026-08-19). https://pydevtools.com/handbook/explanation/ruff-complete-guide/
[251] benchflow-ai/skillsbench. uv-package-manager SKILL.md (best practices summary)[EB/OL]. (2026). https://github.com/benchflow-ai/skillsbench
[252] Lazarus-AI/clearwing. clearwing CONTRIBUTING.md (uv run as every-command entry)[EB/OL]. (2026-04-14). https://github.com/Lazarus-AI/clearwing/blob/main/CONTRIBUTING.md
[253] astral-sh/uv. uv changelog 0.4.x (0.4.27: PEP 735 dependency-groups)[EB/OL]. https://github.com/astral-sh/uv/blob/main/changelogs/0.4.x.md
[254] pydevtools.com handbook. How do Python type checkers compare?[EB/OL]. (2026-08-19). https://pydevtools.com/handbook/explanation/how-do-mypy-pyright-and-ty-compare/
[255] Simon Willison. "ty: An extremely fast Python type checker and LSP"[EB/OL]. (2025-12-23). https://simonwillison.net/tags/python/?page=2
[256] mypy 2.3.1 documentation. The mypy command line[EB/OL]. https://mypy.readthedocs.io/en/stable/command_line.html
[257] pydevtools.com handbook reference. nox: Python Test Automation Tool[EB/OL]. (2026-07-28). https://pydevtools.com/handbook/reference/nox/
[258] googleapis/release-please (supported strategy types)[EB/OL]. https://github.com/googleapis/release-please
[259] Astral Docs. Building and publishing a package | uv[EB/OL]. (2026-07-28). https://docs.astral.sh/uv/guides/package/
[260] vishalsachdev/canvas-mcp #88. Weekly Maintenance Report (duplicate dev dependency groups)[EB/OL]. (2026-04-12). https://github.com/vishalsachdev/canvas-mcp/issues/88
[261] C++ 单测框架 Catch2、doctest、GTest 等对比（cnblogs，社区综述，已与官方文档交叉验证）[EB/OL]. (2025-03). https://www.cnblogs.com/seapo/p/18796800
[262] Catch2 官方文档 docs/cmake-integration.md（catch_discover_tests）[EB/OL]. (2026-02). https://github.com/catchorg/Catch2/blob/devel/docs/cmake-integration.md
[263] CMake Discourse. catch_discover_tests 交叉编译与 PRE_TEST 发现模式[EB/OL]. (2024-03). https://discourse.cmake.org/t/ctest-complains-about-the-test-executable-but-then-deletes-it/10344
[264] peczenyj/structalign issue #99. Adopt ClusterFuzzLite（OSS-Fuzz 被拒后的自建路径）[EB/OL]. (2026-06). https://github.com/peczenyj/structalign/issues/99
[265] LLVM Discourse. Clang-tidy llvm checks in pre/post commit CI（clang-tidy-diff 增量检查）[EB/OL]. (2025-04). https://discourse.llvm.org/t/clang-tidy-llvm-checks-in-pre-post-commit-ci/85998
[266] OpenSSL Communities. Adding clang-tidy checks（渐进启用哲学）[EB/OL]. (2026-01). https://openssl-communities.org/d/GktZlCcU/adding-clang-tidy-checks-to-openssl
[267] sccache 官方 README（PyPI 镜像同文）[EB/OL]. (2026-02). https://pypi.org/project/sccache/
[268] Bear 官方指南. Generate compile_commands.json for a CMake project（引用 CMake 官方对生成器限制的说明）[EB/OL]. (2026-02). https://rizsotto.github.io/Bear/guides/recipes/cmake.html
[269] MongoDB C++ Driver 官方文档. ABI Versioning（soname/SOVERSION/stable vs unstable ABI）[EB/OL]. (2026-02). https://www.mongodb.com/docs/languages/cpp/cpp-driver/current/reference/api-abi-versioning/abi-versioning/
[270] crate-ci/cargo-release[EB/OL]. (2026). https://github.com/crate-ci/cargo-release
[271] s0undt3ch/pre-commit-hooks. pre-commit hooks running system-installed tools (single-pin rationale)[EB/OL]. (2026-06-15). https://github.com/s0undt3ch/pre-commit-hooks
[272] NixOS Wiki. Flakes（devShell 定义与最小示例，wiki.nixos.org，访问于 2026-08）[EB/OL]. https://wiki.nixos.org/wiki/Flakes
[273] TigerBeetle. "Protocol-Aware Deterministic Simulation Testing"（2026-08-20）[EB/OL]. https://tigerbeetle.com/blog/2026-08-20-protocol-aware-dst/
