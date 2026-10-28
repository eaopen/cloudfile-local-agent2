# CloudFile Agent2 Provider → Local Bridge：共享文件 Core 的最小接入

> 2026-10-29 修订。架构决策详见 [双客户端/单文件数据目录](DUAL_STACK_COEXISTENCE.md)。本功能仅在 `feat/local-bridge-provider` 分支，原 Agent2 `dev` 不修改；Tauri 原样保留，Bridge 核心轻量化。

## 基本判断

不要把 Agent2 的完整文件引擎、Tauri、更新器或用户文件数据搬进 Bridge，也不要另起一份 `CloudFileBridgeProvider` 工作区/Journal。**文件数据必须仍由 Agent2 唯一管理**。第二套 Bridge 通过 Agent2 Provider 的受控 API 与现有文件任务协作，两条连接路线互不接管 Chrome/Host。

### 入口结构

```text
CloudFile Web → 原 CloudFile Chrome → 原 Native Host ────────┐
                                                           │
通用 Web → 新 Chrome → Local Bridge ─→ MCP CloudFile Provider┤
                                                           ↓
                                          Agent2 Core / Tauri / 唯一文件存储
                                           工作副本 / Journal / Intent / Guard
                                                           ↕
                                              CloudFile Hub / Fileserver
```

Bridge 独立承担 Agent（WorkBuddy/Codex/Pi）、本机 MCP、企业 Web 连接和通用任务显示；没有 CloudFile Provider 时上述非文件功能仍可以使用。

## 已实现的只读 Provider（开发分支）

- 在 **原 `cloudfile-native-host`** 中新增可选 `--bridge-provider` 模式，与 Chrome 的 Native Messaging 四字节长度帧入口明确分开。它使用 MCP stdio JSONL，接受本机手动注册的 Bridge 调用，**不改变旧 Chrome Host 的协议、名称或注册信息**。
- `cloudfile_status`：返回 Agent2 组件版本/平台/IPC/Journal 版本与 `storage_scope=agent2_shared`；不读取文件内容、密钥、票据。
- `cloudfile_task_counts`：基于原 `session::tasks` 与 `journal::pending` 读取同一个 Agent2 数据根，统计本地查看/普通编辑/独占编辑、待确认 Intent 的数量；不返回路径、文件名、任务 ID；**不冒充云端发布已完成**。
- 两个工具**只读**，没有上传、Commit、Checkin、Checkout、重试、Shell 或其它任意本机工具调用。
- 复用现有 `storage::root()`，包括 `CLOUDFILE_AGENT2_HOME`（若用户配置）和原始默认路径；**不是**一个新的安装/数据根。Bridge 的配置与授权目录仍独立。
- 两种入口的用户文件任务由 Agent2 掌握，Bridge 端不建第二份任务数据库、Journal/Guard、用户工作副本或新身份体系。

### 最短调用示例

```sh
local-bridge provider add cloudfile /absolute/installed/cloudfile-native-host --bridge-provider
local-bridge provider tools cloudfile
local-bridge provider call cloudfile cloudfile_status '{}'
local-bridge provider call cloudfile cloudfile_task_counts '{}'
local-bridge provider remove cloudfile # 只解除 Bridge 注册，不删除 Agent2 任何数据
```

用户必须手动选择受信可执行程序。Bridge 当前会固定该可执行文件的绝对路径，而 Agent2 更新用版本目录：**升级后的定位/重绑定需要另行解决**。这个问题不能通过使用第二套空文件目录规避；也不需要默认接管旧 Chrome 的 Native Host Manifest。只读状态可先工作，真实运行版本与数据根必须一致。

### P1 已在独立 Provider 分支新增（未合并 Agent2 dev）

