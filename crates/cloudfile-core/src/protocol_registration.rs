//! Optional, current-user URL association registration. Never hijack the
//! Seafile/SeaDrive default handler or tamper with Windows UserChoice.
//! Registering a ProgID is NOT the same as selecting it as OS default.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::Path;

#[cfg(windows)]
const APP_ROOT: &str = r"HKCU\Software\CloudFileAgent2";
#[cfg(windows)]
const APP_NAME: &str = "CloudFile Agent2";
#[cfg(windows)]
const OWNER_VALUE: &str = "CloudFileAgent2Owner";
#[cfg(windows)]
const OWNER_ID: &str = "eaopen.cloudfile-local-agent2";
#[cfg(windows)]
const PROGIDS: [(&str, &str); 2] = [
    ("seafile", "CloudFileAgent2.Seafile"),
    ("cloudfile-open", "CloudFileAgent2.CloudFileOpen"),
];

/// Exactly one executable and one literal OS-supplied URI argument.
/// The binary may contain spaces but not a quotation mark.
pub fn open_command(binary: &Path) -> Result<String> {
    let raw = binary
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid executable path"))?;
    if raw.is_empty() || raw.contains('"') || raw.contains('\0') {
        bail!("invalid executable path");
    }
    Ok(format!(r#""{raw}" --open-uri "%1""#))
}

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("reg.exe").args(args).output()?;
    if !output.status.success() {
        bail!(
            "current-user registry command failed: {}",
            args.first().unwrap_or(&"reg")
        );
    }
    Ok(())
}

#[cfg(windows)]
fn exists(key: &str) -> bool {
    std::process::Command::new("reg.exe")
        .args(["query", key])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(windows)]
fn value(key: &str, name: &str) -> Option<String> {
    let out = std::process::Command::new("reg.exe")
        .args(["query", key, "/v", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // ProgID and our ownership marker are ASCII, independent of the
    // Windows console locale. Do not parse arbitrary registry data as a URI.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            line.split_once("REG_SZ").and_then(|(_, tail)| {
                let v = tail.trim();
                (!v.is_empty()).then(|| v.to_string())
            })
        })
}

#[cfg(windows)]
fn owned(key: &str) -> bool {
    value(key, OWNER_VALUE).as_deref() == Some(OWNER_ID)
}

#[cfg(windows)]
fn add(key: &str, name: Option<&str>, value: &str) -> Result<()> {
    match name {
        Some(name) => reg(&["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"]),
        None => reg(&["add", key, "/ve", "/t", "REG_SZ", "/d", value, "/f"]),
    }
}

#[cfg(windows)]
fn remove(key: &str) -> Result<()> {
    if exists(key) {
        reg(&["delete", key, "/f"])?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn install(binary: &Path) -> Result<()> {
    if !binary.is_absolute() || !binary.is_file() {
        bail!("native host must be an installed absolute executable");
    }
    let command = open_command(binary)?;
    // Preflight every proposed class before writing ANY registration.
    for (_, progid) in PROGIDS {
        let class = format!(r"HKCU\Software\Classes\{progid}");
        if exists(&class) && !owned(&class) {
            bail!("protocol ProgID belongs to another application; refusing takeover");
        }
    }
    if exists(APP_ROOT) && !owned(APP_ROOT) {
        bail!("application registration is not owned by this Agent");
    }
    // The marker is set before any child entries so an interrupted install can
    // be retried or removed without touching unrelated registry keys.
    add(APP_ROOT, Some(OWNER_VALUE), OWNER_ID)?;
    let capabilities = format!(r"{APP_ROOT}\Capabilities");
    add(&capabilities, Some("ApplicationName"), APP_NAME)?;
    add(
        &capabilities,
        Some("ApplicationDescription"),
        "CloudFile user-level local file editor",
    )?;
    let associations = format!(r"{capabilities}\URLAssociations");
    for (scheme, progid) in PROGIDS {
        let class = format!(r"HKCU\Software\Classes\{progid}");
        add(&class, Some(OWNER_VALUE), OWNER_ID)?;
        add(&class, None, &format!("URL:{scheme} (CloudFile Agent2)"))?;
        add(&class, Some("URL Protocol"), "")?;
        add(&format!(r"{class}\shell\open\command"), None, &command)?;
        add(&associations, Some(scheme), progid)?;
    }
    // Application capability is deliberately NOT a UserChoice write. Existing
    // Seafile or SeaDrive handling remains the default until user chooses us.
    add(
        r"HKCU\Software\RegisteredApplications",
        Some(APP_NAME),
        r"Software\CloudFileAgent2\Capabilities",
    )?;
    Ok(())
}

#[cfg(windows)]
pub fn uninstall() -> Result<()> {
    if !owned(APP_ROOT) {
        bail!("Agent-owned application registration not found");
    }
    for (_, progid) in PROGIDS {
        let class = format!(r"HKCU\Software\Classes\{progid}");
        if exists(&class) && !owned(&class) {
            bail!("one protocol class is no longer Agent-owned");
        }
    }
    // Remove our capability first, not somebody else's active UserChoice.
    if value(r"HKCU\Software\RegisteredApplications", APP_NAME).as_deref()
        == Some(r"Software\CloudFileAgent2\Capabilities")
    {
        reg(&[
            "delete",
            r"HKCU\Software\RegisteredApplications",
            "/v",
            APP_NAME,
            "/f",
        ])?;
    }
    for (_, progid) in PROGIDS {
        remove(&format!(r"HKCU\Software\Classes\{progid}"))?;
    }
    remove(APP_ROOT)
}

#[cfg(windows)]
pub fn state() -> Result<Value> {
    let registered = owned(APP_ROOT);
    // An existing SeaDrive/Sync Client registration is retained. Read the
    // current per-user association strictly as diagnostic information.
    let seafile_default = value(
        r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\seafile\UserChoice",
        "ProgId",
    )
    .unwrap_or_else(|| "unknown".to_string());
    Ok(json!({
        "platform":"windows",
        "registered_candidates": registered,
        "seafile_current_progid":seafile_default,
        "using_agent_for_seafile": seafile_default == "CloudFileAgent2.Seafile",
        "note": "Select CloudFile Agent2 in Windows Settings > Default Apps by link type. No automatic SeaDrive takeover."
    }))
}
#[cfg(not(windows))]
pub fn install(_binary: &Path) -> Result<()> {
    bail!("OS URI registration is currently implemented for Windows users only; macOS requires a signed app URL declaration")
}
#[cfg(not(windows))]
pub fn uninstall() -> Result<()> {
    bail!("OS URI registration is currently implemented for Windows users only")
}
#[cfg(not(windows))]
pub fn state() -> Result<Value> {
    Ok(json!({"platform":std::env::consts::OS,
        "registered_candidates":false,
        "note":"Use Chrome Native Messaging until a native app URL handler is registered"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_command_keeps_uri_in_one_argument() {
        assert_eq!(
            open_command(Path::new("C:\\Program Files\\CloudFile\\agent.exe")).unwrap(),
            r#""C:\Program Files\CloudFile\agent.exe" --open-uri "%1""#
        );
        assert!(open_command(Path::new("C:\\bad\"quote.exe")).is_err());
    }
}
