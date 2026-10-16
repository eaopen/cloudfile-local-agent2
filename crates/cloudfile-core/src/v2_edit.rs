//! CloudFile V2 browser-authorized editing; NO separate Agent login,
//! server Session or server Intent. The only persistent remote state is
//! a Checkout Guard. Rust's durable local Journal owns uncertain writes.
use crate::{
    journal::{self, Action, Intent, Receipt},
    session::{self, Task},
    storage,
};
use anyhow::{bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
struct Guard {
    guard_id: String,
    generation: u64,
    proof: String,
    base_file_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    ticket: String,
    guard: Option<Guard>,
}
fn state_file(root: &Path, id: &str) -> Result<std::path::PathBuf> {
    Ok(session::task_dir(root, id)?.join("v2-edit.json"))
}
pub fn is_v2(root: &Path, id: &str) -> bool {
    state_file(root, id).is_ok_and(|file| file.exists())
}
/// A durable local credential indicates a possibly held Checkout even when
/// the server has not been reached. Treat it as active for exit/upgrade safety.
pub fn has_checkout(root: &Path, id: &str) -> bool {
    load(root, id).is_ok_and(|state| state.guard.is_some())
}
fn load(root: &Path, id: &str) -> Result<State> {
    storage::read(&state_file(root, id)?)
}
fn store(root: &Path, id: &str, st: &State) -> Result<()> {
    storage::write(&state_file(root, id)?, st)
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn endpoint(root: &Path, task: &Task, op: &str) -> Result<String> {
    let prefix = session::service_path(root, &task.server)?;
    Ok(format!(
        "{}{}{}",
        task.server,
        prefix,
        format!("/seafhttp/cf-edit/v1/{op}")
    ))
}
fn valid_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read_response(mut res: reqwest::blocking::Response) -> Result<Value> {
    let status = res.status();
    let mut buf = Vec::new();
    res.take(65537).read_to_end(&mut buf)?;
    if buf.len() > 65536 {
        bail!("CloudFile 响应过长")
    }
    if !status.is_success() {
        bail!("CloudFile 拒绝操作：HTTP {status}；工作文件已保留")
    }
    Ok(serde_json::from_slice(&buf)?)
}
fn request_json(root: &Path, task: &Task, st: &State, op: &str, body: &Value) -> Result<Value> {
    let res = client()?
        .post(endpoint(root, task, op)?)
        .bearer_auth(&st.ticket)
        .json(body)
        .send()?;
    read_response(res)
}
fn ticket_format(s: &str) -> bool {
    s.len() < 8192
        && s.split('.').count() == 3
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn uri_parts(uri: &str) -> Result<(String, String, String)> {
    if uri.len() > 16384 {
        bail!("无效的编辑授权")
    }
    let u = url::Url::parse(uri)?;
    if u.scheme() != "cloudfile-open"
        || u.host_str() != Some("v2")
        || u.path() != "/open"
        || u.fragment().is_some()
    {
        bail!("不是 CloudFile V2 编辑地址")
    }
    let pairs = u.query_pairs().collect::<Vec<_>>();
    if pairs.len() != 3
        || pairs[0].0 != "instance"
        || pairs[1].0 != "session"
        || pairs[2].0 != "ticket"
    {
        bail!("无效的 V2 编辑字段")
    }
    let instance = pairs[0].1.to_string();
    session::origin(&instance)?;
    let id = pairs[1].1.to_string();
    if uuid::Uuid::parse_str(&id).is_err() {
        bail!("无效编辑会话标识")
    }
    let ticket = pairs[2].1.to_string();
    if !ticket_format(&ticket) {
        bail!("无效的文件授权")
    }
    Ok((instance, id, ticket))
}
/// Trust is granted by the user's desktop service settings. A signed Hub
/// token is NEVER treated as proof until the native fileserver verifies it.
pub fn open(root: &Path, uri: &str) -> Result<Task> {
    let (server, sid, ticket) = uri_parts(uri)?;
    if !session::config(root)?.allowed_origins.contains(&server) {
        bail!("请在 CloudFile Agent 服务设置中先添加此服务地址")
    }
    let id = format!(
        "{:x}",
        Sha256::digest(format!("{server}\\0{sid}").as_bytes())
    );
    let dir = session::task_dir(root, &id)?;
    storage::private_dir(&dir)?;
    let state = State {
        ticket,
        guard: None,
    };
    if dir.join("task.json").exists() {
        let task = session::load(root, &id)?;
        if task.server != server || task.session_id != sid || !is_v2(root, &id) {
            bail!("已有同名任务，不能覆盖")
        }
        session::local_path(root, &task)?;
        let previous = load(root, &id)?;
        store(
            root,
            &id,
            &State {
                ticket: state.ticket,
                guard: previous.guard,
            },
        )?;
        return Ok(task);
    }
    let prefix = session::service_path(root, &server)?;
    let res = client()?
        .get(format!("{server}{prefix}/seafhttp/cf-edit/v1/read"))
        .bearer_auth(&state.ticket)
        .send()?;
    if !res.status().is_success() {
        bail!("CloudFile 原生读取失败，请重新从网页发起")
    }
    let file_id = res
        .headers()
        .get("X-CF-File-ID")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("")
        .to_string();
    let repo_id = res
        .headers()
        .get("X-CF-Repo-ID")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("")
        .to_string();
    let encoded_path = res
        .headers()
        .get("X-CF-Path-B64")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("");
    let remote = String::from_utf8(B64.decode(encoded_path)?)?;
    let mode = res
        .headers()
        .get("X-CF-Mode")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("")
        .to_string();
    let size = res
        .content_length()
        .ok_or_else(|| anyhow::anyhow!("文件长度不明"))?;
    if !valid_id(&file_id)
        || uuid::Uuid::parse_str(&repo_id).is_err()
        || !remote.starts_with('/')
        || remote.contains("..")
        || !matches!(mode.as_str(), "optimistic-edit" | "exclusive-edit")
        || size > crate::device::MAX_FILE
    {
        bail!("服务端文件元数据无效")
    }
    let filename = std::path::Path::new(&remote)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty() && *s != ".")
        .ok_or_else(|| anyhow::anyhow!("文件名不可用"))?
        .to_string();
    let dir_name = filename.replace('/', "_").replace(char::from(92), "_");
    let working = session::prepare_working_dir(root, &id)?;
    let dest = working.join(&dir_name);
    if dest.exists() {
        bail!("工作副本已存在，不能覆盖")
    }
    let mut file = tempfile::NamedTempFile::new_in(&working)?;
    let downloaded = std::io::copy(&mut res.take(size + 1), &mut file)?;
    if downloaded != size {
        bail!("文件下载不完整")
    }
    file.flush()?;
    file.as_file().sync_all()?;
    let mut state = state;
    let task = Task {
        task_id: id.clone(),
        device_id: None,
        file_name: dir_name,
        mode: mode.clone(),
        server,
        repo_id,
        remote_path: remote,
        session_id: sid,
        baseline_file_id: file_id.clone(),
        baseline_digest: session::digest(file.path())?,
        size,
    };
    if mode == "exclusive-edit" {
        let terminal_type = match std::env::consts::OS {
            "windows" => "windows",
            "macos" => "macos",
            "linux" => "linux",
            _ => "other",
        };
        let terminal_name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_default();
        let name = terminal_name
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect::<String>();
        let result = request_json(
            root,
            &task,
            &state,
            "checkout",
            &json!({
                 "base_file_id":file_id,"terminal_type":terminal_type,
                 "terminal_name":name
            }),
        )?;
        let guard = Guard {
            guard_id: result["guard_id"].as_str().unwrap_or("").into(),
            generation: result["generation"].as_u64().unwrap_or(0),
            proof: result["proof"].as_str().unwrap_or("").into(),
            base_file_id: file_id,
        };
        if uuid::Uuid::parse_str(&guard.guard_id).is_err()
            || guard.generation == 0
            || guard.proof.len() != 64
        {
            bail!("独占凭据无效")
        }
        state.guard = Some(guard);
    }
    // Persist the Guard before exposing the work copy. Checkout can expire
    // safely after an interrupted disk write; never release on an unknown reply.
    store(root, &id, &state)?;
    file.persist_noclobber(dest)?;
    storage::write(&dir.join("task.json"), &task)?;
    Ok(task)
}
fn status(root: &Path, task: &Task, st: &State) -> Result<Value> {
    let res = client()?
        .get(endpoint(root, task, "status")?)
        .bearer_auth(&st.ticket)
        .send()?;
    read_response(res)
}
fn issue(root: &Path, id: &str, observed: Option<&str>, checkin: bool) -> Result<Value> {
    let task = session::load(root, id)?;
    let dir = session::task_dir(root, id)?;
    let _lock = storage::lock(&dir.join("upload.lock"))?;
    let mut st = load(root, id)?;
    if task.mode == "exclusive-edit" && st.guard.is_none() {
        bail!("Checkout 不存在")
    }
    let pending = journal::pending(root, id)?;
    let was_pending = pending.is_some();
    let intent = match pending {
        Some(i) => {
            if observed.is_some() {
                return Ok(json!({"state":"pending"}));
            }
            i
        }
        None => {
            if !checkin
                && session::digest(&session::local_path(root, &task)?)? == task.baseline_digest
            {
                return Ok(json!({"state":"unchanged"}));
            }
            journal::capture_observed(
                root,
                id,
                if checkin {
                    Action::Checkin
                } else if task.mode == "exclusive-edit" {
                    Action::Commit
                } else {
                    Action::OptimisticUpdate
                },
                observed,
            )?
        }
    };
    // A caller-triggered retry MUST first check its original remote state.
    // Automatically observed edits never retry an uncertain pending snapshot.
    if was_pending {
        let value = status(root, &task, &st)?;
        if value["sha256"] == intent.content_digest {
            return accept(root, id, &mut st, &intent, &value);
        }
        if value["file_id"] != intent.base_file_id {
            return Ok(json!({"state":"conflict","local_preserved":true}));
        }
    }
    let file = dir.join(format!("{}.snapshot", intent.intent_id));
    let meta = fs::symlink_metadata(&file)?;
    if !meta.is_file() || meta.len() != intent.snapshot_size {
        bail!("原快照失效")
    }
    if session::digest(&file)? != intent.content_digest {
        bail!("原快照校验失败")
    }
    let action = match intent.action {
        Action::OptimisticUpdate => "optimistic",
        Action::Commit => "commit",
        Action::Checkin => "checkin",
    };
    let mut request = client()?
        .post(endpoint(root, &task, "publish")?)
        .bearer_auth(&st.ticket)
        .header("X-CF-Action", action)
        .header("X-CF-Base-ID", &intent.base_file_id)
        .header("X-CF-SHA256", &intent.content_digest)
        .header("Idempotency-Key", intent.intent_id.to_string());
    if let Some(guard) = &st.guard {
        request = request
            .header("X-CF-Guard-ID", &guard.guard_id)
            .header("X-CF-Guard-Generation", guard.generation.to_string())
            .header("X-CF-Guard-Proof", &guard.proof);
    }
    let res = request
        .body(reqwest::blocking::Body::sized(
            fs::File::open(file)?,
            intent.snapshot_size,
        ))
        .send()?;
    let value = read_response(res)?;
    let file = value["file_id"].as_str().unwrap_or("");
    if value["state"] != "published" || !valid_id(file) || value["checked_in"] != checkin {
        bail!("服务器未确认原生提交")
    }
    let mut receipt = json!({"file_id":file});
    if checkin {
        receipt["checked_out"] = json!(false)
    }
    accept(root, id, &mut st, &intent, &receipt)
}
fn accept(root: &Path, id: &str, st: &mut State, intent: &Intent, remote: &Value) -> Result<Value> {
    let file = remote["file_id"].as_str().unwrap_or("");
    if !valid_id(file)
        || remote["sha256"]
            .as_str()
            .is_some_and(|s| s != intent.content_digest)
    {
        bail!("文件回执不匹配")
    }
    if matches!(intent.action, Action::Checkin) && remote["checked_out"] != false {
        // When an HTTP reply is lost, a new owner's Guard is ambiguous.
        return Ok(json!({"state":"pending","local_preserved":true}));
    }
    let checked_in = matches!(intent.action, Action::Checkin);
    let receipt = Receipt {
        intent_id: intent.intent_id,
        state: "published".into(),
        action: intent.action.clone(),
        base_file_id: intent.base_file_id.clone(),
        content_digest: intent.content_digest.clone(),
        result_file_id: file.into(),
        checked_in,
    };
    journal::accept(root, id, receipt)?;
    if checked_in {
        st.guard = None
    } else if let Some(g) = st.guard.as_mut() {
        g.base_file_id = file.into()
    }
    store(root, id, st)?;
    Ok(json!({"state":"published","checked_in":checked_in}))
}
pub fn save(root: &Path, id: &str, observed: Option<&str>) -> Result<Value> {
    issue(root, id, observed, false)
}
pub fn commit(root: &Path, id: &str, checkin: bool) -> Result<Value> {
    issue(root, id, None, checkin)
}
pub fn recover(root: &Path, id: &str) -> Result<Value> {
    let task = session::load(root, id)?;
    let dir = session::task_dir(root, id)?;
    let _lock = storage::lock(&dir.join("upload.lock"))?;
    let intent =
        journal::pending(root, id)?.ok_or_else(|| anyhow::anyhow!("没有待恢复的本地提交"))?;
    let mut st = load(root, id)?;
    let remote = status(root, &task, &st)?;
    if remote["sha256"] == intent.content_digest {
        return accept(root, id, &mut st, &intent, &remote);
    }
    if remote["file_id"] != intent.base_file_id {
        return Ok(json!({"state":"conflict","local_preserved":true}));
    }
    Ok(json!({"state":"pending","local_preserved":true}))
}
fn guard_action(root: &Path, id: &str, action: &str) -> Result<Value> {
    let task = session::load(root, id)?;
    let mut st = load(root, id)?;
    let g = st
        .guard
        .clone()
        .ok_or_else(|| anyhow::anyhow!("无独占凭据"))?;
    if journal::pending(root, id)?.is_some() {
        bail!("存在未确认提交，请先查询原文件状态")
    }
    let response = request_json(
        root,
        &task,
        &st,
        action,
        &json!({
           "guard_id":g.guard_id,"generation":g.generation,"proof":g.proof
        }),
    )?;
    if response["ok"] != true {
        bail!("独占状态未确认")
    }
    if action == "release" {
        st.guard = None;
        store(root, id, &st)?;
        journal::close(root, id)?;
    }
    Ok(json!({"state":if action=="renew"{"renewed"}else{"released"}}))
}
pub fn renew(root: &Path, id: &str) -> Result<Value> {
    guard_action(root, id, "renew")
}
pub fn release(root: &Path, id: &str) -> Result<Value> {
    guard_action(root, id, "release")
}
pub fn is_open_uri(uri: &str) -> bool {
    uri.starts_with("cloudfile-open://v2/open?")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_v2_uri_selects_new_transport() {
        assert!(is_open_uri("cloudfile-open://v2/open?instance=x"));
        assert!(!is_open_uri("cloudfile-open://v1/open?instance=x"));
        assert!(uri_parts("cloudfile-open://v2/open?instance=https%3A%2F%2Ffiles.example.com&session=11111111-1111-4111-8111-111111111111&ticket=x.y.z").is_ok());
        assert!(uri_parts("cloudfile-open://v2/open?instance=https%3A%2F%2Ffiles.example.com&session=11111111-1111-4111-8111-111111111111&ticket=nope").is_err());
    }
}
