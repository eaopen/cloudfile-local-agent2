//! Optional generic ApplicationProfile V1 consumer.
//! No dependency on a Local Bridge process, EAP-specific routing or credentials.
//! Downloading metadata must never mutate trusted CloudFile origins or active tasks.
use crate::{session, storage};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use url::Url;

pub const DISCOVERY_PATH: &str = "/admin-api/local-bridge/profile/v1";
const MAX_PROFILE_BYTES: u64 = 65536;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProfile {
    pub schema_version: u32,
    pub profile_id: String,
    pub provider: String,
    pub environment: String,
    pub revision: u64,
    pub expires_at: u64,
    pub application: ApplicationBrand,
    pub origin: String,
    #[serde(default)]
    pub services: BTreeMap<String, String>,
    #[serde(default)]
    pub channels: Vec<ProfileChannel>,
    #[serde(default)]
    pub update: Option<ProfileUpdate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationBrand {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub logo_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChannel {
    pub id: String,
    pub title: String,
    pub kind: ChannelKind,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default = "true_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub optional: bool,
}

fn true_by_default() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Web,
    Native,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileUpdate {
    pub track: String,
    pub source_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundProfile {
    pub source_origin: String,
    pub fetched_at: u64,
    pub profile: ApplicationProfile,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileLabel {
    pub profile_id: String,
    pub application_name: String,
    pub environment: String,
    pub file_server: String,
    pub expires_at: u64,
    pub stale: bool,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn endpoint(raw: &str) -> Result<Url> {
    if raw.len() > 2048 {
        bail!("profile endpoint too long");
    }
    let uri = Url::parse(raw)?;
    if !matches!(uri.scheme(), "http" | "https")
        || uri.host_str().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
        || uri.query().is_some()
    {
        bail!("invalid profile service URL");
    }
    Ok(uri)
}

fn web_entry(raw: &str) -> bool {
    raw.len() <= 256
        && raw.starts_with('/')
        && !raw.starts_with("//")
        && !raw.contains("..")
        && raw
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_'))
}

pub fn validate(profile: &ApplicationProfile, now: u64) -> Result<()> {
    if profile.schema_version != 1
        || !identifier(&profile.profile_id)
        || !identifier(&profile.provider)
        || !identifier(&profile.environment)
        || profile.revision == 0
        || profile.expires_at <= now
        || profile.expires_at > now.saturating_add(7 * 86400)
        || session::origin(&profile.origin).is_err()
    {
        bail!("invalid, stale or unsupported ApplicationProfile V1");
    }
    if !identifier(&profile.application.id)
        || profile.application.name.is_empty()
        || profile.application.name.chars().count() > 100
        || profile.application.name.chars().any(char::is_control)
    {
        bail!("invalid ApplicationProfile brand");
    }
    if let Some(logo) = profile.application.logo_url.as_deref() {
        let parsed = endpoint(logo)?;
        if parsed.scheme() == "http" && parsed.origin().ascii_serialization() != profile.origin {
            bail!("HTTP logo must be hosted on the application origin");
        }
    }
    if profile.services.len() > 16 || profile.channels.len() > 32 {
        bail!("too many ApplicationProfile services or channels");
    }
    for (name, url) in &profile.services {
        if !identifier(name) {
            bail!("invalid ApplicationProfile service name");
        }
        endpoint(url)?;
    }
    let mut seen = BTreeSet::new();
    for channel in &profile.channels {
        if !identifier(&channel.id)
            || !seen.insert(&channel.id)
            || channel.title.is_empty()
            || channel.title.chars().count() > 80
            || channel.title.chars().any(char::is_control)
            || channel.capabilities.len() > 16
            || !channel.capabilities.iter().all(|c| identifier(c))
        {
            bail!("invalid ApplicationProfile channel");
        }
        match channel.kind {
            ChannelKind::Native
                if channel.entry.is_some()
                    || !channel.module.as_deref().is_some_and(identifier) =>
            {
                bail!("native channel requires a capability module ID, not a command");
            }
            ChannelKind::Web
                if channel.module.is_some() || !channel.entry.as_deref().is_some_and(web_entry) =>
            {
                bail!("web channel requires an origin-relative path");
            }
            _ => {}
        }
    }
    if profile
        .update
        .as_ref()
        .is_some_and(|update| !identifier(&update.track) || !identifier(&update.source_id))
    {
        bail!("invalid ApplicationProfile update label");
    }
    Ok(())
}

fn profile_path(root: &Path, id: &str) -> Result<std::path::PathBuf> {
    if !identifier(id) {
        bail!("invalid ApplicationProfile ID");
    }
    Ok(root.join("application-profiles").join(format!("{id}.json")))
}

/// A profile may label already trusted files but MUST NOT enroll a new CloudFile
/// service, replace update keys, add OAuth cookies, or authorize a write.
fn check_bound_file_service(root: &Path, profile: &ApplicationProfile) -> Result<String> {
    if !profile.channels.iter().any(|c| {
        c.enabled
            && c.kind == ChannelKind::Native
            && c.module.as_deref() == Some("cloudfile")
            && c.capabilities.iter().any(|id| id == "cloudfile.file.view")
    }) {
        bail!("profile does not declare a CloudFile file capability");
    }
    let address = profile
        .services
        .get("files")
        .context("profile has no CloudFile service")?;
    let (server, path) = session::address(address)?;
    let cfg = session::config(root)?;
    if !cfg.allowed_origins.iter().any(|known| known == &server) {
        bail!("CloudFile service is not already trusted by this Agent");
    }
    if cfg
        .service_paths
        .get(&server)
        .map(String::as_str)
        .unwrap_or("")
        != path
    {
        bail!("profile CloudFile path differs from the already trusted service");
    }
    Ok(server)
}

/// Both a bare application origin and the actual public discovery URL work.
/// Etech Web and similar reverse proxies publish their EAP API under a prefix
/// (for example /prod-api/); that prefix is transport routing, never part of
/// profile_id or the trusted ApplicationProfile origin.
fn discovery_target(address: &str) -> Result<(String, String)> {
    if session::origin(address).is_ok() {
        return Ok((address.to_string(), format!("{address}{DISCOVERY_PATH}")));
    }
    let parsed = endpoint(address)?;
    let origin = parsed.origin().ascii_serialization();
    session::origin(&origin)?;
    let prefix = parsed
        .path()
        .strip_suffix(DISCOVERY_PATH)
        .context("expected the exact ApplicationProfile discovery path")?;
    if !session::valid_service_path(prefix)
        || address != format!("{origin}{prefix}{DISCOVERY_PATH}")
    {
        bail!("non-canonical ApplicationProfile discovery URL");
    }
    Ok((origin, address.to_string()))
}

pub fn sync(root: &Path, address: &str) -> Result<Value> {
    // User-initiated CLI only, no cookies or browser-supplied Host/EXE fields.
    // Public reverse proxies may expose the fixed endpoint below a prefix.
    let (app_origin, url) = discovery_target(address)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(&url)
        .send()
        .context("Profile discovery unavailable")?;
    if response.status() != reqwest::StatusCode::OK {
        bail!("Profile discovery failed (HTTP {})", response.status());
    }
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|x| x.to_str().ok())
        .is_some_and(|x| x.starts_with("application/json"))
    {
        bail!("Profile endpoint did not return JSON");
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_PROFILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROFILE_BYTES {
        bail!("Profile response is too large");
    }
    let profile: ApplicationProfile = serde_json::from_slice(&bytes)?;
    validate(&profile, crate::now())?;
    if profile.origin != app_origin {
        bail!("Profile origin does not match the explicitly selected application");
    }
    let file_server = check_bound_file_service(root, &profile)?;
    let path = profile_path(root, &profile.profile_id)?;
    let dir = path.parent().context("invalid profile directory")?;
    storage::private_dir(dir)?;
    let _lock = storage::lock(&root.join("application-profiles.lock"))?;
    if path.is_file() {
        let old: BoundProfile = storage::read(&path)?;
        if old.source_origin != app_origin
            || old.profile.provider != profile.provider
            || old.profile.environment != profile.environment
        {
            bail!("profile identity belongs to a different app origin/environment");
        }
        if profile.revision < old.profile.revision {
            bail!("profile revision rolled back");
        }
    }
    let result = json!({
        "profile_id": profile.profile_id,
        "environment": profile.environment,
        "application_name": profile.application.name,
        "file_server": file_server,
        "expires_at": profile.expires_at,
        "bound_to_existing_cloudfile": true,
        "file_permissions_changed": false
    });
    storage::write(
        &path,
        &BoundProfile {
            source_origin: app_origin,
            fetched_at: crate::now(),
            profile,
        },
    )?;
    Ok(result)
}

/// Local-only view; corrupt metadata is skipped and never blocks file tasks.
pub fn labels(root: &Path) -> Vec<ProfileLabel> {
    let dir = root.join("application-profiles");
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return result;
    };
    for entry in entries.flatten().take(32) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json")
            || !path.is_file()
            || !matches!(std::fs::metadata(&path), Ok(meta) if meta.len() <= MAX_PROFILE_BYTES)
        {
            continue;
        }
        let Ok(record) = storage::read::<BoundProfile>(&path) else {
            continue;
        };
        if record.profile.origin != record.source_origin {
            continue;
        }
        let now = crate::now();
        // Expired metadata may still label an existing task, but never grants
        // new capabilities or permission to refresh authorization.
        if validate(
            &record.profile,
            now.min(record.profile.expires_at.saturating_sub(1)),
        )
        .is_err()
        {
            continue;
        }
        if let Ok(file_server) = check_bound_file_service(root, &record.profile) {
            result.push(ProfileLabel {
                profile_id: record.profile.profile_id,
                application_name: record.profile.application.name,
                environment: record.profile.environment,
                file_server,
                expires_at: record.profile.expires_at,
                stale: now >= record.profile.expires_at,
            });
        }
    }
    result.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    result
}

pub fn summaries(root: &Path) -> Value {
    json!(labels(root)
        .iter()
        .map(|p| json!({
            "profile_id":p.profile_id,
            "application_name":p.application_name,
            "environment":p.environment,
            "expires_at":p.expires_at,
            "stale":p.stale,
        }))
        .collect::<Vec<_>>())
}

/// 自动刷新的默认间隔：一天。品牌这类元数据没必要更勤，服务端也没有变更通知。
pub const REFRESH_INTERVAL: u64 = 24 * 60 * 60;

/// 要拉取的应用信息地址。
///
/// 一台机器上同时存在测试与生产两套配置：**软件只有一个、更新源也只有一个**，
/// 但两套配置各自独立缓存，由「使用测试服」开关决定当前生效哪一套。
/// 所以这里返回的是**所有已知来源**，逐个拉（失败互不影响），各自落到自己的缓存文件里。
///
/// 顺序（先精确后兜底）：
/// 1. `config.profile_sources`（部署/上次成功时记下的环境→地址）；
/// 2. 旧的单值 `config.profile_source`（兼容早期安装）；
/// 3. 从已信任服务推导：`<origin><DISCOVERY_PATH>` 与 `<origin>/prod-api<DISCOVERY_PATH>`。
///    Etech 两套服务的 `/admin-api` 裸路径都会被 nginx 回成前端 HTML，只有带
///    `/prod-api` 的才是真接口 —— 两个都试，谁先给出 JSON 谁算数。
pub fn sources(config: &session::Config) -> Vec<String> {
    let mut result: Vec<String> = config
        .profile_sources
        .values()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if let Some(single) = config
        .profile_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        result.push(single.to_string());
    }
    // 推导出来的候选**总是**补在最后，不能因为"已经记住过一个环境"就停掉。
    // 现场踩过：先成功拉到测试服，`profile_sources` 只剩 `test`，于是生产那条
    // 再也不会被尝试 —— 生产环境永远没有缓存，界面只能退回默认名。
    for origin in &config.allowed_origins {
        result.push(format!("{origin}{DISCOVERY_PATH}"));
        result.push(format!("{origin}/prod-api{DISCOVERY_PATH}"));
    }
    let mut seen = BTreeSet::new();
    result.retain(|value| seen.insert(value.clone()));
    result.truncate(8);
    result
}

/// 界面该显示的名字。
///
/// 顺序：当前环境的配置 → **任意**已绑定配置 → `CloudFile`。
/// 中间这层是必要的：开关停在"生产"、但本机只缓存了测试配置时（某一侧拉取失败很常见），
/// 退回 `CloudFile` 会让用户以为"品牌没生效"，其实站点名字是知道的。
pub fn display_name(root: &Path, use_test: bool) -> String {
    if let Some(profile) = active(root, use_test) {
        if !profile.application_name.trim().is_empty() {
            return profile.application_name;
        }
    }
    let Ok(entries) = std::fs::read_dir(root.join("application-profiles")) else {
        return "CloudFile".to_string();
    };
    for entry in entries.flatten().take(32) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(record) = storage::read::<BoundProfile>(&path) else {
            continue;
        };
        let name = record.profile.application.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    "CloudFile".to_string()
}

/// 当前生效的那一套配置（由「使用测试服」开关选）。
///
/// 两套配置各存一份文件（`profile_id` 不同），这里按 `environment` 挑；
/// 挑不到就返回 None —— 界面退回默认品牌，绝不因为某个环境没拉到就不让用。
#[derive(Clone, Debug, Serialize)]
pub struct ActiveProfile {
    pub profile_id: String,
    pub application_name: String,
    pub environment: String,
    pub origin: String,
    pub stale: bool,
    /// 该环境下开放的能力模块（native channel 且 enabled）。
    /// 这是**功能可见性**的依据，不涉及文件授权 —— 能不能读写在服务端票据里。
    pub features: Vec<String>,
    /// 该环境声明的 Web 入口（如元枢），供界面按环境显隐。
    pub web_entries: Vec<String>,
}

pub fn active(root: &Path, use_test: bool) -> Option<ActiveProfile> {
    let wanted = if use_test { "test" } else { "prod" };
    let mut startable = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("application-profiles")) else {
        return None;
    };
    for entry in entries.flatten().take(32) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json")
            || !matches!(std::fs::metadata(&path), Ok(meta) if meta.len() <= MAX_PROFILE_BYTES)
        {
            continue;
        }
        let Ok(record) = storage::read::<BoundProfile>(&path) else {
            continue;
        };
        if record.profile.origin != record.source_origin
            || record.profile.environment != wanted
            || check_bound_file_service(root, &record.profile).is_err()
        {
            continue;
        }
        let features = record
            .profile
            .channels
            .iter()
            .filter(|channel| channel.kind == ChannelKind::Native && channel.enabled)
            .filter_map(|channel| channel.module.clone())
            .collect();
        let web_entries = record
            .profile
            .channels
            .iter()
            .filter(|channel| channel.kind == ChannelKind::Web && channel.enabled)
            .filter_map(|channel| channel.entry.clone())
            .collect();
        startable.push(ActiveProfile {
            profile_id: record.profile.profile_id.clone(),
            application_name: record.profile.application.name.clone(),
            environment: record.profile.environment.clone(),
            origin: record.profile.origin.clone(),
            stale: crate::now() >= record.profile.expires_at,
            features,
            web_entries,
        });
    }
    // 同一环境绑定多份时按 profile_id 取第一个，保证结果稳定。
    startable.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    startable.into_iter().next()
}

