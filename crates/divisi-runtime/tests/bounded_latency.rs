//! Regression: the daemon must answer `Status` and `AgentList` within a bounded time
//! (E27/01 P1 hang), including while another connection is held open and idle.

use divisi_protocol::{Request, Response, ResponseData};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(30);

fn send_bounded(socket: &std::path::Path, request: &Request) -> Response {
    let mut stream = UnixStream::connect(socket).expect("connect");
    stream.set_read_timeout(Some(BOUND)).unwrap();
    let mut payload = serde_json::to_string(request).unwrap();
    payload.push('\n');
    stream.write_all(payload.as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("daemon did not answer within the bound");
    serde_json::from_str(line.trim_end()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_and_agent_list_answer_within_bound_even_with_an_idle_connection_open() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
    let socket = dir.path().join("state").join("runtime.sock");
    let serve_socket = socket.clone();
    tokio::spawn(async move { divisi_runtime::server::serve(&serve_socket).await.unwrap() });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "runtime socket never appeared");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A client that connects and says nothing must not block other callers.
    let _idle = UnixStream::connect(&socket).unwrap();

    for request in [Request::Status, Request::AgentList] {
        let socket = socket.clone();
        let start = Instant::now();
        let response = tokio::task::spawn_blocking(move || send_bounded(&socket, &request)).await.unwrap();
        assert!(start.elapsed() < BOUND);
        assert!(
            matches!(
                response,
                Response::Ok { data: ResponseData::Status(_) | ResponseData::Agents(_) }
            ),
            "unexpected response"
        );
    }
}
