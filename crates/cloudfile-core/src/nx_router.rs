//! Native Windows NX session detection, deliberately separate from file authority.
//! NX 7.5 has a proven -retrieve: path when no session exists; only route
//! through ugs_router.exe if exactly one *matching* installed NX is running.
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RunningNx {
    pub pid: u32,
    // None means a session exists but its executable path could not be inspected.
    pub executable: Option<PathBuf>,
    /// 进程创建时间（Unix 秒）。多个会话时用它判断"哪个是最近打开的"。
    /// None 表示查不到（权限/平台），此时按 pid 大小兜底。
    pub started: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    StartNew,
    RouteTo(u32),
}

fn normalized(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical
        .to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches("\\\\?\\")
        .to_lowercase()
}

pub fn choose(ugraf: &Path, running: &[RunningNx]) -> Result<Choice> {
    if running.is_empty() {
        return Ok(Choice::StartNew);
    }
    let installed = normalized(ugraf);
    // 只认与 Agent 检测到的安装**同一份可执行文件**的会话；别的版本一概不碰。
    let matching: Vec<&RunningNx> = running
        .iter()
        .filter(|item| {
            item.executable
                .as_ref()
                .is_some_and(|current| normalized(current) == installed)
        })
        .collect();

    if matching.is_empty() {
        // 一个都核实不了（多半是权限）与"核实了但版本不同"要分开说，处理建议不一样。
        if running
            .iter()
            .all(|item| item.executable.as_ref().is_none_or(|_| true))
            && running.iter().any(|item| item.executable.is_none())
        {
            bail!(
                "检测到 NX 正在运行，但无法核实它的安装版本。请在已打开的 NX 中手动打开本地工作副本。"
            );
        }
        bail!("已经运行的 NX 与 Agent 检测到的安装版本不同，为避免在错误版本中打开文件，请从现有 NX 窗口手动打开工作副本。");
    }
    if matching.len() == 1 {
        return Ok(Choice::RouteTo(matching[0].pid));
    }

    // 多个会话：优先用户此刻正在用的那个（前台窗口），否则用最近打开的那个。
    // 现场就是为了"再点一次别又开一个窗口"，报错让人手工打开反而更麻烦。
    if let Some(pid) = foreground_pid() {
        if let Some(item) = matching.iter().find(|item| item.pid == pid) {
            return Ok(Choice::RouteTo(item.pid));
        }
    }
    let newest = matching
        .iter()
        .max_by_key(|item| (item.started.unwrap_or(0), item.pid))
        .expect("matching 非空");
    Ok(Choice::RouteTo(newest.pid))
}

/// 当前前台窗口属于哪个进程；不是 NX 或取不到就返回 None。
#[cfg(windows)]
fn foreground_pid() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    // SAFETY: 两个调用都只读句柄/写一个 u32，没有前置条件。
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        (pid != 0).then_some(pid)
    }
}

#[cfg(not(windows))]
fn foreground_pid() -> Option<u32> {
    None
}

pub fn router(ugraf: &Path) -> Option<PathBuf> {
    let path = ugraf.parent()?.join("ugs_router.exe");
    path.is_file().then_some(path)
}

#[cfg(windows)]
pub fn running() -> Result<Vec<RunningNx>> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            bail!("无法枚举现有 NX 进程，请从 NX 窗口手动打开文件");
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut processes = Vec::new();
        if Process32FirstW(snapshot, &mut entry) == 0 {
            CloseHandle(snapshot);
            bail!("无法读取 NX 进程列表，请从 NX 窗口手动打开文件");
        }
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|x| *x == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            if name.eq_ignore_ascii_case("ugraf.exe") {
                let pid = entry.th32ProcessID;
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                let (executable, started) = if handle.is_null() {
                    (None, None)
                } else {
                    let mut buf = [0u16; 32768];
                    let mut size = buf.len() as u32;
                    let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
                    let executable = (ok != 0)
                        .then(|| PathBuf::from(OsString::from_wide(&buf[..size as usize])));
                    let started = created_at(handle);
                    CloseHandle(handle);
                    (executable, started)
                };
                processes.push(RunningNx {
                    pid,
                    executable,
                    started,
                });
            }
            if Process32NextW(snapshot, &mut entry) == 0 {
                break;
            }
        }
        CloseHandle(snapshot);
        Ok(processes)
    }
}

/// 进程创建时间（Unix 秒）。取不到就 None —— 排序有 pid 兜底，不该因此失败。
#[cfg(windows)]
unsafe fn created_at(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
    if ok == 0 {
        return None;
    }
    let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    // FILETIME 是 1601-01-01 起的 100ns；换算到 Unix 秒。
    const EPOCH_DIFFERENCE: u64 = 11_644_473_600;
    let seconds = ticks / 10_000_000;
    seconds.checked_sub(EPOCH_DIFFERENCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(pid: u32, executable: Option<&str>, started: Option<u64>) -> RunningNx {
        RunningNx {
            pid,
            executable: executable.map(PathBuf::from),
            started,
        }
    }

    #[test]
    fn nx_routes_only_to_one_exact_running_version() {
        let installed = Path::new("C:/Siemens/NX 2306/NXBIN/ugraf.exe");
        assert_eq!(choose(installed, &[]).unwrap(), Choice::StartNew);
        assert_eq!(
            choose(
                installed,
                &[session(
                    123,
                    Some("C:/Siemens/NX 2306/NXBIN/ugraf.exe"),
                    Some(1)
                )]
            )
            .unwrap(),
            Choice::RouteTo(123)
        );
        // 路径读不到：无法核实版本，宁可让用户手工打开。
        assert!(choose(installed, &[session(1, None, Some(5))]).is_err());
        // 跑的是另一个版本：不能往它里面塞文件。
        assert!(choose(
            installed,
            &[session(1, Some("D:/UGS/NX 7.5/UGII/ugraf.exe"), Some(5))]
        )
        .is_err());
    }

    /// 多个 NX 窗口时选"最近打开的那个"，而不是报错让人手工打开。
    #[test]
    fn several_sessions_route_to_the_most_recent_one() {
        let installed = Path::new("C:/Siemens/NX 2306/NXBIN/ugraf.exe");
        let path = "C:/Siemens/NX 2306/NXBIN/ugraf.exe";
        let running = [
            session(100, Some(path), Some(1_000)),
            session(200, Some(path), Some(3_000)),
            session(150, Some(path), Some(2_000)),
        ];
        assert_eq!(choose(installed, &running).unwrap(), Choice::RouteTo(200));

        // 拿不到启动时间时按 pid 兜底（pid 更大的更新）。
        let unknown = [
            session(100, Some(path), None),
            session(700, Some(path), None),
        ];
        assert_eq!(choose(installed, &unknown).unwrap(), Choice::RouteTo(700));

        // 混着别的版本：只在同一版本里挑最近的。
        let mixed = [
            session(100, Some(path), Some(1_000)),
            session(200, Some(path), Some(2_000)),
            session(999, Some("D:/UGS/NX 7.5/UGII/ugraf.exe"), Some(9_999)),
        ];
        assert_eq!(choose(installed, &mixed).unwrap(), Choice::RouteTo(200));
    }
}
