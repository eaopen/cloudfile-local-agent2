# CloudFile Agent2 ↔ Local Bridge V1：适配合同与存量保护

状态：**设计与跨仓对接约束，当前 Agent2 文件主链不因本合同发生变化**。通用 Profile/Task/Capability 合同以 [Local Bridge](https://github.com/eaopen/local-bridge/blob/dev/docs/APPLICATION_PROFILE_V1.md) 为准。

## 职责与依赖

- CloudFile Agent2 是文件能力提供者；可认识 Local Bridge，暴露受限的任务摘要和文件能力，但保持原有网页/Chrome/Host 的直接调用路径。
- Local Bridge 是通用能力路由/统一任务中心；可认识 CloudFile 的受限适配器；不应依赖任何上层企业应用的业务对象或用户名格式。
- 上层应用通过自己提供的 ApplicationProfile 与 TaskContext 接入，Agent2 不应针对某个项目名称或 Profile 添加文件权限例外。

## 当前直连 Agent2 与未来 Bridge 共用 Profile V1

**现役运行时是 CloudFile Rust Agent2，不是 Local Bridge。** 上层应用可从现在开始声明 [通用 ApplicationProfile V1](https://github.com/eaopen/local-bridge/blob/dev/docs/APPLICATION_PROFILE_V1.md)，但不能因此要求安装 Bridge 才能打开或编辑文件。当前网页/Chrome/Host 直连 CloudFile Rust Agent2 的全部文件操作保持原样。

一份应用 Profile 的 `profile_id`、`origin`、`application`、`services` 和 `channels` 跨版本稳定，文件通道 `module=cloudfile` 及 `cloudfile.file.view/edit/exclusive` 表达**逻辑文件能力**而不是可执行程序或 Host 名称。

- **现在**：CloudFile Agent2 直接执行文件操作。Profile V1 读取/应用尚需作为*可选增量*实现：只处理合法受信的应用名称、来源、能力声明和实例关联，不能借由 Profile 升级文件访问授权或修改原任务状态。现有客户端没有 Profile 仍能使用。
- **过渡期**：Bridge 可独立安装，读取相同 Profile V1，提供只读任务聚合与 AI/MCP 增强；Agent2 仍独占文件执行权。不能对未知上传或活动 Guard 跨 Host 自动重试。
- **未来**：本机 LocalBinding 选择 Bridge 的 `cloudfile` adapter 路由新任务；已有任务由 Agent2 原 Journal 继续完成，云端文件身份和权限不变。上层应用无需换 Profile。

不要增加 CloudFile Rust Agent 私有 Profile 版本或将 `runtime=local-bridge` 写回应用 Profile。可复用通用 V1 数据合同/校验器或其固定版本测试样例，但不建立对 Bridge **运行进程**的依赖。安装/更新信任源和设备凭据仍由本机受信配置管理。

**更新说明：** `feat/consume-application-profile-v1-20261027` 已添加独立的可选在线 Profile 同步、缓存和品牌显示代码（合并、发行与实机验收另确认）。Bridge IPC 任务聚合与自动切流仍未实现；原 V2 文件引擎与 Chrome/Host 路由不变。参见 [Agent Profile V1](PROFILE_DIRECT_V1.md)。

## 桥接对照（未来的只读 IPC 起点）

| Bridge 目标能力 | Agent2 既有来源 | 当前允许的迁移 |
|---|---|---|
| `cloudfile.status` | 已有 `--component` / status | 已由 Bridge 0.1 只读探测 |
| `cloudfile.task.list` | 任务中心 + Journal 派生状态 | 下一个可交付的**只读摘要**，不返回本地任意绝对路径、Cookie、设备密钥 |
| `cloudfile.file.view` | V2 local-view | 保持原网页票据/本机领取，完成联动验收后再经 IPC |
| `cloudfile.file.edit` | V2 optimistic-edit | Agent2 持有保存/上传/冲突与结果未知语义 |
| `cloudfile.file.exclusive` | V2 exclusive-edit / Guard | Agent2 持有 Checkout/续租/Commit/Checkin，不交给 Bridge 重造 |

通用 `BridgeTask` 身份为 (`profile_id`, `owner_module=cloudfile`, `task_id`)；仅作为 Bridge 展示键，不修改 Agent2 原任务 ID、Journal、Intent 或工作副本路径。上传结果未知映射为 `unknown`，不能被 UI 当作失败后自动重建 Intent。有效 Guard 不因 Bridge 不在线而主动释放。

## 并行开发与版本隔离

- 当前正式 Windows Native Host：`com.cloudfile.current_agent`；macOS 正式 Host：`com.cloudfile.agent2`；开发 Host：`com.cloudfile.agent22.dev`。依照实际安装的版本核验，**不要在本设计阶段接管它们**。
- 浏览器现有 `status`、`open_uri`、`task_summary`、`focus_task`、本地查询、更新和配对调用维持原语义。
- 当前 SSO、Hub V2 票据、Fileserver 条件提交、权限、工作副本、Journal、Guard 以及更新/回退机制保持不变。
- Dev/Prod 使用分离的 Host Manifest、扩展 ID、设备凭据、工作区和更新源；没有完成同用户双安装验收前，不声称已经实现真正并行运行。
- Linux 平台沿用相同合同，但 Windows 真实体验和既有生产兼容优先。

## 开发步骤（不在本 PR 修改运行时代码）

1. Bridge 先离线解析通用 Profile；Agent2 无需感知任意应用名称。
2. 在 DEV 建立**受信只读** task-summary IPC；由 Agent2 对真实数据做脱敏映射；断连不影响原任务中心。
3. 只读验收之后，按查看 → 普通编辑 → 独占编辑顺序，分开新增受控操作 IPC。Bridge 只负责调用与确认，不取得 CloudFile 的文件发布权。
4. Windows Chrome + 真实 Hub/Server 联动，覆盖只读、保存、冲突、撤权、未知结果、Guard、重启与升级；再扩展到 macOS/Linux。
5. 任何一次切换均要求回退到现有操作入口；不得在写入结果未知时跨 Host 静默重试。

当前 [Agent2 #1](https://github.com/eaopen/cloudfile-local-agent2/issues/1)、[#10](https://github.com/eaopen/cloudfile-local-agent2/issues/10)、[#11](https://github.com/eaopen/cloudfile-local-agent2/issues/11) 的 P0 主链与实际验收优先级不变。

**本轮仅设计文档，不接管/重命名 Host，不修改上传引擎或任何在用服务。**
