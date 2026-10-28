//! Optional CloudFile file Provider for Local Bridge's existing MCP stdio Client.
//! This is NOT Chrome Native Messaging: Chrome still uses the original framed
//! protocol and the current Agent2 Desktop/Journal/Guard remain authoritative.
//! V1 exposes only sanitized, local-OS-user read-only status and task counts.
use anyhow::{bail, Result};
use cloudfile_core::{journal, session, storage, update};
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;

const PROTOCOL: &str = "2025-06-18";
const MAX_LINE: u64 = 64 * 1024;
const MAX_TASKS: usize = 2_000;

fn tools() -> Value {
    json!([
        {
            "name":"cloudfile_status",
            "title":"CloudFile Agent2 component status",
            "description":"Read-only version and protocol metadata; no credentials or file paths",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
            "annotations":{"readOnlyHint":true}
        },
        {
            "name":"cloudfile_task_counts",
            "title":"CloudFile Agent2 local task counts",
            "description":"Read-only task counts by file mode; not a cloud-publish completion report",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
            "annotations":{"readOnlyHint":true}
        }
    ])
}
fn status() -> Value {
    json!({
        "provider_id":"cloudfile",
        "schema_version":1,
        "agent_family":"agent2",
        "version":cloudfile_core::VERSION,
        "platform":update::platform_id(std::env::consts::OS, std::env::consts::ARCH),
        "ipc_version":update::IPC_VERSION,
        "journal_version":update::JOURNAL_VERSION,
        "component_installed":true,
        "file_actions_exposed":false,
        "file_authorization":"not_checked",
        "data_owner":"cloudfile-agent2",
        "storage_scope":"agent2_shared"
    })
}
fn counts(root: &Path) -> Result<Value> {
    // Use the existing fail-closed, private-directory task reader rather than
    // directly opening arbitrary task.json files or walking working copies.
    let tasks = session::tasks(root)?;
    if tasks.len() > MAX_TASKS {
        bail!("task listing exceeds bounded Provider limit");
    }
    let mut views = 0usize;
    let mut ordinary = 0usize;
    let mut exclusive = 0usize;
    let mut other = 0usize;
    let mut pending = 0usize;
    for task in &tasks {
        match task.mode.as_str() {
            "local-view" => views += 1,
            "local-edit" | "optimistic-edit" => ordinary += 1,
            "exclusive-edit" => exclusive += 1,
            _ => other += 1,
        }
        // Reading the original Journal does not publish, renew, release,
        // consume a one-time ticket or claim a cloud version has completed.
        if journal::pending(root, &task.task_id)?.is_some() {
            pending += 1;
        }
    }
    Ok(json!({
        "provider_id":"cloudfile",
        "total":tasks.len(),
        "local_view":views,
        "ordinary_edit":ordinary,
        "exclusive_edit":exclusive,
        "other":other,
        "pending_intents":pending,
        "cloud_publish_verified":false,
        "shared_file_tasks":true,
        "task_completion_inferred":false
    }))
}
fn content(value: Value, is_error: bool) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"isError":is_error})
}
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn handle(input: &Value, root: &Path) -> Option<Value> {
    // Proper MCP notifications (including initialized) never receive replies.
    let id = input.get("id")?.clone();
    if input["jsonrpc"] != "2.0" {
        return Some(error(id, -32600, "invalid request"));
    }
    let output = match input["method"].as_str() {
        Some("initialize") => {
            let requested = input["params"]["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL);
            let version = if matches!(requested, "2025-03-26" | "2025-06-18" | "2025-11-25") {
                requested
            } else {
                PROTOCOL
            };
            json!({
                "protocolVersion":version,
                "capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"cloudfile-agent2-provider","version":cloudfile_core::VERSION}
            })
        }
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools":tools()}),
        Some("tools/call") => {
            let params = input["params"].as_object();
            let valid = params.is_some_and(|p| {
                p.len() == 2
                    && p.get("arguments")
                        .is_some_and(|v| v.as_object().is_some_and(|args| args.is_empty()))
            });
            if !valid {
                content(json!({"code":"invalid_tool_arguments"}), true)
            } else {
                match input["params"]["name"].as_str() {
                    Some("cloudfile_status") => content(status(), false),
                    Some("cloudfile_task_counts") => match counts(root) {
                        Ok(result) => content(result, false),
                        // Keep OS paths, private task/Journals and passwords out
                        // of JSON-RPC responses and ordinary model/CLI output.
                        Err(_) => content(json!({"code":"task_store_unavailable"}), true),
                    },
                    _ => content(json!({"code":"unknown_or_unapproved_tool"}), true),
                }
            }
        }
        _ => return Some(error(id, -32601, "method not implemented")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":output}))
}
pub fn serve() -> Result<()> {
    let root = storage::root()?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    loop {
        let mut line = Vec::new();
        let count = (&mut input)
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if count as u64 > MAX_LINE || !line.ends_with(b"\n") {
            bail!("CloudFile Provider request exceeds line limit");
        }
        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => handle(&value, &root),
            Err(_) => Some(error(Value::Null, -32700, "invalid JSON")),
        };
        if let Some(value) = response {
            serde_json::to_writer(&mut output, &value)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_is_safe_and_does_not_expose_file_operations() {
        let root = tempfile::tempdir().unwrap();
        let input = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"cloudfile_status","arguments":{}}});
        let answer = handle(&input, root.path()).unwrap();
        let status: Value =
            serde_json::from_str(answer["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(status["provider_id"], "cloudfile");
        assert_eq!(status["file_actions_exposed"], false);
        assert_eq!(status["file_authorization"], "not_checked");
        for sensitive in [
            "working_copy",
            "token",
            "secret",
            "device_key",
            "local_path",
        ] {
            assert!(status.get(sensitive).is_none());
        }
        let listed = handle(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            root.path(),
        )
        .unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn only_empty_read_only_inputs_are_accepted() {
        let root = tempfile::tempdir().unwrap();
        for (tool, args) in [
            ("cloudfile_status", json!({"path":"/tmp/private"})),
            ("cloudfile_task_counts", json!({"repo_id":"secret"})),
            ("cloudfile_commit", json!({})),
        ] {
            let response = handle(
                &json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":tool,"arguments":args}}),
                root.path(),
            )
            .unwrap();
            assert_eq!(response["result"]["isError"], true);
        }
        assert!(handle(
            &json!({"jsonrpc":"2.0",
            "method":"notifications/initialized"}),
            root.path()
        )
        .is_none());
    }
    #[test]
    fn empty_task_store_is_counted_without_claiming_published() {
        let root = tempfile::tempdir().unwrap();
        let result = counts(root.path()).unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["pending_intents"], 0);
        assert_eq!(result["cloud_publish_verified"], false);
    }
}
