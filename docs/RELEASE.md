# Rust Agent 2.2 安装、发行和升级约束

## 发行目标

按 Windows x64、macOS ARM64 和 macOS Intel 输出独立完整 ZIP，内含 Tauri Desktop 与 Rust Native Messaging Host；不绑定旧 Go/.NET 运行时。构建时可使用 Python/Rust/Node，**用户机器运行和注册不要求这些工具**。macOS 系统 WebKit，Windows 依赖已安装的 WebView2 Runtime：安装器先探测并在缺失时**引导自助安装**（随包安装器、装到当前用户、不需要管理员），而不是只给一句英文错误或下载页。缺失运行时**不阻断安装**：安装仍成功，任务中心在用户拒绝/安装失败时才降级为托盘常驻（已打开的本地副本与独占租约继续；上传/提交需等任务中心恢复），并给出中文安装引导与官方下载地址；装完运行时从托盘退出 Agent 再重开（窗口只在启动时创建）。不得把 Tauri 的英文原生错误框当作交付结果（2026-11-02 现场）。

## WebView2 运行时分发

- **Windows 发行 ZIP 必须带 `webview2/MicrosoftEdgeWebview2Setup.exe` 与 `webview2/installer.sha256`**。`scripts/package.py` 默认下载微软官方引导器（在线，约 2MB）；内网/离线部署用 `CLOUDFILE_WEBVIEW2_SETUP=<Evergreen 独立安装器>` 打包（约 150MB，离线可装，文件同名）。外部发版脚本 `make-agent22-release.ps1` 也必须做同样处理（本仓库只约束产物结构）。
- Agent 只执行与 `installer.sha256`（或 `CLOUDFILE_WEBVIEW2_SETUP_SHA256`）一致的安装器；没有期望哈希或哈希不一致一律拒绝执行，不允许跳过校验。
- 安装以**当前用户**身份进行（`%LOCALAPPDATA%`，不触发 UAC）：桌面引导保留微软安装界面；`cloudfile-native-host --install-webview2` 为静默版，供安装脚本/IT 使用。
- 推荐 IT 用组策略/SCCM/Intune 机器级预装（一次覆盖该机器所有用户）；仅当安全策略禁止运行安装器时才只能降级并提示联系 IT。

## 身份与数据隔离

开发 Host 为 `com.cloudfile.agent22.dev`。正式 Windows Host `com.cloudfile.current_agent`，正式 macOS `com.cloudfile.agent2`。开发应用标识、数据目录与正式版需隔离，正式切换前不要自动迁移/清除任何已经存在的工作副本。Linux 不是首发目标。

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
- WebView2 **缺失**的机器单独验一遍（2026-11-02 现场）：安装照样成功、`--status` 报告 `runtime.task_center=false` 与 `runtime.installer.available=true`、双击任务中心弹出中文「立即安装」询问（不再是英文原生错误框）、点「是」后以**普通用户**完成安装并在同一次启动里打开任务中心；点「否」或让安装失败时，托盘常驻且菜单有「安装 WebView2 运行时（无需管理员）」。另验：`cloudfile-native-host --install-webview2` 在普通用户下静默成功；把 `installer.sha256` 改错后 Agent **拒绝执行**。**必须用 release 构建 + 真实 Windows 验**：那句英文框只在 `not(debug_assertions)` + Windows 出现，debug 自测发现不了。托盘被禁 + 缺运行时的零窗口零托盘组合：进程仍后台运行（日志有记录），退出只能靠任务管理器 —— 验收时明确记录这一行为。
- 验证 macOS ARM64/Intel 用户级安装、菜单栏/Dock、Gatekeeper 提示、不同 Chrome Profile 隔离、启动/卸载后用户数据保护。
- 若产品未准备正式发行，不接管 `com.cloudfile.current_agent` 与 `com.cloudfile.agent2`，开发阶段始终保持 `com.cloudfile.agent22.dev`。
