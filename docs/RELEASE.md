# Rust Agent 2.2 安装、发行和升级约束

## 发行目标

按 Windows x64、macOS ARM64 和 macOS Intel 输出独立完整 ZIP，内含 Tauri Desktop 与 Rust Native Messaging Host；不绑定旧 Go/.NET 运行时。构建时可使用 Python/Rust/Node，**用户机器运行和注册不要求这些工具**。macOS 系统 WebKit，Windows 依赖已安装的 WebView2 Runtime，安装器先探测并清晰说明，不能默认 Chrome 代替 WebView2。

## 身份与数据隔离

当前 dev 基线的历史开发 Host 为 `com.cloudfile.agent22.dev`；Windows 仍使用固定 `com.cloudfile.current_agent`，不能据此宣称双通道已完成隔离。正式 macOS Host 为 `com.cloudfile.agent2`。

目标方案见 [Chrome 扩展与 Agent 架构及 Windows 兼容契约](AGENT_EXTENSION_ARCHITECTURE.md)：一个正式扩展按能力兼容，正式 Windows 身份保持不变，开发 Agent 按需隔离。未来开发 Host 的统一不自动改名现有注册、不迁移或清除工作副本。Linux 支持及发行分别以对应功能 PR 与实机验收为准。

## 安装、更新、卸载

- 支持当前用户安装与绿色直接运行。注册 Native Host 前检查 Host 所有权；用户显式安装/升级 Rust Agent 时，已识别的旧 CloudFile 注册先备份再替换；其它程序的注册仍要求明确接管授权。开发 Agent 不自动替换正式注册。
- `cloudfile-native-host` 已有 `--register-host`、`--unregister-host`、`--rollback-host`、`--enable-autostart`、`--disable-autostart`、`--check-update`、`--update`、`--rollback`、`--prune` 等代码；迁移首先保证它们在新仓库构建和隔离测试中正常，勿重新发明更新框架。
- 更新包必须校验信任公钥和签名清单、SHA-256、平台、IPC/Journal 版本；更新到新版本目录，不覆盖当前正在运行二进制。只有健康检查通过后才切换 Host；失败时现有可用版本和用户文件不变。
- 存在未确认 Intent、进行中上传或活动 Checkout 时，默认禁止不安全替换；如果允许用户显式强制停止，必须说明续租中断风险，并保存可恢复任务数据。
- 卸载与回退只删除本 Agent 有权管理的注册和版本目录，不删除工作副本、Journal、设备凭据或其他程序的 Host；保留原 .NET 版本的可恢复状态直到正式验收。
- Go 未发布：不要制作 Go→Rust 过渡安装器、版本桥、数据读取兼容器或 Go Sidecar。

## 验证

- CI 生成 ZIP 只是编译/打包证据，不能代表 Windows 普通用户注册、macOS Finder 首开、企业策略、Chrome 完整调用或真实服务器回写验收。
- 验证 Windows HKCU 注册、无管理员模式、WebView2 存在/缺失、托盘被禁、无需 PowerShell/Python、版本切换、回退与失败恢复。
- 验证 macOS ARM64/Intel 用户级安装、菜单栏/Dock、Gatekeeper 提示、不同 Chrome Profile 隔离、启动/卸载后用户数据保护。
- 双通道完成验收前，开发验证优先使用独立容器或 OS 用户。不得因安装开发包自动接管正式 Host，也不得仅因安装目录不同就宣称支持同一用户下安全并存。
