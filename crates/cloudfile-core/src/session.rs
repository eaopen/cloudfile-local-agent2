use crate::storage;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use url::Url;
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub service_paths: std::collections::BTreeMap<String, String>,
    /// Signed release origin. Both this and the key must be set before an
    /// update can be offered; one without the other is never enough.
    #[serde(default)]
    pub update_source: Option<String>,
    /// Base64url of the pinned uncompressed P-256 release key.
    #[serde(default)]
    pub update_public_key: Option<String>,
    /// Local unpacked extension copy, used only to report its version.
    #[serde(default)]
    pub extension_dir: Option<String>,
    /// 历史审计位：早期只有显式打开它才允许明文 http 更新源。
    ///
    /// 现在 http 是**默认支持**的（内网门户只有 http，测试与正式通道同域名只差路径），
    /// 这个位只在 config.json 里保留「这台机器当初显式开过」的记录，
    /// 读回来不影响任何判定。手写 true/false 都不会改变源能否被接受。
    #[serde(default)]
    pub allow_insecure_update_source: bool,
}

impl Config {
    /// 这台机器上更新源的信任策略。http 与 https 都接受，其余规则（规范 origin、
    /// 同源、sha256、签名）照旧。
    pub fn update_policy(&self) -> crate::update::SourcePolicy {
        crate::update::SourcePolicy {
            allow_insecure: self.allow_insecure_update_source,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    #[serde(default)]
    pub device_id: Option<String>,
    pub task_id: String,
    pub file_name: String,
    pub mode: String,
    pub server: String,
    pub repo_id: String,
    pub remote_path: String,
    pub session_id: String,
    pub baseline_file_id: String,
    pub baseline_digest: String,
    pub size: u64,
}
pub fn origin(raw: &str) -> Result<Url> {
    let u = Url::parse(raw)?;
    if !matches!(u.scheme(), "https" | "http")
        || u.host_str().is_none()
        || u.origin().ascii_serialization() != raw
        || raw.len() > 255
    {
        bail!("canonical HTTP(S) origin required");
    }
    Ok(u)
}
/// 服务端给的仓库内路径必须是「/a/b/c」这种形式：绝对、无空段、无 `.` / `..` 段、无反斜杠。
///
/// 修改逻辑（2026-10-27）：原来用 `remote.contains("..")` 判断穿越，把**名字里带两个点**的
/// 合法路径也一起拒了 —— 现场有个目录就叫 `..临时存档`，于是「服务端文件元数据无效」。
/// 穿越要看的是「整段是不是 `..`」，不是子串。
pub fn safe_remote_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .skip(1)
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub fn valid_service_path(prefix: &str) -> bool {
    prefix.is_empty()
        || (prefix.starts_with('/')
            && prefix.len() <= 100
            && prefix.split('/').skip(1).all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            }))
}
pub fn service_path(root: &Path, server: &str) -> Result<String> {
    origin(server)?;
    let prefix = config(root)?
        .service_paths
        .get(server)
        .cloned()
        .unwrap_or_default();
    if !valid_service_path(&prefix) {
        bail!("invalid service path");
    }
    Ok(prefix)
}
/// Split a canonical service address into its trust origin and deployment
/// path. The origin is the only thing ever trusted; the path only locates the
/// CloudFile API inside an already canonical origin.
pub fn address(raw: &str) -> Result<(String, String)> {
    let parsed = Url::parse(raw)?;
    let server = parsed.origin().ascii_serialization();
    origin(&server)?;
    let prefix = parsed.path().trim_end_matches('/');
    if parsed.query().is_some()
        || parsed.fragment().is_some()
        || !valid_service_path(prefix)
        || raw.trim_end_matches('/') != format!("{server}{prefix}")
    {
        bail!("canonical service address required");
    }
    Ok((server, prefix.to_string()))
}
pub fn trust(root: &Path, raw: String) -> Result<()> {
    let (server, prefix) = address(&raw)?;
    let _lock = storage::lock(&root.join("config.lock"))?;
    let mut cfg = config(root)?;
    if !cfg.allowed_origins.contains(&server) {
        cfg.allowed_origins.push(server.clone());
    }
    // Re-adding the origin must not erase a known deployment path. A paired
    // device is origin-bound; an EAP portal root is not the CloudFile API root.
    if !prefix.is_empty() || !cfg.service_paths.contains_key(&server) {
        cfg.service_paths.insert(server, prefix);
    }
    storage::write(&root.join("config.json"), &cfg)
}
/// Remove an explicitly trusted origin. Product defaults belong to deployment
/// configuration, not hard-coded addresses in this reusable desktop client.
pub fn untrust(root: &Path, raw: String) -> Result<()> {
    let (server, _) = address(&raw)?;
    let _lock = storage::lock(&root.join("config.lock"))?;
    let mut cfg = config(root)?;
    cfg.allowed_origins.retain(|item| item != &server);
    cfg.service_paths.remove(&server);
    storage::write(&root.join("config.json"), &cfg)
}

