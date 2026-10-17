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
