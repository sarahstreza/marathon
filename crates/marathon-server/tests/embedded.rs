//! `EmbeddedServer`: the server running in-process on its own thread, as
//! the desktop client's `--embedded` mode uses it (PLAN.md M4).
//!
//! These tests are deliberately synchronous, like the desktop client that
//! hosts the server: no Tokio runtime of their own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

use common::TestEnv;
use marathon_server::config::Entry;
use marathon_server::embedded::{EMBEDDED_BIND, EmbeddedError, EmbeddedServer};
use marathon_server::{DATABASE_FILE, StartError};

/// Sends a bare HTTP/1.1 request and returns the raw response.
fn request(addr: SocketAddr, method: &str, path: &str, headers: &str, body: &str) -> String {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn get(addr: SocketAddr, path: &str) -> String {
    request(addr, "GET", path, "", "")
}

fn json_body(response: &str) -> serde_json::Value {
    let body = response.split_once("\r\n\r\n").unwrap().1;
    serde_json::from_str(body).unwrap_or_else(|err| panic!("{err}: {response}"))
}

fn assert_port_closed(addr: SocketAddr) {
    assert!(
        TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err(),
        "{addr} still accepts connections"
    );
}

#[test]
fn starts_on_an_os_assigned_port_and_answers_api_calls() {
    let env = TestEnv::new();
    let server = EmbeddedServer::start(env.config()).unwrap();

    let addr = server.local_addr();
    assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_ne!(addr.port(), 0, "the OS assigned a real port");
    assert_eq!(
        server.base_url(),
        format!("http://127.0.0.1:{}", addr.port())
    );

    let response = get(addr, "/api/v1/channels");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let channels = json_body(&response);
    let names: Vec<&str> = channels
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Test Show", "Essays", "Seasons", "Misc", "Empty"]);

    server.shutdown().unwrap();
    assert_port_closed(addr);
}

/// A non-loopback address of this machine, if it has one: the local end of
/// a UDP "connection" towards a documentation address (no packets are
/// sent).
fn non_loopback_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

#[test]
fn binds_only_to_127_0_0_1_whatever_the_config_says() {
    let env = TestEnv::new();
    let mut config = env.config();
    // A standalone-server config that listens on every interface.
    let configured: SocketAddr = "0.0.0.0:7979".parse().unwrap();
    config.server.bind = configured;
    config.server.token = Some("sesame".into());
    let server = EmbeddedServer::start(config).unwrap();

    let addr = server.local_addr();
    assert_eq!(EMBEDDED_BIND.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(addr.ip(), EMBEDDED_BIND.ip());
    assert_ne!(addr.port(), configured.port(), "the port is OS-assigned");

    // Not reachable on the machine's network address.
    match non_loopback_ip() {
        Some(ip) => assert_port_closed(SocketAddr::new(ip, addr.port())),
        None => eprintln!("no non-loopback address; skipping the network reachability check"),
    }

    // The configured token is still required.
    let response = get(addr, "/api/v1/channels");
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    let response = request(
        addr,
        "GET",
        "/api/v1/channels",
        "Authorization: Bearer sesame\r\n",
        "",
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

/// Quitting the desktop app must leave nothing behind: the port is free,
/// SQLite has been closed cleanly (its write-ahead log checkpointed and
/// removed, so no lock or pending state lingers), and the next start sees
/// the saved state.
#[test]
fn shutdown_releases_the_port_and_closes_the_database() {
    let env = TestEnv::new();
    let server = EmbeddedServer::start(env.config()).unwrap();
    let addr = server.local_addr();

    let now = json_body(&get(addr, "/api/v1/channels/show/now"));
    let item_id = now["item"]["id"].as_str().unwrap().to_owned();
    let progress = format!(r#"{{"item_id":"{item_id}","position_ms":90000,"paused":false}}"#);
    let response = request(
        addr,
        "POST",
        "/api/v1/channels/show/progress",
        "Content-Type: application/json\r\n",
        &progress,
    );
    assert!(response.starts_with("HTTP/1.1 2"), "{response}");
    let wal = env.data_dir().join(format!("{DATABASE_FILE}-wal"));
    assert!(
        wal.exists(),
        "the database is open in WAL mode while serving"
    );

    server.shutdown().unwrap();
    assert_port_closed(addr);
    assert!(
        !wal.exists(),
        "closing the database checkpoints and removes the WAL"
    );

    // Same data_dir, new server: the state survived and nothing is locked.
    let again = EmbeddedServer::start(env.config()).unwrap();
    let now = json_body(&get(again.local_addr(), "/api/v1/channels/show/now"));
    assert_eq!(now["item"]["id"], item_id.as_str());
    assert_eq!(now["position_ms"], 90_000);
}

#[test]
fn dropping_the_server_shuts_it_down() {
    let env = TestEnv::new();
    let server = EmbeddedServer::start(env.config()).unwrap();
    let addr = server.local_addr();
    drop(server);
    assert_port_closed(addr);
}

#[test]
fn a_bad_config_fails_to_start_with_the_reason() {
    let env = TestEnv::new();
    let mut config = env.config();
    let Some(Entry::Items(selector)) = config.channels[0].items.get_mut(0) else {
        panic!("the test config's first entry is a selector");
    };
    selector.source = "nowhere".into();
    let err = EmbeddedServer::start(config).unwrap_err();
    assert!(
        matches!(err, EmbeddedError::Start(StartError::Config(_))),
        "{err:?}"
    );
    assert!(err.to_string().contains("unknown source"), "{err}");
}
