//! Native messaging host registration owned by the agent itself.
//!
//! The historical installer was a Python or PowerShell script, which meant the
//! user's machine needed a scripting runtime just to connect the extension. The
//! agent now performs every registration step itself, keeps a rollback copy of
//! whatever it replaced, and refuses to overwrite a host owned by another
//! program. A registration from an older CloudFile Agent is ours: an explicit
//! install upgrades it without a second confirmation.
//!
//! macOS/Linux share `com.cloudfile.agent2`; Windows retains its fixed controller
//! slot `com.cloudfile.current_agent`. Registration backs up recognized CloudFile
//! manifests and requires explicit takeover for unrelated programs.
use crate::storage;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Historical Rust dev identity retained for explicit CLI registration and tests.
pub const DEV_HOST: &str = "com.cloudfile.agent22.dev";

/// **本平台**上真正注册的 Host ID。
///
/// Windows 上必须是 `com.cloudfile.current_agent` —— 这是扩展的契约，不是偏好：
/// `cloudfile-chrome-extension/background.js` 的 `preferredNative()` 在 Windows 上**只**向这一个
/// Host 发消息，且 `tests/agent2-routing.cjs` 用两条用例把它钉死了
/// （`Windows never probes Go Hosts when its fixed Host is missing`、
/// `a Windows Rust controller uses the same Host ID`）。也就是说 Windows 上的控制器
/// 只分到一个槽位，谁当控制器谁就得占它 —— 否则扩展直接回「本机 Agent 未就绪」。
///
/// Go Agent 已在发布前废弃。macOS/Linux 使用同一个 Rust Host，
/// 两个系统在各自用户目录注册，不需要用不同名称隔离。
pub const HOST: &str = if cfg!(windows) {
    "com.cloudfile.current_agent"
} else {
    "com.cloudfile.agent2"
};

/// Hosts the shipped agents own. Listed so takeover can be explicit and
/// reversible; never rewritten during development.
///
/// 注意：Windows 上 [`HOST`] 就在这个列表里（控制器槽位只有那一个）。这不矛盾 ——
/// 列表的用途是「替换别人的注册必须显式」，而 `install` + `--takeover` 正是那条显式路径。
pub const LEGACY_HOSTS: &[&str] = &["com.cloudfile.agent2", "com.cloudfile.current_agent"];

pub const DESCRIPTION: &str = "CloudFile Agent 2.2 Native Host";

/// 本产品对应的 Chrome 扩展 ID：**固定值**，不是每台机器一个。
///
/// 扩展的 `manifest.json` 里钉了 `key`（RSA 公钥），Chrome 由它推导 ID —— 所以
/// ID 与解压到哪个目录无关，也不用用户去 `chrome://extensions` 里抄一串字符再回来粘贴。
/// 「谁能调用本机 Agent」这件事仍然由 manifest 的 `allowed_origins` 决定，只是这个值
/// 现在来自产品常量，而不是一次手输。
///
/// 换扩展公钥（重新打包 `key`）时必须同步改这里，否则注册出来的 Host 会拒绝
/// 真正的扩展 —— `hostreg::install` 只校验 32 位 a-p 的形状，认不出「ID 是另一把钥匙的」。
pub const EXTENSION_ID: &str = "gocnpbhnbikcpnlafmodflddgnmfcjoc";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub host: String,
    pub manifest_path: PathBuf,
    pub binary: Option<PathBuf>,
    pub extension_id: Option<String>,
    /// A different program currently owns this host name.
    pub foreign: bool,
}

pub fn validate_extension_id(value: &str) -> Result<()> {
    if value.len() != 32 || !value.bytes().all(|b| (b'a'..=b'p').contains(&b)) {
        bail!("invalid Chrome extension ID");
    }
    Ok(())
}

pub fn validate_host(value: &str) -> Result<()> {
    let ok = value.len() <= 128
        && value.contains('.')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !ok {
        bail!("invalid native messaging host name");
    }
    Ok(())
}

