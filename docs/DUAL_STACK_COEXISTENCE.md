# 双产品栈长期独立与升级合并门禁

**决策（2026-10-29）：CloudFile 原产品与 Local Bridge 新产品长期保持两套可独立安装、运行、升级、回退的体系。** 只有用户另行明确批准并完成真实迁移验收后，才允许收敛为一套；没有强制合并时间表。

本文在 `feat/local-bridge-provider` 特性分支维护，不触碰 CloudFile Agent2 `dev` 与既有正式用户链路。

## 两套独立产品

|  | ① CloudFile 现有产品 | ② Local Bridge 新产品 |
| --- | --- | --- |
| 源码仓库 | `eaopen/cloudfile-local-agent2` **当前 dev** + `eaopen/cloudfile-chrome-extension` | `eaopen/local-bridge` + `eaopen/local-bridge-chrome-extension` + CloudFile Provider 独立特性分支/产物 |
| Chrome 扩展 | 旧 CloudFile 扩展，已有固定 ID（`gocnpbhnbikcpnlafmodflddgnmfcjoc`） | 新独立 Chrome 扩展及发布 ID；当前未公布/固定新的产品 ID，不复用旧 ID |
| Chrome Native Host | Windows `com.cloudfile.current_agent`；macOS 既有正式 `com.cloudfile.agent2`；当前 Rust 开发入口 `com.cloudfile.agent22.dev` | `com.eaopen.browser_bridge`，只授权第二套插件的实际 ID |
| 文件执行 | Agent2 原 Native Host、Tauri、CloudFile Core、Hub/Fileserver | 可选独立 `cloudfile-bridge-provider`，借用 Core 源码，但属于第二套 Provider 安装 |
| 本地状态 | Agent2 已有工作副本/Journal/Guard/Intent/设备凭据/更新设置 | Provider 自己的 `CloudFileBridgeProvider` 私有根；Bridge 自己的 Profile/授权/任务摘要 |
| 发行和退出 | 保留现有 Agent2/CloudFile 扩展安装、签名更新及回退 | Bridge/新 Chrome/Provider 独立安装升级，互不替换旧 Host/工作区/更新签名 |
| 依赖 | 不需要安装 Bridge 或第二套插件 | Bridge 与新 Chrome 的基本功能不需要安装 CloudFile；Provider 缺失时只隐藏文件能力 |

两套可以调用同一 CloudFile Hub/Fileserver **云端服务**，但不同本地执行者必须分别获取有效、限范围的服务端授权，并接受服务端 ACL/File ID/Guard 条件判断。共享云服务不等于共享本地 Journal、工作副本或任务 ID。

### 原则：不跨产品栈自动转发或回退

1. **第一套永不默认走 Bridge。** CloudFile Web → 原 CloudFile Chrome → 原 CloudFile Host → Agent2 Tauri/File Core 继续独立执行，包括本地查看、普通编辑、独占编辑。
2. **第二套永不调用第一套 Host 来替代 Provider。** 通用 Web → 新 Chrome → Local Bridge → 本套 `cloudfile-bridge-provider`；不读取旧 Host Manifest 来定位旧 Agent2 任务/凭据，也不在 Provider 不可用时试探旧 CloudFile Host。
3. Bridge 只负责连接/能力/Agent/MCP/任务汇总；**CloudFile 业务动作由本套 Provider 和 CloudFile Server 确认**。AI `runAgent` 不混入 CloudFile 文件操作。
4. 任务一经受理，绑定所属产品栈、Provider、服务实例和版本；网络超时、Unknown/Conflict/Checkout 期间**不能跨栈重发**，不自动迁移有未确认操作的 Journal。
5. 两套更新与卸载互不影响。卸载 Bridge、新插件或 Provider 不删除旧 Agent2 的工作副本；卸载旧 CloudFile 扩展/Agent2 不删除第二套 Provider 任务。
6. 源码可以复用同一 `cloudfile-core` 模块，但安装目录、默认数据根、Native Host、扩展 ID、Profile/令牌、用户任务及发行通道必须各有归属。**代码共享不表示产品合并。**

### Provider P0 已实施边界

`feat/local-bridge-provider` 的新产物为独立 `cloudfile-bridge-provider` **MCP stdio 二进制**，不再把 `--bridge-provider` 注入原 `cloudfile-native-host` 默认入口。它目前只有 `cloudfile_status`、`cloudfile_task_counts` 两项只读工具，不能上传、Checkout、Commit/Checkin，也不能读取旧 Agent2 的实际任务。

默认本机数据目录：当前用户本地应用数据下 `CloudFileBridgeProvider`；可通过专门的 `CLOUDFILE_BRIDGE_PROVIDER_HOME` 选择另一个**绝对目录**。此二进制忽略 `CLOUDFILE_AGENT2_HOME`，不使用 Windows 旧 `D:\CloudFileAgent` 默认路径。当前查询的计数只属于第二套 Provider 自己的数据根；未接入写入流程前为零正常，**不能显示成第一套 CloudFile 任务已清空**。

本机安装/注册示例（需要实际安装的新 Provider 可执行文件，不是旧 Host）：

```sh
local-bridge provider add cloudfile /absolute/path/cloudfile-bridge-provider
local-bridge provider tools cloudfile
local-bridge provider call cloudfile cloudfile_status '{}'
local-bridge provider call cloudfile cloudfile_task_counts '{}'
```

第二套 Chrome 插件仍只连接 `com.eaopen.browser_bridge`；Provider 由本地 Bridge 按用户授权使用。Web、模型消息和 ApplicationProfile 不能替用户指定 executable/数据目录，Bridge 不向网页自动暴露通用 MCP 工具。

**当前未完成：** Provider 独立发行包/自动升级及完整文件三模式；Windows/macOS 用户机两套同时安装验收；Bridge UI/统一文件任务聚合。不能因 P0 只读源码或 CI 成功提前宣称这些完成。

### 双轨与 Dev/Prod 是不同维度

①/② 是两个**产品**；每个产品内部的 DEV/STABLE 发行通道和企业服务 DEV/PROD 又是另两个维度。不能把“Provider 分支运行在 DEV”误写成“已能与旧 PROD 共享本地账号和工作副本”。同时安装多个 Provider 实例时还需各自明确数据根、身份/注册和发行信任；本 P0 不隐式开启跨实例访问。

### 长期演进但不强制合并

- **现在：** ①持续交付已有 Agent2/CloudFile Chrome，Tauri 留在 Agent2；②独立交付 Bridge/新 Chrome，Provider 仅从只读状态开始，Bridge 核心保持轻量。
- **下一阶段：** ②逐项补 Provider 自己的文件查看/普通编辑/独占编辑，并在本套工作区形成真实任务；只验第二套，不重写第一套发行链路。
- **再往后：** ②可选通用任务中心、通知、托盘和安装入口；文件 Core/Journal/Guard 保持专业模块。不把 Tauri/WebView2 变成 Bridge Core 默认依赖。
- **未来升级合并（需另行批准）：** 决定最终 Chrome ID/Host、升级入口、用户工作副本/Journal/设备授权如何保留或迁移；验证旧数据不丢失、结果未知不重复提交、两个用户 Checkout/Checkin、Windows Office/CAD、版本回退、Chrome/SSO 和正式更新。完成这些**明确门禁并获用户确认**前，①/②一直是两套独立产品。

**不做：** 自动替换旧 CloudFile Chrome/Host、不静默共享两套私有目录/设备密钥、不让 Bridge 直接上传/Checkin、不构建第二套文件提交数据库、不因源码改造强制迁移现有 CloudFile 用户。
