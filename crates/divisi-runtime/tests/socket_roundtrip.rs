//! End-to-end: starts the real Unix socket server against a temp socket
//! path and temp `SINGLE_CONFIG_DIR`, then drives it exactly the way
//! `divisi-cli`'s client does — proving the runtime/CLI IPC boundary
//! actually works, not just the in-process handler function.

use divisi_protocol::{Request, Response, ResponseData};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn send(socket_path: &std::path::Path, request: &Request) -> Response {
    let mut stream = UnixStream::connect(socket_path).expect("connect to test runtime socket");
    let mut payload = serde_json::to_string(request).unwrap();
    payload.push('\n');
    stream.write_all(payload.as_bytes()).unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(line.trim_end()).unwrap()
}

// The server loop runs forever and the client side below uses blocking
// std::os::unix::net calls (deliberately — it mirrors divisi-cli's real
// client exactly). On tokio's default current-thread test runtime, those
// blocking calls would starve the spawned server task and deadlock the
// test; multi_thread gives the server its own OS thread to make progress on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_and_agent_list_round_trip_over_the_socket() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
    let socket_path = dir.path().join("state").join("runtime.sock");

    let serve_socket = socket_path.clone();
    tokio::spawn(async move {
        divisi_runtime::server::serve(&serve_socket).await.unwrap();
    });

    // Wait for the listener to come up rather than a fixed sleep guess.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !socket_path.exists() {
        if std::time::Instant::now() > deadline {
            panic!("runtime socket never appeared at {}", socket_path.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let status_response = tokio::task::spawn_blocking({
        let socket_path = socket_path.clone();
        move || send(&socket_path, &Request::Status)
    })
    .await
    .unwrap();
    let Response::Ok { data: ResponseData::Status(status) } = status_response else {
        panic!("expected Ok(Status), got something else");
    };
    assert!(status.agents_known >= 22, "expected at least 22 known agents, got {}", status.agents_known);

    let agents_response = send(&socket_path, &Request::AgentList);
    let Response::Ok { data: ResponseData::Agents(agents) } = agents_response else {
        panic!("expected Ok(Agents)");
    };
    assert_eq!(agents.len(), status.agents_known);
    // perplexity's only real CLI (`pplx`) is a Search API client, not a
    // coding agent — see docs/install-methods.md. Flagged via `notes`
    // rather than `unverified`, since the install method itself IS verified.
    assert!(agents.iter().any(|a| a.name == "perplexity" && a.notes.is_some()));

    // A malformed request should come back as a structured error, not kill the connection.
    let mut stream = UnixStream::connect(&socket_path).unwrap();
    stream.write_all(b"{ not json\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: Response = serde_json::from_str(line.trim_end()).unwrap();
    assert!(matches!(response, Response::Error { .. }));
}
