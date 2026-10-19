# CloudFile Agent 2.2 · MVP

本仓库 `eaopen/cloudfile-local-agent2` 是唯一的 Rust + Tauri v2 Agent 开发主线，长期分支 `dev`。
全新 Rust + Tauri v2 实现，Chrome Native Messaging 与原有 CloudFile Hub/Server 保留为协议边界。
历史参考基线：`e33d380f7e7ae65efdf39bcb0dec091e6d13b755`；不依赖 Go/.NET/Swift 运行时。

仓库根目录即 Cargo Workspace（`crates/cloudfile-core`、`crates/cloudfile-native-host`、
`crates/cloudfile-desktop`），没有外层目录。迁移来源与历史边界见 [docs/MIGRATION.md](docs/MIGRATION.md)，
外部协议与本地身份契约见 [docs/CONTRACTS.md](docs/CONTRACTS.md)，开发顺序见 [docs/ROADMAP.md](docs/ROADMAP.md)，
安装与发行约束见 [docs/RELEASE.md](docs/RELEASE.md)。
旧仓库 `eaopen/cloudfile-local-agent` 的 `agent-rust` 只作只读回退参考，Rust 代码只在本仓库继续。

## 当前 MVP 实现

- Tauri 任务中心、托盘、可拖动悬浮入口、非抢焦点轻提示、服务配置、设备连接、本地打开与修改状态。
- **任务中心三态展示**：待处理 / 进行中 / 已完成。分组与计数由桌面进程派生，界面只渲染，不自行判断任务是否结清。任务详情按编辑模式给对应操作，普通编辑与独占编辑共用一套界面。
- **任务 / 服务 / 设置分区**：首次使用按服务与本机组件状态展示连接引导；已连接且无任务时提示从网页开始编辑，有任务时聚焦进度与操作。偏好、更新和浏览器诊断集中在设置，安装目录与 Host 信息默认折叠。
- **统一菱形云文件图标**：状态栏、任务中心与悬浮入口共用同一透明标识。macOS 状态栏使用系统模板适配深浅色；其它平台保留 Agent 蓝色。图标来自 GPT Image 方案 B，原始设计和提示词保存在 `assets/branding/concepts/`。
- **可拖动悬浮入口**：拖动交给系统原生手势（跨显示器与缩放比例都不会偏移），位置按显示器分别保存；无任务时按用户设置隐藏；有任务待处理时显示计数徽标。拖动落点由桌面进程在窗口停止移动后写入设置。
- **退出保护**：托盘“退出”与窗口关闭都不会在存在未确认 Intent 或有效独占 Checkout 时静默结束进程。先给出具体风险清单，用户确认后才退出；退出只停止后台续租，不释放 Checkout、不删除本地工作副本与待确认 Intent。
- Chrome `open_uri` 设备签名会话：认领、下载普通编辑工作副本。
- **本地保存后自动上传**（普通本地编辑）：文件内容稳定至少 3 秒后，Agent 捕获快照、签名授权并全量上传；无需手动选文件或网页上传。支持按任务暂停自动上传，也可点击“保存云端”立即提交。
- **独占编辑（Checkout 租约）与手动提交**：网页发起独占会话后，Agent 用当前配对设备签名领取并确认短时租约（`lease-*`），可续租；修改后由用户在任务中心点击“提交”保留 Checkout，或“签入并释放”结束本次独占。提交走独立的签名 `commit-*` 意图 + 原生条件发布，不复用普通上传接口。
- 发布确认后推进基线，可继续编辑再次保存。响应丢失先查询原 Intent；重试发送原快照，不替换成后来修改的内容。
- **大文件 CDC 分块上传（可选，默认关闭）**：≥16 MiB 的快照先按 Seafile 原生 CDC（Rabin，6/8/10 MiB）切分，用服务端已存在的原生块做签名 missing-block 计划；只有确实节省 ≥4 MiB 且复用 ≥25% 时才改走 `*-block-plan` / `*-block-stage` / `*-block-publish`，否则仍用已验证的全量传输。独占与普通本地编辑两条路径共用同一套计划与回退逻辑。
- **本机安装、升级与回退均由自带程序完成**：`cloudfile-native-host` 同时是 CLI，注册 Host、登录项、下载校验、版本目录切换、回退与清理都不需要目标机器安装 Python 或 Rust 工具链。
- 密钥存储在 macOS Keychain / Windows Credential Manager；采用 Rust P-256 签名，不依赖旧 Agent。当前是系统保护的可读密钥，不宣称 CNG/硬件不可导出密钥。
- 独立用户目录 `CloudFileAgent22`、开发 Host `com.cloudfile.agent22.dev`。
- 开发版新工作副本默认放在用户主目录 `CloudFile-Dev/Workspace/<任务 ID>/`，可在任务中心更改；首次打开目录时自动创建。修改目录只影响新任务，已有任务仍使用原位置。密钥、任务记录、Intent 与上传快照继续保存在私有 `CloudFileAgent22` 目录。

