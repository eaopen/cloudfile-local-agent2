# Rust Agent 2.3 安装、发行和升级约束

## 发行目标

按 Windows x64、macOS ARM64 和 macOS Intel 输出独立完整 ZIP，内含 Tauri Desktop 与 Rust Native Messaging Host；不绑定旧 Go/.NET 运行时。构建时可使用 Python/Rust/Node，**用户机器运行和注册不要求这些工具**。macOS 系统 WebKit，Windows 依赖已安装的 WebView2 Runtime，安装器先探测并清晰说明，不能默认 Chrome 代替 WebView2。

## 身份与数据隔离

开发 Host 为 `com.cloudfile.agent23.dev`。正式 Windows Host `com.cloudfile.current_agent`，正式 macOS `com.cloudfile.agent2`。开发应用标识、数据目录与正式版需隔离，正式切换前不要自动迁移/清除任何已经存在的工作副本。Linux 不是首发目标。

## 安装、更新、卸载

- 支持当前用户安装与绿色直接运行。注册 Native Host 前检查 Host 所有权；正式替换旧 .NET Host 必须显式确认并保存上一个有效 Manifest。
- `cloudfile-native-host` 已有 `--register-host`、`--unregister-host`、`--rollback-host`、`--enable-autostart`、`--disable-autostart`、`--check-update`、`--update`、`--rollback`、`--prune` 等代码；迁移首先保证它们在新仓库构建和隔离测试中正常，勿重新发明更新框架。
- 更新包必须校验信任公钥和签名清单、SHA-256、平台、IPC/Journal 版本；更新到新版本目录，不覆盖当前正在运行二进制。只有健康检查通过后才切换 Host；失败时现有可用版本和用户文件不变。
- 存在未确认 Intent、进行中上传或活动 Checkout 时，默认禁止不安全替换；如果允许用户显式强制停止，必须说明续租中断风险，并保存可恢复任务数据。
- 卸载与回退只删除本 Agent 有权管理的注册和版本目录，不删除工作副本、Journal、设备凭据或其他程序的 Host；保留原 .NET 版本的可恢复状态直到正式验收。
- Go 未发布：不要制作 Go→Rust 过渡安装器、版本桥、数据读取兼容器或 Go Sidecar。

## 验证

- CI 生成 ZIP 只是编译/打包证据，不能代表 Windows 普通用户注册、macOS Finder 首开、企业策略、Chrome 完整调用或真实服务器回写验收。
- 验证 Windows HKCU 注册、无管理员模式、WebView2 存在/缺失、托盘被禁、无需 PowerShell/Python、版本切换、回退与失败恢复。
- 验证 macOS ARM64/Intel 用户级安装、菜单栏/Dock、Gatekeeper 提示、不同 Chrome Profile 隔离、启动/卸载后用户数据保护。
- 若产品未准备正式发行，不接管 `com.cloudfile.current_agent` 与 `com.cloudfile.agent2`，开发阶段始终保持 `com.cloudfile.agent23.dev`。
