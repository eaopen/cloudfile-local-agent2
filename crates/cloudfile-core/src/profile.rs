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