本地应用 Ctrl/Cmd+S 保存到磁盘后，普通编辑由 Agent 自动观察工作副本并上传；独占编辑不会自动上传，必须由用户在任务中心显式提交。发生网络中断、冲突或未确认回执时暂停或保持待确认状态，保留工作副本与原上传快照；不会盲目自动重发。
当前单文件上限与现有服务端一致，为 128 MiB。
旧 `cloudfile-local/v2` 会话仍可本地打开，但不具备新设备上传授权；应从网页重新发起普通本地编辑。

### 安装、升级与回退

注册、登录项、升级、回退与清理统一走 `cloudfile-native-host` 的命令行，不使用任何脚本运行时：

```sh
cloudfile-native-host --register-host --extension-id YOUR_EXTENSION_ID   # 注册当前用户 Host
cloudfile-native-host --host-state                                       # 查看当前注册与所属
cloudfile-native-host --rollback-host                                    # 还原上一次被替换的注册
cloudfile-native-host --unregister-host                                  # 只移除本 Agent 写入的注册
cloudfile-native-host --enable-autostart / --disable-autostart           # 可选登录启动
# 空白机器：先跑引导体（它只做定源 + 验签 + 下载 native host，然后交给下面那条命令）
cloudfile-install --source https://host/dir --source-fallback https://ip/dir --extension-id <id>

cloudfile-native-host --install --source https://host/dir --source-fallback https://ip/dir \
    --public-key <b64> --extension-id <id>                               # 一键安装/修复
cloudfile-native-host --set-update-source https://host/dir --public-key <b64>  # 只配置更新通道
cloudfile-native-host --clear-update-source                              # 清空更新通道
cloudfile-native-host --check-update                                     # 报告可用的已签名版本
cloudfile-native-host --update                                           # 下载、校验、暂存、健康检查、切换
cloudfile-native-host --update-extension                                 # 装最新扩展包，再由扩展自己重载
cloudfile-native-host --rollback                                         # 指回上一个版本目录
cloudfile-native-host --prune                                            # 只保留当前版本 + 一个回退点
cloudfile-native-host --status                                            # 版本、注册、运行时、应用检测、退出风险
cloudfile-native-host --self-test                                        # 本机自检（JSON；有问题时退出码非 0）
cloudfile-desktop --self-test                                            # 同一套检查，从桌面进程那一侧看
```

空白机器上什么都没有，而 `--install` 需要一个**已存在的**二进制来跑，所以另有一个引导体
`cloudfile-install`（对应 Go 侧 `install.cmd` 的角色）：

- 它**不带任何负载**：定源（同样的主备探测）→ 取**已签名**的清单并用**内置的钉住公钥**验签 →
  下载 `cloudfile-native-host`（校验清单里的 sha256）→ 把参数原样转给它 `--install`。
  注册、暂存、健康检查、激活、自启、启动全部仍由 native host 完成 —— 会改机器状态的动作只有一份实现。
- 与 `install.cmd` 的本质区别：`install.cmd` 从通道下载注册脚本并**无签名**地执行（只查 sha256）；
  这里必须先验签才下载任何字节，而且**公钥在打包时注入**（`CLOUDFILE_INSTALL_PUBLIC_KEY`），
  绝不在运行时从通道取 —— 否则「谁签发」就变成「通道说了算」。没注入又没给 `--public-key` 时它**拒绝干活**。
- `--dry-run` 只定源并报告计划、不改机器：现场排查「这台机器能不能连上通道」用。
- 双击启动（无参数 + 终端）时结尾会等回车，自动化里不会挂住；`--no-pause` 可显式关掉。

一键安装（`--install`）等价于 Go 的 `install.cmd`，但目标机器**不需要任何脚本运行时**：

