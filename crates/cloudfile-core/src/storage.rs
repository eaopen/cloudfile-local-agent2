use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Agent 私有树的名字：`D:\CloudFileAgent`，没有 D 盘时是 `%LOCALAPPDATA%\CloudFileAgent`。
const AGENT_DIR: &str = "CloudFileAgent";
/// 旧名字（带版本后缀的那一版）。只有升级上来的机器上才有，用来找迁移源。
const LEGACY_AGENT_DIR: &str = "CloudFileAgent22";

/// Resolve an already-existing Agent2 tree without initializing, migrating or
/// writing any settings. Read-only MCP consumers must not bootstrap file state.
pub fn existing_root_readonly() -> Result<PathBuf> {
    let root = match std::env::var_os("CLOUDFILE_AGENT2_HOME") {
        Some(home) if !home.is_empty() => {
            let path = PathBuf::from(home);
            if !path.is_absolute() {
                bail!("CLOUDFILE_AGENT2_HOME must be an absolute path");
            }
            path
        }
        _ => {
            let base = dirs::data_local_dir().context("user data directory unavailable")?;
            let current = base.join(AGENT_DIR);
            let legacy = base.join(LEGACY_AGENT_DIR);
            #[cfg(windows)]
            {
                let opted_out = std::env::var_os(NO_DRIVE_PREFERENCE)
                    .is_some_and(|v| !v.is_empty());
                let preferred = preferred_root();
                if !opted_out && preferred.is_dir() && !is_empty_dir(&preferred) {
                    preferred
                } else if current.is_dir() {
                    current
                } else {
                    legacy
                }
            }
            #[cfg(not(windows))]
            {
                if current.is_dir() { current } else { legacy }
            }
        }
    };
    if !root.is_dir() {
        bail!("existing Agent2 data root unavailable");
    }
    Ok(root)
}

/// 本机私有目录（Agent 的整棵树：配置、任务、版本）。
///
/// `CLOUDFILE_AGENT2_HOME` 可以覆盖它 —— 对齐 Go 侧 `install.cmd` 与 `internal/home`：
/// 现场常把 Agent 装到别的盘，安装器与测试也需要能把整棵树搬走。
///
/// 没有显式覆盖时**Windows 上默认落 D 盘**（`D:\CloudFileAgent`）：企业机器的系统盘
/// 通常很小，而 `bin\<版本>\` 会堆好几份二进制。判据与工作目录
/// （`workspace::PREFERRED_ROOT`）共用一套：固定本地盘 + 真能写。
pub fn root() -> Result<PathBuf> {
    let path = match std::env::var_os("CLOUDFILE_AGENT2_HOME") {
        Some(home) if !home.is_empty() => {
            let path = PathBuf::from(home);
            if !path.is_absolute() {
                bail!("CLOUDFILE_AGENT2_HOME must be an absolute path");
            }
            path
        }
        _ => default_root()?,
    };
    private_dir(&path)?;
    crate::settings::initialize_workspace(
        &path,
        &dirs::home_dir().context("home directory unavailable")?,
    )?;
    Ok(path)
}

/// 不落 D 盘的开关：测试沙箱与便携部署用它把整棵树钉在给定的目录里。
///
/// `tests/cli_contract.rs` 会把 `HOME`/`LOCALAPPDATA` 重定向到临时目录来隔离注册、配置与
/// 版本目录；如果这时还去探 D 盘，测试就会写进开发机真实的 `D:\CloudFileAgent` ——
/// 那是最难查的一类污染（测试「通过」了，但真机上的配置被改掉了）。
///
/// 只有 Windows 的 `default_root()` 读它：非 Windows 分支没有 D 盘可探，留着常量会被
/// `-D warnings` 的 dead_code 判红。
#[cfg(windows)]
const NO_DRIVE_PREFERENCE: &str = "CLOUDFILE_AGENT2_NO_DRIVE_PREFERENCE";

