# Linux 桌面开发版

Linux 使用现有 Rust Core、Tauri 任务中心和 V2 设备签名协议，macOS/Linux 共用 `com.cloudfile.agent2`，Windows 保持 `com.cloudfile.current_agent`。
当前验收基线为 Ubuntu 24.04 x86_64、Google Chrome 和登录用户的图形桌面会话。
Ubuntu 20.04 默认仓库缺少 WebKitGTK 4.1，不能直接安装此桌面版。

## 系统依赖

构建需 Rust 1.90、C 编译器、pkg-config，以及 `libwebkit2gtk-4.1-dev`、
`libxdo-dev`、`libssl-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、
`libdbus-1-dev`。运行需对应运行库、D-Bus 用户会话及 Secret Service（例如 GNOME Keyring）。

设备私钥由 Secret Service 保存；未启动、未解锁或不可用时拒绝配对，不回退到明文文件。
Chrome、Native Host、桌面 Agent 必须使用同一用户、HOME 和 D-Bus 会话。

## 构建与注册

```sh
cargo build --workspace --release --locked
python3 scripts/prepare-extension.py --extension-src ../cloudfile-chrome-extension
./target/release/cloudfile-native-host --register-host
./target/release/cloudfile-native-host --self-test
./target/release/cloudfile-desktop
```

Linux 与 macOS 的 Host 均为 `com.cloudfile.agent2`，Chrome 注册位于
`~/.config/google-chrome/NativeMessagingHosts/`。开发扩展使用
`dist/chrome-extension-agent22`，由用户在 Chrome 扩展管理页加载。
Windows 保持 `com.cloudfile.current_agent`；不探测已废弃的 Go Host。

在 Agent 添加组织提供的服务地址，网页登录后发起本地查看或编辑。
当前 V2 使用网页的一次性授权，不要求单独登录 Agent 或先完成旧 V1 设备配对。
设备签名能力使用 Secret Service；首次创建密钥环时，由用户在桌面提示框中设定解锁密码。

当前不包含 Linux 登录自启、Chromium 自定义配置目录及 Linux 自动更新发行通道。
系统库、窗口、Host 往返、真实系统密钥往返和真实服务端编辑分别验收；编译通过不代表联验通过。

依赖依据：[Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/)、
[keyring Secret Service](https://github.com/open-source-cooperative/keyring-rs/tree/v3.6.3)。

## 实机验证（2026-10-26）

Ubuntu 24.04 x86_64、Google Chrome、LibreOffice 24.2 的开发包已完成：

- Rust 工作区 171 项测试通过、1 项系统凭据测试默认忽略；另显式执行系统凭据往返测试通过。
- Clippy、格式检查、Native Messaging 状态/任务往返及 Tauri 任务中心实际启动通过。
- Chrome 显示实际构建版本；V2 本地查看生成独立只读副本，不上传。
- 普通编辑的真实保存回传已得到 `published` 回执，服务端 File ID 与 SHA-256 和回执一致。

本次未验收 Linux 独占编辑、断网恢复及自动升级；生产环境仅确认域名解析与入口连通。