- **主备地址**：`--source` 是主源（域名），`--source-fallback` 是备用源（通常是直连静态服务的 IP）。依次探测，**把实际可用的那个写回配置** —— 不然用户装完、第一次升级时才发现主源在这台机器上根本不通。主源可用时备源一次都不会被探测（省掉一次超时等待）；两个都不通时报错里带着每个地址**各自的原因**，只报一句「安装失败」在现场没有用。空地址会被跳过，主备写成同一个地址只探一次。
- **顺序**：先判安装目录可写（别下完几 MB 才发现装不进去）→ 定源并写回 → 有更新才换二进制 → 注册 Host → 设开机自启 → 启动任务中心。
- **幂等**：已经装好的机器重跑一遍等于**修复** —— 注册与自启会重做，二进制不动（清单不比当前新就不换）。
- 自启失败与启动失败都**不判安装失败**（各只警告一句）：它们和注册是两件事，谁挂了都不该拖累另一个，二进制已经就位、用户也能自己双击。
- 可跳过：`--skip-registration` / `--skip-autostart` / `--skip-launch`（现场分步排查、或只想升级二进制时用）。
- **装到哪**：`CLOUDFILE_AGENT2_HOME` 覆盖 Agent 的整棵树（配置、任务、版本），与 Go 的 `install.cmd` / `internal/home` 一致；引导体的 `--install-dir` 是同一件事的显式写法。相对路径一律拒绝。
- **全新安装与升级走同一条路**：先暂存（健康检查通过才落进版本目录）、再按「有没有旧注册」决定接管还是新建注册。`update::activate` 是「接管既有注册」专用的，所以注册与激活在 `--install` 里是分开的两步 —— 否则空白机器上会因为「没有旧注册可接管」而失败。

更新通道的信任策略：

- 源必须是**规范 origin**（`http://host/dir`，不带尾斜杠 / userinfo / fragment），
  **http 与 https 一视同仁**：内网门户只提供 http，测试通道与正式通道挂在同一个域名下、
  只差路径一节（`/cloudfile-updates-dev` 与 `/cloudfile-updates`）。
- 放宽的只有 scheme 一项：包只能从同一 origin 下载、不跟随重定向、sha256 与 P-256 签名
  一条都不放松 —— 「通道是不是明文」与「这个包是不是我们签的」是两件事。
- `--allow-insecure-source` / `allow_insecure_update_source` 保留为**兼容位**：旧脚本带着它
  不会报错，值仍记在 config.json 与 `--status` 的 `update_source_insecure_allowed` 里当审计，
  但**不再影响**源能否被接受。
- 允许「有源、没有签名公钥」的通道（显式策略下）：那种通道只服务**扩展更新**
  （扩展不签名，信任锚是同源 + sha256），此时 `--check-update` / `--status` 会明说
  「只有扩展更新可用，Agent 自身不可升级」。带公钥才是可升级的通道。
- **二进制只编一份**：测试与正式用的是同一套 exe，区别只在安装入口脚本 ——
  `install.cmd`（正式通道）与 `install.dev.cmd`（测试通道），两者只差 `CHANNEL` 一行。
  清单按通道各生成一份（artifact URL 必须与源同源），exe 永远只有一套。

规则：

- 被其他程序注册的 Host **不会**被静默替换。需要显式加 `--takeover`，且原 Manifest 一定先备份为 `<host>.previous.json`，可用 `--rollback-host` 还原。任务中心里对应的接管需要用户确认。
- 升级只信任签名清单：校验签名、SHA-256、平台与协议版本后才暂存到新的版本目录，不覆盖正在运行的程序；暂存程序必须能启动、`--version` 与 `--component` 都通过才允许切换。健康检查失败会删除暂存目录，当前版本保持不变。
- 升级前会检查活动任务。存在有效 Checkout 或未确认 Intent 时默认拒绝升级，因为升级会替换承担续租与发布的进程；用户可在任务中心看到风险清单后明确选择继续。失败时保留用户数据，不会把未确认任务当作已完成。
- Windows 需要 WebView2 运行时，`--status` 会报告 `runtime.webview2`；macOS 使用系统 WebKit（该字段为 `null`，表示不适用而非缺失）。不捆绑离线 WebView2，以保持包体积。
- 当前替代目标是 **A 类：功能完全替代** —— 首次切换允许重新配对设备，但不丢失任何工作副本。原设备身份与原任务的无感原位迁移（B 类）尚未实现。

### 发版（Rust 通道）

**Agent2 现在只有 Rust 这一条线**：Go 版（`dev` 分支）已停更，`agent1` 是 1.0 生产版，两者都不再发布；测试通道目录里只留 Rust 的产物。

Rust 必须验签（`update::verify` 是硬策略，不会降级接受）：

| | 清单 | 产物 | 信任锚 |
|---|---|---|---|
| **Rust Agent22（主源，域名）** | **`cloudfile-agent2-update.json`** | **`cloudfile-native-host-<v>.exe` / `cloudfile-desktop-<v>.exe`** | **钉住的 P-256 公钥** |
| **Rust Agent22（备用源，IP 直连）** | **`cloudfile-agent2-update.ip.json`** | 同一批产物 | 同上 |
| 扩展 | `extension-update.json` | `cloudfile-local-session-receiver-<v>-dev.zip` | 同源 + sha256 |

两份清单是**必须**的：清单里的 artifact URL 是绝对地址，`verify` 要求它与更新源同源；源从域名换成 IP 就是换了 origin，复用域名那份会被拒（`release artifact does not come from the update source`）。Rust 按 host 是不是 IP 自动选文件名（`update::manifest_file`），调用方不用传参。

