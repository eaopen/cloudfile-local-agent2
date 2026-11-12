pub mod apps;
pub mod autostart;
/// 找 Chrome、打开扩展管理页 —— 引导页「安装扩展」那一步只提示、不代替用户操作。
pub mod browser;
pub mod guard;
pub mod hostreg;
pub mod ipc;
pub mod journal;
pub mod legacy_seafile;
pub mod native;
pub mod nx_router;
/// Runtime-neutral ApplicationProfile V1: optional metadata only, no file permission changes.
pub mod profile;
pub mod protocol_registration;
pub mod session;
pub mod settings;
pub mod storage;
pub mod update;
/// 桌面端在 GNU 工具链下必需的外部加载器（`WebView2Loader.dll`）。
pub mod webview2;
/// 随包携带的 WebView2 安装器：缺运行时时由 Agent 在当前用户下自助安装（免管理员）。
pub mod webview2_setup;
/// 无窗口时的原生询问框，仅 Windows —— 缺 WebView2 运行时与退出保护的最后出口。
pub mod winbox;
pub mod wire;

/// 子进程控制台窗口控制（其它平台是空操作）。
pub mod childproc;
/// 「启动」文件夹里的快捷方式（`.lnk`），仅 Windows —— 登录项的兜底途径。
#[cfg(windows)]
pub mod shortcut;
/// 进程内注册表访问，仅 Windows —— 用来替代 spawn `reg.exe`。
#[cfg(windows)]
pub mod winreg;
/// 本地工作副本（镜像）根目录的策略与一次性搬迁。
pub mod workspace;

/// 本构建自称的版本号。
///
/// 默认取工作区的 `Cargo.toml`（开发时手跑 `cargo build` 得到的就是它）；发版时由
/// `CLOUDFILE_AGENT2_VERSION` **在编译期**注入 —— 等价于 Go 侧的
/// `-ldflags "-X main.version=..."`。这样做而不是让发版脚本改 `Cargo.toml`，是因为
/// 升级链路上有三处会把这个字符串当成事实来比较：清单里的 `version`、
/// `update::probe` 拿到候选二进制 `--version` 后的比对、以及 `stage_release` 里
/// 「报告版本必须等于清单版本」。三者必须同源，而改仓库文件的方式一旦中途失败
/// 就会留下一个「构建出来的版本 ≠ 签发的版本」的中间态。
///
/// 值必须能通过 `update::valid_version`（`x.y.z`，可带 `-预发布`）；发版脚本会在
/// 构建**之前**校验这一点，因为带着非法版本号的构建连自己的健康检查都过不去。
pub const VERSION: &str = match option_env!("CLOUDFILE_AGENT2_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
/// 本平台注册的 Native Messaging Host ID。定义在 `hostreg`，这里只做转出 ——
/// 「注册用哪个名字」和「自带检查查哪个名字」必须是同一个常量，否则自检会在
/// 一个根本没被注册的 host 上给出「未注册」，而扩展其实连得上。
pub const HOST: &str = hostreg::HOST;
/// 本产品固定的 Chrome 扩展 ID。与 [`HOST`] 同理：注册、状态显示、引导页
/// 都必须引用同一个常量，不允许界面上再出现「手输 ID」的第二条路径。
pub const EXTENSION_ID: &str = hostreg::EXTENSION_ID;
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 给人看的本地时间戳：`2026-11-12 10:56:59`。
///
/// 日志（update-check / profile-refresh / nx-route）一律用它，现场不用再拿 Unix 秒去换算。
/// 程序自己读的状态文件（opened.json、update-announce.json 等）仍然存 `now()` 的秒数 ——
/// 那些是给代码比的，字符串反而会多一层解析。
#[cfg(windows)]
pub fn local_stamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime 只写这一个结构体，没有前置条件。
    let time = unsafe {
        let mut value = std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>();
        GetLocalTime(&mut value);
        value
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// 非 Windows 平台没有统一的本地时区接口（macOS 不是首发目标），退回 UTC 并标注 `Z`。
#[cfg(not(windows))]
pub fn local_stamp() -> String {
    let seconds = now();
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// 1970-01-01 起的天数 → (年, 月, 日)。Howard Hinnant 的 civil_from_days。
#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub mod cdc;
pub mod device;
pub mod exclusive;
/// 浏览器扩展的自更新（下载 + 校验 + 原子替换目录）。
pub mod extension;
/// 「只有属主能碰吗」—— 读侧加固的唯一判据（对应 Go 的 `internal/fsaccess`）。
pub mod fsaccess;
pub mod paired_session;
/// 本机自检：把「这台机器上装的东西到底能不能工作」变成一次可脚本化的问答。
pub mod selftest;
pub mod upload;
pub mod v2_edit;

mod v2_cdc;
