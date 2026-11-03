//! WebView2 的加载器 DLL（`WebView2Loader.dll`）。
//!
//! 桌面端用的是 `webview2-com`。它在 **MSVC** 工具链下把加载器**静态**链进 exe，但在
//! **GNU** 工具链（`x86_64-pc-windows-gnu`，本机发版用的就是它）下只能**动态导入**
//! `WebView2Loader.dll` —— 这个 DLL 必须和 exe 在同一个目录，否则进程在进入 `main`
//! 之前就起不来，Windows 弹「由于找不到 WebView2Loader.dll，无法继续执行代码」。
//!
//! 2026-10-19 现场报的就是这个：任务中心更新走到**暂存后的健康检查**那一步挂住
//! （`update::probe` 要跑 `cloudfile-desktop --version`，进程根本起不来，拿不到版本），
//! 于是更新被正确地拒绝了 —— 但用户看到的只是一个系统弹窗。
//!
//! 所以这份 DLL 被**编进二进制**，随暂存/激活一起落到版本目录（`bin/<版本>/`）里：
//! - `update::stage`：写进去，健康检查才有机会通过（更新的第一现场）；
//! - `update::activate`：兜住「早先的版本目录里没有这个文件」（例如回退到旧版本）。
//!
//! **为什么不放进发布清单**：清单的产物结构（恰好 native-host 与 desktop 各一个）与
//! 字段顺序都是签名载荷的一部分（见 `update::signing_payload` 的逐字节用例），为一个
//! 运行时依赖去改它，等于让所有已装机器重新接受一套新契约；它也不是「可替换的产物」，
//! 必须与构建它的那个 SDK 同源，跟着 exe 走才对。
//!
//! 文件来源：`webview2-com-sys` 0.39.1 自带的 `x64/WebView2Loader.dll`
//! （WebView2 SDK 1.0.3800.47，sha256 见 `LOADER_SHA256`）。升级 webview2 依赖时
//! 顺手把它一起换掉 —— 版本不一致不会立刻坏，但两者本来就该同源。

use anyhow::Result;
use std::path::{Path, PathBuf};

/// 文件名。Windows 的加载器按这个名字找，不能改。
pub const LOADER_FILE: &str = "WebView2Loader.dll";

/// 随包携带的这份加载器的 sha256（小写十六进制）。发给现场做比对用。
pub const LOADER_SHA256: &str = "86545b66cdb0603bc26b626fb9ad610cb6e71f28d468f5ea66df23b03dda96d5";

/// 加载器在版本目录里的路径。
pub fn loader_path(dir: &Path) -> PathBuf {
    dir.join(LOADER_FILE)
}

#[cfg(windows)]
const LOADER: &[u8] = include_bytes!("../assets/win-x64/WebView2Loader.dll");

/// `dir` 里那份加载器是不是**随包的这一份**（内容逐字节相同）。
///
/// 「完整安装」的判据要用它：文件在但被换成了别的版本，和不存在没有区别 ——
/// 加载器与构建它的 SDK 必须同源。
#[cfg(windows)]
pub fn present(dir: &Path) -> bool {
    std::fs::read(loader_path(dir)).is_ok_and(|current| current == LOADER)
}

/// 非 Windows 上没有这个依赖，永远算「在」。
#[cfg(not(windows))]
pub fn present(_dir: &Path) -> bool {
    true
}

/// 把加载器放进 `dir`（幂等：内容已经一致就不动它）。
///
/// 非 Windows 上是空操作：那边没有这个依赖。
#[cfg(windows)]
pub fn ensure(dir: &Path) -> Result<()> {
    use anyhow::Context;

    if present(dir) {
        return Ok(());
    }
    let path = loader_path(dir);
    // 加载器属于安装产物：等它（INSTALL_BUDGET），别用状态文件那份短预算。
    crate::storage::write_bytes_within(&path, LOADER, crate::storage::INSTALL_BUDGET)
        .with_context(|| format!("无法把 {LOADER_FILE} 放进 {}", dir.display()))
}

/// 把加载器放进 `dir`（非 Windows 上的空实现）。
#[cfg(not(windows))]
pub fn ensure(_dir: &Path) -> Result<()> {
    Ok(())
}