版本号**在编译期注入**，不改仓库里任何文件（等价于 Go 的 `-ldflags -X`）：

```sh
CLOUDFILE_AGENT2_VERSION=2.2.4 cargo build -p cloudfile-native-host --release --locked
```

`cloudfile_core::VERSION` 优先取这个环境变量，没设才回落到工作区 `Cargo.toml` 的版本（所以开发时手跑 `cargo build` 得到的仍是开发版本）。之所以不采用「发版脚本改 Cargo.toml」：升级链路上有三处把这个字符串当事实比较（清单的 `version`、`update::probe` 拿到的 `--version`、`stage_release` 里「报告版本必须等于清单版本」），三者必须同源，而改文件的方式一旦中途失败就会留下「构建出来的版本 ≠ 签发的版本」的中间态。

通道侧的签名器与发版脚本在另一个仓库（`cloudfile-release/tools/dev-release/`）：

```sh
python sign-agent22-release.py keygen dev-keys/agent22-dev.pem dev-keys/agent22-dev.pub.json
python sign-agent22-release.py pubkey dev-keys/agent22-dev.pem          # 导出钉住的公钥
powershell -ExecutionPolicy Bypass -File make-agent22-release.ps1 \
    -Version 2.2.4 -ReleaseKey dev-keys/agent22-dev.pem
```

`make-agent22-release.ps1` 会构建两个产物（内置版本号）、逐个跑 `--version` 核对版本、写清单、签名、回验，并做**端到端自检**：起本地通道，用一个版本更低的探针二进制跑真实的 `--check-update`，确认签名被真实 Rust 验签器接受 —— 这是唯一能证明「Python 签名器与 `update.rs` 没漂移」的检验。

跨界面的契约只有一条：`signing_payload` 的字节形状。`update.rs` 里有一条 `the_signing_payload_is_a_fixed_byte_string` 逐字节钉住它；**Rust 侧改了字段顺序或分隔符，那条测试会红**，届时必须同步改签名器，否则通道上签出的清单会让所有已装 Agent 报「签名与钉住的公钥不符」。

### 大文件 CDC 分块上传

**V2 目标语义：** CDC 是同一编辑提交引擎的自动传输策略，根据当前文件大小、块复用收益和服务端提供的实际分块接口能力选择；不要求用户在客户端和服务端各配置一个功能 ENABLE。以下为现有开发版尚待真实联调的传输安全边界：

- 计划、暂存、发布每一步都单独向 `commit-upload-authorize`（或 `optimistic-upload-authorize`）申请一次性的 60 秒票据；不跨步复用，票据只在 header 里传。
- 暂存原始块不改变任何文件：只有原生 Branch 内的守卫事务才写文件路径，并在其中重新校验账号、设备、租约、基线与回执。
- 回退是安全的：计划请求失败或计划显示不划算时，尚未发布任何内容，因此回落到全量传输仍使用同一个已准备 Intent。一旦发布请求已发出，就不再回退——回执丢失时保留工作副本与原快照，先“查询提交结果”。
- 单文件上限 128 MiB、单块上限 10 MiB、单 Intent 最多 64 块，与 `cf_device_block_plan` 一致。

**V2 对接边界：** 普通乐观编辑始终属于 CloudFile 内置能力；独占编辑依赖服务端真实安装并验证的 `cf_edit_guard` 和原生写入围栏。CDC 仅在真实分块协议支持时才选择，否则在尚未发布的安全边界内使用全量上传。旧版 mock/v0.9 协议中的 `CLOUDFILE_*_ENABLED` 参数不是 V2 产品部署要求；本仓库对旧协议的运行实现与真实 Hub/Server 对接尚未完成，不能仅凭文档更新宣称已取消旧代码门禁。接口缺失必须明确返回不可用并保留工作副本，不能盲目回退或越权发布。

验证：macOS 编译、静态检查与协议集成测试。CDC 分块切分与参考 Agent 的字节输出做交叉校验（32 MiB 镜像的块 ID、尺寸、SHA-256 与 File ID 完全一致），并在 mock Hub 上覆盖计划、只上传缺失块、发布、不划算回落、传输未启用回落与暂存被拒保留原 Intent。尚未完成运行中 Hub/Server + Chrome + Office 的端到端验收，也未实机验证 Windows，真实服务端分块上传仍待联调。

## 安装引导（首次运行）

桌面程序第一次启动（`settings.setup_completed` 为 false）时停在**安装引导页**，之后可以从左侧「安装引导」随时打开做修复。
它把六件事按顺序列出来，并各自给出完成判据（判据全部来自 `setup_state`，界面不做推断）：

