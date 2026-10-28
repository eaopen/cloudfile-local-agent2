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
