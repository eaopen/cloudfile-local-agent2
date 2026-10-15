//! Browser first-local-edit trust decision: the page is already the
//! authenticated user, but the Agent only trusts an address after the server
//! proves it is a real CloudFile instance. No credential is created here.
use cloudfile_core::{device, session};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

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
    String::from_utf8(head).unwrap()
}

/// A CloudFile instance at `/seafile` that answers the capability document and
/// exposes the device management route (GET is 405), exactly like the Hub.
fn verified_instance(capability: bool) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(request.contains("GET /seafile/api/v2.1/cloudfile/capabilities/"));
        let body = if capability {
            r#"{"capabilities":{"local.open-edit":{"enabled":true}}}"#
        } else {
            r#"{"capabilities":{"local.open-edit":{"enabled":false}}}"#
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        drop(stream);
        if capability {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(request.contains(
                "GET /seafile/api/v2.1/cloudfile/extensions/local-edit/v1/devices/pair-start/"
            ));
            write!(
                stream,
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
    });
    (server, handle)
}

#[test]
fn first_local_edit_verifies_then_trusts_the_instance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (server, handle) = verified_instance(true);
    device::verify_and_trust(&root, &server, "/seafile").unwrap();
    handle.join().unwrap();
    assert_eq!(
        session::config(&root).unwrap().allowed_origins,
        vec![server.clone()]
    );
    assert_eq!(session::service_path(&root, &server).unwrap(), "/seafile");
    // Trust is earned by the probe alone; the credential is still the page's
    // pair-start/pair-confirm handshake.
    assert!(!root.join("pairings").exists());
}

#[test]
fn a_service_without_the_capability_is_never_trusted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (server, handle) = verified_instance(false);
    let error = device::verify_and_trust(&root, &server, "/seafile")
        .unwrap_err()
        .to_string();
    handle.join().unwrap();
    assert!(error.contains("尚未启用"));
    assert!(session::config(&root).unwrap().allowed_origins.is_empty());
    assert!(!root.join("pairings").exists());
}

#[test]
fn an_established_deployment_path_cannot_be_moved_by_a_page() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    session::trust(&root, "https://files.example.com/seafile".into()).unwrap();
    let error = device::verify_and_trust(&root, "https://files.example.com", "/other")
        .unwrap_err()
        .to_string();
    assert!(error.contains("其他部署路径"));
    assert_eq!(
        session::service_path(&root, "https://files.example.com").unwrap(),
        "/seafile"
    );
}

#[test]
fn invalid_addresses_are_rejected_before_any_network() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for (server, path) in [
        ("https://example.com/a/../b", ""),
        ("https://example.com", "/seafile/"),
        ("https://example.com", "/a//b"),
    ] {
        assert!(device::verify_and_trust(&root, server, path).is_err());
        assert!(device::prepare(&root, server, path).is_err());
    }
    assert!(session::config(&root).unwrap().allowed_origins.is_empty());
    assert!(!root.join("pairings").exists());
}