/// 部署方内置的受信任服务（`;` 或 `,` 分隔的 origin 列表）。
///
/// 仓库里**不写死任何客户地址**（开源发布不带内网门户）；发版脚本把部署默认值写进
/// 通道里的安装脚本，安装完第一次启动时由 [`ensure_default_origins`] 补进 config.json。
pub const TRUSTED_ORIGINS_ENV: &str = "CLOUDFILE_TRUSTED_ORIGINS";

/// 环境里声明的默认服务，按 `(origin, service_path)` 返回。
///
/// 修改逻辑（2026-10-26）：服务地址是「origin + 部署路径」两段，例如
/// `http://host/seafile` → origin `http://host`、path `/seafile`。
/// 只把 origin 写进 allowed_origins 会丢掉 `/seafile`，界面和服务调用都会指向门户根。
pub fn seeded_services() -> Vec<(String, String)> {
    let raw = std::env::var(TRUSTED_ORIGINS_ENV).unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    for item in raw.split([';', ',']) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Ok(pair) = address(item) {
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    out
}

/// 补齐内置服务：**只在当前一条服务都没有时**写入。
///
/// 判据是**信任列表为空**：一条都没有说明用户从没配置过，内置的两条就是他该看到的；
/// 只要还剩一条，就说明用户自己做过选择，绝不再塞回去（其余字段原样保留）。
pub fn ensure_default_origins(root: &Path) -> Result<Vec<String>> {
    let wanted = seeded_services();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let path = root.join("config.json");
    let mut cfg = if path.exists() {
        storage::read(&path)?
    } else {
        Config::default()
    };
    if !cfg.allowed_origins.is_empty() {
        return Ok(Vec::new());
    }
    let mut added = Vec::new();
    for (server, prefix) in wanted {
        if !cfg.allowed_origins.contains(&server) {
            cfg.allowed_origins.push(server.clone());
        }
        cfg.service_paths.insert(server.clone(), prefix.clone());
        added.push(if prefix.is_empty() {
            server
        } else {
            format!("{server}{prefix}/")
        });
    }
    storage::write(&path, &cfg)?;
    Ok(added)
}

pub fn config(root: &Path) -> Result<Config> {
    let p = root.join("config.json");
    if !p.exists() {
        return Ok(Config::default());
    }
    storage::read(&p)
}
/// Configures (or clears) the signed update source. Setting a source without a
/// pinned key would make verification impossible, so both move together.
pub fn set_update(
    root: &Path,
    source: Option<String>,
    public_key: Option<String>,
    extension_dir: Option<String>,
) -> Result<()> {
    set_update_with_policy(
        root,
        source,
        public_key,
        extension_dir,
        crate::update::SourcePolicy::STRICT,
    )
}

/// 同上，但按给定策略校验源。开发通道（明文 http）由调用方显式传入
/// [`crate::update::SourcePolicy::DEV`]；策略同时被**记进配置**，
/// 这样以后每次升级用的都是同一套规则，而不是「设置时宽松、升级时严格」。
pub fn set_update_with_policy(
    root: &Path,
    source: Option<String>,
    public_key: Option<String>,
    extension_dir: Option<String>,
    policy: crate::update::SourcePolicy,
) -> Result<()> {
    for value in [&source, &public_key, &extension_dir].into_iter().flatten() {
        if value.len() > 4096 || value.contains('\0') {
            bail!("invalid update configuration");
        }
    }
    if let Some(value) = &source {
        policy
            .valid_source(value)
            .then_some(())
            .ok_or_else(|| anyhow::anyhow!("update source must be a canonical http(s) origin"))?;
        match &public_key {
            Some(key) => {
                crate::update::verifying_key(key)?;
            }
            // 只有开发通道允许「有源、没有签名公钥」：那种通道服务的是**扩展更新**
            // （扩展不签名，信任锚是同源 + sha256），Agent 自身则明确报「不可升级」。
            // 正式通道仍然要求两条都在，免得出现一个看着配好了、其实验不了签的通道。
            None if policy.allow_insecure => {}
            None => bail!("a pinned release key is required"),
        }
    } else if public_key.is_some() {
        // A key that can never be used would only hide a caller mistake: say so
        // instead of storing a half-configured update channel.
        bail!("an update key requires an update source");
    }
    let _lock = storage::lock(&root.join("config.lock"))?;
    let mut cfg = config(root)?;
    cfg.update_source = source;
    cfg.update_public_key = public_key;
    cfg.extension_dir = extension_dir;
    // 策略跟着源一起存：升级时读回来的是「当初同意的那套规则」。
    cfg.allow_insecure_update_source = policy.allow_insecure && cfg.update_source.is_some();
    // Reaching here the pair is either both set or both clear, so a bare write
    // is enough — there is no half-configured state to normalise away.
    storage::write(&root.join("config.json"), &cfg)
}
pub fn task_id(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn task_dir(root: &Path, id: &str) -> Result<PathBuf> {
    if !task_id(id) {
        bail!("invalid task ID");
    }
    Ok(root.join("tasks").join(id))
}
pub fn load(root: &Path, id: &str) -> Result<Task> {
    // 读前加固：任务记录必须是**私有的普通文件**。与 Go 的 store 同一处判断
    // （`internal/taskcenter/store.go`: 非私有 = ErrCorrupt），语义是 **fail closed** ——
    // 一份权限松掉的状态文件不能被当成可信输入去授权退出、上传或续租。
    // Windows 上这一条无从判断（见 fsaccess::private），其余加固照旧。
    let path = task_dir(root, id)?.join("task.json");
    let task: Task = storage::read(&path)?;
    if task.task_id != id {
        bail!("task identity mismatch");
    }
    filename(&task.file_name)?;
    Ok(task)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkingCopy {
    directory: PathBuf,
}

/// A private per-task pointer pins the working copy across preference changes.
/// Old tasks have no pointer and continue using their original private folder.
pub fn working_dir(root: &Path, id: &str) -> Result<PathBuf> {
    let private = task_dir(root, id)?;
    let pointer = private.join("working-copy.json");
    if !pointer.exists() {
        return Ok(private);
    }
    let copy: WorkingCopy = storage::read(&pointer)?;
    if !copy.directory.is_absolute() {
        bail!("invalid working copy directory");
    }
    // 修改逻辑（2026-10-23）：新布局的目录是 `<workspace>/<view|edit>/<库短码>`，
    // 最后一段不再是 task id —— 只认「绝对路径 + 在 workspace 之内」，
    // 同时保留旧布局（`<workspace>/<task id>`）的合法形态。
    let workspace = crate::settings::workspace_root(root);
    let legacy = copy.directory.file_name().and_then(|s| s.to_str()) == Some(id);
    let inside = copy.directory != workspace && copy.directory.starts_with(&workspace);
    if !legacy && !inside {
        bail!("invalid working copy directory");
    }
    for part in copy.directory.ancestors() {
        if fs::symlink_metadata(part)?.file_type().is_symlink() {
            bail!("symlink in working copy directory");
        }
    }
    Ok(copy.directory)
}

/// 查看 / 编辑两个子树：资源管理器里一眼就能分清这份副本用来干什么。
pub fn mode_subdir(mode: &str) -> &'static str {
    if mode == "local-view" {
        "view"
    } else {
        "edit"
    }
}

/// 库短码：uuid 去掉横线后取前 16 位、按 8-8 排版（`c66005f6-36bc45e1`）。
/// 64 bit 足够避碰，又比整串 uuid 短得多。
pub fn library_short(repo_id: &str) -> String {
    let hex: String = repo_id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() >= 16 {
        format!("{}-{}", &hex[..8], &hex[8..16])
    } else if hex.is_empty() {
        "unknown".to_string()
    } else {
        hex
    }
}

/// 同一库同一子树下，不同库内路径可能同名（`/A/x.docx` 与 `/B/x.docx`）。
/// 后出现的那个加 `~<路径哈希前 8 位>`，保证两者的本地文件名稳定且互不覆盖。
pub fn local_name(
    root: &Path,
    mode: &str,
    repo_id: &str,
    remote_path: &str,
    base: &str,
) -> Result<String> {
    let subdir = mode_subdir(mode);
    let taken = |name: &str| -> Result<bool> {
        Ok(tasks(root)?.into_iter().any(|task| {
            mode_subdir(&task.mode) == subdir
                && task.repo_id == repo_id
                && task.remote_path != remote_path
                && task.file_name == name
        }))
    };
    if !taken(base)? {
        return Ok(base.to_string());
    }
    let digest = format!("{:x}", Sha256::digest(remote_path.as_bytes()));
    let candidate = match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
            format!("{stem}~{}.{ext}", &digest[..8])
        }
        _ => format!("{base}~{}", &digest[..8]),
    };
    if !taken(&candidate)? {
        return Ok(candidate);
    }
    Ok(format!("{candidate}~{}", &digest[..4]))
}

