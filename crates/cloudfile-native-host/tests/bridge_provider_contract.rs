//! A local-only contract test using synthetic MCP messages and a sandboxed
//! user profile. It never logs in, executes a file task, or touches Chrome.
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn bridge_mcp_provider_is_read_only_and_uses_json_lines() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let local = home.join("AppData/Local");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(home.join("Library/Application Support")).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_cloudfile-native-host"))
        .arg("--bridge-provider")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("LOCALAPPDATA", &local)
        .env("CLOUDFILE_AGENT2_HOME", home.join("CloudFileAgent"))
        .env("CLOUDFILE_AGENT2_NO_DRIVE_PREFERENCE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"local-bridge","version":"0.1.0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"cloudfile_status","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"cloudfile_task_counts","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"cloudfile_task_summaries","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
            "params":{"name":"cloudfile_commit","arguments":{"task_id":"synthetic"}}}),
    ];
    let mut input = child.stdin.take().unwrap();
    for request in requests {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8(output.stdout).unwrap();
    let answers: Vec<Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(answers.len(), 6, "notification must not receive a reply");
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(answers[1]["result"]["tools"].as_array().unwrap().len(), 3);
    for answer in answers.iter().take(5) {
        assert_eq!(answer["jsonrpc"], "2.0");
        assert!(answer.get("error").is_none());
    }
    let status: Value =
        serde_json::from_str(answers[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["agent_family"], "agent2");
    assert_eq!(status["file_actions_exposed"], false);
    assert_eq!(status["storage_scope"], "agent2_shared");
    assert_eq!(status["data_owner"], "cloudfile-agent2");
    let counts: Value =
        serde_json::from_str(answers[3]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(counts["total"], 0);
    assert_eq!(counts["pending_intents"], 0);
    assert_eq!(counts["shared_file_tasks"], true);
    assert!(home.join("CloudFileAgent").is_dir());
    assert!(!home.join("CloudFileBridgeProvider").exists());
    let summaries: Value =
        serde_json::from_str(answers[4]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(summaries["total"], 0);
    assert_eq!(summaries["storage_scope"], "agent2_shared");
    assert_eq!(summaries["read_only"], true);
    assert_eq!(summaries["items"], json!([]));
    assert_eq!(answers[5]["result"]["isError"], true);
    assert!(!body.contains(home.to_string_lossy().as_ref()));
    assert!(!body.contains("device_key"));
    assert!(!body.contains("access_token"));
}
