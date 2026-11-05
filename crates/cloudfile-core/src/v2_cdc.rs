//! Current V2 CDC transport. Durable receipts belong to the existing immutable
//! Journal Intent; they are never credentials for another path or actor.
use crate::{cdc, journal::Intent, storage};
use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    digest: String,
    gc: String,
    receipts: Vec<String>,
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
    let endpoint = |op: &str| format!("{server}/seafhttp/cf-edit/v1/{op}");
    let capability = client
        .get(endpoint("capabilities"))
        .bearer_auth(ticket)
        .send()?;
    // Only a known unsupported route can select full upload; authentication,
    // policy failures and transport errors must never trigger fallback.
    if capability.status().as_u16() == 404 {
        return Ok(None);
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
            return Ok(None);
        }
        bail!("服务端拒绝 CDC 能力查询；工作文件已保留")
    }
    let capability = json_response(capability)?;
    if capability["cdc_v1"] != true {
        return Ok(None);
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
        bail!("服务端 CDC 分块能力不匹配")
    }
    let path = dir.join(format!("{}.cdc.json", intent.intent_id));
    let mut progress: Progress = if path.exists() {
        storage::read(&path)?
    } else {
        Progress::default()
    };
    if progress.digest != scan.sha256 || progress.receipts.len() != scan.blocks.len() {
        progress = Progress {
            digest: scan.sha256.clone(),
            gc: String::new(),
            receipts: vec![String::new(); scan.blocks.len()],
        }
    }
    let plan = json_response(
        client
            .post(endpoint("cdc-plan"))
            .bearer_auth(ticket)
            .json(&manifest(intent, &scan, &progress))
            .send()?,
    )?;
    let gc = plan["gc"].as_str().context("CDC 缺少回收代际")?;
    if progress.gc != gc {
        progress.receipts.fill(String::new());
        progress.gc = gc.into()
    }
    let missing = plan["missing"].as_array().context("CDC 缺少分块计划")?;
    // Sequential bounded chunks make resource use predictable. Repeated IDs
    // reuse the same receipt, while the manifest retains their original order.
    for index in missing {
        let i = index.as_u64().context("CDC 分块索引无效")? as usize;
        let block = scan.blocks.get(i).context("CDC 分块索引越界")?;
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
            client
                .post(endpoint("cdc-block"))
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
mod tests {
    use super::*;
    use crate::journal::Action;
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
        thread,
        time::Duration,
    };
    fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
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
    fn respond(stream: &mut TcpStream, status: u16, value: Value) {
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
            action: Action::OptimisticUpdate,
            base_file_id: "b".repeat(40),
            content_digest: scan.sha256,
            snapshot_size: scan.size,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for step in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
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
                        assert!(header.to_lowercase().contains("x-cf-action: optimistic"));
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
                .header("X-CF-Action", "optimistic")
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
            let (mut s, _) = listener.accept().unwrap();
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
}
