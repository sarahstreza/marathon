//! The server end to end over a real socket: bind to an OS-assigned port,
//! serve, answer HTTP, and shut down cleanly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::TestEnv;
use marathon_server::Server;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Sends a bare HTTP/1.1 request and returns the raw response.
async fn raw_request(addr: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn serves_over_a_real_port_and_shuts_down() {
    let env = TestEnv::new();
    let server = Server::bind(env.config()).await.unwrap();
    let addr = server.local_addr();
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0, "OS assigns a real port");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.serve(async {
        let _ = stopped.await;
    }));

    let response = raw_request(
        addr,
        "GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with(r#"{"status":"ok"}"#), "{response}");

    let response = raw_request(
        addr,
        "GET /api/v1/channels/show/now HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("Test Show - S01E01 - Pilot.mkv"),
        "{response}"
    );

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("server stops promptly")
        .unwrap()
        .unwrap();
    // The port is released.
    assert!(TcpStream::connect(addr).await.is_err());
    // The database was created in data_dir and is reusable after shutdown.
    assert!(env.data_dir().join(marathon_server::DATABASE_FILE).exists());
    let again = Server::bind(env.config()).await.unwrap();
    drop(again);
}