/// 默认安装根：能落 D 盘就落 D 盘，否则回落到 `%LOCALAPPDATA%`。
///
/// `%LOCALAPPDATA%` 下可能有两棵树：新的 `CloudFileAgent` 与旧的 `CloudFileAgent22`。
/// 有 D 盘时拿**存在的那一棵**当迁移源（迁完 D 盘上叫 `CloudFileAgent`）；没有 D 盘时
/// 就地用存在的那一棵 —— 老机器不因为改名而丢掉配置，也不去动它的注册表指向。
fn default_root() -> Result<PathBuf> {
    let base = dirs::data_local_dir().context("user data directory unavailable")?;
    let local = base.join(AGENT_DIR);
    let legacy = base.join(LEGACY_AGENT_DIR);
    let existing = if local.is_dir() {
        local.clone()
    } else if legacy.is_dir() {
        legacy
    } else {
        local
    };
    #[cfg(windows)]
    let chosen = {
        let opted_out =
            std::env::var_os(NO_DRIVE_PREFERENCE).is_some_and(|value| !value.is_empty());
        if opted_out {
            existing
        } else {
            choose_root(&existing, Some(&preferred_root()))
        }
    };
    #[cfg(not(windows))]
    let chosen = existing;
    Ok(chosen)
}

/// D 盘上的首选安装根。名字与 `%LOCALAPPDATA%` 下的一致，现场一眼能对上。
#[cfg(windows)]
pub(crate) fn preferred_root() -> PathBuf {
    Path::new("D:\\").join(AGENT_DIR)
}

/// 选一个安装根（纯决策，方便测试直接给两个临时目录）。
///
/// 1. 首选盘**不可用**（没有 D 盘 / 不是固定盘 / 只读 / 写不进去）→ 留在老地方；
/// 2. 已经在首选盘上 → 用它；
/// 3. 全新机器（两边都没有）→ 首选盘；
/// 4. 只有 C 盘那棵老树（从旧版本升级上来的机器）→ **整棵树搬一次**到首选盘，
///    原目录原地留作备份；
/// 5. 搬不动 → 继续用老地方。**绝不因为换盘把用户的任务与配置弄丢。**
#[cfg(windows)]
pub(crate) fn choose_root(local: &Path, preferred: Option<&Path>) -> PathBuf {
    let Some(preferred) = preferred else {
        return local.to_path_buf();
    };
    // 「已经在首选盘上」要**有内容**才算：空目录可能只是某次探测留下的，
    // 拿它当已完成会让老机器的配置与任务留在系统盘。
    if preferred.is_dir() && !is_empty_dir(preferred) {
        return preferred.to_path_buf();
    }
    if !crate::workspace::usable(preferred) {
        return local.to_path_buf();
    }
    if !local.is_dir() {
        return preferred.to_path_buf();
    }
    match migrate(local, preferred) {
        Ok(()) => preferred.to_path_buf(),
        Err(_) => {
            // 只删得掉空目录：真拷了一半的目录留着，下次启动还能重试。
            let _ = fs::remove_dir(preferred);
            local.to_path_buf()
        }
    }
}

/// 目录空不空（不存在也算空）。用来区分「已经搬过去了」和「只是留了个空壳」。
#[cfg(windows)]
fn is_empty_dir(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true)
}

/// 整棵树搬家：先在**同一块盘**的临时名字里拷完，再改名成正式目录 ——
/// 中途失败不会留下一个「半新半旧」的安装。
#[cfg(windows)]
fn migrate(from: &Path, to: &Path) -> Result<()> {
    let name = to
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(AGENT_DIR);
    let staging = to.with_file_name(format!("{name}.migrating"));
    let _ = fs::remove_dir_all(&staging);
    copy_tree(from, &staging)?;
    // `usable()` 探测时已经把目标目录建出来了（空的），改名之前清掉。
    if to.exists() {
        fs::remove_dir(to)?;
    }
    fs::rename(&staging, to)?;
    Ok(())
}

/// 递归复制；符号链接与非常规文件一律跳过（绝不跟随到别的地方去）。
#[cfg(windows)]
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
pub fn private_dir(path: &Path) -> Result<()> {
    // Reject symlink ancestors before following paths in the private store.
    for part in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(part) {
            if meta.file_type().is_symlink() {
                bail!("symlink in private storage");
            }
        }
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    private_dir(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut tmp, value)?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
/// Atomic byte write for payloads that are already serialized (for example a
/// manifest copied verbatim during a rollback).
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    write_bytes_within(path, bytes, STATE_BUDGET)
}

