# CloudFile Local Agent2 — AI Coding 工作规约

## 产品与开发第一原则：简单、稳定、可靠（强制）

**CloudFile 是面向企业长期运行的通用文件服务，不是技术实验平台或功能堆砌。** 首要目标是文件、权限和常用业务能力**持续可用、行为可预测、易于维护**。开发优先级：**数据与授权正确、服务稳定 > 已有功能兼容和用户体验 > 交付效率 > 新功能数量或架构完整度**。“快速完成”不能以破坏可靠性为代价，“安全”也不能成为无证据地无限增加机制的理由。

- **先解决真实问题**：优先修复正在使用的功能、明确故障和实际性能瓶颈；不为假设中的远期场景提前建设。新功能必须说明使用场景、最小交付与退出条件。
- **成熟能力优先，改动最小**：先复用 Seafile CE 原生事实和现有 CloudFile V2 模块，再做局部增强。没有证明必要性，不新增数据库表、第二套权限/身份状态、缓存层、后台任务、轮询、兼容框架、配置开关或微服务；确需扩展时说明不可替代的理由和运维成本。
- **故障隔离而非连带停机**：可选 IdP/Provider、搜索、预览或其他外部服务故障，只影响依赖它的操作，不应无故拖垮核心文件服务；但具体操作缺少必要授权或写入证明时仍应明确拒绝，绝不以“可用性”为由放宽权限、假报成功或损坏文件版本。
- **低频事务不要变成实时复杂系统**：企业管理员、业务配置等低频变化，优先启动时或显式操作同步，再使用稳定的原生执行状态；没有实证需要，不引入每请求远程核验、持续补偿任务、额外撤权平台或多重状态账本。接受并明确告知更新生效边界。
- **运行成本和维护成本可控**：目录读取有界、批量/分页和缓存按实际负载优化；不因单个异常就全量扫描、重复编译或放大 Redis/数据库压力。代码、部署、故障定位和恢复流程应简单，关键操作需有明确结果与可操作的失败处理。
- **通用产品、真实企业落地**：CloudFile 应能独立部署，不依赖特定客户系统。配置默认值优先照顾已经验证的企业实际用法，但业务专属账号、接口、权限策略和数据表留在应用/Provider 适配层，不硬编码进通用 Core。
- **按风险验证、如实交付**：文档变更不编译；小改只跑受影响的定向检查；权限、数据写入、身份及跨仓发布须做对应真实联验。代码、CI、镜像和 DEV/生产验收分开报告；未经明确任务不擅自部署、重建数据或扩大范围。

**方案审查只问四件事：真实要解决什么？现有能力能否更简单地解决？失败时是否安全且不影响无关服务？如何用最小测试证明并恢复？** 解释不清新增复杂性的收益，就拒绝该设计；不需要为此另建流程、平台或表格。

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
- 服务端扩展数据不会从历史开发版本迁移。当前库绑定从 Seafile 原生事实重新登记，客户 Provider 重新下发权限与身份组映射。Agent2 本地未提交文件由 Journal 管理，不因服务端开发库重建而自动丢弃；实际 DEV 重建前应停止未完成的独占任务。
