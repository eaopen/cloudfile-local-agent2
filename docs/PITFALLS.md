# 踩过的坑（回归清单）

> 目的：同一类问题**不要反复出现**。每条都写清「现象 / 根因 / 现在的防线」。
> 新增防线时，优先加**可执行的门禁**（单测、脚本用例、CI 断言），而不是只记文档 ——
> 文档会被忘掉，测试会亮红灯。

## 一、界面交互

### 1. 按钮点了没反应（作用域错误）—— 2026-11-13
- **现象**：弹窗正常出现，点「下载并升级」什么都不发生，无报错、无遮罩。
- **根因**：`applyUpdate()` 定义在 `wire()` 内部，而顶层的 `promptUpdate()` 引用它 →
  点击时抛 `ReferenceError`，`onclick` 里抛错没人接。
- **防线**：
  - `applyUpdate` 已提到顶层，只依赖顶层函数；
  - `ui-contract.test.cjs`：`promptUpdate` 后点第一个按钮，必须真的调用 `run_update`；
  - `installErrorDialog()`：任何未捕获异常 / 未处理 rejection 都弹「操作没有完成：<原因>」，
    不再静默（**新增顶层函数时，记得它引用不到 `wire()` 里的任何东西**）。

### 2. 原生 `window.confirm` 在 WebView2 里被拦 —— 2026-11-12
- **现象**：需要二次确认的操作（升级受阻、释放独占、删除任务、接管 Host）点了像没反应。
- **根因**：WebView2 会吞掉原生对话框，Promise 也不返回。
- **防线**：全部改用应用内弹窗 `askDialog()`；`ui-contract.test.cjs` 禁止
  `window.confirm(` / `window.alert(` / `window.prompt(` 出现在 `app.js`。

### 3. 装完新版还是旧界面 —— 2026-11-12
- **现象**：跑完 `install.cmd`／升级完，界面还是旧版，也没有任何提示。
- **根因**：第二个实例抢不到 `desktop.lock` → 直接 `return`，旧窗口保持原样。
- **防线**：抢锁失败时 `ipc::call(Request::OpenTaskCenter)` 把已有窗口叫到前台；
  若 `bin/` 里有更新的版本，emit `update-installed` → 弹「新版本已安装，需要重启」；
  启动时还会调一次 `pending_restart`。

### 4. 升级提示每次启动都弹 —— 2026-11-12
- **根因**：提醒状态只存在内存里，进程一重启就没了。
- **防线**：`update-announce.json` 记 `{version, at}`，同一版本 24 小时只提醒一次；
  「检查更新」按钮上的红点（`pending_update`）不受节流影响，一直在。

## 二、安装与更新链路

### 5. 主源不可用就永远看不到新版 —— 2026-11-12
- **根因**：`check_update` / `--update` 只认 `config.update_source` 一个地址。
- **防线**：安装器把两个候选写进 `update-channels.json`；拉清单先主后备，
  **验签基准跟着换**；发版必须同时传主清单与 `.ip.json`。

### 6. 缺公钥时静默「已是最新」 —— 2026-11-12
- **根因**：`check_update` 拿不到公钥直接返回，界面把空结果解释成"已是最新"。
- **防线**：缺源 / 缺公钥都写 `update_error`，界面与 `update-check.log` 都能看到原因。

### 7. `update-check.log` 时间戳看不懂、无限增长 —— 2026-11-12
- **防线**：日志用本机时间字符串（`cloudfile_core::local_stamp()`）；两份日志超过 256 KiB
  自动丢掉前一半（按字符边界切）。

### 8. 应用信息（品牌名）不跟着环境走 —— 2026-11-12
- **根因**：刷新判据用"所有环境里最大的 fetched_at"，且独立线程 24 小时才真拉。
- **防线**：按 `profile.environment` 各算各的；检查更新时顺带刷新（最小间隔 10 分钟、
  独立线程）；先试当前环境地址，全失败再回退其余候选。

### 9. 清单已上传、产物没上传 —— 2026-11-11
- **现象**：客户端永远只能看到旧版本，看起来像"缓存"。
- **根因**：只传了 `cloudfile-agent2-update.json`，清单里引用的 exe 404。
- **防线**：核对时**按清单 URL 逐个下载比 sha256**（不是只看清单在不在）。

