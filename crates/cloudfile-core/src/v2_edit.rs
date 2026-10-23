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

/// 同文件重复「本地编辑」时，线上已经向前走了，本地那份不能再被静默替换：
/// 把线上版本旁挂成 `<文件名>.cloudfile-conflict`，等用户在任务中心选一份。
#[derive(Clone, Serialize, Deserialize)]
struct Conflict {
    file_id: String,
    size: u64,
    digest: String,
    session_id: String,
    at: u64,
}
#[derive(Deserialize)]
struct TicketClaims {
    repo_id: String,
    path: String,
    mode: String,
    exp: i64,
}
/// Routing and expiry hint only: the native fileserver verifies every ticket.
fn ticket_claims(ticket: &str) -> Option<TicketClaims> {
    let payload = ticket.split('.').nth(1)?;
    serde_json::from_slice(&B64.decode(payload).ok()?).ok()
}
/// Unix time at which the stored browser ticket stops authorizing the task.
pub fn ticket_expires_at(root: &Path, id: &str) -> Option<i64> {
    ticket_claims(&load(root, id).ok()?.ticket).map(|claims| claims.exp)
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
fn endpoint(_root: &Path, task: &Task, op: &str) -> Result<String> {
    Ok(format!("{}/seafhttp/cf-edit/v1/{op}", task.server))
}
fn supported_open_mode(mode: &str) -> bool {
    matches!(mode, "local-view" | "optimistic-edit" | "exclusive-edit")
}
fn valid_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read_response(res: reqwest::blocking::Response) -> Result<Value> {
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
    if !dir.join("task.json").exists() {
        if let Some(task) = resume(root, &server, &ticket)? {
            return Ok(task);
        }
    }
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
    let mut res = client()?
        .get(format!("{server}/seafhttp/cf-edit/v1/read"))
        .bearer_auth(&state.ticket)
        .send()?;
    if !res.status().is_success() {
        // 修改原因（2026-10-23）：这句话原来不带状态码也不带服务端原因，线上只能看到
        // 「原生读取失败」。401=票据被服务端拒（寿命上限/密钥不一致），403=当前无权限，
        // 404=路径或仓库不对 —— 三者处理方式完全不同。带上状态码与一小段服务端正文。
        let status = res.status();
        let detail: String = res
            .text()
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect();
        bail!(
            "CloudFile 原生读取失败（HTTP {}）{}，请重新从网页发起",
            status.as_u16(),
            if detail.trim().is_empty() {
                String::new()
            } else {
                format!("：{}", detail.trim())
            }
        )
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
        || !supported_open_mode(&mode)
        || size > crate::device::MAX_FILE
    {
        bail!("服务端文件元数据无效")
    }
    // 同一文件重复「本地查看 / 本地编辑」时先尝试合并到已有任务，绝不再建第二条。
    if let Some(existing) = session::find_task(root, &repo_id, &remote, &mode)? {
        if existing.server == server && is_v2(root, &existing.task_id) {
            if let Some(merged) = merge(
                root,
                existing,
                Merge {
                    ticket: state.ticket.clone(),
                    sid: sid.clone(),
                    mode: mode.clone(),
                    file_id: file_id.clone(),
                    size,
                    res: &mut res,
                },
            )? {
                return Ok(merged);
            }
        }
    }
    let filename = std::path::Path::new(&remote)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty() && *s != ".")
        .ok_or_else(|| anyhow::anyhow!("文件名不可用"))?
        .to_string();
    let dir_name = filename.replace(['/', char::from(92)], "_");
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
    if mode == "local-view" {
        // Defense in depth only. The native Fileserver is the authority:
        // this ticket is valid for READ alone, never publish or Checkout.
        let mut permissions = file.as_file().metadata()?.permissions();
        permissions.set_readonly(true);
        file.as_file().set_permissions(permissions)?;
    }
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
/// Reopening a checked-out file from the browser is the only way to obtain a
/// fresh ticket (no Agent login). Adopt it into the existing task ONLY when it
/// renews the original Guard, which proves the same owner; otherwise fall
/// back to an ordinary new Checkout.
fn resume(root: &Path, server: &str, ticket: &str) -> Result<Option<Task>> {
    let Some(claims) = ticket_claims(ticket) else {
        return Ok(None);
    };
    if claims.mode != "exclusive-edit" {
        return Ok(None);
    }
    // An unreadable sibling task must not block ordinary opening.
    for task in session::tasks(root).unwrap_or_default() {
        if task.server != server
            || task.repo_id != claims.repo_id
            || task.remote_path != claims.path
            || task.mode != "exclusive-edit"
            || !is_v2(root, &task.task_id)
            || journal::closed(root, &task.task_id).unwrap_or(true)
        {
            continue;
        }
        let dir = session::task_dir(root, &task.task_id)?;
        let _lock = storage::lock(&dir.join("upload.lock"))?;
        let mut st = load(root, &task.task_id)?;
        let Some(g) = st.guard.clone() else {
            continue;
        };
        let candidate = State {
            ticket: ticket.into(),
            guard: Some(g.clone()),
        };
        let renewed = request_json(
            root,
            &task,
            &candidate,
            "renew",
            &json!({
               "guard_id":g.guard_id,"generation":g.generation,"proof":g.proof
            }),
        );
        if renewed.is_ok_and(|value| value["ok"] == true) {
            st.ticket = ticket.into();
            store(root, &task.task_id, &st)?;
            return Ok(Some(task));
        }
    }
    Ok(None)
}
/// 把「同一文件再次打开」合并进已有任务。
///
/// - `local-view`：本地副本只是线上镜像，直接用新票据下载到的字节覆盖，任务保持一条。
/// - `optimistic-edit`：`file_id` 没变说明本地那份就是最新，只换票据；变了说明线上
///   已经向前走，把线上版本旁挂成 `<文件名>.cloudfile-conflict` 并写 `conflict.json`，
///   任务仍是一条，由任务中心的「用线上覆盖本地 / 保留本地版本」决定。
///
/// 返回 `Ok(None)` 表示这条旧记录已经不可用（工作副本丢了），调用方按新任务继续。
struct Merge<'a> {
    ticket: String,
    sid: String,
    mode: String,
    file_id: String,
    size: u64,
    res: &'a mut reqwest::blocking::Response,
}

fn merge(root: &Path, mut task: Task, input: Merge<'_>) -> Result<Option<Task>> {
    let Merge { ticket, sid, mode, file_id, size, res } = input;
    let mode = mode.as_str();
    let dir = session::task_dir(root, &task.task_id)?;
    let working = match session::working_dir(root, &task.task_id) {
        Ok(working) => working,
        Err(_) => {
            let _ = session::remove_task(root, &task.task_id);
            return Ok(None);
        }
    };
    let local = match session::local_path(root, &task) {
        Ok(local) => local,
        Err(_) => {
            // 记录还在、副本丢了：留着只会挡住下一次打开，直接清掉按新任务处理。
            let _ = session::remove_task(root, &task.task_id);
            return Ok(None);
        }
    };
    let mut state = load(root, &task.task_id).unwrap_or(State {
        ticket: ticket.clone(),
        guard: None,
    });
    state.ticket = ticket;
    if mode != "local-view" && file_id == task.baseline_file_id {
        // 线上没变：本地那份就是最新，丢弃这次下载内容，只换票据。
        task.session_id = sid;
        storage::write(&dir.join("task.json"), &task)?;
        store(root, &task.task_id, &state)?;
        return Ok(Some(task));
    }
    let mut downloaded = tempfile::NamedTempFile::new_in(&working)?;
    let copied = std::io::copy(&mut res.take(size + 1), &mut downloaded)?;
    if copied != size {
        bail!("文件下载不完整")
    }
    downloaded.flush()?;
    downloaded.as_file().sync_all()?;
    let digest = session::digest(downloaded.path())?;
    if mode == "local-view" {
        clear_read_only(&local)?;
        downloaded.persist(&local)?;
        // 查看副本永远是线上镜像：本地改它没有任何意义，重新压成只读。
        let mut permissions = fs::metadata(&local)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&local, permissions)?;
        task.baseline_file_id = file_id;
        task.baseline_digest = digest;
        task.size = size;
        task.session_id = sid;
        storage::write(&dir.join("task.json"), &task)?;
        store(root, &task.task_id, &state)?;
        return Ok(Some(task));
    }
    // 编辑：线上向前走了。落一份旁挂副本等用户选择，绝不碰本地那份。
    let conflict_path = working.join(format!("{}.cloudfile-conflict", task.file_name));
    let _ = fs::remove_file(&conflict_path);
    downloaded.persist(&conflict_path)?;
    storage::write(
        &dir.join("conflict.json"),
        &Conflict {
            file_id,
            size,
            digest,
            session_id: sid.clone(),
            at: crate::now(),
        },
    )?;
    task.session_id = sid;
    storage::write(&dir.join("task.json"), &task)?;
    store(root, &task.task_id, &state)?;
    Ok(Some(task))
}