/// 同 [`write_bytes`]，但可以指定等待预算。
///
/// 安装产物里的小文件（`WebView2Loader.dll`）也走这条路：它们属于「必须装上」的东西，
/// 用 [`INSTALL_BUDGET`]；而配置/journal 这类状态文件用 [`STATE_BUDGET`]，
/// 免得界面上的一次保存被卡十分钟。
pub fn write_bytes_within(path: &Path, bytes: &[u8], budget: std::time::Duration) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    private_dir(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    // 修改逻辑（2026-10-28）：`persist` 在 Windows 上是「改名替换」，而**刚写出来的文件
    // 常被杀软/索引器短暂占住**（它们打开时不带 FILE_SHARE_DELETE），改名于是拿到
    // error 5 / 32。客户机安装失败就是这个原因：暂存目录里的 exe 刚落地就被扫描，
    // 紧接着的改名一律拒绝访问。这里退避重试几次（内容已经落盘且 fsync 过，
    // 只是名字还没换上），失败才把错误抛出去。
    // 落盘之后的最后一步是**改名**（persist）：Windows 上要求目标没有打开的句柄，
    // 而刚写出来的文件正被安全软件/索引器读着是常态。等它（patient），而不是几秒后放弃。
    let mut pending = Some(tmp);
    patient("写入", path, budget, is_busy, || {
        let file = pending.take().expect("temp file already consumed");
        match file.persist(path) {
            Ok(_) => Ok(()),
            Err(error) => {
                // persist 失败会把临时文件还回来，下次重试继续用它（内容不用重写）。
                pending = Some(error.file);
                Err(error.error)
            }
        }
    })?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// 状态类文件（配置、journal、任务）的等待预算。
///
/// 这些文件很小，安全软件扫它们通常只有几十毫秒；给两分钟足够跨过任何短暂占用，
/// 又不至于让界面上的一次保存看起来像卡死。
pub const STATE_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// 安装产物（十兆级的 exe）的等待预算。
///
/// 客户机上的企业 EDR / 云查杀 / 同步盘会**持续**打开刚写出来的可执行文件
/// （云查杀、沙箱、上传审查），实测几秒到几十秒都有，而要求加白名单又推不动。
/// 用户明确接受「慢一点没关系，必须装上」，所以这里给足十分钟。
pub const INSTALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

/// 这个错误是不是「文件正被别的进程占着，等一等就会好」。
///
/// Windows：5 = ACCESS_DENIED、32 = SHARING_VIOLATION、33 = LOCK_VIOLATION。
/// 现场（企业杀软实时防护、同步盘上传审查、DLP 加密）拿到的就是这几号 ——
/// 它们**不是**权限配置问题，别去改目录 ACL，等它扫完就行。
pub fn is_busy(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
        || error.kind() == std::io::ErrorKind::PermissionDenied
}

/// 反复重试一个可能「被占用」的文件操作，直到成功或预算用完。
///
/// 只重试 [`is_busy`]（或调用方另行指定的可重试错误），其余错误立刻抛出 ——
/// 磁盘真的坏了、路径真的不存在，等十分钟也没有意义。
/// 等待期间每 5 秒往 stderr 写一行进度：`install.cmd` 会把 stderr 显示给用户，
/// 否则一小时都只看到「正在安装」，像卡死。
pub fn patient<T>(
    label: &str,
    path: &Path,
    budget: std::time::Duration,
    retryable: fn(&std::io::Error) -> bool,
    mut op: impl FnMut() -> std::io::Result<T>,
) -> Result<T> {
    let started = std::time::Instant::now();
    let mut delay = 40u64;
    let mut reported = 0u64;
    loop {
        match op() {
            Ok(value) => {
                let waited = started.elapsed().as_secs();
                if waited >= 2 {
                    eprintln!("{label}：等待 {waited}s 后完成（{}）", path.display());
                }
                return Ok(value);
            }
            Err(error) if retryable(&error) && started.elapsed() < budget => {
                let waited = started.elapsed().as_secs();
                if waited >= reported + 5 {
                    reported = waited;
                    eprintln!(
                        "{label}：文件正被其他程序占用（多为安全软件正在扫描），继续等待，已等 {waited}s：{}",
                        path.display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(delay));
                delay = (delay * 2).min(1_000);
            }
            Err(error) => {
                return Err(anyhow::Error::from(error))
                    .with_context(|| format!("{label}失败：{}", path.display()));
            }
        }
    }
}
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        bail!("invalid state file");
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("state file too large");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn lock(path: &Path) -> Result<fs::File> {
    private_dir(path.parent().context("missing lock parent")?)?;
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
        bail!("invalid lock");
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file).context("another operation is active")?;
    Ok(file)
}

/// Test-only root. The private store rejects symlinked ancestors, and the
/// platform temporary directory is itself reached through one on macOS, so the
/// canonical path is what the tests must use.
#[cfg(test)]
pub fn test_root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_machine_lands_on_the_preferred_drive() {
        let (_dir, local) = test_root();
        let local = local.join(AGENT_DIR);
        let (_other, preferred) = test_root();
        let preferred = preferred.join("D-drive");

        assert_eq!(choose_root(&local, Some(&preferred)), preferred);
        assert!(!local.exists(), "全新机器不该在系统盘留一棵空树");
    }

    #[test]
    fn an_existing_tree_is_carried_over_and_the_old_one_kept() {
        let (_dir, local) = test_root();
        let local = local.join(AGENT_DIR);
        fs::create_dir_all(local.join("bin").join("2.2.5")).unwrap();
        fs::write(local.join("config.json"), b"{\"schema\":1}").unwrap();
        fs::write(local.join("bin").join("2.2.5").join("host.exe"), b"bin").unwrap();
        let (_other, preferred) = test_root();
        let preferred = preferred.join("D-drive");

        assert_eq!(choose_root(&local, Some(&preferred)), preferred);
        // 搬过去的是整棵树：配置、版本目录都在。
        assert_eq!(
            fs::read(preferred.join("config.json")).unwrap(),
            b"{\"schema\":1}"
        );
        assert_eq!(
            fs::read(preferred.join("bin").join("2.2.5").join("host.exe")).unwrap(),
            b"bin"
        );
        // 老目录留着当备份，一个字都没删。
        assert!(local.join("config.json").is_file());
    }

    #[test]
    fn an_already_migrated_tree_is_not_copied_again() {
        let (_dir, local) = test_root();
        let local = local.join(AGENT_DIR);
        fs::create_dir_all(local.join("bin")).unwrap();
        let (_other, preferred) = test_root();
        let preferred = preferred.join("D-drive");
        fs::create_dir_all(&preferred).unwrap();
        fs::write(preferred.join("marker"), b"new").unwrap();

        assert_eq!(choose_root(&local, Some(&preferred)), preferred);
        // 已经在首选盘上：不合并、不覆盖，里面的东西原样不动。
        assert_eq!(fs::read(preferred.join("marker")).unwrap(), b"new");
        assert!(!preferred.join("bin").exists());
    }

    #[test]
    fn an_empty_preferred_directory_is_not_an_install() {
        // 探盘可能留下一个空壳目录；拿它当「已经搬过去了」会让老机器的配置与任务
        // 永远留在系统盘 —— 必须真的把树填进去。
        let (_dir, local) = test_root();
        let local = local.join(AGENT_DIR);
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("config.json"), b"{}").unwrap();
        let (_other, preferred) = test_root();
        let preferred = preferred.join("D-drive");
        fs::create_dir_all(&preferred).unwrap();

        assert_eq!(choose_root(&local, Some(&preferred)), preferred);
        assert!(
            preferred.join("config.json").is_file(),
            "空壳目录要被真正的安装内容填上"
        );
    }

    #[test]
    fn an_unusable_preferred_drive_keeps_working_from_the_old_place() {
        let (_dir, local) = test_root();
        let local = local.join(AGENT_DIR);
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("config.json"), b"{}").unwrap();
        // 不存在的盘符：GetDriveTypeW 不是 DRIVE_FIXED，usable() 会拒。
        let preferred = PathBuf::from(r"Z:\CloudFileAgent");

        assert_eq!(choose_root(&local, Some(&preferred)), local);
        assert!(local.join("config.json").is_file());
    }

    #[test]
    fn no_preferred_drive_means_the_local_tree() {
        let (_dir, local) = test_root();
        assert_eq!(choose_root(&local, None), local);
    }
}
