# CloudFile Agent2 → Local Bridge：Provider 路线与完整迁移比较

> 日期：2026-10-28；状态：**已在 Provider 独立分支实现 P0 只读 MCP 入口，尚未合并 dev；不改变旧运行路径、桌面 UI 或正式发行**。
> Agent2 基线：`adade250`（2.0.12）；Local Bridge 基线：`ace22302`（Web V2 Agent 统一后）。
> 关联 [Agent2 Bridge 兼容合同](BRIDGE_COMPAT_V1.md)、[Agent2 #21](https://github.com/eaopen/cloudfile-local-agent2/issues/21)、[Bridge Migration](https://github.com/eaopen/local-bridge/blob/dev/docs/CLOUDFILE_TO_BRIDGE_MIGRATION.md) 和 [BridgeTask](https://github.com/eaopen/local-bridge/blob/dev/docs/BRIDGE_TASK_V1.md)。

## 决策

**推荐 B：Agent2 继续独立负责 CloudFile 文件引擎，新增受控 Provider 入口；Bridge 统一连接、能力发现和可选任务聚合。**

进一步分成两层：B0 **先只接 Provider，不迁核心代码**；B1 只有在同一份通用逻辑已经被两仓实际复用且重复维护产生问题时，才抽小块可移植合同。用户最终可以只有一个统一入口或安装体验，但**一个入口不等于一个二进制、一个进程或一个状态数据库**。

CloudFile 是 `cloudfile` 文件能力 Provider，**不是** `runAgent(agent=workbuddy/codex/pi)` 的第四种大模型 Agent。文件能力使用 `cloudfile.task.*` / `cloudfile.file.*`，不把文件提交塞进 AI 任务执行接口。

## 本轮明确的产品决策

- **保留 Agent2 的 `dev` 不变**：文件 V2 主链、Tauri 任务中心、Windows/Mac 现有 Host/Chrome、安装/更新/工作区继续由 Agent2 维护；Provider 的功能提交只进入 `feat/local-bridge-provider` 分支。
- **Local Bridge 保持轻量**：只做通用 MCP/HTTP/Native 通信、能力注册与任务摘要聚合，不接入 Agent2 的 Tauri 或文件上传、密钥、Guard、升级实现；本轮不要求启动 Bridge daemon。
- **渐进迁移的是“通用职责”，不是整套 Rust 文件代码**。当统一任务中心/托盘/UI/安装引导确有真实价值且已验收后，才将对应通用壳层迁至 Bridge 的**可选 UI/发行组件**；Agent2 最终专注文件服务端票据、原生文件操作、Journal、Guard、工作副本和文件类型专属打开规则。
- **保留独立回退**：只要 Bridge Provider 缺失、不可用或新能力失败，原 CloudFile Chrome → Native Host → Tauri/Desktop 继续完成所有文件任务，既有任务不跨 Provider 重交，不自动删除用户工作副本。

## 当前源码基线与能力差异

| 维度 | CloudFile Agent2 | Local Bridge |
| --- | --- | --- |
| 文件三模式 | `local-view` / `optimistic-edit` / `exclusive-edit`；Hub 限范围票据、Seafile 原生提交 | 仅 `--component` 只读状态探测；**没有**文件执行 Adapter |
| 文件状态事实 | 工作副本、Journal、Intent、快照、Checkout Guard、冲突、原生 File ID / 发布回执 | BridgeTask 是结构合同；无统一文件执行账本 |
| 用户界面 | 已有 Tauri 任务中心、托盘、悬浮入口、应用打开规则 | CLI、Native Host、Web V2；尚无可替代 Agent2 的完整任务中心 |
| 运行与 IPC | Desktop 单实例 + 私有文件邮箱 IPC；部分 `ipc::call` 可等 330 秒 | MCP Provider 按调用启动 stdio 子进程；当前同步调用超时 90 秒；Web V2 HTTP/SSE 主要支持 Agent 文本事件 |
| 安装与升级 | 自带引导、Host 注册、P-256 签名、版本目录、健康检查、回退、OS 数据目录 | 通用 Bridge/Chrome 独立安装；不能直接替代 Agent2 升级器 |
| 兼容身份 | Windows `com.cloudfile.current_agent`；macOS `com.cloudfile.agent2`；开发 `com.cloudfile.agent22.dev` | 独立通用 Chrome/Native Host，不接管旧 CloudFile Host |
| 配置 | 现有 CloudFile 服务、凭据、工作区和可选 ApplicationProfile V1 标签 | 通用 Profile V1/V2、按 Origin 的 Agent/MCP 授权；不承担文件服务端权限 |

Agent2 `dev` 当前 CI 的 Rust fmt/Clippy/Workspace 测试通过，但任务中心 UI 集成测试有 1 项状态文案断言失败，使整体 CI 为红；独立 Linux 功能 PR #19 尚未合并。创建 Provider 分支**不代表主线已可发布**。

## 两个主要选项

| 考量 | A. 全量迁进 Local Bridge | B. Agent2 独立 Provider（推荐） |
| --- | --- | --- |
| 代码 | Bridge 接收 Core、Native Host、Desktop、Journal、安装更新、文件操作 | Agent2 保留上述代码，新增窄 Provider 边界；Bridge 加通用消费者 |
| 文件可靠性 | 要重验整个持久化与更新生命周期；变动大 | 依赖原文件事实源，尽量不迁数据 |
| 研发量 | 高：Tauri/Windows WebView2/独占编辑/更新器统一改造 | 低到中：先状态与摘要，再有界异步动作 |
| 安装/UI | 最终理论上最统一；初期风险最高 | 先共存；成熟后可做统一启动器/任务入口，无需同进程 |
| 失败隔离与回退 | Bridge 故障可能影响所有文件操作 | Bridge 不可用时仍走 CloudFile 原 Chrome/Host/Desktop |
| 跨项目复用 | 单仓源码共用，但文件引擎强耦合通用产品 | 通用协议、路由、任务显示共用；文件引擎归 Agent2 |
| 当前建议 | **不作为近期工作** | **在新分支做最小垂直切片** |

## 推荐依赖图

```text
现有 CloudFile Web/Chrome ──→ 现有 CloudFile Native Host ──→ Agent2 Desktop
       │                           （保留，正式文件主链）       │
       │                                                       ▼
       │                                                CloudFile Core
       │                                           工作副本 / Journal / Guard
       │                                                ↕ Hub / Server
       │
通用 Web / Chrome ──→ Local Bridge（独立） ──→ CloudFile Provider（Agent2 新入口）
                         │      │                          │
                         │      └── AI Agent / MCP          └── 同一套 CloudFile Core/任务状态
                         └── 任务摘要聚合
```

Bridge 不认识 Etech、具体企业账号或库业务规则；Etech 提供应用 Profile 和当前用户经现有 SSO 验证的业务上下文；CloudFile Hub/Fileserver 最终验证文件用户/库/路径/模式权限。Bridge 不读取浏览器 Cookie，不保存设备私钥，不从普通网页接收可执行程序和本机路径。

## Provider V1 最小合同

- `provider_id=cloudfile`，`owner_module=cloudfile`，稳定 `schema_version=1`；能力按实际实现发布，不把未来计划列为可执行。
- `provider.status`：组件版本/平台/是否可访问、受信绑定状态，**不是**读取凭据或完整 Host 信息。
- `provider.tasks.list`：默认只在已授权的当前 OS 用户本地 Bridge 视图返回脱敏任务摘要；站点、AI/MCP 的可见范围须额外收敛，不直接公开原 `native::task_summary` 全量结果。
- `provider.task.get`：按 `(profile_id, owner_module, task_id)` 查一条摘要、模式、待处理/冲突/Checkout 和可操作项；不能把 `profile_id` 当业务 ACL。
- `provider.action.request`（后续单独启用）：限 `open / upload / commit / checkin / recover` 等**明确类型化动作**，以现有 CloudFile 任务 ID/新 Hub scope ticket 和 Agent2 的最终授权执行；禁止文件绝对路径、EXE、Shell/argv、跨任务任意写入。
- `provider.operation.get`：查询**原操作**真实状态/回执，结果未知只查询，不由 Bridge 生成新 Intent 或重发一次性票据。

### 状态与授权的两层判定

Provider 的 `installed / available` 仅表示组件可被 Bridge 连接；文件动作的 `authorized` 必须由 Agent2 + 原 CloudFile Server 对本次票据、模式和业务范围终判。

不能将 Agent2 UI 的分组 `needs-attention / in-progress / done` 生硬变成 BridgeTask 的 `failed / running / completed`：

- `needs-attention` 通常映射 `needs_action` 并带具体原因；pending / unknown publish 仍标为 `unknown` 或 `needs_action`，绝不推断上传失败。
- `in-progress` 表示工作副本或 Checkout 仍在使用，不等于新云版本已提交。
- `done` 只是 UI 分组；须由 Agent2 明确 `published/closed/opened` 等事实确认相应**操作**完成，不可把正在编辑但本地未修改的任务直接标为整条文件任务 completed。
- `available_actions` 只能由 Agent2 基于真实状态计算；Bridge 不能靠扩展名、模型回复或网页按钮猜测 `commit/checkin` 是否可用。

## 首次接入示例（仅 Bridge 本机 CLI，不自动开放网页权限）

```sh
# 使用用户手动确认的已安装版本路径；不要让网页或模型传可执行文件路径。
local-bridge provider add cloudfile /absolute/installed/cloudfile-native-host --bridge-provider
local-bridge provider tools cloudfile
local-bridge provider call cloudfile cloudfile_status '{}'
local-bridge provider call cloudfile cloudfile_task_counts '{}'
# 从 Bridge 注销，不影响 CloudFile 程序、Chrome Host、工作副本及 Journal
local-bridge provider remove cloudfile
```

两种工具均为**只读**；`cloudfile_status` 只返回组件版本/平台/协议，`cloudfile_task_counts` 只返回当前 OS 用户任务数量与按模式统计（含原 Journal pending 数）。均不公开文件名、本机路径、业务服务、设备私钥、票据、任务 ID，也**不推导**文件已发布或拥有写权限。任务目录不可读取时必须返回不可用，不能显示为 0 个任务。

当前 Bridge 的通用 MCP Provider 注册与调用已足以执行这两个工具，**无需修改 Bridge 的运行时代码**。Bridge Web V2 不会自动获得 Provider 工具调用权限。CLI 调用是本机用户的显式操作，不能等同于任意网站、Agent 已获得文件权限。

## 初始实现边界（新分支）

### P0：只读 Provider，保持零侵入

**当前 Provider 分支已新增**现有 `cloudfile-native-host --bridge-provider` 独立入口，复用 Agent2 Core 与私有数据目录，不新建守护进程、Tauri 入口或文件账本。该模式输出 MCP stdio JSON-RPC（换行分隔），与 Chrome Native Messaging 的四字节长度帧是**两套隔离的启动模式**。从 Chrome 发起时仍必须带受信扩展身份，不能通过普通网页消息切换成 MCP 模式。

Bridge 已有用户显式注册的 `mcp_provider` 机制，可使用固定绝对路径 + 固定 `--bridge-provider` 参数；P0 只公布 `cloudfile_status`、可脱敏的 `cloudfile_task_counts`。短会话可以承受 Bridge 当前 90 秒超时，不做文件打开/上传/独占动作。现有 Bridge 自带 `cloudfile.status` 探针在此阶段保持兼容，避免一次性切掉可用路径。

**安装路径的真实兼容点**：Agent2 安装使用 `bin/<version>/cloudfile-native-host`（按版本目录切换），而 Bridge 目前 `provider add` 会把路径 `canonicalize` 后固化为绝对文件位置。直接注册版本目录里的 Host **会在升级/裁剪旧版本后失效**。DEV 只读实验可以暂用明确版本路径；正式方案应由 Bridge 的 CloudFile 专用适配器只读解析当前用户已注册且所有权可信的 **Agent2 Host Manifest/注册项**，在每次打开短会话时定位活动二进制，并验证 `agent_family` / 组件协议，不扩大通用 MCP Provider 的任意程序发现能力。另一种备选是 Agent2 自有稳定 launcher，但它需要额外验证签名升级与 Windows 杀软兼容；P0 不因这项未来稳定化而重写安装器。

Provider 状态查询优先复用轻量 `--component` 或纯本地投影，避免直接调用会进行联网检查的完整 `native::status`；任务列表读取要有合理上限，不让大任务目录扫描拖慢 Bridge 首屏。

这一步在独立分支只新增入口/测试，不改 `run_stdio` 默认 Chrome 模式，不改原 `status`、`open_uri`、`task_summary`、Host Manifest、安装/升级、工作目录、Journal 和服务端协议。即使 Provider 缺失、安装失败、Bridge 不在，Agent2 正式文件主链不变。

### P1：只读任务汇总

抽取 Agent2 **现有任务中心的只读状态投影**供 Desktop 和 Provider 共用，而不是将任务中心搬到 Bridge。不要直接复用较简化的 `native::task_summary` 冒充完整 Guard/Intent/冲突状态。

BridgeTask 先读取脱敏、当前本机用户许可的摘要；Bridge 不持久化另一份业务任务账本，不展示本机绝对工作目录或票据，不把服务端未确认状态算完成。Bridge UI 不可用时 Agent2 自有任务中心照常工作。

### P2：少量受控文件动作（必须真实验收后）

按 `view → optimistic-edit → exclusive-edit` 顺序逐项接入。必须先解决目前 `ipc::call` 可能同步等待 330 秒的问题：**受理需落盘后快速回 `accepted/operation_id`，执行由 Agent2 自有 Desktop/Journal 持续运行，Bridge 只查询/订阅原操作。** 当前 MCP Provider 90 秒单次进程接口不作为长文件任务执行通道；实际动作用 Agent2 已存在的私有 IPC 增量做有界受理/查询，或独立认证的本机 IPC，不额外引入通用远程 Shell/HTTP 写入服务。

服务端 V2 限范围票据依然由原登录网页申请、由 Agent2 消费；Bridge 只能经过已批准来源转送，不修改权限、不能重复消费结果未知的票据。收到 `accepted` 不代表文件已打开、已上传或已 checkin。

### P3：通用壳层逐步迁到 Bridge，文件核心继续由 Agent2 管理

可选将 Bridge 作为统一快捷入口，复用 Agent2 任务摘要，按 `focus_task` 跳转 Agent2 的详细任务中心。等真正降低用户操作后，再评估统一托盘/安装向导/通知入口。**统一安装包也可内含两个独立程序**，无需合并它们的签名、IPC/Journal 或退出/升级控制；Agent2 活动 Checkout/待确认 Intent 时不得被 Bridge 升级操作强制终止。

## 必须保留的兼容与隔离

1. Windows 正式 `com.cloudfile.current_agent`、macOS 正式 `com.cloudfile.agent2`、当前开发 `com.cloudfile.agent22.dev` 及对应 Chrome 扩展 ID 不变，不被通用 Bridge Host 自动替换；历史设计里的未来新 Dev Host 命名尚未合入，不在本轮直接改名。
2. Agent2 自己决定工作区、设备凭据、守护任务、Journal、Guard、签名更新与回退。Bridge 卸载/升级不能删工作副本、结束租约或迁移未知任务。
3. 发行通道 `stable/dev` 与服务端部署环境 `DEV/PROD` 是两条独立轴；Provider 绑定和任务查询不能跨通道/原应用实例误路由。
4. Bridge 注册 Provider 只能通过本机用户指定的已安装可执行文件，Web/Profile/Agent 提示词不得指定本机路径、执行命令、安装版本或自动接管 Host。
5. 当前 Bridge Web V2 只开放受限状态 MCP，不是任意 `tools/call`；P0 Provider **不自动扩大网页权限**。
6. 不给 Provider 新建文件上传/Checkout 数据库和第二套通用限流、DLP、企业 SSO、设备登录机制。

## 先决技术债（不要混入架构大重构）

- Agent2 当前 `dev` CI 的 Rust 检查与 Workspace 单测通过，任务中心 HTML/JS 状态文案测试 1 项失败；在 Provider 代码合并前先修/隔离这条现有失败，不能把红 CI 当作迁移功能故障。
- Agent2 的 `README` 关于手动上传与 2026-10-19 三模式验收文档中的“普通编辑自动上传”存在口径差异。新 Provider 必须依赖**当前代码支持的有效动作**，先统一真实用户语义，不能因为旧文档而自动执行写入。
- 既有 Linux 支持 PR #19 仍开放，Windows 文件功能与升级兼容优先；Linux Provider 只读可以独立开发，但不得把 PR #19 的实现当成已合入当前 `dev`。
- 第一次 P0 只需 Agent2 Provider 只读契约测试与 Bridge 本机模拟 MCP 握手、状态/任务摘要检查；不安排重复的重型文件端到端验证。真正接入写能力时再做真实 Windows/CloudFile 服务器验收。

## 暂缓 / 不做

不整体复制或搬迁 `cloudfile-core`、`v2_edit.rs`、`journal.rs`、`guard.rs`、`update.rs`、Tauri UI、WebView2 和安装器到 Bridge；不更换原文件 Host/Chrome 主链；不写双数据库、双上传引擎或动态插件市场。只有真实重复劳动出现时，再考虑抽一个独立的小型共享 `provider-contract` schema/测试样例，避免跨仓源码耦合。