`cloudfile_task_summaries` 无参数、只读且最多返回 80 条脱敏任务摘要：仅包含原 Agent2 的 `task_id`、`mode`、`status`、阶段、待确认 Intent/冲突/Checkout 标识；不返回文件名、服务器地址、库 ID、远程/本地路径、票据或设备凭据。超过上限明确报告 `truncated`。原 Journal 中有未确认提交时映射为 `unknown`，冲突为 `needs_action`，确认完成的独占 Checkin 才可映射 `completed`；其他本地文件任务保守显示 `running`，不把本地保存等同于云端发布。损坏的 Journal 返回工具错误，不伪装为空任务列表。

Bridge 初期仅在用户本地显式调用 `tasks list --cloudfile` 时读取该工具，**不自动开放给网站或模型**。旧 Agent2 主文件链、Tauri、原 Chrome Host/数据根不变。

## 避免竞争与迁移顺序

| 阶段 | Bridge/Provider | 仍由 Agent2 负责 |
| --- | --- | --- |
| P0：只读状态（已写代码） | MCP 握手、组件与共享文件任务计数 | 所有文件主链、Tauri、设备凭据、Journal、Guard、更新 |
| P1：统一任务摘要 | 通过已有任务源显示摘要，不产生新的 task_id | 任务状态事实、冲突/未知回执、允许操作的最终判定 |
| P2：文件调用接线 | 有界受理、查询原 operation、返回真实状态；不自动跨入口重试 | File Core 唯一执行写操作，沿用原锁/Journal/Intent、Guard 与 Fileserver |
| P3：逐渐转移通用 UI | 可选 Bridge 任务入口、通知、通用工具展示 | Agent2 Tauri 继续有效，文件特有 UI/应用规则保持 |
| 未来升级合并 | 必须有单独决策、兼容测试和回滚方案 | 一份文件存储原位保留，不搬两套目录或双写 |

Provider 是只读时可在单独子进程读取原 Agent2 私有文件记录；**开放任何写能力之前**，必须只通过现有 Agent2 单实例 Desktop/IPC 提交，验证服务端 scope 与授权。禁止一个 Native Host Writer 和一个 Provider Writer 并存、禁止 Provider 独立 renew/commit、禁止写丢回执时重新发一个新 Intent。`accepted` 不是发布成功；`unknown` 只能查询原操作。

自定义 `CLOUDFILE_AGENT2_HOME` 在原 Agent2 和 Provider 的进程环境中须指向同一有效路径；不能把错误的新根自动创建为“0 个任务”却报告已连接。正式接入时优先使用受信 Agent2 配置确定路径，并测试读写权限与版本兼容，不另造文件存储注册系统。Dev/Prod 两个 Agent2 安装如果已经隔离，各自有自己的一份文件根，**同一实例内部只有一份**。

## 双产品隔离不变

第一套：`cloudfile-local-agent2 + cloudfile-chrome-extension`。第二套：`local-bridge + local-bridge-chrome-extension + Provider`。两套 Chrome 扩展 ID、Host（CloudFile `com.cloudfile.current_agent` vs Bridge `com.eaopen.browser_bridge`）、发行通道和 Browser 权限都独立；Bridge 失效不影响第一套文件任务。Provider 复用同一个 Agent2 文件目录是**刻意设计**，不是跨 Chrome 权限共享，也不是两套上传引擎。

**不做**：CloudFile Chrome 改连 Bridge、Bridge 接管旧 Host、另建第二份 CloudFile 文件存储、把 Tauri 强制搬入 Bridge Core、写两个 Journal 或为了预留未来能力开发动态插件平台。

## 完成条件与边界

现有 PR 的 Rust/测试 CI 与旧 Agent2 `dev` 的已知 HTML/JS 文案失败要分开判断。本次设计不做真实账号、文件写入或修改 Hub/Fileserver；只有 P2 真正接入写操作时才需要 Windows/macOS + Chrome + 真实 CloudFile 服务的查看/编辑/独占验收。

关联：[Agent2 Provider Draft PR #26](https://github.com/eaopen/cloudfile-local-agent2/pull/26)、
[Bridge 两套入口迁移合同](https://github.com/eaopen/local-bridge/blob/docs/dual-independent-stacks-until-merge/docs/CLOUDFILE_TO_BRIDGE_MIGRATION.md)。
