# 外部协议和本地身份契约

## 不与 Go 兼容 ≠ 可以更改浏览器协议

Go Agent2 没有正式发行，因此不要求 Go 数据升级、Go Journal 读取、Go 设备密钥复用、Go 安装脚本、Go 版本回退与 Go Sidecar。

正式 Chrome Extension 已有入口：

| 平台 | 正式 Native Messaging Host | 用途 |
|---|---|---|
| Windows | `com.cloudfile.current_agent` | 替代已使用的 .NET Agent1 Host |
| macOS | `com.cloudfile.agent2` | 与已存在的 Chrome 插件固定路由对齐；不意味着兼容未发布 Go |
| 开发 | `com.cloudfile.agent23.dev` | 隔离使用独立浏览器配置文件及应用数据 |

新仓库开发阶段维持 `com.cloudfile.agent23.dev`，不可默认覆盖另两个正式 Host。接管时校验真实所有权、备份 Manifest/注册位置、活动文件状态，允许一次性切换与回退；不接受用户未确认的 silent takeover。生产 Host 的应用签名和应用数据存储应与开发 Host 区分，避免测试工作副本混入正式运行。

## 浏览器消息需要保留的能力

当前扩展使用 `status`、`open_uri`、`open_session`、`task_summary`、`focus_task`、`query_local_file`、`query_local_folder`、`open_workspace`、`check_update`、`update`、`update_extension`，以及与设备配对有关的受信流程。Rust 可按实际产品契约逐一实现或明确拒绝不支持的操作；正式替代门禁必须检查现有 Chrome 页面及 popup 的真实调用，不可只通过 Native Host 的 `status` 证明兼容。

设备配对沿用 `pair_enrollment`/`pair_sign`/`pair_confirm`，另加受控的 `pair_prepare`：由已登录页面的首次本地编辑触发，先由 Agent 探测该地址确为启用 `local.open-edit` 的 CloudFile 实例，再复用 `device::begin()` 建立短时待配对密钥。配对仍由页面的 `pair-start`/`pair-confirm` 完成，Agent 不单独完成注册；Cookie/token/sessionid 不进入 Native Host。

开发工作副本默认位于用户主目录 `CloudFile-Dev/Workspace/<task_id>/`。私有数据目录及既有 Journal 格式不变；新任务在私有任务目录写入 `working-copy.json` 记录绝对工作目录，已有无该记录的任务继续使用原私有目录。修改工作区仅影响新任务，不移动、覆盖或重新绑定现有工作副本；浏览器不能提供本地文件路径。开发打包不再注入独立手动配对按钮。

不需要为未发布 Go 的私有 CLI/Journal 或从未启用的实验消息制作兼容适配。正式已用旧版页面操作不应因 Rust 替代而消失。

## Server 与编辑

CloudFile Hub/Server 复用设备签名挑战和受控 API，不能把 OAuth/OIDC Cookie 直接交给 Native Host。普通编辑和独占编辑的操作授权不同；读取票据不授予发布权限。Journal 在上传前持久化 Intent 和快照，确认原生受控回执后才能标记成功；丢回执先查询原 Intent、不可生成新的 Intent 盲重发。独占 Checkout 必须考虑续租、退出与断线恢复，Checkin 由用户明确触发。CDC 必须匹配 Seafile v1 Rabin 边界、块 ID 和 File Object ID，使用同一 Intent 的条件发布。

服务端可使用标准 OIDC 与旧 OAuth；Agent 不直接实现双网页登录，使用 Hub 设备配对与受控授权。HTTP/HTTPS 同源企业内网可用，但不得静默降级或跨域处理票据。

## 兼容验证方式

开发用隔离 Host + Chrome 独立配置。正式接管一次性替代 Windows .NET Host，需验证旧真实用户工作副本不会被静默覆盖。没有 Go 发行用户，无需为 Go 制作迁移器。