| 步骤 | 判据 | 谁来做 |
|---|---|---|
| 安装 Agent | 进程正在运行 | 引导体 `cloudfile-install` / `install.cmd` |
| 受信任的 CloudFile 服务 | `trusted_missing_defaults == 0` | 启动时自动补齐出厂默认项 |
| 工作目录 | 目录存在 | 默认 `D:\CloudFileAgent2\workspace`，可在设置里改 |
| 注册 Chrome Host | `host_registered` | 「一键完成」自动注册（固定扩展 ID） |
| 开机自启 | `autostart_enabled` | 「一键完成」自动设置 |
| 安装浏览器扩展 | `extension_version` 有值 | 自动下载解包；**最后在 Chrome 里点三下** |

「一键完成」是幂等的：它依次做「补默认服务 → 建工作目录 → 注册 Host（接管会先备份）→ 设自启 → 部署扩展」，
每一步独立成败，失败项在页面上带原因，可以再点一次重试。

Chrome 不允许程序静默安装非商店扩展，所以最后一步只能提示：页面提供「打开扩展管理页」（用注册表
`App Paths` 找到的 `chrome.exe` 打开 `chrome://extensions/`）和「打开扩展目录」，并显示固定的扩展 ID。
开关「开发者模式 → 加载已解压的扩展程序」必须由用户点。

**默认值都是 1.0 的形状，不需要用户重配一遍**：

- 受信任服务：`http://10.9.8.162:6111`、`http://10.12.1.157:8092`、`http://etech.stcetech.ad01.sec.com`、
  `https://etech.stcetech.ad01.sec.com`（`session::DEFAULT_ORIGINS`）。这些是**默认项**，界面上标「默认 · 不可删除」，
  `session::untrust` 会拒绝删除它们；自己加的服务可以移除。
- 工作目录：`D:\CloudFileAgent2\workspace`（1.0 的默认地址），本地副本落在其下的 `tasks\`。
  `settings.workspace_root` 显式配置永远优先，设置面板有「恢复默认地址」。
- 扩展 ID：`gocnpbhnbikcpnlafmodflddgnmfcjoc`（`hostreg::EXTENSION_ID`）。扩展 `manifest.json` 钉了 `key`，
  所以 ID 与解压路径无关 —— 界面和安装都不再要求手输。


在本目录执行（工具链由 `rust-toolchain.toml` 固定）：

```sh
cargo test --workspace --locked
cargo build --workspace --locked
cargo run -p cloudfile-desktop --locked
```

桌面程序接受三个自检参数，都不需要窗口或私有目录：`--version`、`--component`（本地组件契约 JSON）、`--help`。
登录项会用 `--autostart` 启动它：有托盘时保持常驻不打开窗口，避免每次登录抢焦点。

先在任务中心添加完整服务地址，例如 `https://files.example.com` 或 `https://files.example.com/seafile/`。部署路径由本机配置保存，设备签名仍绑定根 Origin。连接设备前会检查服务端编辑能力和配对接口；未启用时直接显示原因，不创建配对密钥。
Windows 需已安装 WebView2 Runtime；Chrome 本身不提供此运行时，`cloudfile-native-host --status` 会如实报告是否检测到。macOS 使用系统 WebKit。
当前 ZIP 是开发包，不含签名、公证或运行时安装器。

## Chrome 开发联调

```sh
python3 scripts/prepare-extension.py --extension-src ../cloudfile-chrome-extension
```

（`prepare-extension.py` 是构建期工具，只用于生成开发扩展副本；目标机器不需要 Python。）
共享扩展在本仓库之外，因此必须用 `--extension-src` 或 `CLOUDFILE_EXTENSION_SRC` 指定其 checkout；
未指定时先找同级 `../cloudfile-chrome-extension`、再找上级 `../../cloudfile-chrome-extension`，
都找不到就报错退出。开发副本始终写入 `dist/`，不提交、不回写共享扩展仓库。

使用独立的 Chrome 配置文件（开发副本沿用现有扩展 ID，不能与正式扩展装在同一配置文件）。在扩展管理页以开发者模式加载 `dist/chrome-extension-agent22`，然后：

```sh
cargo build -p cloudfile-native-host --locked
./target/debug/cloudfile-native-host --register-host
```

扩展 ID 是产品常量（`hostreg::EXTENSION_ID`），不需要传 —— 只有换扩展公钥重新打包时才需要 `--extension-id`。
任务中心里也不再有「填 ID + 注册」这一路：引导页的「一键完成」或「Chrome 注册与升级」里的「重新注册」都用同一个常量。
注册只影响当前用户的 Host，不替换旧 Host；若该 Host 已被其他程序注册，会先备份再接管
（原 Manifest 存为 `<host>.previous.json`，`--rollback-host` 可还原）。