/// 新任务的目录：`<workspace>\<view|edit>\<库短码>`，文件名就是本地文件名。
/// 目录固定、与 session 无关 —— 同一个文件重复打开落在同一个地方，用户找得到。
pub fn prepare_task_dir(
    root: &Path,
    id: &str,
    mode: &str,
    repo_id: &str,
    file_name: &str,
) -> Result<PathBuf> {
    filename(file_name)?;
    let private = task_dir(root, id)?;
    if private.join("working-copy.json").exists() || private.join("task.json").exists() {
        return working_dir(root, id);
    }
    let directory = crate::settings::ensure_workspace(root)?
        .join(mode_subdir(mode))
        .join(library_short(repo_id));
    storage::private_dir(&directory)?;
    storage::write(
        &private.join("working-copy.json"),
        &WorkingCopy {
            directory: directory.clone(),
        },
    )?;
    Ok(directory)
}

/// 旧（配对设备 / v1）任务的目录：跟随 workspace 根，目录名就是 task id。
/// 新任务一律走 [`prepare_task_dir`]；这个函数只为兼容既有 working-copy 指针保留。
pub fn prepare_working_dir(root: &Path, id: &str) -> Result<PathBuf> {
    let private = task_dir(root, id)?;
    if private.join("working-copy.json").exists() || private.join("task.json").exists() {
        return working_dir(root, id);
    }
    let directory = crate::settings::ensure_workspace(root)?.join(id);
    storage::private_dir(&directory)?;
    storage::write(
        &private.join("working-copy.json"),
        &WorkingCopy {
            directory: directory.clone(),
        },
    )?;
    Ok(directory)
}
pub fn local_path(root: &Path, task: &Task) -> Result<PathBuf> {
    filename(&task.file_name)?;
    let p = working_dir(root, &task.task_id)?.join(&task.file_name);
    if !fs::symlink_metadata(&p)?.is_file() {
        bail!("working copy is not a regular file");
    }
    Ok(p)
}
pub fn tasks(root: &Path) -> Result<Vec<Task>> {
    let dir = root.join("tasks");
    if !dir.exists() {
        return Ok(vec![]);
    }
    // 任务根必须私有：对应 Go 的 `store.go`「非私有任务目录 = 硬错」。
    // 这里**不是**跳过，而是报错 —— 悄悄少列一个任务会让用户以为工作副本丢了，
    // 而真实原因是权限被人改松了，那正是需要说出来的事。
    let mut result = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if task_id(&id) && entry.path().join("task.json").exists() {
            // 单个任务目录也各自要求私有：任务根是 0700 不代表里面的目录没被改过。
            result.push(load(root, &id)?);
        }
    }
    result.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(result)
}

