//! 随包携带的 WebView2 安装器：缺运行时时的**自助安装**入口。
//!
//! 为什么需要它（2026-11-02 现场追问）：只给一个下载页不算解决。现场机器往往
//! **没有管理员权限**，也可能根本打不开微软站点。Windows 的 WebView2 安装器以
//! **当前用户**身份运行时会装到 `%LOCALAPPDATA%`（不触发 UAC），所以把安装器随包
//! 带着、由 Agent 自己在当前用户下装，是唯一既不依赖管理员、也不依赖用户会操作的路径。
//!
//! 分发方式（两条都能用，代码不变，只换文件）：
//! - 在线：发行包内置微软官方引导器 `MicrosoftEdgeWebview2Setup.exe`（约 2MB，安装时联网）；
//! - 内网/离线：把同一路径换成 Evergreen 独立安装器（约 150MB，离线可用）。
//!
//! 安全边界：**只执行与随包 SHA-256 完全一致的安装器**。哈希由打包时写入
//! `installer.sha256`（与安装器同目录），也可以用 `CLOUDFILE_WEBVIEW2_SETUP_SHA256`
//! 显式覆盖；找不到期望哈希时一律拒绝执行，不提供「跳过校验」开关。

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 安装器在发行包里的目录名（与桌面进程同级的 `webview2/`）。
pub const INSTALLER_DIR: &str = "webview2";
/// 文件名。微软引导器与独立安装器同名，内网包直接替换文件即可。
pub const INSTALLER_NAME: &str = "MicrosoftEdgeWebview2Setup.exe";
/// 随包记录的 SHA-256 文件名（与安装器同目录）。
pub const DIGEST_NAME: &str = "installer.sha256";

/// 显式指定安装器路径（内网部署/现场排查用）；仍要过 SHA-256 校验。
pub const ENV_INSTALLER: &str = "CLOUDFILE_WEBVIEW2_SETUP";
/// 显式指定期望的 SHA-256（十六进制，大小写不敏感）。
pub const ENV_INSTALLER_SHA256: &str = "CLOUDFILE_WEBVIEW2_SETUP_SHA256";

/// 静默安装参数：`/silent` 不显示 UI，`/install` 安装（当前用户）。
pub const SILENT_INSTALL_ARGS: [&str; 2] = ["/silent", "/install"];
/// 可见安装参数：保留微软安装器自己的界面，用户能看到进度（显式点击「安装」时用）。
pub const VISIBLE_INSTALL_ARGS: [&str; 1] = ["/install"];

/// 默认等待上限：引导器要联网下载运行时，慢网上可能几分钟。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);

/// 安装结果。`installed` 以**运行时探测**为准，不以安装器退出码为准 ——
/// 安装器返回 0 只说明它自己结束了，运行时是否真的可用要重新问系统。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub installed: bool,
    pub version: Option<String>,
}