### Host 名（为什么 Windows 上是 `com.cloudfile.current_agent`）

扩展在 Windows 上**只**向 `com.cloudfile.current_agent` 发消息，且它的用例明确禁止回落到其它 Host
（`cloudfile-chrome-extension/tests/agent2-routing.cjs`：`Windows never probes Go Hosts when its fixed
Host is missing`、`a future Windows Go controller uses the same Host ID`）。也就是说 Windows 上的控制器
**只有一个槽位**，谁当控制器谁就得占它——名字写错，扩展只会回「本机 Agent 未就绪」，与 Agent 装没装无关。

因此 `hostreg::HOST` 按平台取值：Windows → `com.cloudfile.current_agent`，其它平台 → `com.cloudfile.agent22.dev`
（macOS 上扩展找的是 Go 的 `com.cloudfile.agent2`，Rust MVP 不去抢）。Windows 上占这个槽位必须是**显式**动作：
`--register-host` 不 overwrite 别人的注册，需要 `--takeover`，且先备份、可还原。

清单里的 `path` 写**普通绝对路径**：`canonicalize()` 在 Windows 上给出的 `\\?\` verbatim 前缀会被剥掉，
与 Go 侧写出的形状一致（UNC 的 `\\?\UNC\...` 保留，剥了会变成非法路径）。

开发扩展是共享扩展的隔离副本；连接步骤：

1. 在开发 Chrome 配置文件登录 CloudFile 服务。
2. 支持首次配对的网页在第一次本地编辑时通过 `pair_prepare`、`pair_sign`、`pair_confirm` 完成设备配对；扩展弹窗不再添加临时“连接 Agent 2.2”按钮。浏览器登录 Cookie 不会交给 Agent。此流程需要网页和 Hub 同步支持并启用相关接口，Agent 就绪不代表设备已配对。
3. 从网页发起“普通本地编辑”（设备会话 `cloudfile-open://v1/open`），在本地应用保存修改。
4. Agent 在文件内容稳定后自动上传，也可在任务中心点击“保存云端”。确认发布后可继续编辑。
5. 独占编辑：从网页发起独占会话（服务端需具备并实际执行 Checkout Guard 能力），Agent 领取并确认 Checkout 租约后在任务中心显示“独占编辑”任务；点击“提交”保留 Checkout，点击“签入并释放”结束本次独占。提交结果未确认时先点“查询提交结果”，不会自动重发。

V2 不要求单独开启普通编辑或独占编辑功能开关：普通编辑必须有合法原生发布接口；独占取决于服务端 Guard 是否已安装、验证并强制执行。Agent2 当前旧版设备会话/签名和真实 V2 Hub/Server 仍需联调，不能把历史开关参数直接复制到新版部署。接口或授权不可用时显示失败并保留副本，不降级为未经验证的 Web 上传。
原上传待确认时，“查询上传结果”只查询；“重试原上传”先查询，再在未发布时重发同一 Intent 的原快照。后来修改仍留在工作副本，下次保存再上传。独占编辑对应“查询提交结果”，语义相同，但只在新独立提交/签入时可再次发起，且签入成功后本地会话关闭，必须从网页重新发起。

```sh
python3 scripts/package.py
```

构建本机架构 ZIP，两个可执行文件必须放在同一目录。ZIP 内不再附带任何注册脚本：
注册、登录项、升级与回退都由 `cloudfile-native-host` 自己完成，解压即可运行，
目标机器不需要 Python 或 Rust 工具链。
打包脚本本身仍需 Python 3.11 或更高版本（仅构建期）。macOS 包含可双击的开发 `.app`。
GitHub Actions 对 macOS / Windows 分别执行 `cargo fmt --all --check`、`cargo clippy`（`-D warnings`）、
Rust 测试与开发扩展打包测试，并生成开发 ZIP 上传 Artifact；
构建通过不代替 Windows 实机和真实服务端验收。

## 代码边界