/// 这次该拉哪些地址（空 = 不用拉）。
///
/// 触发条件（满足任意一条就拉**全部**来源，两套配置一起保鲜）：
/// - 程序版本与上次成功拉取的版本不同（升级后立刻拿最新配置）；
/// - 当前生效环境没有缓存，或已过期；
/// - 距离上次成功同步超过 `interval`。
pub fn refresh_due(root: &Path, interval: u64, version: &str, use_test: bool) -> Vec<String> {
    let Ok(config) = session::config(root) else {
        return Vec::new();
    };
    let targets = sources(&config);
    if targets.is_empty() {
        return Vec::new();
    }
    let settings = crate::settings::load(root).unwrap_or_default();
    let now = crate::now();
    let snapshots = bound_snapshots(root);
    let last_fetch = snapshots.iter().map(|(at, _)| *at).max().unwrap_or(0);
    let urgent_version = settings.profile_pull_version.as_deref() != Some(version);
    let active_missing = active(root, use_test).is_none();
    let active_expired = snapshots.iter().any(|(_, expires)| now >= *expires);
    let aged = now.saturating_sub(last_fetch) >= interval;
    if urgent_version || active_missing || active_expired || aged {
        targets
    } else {
        Vec::new()
    }
}

/// 本地已绑定记录的（同步时刻、有效期）。坏文件直接跳过 —— 刷新判据不值得为它失败。
fn bound_snapshots(root: &Path) -> Vec<(u64, u64)> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("application-profiles")) else {
        return result;
    };
    for entry in entries.flatten().take(32) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json")
            || !matches!(std::fs::metadata(&path), Ok(meta) if meta.len() <= MAX_PROFILE_BYTES)
        {
            continue;
        }
        if let Ok(record) = storage::read::<BoundProfile>(&path) {
            if record.profile.origin == record.source_origin {
                result.push((record.fetched_at, record.profile.expires_at));
            }
        }
    }
    result
}

