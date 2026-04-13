# Codex 内部架构演化全景：从快速验证到生产级设计

> 基于对 codex 仓库全部 4979 个 commit 的逐一分析（commit message + diff），完整
> 梳理 Codex 项目中各子系统的迭代演化脉络。每一次演化背后都有明确的工程动机——不
> 是为了"重构而重构"，而是在实际使用中遇到了具体的瓶颈和限制。

> **参考文档**：各子系统的设计文档位于对应 crate 的 `README.md` 中（如
> `codex-rs/app-server/README.md`、`codex-rs/core/src/config_loader/README.md`、
> `codex-rs/core/src/memories/README.md`、`codex-rs/shell-escalation/README.md`、
> `codex-rs/network-proxy/README.md`、`codex-rs/execpolicy/README.md`）。项目不
> 使用独立的 RFC 文档，设计决策记录在 PR description 和 crate README 中。

---

<!-- markdown-toc start - Don't edit this section. Run M-x markdown-toc-refresh-toc -->
**Table of Contents**

- [Codex 内部架构演化全景：从快速验证到生产级设计](#codex-)
  - [第一部分：基础架构演化](#)
    - [1. TypeScript → Rust 大重写](#1-typescript--rust-)
    - [2. codex-core 单体拆分](#2-codex-core-)
    - [3. Configuration 系统四代演进](#3-configuration-)
    - [4. Feature Flag 生命周期管理体系](#4-feature-flag-)
  - [第二部分：工具与执行引擎](#)
    - [5. Shell 工具：四代演进](#5-shell-)
    - [6. Apply Patch：从 JSON 包裹到自由文本](#6-apply-patch-json-)
    - [7. 工具执行流水线重构](#7-)
    - [8. Exec Policy：三代策略引擎](#8-exec-policy)
  - [第三部分：模型与通信层](#)
    - [9. Model Provider：硬编码到可插拔](#9-model-provider)
    - [10. Chat Completions → Responses API 统一](#10-chat-completions--responses-api-)
    - [11. HTTP SSE → WebSocket 传输层](#11-http-sse--websocket-)
    - [12. ModelInfo/ModelFamily 合并](#12-modelinfomodelfamily-)
  - [第四部分：多代理协作](#)
    - [13. Multi-Agent：从扁平 ID 到层级命名](#13-multi-agent-id-)
  - [第五部分：协议与 API 层](#-api-)
    - [14. MCP 子系统：从零到全栈](#14-mcp-)
    - [15. App-Server 协议 v1 → v2](#15-app-server--v1--v2)
    - [16. codex-protocol crate 提取](#16-codex-protocol-crate-)
    - [17. Realtime WebSocket v1 → v2](#17-realtime-websocket-v1--v2)
  - [第六部分：安全与沙箱](#)
    - [18. 沙箱系统：五代演进](#18-)
    - [19. 审批系统：从布尔到 Guardian 自动审批](#19--guardian-)
    - [20. 网络策略：从无到精细化代理](#20-)
  - [第七部分：状态管理与持久化](#)
    - [21. Conversation History → Context Manager](#21-conversation-history--context-manager)
    - [22. Session Resume/Rollout 系统](#22-session-resumerollout-)
    - [23. JSONL → SQLite 状态存储](#23-jsonl--sqlite-)
    - [24. Memory 系统 v1 → v2](#24-memory--v1--v2)
  - [第八部分：用户界面](#)
    - [25. TUI 架构：三次重写](#25-tui-)
    - [26. Auth 系统：四代演进](#26-auth-)
  - [第九部分：生态系统](#)
    - [27. Hooks 引擎：从 notify 到完整事件系统](#27-hooks--notify-)
    - [28. Skills 系统演化](#28-skills-)
    - [29. Plugin 生态系统](#29-plugin-)
  - [30. 总结：Codex 演化的七个共同模式](#30-codex-)
    - [1. 从具体到抽象](#1-)
    - [2. 从隐式到显式](#2-)
    - [3. 从耦合到分层](#3-)
    - [4. 渐进迁移而非大爆炸](#4-)
    - [5. 快速纠正过度工程](#5-)
    - [6. 数据驱动清理](#6-)
    - [7. 安全左移](#7-)

<!-- markdown-toc end -->


## 第一部分：基础架构演化

### 1. TypeScript → Rust 大重写

**为什么**：Node.js 22+ 的运行时依赖严重限制了安装覆盖面——用户必须先安装特定版本
的 Node.js。Rust 提供独立二进制分发、原生沙箱能力（seccomp/landlock/seatbelt）、
零 GC 开销和更低的内存占用。

**解决的问题**：
- 消除了 Node.js 运行时依赖
- 原生实现 Linux Landlock/seccomp 和 macOS Seatbelt 沙箱（Node.js 需要 FFI 或子
  进程）
- 减少了冷启动时间和内存占用

**演化路径**：
```
TypeScript/Node.js CLI (codex-cli/) → 并行开发 Rust (codex-rs/) → TypeScript 完全删除 → Rust 唯一实现
```

**关键 commit**：
- `31d0d7a30` — Rust 初始导入（~14000 行），建立 workspace 结构
- `408c7ca14` — **删除全部 TypeScript 代码**（-36000 行，216 文件）——标志着迁移
  完成
- `cca1122dd` — 删除 `interactive/` crate（Ratatui 全屏重绘与滚动历史不兼容）
- `c432d9ef8` — 删除 `repl/` crate（"served its purpose"）

**子演化：4 CLI → 1 CLI**：初始有 `cli`/`tui`/`repl`/`exec`/`interactive` 五个
crate，逐步合并为 `cli` + `tui` 入口。

**优缺点**：

| 维度 | TypeScript | Rust |
|---|---|---|
| 开发速度 | 快（动态类型，npm 生态） | 慢（编译时间，学习曲线） |
| 分发 | 需要 Node.js 运行时 | 独立二进制 |
| 沙箱能力 | FFI/子进程 | 原生系统调用 |
| 跨平台 | 好（Node 跨平台） | 需要各平台 CI 编译 |
| 性能 | GC 暂停，高内存 | 零 GC，低内存 |

**权衡**：丧失了 JS 生态的快速原型能力（PR 周转时间变长），换来的是编译时安全和
运行时性能。npm 分发策略也经历了独立包 → dist-tags 的演化（`c19969c67` →
`d9c014efc`）。

---

### 2. codex-core 单体拆分

**为什么**：`codex-core` 作为单体 crate 随着功能增长变成了编译瓶颈——任何修改都触
发全量重编译。一次提取 `codex-shell-command` 就减少了 12% 的编译时间。

**解决的问题**：
- 编译时间过长，影响开发迭代速度
- 依赖关系不清晰——所有代码都在一个 crate 中，无法识别真正的模块边界
- 其他 crate（如 `exec-server`）不得不依赖整个 `codex-core`，拉入不需要的代码

**演化路径**：
```
codex-core (单体, 所有逻辑) → 逐步提取 20+ 独立 crate → 删除 re-exports 强制显式依赖
```

**关键 commit**：
- `d735df1f5` — 提取 `codex-hooks` crate
- `d8f9bb65e` — 提取 `codex-shell-command`（减少 12% 编译时间）
- `8b7f8af34` — **消除 `codex-common`**，拆为 6 个 `codex-utils-*` crate
- `577a416f9` — 提取 `codex-config`
- `f49eb8e9d` — 提取 `codex-sandboxing`
- `1af2a37ad` — **删除 re-exports**（149 文件修改，强制下游直接依赖源 crate）
- 44d28f500 ~ 258ba436f — `codex-tools` 的 12 个渐进提取 commit

**优缺点**：

| 维度 | 单体 crate | 拆分 crate |
|---|---|---|
| 编译速度 | 慢（全量重编译） | 快（增量编译） |
| 依赖清晰度 | 差（all-in-one） | 好（显式 Cargo.toml） |
| 代码复用 | 简单（内部 pub） | 需要考虑 API 边界 |
| 维护成本 | 低（一个 Cargo.toml） | 高（20+ 个 Cargo.toml） |
| 重构风险 | 低（crate 内部移动） | 中（跨 crate 移动需要考虑 API） |

**权衡**：更多 crate = 更多 manifest 维护。CI 增加了 `verify codex-rs Cargo
manifests inherit workspace settings`（`9a8730f31`）检查来防止 manifest 不一致。
关键手法是先提取再删除 re-export——渐进迁移而非大爆炸。

---

### 3. Configuration 系统四代演进

**为什么**：随着 Codex 从开发者工具演化为企业级产品，配置需求从简单的环境变量发
展到需要多层配置合并、企业约束、per-key 来源追踪的复杂系统。

**解决的问题**：
- v1 → v2：开发者需要持久化配置而非每次设置环境变量
- v2 → v3：企业客户需要管理员级配置覆盖和多来源配置合并
- v3 → v4：企业需要**约束**（不只是默认值）——强制特定 sandbox mode/approval
  policy

**演化路径**：
```
v1: 环境变量 + .env
v2: config.toml + --profile + -c key=val
v3: ConfigBuilder + ConfigLayerStack (MDM/System/User/Project/Session 层)
v4: + requirements.toml + Constrained<T>
```

**关键 commit**：
- `4eda4dd77` — 引入 `ConfigOverrides` 模式
- `574656142` — `config.rs` 拆分为 `config.rs` + `config_types.rs`
- `b90328574` — **引入 ConfigLayerStack**（+635/-244 行）——核心架构变更
- `3d4ced3ff` — 迁移所有调用方到 `ConfigBuilder`
- `2f048f206` — 引入 `requirements.toml`
- `dc61fc5f5` — `allowed_sandbox_modes` 约束
- `8ff16a771` — 支持 in-repo `.codex/config.toml`（Project 层）
- `bfff0c729` — **企业 feature requirements 强制执行**（+1718 行）

**设计文档**：`codex-rs/core/src/config_loader/README.md` 详细描述了层级模型和优
先级。

**优缺点**：

| 维度 | 环境变量 | ConfigLayerStack |
|---|---|---|
| 简单场景 | 一行搞定 | 需要了解层级 |
| 企业管控 | 无 | `requirements.toml` + `Constrained<T>` |
| per-key 溯源 | 不可能 | `origins()` 方法 |
| 配置冲突 | 最后设置赢 | 明确的层级优先级 |
| 复杂度 | 低 | 高（6 个源文件 + merge/fingerprint） |

**权衡**：`Constrained<T>` 引入了运行时验证开销——每次设值都需要检查约束。但这比
静默忽略企业策略更安全。`AbsolutePathBuf` 类型在反序列化时自动解析相对路径，增加
了类型系统的复杂度但消除了一类路径混淆 bug。

---

### 4. Feature Flag 生命周期管理体系

**为什么**：代码中散落着大量 ad-hoc 布尔值控制功能开关，没有统一的生命周期管理。
当需要清理旧功能时，无法确定哪些 flag 还有用户在用。

**解决的问题**：
- 统一了分散的功能开关管理
- 提供了从实验到稳定到废弃的清晰路径
- 通过遥测追踪了旧别名使用量，数据驱动清理决策
- 企业可以通过 `requirements.toml` 锁定 feature flag

**关键 commit**：
- `f7b4e2960` — **引入 Feature flag 系统**（`Feature` 枚举 + `Stage` 生命周期）
- `775fbba6e` — 对未知 feature 名称报错
- `060637b4d` — 废弃功能的警告系统
- `ac6ba286a` — `/experimental` TUI 菜单（自动渲染所有 Beta 功能）
- `3cc9122ee` — **实验性 API 宏**（`#[experimental("reason")]`）
- `d65f09b91` — 断言 `UnderDevelopment` feature 必须默认关闭

**生命周期**：
```
UnderDevelopment → Experimental → Stable → Deprecated → Removed
   (不可见)      (/experimental)   (默认开)  (仍可用)     (清除)
```

**设计决策**：
- `legacy.rs` 提供新旧名称映射，使用旧名时输出 `info!` 日志
- `record_legacy_usage` 追踪旧别名使用，为清理提供数据
- `#[experimental("reason")]` derive 宏在 schema 生成时可过滤实验性字段

**实际生命周期案例**（来自 commit 历史）：

| Feature | 引入 | Experimental | Stable | Deprecated | Removed |
|---|---|---|---|---|---|
| `plan_tool` | `1b10a3a1b` | — | 直接毕业 | — | — |
| `personality` | `ce3d764ae` | `dfafc546a` | `11c912c4a` | — | — |
| `rmcp_client` | `e555a36c6` | — | — | — | `987dd7fde` |
| `undo` | — | — | — | `7a8407bbb` | `45727b9ed` |
| `search_tool` | — | — | — | — | 合入 `Apps` |
| `web_search_request` | — | — | — | `851617ff5` | — |

---

## 第二部分：工具与执行引擎

### 5. Shell 工具：四代演进

**为什么**：AI coding agent 的核心能力是执行 shell 命令。从简单的 fork+exec 开始，
用户需求逐步推动了交互能力、环境继承、per-command 审批的发展。

**解决的问题**：
- v1→v2：模型需要拼 `cd xxx && cmd` 的脆弱模式 → 结构化 `workdir` 参数
- v2→v3：无法与交互式程序（ssh/sudo/安装脚本）配合 → PTY 持久进程
- v3→v4：每次 fork 新进程开销高且不继承 shell 环境 → fork zsh 进程 + per-execve
  拦截

**关键 commit**：
- `e3b03eacc` — 引入 `exec_command` + `write_stdin`（+1096 行）
- `c09ed74a1` — 引入 `unified_exec`（PTY，+653 行）
- `29364f3a9` — 引入 `shell_command` tool（`command` 为 string 而非 array）
- `856f97f44` — 删除 `shell_command` feature flag（成为默认）
- `edacbf7b6` — 引入 `zsh_exec_bridge`（patched zsh + Unix socket IPC）
- `38f84b6b2` — **删除 `exec-server`**，escalation 逻辑移入 `shell-escalation`
  crate
- `3ca0e7673` — zsh-fork 切换到 `shell-escalation`，删除 `zsh_exec_bridge`

**设计文档**：`codex-rs/shell-escalation/README.md` 描述了 escalation 协议
（Run/Escalate/Deny）。

**优缺点**：

| 维度 | shell | shell_command | unified_exec | zsh_fork |
|---|---|---|---|---|
| 复杂度 | 低 | 中 | 高（PTY） | 高（patched zsh） |
| 交互能力 | 无 | 无 | 有 | 有 |
| 环境继承 | 无 | 无 | 无 | 完整 `.zshrc` |
| per-cmd 审批 | 不支持 | 不支持 | 不支持 | 支持 |
| 平台 | 全平台 | 全平台 | 需 ConPTY | 仅 zsh 用户 |

**权衡**：zsh_fork 通过 patched zsh 实现 per-execve 拦截，获得了最细粒度的审批能
力，但代价是需要维护一个 zsh 补丁和编译预构建的 zsh 二进制（10 个 OS 变体）。
`shell-escalation` crate 使用 FD-based IPC 而非 Unix domain socket——更难被沙箱内
进程篡改。

---

### 6. Apply Patch：从 JSON 包裹到自由文本

**为什么**：patch 内容含有大量换行符、引号、特殊字符。JSON 模式下模型必须正确转
义所有这些字符，导致大量无效输出——JSON 解析失败意味着整个 patch 丢失，模型必须重
新生成。

**解决的问题**：
- JSON 转义导致的大量无效 patch（模型经常生成无效 JSON）
- 错误不可恢复（parse error 丢失全部内容）
- JSON 包裹增加了不必要的 token 开销

**关键 commit**：
- `6df8e3531` — 添加 `apply_patch` 作为真实工具
- `236c4f76a` — Freeform `apply_patch` 用于 GPT-5 custom tools
- `415778831` — 因稳定性问题默认禁用 freeform
- `4764fc1ee` — Freeform apply_patch 作为迁移步骤（+in-flight output rewriting）
- `6f7511469` — 修复 Lark 语法：允许 patch 中的空行（`.+` → `.*`）

**优缺点**：

| 维度 | JSON Function | Freeform Grammar |
|---|---|---|
| 模型兼容性 | 所有模型 | 需要 freeform tool 支持 |
| 输出正确率 | 低 | 高 |
| 验证机制 | JSON schema | Lark grammar |
| token 效率 | 低（JSON 开销） | 高 |
| 错误诊断 | "JSON parse error" | 语法级定位 |

**权衡**：通过 `ModelInfo.apply_patch_tool_type` 让不同模型使用不同模式——新模型
用 freeform，旧/OSS 模型用 JSON。`legacy.rs` 保留了两个旧别名
（`include_apply_patch_tool`、`experimental_use_freeform_apply_patch`）映射到
`ApplyPatchFreeform`。

---

### 7. 工具执行流水线重构

**为什么**：随着工具种类增加（shell、apply_patch、MCP、unified_exec、
view_image...），散落在 `codex.rs` 和 `exec_command/` 中的执行逻辑变得不可维护。
沙箱、审批、执行三个关注点紧耦合。

**解决的问题**：
- 执行逻辑散落在多个文件中，无法统一添加遥测和钩子
- 沙箱策略与执行实现紧耦合
- 添加新工具需要修改多处代码

**关键 commit**：
- `5e4f3bbb0` — **rework tools execution workflow**（删除 3376 行旧代码）
  - 删除 `exec_command/` 和 `executor/` 模块
  - 新建 `tools/orchestrator.rs`（审批→沙箱→执行→重试）
  - 新建 `tools/runtimes/`（shell/apply_patch/unified_exec）
- `33d3ecbcc` — Tool Registry/Router/Handler 架构
- `dc3c6bf62` — 并行工具调用支持
- `aa04ea6bd` — Tool output 强类型化（`ToolOutput` trait）
- `d71e04269` — 强制每个 handler 单一输出类型

**新架构**：
```
模型输出 → Router (解析 tool call) → Registry (dispatch) → Orchestrator (审批+沙箱+重试) → Runtime (执行)
```

**权衡**：增加了一层抽象（Orchestrator），但统一了遥测、钩子、审批的注入点。
`ToolOutput` trait 强类型化增加了 handler 的样板代码，但确保了 output schema 关
联的正确性。

---

### 8. Exec Policy：三代策略引擎

**为什么**：安全命令审批经历了多次反复——最初在 TS CLI 中的 `safeCommands` 配置因
不安全的 shell 解析（`split(/\s+/)` 而非 `shell-quote`）被回滚。需要一个真正安全
的策略引擎。

**解决的问题**：
- v1 的正则匹配表达力不足且硬编码
- 管道命令只检查第一个命令的安全漏洞（`allowed | rm -rf ./`）
- 用户无法持久化审批决策

**关键 commit**：
- `58f0e5ab7` — 引入 `codex_execpolicy` crate（Rust，替代 TS 中被回滚的方案）
- `a941ae763` — `execpolicy2`（Starlark 语法，prefix_rule）
- `fb9849e1e` — **大重命名**：v1→`execpolicy-legacy`，v2→`execpolicy`（52 文件）
- `3d35cb461` — 修复管道命令安全漏洞（`fallback` 评估函数）
- `b148d98e0` — `host_executable()` 路径解析（basename-aware matching）
- `c3048ff90` — 网络审批持久化为 `network_rule()` 条目

**设计文档**：`codex-rs/execpolicy/README.md` 详细描述了 `prefix_rule()` 和
`host_executable()` 的语法和匹配语义。

**优缺点**：

| 维度 | Legacy (正则) | Starlark 规则 |
|---|---|---|
| 表达力 | 低 | 高（alternatives、justification） |
| 用户可配置 | 否 | `.rules` 文件 |
| 自测试 | 否 | `match`/`not_match` 内置验证 |
| 管道安全 | 有漏洞 | `fallback` 评估 |
| 学习成本 | 低 | 中 |

**权衡**：Starlark 解释执行比正则匹配稍慢，但 `match`/`not_match` 示例在加载时验
证——相当于规则的单元测试，大幅提升了安全性。旧引擎保留为 `execpolicy-legacy` 因
为某些场景仍依赖。

---

## 第三部分：模型与通信层

### 9. Model Provider：硬编码到可插拔

**为什么**：Codex 最初只支持 OpenAI API。用户需要使用 Azure、Ollama、LM Studio
等替代 provider。

**解决的问题**：打破了 OpenAI 供应商锁定，支持企业自建模型和本地开源模型。

**关键 commit**：
- `eafbc7561` — TS CLI 核心多 provider 支持（`responses.ts` 736 行，
  Responses→Chat Completions 转换层）
- `86022f097` — Rust CLI 引入 `ModelProviderInfo`
- `e924070ce` — Chat Completions API 支持（`wire_api` 字段）
- `928535084` — `--oss` flag + Ollama crate（+924 行）
- `837bc98a1` — LM Studio 支持
- `00cc00ead` — 引入 `ModelsManager`（集中管理模型发现）
- `222a49157` — 从磁盘加载模型（TTL + etag 缓存）

**权衡**：Responses-to-Chat 转换层增加了维护负担（每个新 API 特性都需要在两个路
径实现），最终导致了 Chat API 的移除（见下节）。

---

### 10. Chat Completions → Responses API 统一

**为什么**：维护两条 API 路径（Chat Completions + Responses API）意味着每个功能
都要实现两次。Responses API 已经是主路径。

**关键 commit**：
- `43e6e7531` — 发出 `wire_api = "chat"` 的 deprecation notice
- `d2394a249` — **删除 Chat Completions API**（-2900 行）
- `88598b940` — 删除 `wire_api` 字段
- `d5e724895` — 重构 `codex-api`（`streaming.rs` → `session.rs`）

**权衡**：保留了 `ollama-chat` 临时 provider 类型——显式声明为临时兼容桥接。

---

### 11. HTTP SSE → WebSocket 传输层

**为什么**：HTTP SSE 每轮都要重发完整上下文。WebSocket 可以维持 session 并增量
append。

**关键 commit**：
- `490c1c1fd` — 引入 `ModelClientSession`（+924 行）
- `e726a82c8` — WebSocket incremental append
- `e416e578b` — WebSocket preconnect warmup（+717 行）
- `a94505a92` — `premessage-deflate` 压缩
- `6d08298f4` — `UPGRADE_REQUIRED` 自动降级到 HTTP
- `770616414` — **WebSocket 成为默认**，移除所有 feature flag

**权衡**：WebSocket 增加了连接管理复杂度（reconnect、session state），但
`x-codex-turn-state` header 的 sticky routing 解决了同一 turn 的路由一致性问题。

---

### 12. ModelInfo/ModelFamily 合并

**为什么**：`ModelFamily` 是多余的中间层。`compaction_limit`、`context_window`
等属性应直接属于模型。

**关键 commit**：
- `f0dc6fd3c` — `openai_models` 重命名为 `models_manager`
- `9179c9dea` — **合并 ModelFamily 到 ModelInfo**（+964/-777 行）
- `40de81e7a` — 删除 `reasoning_format` 配置

**权衡**：合并后 `ModelInfo` 变大，但消除了"某个属性到底在 ModelInfo 还是
ModelFamily"的困惑。

---

## 第四部分：多代理协作

### 13. Multi-Agent：从扁平 ID 到层级命名

**为什么**：v1 用 UUID 寻址代理——模型必须在上下文中维护 UUID 映射表，消耗 token
且容易出错。消息缺乏来源信息，通信只有"立即触发"一种语义。

**解决的问题**：
- UUID 不可读 → AgentPath 命名路径
- 无结构化消息 → `InterAgentCommunication`（sender/recipient/content）
- 全量/不 fork 二选一 → `fork_turns` 精细控制
- 无代理发现 → `list_agents`
- 一律立即触发 → `send_message`（入队）vs `assign_task`（唤醒）

**关键 commit**：
- `568b938c8` — 首次 collab tool
- `1dd1355df` — 引入 `AgentControl` + `AgentBus`
- `188f79afe` — **第二天删除 AgentBus**（过度工程的快速纠正）
- `e41536944` — `collab` 重命名为 `multi_agent`
- `79ad7b247` — **UUID → path-based addressing**
- `450dc289c` — 拆分 `multi_agents_v2/` 模块
- `18f1a08bc` — `InterAgentCommunication` op 类型
- `1fc8aa0e1` — `fork_turns` 参数（`none`/`all`/N）
- `213756c9a` — mailbox-based wait（替代 target-based）

**优缺点**：

| 维度 | v1 | v2 |
|---|---|---|
| 模型认知负担 | 高（记 UUID） | 低（用名字） |
| 消息语义 | 模糊 | 清晰（queue vs trigger） |
| 内容类型 | 任意 | 仅文本（暂时限制） |
| 深度限制 | 禁用所有协作 | 仅禁止 spawn |
| 实现复杂度 | 低 | 高 |

**权衡**：v2 暂时只支持文本内容——先确保文本通信的语义正确，再扩展到多模态。v1 的
`resume_agent` 在 v2 中移除，因为路径可能已被复用。

---

## 第五部分：协议与 API 层

### 14. MCP 子系统：从零到全栈

**为什么**：MCP（Model Context Protocol）让 Codex 既能作为 MCP tool 被其他 AI 使
用，又能消费外部 MCP server 扩展工具能力。

**关键 commit**：
- `83961e029` — `mcp-types` crate（vendored schema codegen）
- `21cd953db` — `mcp-server` crate
- `2cf7aeeeb` — `mcp-client` crate
- `e555a36c6` — 引入 `rmcp-client`（官方 Rust SDK）
- `4cd6b0149` — **删除自研 stdio MCP 客户端**（-799 行）
- `66447d5d2` — **替换自研 mcp-types 为 rmcp**（-8249 行）
- `d9dbf4882` — **MCP/App Server 分离**
- `a26975466` — 删除旧 `mcp_protocol.rs`（-1857 行）

**权衡**：最初自研 MCP 类型是因为 rmcp 不够成熟。当 rmcp 成熟后，果断删除自研代
码（-7329 行）——不惧沉没成本。API schema 从强类型变为 `JsonValue` 以提高灵活性。

---

### 15. App-Server 协议 v1 → v2

**为什么**：v1 直接暴露了 core 内部的 Rust 类型（`snake_case`），core 重构直接破
坏 API。前端需要手动转换字段名。

**关键 commit**：
- `cdc3df379` — 拆分 `protocol.rs` 为 `v1.rs`/`v2.rs`/`common.rs`
- `658255492` — v2 Turn APIs
- `167158f93` — **删除 v1 RPC methods**（-7400 行，214 文件）
- `8da7e4bda` — 导出 v2 schema bundle（14607 行 JSON）

**设计文档**：`codex-rs/app-server/README.md`（1539 行）是完整的 API 文档。

**`v2_enum_from_core!` 宏**：14 处使用。如果 core 枚举新增变体但 v2 未跟进，
`match` 不完整导致编译报错——杜绝映射遗漏。

**命名统一**：
- `conversation` → `thread`（`116059c3a`，83 文件）
- `task` → `turn`（`1aed01e99`，58 文件）
- `assistant_message` → `agent_message`（`c405d8c06`）

---

### 16. codex-protocol crate 提取

**为什么**：协议类型散落在 `core` 和 `mcp-server` 中，存在重复定义。

**关键 commit**：
- `d26224472` — 引入 `codex-protocol` crate
- `097782c77` — 移入 `models.rs`
- `fc6cfd5ec` — `protocol-ts`（TypeScript codegen）
- `c9963b52e` — 合并三个重复的 reasoning 枚举

---

### 17. Realtime WebSocket v1 → v2

**为什么**：跟随 OpenAI Realtime API 协议升级。不跟进则语音功能会断。

**关键 commit**：
- `3e8f47169` — v2 事件解析器
- `eaf81d3f6` — 拆分 `protocol_v1.rs`/`protocol_v2.rs`/`protocol_common.rs`
- `69df12efb` — **删除 v1 WebSocket 实现**（"V2 is the way to go!"）

---

## 第六部分：安全与沙箱

### 18. 沙箱系统：五代演进

**为什么**：AI agent 执行任意代码必须有安全边界。随着平台扩展和企业需求，沙箱从
简单的权限列表发展为跨平台统一抽象 + 细粒度策略拆分。

**关键 commit**：
- `0a00b5ed2` — `SandboxPolicy` 从 enum 重构为 `Vec<SandboxPermission>`
- `89ef4efdc` — `codex-linux-sandbox` crate（arg0 trick）
- `0776d7835` — 沙箱配置重设计（`sandbox_mode` + `[sandbox_workspace_write]`）
- `77a8b7fde` — `codex sandbox {linux|macos}` 子命令
- `f956cc2a0` — vendored bubblewrap C 源码（+11261 行）
- `87cce88f4` — Windows 沙箱 Alpha（+2994 行）
- `13c0919bf` — Windows elevated sandbox（DPAPI + sandbox 用户 + 防火墙）
- `f82678b2a` — **Split FileSystem/Network Policy**（10 commit stack，+1477 行）
- `04892b4ce` — **bubblewrap 成为 Linux 默认**

**设计文档**：`codex-rs/linux-sandbox/README.md`

**权衡**：Split Policy 增加了配置复杂度但解决了"不受限文件系统 + 受限网络"的组合
需求。`use_legacy_landlock` 保留为回退——新 Landlock 不支持旧策略组合。

---

### 19. 审批系统：从布尔到 Guardian 自动审批

**为什么**：`suggest`/`full-auto` 二选一太粗。用户要么审批每一条命令（低效），要
么完全不审批（不安全）。

**关键 commit**：
- `725dd6be6` — 引入 `on-request` 审批策略
- `87666695b` — execpolicy TUI flow（交互式白名单）
- `425fff7ad` — `reject` 审批策略（独立控制 sandbox/rules/elicitations）
- `b7dba72db` — `reject` 重命名为 `granular`
- `e84ee33cc` — **Guardian 自动审批 MVP**（+2477 行）
- `4ad3b59de` — `request_permissions` 工具（模型主动申请权限）

**Guardian 设计**：锁定只读沙箱 + `approval_policy=never` 的子代理，使用独立模型
（preferring `gpt-5.4`）审查审批请求。`risk_score < 80` 自动批准。

**权衡**：Guardian 显著增加了 token 消耗（每次审批都要运行子代理），因此设为
Experimental。`request_permissions` 的权限跨 turn 持久化引入了状态管理复杂度。

---

### 20. 网络策略：从无到精细化代理

**为什么**：沙箱限制了文件系统访问，但网络访问缺乏同等的精细控制。

**关键 commit**：
- `77222492f` — 引入 `codex-network-proxy`（HTTP 代理 + 域名策略）
- `877b76bb9` — SOCKS5 支持
- `b527ee289` — 结构化网络审批
- `c3048ff90` — **审批持久化到 execpolicy**（`network_rule()` 条目）
- `8d3d58f99` — MITM 代理能力
- `b3202cbd5` — Linux bwrap netns 隔离（TCP-UDS-TCP bridge）

**设计文档**：`codex-rs/network-proxy/README.md`（220 行）详细描述了策略模型、安
全保证和限制。

**权衡**：域名级策略无法完全防止 DNS rebinding 攻击（需要更底层的 firewall/VPC
配合）。MITM 代理默认关闭（`mitm=false`）因为引入了 CA 管理的复杂度。

---

## 第七部分：状态管理与持久化

### 21. Conversation History → Context Manager

**为什么**：散落在 `codex.rs` 中的 `Vec<ResponseItem>` 直接操作导致多处重复的截
断、过滤、规范化逻辑。

**关键 commit**：
- `273819aaa` — 将 mutation 操作移入 `ConversationHistory`
- `722636539` — 集中截断逻辑
- `1a89f7001` — 拆分为
  `context_manager/`（`history.rs`/`normalize.rs`/`truncate.rs`）
- `2287d2afd` — 引入 `TurnContext`（替代 `sub_id`，28 文件重构）

---

### 22. Session Resume/Rollout 系统

**为什么**：用户需要在中断后继续会话。需要统一 resume 和 fork 的代码路径。

**关键 commit**：
- `d77b33ded` — 提取 rollout 为独立模块
- `43809a454` — 引入 `RolloutItems`
- `162e1235a` — **从 rollout 文件读取 fork**（统一 resume/fork 路径）
- `234c0a046` — TUI session resume picker
- `bbea6bbf7` — compaction 后的 resume 处理（838 行测试）
- `695957a34` — 统一 rollout 重建与 resume/fork TurnContext hydration

---

### 23. JSONL → SQLite 状态存储

**为什么**：JSONL 文件不支持高效查询、过滤和自动清理。

**关键 commit**：
- `3878c3dc7` — SQLite Phase 1（+2882 行，先镜像再验证）
- `4e6c6193a` — 日志拆分到 `logs_1.sqlite`（减少锁竞争）
- `ad98504d7` — 日志保留期 10 天
- `100eb6e6f` — DB-first 查询 + 文件回退

**权衡**：渐进迁移——先在 SQLite 中镜像 JSONL 数据并对比结果，验证无误后切换。

---

### 24. Memory 系统 v1 → v2

**为什么**：v1 的 per-cwd memory bucket 复杂且碎片化。需要统一的全局记忆管理。

**关键 commit**：
- `6049ff02a` — memories 模块基础
- `a6e9469fa` — **统一 memory job flow**（+2445/-3282 行）
- `07da740c8` ~ `674799d35` — Mem v2 PRs 1-6
- `f741fad5c` — Phase 1 清理（flat `phase1.rs`/`phase2.rs`/`start.rs`）

**设计文档**：`codex-rs/core/src/memories/README.md`（132 行）详细描述了两阶段
pipeline 的设计（Phase 1 per-rollout 提取，Phase 2 全局合并）和选择/遗忘机制。

**权衡**：v2 使用 DB 锁保证 Phase 2 全局串行，增加了 DB 争用但确保了记忆一致性。
Usage-based 选择替代了简单的 recency——常用的记忆优先保留。

---

## 第八部分：用户界面

### 25. TUI 架构：三次重写

**为什么**：最初的 TUI 是基础 Ratatui 框架。TUI2 实验尝试了 transcript-owned
viewport 但因终端兼容性问题退役。最终统一到 app-server 驱动的架构。

**关键 commit**：
- `d86270696` — TUI 视觉大修
- `d62b703a2` — 自定义 textarea（1294 行，替代 `tui-textarea`）
- `8068cc75f` — 自研 markdown 渲染器（替代 `tui_markdown`）
- `0c8828c5e` — 引入 feature-flagged TUI2
- `a489b64cb` — **退役 TUI2**（"combinatorial explosion of edge cases"）
- `db89b73a9` — 引入 `tui_app_server`
- `d65deec61` — **删除 legacy TUI**（-870 文件）
- `61429a6c1` — 重命名 `tui_app_server` → `tui`

**权衡**：放弃 TUI2 是务实的选择——终端模拟器/OS/输入模态/多路复用器/字体/主题
/alt-screen 的组合太多。统一到 app-server 驱动后，TUI 和 IDE 扩展共享同一 API。

---

### 26. Auth 系统：四代演进

**为什么**：从简单的 API key 到企业级多模式认证，每次演进都由新的部署场景驱动。

**关键 commit**：
- `515b6331b` — Rust CLI login（嵌入 Python 脚本）
- `e9b597cfa` — **Python login server 移植到 Rust**（-933 Python，+443 Rust）
- `ea01a5ffe` — 引入 `CodexAuth` 多模式抽象
- `dc42ec0eb` — 引入 `AuthManager`
- `377ab0c77` — **CodexAuth 从 struct 重构为 enum**（消除非法状态组合）
- `eb5b1b627` — 引入 `AuthStorage` 抽象（keyring 支持）
- `103acdfb0` — **统一 `ExternalAuth` trait**（泛化 bearer token 来源）

**权衡**：`CodexAuth` enum 化让非法状态不可表示（之前 struct 有 `PathBuf::new()`
这样的无意义默认值），但增加了 match 分支。Keyring 支持需要 OS 特定实现，hybrid
mode 提供了文件回退。

---

## 第九部分：生态系统

### 27. Hooks 引擎：从 notify 到完整事件系统

**为什么**：最初只有简单的用户通知机制。随着需求增长，需要在工具执行前后、用户输
入前、turn 结束时注入自定义逻辑。

**关键 commit**：
- `3b54fd733` — 引入 Hooks service（+608 行）
- `d735df1f5` — 提取到 `codex-hooks` crate
- `7112e1680` — `AfterToolUse` hook
- `244b2d53f` — **Hooks 引擎完整实现**（+4791 行）
- `73bbb07ba` — `PreToolUse` hook（可阻止工具执行）
- `6fef42165` — `UserPromptSubmit` hook（可阻止/修改用户输入）

**权衡**：Hook 同步阻塞 turn 推进——保证了执行顺序但可能增加延迟。并行执行多个匹
配的 hook 但聚合结果。

---

### 28. Skills 系统演化

**为什么**：用户需要可复用的提示模板，可以跨项目共享和版本管理。

**关键 commit**：
- `a8d5ad37b` — 实验性 `skills.md` 支持
- `5d77d4db6` — `SkillsManager` + `skills/list` API
- `da3869eeb` — SYSTEM skills 嵌入二进制
- `5b6911cb1` — Skill permission profiles（`openai.yaml`）
- `0bb152b01` — **移除 `SkillMetadata.permissions`**，统一到
  `permission_profile`
- `01fa4f021` — **移除特殊 execve 处理**（简化到标准路径）

**权衡**：权限从双重表示统一为单一来源，简化了但需要懒编译（在 zsh-fork
escalation 时）。

---

### 29. Plugin 生态系统

**为什么**：Skills 是静态模板，Plugin 需要更丰富的生态——marketplace、安装/卸载、
版本管理、MCP 集成。

**关键 commit**：
- `752402c4f` — 基础 plugin 加载（+1389 行）
- `024373430` — curated marketplace
- `b5f927b97` — GitHub HTTP-based 版本管理（替代 git clone）
- `6ad448b65` — `plugin/uninstall`
- `590cfa617` — `@plugin` 替代 `$plugin`（mention 语法）

**权衡**：Marketplace 从本地 git 切换到 GitHub HTTP + SHA-based 版本——减少了 git
依赖但需要网络访问。

---

## 30. 总结：Codex 演化的七个共同模式

纵观以上 29 个演化方向和 4979 个 commit，可以归纳出一致的工程模式：

### 1. 从具体到抽象
先用最简单的方案解决具体问题，在多个具体方案积累后提取统一抽象。
- shell 直接 fork → `ConfigShellToolType` 枚举
- 平台专属沙箱 → `SandboxManager`
- 散落的模型配置 → `ModelsManager`

### 2. 从隐式到显式
把靠约定或猜测的语义变成代码级的显式表达。
- `send_input`（语义模糊）→ `send_message` + `assign_task`（语义清晰）
- 布尔 flag → 枚举（`WebSearchMode`）
- struct with optional fields → enum（`CodexAuth`，消除非法状态）

### 3. 从耦合到分层
内部类型不再直接暴露给外部，允许两侧独立演进。
- app-server v2 API 层（`v2_enum_from_core!` 宏）
- `codex-protocol` crate 提取
- `codex-core` → 20+ 独立 crate

### 4. 渐进迁移而非大爆炸
feature flag 生命周期 + legacy 别名 + handler 别名，确保新旧版本共存。
- Chat API：先 deprecation notice → 再删代码 → 保留 `ollama-chat` 兼容
- ExecPolicy：v2 验证后才将 v1 改名为 `legacy`
- JSONL→SQLite：先镜像对比，再切换查询

### 5. 快速纠正过度工程
不怕尝试，但果断放弃不工作的方案。
- `AgentBus` 引入第二天删除（`1dd1355df` → `188f79afe`）
- TUI2 实验后退役（`0c8828c5e` → `a489b64cb`）
- streaming markdown 同天 revert（`2b7139859` → `52e12f2b6`）
- TS CLI `safeCommands` 因不安全 shell 解析被回滚（`ca7ab7656` → `d36d295a1`）

### 6. 数据驱动清理
遥测追踪 feature flag 和 legacy 别名使用量，有数据支撑地决定何时移除。
```rust
pub fn emit_metrics(&self, otel: &SessionTelemetry) { ... }
features.record_legacy_usage(alias_key, feature);
```

### 7. 安全左移
每次执行能力扩展都同步增加对应的安全约束。
- PTY 持久进程 → `unified_exec_allowed_in_environment()` 约束
- zsh fork → per-execve 审批（escalation protocol）
- 网络代理 → 域名级策略 + 审批持久化到 `.rules`
- Split sandbox policies → 独立的文件系统/网络策略

---

这套体系让 Codex 能够在保持线上稳定性的同时持续演进——4979 个 commit，29 条演化线
索，每一次"v2"都不是推倒重来，而是在 v1 的运行经验之上，解决实际遇到的问题。
