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
    if running.len() != 1 {
        bail!("检测到多个 NX 会话，无法确定应该打开在哪个窗口。请在目标 NX 窗口中手动打开本地工作副本。");
    }
    let one = &running[0];
    let Some(current) = one.executable.as_ref() else {
        bail!(
            "检测到 NX 正在运行，但无法核实它的安装版本。请在已打开的 NX 中手动打开本地工作副本。"
        );
    };
    if normalized(current) != normalized(ugraf) {
        bail!("已经运行的 NX 与 Agent 检测到的安装版本不同，为避免在错误版本中打开文件，请从现有 NX 窗口手动打开工作副本。");
    }
    Ok(Choice::RouteTo(one.pid))
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
                let executable = if handle.is_null() {
                    None
                } else {
                    let mut buf = [0u16; 32768];
                    let mut size = buf.len() as u32;
                    let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
                    CloseHandle(handle);
                    (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buf[..size as usize])))
                };
                processes.push(RunningNx { pid, executable });
            }
            if Process32NextW(snapshot, &mut entry) == 0 {
                break;
            }
        }
        CloseHandle(snapshot);
        Ok(processes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx_routes_only_to_one_exact_running_version() {
        let installed = Path::new("C:/Siemens/NX 2306/NXBIN/ugraf.exe");
        assert_eq!(choose(installed, &[]).unwrap(), Choice::StartNew);
        let session = RunningNx {
            pid: 123,
            executable: Some(installed.to_path_buf()),
        };
        assert_eq!(
            choose(installed, std::slice::from_ref(&session)).unwrap(),
            Choice::RouteTo(123)
        );
        assert!(choose(installed, &[session.clone(), session]).is_err());
        assert!(choose(
            installed,
            &[RunningNx {
                pid: 1,
                executable: None
            }]
        )
        .is_err());
        assert!(choose(
            installed,
            &[RunningNx {
                pid: 1,
                executable: Some(PathBuf::from("D:/UGS/NX 7.5/UGII/ugraf.exe")),
            }]
        )
        .is_err());
    }
}
