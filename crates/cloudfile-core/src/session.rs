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
    /// ApplicationProfile V1 的发现地址（完整 URL 或裸 origin）。
    ///
    /// 由部署方注入：不同部署把同一条 `/admin-api/local-bridge/profile/v1`
    /// 挂在不同的反向代理前缀下（例如 Etech 生产是 `/prod-api/admin-api/...`），
    /// agent 无法从服务地址里推出来。留空时界面用第一个受信任服务的 origin 兜底。
    ///
    /// 它只是**去哪个地址取应用元数据**，不是信任：能绑定哪一个文件服务仍由
    /// `allowed_origins` + `service_paths` 决定，profile 不能新增受信任服务。
    ///
    /// 2026-11-04 起多环境共存：真正使用的是下面按环境分开的 `profile_sources`，
    /// 这个单值保留给早期安装与手工填入的地址。
    #[serde(default)]
    pub profile_source: Option<String>,
    /// 按环境（`prod` / `test`）分开的应用信息地址。
    ///
    /// 一套软件、一个更新源，但测试与生产是**两套配置**：测试服多开放一些生产没有的
    /// 能力，需要能切换。两份各自缓存到本地，切换只是"选哪一份生效"，
    /// 不改变任何信任关系。键用小写环境名，值是该环境的发现地址。
    #[serde(default)]
    pub profile_sources: std::collections::BTreeMap<String, String>,
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
    /// 库名，只用于任务中心显示「这是哪个库的文件」。
    ///
    /// 来源是服务端签发的 V2 票据里的 `repo_name` claim（cloudfile-hub 写入，
    /// 由 `seafile_api.get_repo().name` 得到）。它**不参与任何判断**：能不能读、
    /// 能不能写仍然只看票据 scope 与文件服务端校验。
    ///
    /// 老任务文件没有这个键 → `None`，界面上退回显示路径首段的库号。
    #[serde(default)]
    pub library_name: Option<String>,
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
/// 删除前先确认这个文件**删得掉**（而不是"有没有人打开"）。
///
/// 踩过的坑：第一版用 `share_mode(0)` 独占打开来探测，结果**我们自己的**摘要读取
/// （std 默认按 read|write|delete 共享）也会让它失败 —— 用户明明没开文件，却报
/// "被其他程序占用"。杀毒/索引服务短时持有句柄时同样误判。
///
/// 真正决定删除成败的是「能否以 DELETE 访问打开」：Office 之类打开文件时不会给
/// FILE_SHARE_DELETE，这里就会失败；而共享删除的普通读句柄不受影响。
/// 这个 PID 现在还是当初那个程序吗？
///
/// 只比 PID 会被复用坑到（重启后 1234 可能是别的程序），所以连映像名一起比。
#[cfg(windows)]
fn process_alive(pid: u32, app: &str) -> bool {
    use std::ffi::c_void;
    const QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 258;
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn QueryFullProcessImageNameW(
            handle: *mut c_void,
            flags: u32,
            buffer: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    unsafe {
        let handle = OpenProcess(QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        if queried == 0 || !running {
            return false;
        }
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        let name = path.rsplit(['\\', '/']).next().unwrap_or("");
        !app.is_empty() && name.eq_ignore_ascii_case(app)
    }
}

#[cfg(not(windows))]
fn process_alive(_pid: u32, _app: &str) -> bool {
    false
}

fn ensure_file_released(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // 不共享的独占打开：只要有**任何**程序打开着这个文件（不管它给不给删除共享），
        // 这里就失败。自家读取由 OPEN_GUARD 排队排除，不会造成误报。
        let mut blocked = false;
        for _ in 0..3 {
            match std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
            {
                Ok(_) => {
                    blocked = false;
                    break;
                }
                // 32 = ERROR_SHARING_VIOLATION，33 = ERROR_LOCK_VIOLATION
                Err(error) if matches!(error.raw_os_error(), Some(32) | Some(33)) => {
                    // 杀毒/索引的瞬时句柄重试一次就散了；真有人开着则一直失败。
                    blocked = true;
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
                // 只读属性（本地查看副本）会给出 ACCESS_DENIED，删除本身是允许的。
                Err(_) => {
                    blocked = false;
                    break;
                }
            }
        }
        if blocked {
            bail!(
                "「{}」正在被其他程序使用，无法删除。请先关闭该文件再重试。",
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned())
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

/// 清理目录：**失败不算删除失败**。
///
/// 现场：工作副本里的文件删掉了，紧接着 `remove_dir_all` 因为目录只读属性（或别的
/// 程序短暂持有目录句柄）失败 —— 于是整个删除被报成"失败"，可文件其实已经没了，
/// 任务记录还在，任务就变成一条"本地文件不见了"的待处理项。
/// 目录残留是脏但无害的（`tasks()` 只认带 task.json 的目录），绝不能因为它毁掉整个操作。
fn remove_dir_quiet(path: &Path) {
    if !path.exists() {
        return;
    }
    if fs::remove_dir_all(path).is_ok() {
        return;
    }
    #[cfg(windows)]
    {
        // 只读属性会让 RemoveDirectory 报拒绝访问：清掉再试一次。
        // 这里就是要清掉只读**属性**（不是改 ACL），clippy 的通用告警不适用于该场景。
        #[allow(clippy::permissions_set_readonly_false)]
        if let Ok(metadata) = fs::metadata(path) {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                if fs::set_permissions(path, permissions).is_ok()
                    && fs::remove_dir_all(path).is_ok()
                {
                    return;
                }
            }
        }
    }
    let _ = fs::remove_dir_all(path);
}

pub fn remove_task(root: &Path, id: &str) -> Result<serde_json::Value> {
    remove_task_confirming(root, id, false)
}

/// `force = false` 时，若"文件已经不被任何程序占用、但我们启动的那个程序还开着"，
/// 不会直接删，而是回一个需要确认的结果 —— 这种状态没法从系统层面判断文件是否还开着，
/// 只能问用户一次。
pub fn remove_task_confirming(root: &Path, id: &str, force: bool) -> Result<serde_json::Value> {
    let task = load(root, id)?;
    let dir = task_dir(root, id)?;
    let workspace = crate::settings::workspace_root(root);
    let working = working_dir(root, id).ok();

    // 整段持写锁：探测和删除期间不让自家读取插进来，否则又会误判成"被占用"。
    let _exclusive = hold_write();

    // 1) 先说清楚"这份文件删不删得掉"。删不掉就整体拒绝，什么都不动。
    if let Some(copy) = working.as_ref() {
        ensure_file_released(&copy.join(&task.file_name))?;
    }

    // 1b) 有些 CAD 读完文件就把句柄放掉了（NX 就是这样），操作系统层面看不出占用。
    //     这时只能看"我们启动的那个程序还在不在"。
    if !force {
        if let Ok(marker) = crate::storage::read::<serde_json::Value>(&dir.join("opened.json")) {
            let pid = marker["pid"].as_u64().unwrap_or(0) as u32;
            let app = marker["app"].as_str().unwrap_or("");
            if pid != 0 && process_alive(pid, app) {
                // 句柄已经没了（上面那步通过了），但开它的程序还在跑：可能只是关了文件，
                // 也可能只是把文档关到后台。这种只能问一次，不能自己拍板。
                return Ok(serde_json::json!({
                    "ok": false,
                    "requires_confirm": true,
                    "task_id": id,
                    "file_name": task.file_name,
                    "app": app,
                    "message": format!(
                        "「{}」已经不在任何程序里打开了，但 {} 还在运行。\n                         如果你刚刚关掉了这个文件，可以放心删除；否则请先回到 {} 里确认。",
                        task.file_name, app, app
                    ),
                }));
            }
        }
    }

    // 2) 删文件本身 —— 这一步的成功与否决定整个删除的成败。
    //    只删这一份文件及其冲突副本：同目录里别人放的东西一律不动。
    let mut leftover = 0usize;
    if let Some(copy) = working.as_ref() {
        let local = copy.join(&task.file_name);
        if local.exists() {
            fs::remove_file(&local).map_err(|error| {
                if matches!(error.raw_os_error(), Some(32) | Some(33)) {
                    anyhow::anyhow!(
                        "「{}」正在被其他程序使用，无法删除。请先关闭该文件再重试。",
                        task.file_name
                    )
                } else {
                    anyhow::anyhow!("删除本地文件失败：{error}")
                }
            })?;
        }
        let conflict = copy.join(format!("{}.cloudfile-conflict", task.file_name));
        if conflict.exists() {
            let _ = fs::remove_file(&conflict);
        }
        // 目录空了才清；还有别的文件就留着，并在结果里说明。
        leftover = fs::read_dir(copy)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        if leftover == 0 && copy != &dir && copy.starts_with(&workspace) {
            remove_dir_quiet(copy);
        }
    }

    // 3) 删任务记录：task.json 是"这条任务还在"的唯一判据，必须删掉。
    let record = dir.join("task.json");
    if record.exists() {
        fs::remove_file(&record)?;
    }
    remove_dir_quiet(&dir);

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
        // 目录里还有别人的文件时不删目录，把这个数交给界面说明。
        "leftover_files": leftover,
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
/// 谁在打开本地文件、谁在删它 —— 全在这个锁里排队。
///
/// 现场两种相反的事故：独占打开探测会把**自家摘要读取**误判成"被占用"（明明没开文件
/// 也拒绝删除）；反过来只看"能否以 DELETE 打开"，又会让共享删除的程序（很多看图/CAD
/// 软件这么开）被直接删掉、用户看不到任何提示。
/// 现在以"有没有任何程序打开着"为准：探测前先拿到写锁，自家读取（持读锁）一旦结束
/// 就会让路，别人（Word/CAD/查看器）的句柄则一定还在，探测就能如实拒绝。
static OPEN_GUARD: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn hold_read() -> std::sync::RwLockReadGuard<'static, ()> {
    OPEN_GUARD.read().unwrap_or_else(|error| error.into_inner())
}

fn hold_write() -> std::sync::RwLockWriteGuard<'static, ()> {
    OPEN_GUARD
        .write()
        .unwrap_or_else(|error| error.into_inner())
}

/// 读摘要，但把"文件正被别的程序占用"当成**暂时读不到**而不是错误。
///
/// 现场：Word/CAD 打开着文件时，界面上这条任务会突然变成"待处理" —— 其实是
/// `digest` 拿不到句柄，被 `summaries` 当成"本地状态读不出来"（需要检查）。
/// 占用是暂时的、和任务本身健康无关，不能因此改变任务的状态。
pub fn digest_soft(path: &Path) -> Result<Option<String>> {
    let _reading = hold_read();
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        // 32 = ERROR_SHARING_VIOLATION，33 = ERROR_LOCK_VIOLATION
        Err(error) if matches!(error.raw_os_error(), Some(32) | Some(33)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

pub fn digest(path: &Path) -> Result<String> {
    let _reading = hold_read();
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
    let opened = crate::apps::open_path(root, &task.mode, &task.file_name, &path)?;
    // 记下"这份文件交给了哪个进程"。NX 这类程序读完就放句柄，操作系统看不出占用，
    // 只能靠这个记录在删除时提醒用户先关掉程序。
    if let Some((pid, app)) = opened {
        let marker = serde_json::json!({"pid": pid, "app": app, "at": crate::now()});
        if let Ok(dir) = task_dir(root, id) {
            let _ = crate::storage::write(&dir.join("opened.json"), &marker);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    /// 目录清理失败不能把整个删除判成失败：现场就是这样留下"文件已删、任务还在"，
    /// 那条任务随即变成待处理。
    #[cfg(windows)]
    #[test]
    fn a_leftover_directory_does_not_fail_the_deletion() {
        use std::os::windows::fs::OpenOptionsExt;

        let (_dir, root) = storage::test_root();
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let id = "e".repeat(64);
        let private = task_dir(&root, &id).unwrap();
        storage::private_dir(&private).unwrap();
        let working = prepare_working_dir(&root, &id).unwrap();
        fs::write(working.join("a.docx"), b"open me").unwrap();
        // 冲突副本被占着：目录清不掉，但主文件能删。
        fs::write(working.join("a.docx.cloudfile-conflict"), b"theirs").unwrap();
        let task = Task {
            task_id: id.clone(),
            device_id: None,
            file_name: "a.docx".into(),
            mode: "local-view".into(),
            server: "https://files.example.test".into(),
            repo_id: "11111111-1111-4111-8111-111111111111".into(),
            remote_path: "/a.docx".into(),
            library_name: None,
            session_id: "s".into(),
            baseline_file_id: "b".repeat(40),
            baseline_digest: "c".repeat(64),
            size: 7,
        };
        storage::write(&private.join("task.json"), &task).unwrap();

        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(working.join("a.docx.cloudfile-conflict"))
            .unwrap();

        remove_task(&root, &id).expect("目录清不掉也只是脏目录，不该让删除失败");
        assert!(
            !private.join("task.json").exists(),
            "任务记录必须被删掉，否则会变成一条待处理任务"
        );
        assert!(!working.join("a.docx").exists(), "主文件必须被删掉");
        drop(held);
    }

    /// 删除只清掉"这份文件"和它的冲突副本，同目录里别人的文件一律不动。
    #[test]
    fn deletion_only_removes_this_files_own_copies() {
        let (_dir, root) = storage::test_root();
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let id = "f".repeat(64);
        let private = task_dir(&root, &id).unwrap();
        storage::private_dir(&private).unwrap();
        let working = prepare_working_dir(&root, &id).unwrap();
        fs::write(working.join("a.docx"), b"mine").unwrap();
        fs::write(working.join("a.docx.cloudfile-conflict"), b"theirs").unwrap();
        fs::write(working.join("会议记录.txt"), b"not ours").unwrap();
        storage::write(
            &private.join("task.json"),
            &Task {
                task_id: id.clone(),
                device_id: None,
                file_name: "a.docx".into(),
                mode: "local-view".into(),
                server: "https://files.example.test".into(),
                repo_id: "11111111-1111-4111-8111-111111111111".into(),
                remote_path: "/a.docx".into(),
                library_name: None,
                session_id: "s".into(),
                baseline_file_id: "b".repeat(40),
                baseline_digest: "c".repeat(64),
                size: 4,
            },
        )
        .unwrap();

        let result = remove_task(&root, &id).unwrap();
        assert!(!working.join("a.docx").exists(), "自己的文件应当被删掉");
        assert!(
            !working.join("a.docx.cloudfile-conflict").exists(),
            "冲突副本也属于这份文件，应当一起删"
        );
        assert!(
            working.join("会议记录.txt").exists(),
            "同目录里的其他文件不是我们的，绝不能删"
        );
        assert_eq!(result["leftover_files"], 1, "要如实报告还有别的文件");
        assert!(working.is_dir(), "还有别人的文件时目录必须保留");
    }

    /// 文件还开在我们启动的那个程序里（NX 这类读完就放句柄的），不能删。
    #[cfg(windows)]
    #[test]
    fn a_task_opened_in_a_running_application_is_not_deleted() {
        let (_dir, root) = storage::test_root();
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let id = "a".repeat(64);
        let private = task_dir(&root, &id).unwrap();
        storage::private_dir(&private).unwrap();
        let working = prepare_working_dir(&root, &id).unwrap();
        fs::write(working.join("part.prt"), b"model").unwrap();
        storage::write(
            &private.join("task.json"),
            &Task {
                task_id: id.clone(),
                device_id: None,
                file_name: "part.prt".into(),
                mode: "local-view".into(),
                server: "https://files.example.test".into(),
                repo_id: "11111111-1111-4111-8111-111111111111".into(),
                remote_path: "/part.prt".into(),
                library_name: None,
                session_id: "s".into(),
                baseline_file_id: "b".repeat(40),
                baseline_digest: "c".repeat(64),
                size: 5,
            },
        )
        .unwrap();
        // 用"测试进程自己"冒充打开文件的程序：它当然还活着，映像名也对得上。
        let exe = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        storage::write(
            &private.join("opened.json"),
            &serde_json::json!({"pid": std::process::id(), "app": exe, "at": crate::now()}),
        )
        .unwrap();

        // 不直接删：先回一个"需要确认"（文件已不在任何程序里，但程序还开着）。
        let pending = remove_task(&root, &id).unwrap();
        assert_eq!(pending["requires_confirm"], true, "{pending}");
        assert!(working.join("part.prt").exists(), "确认前文件必须还在");
        assert!(
            private.join("task.json").is_file(),
            "确认前任务记录也要保留"
        );

        // 用户点"仍然删除"之后才真的删掉。
        let done = remove_task_confirming(&root, &id, true).unwrap();
        assert_eq!(done["ok"], true, "{done}");
        assert!(!working.join("part.prt").exists());
        assert!(!private.join("task.json").exists());
    }

    /// 文件被占用时摘要读不到，这是**暂时**状态，不是"任务坏了"。
    /// 现场：Word 开着文件，任务会自己跳进「待处理」。
    #[cfg(windows)]
    #[test]
    fn digest_soft_reports_a_busy_file_as_temporarily_unreadable() {
        use std::os::windows::fs::OpenOptionsExt;

        let (_dir, root) = storage::test_root();
        let path = root.join("a.bin");
        fs::write(&path, b"hello").unwrap();
        let free = digest_soft(&path).unwrap();
        assert!(free.is_some(), "正常文件应能算出摘要");

        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(
            digest_soft(&path).unwrap().is_none(),
            "被占用时应返回 None，而不是把任务判成异常"
        );
        drop(held);
        assert_eq!(digest_soft(&path).unwrap(), free);
    }

    /// 文件被别的程序占着时，删除必须**整体拒绝**：任务记录和本地副本都要原样保留。
    /// 现场踩过：文件删不掉、记录却被动过，任务变成"待处理"，用户以为已经删了。
    #[cfg(windows)]
    #[test]
    fn removing_a_task_refuses_while_the_copy_is_still_open() {
        use std::os::windows::fs::OpenOptionsExt;

        let (_dir, root) = storage::test_root();
        crate::settings::update(&root, |s| {
            s.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let id = "d".repeat(64);
        let private = task_dir(&root, &id).unwrap();
        storage::private_dir(&private).unwrap();
        let working = prepare_working_dir(&root, &id).unwrap();
        fs::write(working.join("a.docx"), b"open me").unwrap();
        let task = Task {
            task_id: id.clone(),
            device_id: None,
            file_name: "a.docx".into(),
            mode: "local-view".into(),
            server: "https://files.example.test".into(),
            repo_id: "11111111-1111-4111-8111-111111111111".into(),
            remote_path: "/a.docx".into(),
            library_name: None,
            session_id: "s".into(),
            baseline_file_id: "b".repeat(40),
            baseline_digest: "c".repeat(64),
            size: 7,
        };
        storage::write(&private.join("task.json"), &task).unwrap();

        // ① Word 之类：独占打开（不给共享删除）→ 必须拒绝，且什么都不动。
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(working.join("a.docx"))
            .unwrap();
        let error = remove_task(&root, &id).unwrap_err().to_string();
        assert!(error.contains("正在被其他程序使用"), "{error}");
        assert!(
            private.join("task.json").is_file(),
            "删除被拒绝时任务记录必须还在"
        );
        assert!(
            working.join("a.docx").exists(),
            "删除被拒绝时本地副本必须还在"
        );
        drop(held);

        // ② 我们自己的摘要读取不该被当成"占用"：它持的是读锁，删除会等它结束再探测。
        // 这条是现场回归：以前用户根本没开文件也报"被占用"。
        // 读锁在**本线程**持有（Guard 不能跨线程），删除交给另一个线程：
        // 它必须排队等读锁释放，而不是直接报"被占用"。
        let reading = hold_read();
        let root_copy = root.clone();
        let id_copy = id.clone();
        let started = std::time::Instant::now();
        let worker = std::thread::spawn(move || remove_task(&root_copy, &id_copy));
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(reading);
        worker
            .join()
            .expect("删除线程不应崩溃")
            .expect("自家读取不该挡住删除");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(150),
            "删除应当等自家读取结束，而不是直接误报被占用"
        );
        assert!(
            !private.join("task.json").exists(),
            "删除后任务记录应当消失"
        );
    }

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
            library_name: None,
            session_id: "session".into(),
            baseline_file_id: "b".repeat(40),
            baseline_digest: "d".repeat(64),
            size: 1,
        };
        storage::write(&task_dir(root, id).unwrap().join("task.json"), &task).unwrap();
        fs::write(working.join("a.docx"), b"x").unwrap();
        working
    }

    /// 库名（library_name）是后加的键：升级前写下的 task.json 里没有它。
    /// 老文件必须照常读出来（退回 None，界面显示路径首段的库号），
    /// 不能因为多一个展示字段就让用户手上的任务全部失效。
    #[test]
    fn task_files_written_before_the_library_name_key_still_load() {
        let (_dir, root) = storage::test_root();
        let id = "9".repeat(64);
        let dir = task_dir(&root, &id).unwrap();
        crate::storage::private_dir(&dir).unwrap();
        fs::write(
            dir.join("task.json"),
            r#"{"task_id":"9999999999999999999999999999999999999999999999999999999999999999",
                "file_name":"a.docx","mode":"local-view","server":"http://127.0.0.1:6111",
                "repo_id":"11111111-1111-4111-8111-111111111111","remote_path":"/10220942/a.docx",
                "session_id":"session","baseline_file_id":"bbbb","baseline_digest":"dddd","size":1}"#,
        )
        .unwrap();
        let task = load(&root, &id).unwrap();
        assert_eq!(task.remote_path, "/10220942/a.docx");
        assert_eq!(task.library_name, None);
        // 新写出的任务带库名，读回来要一致。
        let named = Task {
            library_name: Some("方案设计库".into()),
            ..task
        };
        storage::write(&dir.join("task.json"), &named).unwrap();
        assert_eq!(
            load(&root, &id).unwrap().library_name.as_deref(),
            Some("方案设计库")
        );
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
            library_name: None,
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
