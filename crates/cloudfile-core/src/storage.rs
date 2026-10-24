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
    let parent = path.parent().context("missing parent")?;
    private_dir(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
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
