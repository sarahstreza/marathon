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

/// A player streaming a long video keeps its connection open, and graceful
/// shutdown would wait for it forever. The server gives such connections
/// a grace period and then closes them, so quitting (or an embedded
/// server stopping) never hangs.
#[tokio::test]
async fn shutdown_does_not_wait_forever_for_an_open_stream() {
    let env = TestEnv::new();
    // Big enough that the response cannot fit in the socket buffers.
    let misc = env
        .library()
        .join("Misc/Café Society's Ünïcode Clip — Ørsted.mkv");
    std::fs::File::options()
        .write(true)
        .open(&misc)
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    let server = Server::bind(env.config())
        .await
        .unwrap()
        .shutdown_grace(Duration::from_millis(200));
    let addr = server.local_addr();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.serve(async {
        let _ = stopped.await;
    }));

    let now = raw_request(
        addr,
        "GET /api/v1/channels/misc/now HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    let body = now.split("\r\n\r\n").nth(1).unwrap();
    let now: serde_json::Value = serde_json::from_str(body).unwrap();
    let id = now["item"]["id"].as_str().unwrap();

    // Start streaming, read a little, and then stall like a paused player.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET /api/v1/stream/{id} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await.unwrap();
    assert!(buf[..n].starts_with(b"HTTP/1.1 200"));

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("server stops after the grace period despite the open stream")
        .unwrap()
        .unwrap();
    assert!(TcpStream::connect(addr).await.is_err());
    drop(stream);
}
