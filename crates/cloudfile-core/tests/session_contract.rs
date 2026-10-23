// V2 local edit has no server-side claim or handoff file.
use cloudfile_core::{session, storage};
use serde_json::json;
use std::io::{Read, Write};

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut head = Vec::new();
    let mut byte = [0];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
        assert!(head.len() < 16384);
    }
    let text = String::from_utf8(head).unwrap();
    let length = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if length > 0 {
        stream.read_exact(&mut body).unwrap();
    }
    text + &String::from_utf8_lossy(&body)
}
#[test]
fn singleton_lock_and_interrupted_mailbox() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let lock = storage::lock(&root.join("desktop.lock")).unwrap();
    assert!(storage::lock(&root.join("desktop.lock")).is_err());
    drop(lock);
    assert!(storage::lock(&root.join("desktop.lock")).is_ok());
    let path = root
        .join("ipc")
        .join(format!("{}.processing", uuid::Uuid::new_v4()));
    storage::write(&path, &json!({"ticket":"secret"})).unwrap();
    cloudfile_core::ipc::recover(&root).unwrap();
    assert!(!path.exists());
    let value: serde_json::Value = storage::read(&path.with_extension("response")).unwrap();
    assert_eq!(value["ok"], false);
    assert!(!value.to_string().contains("secret"));
}

#[test]
fn service_address_accepts_deployment_path_without_changing_trust_origin() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    session::trust(&root, "https://files.example.com/seafile/".into()).unwrap();
    assert_eq!(
        session::config(&root).unwrap().allowed_origins,
        vec!["https://files.example.com"]
    );
    assert_eq!(
        session::service_path(&root, "https://files.example.com").unwrap(),
        "/seafile"
    );
    for raw in [
        "https://files.example.com/a/../b",
        "https://files.example.com/seafile?token=x",
        "https://files.example.com//seafile",
        "https://files.example.com/%2e%2e",
    ] {
        assert!(session::trust(&root, raw.into()).is_err());
    }
}
#[test]
fn disabled_edit_capability_is_reported_before_pairing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    session::trust(&root, format!("{origin}/seafile")).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(request.contains("GET /seafile/api/v2.1/cloudfile/capabilities/"));
        let body = r#"{"capabilities":{"local.open-edit":{"enabled":false}}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let error = cloudfile_core::device::preflight(&root, &origin)
        .unwrap_err()
        .to_string();
    assert!(error.contains("尚未启用"));
    assert!(!root.join("pairings").exists());
    worker.join().unwrap();
}

#[test]
fn adding_origin_again_preserves_the_paired_service_deployment_path() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    session::trust(&root, "https://files.example.com/seafile/".into()).unwrap();
    session::trust(&root, "https://files.example.com".into()).unwrap();
    assert_eq!(
        session::service_path(&root, "https://files.example.com").unwrap(),
        "/seafile"
    );
    session::trust(&root, "https://files.example.com/other".into()).unwrap();
    assert_eq!(
        session::service_path(&root, "https://files.example.com").unwrap(),
        "/other"
    );
}

#[test]
fn retired_v1_uri_never_claims_a_server_session() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let uri = "cloudfile-open://v1/open?instance=https%3A%2F%2Fexample.com&session=11111111-1111-4111-8111-111111111111&ticket=not-v2";
    assert!(cloudfile_core::paired_session::open(&root, uri).is_err());
    assert!(session::tasks(&root).unwrap().is_empty());
}
