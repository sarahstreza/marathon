//! The remote-control routes (PLAN.md §8, §9 "Remote adapters", M6) over a
//! real socket: clients on `/clients/connect`, actions relayed with
//! `POST /clients/{id}/actions`, and events streamed on `GET /events`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use common::{CHANNELS, TestEnv};
use futures_util::{SinkExt, StreamExt};
use marathon_api::{Action, CLIENT_ID_HEADER, ClientInfo, Event, RelayedEvent};
use marathon_server::Server;
use marathon_server::config::Config;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A server on an OS-assigned loopback port.
struct Running {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: JoinHandle<std::io::Result<()>>,
    http: reqwest::Client,
    token: Option<String>,
    _env: TestEnv,
}

impl Running {
    async fn start(token: Option<&str>) -> Self {
        let env = TestEnv::new();
        let extra = token
            .map(|t| format!("token = \"{t}\""))
            .unwrap_or_default();
        let config = Config::from_toml_str(&env.config_toml(&extra, CHANNELS)).unwrap();
        let server = Server::bind(config).await.unwrap();
        let addr = server.local_addr();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(server.serve(async {
            let _ = stopped.await;
        }));
        Self {
            addr,
            stop: Some(stop),
            task,
            http: reqwest::Client::new(),
            token: token.map(str::to_owned),
            _env: env,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}/api/v1{path}", self.addr)
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.authed(self.http.get(self.url(path)))
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.authed(self.http.post(self.url(path)))
    }

