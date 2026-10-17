//! Narrow, non-privileged compatibility with Seafile's Open via Client URL.
//! A seafile:// link is a locator only. It does not contain authentication,
//! server identity or an upload grant. Never translate it into a write token.
use crate::session;
use anyhow::{bail, Result};
use std::path::Path;
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub struct Locator {
    pub repo_id: String,
    pub path: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Destination {
    /// Reopen an existing Agent-owned copy; its original grant still governs upload.
    ExistingTask(String),
    /// Navigate to the already trusted web instance for interactive authentication.
    BrowserUrl(String),
}

pub fn parse(raw: &str) -> Result<Locator> {
    if raw.len() > 8192 {
        bail!("Seafile URL exceeds limit");
    }
    let u = Url::parse(raw)?;
    if u.scheme() != "seafile"
        || u.host_str() != Some("openfile")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
        || !matches!(u.path(), "" | "/")
        || u.fragment().is_some()
    {
        bail!("unsupported Seafile URL");
    }
    let mut repo = None;
    let mut path = None;
    for (key, value) in u.query_pairs() {
        match key.as_ref() {
            "repo_id" if repo.is_none() => repo = Some(value.into_owned()),
            "path" if path.is_none() => path = Some(value.into_owned()),
            // Official desktop clients historically accept this hint. An
            // email is NEVER an identity, endpoint or permission assertion.
            "email" => {}
            _ => bail!("duplicate or unknown Seafile URI argument"),
        }
    }
    let repo_id = repo.ok_or_else(|| anyhow::anyhow!("missing repository"))?;
    uuid::Uuid::parse_str(&repo_id)?;
    let path = path.ok_or_else(|| anyhow::anyhow!("missing file path"))?;
    if path == "/"
        || !path.starts_with('/')
        || path.ends_with('/')
        || path.len() > 4096
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("invalid Seafile file path");
    }
    Ok(Locator { repo_id, path })
}

/// A legacy URL has no server origin. Only stored, explicitly trusted instances
/// may be used. Multiple candidates are rejected rather than guessing.
pub fn resolve(root: &Path, locator: &Locator) -> Result<Destination> {
    let known = session::tasks(root)?;
    let same_file: Vec<_> = known
        .iter()
        .filter(|task| task.repo_id == locator.repo_id && task.remote_path == locator.path)
        .collect();
    if same_file.len() == 1 {
        let task = same_file[0];
        // Never fall back to an unexpected path if this copy is lost or moved.
        session::local_path(root, task)?;
        return Ok(Destination::ExistingTask(task.task_id.clone()));
    }
    if same_file.len() > 1 {
        bail!("multiple local editing tasks; choose one in the task center");
    }
    let cfg = session::config(root)?;
    let mut origins = cfg
        .allowed_origins
        .iter()
        .filter(|s| session::origin(s).is_ok())
        .cloned()
        .collect::<Vec<_>>();
    origins.sort();
    origins.dedup();
    let known_repo = known
        .iter()
        .filter(|task| task.repo_id == locator.repo_id && origins.contains(&task.server))
        .map(|task| task.server.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let server = if known_repo.len() == 1 {
        known_repo.iter().next().unwrap().clone()
    } else if known_repo.is_empty() && origins.len() == 1 {
        origins.remove(0)
    } else {
        bail!("server ambiguous or not trusted; select the CloudFile server in Agent settings");
    };
    let prefix = session::service_path(root, &server)?;
    let mut web = Url::parse(&format!("{server}{prefix}/lib/{}/file", locator.repo_id))?;
    {
        let mut parts = web
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid web file location"))?;
        for part in locator.path.split('/').skip(1) {
            parts.push(part);
        }
    }
    Ok(Destination::BrowserUrl(web.into()))
}

pub fn open(root: &Path, uri: &str) -> Result<Destination> {
    resolve(root, &parse(uri)?)
}

/// No write permission is ever granted by seafile:// itself. A new locator
/// opens an authenticated Seahub page; the user then starts V2 editing.
pub fn dispatch(root: &Path, uri: &str) -> Result<Destination> {
    let target = open(root, uri)?;
    match &target {
        Destination::ExistingTask(id) => session::launch(root, id)?,
        Destination::BrowserUrl(url) => open::that(url)?,
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_seafile_link_but_not_foreign_servers_or_path_traversal() {
        let good = "seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2F%E8%AE%BE%E8%AE%A1%2Ftest%20one.docx&email=a%40b";
        let p = parse(good).unwrap();
        assert_eq!(p.path, "/设计/test one.docx");
        for url in [
            "seafile://other?repo_id=11111111-1111-4111-8111-111111111111&path=%2Fa",
            "seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2F..%2Fsecret",
            "seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2Fa&path=%2Fb",
            "seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2Fa&server=https%3A%2F%2Fevil",
            "seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2Fa#frag",
            "seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2Fa%5Cb",
        ] {
            assert!(parse(url).is_err(), "{url}");
        }
    }
    #[test]
    fn only_known_single_origin_may_receive_legacy_locator() {
        let (_, root) = crate::storage::test_root();
        let l = parse("seafile://openfile?repo_id=11111111-1111-4111-8111-111111111111&path=%2F%E8%AE%BE%E8%AE%A1%2Fa.docx").unwrap();
        assert!(resolve(&root, &l).is_err());
        session::trust(&root, "https://files.example.com/cfile".to_owned()).unwrap();
        assert_eq!(resolve(&root, &l).unwrap(), Destination::BrowserUrl(
            "https://files.example.com/cfile/lib/11111111-1111-4111-8111-111111111111/file/%E8%AE%BE%E8%AE%A1/a.docx".to_owned()
        ));
        session::trust(&root, "https://other.example.com/cfile".to_owned()).unwrap();
        assert!(resolve(&root, &l).is_err());
    }
}
