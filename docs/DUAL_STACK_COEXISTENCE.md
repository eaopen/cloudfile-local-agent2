# CloudFile 与 Local Bridge：两套入口长期并行、一个文件数据事实源

> 决策修订：2026-10-29。**独立的是两套产品入口和 Chrome/Native Host/发行链；CloudFile 文件数据目录只有一份，由现有 Agent2 管理。** 之前关于 `CloudFileBridgeProvider` 独立工作区、第二份 Journal/Guard/文件引擎的表述已撤销。
> 本设计只在 `feat/local-bridge-provider` 开发，CloudFile Agent2 既有 `dev`、Chrome 扩展以及 Local Bridge `dev` 都不会因本方案自动切换。

## 两套前端/连接产品，公用一个 CloudFile 文件能力

| | ① CloudFile 已有体系 | ② Local Bridge 新体系 |
| --- | --- | --- |
| Chrome | 原 `cloudfile-chrome-extension`；原扩展 ID 不变 | 新 `local-bridge-chrome-extension`；独立扩展 ID |
| Chrome Native Host | 原 Windows `com.cloudfile.current_agent` / macOS `com.cloudfile.agent2`；既有开发 Host 保留 | `com.eaopen.browser_bridge`；绝不注册或覆盖原 CloudFile Host |
| 用户入口 | 原 CloudFile Web + Agent2 Tauri/任务中心 | 新 Chrome + Local Bridge；AI/MCP/通用任务展示可独立演进 |
| CloudFile Provider | 当前 Chrome Host 本身具备文件功能 | 本机用户显式注册的 `cloudfile-native-host --bridge-provider`（Provider 分支功能） |
| **文件工作副本、Journal、Guard、设备凭据** | **唯一的 Agent2 文件数据目录** | **复用同一目录、同一 CloudFile 文件事实源**；不建立第二套 |
| 文件提交与后台执行 | **Agent2 Desktop / CloudFile Core 是唯一文件操作 Owner** | Bridge 只转交受控操作，**不得自己直接写 Journal、另起第二个文件写入者或自动重试** |
| 发行 | 原 CloudFile Chrome/Agent2 持续独立发行 | Bridge/新 Chrome 独立发行；Provider 协议增量演进而非重装一份文件系统 |

现有 CloudFile 功能不依赖 Bridge。Bridge/新 Chrome 即使未安装 Agent2，也能完成自己的 AI/MCP/网页接力功能，但 **CloudFile 文件能力需要本机已有并兼容的 Agent2 Provider**。这是可选依赖，不是复制一份新的 CloudFile 文件引擎。

## 实际调用与数据归属

```text
① CloudFile Web → 原 CloudFile Chrome → 原 CloudFile Native Host ─┐
                                                              ├─→ 唯一 Agent2 Desktop / Core
② 通用 Web → 新 Chrome → Local Bridge → Agent2 Provider (MCP) ─┘            │
      └── AI / 通讯 / 其他 MCP 独立运行                                      │
                                                                             ▼
                                                                唯一文件数据目录
                                                          工作副本 / Journal / Guard
                                                                             ↕
                                                                CloudFile Hub/Fileserver
```

- **单文件任务事实源**：CloudFile 的 `task_id`、工作副本、File ID、Intent、Guard/Checkout 保持原值。Provider 查询这些状态时只读取 Agent2 自有文件存储，Bridge 仅接收脱敏摘要。
- **单写入 Owner**：目前 Provider **仅提供只读 `cloudfile_status` / `cloudfile_task_counts`**，可以通过现有 `session::tasks` 和 `journal::pending` 读取同一目录；不会上传、续租、Commit 或 Checkin。未来写能力必须通过 Agent2 现有常驻 Desktop/IPC、原 Journal 锁和原文件状态机串行/幂等处理，禁止 Provider 自建上传/租约执行器。
- **同一目录不等于所有产品数据混在一起**：Bridge 自己的注册、Agent 会话、站点授权和 AI 收件箱仍在 Bridge 的私有目录。**只有 CloudFile 专业文件数据公用 Agent2 目录**。
- **按 Agent2 的实际数据根定位**：Provider 复用 `storage::root()`，遵从原 `CLOUDFILE_AGENT2_HOME` 和原 Agent2 默认路径选择。Bridge 不能强制指定第二份文件根。若原部署使用自定义路径，Provider 必须获得同一有效配置；根不一致时不得伪装成“没有任务”，应报告未绑定/不可用。
- **版本与并发**：两个入口可能同时操作同一文件，旧/新客户端必须共享原 `task_id` / Journal / 文件锁 / 幂等回执。正式开放写操作前，Provider 与当前 Agent2 文件协议/Journal 版本必须兼容；跨版本不明、未知提交、权限过期或租约异常时拒绝新写入，只查询原任务状态。
- **客户端独立**：Chrome 的来源校验、Host ID、扩展发行、AI/MCP 授权仍分别管理；一个 Chrome 扩展的安装/卸载不修改另一套的注册，不删除共同文件目录。

## 现在与未来

**P0（当前开发分支）**：通过 Agent2 现有 `cloudfile-native-host --bridge-provider` 增量提供 MCP stdio，只读组件状态和同一个 Agent2 文件目录的模式/待确认提交计数。仍是本机显式配置的可选 Bridge Provider。Chrome 原有 Native Messaging 的二进制帧模式不变。这个分支没有合并 Agent2 `dev`，不代表已经在用户机安装。

**P1**：Bridge 用只读任务摘要展示**同一份 CloudFile 文件任务**。Agent2 Tauri 继续存在；Bridge 的列表不成为第二份任务账本。不能因两个入口都展示就创建新任务 ID。

**P2**：按本地查看 → 普通编辑 → 独占编辑分别接入，调用回 Agent2 的现有可靠受理/状态查询接口。上传、Checkout/Commit/Checkin 只有 Agent2 执行与终判；请求已受理后断线不得跨 Host 重发，必须查询原操作。

**P3**：Bridge 可选实现通用桌面入口/通知；Agent2 继续专注专业文件引擎，Tauri 仍保留到单独批准的升级合并。即便将来统一 UI，也无需复制、重建或迁移已有 CloudFile 文件目录。

**正式合并升级门禁**：用户单独批准后，才讨论两套 Chrome/Host 的实际合并。需要验证旧扩展兼容、唯一数据根与工作副本、未知 Intent、Guard 续租、服务端版本、Windows Office/CAD、Dev/Stable 签名升级与回退，且不破坏进行中任务。没有批准及验收，不强制下线旧 Chrome/Tauri。

## Provider P0 开发使用

```sh
# 只能由本机用户注册已经提供 --bridge-provider 模式的兼容 Agent2 Native Host
local-bridge provider add cloudfile /absolute/path/to/cloudfile-native-host --bridge-provider
local-bridge provider tools cloudfile
local-bridge provider call cloudfile cloudfile_status '{}'
local-bridge provider call cloudfile cloudfile_task_counts '{}'
```

**注意**：上述命令不会注册第二个 CloudFile Chrome Host，也不会启动文件写入。`cloudfile_task_counts` 统计 Agent2 当前数据目录，而不是一个新的空文件库；本机安装路径与自定义数据根必须通过真实环境核对。

原则：**两套连接产品长期独立；一个 CloudFile 文件引擎、一份 CloudFile 文件数据，避免两个写入者竞争。**