    fn authed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// Opens a control channel as `client`, sending the token if the
    /// server has one.
    async fn connect(&self, client: &str) -> Result<Socket, tungstenite::Error> {
        let mut request = format!("ws://{}/api/v1/clients/connect", self.addr)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(CLIENT_ID_HEADER, client.parse().unwrap());
        if let Some(token) = &self.token {
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        let (socket, _) = tokio_tungstenite::connect_async(request).await?;
        self.wait_for_client(client).await;
        Ok(socket)
    }

    async fn clients(&self) -> Vec<ClientInfo> {
        self.get("/clients")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// Registration happens just after the WebSocket handshake; wait for
    /// it, so the test does not race it.
    async fn wait_for_client(&self, client: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.clients().await.iter().any(|c| c.id == client) {
            assert!(Instant::now() < deadline, "{client} never registered");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// POSTs `body` (as raw text, with no content type, the way a bare
    /// `curl -d` does) to `client`'s actions.
    async fn send_action(&self, client: &str, body: &str) -> (u16, Value) {
        let response = self
            .post(&format!("/clients/{client}/actions"))
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }

    async fn stop(mut self) -> Duration {
        let started = Instant::now();
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), &mut self.task)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
        started.elapsed()
    }
}

/// The next action a control channel receives.
async fn next_action(socket: &mut Socket) -> Action {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an action arrives")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn publish(socket: &mut Socket, event: &Event) {
    let text = serde_json::to_string(event).unwrap();
    socket.send(Message::text(text)).await.unwrap();
}

/// Reads Server-Sent Events from a streaming response.
struct SseReader {
    response: reqwest::Response,
    buffer: String,
}

impl SseReader {
    async fn open(server: &Running) -> Self {
        let response = server.get("/events").send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"].to_str().unwrap(),
            "text/event-stream"
        );
        Self {
            response,
            buffer: String::new(),
        }
    }

    /// The next message with an `event:` field, as (name, data), within
    /// `limit`.
    async fn next(&mut self, limit: Duration) -> (String, RelayedEvent) {
        tokio::time::timeout(limit, async {
            loop {
                while let Some(end) = self.buffer.find("\n\n") {
                    let message: String = self.buffer.drain(..end + 2).collect();
                    let field = |name: &str| {
                        message
                            .lines()
                            .find_map(|l| l.strip_prefix(name))
                            .map(|v| v.trim_start().to_owned())
                    };
                    if let (Some(event), Some(data)) = (field("event:"), field("data:")) {
                        return (event, serde_json::from_str(&data).unwrap());
                    }
                }
                let chunk = self.response.chunk().await.unwrap().expect("stream open");
                self.buffer.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("an event arrives in time")
    }
}

#[tokio::test]
async fn actions_reach_the_named_client() {
    let server = Running::start(None).await;
    let mut tv = server.connect("tv").await.unwrap();

    let (status, body) = server.send_action("tv", r#"{"action":"channel_up"}"#).await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body, json!({"client_id": "tv"}));
    assert_eq!(next_action(&mut tv).await, Action::ChannelUp);

    // Actions with data arrive intact.
    let (status, _) = server
        .send_action("tv", r#"{"action":"seek_relative","seconds":-30}"#)
        .await;
    assert_eq!(status, 202);
    assert_eq!(
        next_action(&mut tv).await,
        Action::SeekRelative { seconds: -30 }
    );

    let clients = server.clients().await;
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].id, "tv");
    assert!(clients[0].is_default);
    server.stop().await;
}

#[tokio::test]
async fn default_targets_the_most_recently_active_client() {
    let server = Running::start(None).await;
    let mut tv = server.connect("tv").await.unwrap();
    let mut phone = server.connect("phone").await.unwrap();

    // The phone connected last.
    let (_, body) = server.send_action("default", r#"{"action":"play"}"#).await;
    assert_eq!(body["client_id"], "phone");
    assert_eq!(next_action(&mut phone).await, Action::Play);

    // The TV does something; now it is the most recently active.
    publish(&mut tv, &Event::Wake).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.clients().await[0].is_default || server.clients().await[0].id != "tv" {
        assert!(Instant::now() < deadline, "the TV's event was not seen");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (_, body) = server.send_action("default", r#"{"action":"pause"}"#).await;
    assert_eq!(body["client_id"], "tv");
    assert_eq!(next_action(&mut tv).await, Action::Pause);
    server.stop().await;
}

#[tokio::test]
async fn unknown_clients_and_malformed_actions_are_rejected() {
    let server = Running::start(None).await;
    // Nobody connected: not even `default` resolves.
    let (status, body) = server.send_action("default", r#"{"action":"play"}"#).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "not_found");

    let _tv = server.connect("tv").await.unwrap();
    let (status, body) = server.send_action("nobody", r#"{"action":"play"}"#).await;
    assert_eq!(status, 404);
    assert!(
        body["message"].as_str().unwrap().contains("nobody"),
        "{body}"
    );

    for (bad, hint) in [
        (
            r#"{"action":"self_destruct"}"#,
            "unknown variant `self_destruct`",
        ),
        (r#"{"action":"go_to_channel"}"#, "missing field `number`"),
        (
            r#"{"action":"navigate","direction":"sideways"}"#,
            "sideways",
        ),
        ("channel_up", "not a valid action"),
        ("", "not a valid action"),
    ] {
        let (status, body) = server.send_action("tv", bad).await;
        assert_eq!(status, 400, "{bad}: {body}");
        assert_eq!(body["code"], "bad_request");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains(hint), "{bad}: {message}");
    }
    // A bad action is a 400 even for an unknown client.
    let (status, _) = server.send_action("nobody", "{}").await;
    assert_eq!(status, 400);
    server.stop().await;
}

#[tokio::test]
async fn events_reach_subscribers_within_a_second() {
    let server = Running::start(None).await;
    let mut tv = server.connect("tv").await.unwrap();
    let mut events = SseReader::open(&server).await;

    let changed = Event::ChannelChanged {
        channel_id: "show".into(),
        channel_number: 1,
        channel_name: "Test Show".into(),
        resolution_path: vec!["show".into()],
    };
    let playing = Event::NowPlaying {
        channel_id: "show".into(),
        channel_number: 1,
        item_id: "item-1".into(),
        title: "Test Show · S01E01 · Pilot".into(),
        position_ms: 0,
        resolution_path: vec!["show".into()],
    };
    for event in [&changed, &playing] {
        let sent = Instant::now();
        publish(&mut tv, event).await;
        let (name, relayed) = events.next(Duration::from_secs(1)).await;
        assert!(sent.elapsed() < Duration::from_secs(1));
        assert_eq!(name, event.name());
        assert_eq!(&relayed.event, event);
        assert_eq!(relayed.client_id, "tv");
        assert!(relayed.ts_ms > 0);
    }

    // `GET /clients` knows what the TV is playing.
    let tv_info = &server.clients().await[0];
    let playing = tv_info.playing.as_ref().unwrap();
    assert_eq!(playing.item_id, "item-1");
    assert_eq!(playing.title, "Test Show · S01E01 · Pilot");

    // Every event type passes through, and garbage is ignored without
    // closing the channel.
    tv.send(Message::text("not json")).await.unwrap();
    for event in Event::examples() {
        publish(&mut tv, &event).await;
        let (name, relayed) = events.next(Duration::from_secs(1)).await;
        assert_eq!(name, event.name());
        assert_eq!(relayed.event, event);
    }
    server.stop().await;
}

#[tokio::test]
async fn a_reconnecting_client_replaces_its_old_connection() {
    let server = Running::start(None).await;
    let mut old = server.connect("tv").await.unwrap();
    let mut new = server.connect("tv").await.unwrap();
    assert_eq!(server.clients().await.len(), 1);
    server.send_action("tv", r#"{"action":"back"}"#).await;
    assert_eq!(next_action(&mut new).await, Action::Back);
    // The old connection is closed by the server.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match old.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the replaced connection is closed");
    server.stop().await;
}

#[tokio::test]
async fn the_control_channel_needs_a_client_id_and_a_websocket() {
    let server = Running::start(None).await;
    // No client ID.
    let request = format!("ws://{}/api/v1/clients/connect", server.addr)
        .into_client_request()
        .unwrap();
    let err = tokio_tungstenite::connect_async(request).await.unwrap_err();
    assert!(
        matches!(&err, tungstenite::Error::Http(r) if r.status() == 400),
        "{err}"
    );
    // `default` is reserved.
    let err = server.connect("default").await.unwrap_err();
    assert!(
        matches!(&err, tungstenite::Error::Http(r) if r.status() == 400),
        "{err}"
    );
    // A plain GET is not a WebSocket.
    let response = server
        .get("/clients/connect")
        .header(CLIENT_ID_HEADER, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    server.stop().await;
}

#[tokio::test]
async fn control_routes_require_the_token() {
    let server = Running::start(Some("sesame")).await;
    let anonymous = reqwest::Client::new();

    // Without the token: refused, every route.
    let mut request = format!("ws://{}/api/v1/clients/connect", server.addr)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(CLIENT_ID_HEADER, "tv".parse().unwrap());
    let err = tokio_tungstenite::connect_async(request).await.unwrap_err();
    assert!(
        matches!(&err, tungstenite::Error::Http(r) if r.status() == 401),
        "{err}"
    );
    for request in [
        anonymous.get(server.url("/clients")),
        anonymous.get(server.url("/events")),
        anonymous
            .post(server.url("/clients/default/actions"))
            .body(r#"{"action":"play"}"#),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401, "{}", response.url());
    }

    // With it: everything works.
    let mut tv = server.connect("tv").await.unwrap();
    let mut events = SseReader::open(&server).await;
    let (status, _) = server.send_action("tv", r#"{"action":"play"}"#).await;
    assert_eq!(status, 202);
    assert_eq!(next_action(&mut tv).await, Action::Play);
    publish(&mut tv, &Event::Standby).await;
    assert_eq!(events.next(Duration::from_secs(1)).await.0, "standby");
    server.stop().await;
}

/// Control channels and event streams stay open indefinitely. Shutting the
/// server down closes them, rather than waiting out the shutdown grace
/// period (5s by default) for them.
#[tokio::test]
async fn shutdown_closes_control_channels_and_event_streams() {
    let server = Running::start(None).await;
    let mut tv = server.connect("tv").await.unwrap();
    let mut events = SseReader::open(&server).await;
    let took = server.stop().await;
    assert!(took < Duration::from_secs(2), "took {took:?}");

    let close = tokio::time::timeout(Duration::from_secs(5), tv.next())
        .await
        .unwrap();
    assert!(
        matches!(&close, Some(Ok(Message::Close(Some(frame)))) if frame.reason == "server shutting down"),
        "{close:?}"
    );
    let end = tokio::time::timeout(Duration::from_secs(5), events.response.chunk())
        .await
        .unwrap();
    assert!(matches!(end, Ok(None) | Err(_)), "{end:?}");
}