/// 写进 manifest、以及判断注册归属时使用的路径文本。
///
/// Windows 的 `canonicalize()` 会给出 `\\?\D:\...` 这种 verbatim 前缀。Chrome 的 native
/// messaging 清单里放的是**普通绝对路径**（Go 侧一直如此、现场跑通过），而 verbatim 前缀
/// 在 Chrome 内部（`base::CommandLine::FromString` → `CreateProcess`）怎么被处理没有明确
/// 保证。这条路径是「扩展能不能连上 Agent」的关键路径，不该留一个无法确认的变量。
/// 剥前缀只影响这个字符串：文件操作仍然用原来的 `PathBuf`。
fn manifest_path_text(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        // 只剥裸的 `\\?\`。`\\?\UNC\server\share` 剥了会变成非法的 `UNC\...`，必须保留。
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            if !rest.to_ascii_uppercase().starts_with("UNC\\") {
                return rest.to_string();
            }
        }
    }
    text
}

/// The exact manifest Chrome requires. Field order is fixed so a takeover diff
/// stays readable.
pub fn manifest(name: &str, binary: &Path, extension_id: &str) -> Result<Value> {
    validate_host(name)?;
    validate_extension_id(extension_id)?;
    if !binary.is_absolute() {
        bail!("native host binary path must be absolute");
    }
    Ok(json!({
        "name": name,
        "description": DESCRIPTION,
        "path": manifest_path_text(binary),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")],
    }))
}

/// Chrome looks for the manifest in a fixed per-platform directory.
pub fn default_manifest_dir(
    os: &str,
    home: &Path,
    local_app_data: Option<&Path>,
) -> Result<PathBuf> {
    match os {
        "macos" => Ok(home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts")),
        // Linux Chrome 使用当前用户配置目录，与 macOS/Windows 注册互相隔离。
        "linux" => Ok(home.join(".config/google-chrome/NativeMessagingHosts")),
        "windows" => {
            let local = local_app_data.context("LOCALAPPDATA is unavailable")?;
            Ok(local.join("CloudFileAgent").join("native-host"))
        }
        _ => bail!("native host registration supports Windows, macOS and Linux only"),
    }
}

pub fn manifest_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("home directory unavailable")?;
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let current = default_manifest_dir(std::env::consts::OS, &home, local.as_deref())?;
    // 老机器（升级上来的）注册目录还叫 `CloudFileAgent22`：它还在就继续用它 ——
    // 否则新目录是空的，`state()` 会报「未注册」，连回退与升级接管都找不到原注册。
    // 重新 `--install` / `--register-host` 一次就会迁到新目录。
    #[cfg(windows)]
    {
        if let Some(local) = &local {
            let legacy = local.join("CloudFileAgent22").join("native-host");
            if !current.is_dir() && legacy.is_dir() {
                return Ok(legacy);
            }
        }
    }
    Ok(current)
}

pub fn manifest_path(dir: &Path, host: &str) -> PathBuf {
    dir.join(format!("{host}.json"))
}

/// Where the replaced manifest is kept so a bad upgrade can be undone offline.
pub fn previous_manifest_path(dir: &Path, host: &str) -> PathBuf {
    dir.join(format!("{host}.previous.json"))
}

/// 当前用户的 Chrome NativeMessagingHosts 键（带 hive 前缀的完整路径）。
/// 纯函数，所以「查存在」和「写/删」用的一定是同一个键，不会漂移。
pub fn registry_key(host: &str) -> String {
    format!("HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\{host}")
}

/// 这次注册要在注册表里落下的终态：`Some(path)` = 默认值写成 manifest 路径，
/// `None` = 整个键删掉。拆成纯函数是为了让「写什么」在跨平台测试里被钉住，
/// 真正的落盘由 [`winreg`]（Windows，进程内）完成。
pub fn registry_update(host: &str, manifest: Option<&Path>) -> Result<(String, Option<String>)> {
    validate_host(host)?;
    let key = registry_key(host);
    match manifest {
        Some(path) => {
            if !path.is_absolute() {
                bail!("native host manifest path must be absolute");
            }
            Ok((key, Some(path.to_string_lossy().into_owned())))
        }
        None => Ok((key, None)),
    }
}