/// 可能放着安装器的位置，按优先级排列。
///
/// 版本目录每次升级都会换（`bin/<版本>/`），所以不能只认桌面进程同级：就地装过一次的
/// 机器上，安装器应该落在**安装根**的 `webview2/` 里，各版本共用一份。
pub fn candidates(exe_dir: &Path, install_root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in std::iter::once(exe_dir).chain(install_root) {
        for path in [
            root.join(INSTALLER_DIR).join(INSTALLER_NAME),
            root.join(INSTALLER_NAME),
        ] {
            // 开发/绿色运行时 exe 目录可能就等于安装根，去重免得错误信息里同一路径出现两次。
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// 找到可用的安装器并校验内容，返回路径。找不到/校验不过都返回带原因的错误。
pub fn locate(exe_dir: &Path, install_root: Option<&Path>) -> Result<PathBuf> {
    // 显式路径优先：内网部署线上就一个文件，不必改安装器摆放位置。
    if let Some(value) = std::env::var_os(ENV_INSTALLER).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        verify_expected(&path)?;
        return Ok(path);
    }
    let candidates = candidates(exe_dir, install_root);
    for path in &candidates {
        // 校验失败是「找到了但不能用」，直接报出来 —— 悄悄跳过会让现场以为包没带。
        if path.is_file() {
            verify_expected(path)?;
            return Ok(path.clone());
        }
    }
    bail!(
        "未随包提供 WebView2 安装器（查找过：{}）",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    )
}

/// 安装器是否可用（定位 + 校验都通过）。给 `--status` 与托盘菜单用。
pub fn available(exe_dir: &Path, install_root: Option<&Path>) -> bool {
    locate(exe_dir, install_root).is_ok()
}

/// 把随包安装器落到**安装根**的 `webview2/`，让各版本目录共用一份。
///
/// 为什么需要（2026-11-02）：`--install` 只从签名清单下载两个二进制到 `bin/<版本>/`，
/// 发行 ZIP 里的 `webview2/` 不会自动跟着走。没有这一步，装完的机器上桌面进程找不到
/// 安装器，缺运行时又会退回「手动下载」—— 而现场往往正是没有管理员、也打不开微软站点。
///
/// 返回安装器最终路径；发行包里没有安装器时返回 `Ok(None)`（不算失败：安装本身不依赖它）。
/// 找到了但校验不过则报错：不能把来源不明的文件复制到安装根当可信安装器。
pub fn ensure_in_install_root(exe_dir: &Path, install_root: &Path) -> Result<Option<PathBuf>> {
    let Some(source) = candidates(exe_dir, Some(install_root))
        .into_iter()
        .find(|path| path.is_file())
    else {
        return Ok(None);
    };
    let expected = expected_digest(&source)?;
    verify(&source, &expected)?;
    let target_dir = install_root.join(INSTALLER_DIR);
    let target = target_dir.join(INSTALLER_NAME);
    if target == source {
        return Ok(Some(target));
    }
    // 目标已就位且与源同源时不动它：升级/重装反复跑不该改动文件（也可能正被占用）。
    if target.is_file() && verify(&target, &expected).is_ok() {
        return Ok(Some(target));
    }
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("无法创建 WebView2 安装器目录：{}", target_dir.display()))?;
    std::fs::copy(&source, &target).with_context(|| {
        format!(
            "无法把 WebView2 安装器复制到安装根：{} -> {}",
            source.display(),
            target.display()
        )
    })?;
    // 边车按**实际内容**重写：源目录的边车可能不存在（环境变量指定安装器），
    // 而安装根这份以后要单独被桌面进程校验。
    std::fs::write(target_dir.join(DIGEST_NAME), format!("{expected}\n"))
        .with_context(|| format!("无法写入 {}", target_dir.join(DIGEST_NAME).display()))?;
    Ok(Some(target))
}

/// 读期望的 SHA-256：环境变量优先，其次安装器同目录的 `installer.sha256`
/// （也接受 `sha256sum` 那种「哈希 + 文件名」的整行写法）。
pub fn expected_digest(installer: &Path) -> Result<String> {
    if let Some(value) = std::env::var_os(ENV_INSTALLER_SHA256)
        .map(|value| value.to_string_lossy().trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    {
        return Ok(value);
    }
    let sidecars = [
        installer.with_file_name(DIGEST_NAME),
        installer.with_extension("sha256"),
    ];
    for sidecar in sidecars {
        if let Some(digest) = read_digest(&sidecar) {
            return Ok(digest);
        }
    }
    bail!(
        "未找到 WebView2 安装器校验文件（{} 或 {}），已拒绝执行",
        installer.with_file_name(DIGEST_NAME).display(),
        installer.with_extension("sha256").display()
    )
}

/// 一个「定位 + 校验」组合：找到路径后立刻校验，供 [`locate`] 复用。
fn verify_expected(installer: &Path) -> Result<()> {
    if !installer.is_file() {
        bail!("WebView2 安装器不存在：{}", installer.display());
    }
    let expected = expected_digest(installer)?;
    verify(installer, &expected)
}

/// 校验文件哈希。**不一致就拒绝执行**：这个 exe 会以当前用户身份运行安装程序，
/// 不能拿来源不明的文件冒险。
pub fn verify(installer: &Path, expected_hex: &str) -> Result<()> {
    let actual = sha256_hex(installer)?;
    if !actual.eq_ignore_ascii_case(expected_hex.trim()) {
        bail!(
            "WebView2 安装器校验失败：期望 {}，实际 {}。已拒绝执行（{}）",
            expected_hex.trim(),
            actual,
            installer.display()
        );
    }
    Ok(())
}

/// 文件的 SHA-256（小写十六进制）。
pub fn sha256_hex(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("无法读取 WebView2 安装器：{}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("读取 WebView2 安装器失败：{}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// 读 `installer.sha256`：取第一个空白分隔的 token，容忍 `sha256sum` 的整行格式。
fn read_digest(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    let token = body.split_whitespace().next()?;
    let digest = token.trim().to_ascii_lowercase();
    // 只接受 64 位十六进制：配置文件写坏时不能把任意字符串当哈希比。
    (digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit())).then_some(digest)
}

/// 运行安装器并等待结束，然后**以运行时探测为准**判断是否装好。
///
/// `silent=false` 时保留安装器自己的界面：用户手动点「安装」时应该看得到进度；
/// `silent=true` 给 IT 脚本与登录自愈用。
pub fn install(installer: &Path, silent: bool, timeout: Duration) -> Result<Outcome> {
    verify_expected(installer)?;
    let mut command = std::process::Command::new(installer);
    command
        .args(if silent {
            SILENT_INSTALL_ARGS.as_slice()
        } else {
            VISIBLE_INSTALL_ARGS.as_slice()
        })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if silent {
        // 静默安装不该弹黑框；可见安装本来就是 GUI，不用额外处理。
        crate::childproc::hide(&mut command);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("无法启动 WebView2 安装器：{}", installer.display()))?;
    let deadline = Instant::now() + timeout;
    let exit_code = loop {
        match child.try_wait().context("等待 WebView2 安装器退出失败")? {
            Some(status) => break status.code(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("WebView2 安装超时（{} 秒）", timeout.as_secs());
            }
            None => std::thread::sleep(Duration::from_millis(250)),
        }
    };
    // 安装器退出与运行时注册之间有个短暂窗口；以探测为准，最多再等 30 秒。
    // 非 Windows 上不存在 WebView2 运行时，轮询没有意义（也避免脚本白等 30 秒）。
    let version = if cfg!(windows) {
        wait_for_runtime(Duration::from_secs(30))
    } else {
        crate::apps::webview2_version()
    };
    Ok(Outcome {
        exit_code,
        installed: version.is_some(),
        version,
    })
}

/// 轮询运行时探测，直到出现或超时。只有 Windows 会走到这里（其它平台没有 WebView2）。
fn wait_for_runtime(timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(version) = crate::apps::webview2_version() {
            return Some(version);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fake(root: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn candidates_prefer_the_package_layout_and_fall_back_to_install_root() {
        let exe = Path::new("C:/agent/bin/2.0.17");
        let root = Path::new("C:/agent");
        let paths = candidates(exe, Some(root));
        assert_eq!(
            paths[0],
            PathBuf::from("C:/agent/bin/2.0.17/webview2/MicrosoftEdgeWebview2Setup.exe")
        );
        // 升级会建新版本目录，就地装过的安装器必须能从安装根复用。
        assert!(
            paths
                .iter()
                .any(|path| path
                    == &PathBuf::from("C:/agent/webview2/MicrosoftEdgeWebview2Setup.exe"))
        );
    }

    #[test]
    fn only_a_digest_matching_the_file_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let installer = write_fake(dir.path(), "setup.exe", b"fake installer");
        let digest = sha256_hex(&installer).unwrap();
        assert!(verify(&installer, &digest).is_ok());
        // 大小写不敏感：现场手抄的哈希常常是大写。
        assert!(verify(&installer, &digest.to_uppercase()).is_ok());

        std::fs::write(&installer, b"tampered").unwrap();
        let error = verify(&installer, &digest).unwrap_err().to_string();
        assert!(error.contains("校验失败"), "{error}");
        assert!(error.contains("已拒绝执行"), "{error}");
    }

    #[test]
    fn sidecar_is_read_in_sha256sum_format_too() {
        let dir = tempfile::tempdir().unwrap();
        let installer = write_fake(dir.path(), "setup.exe", b"payload");
        let digest = sha256_hex(&installer).unwrap();
        std::fs::write(
            dir.path().join(DIGEST_NAME),
            format!("{digest}  setup.exe\n"),
        )
        .unwrap();
        assert_eq!(expected_digest(&installer).unwrap(), digest);

        // 写坏的文件不能当成哈希用（否则等于没有校验）。
        std::fs::write(dir.path().join(DIGEST_NAME), "not-a-digest").unwrap();
        assert!(expected_digest(&installer).is_err());
    }

    #[test]
    fn locate_refuses_a_valid_file_without_any_digest() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join(INSTALLER_DIR);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(INSTALLER_NAME), b"payload").unwrap();
        let error = locate(dir.path(), None).unwrap_err().to_string();
        assert!(error.contains("校验文件"), "{error}");
    }

    #[test]
    fn locate_finds_the_bundled_installer_when_the_digest_matches() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join(INSTALLER_DIR);
        std::fs::create_dir_all(&package).unwrap();
        let installer = package.join(INSTALLER_NAME);
        std::fs::write(&installer, b"payload").unwrap();
        let digest = sha256_hex(&installer).unwrap();
        std::fs::write(package.join(DIGEST_NAME), &digest).unwrap();
        assert_eq!(locate(dir.path(), None).unwrap(), installer);
        assert!(available(dir.path(), None));
    }

    #[test]
    fn the_installer_is_copied_to_the_install_root_so_every_version_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        // 版本目录（本次安装运行的 exe 所在）里有发行包带来的安装器。
        let version_dir = dir.path().join("bin/2.0.17");
        let package = version_dir.join(INSTALLER_DIR);
        std::fs::create_dir_all(&package).unwrap();
        let installer = package.join(INSTALLER_NAME);
        std::fs::write(&installer, b"payload").unwrap();
        let digest = sha256_hex(&installer).unwrap();
        std::fs::write(package.join(DIGEST_NAME), &digest).unwrap();

        // 安装根里还没有：应当复制过去并写好边车，后续版本目录都从这里找。
        let install_root = dir.path().join("install-root");
        let target = ensure_in_install_root(&version_dir, &install_root)
            .unwrap()
            .unwrap();
        assert_eq!(
            target,
            install_root.join(INSTALLER_DIR).join(INSTALLER_NAME)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        assert!(verify(&target, &digest).is_ok());
        // 桌面进程/新版版本目录都能在安装根定位到它。
        assert!(available(
            dir.path().join("bin/2.0.18").as_path(),
            Some(&install_root)
        ));

        // 幂等：再跑一次不报错、返回同一路径。
        assert_eq!(
            ensure_in_install_root(&version_dir, &install_root)
                .unwrap()
                .unwrap(),
            target
        );
    }

    #[test]
    fn a_tampered_package_installer_is_never_copied_into_the_install_root() {
        let dir = tempfile::tempdir().unwrap();
        let version_dir = dir.path().join("bin/2.0.17");
        let package = version_dir.join(INSTALLER_DIR);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(INSTALLER_NAME), b"payload").unwrap();
        std::fs::write(package.join(DIGEST_NAME), "0".repeat(64)).unwrap();
        let error = ensure_in_install_root(&version_dir, &dir.path().join("install-root"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("校验失败"), "{error}");
        assert!(!dir
            .path()
            .join("install-root")
            .join(INSTALLER_DIR)
            .join(INSTALLER_NAME)
            .exists());
    }

    #[test]
    fn no_bundled_installer_is_not_an_installation_error() {
        let dir = tempfile::tempdir().unwrap();
        let version_dir = dir.path().join("bin/2.0.17");
        std::fs::create_dir_all(&version_dir).unwrap();
        assert_eq!(
            ensure_in_install_root(&version_dir, &dir.path().join("install-root")).unwrap(),
            None
        );
    }

    #[test]
    fn a_bare_machine_reports_why_it_cannot_offer_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let error = locate(dir.path(), None).unwrap_err().to_string();
        assert!(error.contains("未随包提供"), "{error}");
        assert!(!available(dir.path(), None));
        // 已给出的候选位置要写进错误里，现场才知道该把安装器放到哪。
        assert!(error.contains(INSTALLER_NAME), "{error}");
    }
}