/// 当前环境对应的默认发现地址。
///
/// 优先用上次成功记下的「环境 → 地址」；没有就退回第一个候选来源。
/// `use_test` 只决定"优先哪一个"，不影响信任关系。
///
/// 为什么需要可配置：同一条 `/admin-api/local-bridge/profile/v1` 在不同部署下挂在
/// 不同的反代前缀后面（Etech 两套服务都只有 `/prod-api/...` 是真接口），推不出来。
pub fn default_address(config: &session::Config, use_test: bool) -> Option<String> {
    let environment = if use_test { "test" } else { "prod" };
    if let Some(known) = config
        .profile_sources
        .get(environment)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Some(known.to_string());
    }
    if let Some(raw) = config
        .profile_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(raw.to_string());
    }
    sources(config).into_iter().next()
}

/// 设置页的「应用信息」：已绑定的 profile、默认发现地址、以及侧栏该显示的名字。
///
/// 只读投影 + 一个地址，没有任何授权含义：绑定成功与否由 [`sync`] 判定，
/// 这里的每一项都只是把本地已存的东西摊开展示。
pub fn overview(root: &Path, use_test: bool) -> Value {
    let labels = labels(root);
    // 配置读不出来（损坏/权限）时不要在这里报错：任务中心本来还能用，
    // 应用信息只是展示，退回「没有品牌、没有默认地址」即可。
    let config = session::config(root).ok();
    // 当前生效的那一套（按「使用测试服」开关挑）。品牌与功能可见性都以它为准。
    let current = active(root, use_test);
    // 兜底：开关选中的环境没有缓存时，退到「这个文件服务恰好对应一个 profile」的
    // 那个品牌，而不是直接掉回 CloudFile —— 至少别让已经拉到的名字白白浪费。
    let fallback = config.as_ref().and_then(|config| {
        config
            .allowed_origins
            .iter()
            .find_map(|server| label_for_file_server(&labels, server))
    });
    let brand = match (&current, fallback) {
        (Some(current), _) => json!({
            "application_name": current.application_name,
            "profile_id": current.profile_id,
            "environment": current.environment,
            "stale": current.stale,
        }),
        (None, Some(label)) => json!({
            "application_name": label.application_name,
            "profile_id": label.profile_id,
            "environment": label.environment,
            "stale": label.stale,
        }),
        (None, None) => Value::Null,
    };
    json!({
        "source": config.as_ref().and_then(|config| default_address(config, use_test)),
        "configured_source": config.as_ref().and_then(|c| c.profile_source.clone()),
        "use_test": use_test,
        "brand": brand,
        // 当前环境开放的能力模块与 Web 入口：界面据此显隐「只在某个环境有」的功能。
        "active": current.map(|profile| json!({
            "profile_id": profile.profile_id,
            "environment": profile.environment,
            "origin": profile.origin,
            "stale": profile.stale,
            "features": profile.features,
            "web_entries": profile.web_entries,
        })),
        "profiles": summaries(root),
    })
}