/// WebView2 运行时（Evergreen Runtime）官方下载页。
///
/// 和 Tauri 自己那条英文错误里给的地址一致：现场机器上通常只有浏览器能上网，
/// 让用户自己搜「WebView2 下载」很容易被带到第三方站。
pub const RUNTIME_DOWNLOAD_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// 缺少 WebView2 运行时给用户看的中文说明（自助安装走不通时的兜底）。
///
/// 这一段是**现场唯一的自救入口**，所以故意做成任何平台都能编译、能被单元测试盯住的
/// 纯文本：2026-11-02 现场报的就是任务中心弹出一句英文原生对话框
/// （`Could not find the WebView2 Runtime.`），用户既看不懂「缺什么」，也不知道
/// 「装完还要做什么」。说明必须回答三件事：装哪一版、装完做什么、这期间还能用什么。
pub fn runtime_missing_guidance() -> String {
    format!(
        "本机未检测到 Microsoft Edge WebView2 运行时，任务中心窗口无法打开。\n\n\
常见原因：Windows 版任务中心用 WebView2 渲染界面。Windows Server、精简版系统、\
只给其他 Windows 用户装过运行时的机器上会缺失（当前登录账户读不到）；\
运行时损坏或被安全策略拦下也会表现成这样。\n\n\
处理（两种方式都**不需要管理员权限**）：\n\
1. 从托盘菜单「安装 WebView2 运行时（无需管理员）」按提示安装 —— 安装包已随 Agent 提供；\n\
2. 或到官方页面下载「常青版独立安装程序（Evergreen Standalone Installer）」自行安装。\n\
如果这台机器由 IT 统一管理，也可以请管理员预装（一次解决该机器所有用户）。\n\n\
安装完成后：从托盘菜单「退出」Agent，再重新打开任务中心\
（窗口只在启动时创建），不需要重装 Agent。\n\
如果浏览器没有自动打开，请手动访问：{RUNTIME_DOWNLOAD_URL}\n\n\
现在打开官方下载页吗？\n\n\
（安装期间 Agent 仍在后台运行：已打开的本地副本、独占租约会继续；\
上传与提交请等任务中心恢复后再操作。）"
    )
}

/// 启动时「要不要现在装」的询问文案。默认打「不需要管理员」这一点：
/// 现场最常见的误判就是「我没权限，所以装不了」，然后放弃。
pub fn runtime_install_prompt() -> String {
    "检测到本机缺少 Microsoft Edge WebView2 运行时，任务中心窗口无法显示。\n\n\
现在安装吗？安装包已随 Agent 提供，会装到当前 Windows 用户下，**不需要管理员权限**。\n\
点「是」后会打开微软官方安装程序，按提示完成即可（约 1 分钟）；装好后任务中心会自动打开。\n\n\
点「否」：Agent 在后台继续运行，之后可从托盘菜单「安装 WebView2 运行时」重试。"
        .to_string()
}

/// 安装失败后的说明。把「退出码」写出来，现场才能区分「下载不到」和「被策略拦下」。
pub fn runtime_install_failed_guidance(exit_code: Option<i32>) -> String {
    let code = exit_code.map_or_else(|| "无".to_string(), |code| code.to_string());
    format!(
        "WebView2 运行时没有安装成功（安装器退出码：{code}）。\n\n\
常见原因：机器访问不了微软下载（内网/断网），或安全软件、组策略拦截了安装器。\n\
处理：请 IT 提供内网离线安装包（放到安装目录的 webview2\\ 下即可），\
或请管理员在这台机器上预装 WebView2 运行时。\n\
也可以手动下载安装：{RUNTIME_DOWNLOAD_URL}\n\n\
Agent 仍会在后台运行；装好后从托盘菜单「退出」Agent 再重新打开任务中心。"
    )
}

/// 任务中心窗口在这台机器上能不能开。
///
/// 只有 Windows 依赖 WebView2 运行时；macOS 用系统 WebKit（`platform_windows=false`
/// 时恒为可用）。抽成带平台参数的小函数，是为了让 native-host 的 `--status` /
/// `--install` 与文档共用同一判断，并且能在 macOS 的 CI 上测到 Windows 那两个分支。
pub fn task_center_ready(platform_windows: bool, probe: Option<&str>) -> bool {
    !platform_windows || probe.is_some()
}

/// 打开运行时下载页。返回浏览器是否真的被拉起来 —— 调用方据此决定要不要再补一句
/// 「请手动访问上面的地址」，而不是吞掉失败让用户以为程序没反应。
pub fn open_runtime_download() -> bool {
    open::that(RUNTIME_DOWNLOAD_URL).is_ok()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn the_loader_lands_next_to_the_binary_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        ensure(root.path()).unwrap();
        let path = loader_path(root.path());
        let first = std::fs::read(&path).unwrap();
        assert_eq!(first, LOADER, "写下去的字节必须是随包的那一份");
        // 再跑一次不能变（也不能报错）：暂存/激活都可能重复调。
        ensure(root.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first);
    }

    #[test]
    fn a_foreign_loader_is_replaced_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(loader_path(root.path()), b"not a loader").unwrap();
        ensure(root.path()).unwrap();
        assert_eq!(std::fs::read(loader_path(root.path())).unwrap(), LOADER);
    }

    #[test]
    fn the_shipped_loader_is_the_one_the_release_notes_name() {
        use sha2::{Digest, Sha256};
        assert_eq!(format!("{:x}", Sha256::digest(LOADER)), LOADER_SHA256);
    }
}