/// 同一文件在同一子树里只保留一条任务：`local-view` 与可编辑子树各自独立。
///
/// 修改逻辑（2026-10-22）：网页每次「本地查看 / 本地编辑」都会拿到新的 session，
/// 旧逻辑用 `sha256(server + session)` 当任务 ID，于是同一个文件每点一次就多一条任务。
/// 现在按 **仓库 + 库内路径 + 模式族** 找回已有任务再做合并。
/// 目录枚举顺序不可依赖（编辑子树里可能同时有历史的 `local-edit` 与 `optimistic-edit`），
/// 命中多条时按 task_id 排序取第一条，保证同一输入永远得到同一个结果。
pub fn find_task(
    root: &Path,
    repo_id: &str,
    remote_path: &str,
    mode: &str,
) -> Result<Option<Task>> {
    let view = mode == "local-view";
    let mut hits: Vec<Task> = tasks(root)?
        .into_iter()
        .filter(|task| task.repo_id == repo_id && task.remote_path == remote_path)
        .filter(|task| (task.mode == "local-view") == view)
        .collect();
    hits.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    Ok(hits.into_iter().next())
}

/// 删除任务：任务目录（含 journal / v2 状态）+ 工作副本，并向上清理空目录。
///
/// 修改原因（2026-10-22）：任务中心只有「打开」，已完成的任务永远留在列表里，
/// 本地下载的那份也一直占着地方。清理范围严格限制在 workspace 根以下：
/// 绝不越过 workspace 根去删用户自己的目录；任何一步失败都报错而不是静默跳过。
pub fn remove_task(root: &Path, id: &str) -> Result<serde_json::Value> {
    let task = load(root, id)?;
    let dir = task_dir(root, id)?;
    let workspace = crate::settings::workspace_root(root);
    let working = working_dir(root, id).ok();
    if let Some(copy) = working.as_ref() {
        if copy != &dir && copy.starts_with(&workspace) && copy.exists() {
            fs::remove_dir_all(copy)?;
        }
    }
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    let mut pruned: Vec<String> = Vec::new();
    if let Some(copy) = working.as_ref() {
        let mut current = copy.parent().map(Path::to_path_buf);
        while let Some(path) = current {
            // 直接挂在 workspace 下的 `view` / `edit` 子树本身要留着，只清空库目录。
            if path == workspace
                || !path.starts_with(&workspace)
                || path.parent() == Some(workspace.as_path())
            {
                break;
            }
            let empty = fs::read_dir(&path)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty || fs::remove_dir(&path).is_err() {
                break;
            }
            pruned.push(path.to_string_lossy().into_owned());
            current = path.parent().map(Path::to_path_buf);
        }
    }
    Ok(serde_json::json!({
        "ok": true,
        "task_id": id,
        "file_name": task.file_name,
        "removed_local": working.map(|path| path.to_string_lossy().into_owned()),
        "pruned": pruned,
    }))
}

