# Agent2: 本地查看 / 普通编辑 / 独占编辑收口（2026-10-19）

本次只针对 **独立交付的 CloudFile Agent2**；不依赖 `local-bridge`，不移植 Pi/OpenCode，不扩大到虚拟盘、多文件工程和第二套服务端任务/Intent 表。

## 三种语义必须保持隔离

| 模式 | 客户端行动 | 保存 / 上传 | 服务端授权 | 当前情况 |
|---|---|---|---|---|
| 本地查看 (`local-view`) | 受控下载、打开本机应用 | 不允许写回 CloudFile | 仅文件 read scope | Agent2 本 PR 已接收 V2 `local-view` 且写回不可达；CloudFile Hub/Server 分别见 [Hub #27](https://github.com/eaopen/cloudfile-hub/pull/27) / [Server #11](https://github.com/eaopen/cloudfile-server/pull/11)。三端尚需部署联验 |
| 普通编辑 (`optimistic-edit`) | 工作副本、Office/CAD 保存检测 | 稳定约 3 秒后自动提交新版本；保留冲突和原始快照 | 必须有写权限、当前 File ID、一次性受限签名票据 | Agent2 代码已具备；真实 Hub/Server + Chrome + Office 联验待完成 |
| 独占编辑 (`exclusive-edit`) | Checkout Guard、续租、修改本地工作副本 | **人工 Commit / Checkin**；不自动上传 | Guard + 当前版本 / 所有权校验 | Agent2 代码已具备；真实 Guard/部署/失效重试联验待完成 |

严格区分“**本机软件 Ctrl+S 成功**”与“**服务端已确认形成新版本**”。未知结果不能直接宣布成功、删除本地文件、重传新快照或释放 Checkout。

## 本次修补

1. **保存抖动**：Office 原子替换导致文件短暂缺失、路径短暂不可读时，Auto Upload Observer 只重置该任务的稳定检测窗口，不中断其他任务；工作副本如果变成非普通文件则暂停并提示。
2. **V2 乐观编辑竞争**：后台观察到提交锁已被其他操作持有时返回 `busy`；观察后正式捕获快照时发现内容变化则返回 `changing`，不会因此永久暂停自动上传。真实上传仍以冻结快照+Journal 为准。
3. **操作模式门禁**：V2 core 自身强制普通编辑只能 OptimisticUpdate、独占编辑只能 Commit/Checkin、本地查看不能写回；不依赖前端按钮是否隐藏。
4. **独占待确认回执**：人工重试必须复用 **原 Journal Action** 和 **原快照**；尤其原来是 Checkin 就不能误作 Commit 重试。检查 pending 类型后才能发出操作。
5. **单元回归**：用合成任务覆盖临时文件替换、只读/独占不会自动上传、V2 正常竞争返回 busy/changing、跨模式写回拒绝。不使用真实私有文件、密钥或云端环境。

## 未完成的真实系统门槛（三端已分别提出实现，待合并/联验）

### V0：本地查看
- 在 CloudFile V2 Hub 增加 **只读** 的浏览器授权操作，使用 `Authority.can_read` 而不是 `can_write`，并由原生 Fileserver 再校验时效、文件/路径、版本、授权范围。
- V2 ticket 标记 `local-view`、读取返回 `X-CF-Mode=local-view`；Agent2 接入对应只读下载、写回永久拒绝。
- 原生 Fileserver 绝不允许将只读票据用于 Publish / Checkout；Agent2 工作副本保留且不由自动编辑观察器接管。
- 对应补丁在 [Hub #27](https://github.com/eaopen/cloudfile-hub/pull/27)、[Server #11](https://github.com/eaopen/cloudfile-server/pull/11)；Chrome [#3](https://github.com/eaopen/cloudfile-chrome-extension/pull/3) 沿用已有带 Origin 验证的 V2 URI。**只有这三端与本 PR 对应版本共同部署后，才可实际验收 V2 本地查看。**
- 如本次客户 DEV 尚未部署独立只读接口，网页应只显示已验收的“下载”入口而非宣称 V2 本地查看完整可用。

### E1：普通编辑验收
- 登录真实 CloudFile/客户环境 -> 领取 V2 编辑票据 -> Agent2 下载 -> Word/Excel 保存 -> 观察稳定 -> 新 Seafile File ID / 版本可见。
- Office 原子保存/重命名（临时缺失）、两份文件并行、重复保存、旧版本冲突、临时网络中断、上传响应丢失查询原 Intent、票据到期后用户重新从网页打开。
- 本地编辑不应要求 Local Bridge 或额外 AI 功能；如果浏览器/扩展桥仍选择旧 .NET/Go Host，不能记为 Rust Agent2 验收通过。

### X1：独占编辑验收
- 两个真实账号对同一文件 Checkout 互斥、只允许持有人 Commit、Commit 后保持 Guard、Checkin 后释放、断网/退出前风险提醒、Guard 过期/续租/重新领票、原 Checkin 结果未知时只查询或按原 Action 复试。
- 真实 CAD 单文件保存通过后再考虑 DWG/Xref 或 SolidWorks 装配体；不承诺跨文件引用同步。
- 升级/卸载不得删除活动编辑副本，也不得默默释放 Guard。

## 不做事项

- 不改变 `eaopen/local-bridge`、不做通用插件平台。
- 不复制 Go/.NET 实现，不启用泛化 RPA / 远程 Shell。
- 不新增表/API，除非在后续受单独确认的 CloudFile V2 **真实只读授权**集成工作中证明必要。
- 不用模拟 CI 的绿色取代真实 Office/CAD + V2 Hub/Server 的验收。

参考：
- [Agent2 Local Edit UX](LOCAL_EDIT_UX_20261017.md)
- [Agent2 README](../README.md)
- [CloudFile Hub Rust V2 Browser Ticket](https://github.com/eaopen/cloudfile-hub/blob/dev/cloudfile_ext/v2/rust_edit.py)