- `cloudfile-core`：会话、用户配置、任务记录、设备签名、提交快照与全量上传；`exclusive` 模块负责独占 Checkout 租约与 Commit/Checkin，`cdc` 模块负责大文件分块切分与计划/暂存/发布链路，`guard` 模块是“能否安全退出 / 升级”的唯一判定来源，`settings` 负责本机偏好与悬浮位置，`workspace` 负责工作副本根目录策略与一次性搬迁，`apps` 负责应用检测与自定义打开规则，`hostreg` / `update` / `autostart` 负责注册、版本目录与登录项。
- `cloudfile-core` 的三个 Windows 底层保障模块：`winreg`（进程内读写注册表）、`childproc`（子进程不弹控制台窗口）、`shortcut`（「启动」文件夹里的 `.lnk`）。注册、登录项、WebView2 探测、应用探测**一律走它们**，不再 spawn `reg.exe`——否则父进程是 GUI（Tauri 桌面）时每次调用都会闪一个黑窗口，而且在禁用 `reg.exe` 的机器上会直接失败而不是降级。只有「打开文档的应用」与「必须脱离宿主存活的升级进程」例外，前者需要窗口、后者用 `DETACHED_PROCESS` 自带无控制台。
- 读前加固只有一处判据：`fsaccess`（对应 Go 的 `internal/fsaccess`）。任务目录、任务记录、偏好文件在**读**之前都要求「私有的普通文件/目录」，语义是 **fail closed**（与 Go 的 store 层一致）：一份权限被改松的状态文件不能被当成可信输入去授权退出、上传或续租。列表类接口不会把它报成「没有任务」——那会让人以为工作副本丢了。**Windows 上这条判据是刻意的 no-op**：`Metadata` 只反映只读属性，套 POSIX 位会把每一个状态路径都判死；真实访问由 NTFS ACL 管，而 ACL 不通过 `Metadata` 暴露。所以 Windows 上的加固是「绝对路径 + 非重解析点 + 普通文件 + 体积上限」，不含权限私有。
- Windows 的登录项有**两条途径**，装的时候依次尝试：先写当前用户的 `Run` 键，被组策略或安全软件锁住时（企业机器上常见）退到「启动」文件夹里的 `CloudFileAgent22.lnk`。两条都用 `IShellLinkW`/`IPersistFile` 由 shell 自己维护格式，不手写二进制。`--status` 与设置界面会报出实际生效的是哪一条（`run-key` / `startup-folder`），因为这两种情况在任务管理器里长得完全不一样。两条途径都带上 `--autostart`：登录启动必须进托盘而不是抢焦点。
- `cloudfile-native-host`：有界 stdio 消息、本机请求转交，以及安装/升级/回退用的命令行。
- **模式（mode）是子树，不是字符串**：本地副本只有三个子树 —— 查看（`local-view`，只读）、可编辑、独占可编辑；`local-edit` 与 `optimistic-edit` 是**可编辑子树的两个名字**。浏览器请求与任务都在这层比较，所以「缺省/`local-edit`」永远不会命中只读的查看副本（Go 的 `Request.MirrorMode()` 同义，原文理由：编辑子树是唯一不会被「查看」打开覆盖的那个）。反过来才是会伤人的：查看副本是只读的，被当成编辑副本交给用户，用户改完存不进去。可上传/可提交另有一道**模式白名单**（`journal::capture` 的 `editable task required`、`upload`/`exclusive` 各自校验），所以查看副本拿不到 Intent。
- 本机自检（`selftest` 模块）：把「这台机器上装的东西到底能不能工作」变成一次可脚本化的问答（`--self-test`，两个入口共用同一套检查）。十二项各自独立跑、独立报，输出固定形状的 JSON，有项确实不工作时退出码非 0。**刻意不创建窗口、不发网络请求**：建窗口依赖窗口站/桌面会话，在服务、SSH、CI 上必然不稳，自检本身变成「时好时坏」就没有价值；网络请求要等超时，会把「10 秒出结论」拖成「30 秒也不一定」。更新源只报「配没配、够不够用」，内容对不对由 `--check-update` 回答。
- 浏览器扩展的自更新（`extension` 模块）：与 Agent 清单**同级、同一个源**的 `extension-update.json`（`version` / `zip_url` / `sha256` / `notes`），下载后校验 sha256、解包到临时目录、核对包内 `manifest.json` 的版本与清单一致，再整体改名换过去。归档按不可信输入处理：绝对路径、`..`、反斜杠（Windows 上的 zip slip）、符号链接、非普通文件、超过 256 MiB 一律拒绝，且只写新建文件。替换前上一版会改名到 `<dir>.prior`，换不过去就放回原位；`<dir>.prior` 已存在时拒绝动作，等人工看一眼。装完由扩展自己 `chrome.runtime.reload()` 生效——Chrome 不允许外部进程替换已加载的扩展，所以这一步结束时是「文件已就位」，不是「已生效」。
- `cloudfile-desktop`：唯一任务执行进程与轻量静态界面（`ui/` 下四组静态资源：任务中心、悬浮入口、轻提示、样式），无 Node 运行时。
- `cloudfile-install`：引导体，只给空白机器用（定源 → 验签 → 下载 native host → 转交 `--install`）。它自己不带负载，见上一节。

