//! Current V2 CDC transport. Durable receipts belong to the existing immutable
//! Journal Intent; they are never credentials for another path or actor.
use crate::{cdc, journal::Intent, storage};
use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

// Bound control and per-block operations independently of the one-hour full-file budget.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const BLOCK_TIMEOUT: Duration = Duration::from_secs(180);
const FULL_FALLBACK_LIMIT: u64 = 128 << 20;

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    digest: String,
    gc: String,
    receipts: Vec<String>,
}
fn fallback(started: bool) -> Result<Option<Value>> {
    if started {
        bail!("CDC 能力变化，已有分块进度保留；请恢复原提交")
    }
    Ok(None)
}
fn load_progress(path: &Path) -> Result<Progress> {
    // Receipts are reconstructible transfer hints, never the Journal itself.
    // Recover malformed/oversized JSON, but do not hide filesystem failures or symlinks.
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Progress::default()),
        Err(e) => Err(e.into()),
        Ok(meta) if !meta.is_file() => bail!("CDC 进度文件类型无效"),
        Ok(meta) if meta.len() > 1024 * 1024 => Ok(Progress::default()),
        Ok(_) => match storage::read(path) {
            Ok(value) => Ok(value),
            Err(e) if e.is::<serde_json::Error>() => Ok(Progress::default()),
            Err(e) => Err(e),
        },
    }
}
fn block_request(client: &Client, url: String) -> RequestBuilder {
    client.post(url).timeout(BLOCK_TIMEOUT)
}
fn missing_indices(plan: &Value, scan: &cdc::Manifest) -> Result<Vec<usize>> {
    let values = plan["missing"].as_array().context("CDC 缺少分块计划")?;
    if values.len() > scan.blocks.len() {
        bail!("CDC 分块计划过长")
    }
    let mut seen = BTreeSet::new();
    let mut indices = Vec::new();
    for value in values {
        let index = usize::try_from(value.as_u64().context("CDC 分块索引无效")?)?;
        if index >= scan.blocks.len() || !seen.insert(index) {
            bail!("CDC 分块索引无效")
        }
        indices.push(index);
    }
    Ok(indices)
}
fn worthwhile(scan: &cdc::Manifest, missing: &[usize]) -> bool {
    // Count unique transmitted blocks; repeated blocks cost only one upload.
    let mut seen = BTreeSet::new();
    let bytes = missing
        .iter()
        .filter_map(|&i| {
            let b = &scan.blocks[i];
            seen.insert(&b.id).then_some(b.size)
        })
        .sum();
    cdc::worthwhile(scan.size, bytes)
}
fn json_response(mut res: Response) -> Result<Value> {
    let status = res.status();
    let mut bytes = Vec::new();
    (&mut res).take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("CDC 响应过长")
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if !status.is_success() {
        bail!(
            "CDC 请求失败（HTTP {}，{}）；工作文件已保留",
            status.as_u16(),
            value["code"].as_str().unwrap_or("UNKNOWN")
        )
    }
    Ok(value)
}
fn manifest(intent: &Intent, scan: &cdc::Manifest, progress: &Progress) -> Value {
    json!({"base":intent.base_file_id,"size":scan.size,"sha256":scan.sha256,"gc":progress.gc,
        "blocks":scan.blocks.iter().enumerate().map(|(i,b)|json!({"id":b.id,"size":b.size,"receipt":progress.receipts.get(i).cloned().unwrap_or_default()})).collect::<Vec<_>>()})
}
pub(crate) fn publish(
    client: &Client,
    server: &str,
    ticket: &str,
    dir: &Path,
    intent: &Intent,
    full_request: RequestBuilder,
) -> Result<Option<Value>> {
    if intent.snapshot_size < 16 << 20 {
        return Ok(None);
    }
    let path = dir.join(format!("{}.cdc.json", intent.intent_id));
    let started = fs::symlink_metadata(&path).is_ok();
    let endpoint = |op: &str| format!("{server}/seafhttp/cf-edit/v1/{op}");
    let capability = client
        .get(endpoint("capabilities"))
        .timeout(CONTROL_TIMEOUT)
        .bearer_auth(ticket)
        .send()?;
    // Only a known unsupported route can select full upload; authentication,
    // policy failures and transport errors must never trigger fallback.
    if capability.status().as_u16() == 404 {
        return fallback(started);
    }
    if capability.status().as_u16() == 409 {
        // Unsupported capability responses are bounded just like successful JSON.
        let mut bytes = Vec::new();
        capability.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            bail!("CDC 响应过长")
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        if value["code"] == "CHECKOUT_UNSUPPORTED" {
            return fallback(started);
        }
        bail!("服务端拒绝 CDC 能力查询；工作文件已保留")
    }
    let capability = json_response(capability)?;
    if capability["cdc_v1"] != true {
        return fallback(started);
    }
    if intent.snapshot_size > capability["max_file_size"].as_u64().unwrap_or(0) {
        bail!("文件超过服务端 CDC 上限")
    }
    let snapshot = dir.join(format!("{}.snapshot", intent.intent_id));
    let scan = cdc::scan_streaming(&snapshot)?;
    if scan.sha256 != intent.content_digest || scan.size != intent.snapshot_size {
        bail!("CDC 快照校验失败")
    }
    if scan.blocks.len() as u64 > capability["max_blocks"].as_u64().unwrap_or(0)
        || scan
            .blocks
            .iter()
            .any(|b| b.size > capability["max_block_size"].as_u64().unwrap_or(0))
    {
        // A negotiated full-file budget permits safe fallback before staging only.
        if !started {
            return Ok(None);
        }
        bail!("服务端 CDC 分块能力不匹配，已有分块进度保留")
    }
    let mut progress = load_progress(&path)?;
    if progress.digest != scan.sha256
        || progress.receipts.len() != scan.blocks.len()
        || progress.gc.len() > 128
        || progress
            .receipts
            .iter()
            .any(|r| !r.is_empty() && (r.len() != 64 || !r.bytes().all(|b| b.is_ascii_hexdigit())))
    {
        progress = Progress {
            digest: scan.sha256.clone(),
            gc: String::new(),
            receipts: vec![String::new(); scan.blocks.len()],
        }
    }
    let plan = json_response(
        client
            .post(endpoint("cdc-plan"))
            .timeout(CONTROL_TIMEOUT)
            .bearer_auth(ticket)
            .json(&manifest(intent, &scan, &progress))
            .send()?,
    )?;
    let gc = plan["gc"].as_str().context("CDC 缺少回收代际")?;
    if progress.gc != gc {
        progress.receipts.fill(String::new());
        progress.gc = gc.into()
    }
    let missing = missing_indices(&plan, &scan)?;
    // Low reuse on a fresh moderate file avoids unnecessary block round trips.
    // Large files keep CDC resumability; never change transport after staging.
    if !started && scan.size <= FULL_FALLBACK_LIMIT && !worthwhile(&scan, &missing) {
        return Ok(None);
    }
    storage::write(&path, &progress)?;
    // Sequential bounded chunks preserve ordering and reuse duplicate receipts.
    for i in missing {
        let block = &scan.blocks[i];
        if let Some(j) = scan.blocks.iter().enumerate().find_map(|(j, b)| {
            (j < i && b.id == block.id && b.size == block.size && !progress.receipts[j].is_empty())
                .then_some(j)
        }) {
            progress.receipts[i] = progress.receipts[j].clone();
            continue;
        }
        let mut file = fs::File::open(&snapshot)?;
        file.seek(SeekFrom::Start(block.offset))?;
        let value = json_response(
            block_request(client, endpoint("cdc-block"))
                .bearer_auth(ticket)
                .header("X-CF-Base-ID", &intent.base_file_id)
                .header("X-CF-GC-ID", &progress.gc)
                .header("X-CF-Block-ID", &block.id)
                .body(reqwest::blocking::Body::sized(
                    file.take(block.size),
                    block.size,
                ))
                .send()?,
        )?;
        let receipt = value["receipt"].as_str().context("CDC 缺少块回执")?;
        if receipt.len() != 64 || !receipt.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("CDC 块回执无效")
        }
        progress.receipts[i] = receipt.into();
        storage::write(&path, &progress)?;
    }
    // Keep action/Guard headers from the existing publisher, changing only URL
    // and body. The server routes this through the same final commit transaction.
    let mut request = full_request
        .json(&manifest(intent, &scan, &progress))
        .build()?;
    *request.url_mut() = endpoint("cdc-publish").parse()?;
    let value = json_response(client.execute(request)?)?;
    if value["file_id"] != scan.file_id {
        bail!("CDC 原生文件标识不一致；保留待确认任务")
    }
    // Journal acceptance remains the caller's responsibility. A lost reply keeps
    // receipts and the immutable snapshot for explicit status-first recovery.
    Ok(Some(value))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::journal::Action;
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
        thread,
        time::Duration,
    };
    pub(crate) fn accept(listener: &TcpListener) -> TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    return stream;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "timed out waiting for HTTP request"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        }
    }
    pub(crate) fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            stream.read_exact(&mut b).unwrap();
            header.push(b[0]);
        }
        let header = String::from_utf8(header).unwrap();
        let length = header
            .lines()
            .find_map(|l| {
                l.to_lowercase()
                    .strip_prefix("content-length: ")
                    .map(|s| s.parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        (header, body)
    }
    pub(crate) fn respond(stream: &mut TcpStream, status: u16, value: Value) {
        let body = value.to_string();
        write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    }
    #[test]
    fn interrupted_stage_resumes_missing_blocks_and_preserves_publish_headers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let intent_id = uuid::Uuid::new_v4();
        let snapshot = dir.path().join(format!("{intent_id}.snapshot"));
        fs::File::create(&snapshot)
            .unwrap()
            .set_len(24 << 20)
            .unwrap();
        let scan = cdc::scan_streaming(&snapshot).unwrap();
        let expected_file = scan.file_id.clone();
        let intent = Intent {
            intent_id,
            action: Action::Commit,
            base_file_id: "b".repeat(40),
            content_digest: scan.sha256,
            snapshot_size: scan.size,
        };
        // A damaged progress cache must be rebuilt without losing the immutable snapshot.
        fs::write(root.join(format!("{intent_id}.cdc.json")), b"{broken").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for step in 0..8 {
                let mut stream = accept(&listener);
                let (header, body) = read_request(&mut stream);
                assert!(header
                    .to_lowercase()
                    .contains("authorization: bearer test-ticket"));
                let path = header.split_whitespace().nth(1).unwrap();
                match step {
                    0 | 4 => {
                        assert!(path.ends_with("/capabilities"));
                        respond(
                            &mut stream,
                            200,
                            json!({"cdc_v1":true,"max_file_size":1u64<<30,"max_block_size":10<<20,"max_blocks":256}),
                        );
                    }
                    1 | 5 => {
                        assert!(path.ends_with("/cdc-plan"));
                        let m: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(m["blocks"].as_array().unwrap().len(), 3);
                        if step == 5 {
                            assert_eq!(m["blocks"][0]["receipt"], "a".repeat(64));
                        }
                        respond(
                            &mut stream,
                            200,
                            json!({"gc":"epoch","missing":if step==1{vec![0,1,2]}else{vec![1,2]}}),
                        );
                    }
                    2 | 3 | 6 => {
                        assert!(path.ends_with("/cdc-block"));
                        assert_eq!(body.len(), if step == 2 { 10 << 20 } else { 4 << 20 });
                        if step == 3 {
                            respond(&mut stream, 503, json!({"code":"IO_ERROR"}));
                        } else {
                            respond(&mut stream, 200, json!({"receipt":"a".repeat(64)}));
                        }
                    }
                    7 => {
                        assert!(path.ends_with("/cdc-publish"));
                        let headers = header.to_lowercase();
                        for expected in [
                            "x-cf-action: commit",
                            "x-cf-guard-id: guard",
                            "x-cf-guard-generation: 7",
                            "x-cf-guard-proof: proof",
                            "idempotency-key: original-intent",
                        ] {
                            assert!(headers.contains(expected));
                        }
                        assert!(headers.contains(&format!("x-cf-base-id: {}", "b".repeat(40))));
                        assert!(headers.contains("x-cf-sha256: snapshot-digest"));
                        let m: Value = serde_json::from_slice(&body).unwrap();
                        for b in m["blocks"].as_array().unwrap() {
                            assert_eq!(b["receipt"], "a".repeat(64));
                        }
                        respond(
                            &mut stream,
                            200,
                            json!({"state":"published","file_id":expected_file,"checked_in":false}),
                        );
                    }
                    _ => unreachable!(),
                }
            }
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let request = || {
            client
                .post(format!("{server}/seafhttp/cf-edit/v1/publish"))
                .bearer_auth("test-ticket")
                .header("X-CF-Action", "commit")
                .header("X-CF-Base-ID", "b".repeat(40))
                .header("X-CF-SHA256", "snapshot-digest")
                .header("X-CF-Guard-ID", "guard")
                .header("X-CF-Guard-Generation", "7")
                .header("X-CF-Guard-Proof", "proof")
                .header("Idempotency-Key", "original-intent")
        };
        let failure = publish(&client, &server, "test-ticket", &root, &intent, request())
            .err()
            .unwrap();
        assert!(failure.to_string().contains("IO_ERROR"), "{failure:#}");
        assert!(snapshot.exists());
        let resumed = publish(&client, &server, "test-ticket", &root, &intent, request())
            .unwrap()
            .unwrap();
        assert_eq!(resumed["state"], "published");
        worker.join().unwrap();
    }
    #[test]
    fn authorization_failure_never_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let mut s = accept(&listener);
            read_request(&mut s);
            respond(&mut s, 403, json!({"code":"ACL_DENIED"}));
        });
        let client = Client::new();
        let intent = Intent {
            intent_id: uuid::Uuid::new_v4(),
            action: Action::OptimisticUpdate,
            base_file_id: "a".repeat(40),
            content_digest: "b".repeat(64),
            snapshot_size: 16 << 20,
        };
        assert!(publish(
            &client,
            &server,
            "ticket",
            &root,
            &intent,
            client.post(&server)
        )
        .is_err());
        worker.join().unwrap();
    }
    #[test]
    fn block_timeout_is_independent_of_full_file_timeout() {
        let client = Client::builder()
            .timeout(Duration::from_secs(3600))
            .build()
            .unwrap();
        let request = block_request(&client, "http://localhost/block".into())
            .build()
            .unwrap();
        assert_eq!(request.timeout(), Some(&Duration::from_secs(180)));
    }
    #[test]
    fn invalid_progress_is_rebuildable_but_filesystem_types_are_not_hidden() {
        let (_temp, root) = storage::test_root();
        let path = root.join("transfer.cdc.json");
        fs::write(&path, b"not-json").unwrap();
        assert!(load_progress(&path).unwrap().receipts.is_empty());
        fs::File::create(&path)
            .unwrap()
            .set_len((1 << 20) + 1)
            .unwrap();
        assert!(load_progress(&path).unwrap().receipts.is_empty());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load_progress(&path).is_err());
        assert!(path.is_dir());
        assert!(fallback(true).is_err());
    }
    #[test]
    fn fresh_low_reuse_or_incompatible_blocks_fall_back_before_staging() {
        for incompatible in [false, true] {
            let (_temp, root) = storage::test_root();
            let id = uuid::Uuid::new_v4();
            let path = root.join(format!("{id}.snapshot"));
            fs::File::create(&path).unwrap().set_len(16 << 20).unwrap();
            let scan = cdc::scan_streaming(&path).unwrap();
            let intent = Intent {
                intent_id: id,
                action: Action::OptimisticUpdate,
                base_file_id: "b".repeat(40),
                content_digest: scan.sha256,
                snapshot_size: scan.size,
            };
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let server = format!("http://{}", listener.local_addr().unwrap());
            let worker = thread::spawn(move || {
                let mut stream = accept(&listener);
                let (header, _) = read_request(&mut stream);
                assert!(header.starts_with("GET /seafhttp/cf-edit/v1/capabilities "));
                respond(
                    &mut stream,
                    200,
                    json!({"cdc_v1":true,"max_file_size":1u64<<30,"max_block_size":if incompatible {1}else{10<<20},"max_blocks":256}),
                );
                if !incompatible {
                    let mut stream = accept(&listener);
                    let (header, _) = read_request(&mut stream);
                    assert!(header.starts_with("POST /seafhttp/cf-edit/v1/cdc-plan "));
                    respond(&mut stream, 200, json!({"gc":"epoch","missing":[0,1]}));
                }
            });
            let client = Client::new();
            assert!(publish(
                &client,
                &server,
                "ticket",
                &root,
                &intent,
                client.post(&server)
            )
            .unwrap()
            .is_none());
            assert!(path.exists());
            assert!(!root.join(format!("{id}.cdc.json")).exists());
            worker.join().unwrap();
        }
    }
    #[test]
    fn malformed_missing_plan_is_rejected_before_transfer() {
        let scan = cdc::Manifest {
            file_id: String::new(),
            sha256: String::new(),
            size: 1,
            blocks: vec![cdc::Block {
                id: "a".repeat(40),
                offset: 0,
                size: 1,
            }],
        };
        for missing in [json!([-1]), json!([1]), json!([0, 0]), json!(["0"])] {
            assert!(missing_indices(&json!({"missing":missing}), &scan).is_err());
        }
    }
}