#[cfg(windows)]
fn apply_registry(host: &str, manifest: Option<&Path>) -> Result<()> {
    // 进程内读写：以前每注册一次要起两个 `reg.exe`（先 query 探存在、再 add/delete），
    // 在 GUI 父进程下就是两次黑窗口，且机器禁用 reg.exe 时直接失败。
    let (key, value) = registry_update(host, manifest)?;
    match value {
        Some(path) => crate::winreg::write_string(&key, "", &path),
        // 删掉整个键（含子键）。键本来就不在也算成功：「删完不存在」是调用方要的终态。
        None => crate::winreg::delete_key(&key),
    }
}

#[cfg(not(windows))]
fn apply_registry(host: &str, manifest: Option<&Path>) -> Result<()> {
    // Unix 侧注册就是固定目录里的一个文件，没有注册表可写；
    // 这里只保留 host/manifest 的校验（与 Windows 口径一致），然后什么都不做。
    registry_update(host, manifest).map(|_| ())
}

pub fn state(dir: &Path, host: &str, _expected_binary: Option<&Path>) -> Result<Registration> {
    validate_host(host)?;
    let path = manifest_path(dir, host);
    let mut registration = Registration {
        host: host.to_string(),
        manifest_path: path.clone(),
        binary: None,
        extension_id: None,
        foreign: false,
    };
    if !path.is_file() {
        return Ok(registration);
    }
    let value: Value = storage::read(&path)?;
    let binary = value["path"].as_str().map(PathBuf::from);
    registration.extension_id = value["allowed_origins"]
        .as_array()
        .and_then(|origins| origins.first())
        .and_then(|origin| origin.as_str())
        .and_then(|origin| origin.strip_prefix("chrome-extension://"))
        .map(|origin| origin.trim_end_matches('/').to_string());
    // Foreign means "a different program owns this slot". An older CloudFile
    // Agent registration is ours: the user's explicit install/upgrade is the
    // consent to retire it, without a second confirmation prompt.
    registration.foreign = !cloudfile_owned_manifest(&value);
    registration.binary = binary;
    Ok(registration)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallOutcome {
    pub host: String,
    pub manifest_path: PathBuf,
    pub binary: PathBuf,
    /// A previous manifest was preserved at `previous_manifest_path`.
    pub backed_up: bool,
    pub previous_manifest_path: PathBuf,
}

/// Registers exactly one host for the current user.
///
/// `takeover` is required to replace a host that another program currently
/// owns; the previous manifest is always preserved first.
pub fn install(
    dir: &Path,
    host: &str,
    extension_id: &str,
    binary: &Path,
    takeover: bool,
) -> Result<InstallOutcome> {
    validate_extension_id(extension_id)?;
    validate_host(host)?;
    let binary = binary
        .canonicalize()
        .context("native host binary is missing")?;
    if !binary.is_file() {
        bail!("native host binary is missing");
    }
    if cfg!(windows) && binary.extension().is_none() {
        bail!("native host binary must keep its executable extension");
    }
    storage::private_dir(dir)?;
    let path = manifest_path(dir, host);
    let previous = previous_manifest_path(dir, host);
    let mut backed_up = false;
    if path.exists() {
        let current: Value = storage::read(&path)?;
        let replaced = manifest(host, &binary, extension_id)?;
        if current != replaced {
            if !takeover && !cloudfile_owned_manifest(&current) {
                bail!("{host} is registered by another program; run takeover to replace it");
            }
            fs::rename(&path, &previous)?;
            backed_up = true;
        }
    }
    let bytes = serde_json::to_vec_pretty(&manifest(host, &binary, extension_id)?)?;
    storage::write_bytes(&path, &bytes)?;
    // A failed registry write leaves the manifest in place but the old key
    // pointing elsewhere; that is the safe direction and is reported.
    if cfg!(windows) {
        apply_registry(host, Some(&path))?;
    }
    Ok(InstallOutcome {
        host: host.to_string(),
        manifest_path: path,
        binary,
        backed_up,
        previous_manifest_path: previous,
    })
}

/// True for a manifest written by a CloudFile agent: this Rust host, the older
/// .NET agent, or another shipped CloudFile build. Unknown third-party programs
/// are not ours and still require an explicit `--takeover`.
fn cloudfile_owned_manifest(value: &Value) -> bool {
    let description = value["description"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if description.contains("cloudfile") {
        return true;
    }
    value["path"]
        .as_str()
        .map(Path::new)
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().contains("cloudfile"))
}

/// Removes only a manifest this agent owns. A foreign host is left untouched.
pub fn unregister(dir: &Path, host: &str) -> Result<PathBuf> {
    validate_host(host)?;
    let path = manifest_path(dir, host);
    if !path.exists() {
        if cfg!(windows) {
            apply_registry(host, None)?;
        }
        return Ok(path);
    }
    let value: Value = storage::read(&path)?;
    let owned = value["description"]
        .as_str()
        .is_some_and(|text| text.starts_with("CloudFile Agent"))
        && value["type"].as_str() == Some("stdio");
    if !owned {
        bail!("{host} was not registered by this agent; refusing to remove it");
    }
    fs::remove_file(&path)?;
    if cfg!(windows) {
        apply_registry(host, None)?;
    }
    Ok(path)
}

/// Restores the manifest that was replaced by the last successful install.
pub fn rollback(dir: &Path, host: &str) -> Result<PathBuf> {
    validate_host(host)?;
    let previous = previous_manifest_path(dir, host);
    if !previous.is_file() {
        bail!("no previous native host registration to restore");
    }
    let value: Value = storage::read(&previous)?;
    if value["name"].as_str() != Some(host) || value["type"].as_str() != Some("stdio") {
        bail!("previous registration is not a usable native host manifest");
    }
    let path = manifest_path(dir, host);
    let bytes = serde_json::to_vec_pretty(&value)?;
    storage::write_bytes(&path, &bytes)?;
    if cfg!(windows) {
        apply_registry(host, Some(&path))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    /// An absolute directory for the host OS. `Path::is_absolute` is false for
    /// `/cloudfile` on Windows, and `registry_update` refuses a manifest
    /// path that is not absolute, so a Unix literal would fail there.
    #[cfg(windows)]
    const BASE: &str = "C:/cloudfile";
    #[cfg(not(windows))]
    const BASE: &str = "/cloudfile";

    fn binary(dir: &Path) -> PathBuf {
        let name = if cfg!(windows) {
            "cloudfile-native-host.exe"
        } else {
            "cloudfile-native-host"
        };
        let path = dir.join(name);
        fs::write(&path, b"bin").unwrap();
        path.canonicalize().unwrap()
    }

    #[test]
    fn manifest_matches_the_chrome_contract() {
        let (_dir, base) = crate::storage::test_root();
        let path = binary(&base);
        let value = manifest(DEV_HOST, &path, ID).unwrap();
        assert_eq!(value["name"], DEV_HOST);
        assert_eq!(value["type"], "stdio");
        // 写进清单的是**普通绝对路径**（`canonicalize()` 的 `\\?\` 前缀会被剥掉）：
        // 这一条与 Go 侧写出的形状一致，也是 Chrome 现场跑通过的那种写法。
        assert_eq!(value["path"], manifest_path_text(&path).as_str());
        #[cfg(windows)]
        assert!(
            !value["path"].as_str().unwrap().starts_with(r"\\?\"),
            "manifest path must not carry a verbatim prefix: {}",
            value["path"]
        );
        assert_eq!(
            value["allowed_origins"][0],
            format!("chrome-extension://{ID}/")
        );
        assert!(manifest(DEV_HOST, &path, "not-an-id").is_err());
        assert!(manifest("bad host", &path, ID).is_err());
        assert!(manifest(DEV_HOST, Path::new("relative"), ID).is_err());
    }

    #[test]
    fn manifest_dir_follows_the_platform_contract() {
        let home = Path::new("/home/user");
        let local = Path::new("C:/Users/user/AppData/Local");
        assert_eq!(
            default_manifest_dir("macos", home, None).unwrap(),
            home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts")
        );
        assert_eq!(
            default_manifest_dir("windows", home, Some(local)).unwrap(),
            local.join("CloudFileAgent/native-host")
        );
        assert_eq!(
            default_manifest_dir("linux", home, None).unwrap(),
            home.join(".config/google-chrome/NativeMessagingHosts")
        );
        assert!(default_manifest_dir("unknown", home, None).is_err());
        assert!(default_manifest_dir("windows", home, None).is_err());
    }

    #[test]
    fn the_registered_host_follows_the_extension_contract() {
        validate_host(HOST).unwrap();
        #[cfg(windows)]
        {
            // Windows 上扩展**只**向这个 Host 发消息，且不允许回落其它 Host
            // （cloudfile-chrome-extension/tests/agent2-routing.cjs）。名字写错就等于
            // 扩展永远报「本机 Agent 未就绪」—— 所以这里钉住，不靠注释。
            assert_eq!(HOST, "com.cloudfile.current_agent");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(HOST, "com.cloudfile.agent2");
        }
        // 自检与注册必须查同一个名字（lib.rs 的 HOST 只是转出）。
        assert_eq!(crate::HOST, HOST);
    }

    #[test]
    fn the_product_extension_id_is_a_canonical_chrome_id() {
        // Chrome 只发 [a-p]{32}；写错一个字符，注册出来的 Host 就只认一个不存在的扩展。
        assert_eq!(EXTENSION_ID.len(), 32, "{EXTENSION_ID}");
        assert!(
            EXTENSION_ID.chars().all(|c| ('a'..='p').contains(&c)),
            "{EXTENSION_ID}"
        );
        // 与 1.0 的注册脚本（`register-agent2-windows.ps1 -ExtensionId` 默认值）同一个 ID：
        // 换 ID 等于让已经装好扩展的机器全部失联，必须是一次显式决定。
        assert_eq!(EXTENSION_ID, "gocnpbhnbikcpnlafmodflddgnmfcjoc");
        // 与注册时写进 manifest 的写法一致（界面与安装都引用同一个常量）。
        assert_eq!(crate::EXTENSION_ID, EXTENSION_ID);
    }

    #[test]
    fn the_manifest_never_carries_a_windows_verbatim_prefix() {
        #[cfg(windows)]
        {
            // canonicalize() 在 Windows 上给 `\\?\D:\...`，而清单里必须是普通绝对路径
            // （与 Go 侧写的形状一致）。
            assert_eq!(
                manifest_path_text(Path::new(r"\\?\D:\apps\CloudFile\native-host\host.exe")),
                r"D:\apps\CloudFile\native-host\host.exe"
            );
            // UNC 的 verbatim 形式剥掉前缀会变成非法的 `UNC\server\share`，必须原样保留。
            assert_eq!(
                manifest_path_text(Path::new(r"\\?\UNC\server\share\host.exe")),
                r"\\?\UNC\server\share\host.exe"
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                manifest_path_text(Path::new("/opt/cloudfile/host")),
                "/opt/cloudfile/host"
            );
        }
    }

    #[test]
    fn the_registry_key_is_fixed_for_the_current_user() {
        assert_eq!(
            registry_key(DEV_HOST),
            "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.cloudfile.agent22.dev"
        );
        assert_eq!(
            registry_key(LEGACY_HOSTS[0]),
            "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.cloudfile.agent2"
        );
    }

    #[test]
    fn registry_update_is_exact_and_reversible() {
        let manifest = Path::new(BASE).join("native-host/com.cloudfile.agent22.dev.json");
        // 写：键 + 要落到默认值里的 manifest 绝对路径。
        let (key, value) = registry_update(DEV_HOST, Some(&manifest)).unwrap();
        assert_eq!(
            key,
            "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.cloudfile.agent22.dev"
        );
        assert_eq!(value.as_deref(), Some(manifest.to_string_lossy().as_ref()));
        // 删：同一个键，值为 None —— 整个键摘掉，Chrome 就再也找不到这个 Host。
        let (key, value) = registry_update(DEV_HOST, None).unwrap();
        assert_eq!(
            key,
            "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.cloudfile.agent22.dev"
        );
        assert_eq!(value, None);
        assert!(registry_update(DEV_HOST, Some(Path::new("relative.json"))).is_err());
    }

    #[test]
    fn install_is_reversible_and_guards_foreign_hosts() {
        let (_dir, base) = crate::storage::test_root();
        let manifest_dir = base.join("hosts");
        let path = binary(&base);
        let first = install(&manifest_dir, DEV_HOST, ID, &path, false).unwrap();
        assert!(!first.backed_up);
        assert!(first.manifest_path.is_file());
        assert_eq!(
            state(&manifest_dir, DEV_HOST, Some(&path))
                .unwrap()
                .extension_id
                .as_deref(),
            Some(ID)
        );

        // A foreign owner cannot be replaced silently.
        storage::write(
            &manifest_path(&manifest_dir, DEV_HOST),
            &json!({"name":DEV_HOST,"type":"stdio","path":"/opt/other/host"}),
        )
        .unwrap();
        assert!(install(&manifest_dir, DEV_HOST, ID, &path, false).is_err());
        assert!(state(&manifest_dir, DEV_HOST, Some(&path)).unwrap().foreign);

        let second = install(&manifest_dir, DEV_HOST, ID, &path, true).unwrap();
        assert!(second.backed_up);
        assert!(second.previous_manifest_path.is_file());
        rollback(&manifest_dir, DEV_HOST).unwrap();
        let restored: Value = storage::read(&manifest_path(&manifest_dir, DEV_HOST)).unwrap();
        assert_eq!(restored["path"], "/opt/other/host");
        // The restored manifest belongs to another program and stays untouched.
        assert!(unregister(&manifest_dir, DEV_HOST).is_err());
        install(&manifest_dir, DEV_HOST, ID, &path, true).unwrap();
        unregister(&manifest_dir, DEV_HOST).unwrap();
        assert!(!manifest_path(&manifest_dir, DEV_HOST).exists());
    }

    #[test]
    fn a_recognized_legacy_cloudfile_host_is_upgraded_without_takeover() {
        let (_dir, base) = crate::storage::test_root();
        let manifest_dir = base.join("hosts");
        let path = binary(&base);
        // Manifests written by an older CloudFile Agent (.NET or earlier Rust)
        // count as ours: an explicit install is the user's consent to retire it.
        storage::write(
            &manifest_path(&manifest_dir, DEV_HOST),
            &json!({
                "name": DEV_HOST,
                "type": "stdio",
                "path": if cfg!(windows) {
                    "C:/Program Files/CloudFile Agent/CloudFileAgent.exe"
                } else {
                    "/opt/CloudFileAgent/CloudFileAgent"
                },
            }),
        )
        .unwrap();
        assert!(!state(&manifest_dir, DEV_HOST, Some(&path)).unwrap().foreign);
        let outcome = install(&manifest_dir, DEV_HOST, ID, &path, false).unwrap();
        assert!(outcome.backed_up);
        let current: Value = storage::read(&manifest_path(&manifest_dir, DEV_HOST)).unwrap();
        assert_eq!(current["path"], manifest_path_text(&path).as_str());
        assert!(!state(&manifest_dir, DEV_HOST, Some(&path)).unwrap().foreign);
    }

    #[test]
    fn an_unknown_third_party_host_still_requires_takeover() {
        let (_dir, base) = crate::storage::test_root();
        let manifest_dir = base.join("hosts");
        let path = binary(&base);
        storage::write(
            &manifest_path(&manifest_dir, DEV_HOST),
            &json!({"name": DEV_HOST, "type": "stdio", "path": "/opt/other/vendor-host"}),
        )
        .unwrap();
        assert!(state(&manifest_dir, DEV_HOST, Some(&path)).unwrap().foreign);
        let error = install(&manifest_dir, DEV_HOST, ID, &path, false).unwrap_err();
        assert!(error.to_string().contains("another program"));
    }

    #[test]
    fn unregister_refuses_a_manifest_this_agent_did_not_write() {
        let (_dir, base) = crate::storage::test_root();
        storage::write(
            &manifest_path(&base, LEGACY_HOSTS[0]),
            &json!({"name":LEGACY_HOSTS[0],"type":"stdio","path":"/opt/legacy/host","description":"Legacy"}),
        )
        .unwrap();
        assert!(unregister(&base, LEGACY_HOSTS[0]).is_err());
        assert!(manifest_path(&base, LEGACY_HOSTS[0]).exists());
        // Absent hosts are a no-op, not an error.
        assert!(unregister(&base, DEV_HOST).is_ok());
        assert!(rollback(&base, DEV_HOST).is_err());
    }
}