/// 记下本次成功同步用的地址。校验与同步是同一套规则：地址不合法时
/// [`sync`] 已经拒绝了，这里只存**已经用过且成功**的那一份。
/// 记下某个环境成功用过的地址：下次切换/刷新直接用它，不用再逐个候选去试。
pub fn remember_source(root: &Path, address: &str, environment: &str) -> Result<()> {
    let (_, canonical) = discovery_target(address)?;
    let key = environment.trim().to_ascii_lowercase();
    if key.is_empty() {
        return Ok(());
    }
    let _lock = storage::lock(&root.join("config.lock"))?;
    let mut config = session::config(root)?;
    if config.profile_sources.get(&key) == Some(&canonical) {
        return Ok(());
    }
    config.profile_sources.insert(key, canonical);
    storage::write(&root.join("config.json"), &config)
}

/// Only use a brand when a CloudFile origin resolves to *one* application
/// instance. Ambiguous mappings keep the existing CloudFile service display.
pub fn label_for_file_server<'a>(
    profiles: &'a [ProfileLabel],
    server: &str,
) -> Option<&'a ProfileLabel> {
    let mut hits = profiles
        .iter()
        .filter(|profile| profile.file_server == server);
    match (hits.next(), hits.next()) {
        (Some(first), None) => Some(first),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;

    fn sample() -> ApplicationProfile {
        serde_json::from_value(json!({
            "schema_version": 1, "profile_id": "sample-dev", "provider": "sample",
            "environment": "dev", "revision": 1,
            "expires_at": crate::now() + 3600,
            "application": {"id": "sample", "name": "Sample"},
            "origin": "http://127.0.0.1:48080",
            "services": {"files": "https://files.example.test/seafile"},
            "channels": [{"id": "files", "title": "Files", "kind": "native",
                "module": "cloudfile", "capabilities": ["cloudfile.file.view"]}],
            "update": {"track": "dev", "source_id": "trusted-dev"}
        }))
        .unwrap()
    }

    #[test]
    fn validates_bridge_v1_and_blocks_unknown_commands() {
        let mut p = sample();
        validate(&p, crate::now()).unwrap();
        p.channels[0].module = Some("cmd.exe /c".into());
        assert!(validate(&p, crate::now()).is_err());
        p.channels[0].module = Some("cloudfile".into());
        p.origin = "http://other.example".into();
        p.application.logo_url = Some("http://not-origin.example/logo".into());
        assert!(validate(&p, crate::now()).is_err());
    }

    #[test]
    fn binds_only_to_an_existing_trusted_cloudfile_service() {
        let (_temporary, root) = storage::test_root();
        let p = sample();
        assert!(check_bound_file_service(&root, &p).is_err());
        session::trust(&root, "https://files.example.test/seafile".into()).unwrap();
        assert_eq!(
            check_bound_file_service(&root, &p).unwrap(),
            "https://files.example.test"
        );
        let (_unbound, unbound_root) = storage::test_root();
        session::trust(&unbound_root, "https://files.example.test".into()).unwrap();
        // An origin-only enrollment cannot authorize a distinct /seafile deployment.
        let profile_with_subpath = sample();
        assert!(check_bound_file_service(&unbound_root, &profile_with_subpath).is_err());
        let mut changed = p;
        changed
            .services
            .insert("files".into(), "https://new.example.test/seafile".into());
        assert!(check_bound_file_service(&root, &changed).is_err());
    }

    #[test]
    fn discovery_accepts_original_origin_and_actual_reverse_proxy_endpoint() {
        assert_eq!(
            discovery_target("https://portal.example.test").unwrap(),
            (
                "https://portal.example.test".to_string(),
                "https://portal.example.test/admin-api/local-bridge/profile/v1".to_string()
            )
        );
        assert_eq!(
            discovery_target(
                "http://portal.example.test/prod-api/admin-api/local-bridge/profile/v1"
            )
            .unwrap(),
            (
                "http://portal.example.test".to_string(),
                "http://portal.example.test/prod-api/admin-api/local-bridge/profile/v1".to_string()
            )
        );
        assert_eq!(
            discovery_target("http://localhost:8080/proxy-api/admin-api/local-bridge/profile/v1")
                .unwrap()
                .0,
            "http://localhost:8080"
        );
        for bad in [
            "https://portal.example.test/prod-api",
            "https://portal.example.test/prod-api/admin-api/local-bridge/profile/v2",
            "https://portal.example.test/prod-api/admin-api/local-bridge/profile/v1?redirect=1",
            "https://portal.example.test/prod-api/admin-api/local-bridge/profile/v1#frag",
            "https://user@portal.example.test/prod-api/admin-api/local-bridge/profile/v1",
            "https://portal.example.test/x/../prod-api/admin-api/local-bridge/profile/v1",
            "file:///tmp/admin-api/local-bridge/profile/v1",
        ] {
            assert!(discovery_target(bad).is_err(), "{bad}");
        }
    }

    /// 地址来源的优先级：按环境记下的地址 → 旧的单值 → 从受信任服务推导。
    /// 推导时会同时试裸路径和 `/prod-api` 前缀（Etech 两套服务只有后者是真接口）。
    #[test]
    fn sources_and_default_address_follow_the_configured_environment() {
        let (_temporary, root) = storage::test_root();
        // 一个服务都没信任：没有地址，界面应提示先完成安装引导。
        assert!(sources(&session::config(&root).unwrap()).is_empty());
        assert_eq!(
            default_address(&session::config(&root).unwrap(), false),
            None
        );

        session::trust(&root, "https://files.example.test/seafile".to_string()).unwrap();
        let derived = sources(&session::config(&root).unwrap());
        assert_eq!(
            derived,
            vec![
                "https://files.example.test/admin-api/local-bridge/profile/v1".to_string(),
                "https://files.example.test/prod-api/admin-api/local-bridge/profile/v1".to_string(),
            ]
        );
        // 推导顺序决定默认值：先裸路径，失败会被内容类型校验挡住，回落到 /prod-api。
        assert_eq!(
            default_address(&session::config(&root).unwrap(), false).unwrap(),
            derived[0]
        );

        // 按环境记下的地址优先，且 prod / test 各用各的。
        let mut config = session::config(&root).unwrap();
        config.profile_sources.insert(
            "prod".into(),
            "http://prod.example.test/prod-api/admin-api/local-bridge/profile/v1".into(),
        );
        config.profile_sources.insert(
            "test".into(),
            "http://test.example.test/prod-api/admin-api/local-bridge/profile/v1".into(),
        );
        assert_eq!(
            default_address(&config, false).unwrap(),
            "http://prod.example.test/prod-api/admin-api/local-bridge/profile/v1"
        );
        assert_eq!(
            default_address(&config, true).unwrap(),
            "http://test.example.test/prod-api/admin-api/local-bridge/profile/v1"
        );

        // 旧的单值仍然兼容（早期安装只填过一个地址）。
        let mut legacy = session::config(&root).unwrap();
        legacy.profile_source = Some("   ".into());
        assert_eq!(
            default_address(&legacy, false).unwrap(),
            derived[0],
            "空串等于没配"
        );
    }

    /// 没有绑定过 profile 时，设置页要说「尚未同步」而不是报错；
    /// 侧栏也必须保持 CloudFile 默认名。
    #[test]
    fn overview_is_empty_but_successful_before_any_profile_is_bound() {
        let (_temporary, root) = storage::test_root();
        let empty = overview(&root, false);
        assert_eq!(empty["profiles"].as_array().unwrap().len(), 0);
        assert!(empty["brand"].is_null());
        assert!(empty["active"].is_null());
        assert_eq!(empty["use_test"], json!(false));

        session::trust(&root, "https://files.example.test/seafile".to_string()).unwrap();
        let with_service = overview(&root, true);
        assert_eq!(with_service["use_test"], json!(true));
        assert!(with_service["source"].as_str().is_some());
        assert!(with_service["brand"].is_null());
    }

    /// 记住的必须是与同步时相同的那条规范地址：写成 origin 会让下次同步
    /// 打到不带前缀的路径上（正好是漏反代前缀的经典错误）。
    #[test]
    fn a_successful_sync_remembers_the_canonical_address_only() {
        let (_temporary, root) = storage::test_root();
        session::trust(&root, "https://files.example.test/seafile".into()).unwrap();
        assert!(remember_source(&root, "https://files.example.test/prod-api", "prod").is_err());

        remember_source(
            &root,
            "https://files.example.test/prod-api/admin-api/local-bridge/profile/v1",
            "prod",
        )
        .unwrap();
        assert_eq!(
            session::config(&root)
                .unwrap()
                .profile_sources
                .get("prod")
                .cloned()
                .unwrap(),
            "https://files.example.test/prod-api/admin-api/local-bridge/profile/v1"
        );
        // 再记一次同样的地址是幂等的。
        remember_source(
            &root,
            "https://files.example.test/prod-api/admin-api/local-bridge/profile/v1",
            "prod",
        )
        .unwrap();
    }

    /// 品牌兜底：当前环境没有配置时，不能直接掉回 CloudFile —— 站点名字明明已经缓存过。
    #[test]
    fn display_name_falls_back_to_any_bound_profile_before_the_default() {
        let (_temporary, root) = storage::test_root();
        assert_eq!(display_name(&root, false), "CloudFile");

        session::trust(&root, "https://files.example.test/seafile".into()).unwrap();
        let now = crate::now();
        // 只绑了测试那一套，但开关停在生产。
        store_bound(&root, "etech-test", "test", now, now + 86400);
        assert_eq!(display_name(&root, false), "Sample");
        // 当前环境有配置时优先用它。
        store_bound(&root, "etech-prod", "prod", now, now + 86400);
        assert_eq!(display_name(&root, true), "Sample");
        assert_eq!(display_name(&root, false), "Sample");
    }

    /// 极简 HTTP 服务：先绑端口、用真实 base 拼响应体，再回答一次 GET。
    /// 这样 profile 的 `origin`/`services.files` 必然与请求地址一致（同源校验的硬要求）。
    fn serve_profile(build: impl FnOnce(&str) -> String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let body = build(&base).into_bytes();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0u8; 2048];
            let _ = std::io::Read::read(&mut stream, &mut buffer);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut stream, header.as_bytes());
            let _ = std::io::Write::write_all(&mut stream, &body);
        });
        base
    }

    /// 端到端：两个环境各拉一次 → 两套配置并存 → 开关决定用哪一套。
    /// 这条覆盖「界面点同步 / 切换环境」背后真正跑的路径。
    #[test]
    fn two_environments_are_cached_separately_and_the_switch_selects_one() {
        let (_temporary, root) = storage::test_root();
        let prod = serve_profile(|base| {
            profile_json(
                base,
                "etech-prod",
                "prod",
                "生产助手",
                &["cloudfile.file.view"],
            )
        });
        let test = serve_profile(|base| {
            profile_json(
                base,
                "etech-test",
                "test",
                "测试助手",
                &["cloudfile.file.view"],
            )
        });
        session::trust(&root, format!("{prod}/seafile")).unwrap();
        session::trust(&root, format!("{test}/seafile")).unwrap();

        sync(&root, &prod).unwrap();
        sync(&root, &test).unwrap();
        remember_source(&root, &prod, "prod").unwrap();
        remember_source(&root, &test, "test").unwrap();

        // 两套并存，各自独立（互不覆盖）。记住的两条排在最前，推导出来的候选补在后面
        // —— 候选不能因为"已经记住过"就停止尝试，否则另一侧永远不会再拉。
        let targets = sources(&session::config(&root).unwrap());
        assert!(targets.len() >= 2);
        assert!(targets.contains(&format!("{prod}{DISCOVERY_PATH}")));
        assert!(targets.contains(&format!("{test}{DISCOVERY_PATH}")));

        // 不勾测试服 → 生产那套；勾了 → 测试那套。
        let production = overview(&root, false);
        assert_eq!(production["brand"]["application_name"], "生产助手");
        assert_eq!(production["active"]["environment"], "prod");
        let staging = overview(&root, true);
        assert_eq!(staging["brand"]["application_name"], "测试助手");
        assert_eq!(staging["active"]["environment"], "test");
        // 只在测试开放的能力，切到生产就必须消失。
        assert!(staging["active"]["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "cloudfile"));
        assert_eq!(
            staging["source"].as_str().unwrap(),
            format!("{test}{DISCOVERY_PATH}")
        );
        assert_eq!(
            production["source"].as_str().unwrap(),
            format!("{prod}{DISCOVERY_PATH}")
        );
    }

    /// 构造一份环境 profile 的 JSON；`services.files` 必须与请求来源一致（同源校验）。
    fn profile_json(
        base: &str,
        id: &str,
        environment: &str,
        name: &str,
        capabilities: &[&str],
    ) -> String {
        json!({
            "schema_version": 1,
            "profile_id": id,
            "provider": "etech",
            "environment": environment,
            "revision": 1,
            "expires_at": crate::now() + 3600,
            "application": {"id": "etech", "name": name},
            "origin": base,
            "services": {"files": format!("{base}/seafile")},
            "channels": [{
                "id": "files", "title": "文件", "kind": "native",
                "module": "cloudfile",
                "capabilities": capabilities,
            }],
            "update": {"track": "dev", "source_id": "trusted-dev"},
        })
        .to_string()
    }

    /// 自动刷新判据：不该刷的时候一次都别刷（这是「后台偷偷联网」最容易失控的地方）。
    #[test]
    fn refresh_is_due_after_an_upgrade_or_when_the_active_environment_is_missing() {
        let (_temporary, root) = storage::test_root();
        let interval = REFRESH_INTERVAL;
        // 没有受信任服务 → 连地址都没有，什么都不做。
        assert!(refresh_due(&root, interval, "1.0.0", false).is_empty());

        session::trust(&root, "https://files.example.test/seafile".to_string()).unwrap();
        // 有服务但从没绑定过 → 当前环境缺配置，要拉。
        assert!(!refresh_due(&root, interval, "1.0.0", false).is_empty());

        let now = crate::now();
        store_bound(&root, "etech-prod", "prod", now - 60, now + 86400);
        crate::settings::update(&root, |current| {
            current.profile_pull_version = Some("1.0.0".into());
        })
        .unwrap();
        // 刚拉过、还是同一个版本 → 不刷。
        assert!(refresh_due(&root, interval, "1.0.0", false).is_empty());

        // 升级了（版本变了）→ 立刻刷一次。
        assert!(!refresh_due(&root, interval, "2.0.0", false).is_empty());

        // 超过间隔 → 刷。
        store_bound(
            &root,
            "etech-prod",
            "prod",
            now - interval - 60,
            now + 86400,
        );
        assert!(!refresh_due(&root, interval, "1.0.0", false).is_empty());

        // 已过期 → 刷（即使刚同步过）。
        store_bound(
            &root,
            "etech-prod",
            "prod",
            now - 60,
            now.saturating_sub(10),
        );
        assert!(!refresh_due(&root, interval, "1.0.0", false).is_empty());
    }

    /// 往本地写一条绑定记录（只给刷新判据用，不要求能通过完整校验）。
    fn store_bound(root: &Path, id: &str, environment: &str, fetched_at: u64, expires_at: u64) {
        let dir = root.join("application-profiles");
        storage::private_dir(&dir).unwrap();
        let mut profile = sample();
        profile.profile_id = id.to_string();
        profile.environment = environment.to_string();
        profile.expires_at = expires_at;
        storage::write(
            &dir.join(format!("{id}.json")),
            &BoundProfile {
                source_origin: profile.origin.clone(),
                fetched_at,
                profile,
            },
        )
        .unwrap();
    }

    #[test]
    fn report_is_ambiguous_if_two_profiles_reference_one_fileserver() {
        let a = ProfileLabel {
            profile_id: "a-dev".into(),
            application_name: "A".into(),
            environment: "dev".into(),
            file_server: "https://files.example.test".into(),
            expires_at: 99999,
            stale: false,
        };
        let b = ProfileLabel {
            profile_id: "b-dev".into(),
            ..a.clone()
        };
        assert!(label_for_file_server(&[a, b], "https://files.example.test").is_none());
    }
}