// Windows 的只读位只有开/关两态，这里确实要关掉它才能替换文件；
// Unix 分支走 0600，不经过 set_readonly(false)。
#[allow(clippy::permissions_set_readonly_false)]
fn clear_read_only(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 只还给属主读写：set_readonly(false) 在 Unix 上会变成 world-writable。
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        if permissions.readonly() {
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

/// 任务中心展示用：是否还有一份线上版本等用户决定。
pub fn conflict(root: &Path, id: &str) -> Option<Value> {
    let file = session::task_dir(root, id).ok()?.join("conflict.json");
    if !file.exists() {
        return None;
    }
    storage::read::<Conflict>(&file)
        .ok()
        .map(|value| json!({"file_id": value.file_id, "size": value.size, "at": value.at}))
}

/// 用户在任务中心选定一份：`remote` 用线上覆盖本地，`local` 保留本地。
/// 两条路径都不新建任务，任务始终只有一条。
pub fn resolve_conflict(root: &Path, id: &str, choice: &str) -> Result<Value> {
    let mut task = session::load(root, id)?;
    let dir = session::task_dir(root, id)?;
    let conflict_path = dir.join("conflict.json");
    let conflict: Conflict = storage::read(&conflict_path)?;
    let working = session::working_dir(root, id)?;
    let local = session::local_path(root, &task)?;
    let copy = working.join(format!("{}.cloudfile-conflict", task.file_name));
    match choice {
        "remote" => {
            if task.mode == "local-view" {
                clear_read_only(&local)?;
            }
            fs::rename(&copy, &local)?;
            if task.mode == "local-view" {
                let mut permissions = fs::metadata(&local)?.permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&local, permissions)?;
            }
            task.baseline_file_id = conflict.file_id.clone();
            task.baseline_digest = conflict.digest.clone();
            task.size = conflict.size;
        }
        "local" => {
            // 保留本地：基线换成线上最新，后续上传以它为 base；本地内容一个字都不动。
            task.baseline_file_id = conflict.file_id.clone();
            let _ = fs::remove_file(&copy);
        }
        _ => bail!("未知的冲突处理方式"),
    }
    storage::write(&dir.join("task.json"), &task)?;
    let _ = fs::remove_file(&conflict_path);
    Ok(json!({"ok": true, "state": "resolved", "choice": choice, "file_name": task.file_name}))
}

fn status(root: &Path, task: &Task, st: &State) -> Result<Value> {
    let res = client()?
        .get(endpoint(root, task, "status")?)
        .bearer_auth(&st.ticket)
        .send()?;
    read_response(res)
}
/// Business mode is enforced here, not only by the task-center button.
fn valid_action(mode: &str, action: &Action) -> bool {
    matches!(
        (mode, action),
        ("optimistic-edit", Action::OptimisticUpdate)
            | ("exclusive-edit", Action::Commit | Action::Checkin)
    )
}
fn issue(root: &Path, id: &str, observed: Option<&str>, action: Action) -> Result<Value> {
    let task = session::load(root, id)?;
    if !valid_action(&task.mode, &action) {
        bail!("当前编辑模式不允许此提交操作；请从任务中心选择正确的操作")
    }
    let checkin = action == Action::Checkin;
    let dir = session::task_dir(root, id)?;
    let _lock = match storage::lock(&dir.join("upload.lock")) {
        Ok(lock) => lock,
        Err(_) if observed.is_some() => return Ok(json!({"state":"busy"})),
        Err(error) => return Err(error),
    };
    if journal::closed(root, id)? {
        bail!("编辑任务已结束，不能重复提交")
    }
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
            if i.action != action {
                bail!("存在不同操作类型的待确认提交，先查询原提交结果")
            }
            i
        }
        None => {
            if !checkin
                && session::digest(&session::local_path(root, &task)?)? == task.baseline_digest
            {
                return Ok(json!({"state":"unchanged"}));
            }
            journal::capture_observed(root, id, action, observed)?
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
    match issue(root, id, observed, Action::OptimisticUpdate) {
        Err(error) if observed.is_some() && error.is::<journal::WorkingCopyChanged>() => {
            Ok(json!({"state":"changing"}))
        }
        result => result,
    }
}
pub fn commit(root: &Path, id: &str, checkin: bool) -> Result<Value> {
    issue(
        root,
        id,
        None,
        if checkin {
            Action::Checkin
        } else {
            Action::Commit
        },
    )
}
/// Retry only the original immutable exclusive-edit intent, including Checkin.
pub fn retry(root: &Path, id: &str) -> Result<Value> {
    let pending =
        journal::pending(root, id)?.ok_or_else(|| anyhow::anyhow!("没有待确认的独占提交"))?;
    match pending.action {
        Action::Commit => commit(root, id, false),
        Action::Checkin => commit(root, id, true),
        Action::OptimisticUpdate => bail!("此任务不是独占提交"),
    }
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
    #[test]
    fn v2_readonly_mode_is_explicit_and_never_implicitly_editable() {
        assert!(supported_open_mode("local-view"));
        assert!(supported_open_mode("optimistic-edit"));
        assert!(supported_open_mode("exclusive-edit"));
        assert!(!supported_open_mode("local-edit"));
        assert!(!supported_open_mode("download"));
        for action in [Action::OptimisticUpdate, Action::Commit, Action::Checkin] {
            assert!(!valid_action("local-view", &action));
        }
    }
    #[test]
    fn write_operations_never_cross_view_optimistic_or_exclusive_modes() {
        assert!(!valid_action("local-view", &Action::OptimisticUpdate));
        assert!(!valid_action("local-view", &Action::Commit));
        assert!(!valid_action("local-view", &Action::Checkin));
        assert!(valid_action("optimistic-edit", &Action::OptimisticUpdate));
        assert!(!valid_action("optimistic-edit", &Action::Commit));
        assert!(!valid_action("optimistic-edit", &Action::Checkin));
        assert!(valid_action("exclusive-edit", &Action::Commit));
        assert!(valid_action("exclusive-edit", &Action::Checkin));
        assert!(!valid_action("exclusive-edit", &Action::OptimisticUpdate));
    }
    #[test]
    fn optimistic_observed_save_reports_transient_change_and_busy_without_network() {
        let (_dir, root) = storage::test_root();
        let id = "a".repeat(64);
        let dir = session::task_dir(&root, &id).unwrap();
        storage::private_dir(&dir).unwrap();
        let task = Task {
            task_id: id.clone(),
            device_id: None,
            file_name: "edit.txt".into(),
            mode: "optimistic-edit".into(),
            server: "https://example.com".into(),
            repo_id: "11111111-1111-4111-8111-111111111111".into(),
            remote_path: "/edit.txt".into(),
            session_id: "22222222-2222-4222-8222-222222222222".into(),
            baseline_file_id: "a".repeat(40),
            baseline_digest: "b".repeat(64),
            size: 1,
        };
        storage::write(&dir.join("task.json"), &task).unwrap();
        fs::write(dir.join("edit.txt"), b"x").unwrap();
        store(
            &root,
            &id,
            &State {
                ticket: "no-network".into(),
                guard: None,
            },
        )
        .unwrap();
        let changed = save(&root, &id, Some(&"0".repeat(64))).unwrap();
        assert_eq!(changed["state"], "changing");
        assert!(journal::pending(&root, &id).unwrap().is_none());

        let _held = storage::lock(&dir.join("upload.lock")).unwrap();
        let busy = save(&root, &id, Some(&"0".repeat(64))).unwrap();
        assert_eq!(busy["state"], "busy");
        assert!(journal::pending(&root, &id).unwrap().is_none());

        // Cross-mode misuse must fail before any provider request or journal.
        assert!(commit(&root, &id, true).is_err());
        assert!(commit(&root, &id, false).is_err());
    }
    #[test]
    fn ticket_claims_route_reopen_without_trusting_signature() {
        let payload = B64.encode(
            br#"{"repo_id":"r","path":"/a.docx","mode":"exclusive-edit","exp":1700000000}"#,
        );
        let claims = ticket_claims(&format!("h.{payload}.s")).unwrap();
        assert_eq!(claims.repo_id, "r");
        assert_eq!(claims.path, "/a.docx");
        assert_eq!(claims.mode, "exclusive-edit");
        assert_eq!(claims.exp, 1700000000);
        assert!(ticket_claims("h.not-json.s").is_none());
        assert!(ticket_claims("nope").is_none());
    }

    fn conflict_fixture(root: &Path, id: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        crate::settings::update(root, |settings| {
            settings.workspace_root = Some(root.join("workspace").to_string_lossy().into_owned())
        })
        .unwrap();
        let working = session::prepare_working_dir(root, id).unwrap();
        let task = Task {
            device_id: None,
            task_id: id.to_string(),
            file_name: "a.docx".into(),
            mode: "optimistic-edit".into(),
            server: "http://127.0.0.1:6111".into(),
            repo_id: "11111111-1111-4111-8111-111111111111".into(),
            remote_path: "/a.docx".into(),
            session_id: "old".into(),
            baseline_file_id: "b".repeat(40),
            baseline_digest: "d".repeat(64),
            size: 4,
        };
        let local = working.join("a.docx");
        fs::write(&local, b"mine").unwrap();
        fs::write(working.join("a.docx.cloudfile-conflict"), b"theirs").unwrap();
        storage::write(
            &session::task_dir(root, id).unwrap().join("conflict.json"),
            &Conflict {
                file_id: "c".repeat(40),
                size: 6,
                digest: "e".repeat(64),
                session_id: "new".into(),
                at: 1,
            },
        )
        .unwrap();
        storage::write(&session::task_dir(root, id).unwrap().join("task.json"), &task).unwrap();
        (local, working)
    }

    #[test]
    fn remote_choice_overwrites_the_copy_and_keeps_one_task() {
        let (_dir, root) = storage::test_root();
        let id = "e".repeat(64);
        let (local, _working) = conflict_fixture(&root, &id);
        let value = resolve_conflict(&root, &id, "remote").unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(fs::read(&local).unwrap(), b"theirs");
        let task = session::load(&root, &id).unwrap();
        assert_eq!(task.baseline_file_id, "c".repeat(40));
        assert_eq!(task.size, 6);
        assert!(conflict(&root, &id).is_none());
        assert_eq!(session::tasks(&root).unwrap().len(), 1);
    }

    #[test]
    fn local_choice_keeps_bytes_and_moves_the_baseline_forward() {
        let (_dir, root) = storage::test_root();
        let id = "f".repeat(64);
        let (local, working) = conflict_fixture(&root, &id);
        resolve_conflict(&root, &id, "local").unwrap();
        assert_eq!(fs::read(&local).unwrap(), b"mine");
        assert!(!working.join("a.docx.cloudfile-conflict").exists());
        let task = session::load(&root, &id).unwrap();
        assert_eq!(task.baseline_file_id, "c".repeat(40));
        assert!(conflict(&root, &id).is_none());
        assert!(resolve_conflict(&root, &id, "whatever").is_err());
    }
}
