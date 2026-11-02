//! Windows 原生对话框：**没有窗口**的进程唯一还能跟用户说话的方式。
//!
//! 为什么需要它（2026-11-02 现场）：任务中心窗口由 WebView2 渲染，运行时缺失时
//! 进程一个窗口都建不出来。这时更不能静默：既要说明「为什么没有窗口、怎么装」，
//! 也要保住退出保护 —— 有独占任务没确认时，托盘的「退出」不能变成一个点了没反应的
//! 菜单项（没有任务中心窗口可问，就只能在这里问）。
//!
//! 非 Windows 上这里是空实现：桌面端两个平台共用同一份调用代码，不用写两套分支。
//! 注意 macOS 的 WebKit 探测**可能**返回失败（Tauri 自己只 warn 并继续建 webview），
//! 所以调用方不能把「探测失败」当成「一定没有窗口」——缺运行时的门禁只在 Windows 成立。

/// Win32 只认以 NUL 结尾的 UTF-16；`encode_utf16` 之后必须自己补结束符。
#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 「是 / 否」询问框。返回 `true` 表示用户点了「是」。
///
/// 没有真实窗口可作父窗口，因此显式带上 `MB_SETFOREGROUND | MB_TOPMOST`：
/// 登录项启动的进程弹框如果藏在别的窗口后面，用户会以为程序根本没反应。
#[cfg(windows)]
pub fn ask(title: &str, text: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONWARNING, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    };
    let (title, text) = (wide(title), wide(text));
    // SAFETY: 两个指针都指向本次调用期间存活的 NUL 结尾缓冲区；`hwnd` 传 null
    // 表示无属主窗口，MessageBox 允许这样用。
    let answer = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    answer == IDYES
}

/// 只有「确定」的告知框：用于没有窗口时的故障说明（[`ask`] 是提问，这里是纯告知）。
#[cfg(windows)]
pub fn notify(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let (title, text) = (wide(title), wide(text));
    // SAFETY: 同 [`ask`]：缓冲区在调用期间存活，无属主窗口是 MessageBox 支持的用法。
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

/// 非 Windows：桌面端总有窗口可问，这里是空实现（不弹框、当作「否」）。
#[cfg(not(windows))]
pub fn ask(_title: &str, _text: &str) -> bool {
    false
}

/// 非 Windows 的告知框：空实现（调用方只把它当「尽力而为」的补充提示）。
#[cfg(not(windows))]
pub fn notify(_title: &str, _text: &str) {}

#[cfg(test)]
mod tests {
    /// 非 Windows 的空实现必须明确回答「否」：调用方据此继续走有窗口的那条路，
    /// 不能因为平台差异而 panic（桌面端两个平台共用同一份调用代码）。
    #[cfg(not(windows))]
    #[test]
    fn without_a_windows_dialog_the_answer_is_always_no() {
        assert!(!super::ask("CloudFile", "说明"));
        // 告知框是纯副作用，空实现只要不 panic。
        super::notify("CloudFile", "说明");
    }
}
