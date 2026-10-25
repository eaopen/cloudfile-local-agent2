# CloudFile Local Agent2 — AI Coding 工作规约

## 产品与代码边界

本仓库是唯一 Rust + Tauri v2 CloudFile 桌面 Agent 主仓库。Windows/macOS 共用 Rust Core、Tauri Desktop 与 Chrome Native Messaging Host。旧 Windows .NET Agent 是一次性替代对象；未发布 Go Agent2 只是参考，不承担 Go→Rust 数据、安装、设备、更新或代码兼容义务。

外部协议边界：CloudFile Hub/Server、现有 Chrome 扩展、Seafile native CDC/对象 ID、设备 P-256 证明、受控资源与 ACL、Intent/回执。内部实现可改，但不得泄露 Cookie/token、接受浏览器工作副本路径、盲目重发未知提交、失效后继续发布、丢弃用户工作副本。

## 工作模式

- 效率优先、MVP 优先：沿用既有实现并局部修复，避免重写同一状态机、复杂抽象、无限文档与重复测试。
- `dev` 为唯一长期开发分支；使用 `feature/<topic>` 短分支，验收后合入 `dev` 并删除短分支。
- 单主题唯一 Owner；可并行 UI、平台、发布，最终共享构建/发布由唯一集成负责人串行处理。
- 开始任务前看现有源码和 `docs/ROADMAP.md`，只做当前优先级；变更尽量小且可回滚。
- 使用 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked --no-fail-fast` 验证。平台相关要看 Windows/macOS CI 的真实结果。
- 新代码优先扩展 `crates/cloudfile-core`，OS 适配限定在小模块；Tauri 只负责 UI/进程生命周期，不创建第二套业务授权与文件提交逻辑。
- 不擅自更改 Server 端点、Chrome Host 名称、设备签名编码、CDC 块格式、用户数据目录或已有持久 Journal。确需变更要记录契约、测试与迁移边界。
- Windows 单一 `com.cloudfile.current_agent` 可能仍由旧 .NET 客户端占用；首次安装/一键修复**可以替换已识别的旧 CloudFile Agent**（用户明确安装/升级 Rust 版本即视为替换授权，不再二次确认）。任何非 CloudFile 程序的注册仍必须由用户明确确认或 CLI `--takeover` 才可替换，并保留原 manifest 供回退。Rust→Rust 升级按自有注册规则正常进行。

- 开发版 Chrome 扩展仅将 macOS 的 `AGENT2_HOST` 映射到 `com.cloudfile.agent22.dev`；Windows 的 `CURRENT_HOST` 必须保持 `com.cloudfile.current_agent` 并与本仓 `hostreg::HOST` 一致。不得批量替换所有 Host 字符串。
- 目标机器安装无需管理员、Python/Rust/Go/.NET Runtime；macOS 不清除 quarantine 或绕过 Gatekeeper，Windows 不绕过 WDAC/AppLocker。

## 优先级

P0 任务中心；P1 Windows .NET 现有功能覆盖；P2 独占编辑与 CDC 实机收尾。Rust 自身安装、签名升级、回退与卸载并行收尾。不要为未发布 Go 版本增加任何兼容代码。

## 交付纪律

每轮提交明确：改变了什么、为什么、哪些测试实际运行、哪些 E2E 未验证、剩余阻塞；不把 Mock 结果写成真实 Server/Office 验收。保持独立于旧仓库构建；不得覆盖现有工作区未提交改动。保留许可证与 NOTICE。

## Native capability detection (2026-10-17)

- Server-provided, supported editing modes are capabilities, not user-configured ENABLE flags. CloudFile V2 optimistic editing is available regardless of Checkout; Checkout depends on the installed verified Guard table and native enforcement. Never bypass native authorization because of a missing toggle.
- Agent must negotiate runtime capability with Hub/Server and select full upload versus native CDC using actual size/reuse and supported endpoints. Local user preferences (auto-start, tray/floating UI, per-task pause) remain preferences and are not product feature flags.
- Historic `CLOUDFILE_EDITING_ENABLED`, `CLOUDFILE_AGENT2_OPTIMISTIC_UPLOAD_ENABLED`, `CLOUDFILE_AGENT2_CDC_ENABLED` and exclusive-enable switches may describe incomplete older server contracts; do not introduce or keep them as requirements for a finalized V2 API. Preserve fail-closed behavior until server capability negotiation is proved in real end-to-end tests; never silently skip safe-write checks.
- External dependencies and missing Checkout capabilities degrade only their dependent operations. No unapproved server endpoint, DB table or agent protocol change. Record and verify any legacy contract cleanup before deleting live compatibility logic.

## CloudFile 当前 V2 编辑协议（强制）

- 本地查看、普通编辑、独占编辑从已登录 Hub 的 `/api/v2.1/cloudfile/edit/v1/open/` 得到有时效的、绑定用户/库/路径/模式的 scoped JWT；Chrome 扩展仅传递 `cloudfile-open://v2/open`，Agent2 的 `v2_edit` 调用 Go `/cf-edit/v1/*`。
- 文件编辑历史来自 Seafile CE Commit，独占锁只由 `cf_edit_guard` 保存，操作补充审计由 `cf_audit_event` 保存。Agent2 只能持久化自己的本地 Journal，不能要求 Server/Hub 恢复 `cf_edit_session`、`cf_resource`、`cf_commit_intent` 或任何第五表之外的 SQL。
- 未正式发布的 V1 server session claim 与 .cloudfile handoff 已退出；`seafile://openfile` 仅用于打开已持有的本地副本，或跳到正常登录的文件页，不能自行发起旧的服务器会话。
- 不为旧版本创建产品升级路径。变更应对齐 Server/Hub/Docker 当前 `dev`，只做必要的定向验证。

## V2 开发期数据初始化

- Agent2 只读取当前 V2 JWT、Go 文件接口和 `cf_edit_guard` 所代表的独占状态；`cf_audit_event` 由 Server 写入，Agent2 不使用任何旧 CloudFile SQL 表。
- 服务端扩展数据不会从历史开发版本迁移。当前库绑定从 Seafile 原生事实重新登记，Etech 重新下发权限与身份组映射。Agent2 本地未提交文件由 Journal 管理，不因服务端开发库重建而自动丢弃；实际 DEV 重建前应停止未完成的独占任务。