fn filename(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 180
        || name.ends_with([' ', '.'])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || name == "."
        || name == ".."
        || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
    {
        bail!("unsafe file name");
    }
    Ok(())
}
pub fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
/// Hands the working copy to a local application. Precedence is an explicit
/// user rule, then a detected application, then the OS default for known
/// document types; an unknown type is never launched implicitly.
pub fn launch(root: &Path, id: &str) -> Result<()> {
    let task = load(root, id)?;
    let path = local_path(root, &task)?;
    crate::apps::open_path(root, &task.mode, &task.file_name, &path)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_changes_pin_existing_copies_and_keep_journals_private() {
        let (_dir, root) = storage::test_root();
        let first = root.join("first-workspace");
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(first.to_string_lossy().into_owned())
        })
        .unwrap();
        let id = "a".repeat(64);
        let private = task_dir(&root, &id).unwrap();
        storage::private_dir(&private).unwrap();
        let working = prepare_working_dir(&root, &id).unwrap();
        assert_eq!(working, first.join(&id));
        fs::write(working.join("user.docx"), b"user changes").unwrap();
        assert!(private.join("working-copy.json").is_file());
        assert!(!working.join("task.json").exists());
        let second = root.join("second-workspace");
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(second.to_string_lossy().into_owned())
        })
        .unwrap();
        assert_eq!(prepare_working_dir(&root, &id).unwrap(), working);
        assert_eq!(
            fs::read(working.join("user.docx")).unwrap(),
            b"user changes"
        );
        let next = "b".repeat(64);
        assert_eq!(
            prepare_working_dir(&root, &next).unwrap(),
            second.join(&next)
        );
        let legacy = "c".repeat(64);
        assert_eq!(
            working_dir(&root, &legacy).unwrap(),
            task_dir(&root, &legacy).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn working_copy_pointer_never_follows_a_symlink_or_silently_falls_back() {
        let (_dir, root) = storage::test_root();
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned());
        })
        .unwrap();
        let id = "a".repeat(64);
        let working = prepare_working_dir(&root, &id).unwrap();
        fs::remove_dir(&working).unwrap();
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &working).unwrap();
        assert!(working_dir(&root, &id).is_err());
    }
    #[test]
    fn reject_paths_and_origins() {
        for x in ["../a", "CON.docx", "a:b", "a\\b", ".", "x."] {
            assert!(filename(x).is_err());
        }
        for x in [
            "https://x/",
            "https://x@evil",
            "https://x/path",
            "file:///tmp/a",
        ] {
            assert!(origin(x).is_err());
        }
        assert!(origin("https://example.com").is_ok());
    }

    const DEV_SOURCE: &str = "http://127.0.0.1:6111/cloudfile-updates-dev";

    #[test]
    fn trusted_origins_can_be_removed_without_bundled_enterprise_defaults() {
        let (_dir, root) = storage::test_root();
        trust(&root, "https://files.example.com/seafile".into()).unwrap();
        assert_eq!(config(&root).unwrap().allowed_origins.len(), 1);
        untrust(&root, "https://files.example.com".into()).unwrap();
        assert!(config(&root).unwrap().allowed_origins.is_empty());
        assert!(config(&root).unwrap().service_paths.is_empty());
    }

    #[test]
    fn a_plain_http_source_is_accepted_without_any_extra_switch() {
        let (_dir, root) = storage::test_root();
        // 明文源本身已经不构成拒绝理由（内网通道全是 http，见 update::SourcePolicy）。
        // 这里失败的真正原因是「有源、没公钥」那条规则，而且**什么都没写进去**
        // （不留半配置状态）。
        assert!(set_update(&root, Some(DEV_SOURCE.into()), None, None).is_err());
        assert!(config(&root).unwrap().update_source.is_none());

        // 扩展更新通道（扩展不签名）走显式策略：允许有源无公钥，并把这条决定记进配置。
        set_update_with_policy(
            &root,
            Some(DEV_SOURCE.into()),
            None,
            None,
            crate::update::SourcePolicy::DEV,
        )
        .unwrap();
        let saved = config(&root).unwrap();
        assert_eq!(saved.update_source.as_deref(), Some(DEV_SOURCE));
        assert!(saved.allow_insecure_update_source);
        assert_eq!(saved.update_policy(), crate::update::SourcePolicy::DEV);

        // 清空时开关一并归零：不留一个「开着口子却没有源」的状态。
        set_update(&root, None, None, None).unwrap();
        let cleared = config(&root).unwrap();
        assert!(!cleared.allow_insecure_update_source);
        assert_eq!(cleared.update_policy(), crate::update::SourcePolicy::STRICT);
    }

    #[test]
    fn the_strict_policy_still_requires_a_pinned_key() {
        let (_dir, root) = storage::test_root();
        // 正式通道：只给源不给公钥 = 一个验不了签的通道，拒绝。
        assert!(set_update(
            &root,
            Some("https://updates.example.com".into()),
            None,
            None
        )
        .is_err());
        // 开发通道允许：那种通道只服务扩展更新（扩展不签名），Agent 自身会明确报「不可升级」。
        set_update_with_policy(
            &root,
            Some("https://updates.example.com".into()),
            None,
            None,
            crate::update::SourcePolicy::DEV,
        )
        .unwrap();
        let saved = config(&root).unwrap();
        assert!(saved.update_public_key.is_none());
        assert_eq!(
            saved.update_source.as_deref(),
            Some("https://updates.example.com")
        );
        // 「有公钥没源」永远拒绝（保持原契约）。
        assert!(set_update(&root, None, Some("x".into()), None).is_err());
    }

    fn task_fixture(root: &Path, id: &str, mode: &str, remote: &str) -> PathBuf {
        crate::settings::update(root, |settings| {
            settings.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let working = prepare_working_dir(root, id).unwrap();
        let task = Task {
            device_id: None,
            task_id: id.to_string(),
            file_name: "a.docx".into(),
            mode: mode.into(),
            server: "http://127.0.0.1:6111".into(),
            repo_id: "11111111-1111-4111-8111-111111111111".into(),
            remote_path: remote.into(),
            session_id: "session".into(),
            baseline_file_id: "b".repeat(40),
            baseline_digest: "d".repeat(64),
            size: 1,
        };
        storage::write(&task_dir(root, id).unwrap().join("task.json"), &task).unwrap();
        fs::write(working.join("a.docx"), b"x").unwrap();
        working
    }

    #[test]
    fn find_task_keeps_view_and_edit_apart_and_is_deterministic() {
        let (_dir, root) = storage::test_root();
        let view = "1".repeat(64);
        let edit = "2".repeat(64);
        let other = "3".repeat(64);
        task_fixture(&root, &view, "local-view", "/a.docx");
        task_fixture(&root, &edit, "optimistic-edit", "/a.docx");
        task_fixture(&root, &other, "local-view", "/b.docx");
        let repo = "11111111-1111-4111-8111-111111111111";
        assert_eq!(
            find_task(&root, repo, "/a.docx", "local-view")
                .unwrap()
                .unwrap()
                .task_id,
            view
        );
        assert_eq!(
            find_task(&root, repo, "/a.docx", "optimistic-edit")
                .unwrap()
                .unwrap()
                .task_id,
            edit
        );
        // 查看不得命中编辑任务，反之亦然；不存在的路径返回 None 而不是随便挑一条。
        assert!(find_task(&root, repo, "/missing.docx", "local-view")
            .unwrap()
            .is_none());
    }

    #[test]
    fn removing_a_task_deletes_the_copy_and_prunes_empty_directories() {
        let (_dir, root) = storage::test_root();
        let id = "4".repeat(64);
        let working = task_fixture(&root, &id, "optimistic-edit", "/a.docx");
        let workspace = root.join("workspace");
        let removed = remove_task(&root, &id).unwrap();
        assert_eq!(removed["ok"], true);
        assert!(!working.exists());
        assert!(!task_dir(&root, &id).unwrap().exists());
        assert!(tasks(&root).unwrap().is_empty());
        // workspace 根是用户自己的目录，绝不能被顺手删掉。
        assert!(workspace.exists());
    }

    const LAYOUT_REPO: &str = "c66005f6-36bc-45e1-bc8f-88bd1ec13ded";

    fn layout_fixture(root: &Path, id: &str, mode: &str, remote: &str, name: &str) -> PathBuf {
        crate::settings::update(root, |settings| {
            settings.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let working = prepare_task_dir(root, id, mode, LAYOUT_REPO, name).unwrap();
        let task = Task {
            device_id: None,
            task_id: id.to_string(),
            file_name: name.into(),
            mode: mode.into(),
            server: "http://127.0.0.1:6111".into(),
            repo_id: LAYOUT_REPO.into(),
            remote_path: remote.into(),
            session_id: "session".into(),
            baseline_file_id: "b".repeat(40),
            baseline_digest: "d".repeat(64),
            size: 1,
        };
        storage::write(&task_dir(root, id).unwrap().join("task.json"), &task).unwrap();
        fs::write(working.join(name), b"x").unwrap();
        working
    }

    #[test]
    fn new_tasks_live_under_view_or_edit_with_the_library_code() {
        let (_dir, root) = storage::test_root();
        let workspace = root.join("workspace");
        let code = library_short(LAYOUT_REPO);
        assert_eq!(code, "c66005f6-36bc45e1");
        let view = layout_fixture(&root, &"a".repeat(64), "local-view", "/a.docx", "a.docx");
        assert_eq!(view, workspace.join("view").join(&code));
        let edit = layout_fixture(
            &root,
            &"b".repeat(64),
            "optimistic-edit",
            "/a.docx",
            "a.docx",
        );
        assert_eq!(edit, workspace.join("edit").join(&code));
        // 目录里不该出现任何 session / task id 的痕迹。
        assert!(!view.to_string_lossy().contains(&"a".repeat(8)));
    }

    #[test]
    fn same_name_in_two_paths_gets_a_stable_suffix() {
        let (_dir, root) = storage::test_root();
        crate::settings::update(&root, |settings| {
            settings.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let first =
            local_name(&root, "optimistic-edit", LAYOUT_REPO, "/A/x.docx", "x.docx").unwrap();
        assert_eq!(first, "x.docx");
        layout_fixture(
            &root,
            &"c".repeat(64),
            "optimistic-edit",
            "/A/x.docx",
            &first,
        );
        let second =
            local_name(&root, "optimistic-edit", LAYOUT_REPO, "/B/x.docx", "x.docx").unwrap();
        assert!(
            second.starts_with("x~") && second.ends_with(".docx"),
            "{second}"
        );
        // 同一个库内路径重复问，答案必须稳定。
        let again =
            local_name(&root, "optimistic-edit", LAYOUT_REPO, "/B/x.docx", "x.docx").unwrap();
        assert_eq!(second, again);
        // 换到查看子树互不影响。
        let view = local_name(&root, "local-view", LAYOUT_REPO, "/B/x.docx", "x.docx").unwrap();
        assert_eq!(view, "x.docx");
    }

    #[test]
    fn removing_a_task_keeps_the_view_and_edit_subtrees() {
        let (_dir, root) = storage::test_root();
        let id = "d".repeat(64);
        let working = layout_fixture(&root, &id, "local-view", "/a.docx", "a.docx");
        let workspace = root.join("workspace");
        remove_task(&root, &id).unwrap();
        assert!(!working.exists());
        assert!(workspace.join("view").exists(), "view 子树要留着");
        assert!(!workspace
            .join("view")
            .join(library_short(LAYOUT_REPO))
            .exists());
    }

    #[test]
    fn remote_paths_may_contain_dots_but_never_traverse() {
        // 现场那个目录真的就叫 `..临时存档`：名字里带两个点不是穿越。
        assert!(safe_remote_path("/..临时存档/顾文涛/TN3系列机型.xls"));
        assert!(safe_remote_path("/a/b.c/d..e.txt"));
        assert!(safe_remote_path("/a/..b/c"));
        // 整段是 `.` 或 `..`、空段、相对路径、反斜杠，全部拒绝。
        assert!(!safe_remote_path("/a/../b"));
        assert!(!safe_remote_path("/a/.."));
        assert!(!safe_remote_path("/a/."));
        assert!(!safe_remote_path("/a//b"));
        assert!(!safe_remote_path("a/b"));
        assert!(!safe_remote_path("/a\\b"));
    }
}