本机 IPC 使用用户数据目录下的短期文件邮箱及进程锁，无 localhost 服务。
请求成功处理后移除票据；进程中断后不会自动重放一次性票据。
本地副本不因重复打开或浏览器 overwrite 字段而覆盖。只有服务器确认发布才可显示云端保存成功。
独占编辑的 Checkout 令牌只写入所有者可读的任务 journal；签入成功或释放后立即删除，服务端在 Agent 确认 journal 落盘后才会擦除其密文副本。
分块上传的原始块字节来自本任务冻结快照的同一内存镜像，切分与暂存不存在时间差；暂存只写内容寻址的块，不改变文件路径或 Checkout 状态。
`guard::assess` 是退出、托盘状态、浏览器退出查询与升级前置检查共用的唯一判定；任何一处都不允许自行放宽“可以安全退出”的定义。

## 验证状态

验证命令：`cargo test --workspace --locked --no-fail-fast`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all --check`、`node --test scripts/prepare-extension.test.cjs`。前三项同时是 CI 门禁；系统凭据往返测试需要外部条件，默认忽略。
其中 `cloudfile-native-host/tests/cli_contract.rs` 与 `cloudfile-desktop/tests/cli_contract.rs` 直接运行真实二进制，并把 `HOME`（Windows 另加 `LOCALAPPDATA`）重定向到临时目录，因此注册、接管、回退、卸载、健康检查与协议自检都不触碰开发机上的真实 Chrome 注册。

尚未验收：

- **Windows 实机**。注册表读写、WebView2 探测与登录项的两条途径（`Run` 键写入 → 读回 → 撤销，「启动」文件夹 `.lnk` 写入 → 读回 → 撤销，含 shell 路径与文档化公式一致）已在 Windows 11 上实测；**Tauri 窗口与托盘行为、应用探测的真机回归仍未做**。
- **真实服务端联调**。自动上传、独占提交与 CDC 仍只对着 mock Hub 验证过。
- **浏览器扩展联动的完整链路**。`--status` / `capabilities` 的字段已对齐，但**没有在真实 Chrome 里跑过**：安装、加载、`chrome.runtime.reload()` 之后的实际行为都还没验证。扩展自更新的**文件侧**已用开发通道里真实发布的包验证过（摘要与清单一致、9 个条目含 `icons/` 子目录全部解出、版本核对、重复安装幂等），**下载侧（`--update-extension` 走网络）未验证**：见下面那条关于 http 通道的说明。
- ~~**Rust Agent 自身的更新（一键安装的“换二进制”那一步）**~~ 已补齐：开发通道里现在有
  `cloudfile-agent2-update.json`（域名源）与 `cloudfile-agent2-update.ip.json`（IP 直连源）
  两份签名清单 + 两个带版本号的产物，`make-agent22-release.ps1` 负责编译、健康检查、
  逐源写清单、签名与端到端验签。
- ~~**引导体的公钥注入与发布**~~ 已补齐：`cloudfile-install` 的公钥在**打包时注入**
  （`CLOUDFILE_INSTALL_PUBLIC_KEY`，默认没有 —— 没有钉住的公钥就不该开始安装）。
  `cloudfile-release\tools\dev-release\make-agent22-release.ps1` 一条命令完成：
  导出公钥 → 编译 → `--version` 健康检查 → 写清单并签名 → 注入公钥重建引导体 →
  按清单 URL 逐个 HEAD 确认产物可达 → 真实二进制端到端验签。
- **无感原位升级（B 类）**。不迁移旧设备身份与原任务，首次切换需重新配对。
- **Code signing / 公证 / 运行时安装器**。当前 ZIP 是开发包。

参考：[Tauri v2](https://v2.tauri.app/)、[Chrome Native Messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)。

### Seafile `seafile://` legacy links (Windows per-user compatibility)

See [Seafile URI compatibility and non-admin deployment](docs/SEAFILE_URI_COMPAT.md).
The optional Windows registration advertises link handlers without overriding
an installed SeaDrive/Sync Client default. Legacy links remain untrusted
locators; a fresh file opens the authorized web page for a new V2 editing
grant. No SeaDrive engine or file-system driver is bundled.


### 从旧仓库迁入的安装与升级能力（2026-10-20）

本仓库保留 CloudFile V2 的本地查看、普通编辑、独占编辑实现，并迁入
`cloudfile-local-agent/agent-rust/agent22` 的 Windows 首次安装引导、
Native Host 注册/回退、自动启动兜底、扩展自更新、离线检查以及签名升级工具。
Windows Host 使用 `com.cloudfile.current_agent`；非 Windows 使用开发 Host。
本机扩展 ID 由产品常量提供，不再要求用户自行输入。

部署时需要在「服务连接」添加组织提供的服务地址；**不会自动信任旧仓里的
内部 IP/域名**。用户已有工作副本和配置不能用旧仓的整目录覆盖方式迁移，
工作区根变化前应先验证任务恢复，并在明确授权后实施实际搬迁。
CI 和真实 Windows/macOS 安装验收应分别记录，不把静态代码移植视为实机通过。