## 三、Windows / 安装脚本

### 10. `New-Item -Path <注册表键> -Force` 会删键 —— 2026-10-14
- **现象**：`HKCU\...\CurrentVersion` 下的创建直接 `UnauthorizedAccessException`，
  在 `$ErrorActionPreference='Stop'` 下整个注册判失败。
- **防线**：用 `register-agent2-windows.ps1` 的 `New-RegistryKey`；只写值用 `Set-ItemProperty`；
  `scripts/*.ps1` 有静态检查。

### 11. 中文 `.ps1` 必须 UTF-8 **BOM** —— 2026-10
- 否则按 GBK 读 → 乱码且解析坏正则；续行的 `+` 要写在上一行末尾。

### 12. `.cmd` 是 GBK + CRLF + 无 BOM —— 2026-11
- `.gitattributes` 用 `*.cmd -text`、`*.dll binary` 锁住；
  脚本内的**严重度标签必须是 ASCII**（`[ERROR]` / `[WARN]`），正文可以中文 ——
  CI 用 PowerShell 读 GBK 脚本，中文标签会成乱码，门禁只能靠 ASCII 标签判断。

### 13. GNU 工具链必须随包 `WebView2Loader.dll` —— 2026-10
- `webview2-com` 在 windows-gnu 下是动态导入；只在本机 `cargo run` 正常，
  装到别的机器就「找不到 WebView2Loader.dll」。DLL 已收进
  `crates/cloudfile-core/assets/win-x64/`，由 `webview2::ensure` 落盘。

### 14. 安装目录里文件名不能带版本号 —— 2026-10-19
- 桌面找 host、host 找桌面、自检、自启都靠「同目录同一个名字」，
  版本只体现在目录名 `<安装根>/bin/<版本>/`。

## 四、NX 会话路由

### 15. 多个 NX 窗口直接报错 —— 2026-11-12
- **现象**：「检测到多个 NX 会话，无法确定应该打开在哪个窗口」。
- **防线**：多个**同版本**会话时选最近的那个（前台窗口优先 → 进程创建时间 → pid），
  只在与检测到的安装同一个可执行文件的会话里挑；路径不可读 / 版本不一致仍提示手工打开。

### 16. 删工作副本太激进 —— 2026-11
- 路由后 `opened.json` 记**实际运行的 NX PID**；只要曾路由过这份副本，删除前必须用户确认。

## 五、发版与验证

### 17. 打包脚本的输出不能进管道 —— 2026-10-29
- `make-agent22-release.ps1` 内部 `$ErrorActionPreference='Stop'`，cargo 的 "Compiling…"
  走 stderr，一进 `2>&1 | Tee-Object` 就变 `NativeCommandError`，2 秒中止。
  **直接调用，看结尾横幅判断成败**。

### 18. 测试读到脏临时目录 —— 2026-11-12
- 用例的临时目录带 PID 但会复用，上次异常退出的残留会让"刚提醒过/已过期"类断言偶发失败。
- 防线：目录名再加纳秒后缀，且先 `remove_dir_all`。

### 19. 「单测过了」不等于「装完能用」 —— 2026-10
- 2.2.3 之前「装完只有注册、没有任务中心」就是只跑单测漏掉的。
  **安装链路必须在临时根上真跑一遍**；真机已有安装时 e2e 会整片跳过，等于零覆盖。

## 附：改动这类文件时的自检

| 改动 | 必做 |
|---|---|
| `ui/app.js` | `node --test tests/*.cjs`（含禁用原生对话框、按钮真调命令） |
| `crates/cloudfile-core/src/update.rs` 签名载荷 | `the_signing_payload_is_a_fixed_byte_string` 必须同步 `sign-agent22-release.py` |
| `scripts/*.cmd` | 保持 GBK+CRLF、ASCII 标签；`node scripts/installer-guard.test.cjs` |
| `Config` / `Settings` 结构 | **不要加字段**（`deny_unknown_fields` 会让旧版本读不出配置），用独立文件 |
| 新增顶层 JS 函数 | 不能引用 `wire()` 内部的东西 |