/// 现场说明文案的约束。**不依赖 Windows**：文案错了，现场就没有第二次机会，
/// 所以它在 macOS 的 CI 上也必须被检查。
#[cfg(test)]
mod guidance_tests {
    use super::*;

    #[test]
    fn the_guidance_says_what_is_missing_and_what_to_install() {
        let text = runtime_missing_guidance();
        // 「缺什么」「装哪一版」：这两句缺任何一句，用户都只能干瞪眼。
        assert!(text.contains("WebView2"), "{text}");
        assert!(
            text.contains("Evergreen Standalone Installer") || text.contains("常青版"),
            "{text}"
        );
    }

    #[test]
    fn the_guidance_states_the_follow_up_and_the_background_degradation() {
        let text = runtime_missing_guidance();
        // 装完要重启进程（而不是重装 Agent）；期间 Agent 仍在后台干活。
        assert!(text.contains("重新打开任务中心"), "{text}");
        assert!(text.contains("后台运行"), "{text}");
        assert!(text.contains("不需要重装"), "{text}");
        // 后台能做什么必须如实：上传/提交只有任务中心界面能触发，不能写成「一切照常」。
        assert!(
            text.contains("上传与提交请等任务中心恢复后再操作"),
            "{text}"
        );
        assert!(!text.contains("上传与同步都不受影响"), "{text}");
        // 浏览器没被拉起来时用户还能手动输入地址。
        assert!(text.contains(RUNTIME_DOWNLOAD_URL), "{text}");
    }

    #[test]
    fn the_guidance_offers_self_service_before_manual_download() {
        let text = runtime_missing_guidance();
        // 最关键的一句：不用管理员也能装（否则现场直接放弃）。
        assert!(text.contains("不需要管理员权限"), "{text}");
        assert!(text.contains("托盘菜单"), "{text}");
        assert!(text.contains("IT"), "{text}");
    }

    #[test]
    fn the_install_prompt_says_it_needs_no_administrator() {
        let text = runtime_install_prompt();
        assert!(text.contains("不需要管理员权限"), "{text}");
        // 要说明「装好后会自动打开」与「不装会怎样」，否则用户不敢点。
        assert!(text.contains("任务中心会自动打开"), "{text}");
        assert!(text.contains("后台继续运行"), "{text}");
    }

    #[test]
    fn the_failure_guidance_carries_the_exit_code_and_the_offline_hint() {
        let text = runtime_install_failed_guidance(Some(1603));
        assert!(text.contains("1603"), "{text}");
        assert!(text.contains("内网离线安装包"), "{text}");
        assert!(text.contains("退出"), "{text}");
        // 自助安装失败后仍要留一条手动出路：官方下载页。
        assert!(text.contains(RUNTIME_DOWNLOAD_URL), "{text}");
        // 没有退出码（被超时/信号打断）时不能显示 "None" 这种实现细节。
        let no_code = runtime_install_failed_guidance(None);
        assert!(no_code.contains("无"), "{no_code}");
        assert!(!no_code.contains("None"), "{no_code}");
    }

    #[test]
    fn only_windows_treats_a_missing_probe_as_a_blocked_task_centre() {
        // Windows：探测不到运行时 = 任务中心打不开。
        assert!(!task_center_ready(true, None));
        assert!(task_center_ready(true, Some("120.0.0")));
        // macOS：系统 WebKit 恒可用，探测失败也可能只是 NSBundle 查询出错；
        // 把这里当成门禁会让 macOS 无故变成零窗口（Tauri 自己在 macOS 只 warn）。
        assert!(task_center_ready(false, None));
        assert!(task_center_ready(false, Some("17.0")));
    }

    #[test]
    fn the_download_page_is_the_official_https_one() {
        assert!(RUNTIME_DOWNLOAD_URL.starts_with("https://developer.microsoft.com/"));
        // 不能被换成厂商之外的域名：这条提示会直接变成点击后的落点。
        assert_eq!(
            RUNTIME_DOWNLOAD_URL,
            "https://developer.microsoft.com/microsoft-edge/webview2/"
        );
    }
}
