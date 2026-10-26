# Agent 2.2 开发顺序与验收

## 阶段 0：独立仓库迁移（仓库拆分已完成）

- 用保留历史的子目录提取方式创建 `eaopen/cloudfile-local-agent2`，默认分支 `dev`。
- 新仓库根目录放 Cargo、crates、scripts、CI、AGENTS.md、LICENSE/NOTICE；不包含历史 Go/.NET/Swift。
- 修复 Chrome 扩展源码路径，Windows/macOS CI 全绿、开发 ZIP 可用；`git diff --check` 和测试/构建结果记录。
- 旧仓 `agent-rust` 已写入停止开发声明；Rust 后续只在本仓 `dev` / feature PR 开发。**本阶段不声称正式替代旧客户端**。
- 旧仓 10/16–10/19 的增量移植在 [PR #7](https://github.com/eaopen/cloudfile-local-agent2/pull/7)，CI 通过不代表已合并或实机验收。

## P0：任务中心（最高优先级）

- 任务中心多任务窗口；待处理、进行中、已完成三态与可靠计数。
- 单一可拖动悬浮入口，支持多显示器/缩放、位置持久化、无任务隐藏、徽标提醒。
- 托盘或菜单栏、窗口/任务栏/Dock 后备入口；浏览器退出后仍可访问。
- 普通编辑自动上传状态、待确认 Intent、独占 Checkout、失败/冲突、持久化恢复和明确操作入口。
- 保存轻提示不抢焦点，重要异常留在任务列表；退出保护不删除工作副本、不自动释放 Checkout。
- Chrome `task_summary` / `focus_task` 真正联动；优先修现有 Tauri 代码，不重写上传引擎。

验收：同时打开多文件，前后端状态一致；断网/重启有真实待处理提示；托盘不可用时仍有可见入口；普通用户可通过窗口或悬浮入口处理任务。

## P1：已使用的 Windows .NET 业务功能对等

- Word/Excel/PPT/Visio/Office 与常见 CAD/三维软件识别；用户自定义 open_rules 与默认关联优先级。
- 工作副本/文件与目录存在性查询、冲突判定、打开本地工作区、目录路径和文件名展示；旧 Web/Chrome 真正调用的功能必须可用。
- 与现有 Chrome 插件的状态、版本检查、更新提示、Native Messaging 协议对齐。
- Windows/macOS HTTP/HTTPS 内网、当前用户权限、WebView2 检测、签名与安全输入限制。

验收：有一份真实 Windows .NET 行为/Chrome 调用清单，逐项标注已实现/未实现，不凭未发布 Go 的功能推断对等。

## P2：独占编辑与 CDC 增量上传收尾

- 独占 Checkout 后台续租，退出/挂起/网络断开后的恢复和过期语义。
- “查询原提交结果”严格只读；“重试原 Intent”需明确用户操作和条件；丢响应不可盲目再次发布。
- 普通编辑自动上传、独占 Commit/Checkin 和 CDC 共用可靠的快照/Intent/原生条件发布边界。
- 真实 CloudFile Hub/Server + Chrome + Office 文件演练；CDC 至少证明旧块复用、缺块上传、File ID 一致、冲突拒绝。

## 并行：安装、维护与升级

- macOS/Linux 共用 Rust Host，Windows 保持固定控制器 Host；普通用户绿色运行，无 Go/.NET/Python 目标机运行依赖。
- Rust→Rust 更新：签名清单、SHA-256、分版本目录、健康检查、活动任务阻断、原地安全切换与回退。
- Windows 引导执行前必须可信：随发行包提供 bootstrap EXE，或部署方固定 EXE 的 SHA-256；不能仅检查下载字节数就执行。
- Windows .NET 一次性替换：用户确认、旧 Host Manifest 备份、工作文件不覆盖、验证新版、允许回退；不做 Go→Rust 迁移。
- macOS 无签名/公证必须说明标准系统授权限制，不能绕过 Gatekeeper；企业策略禁止时友好报错。

## 发布门槛

1. Windows/macOS CI 全绿，真实机 UI 与普通用户安装验证通过。
2. Chrome 插件正式使用的能力回归；服务端双平台配对、打开、普通保存和异常恢复真实演练。
3. 独占 Commit/Checkin 与 CDC 的真实服务器测试，结果未知时无无意重复提交或数据丢失。
4. 新版可升级、可回退、可卸载，原 .NET 工作用文件仍存在。
5. 未执行的测试明确标注，不能将 Mock 成功替代生产证明。
