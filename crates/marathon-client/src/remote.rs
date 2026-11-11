//! Remote control (PLAN.md §9 "Remote adapters", M6): the client's end of
//! its control channel to the server. Behind the `remote` feature.
//!
//! [`RemoteSource`] is an [`InputSource`] like the keyboard, except that its
//! actions come from the network: it keeps a WebSocket open to the server's
//! `/clients/connect` and puts every [`Action`] the server relays (from
//! `POST /clients/{id}/actions`) on the action bus. In the other direction,
//! the [`EventPublisher`] it hands out sends the client's [`Event`]s up the
//! same socket, for the server to republish on `GET /events`.
//!
//! ```text
//! curl, home automation, ... ─► server ─WebSocket─► RemoteSource ─► action bus
//!            observers ◄─ GET /events ◄─ server ◄─WebSocket─ EventPublisher ◄─ tuner
//! ```
//!
//! The connection is kept up for as long as the source runs. If it cannot
//! connect, or the connection drops (the server restarted, the network
//! went away), it reconnects with exponential [`Backoff`], without anyone
//! having to intervene. Events published while disconnected are not
//! queued, since observers only care about what is happening now. Instead,
//! on every (re)connect the source re-sends the events that describe the
//! client's current state (what it is playing, whether it is paused or in
//! standby), so the server's `GET /clients` is right again straight away.
//!
//! The socket runs on its own thread with a small single-threaded Tokio
//! runtime, so the rest of the client stays synchronous.

use std::thread::JoinHandle;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use marathon_api::{Action, Event};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::api::HttpApi;
use crate::bus::ActionSender;
use crate::input::{InputError, InputSource};

/// The name the source's actions carry on the bus.
pub const SOURCE_NAME: &str = "remote";

/// How long to wait for the server to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often to ping the server, and how long without hearing anything
/// back before the connection is presumed dead. A server that restarts
/// closes its sockets, which is noticed at once; this catches the network
/// silently going away.
const PING_EVERY: Duration = Duration::from_secs(15);
const DEAD_AFTER: Duration = Duration::from_secs(45);

/// Exponential backoff between connection attempts: `initial`, doubling
/// up to `max`, and back to `initial` once a connection succeeds.
///
/// There is no jitter: a household has a handful of clients, not enough
/// for reconnecting at the same moment to matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    last: Option<Duration>,
}

impl Default for Backoff {
    /// 0.5s, doubling to at most 30s.
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(30))
    }
}

impl Backoff {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            last: None,
        }
    }

    /// How long to wait before the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        let delay = match self.last {
            None => self.initial,
            Some(last) => last.saturating_mul(2).min(self.max),
        };
        self.last = Some(delay);
        delay
    }

    /// Starts over from `initial`, after a successful connection.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Where and how to connect.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// The control channel's `ws://` or `wss://` URL.
    pub url: String,
    /// Headers for the handshake: the client ID, and the token if any.
    pub headers: Vec<(String, String)>,
    pub backoff: Backoff,
}

impl RemoteConfig {
    /// The control channel of the server `api` talks to, identifying as
    /// the same client with the same token.
    pub fn for_api(api: &HttpApi) -> Self {
        Self {
            url: api.control_url(),
            headers: api.media_headers().to_vec(),
            backoff: Backoff::default(),
        }
    }
}

/// Sends the client's events up the control channel. Clone freely; never
/// blocks.
#[derive(Debug, Clone)]
pub struct EventPublisher {
    tx: mpsc::UnboundedSender<Event>,
}

impl EventPublisher {
    /// Publishes `event`, if connected (see the module docs). Does nothing
    /// once the source has stopped.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

/// The remote-control [`InputSource`]. See the module docs.
pub struct RemoteSource {
    config: RemoteConfig,
    events: Option<mpsc::UnboundedReceiver<Event>>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteSource {
    /// A source for `config`, and the publisher for the client's events.
    /// Nothing connects until the source is started.
    pub fn new(config: RemoteConfig) -> (Self, EventPublisher) {
        let (tx, rx) = mpsc::unbounded_channel();
        let source = Self {
            config,
            events: Some(rx),
            stop: None,
            thread: None,
        };
        (source, EventPublisher { tx })
    }
}

impl InputSource for RemoteSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    fn start(&mut self, actions: ActionSender) -> Result<(), InputError> {
        let error = |message: String| InputError {
            source_name: SOURCE_NAME.to_owned(),
            message,
        };
        let events = self
            .events
            .take()
            .ok_or_else(|| error("already started".to_owned()))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| error(format!("cannot start its runtime: {e}")))?;
        let (stop, stopped) = oneshot::channel();
        let config = self.config.clone();
        let thread = std::thread::Builder::new()
            .name("remote".into())
            .spawn(move || runtime.block_on(run(config, events, actions, stopped)))
            .map_err(|e| error(format!("cannot start its thread: {e}")))?;
        self.stop = Some(stop);
        self.thread = Some(thread);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("remote control thread panicked");
        }
    }
}

impl Drop for RemoteSource {
    fn drop(&mut self) {
        self.stop();
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Why a connection ended.
enum End {
    /// Asked to stop, or the bus closed: the source is done.
    Done,
    /// The connection was lost; reconnect.
    Lost(String),
}

/// The client's current state, as the events that describe it.
#[derive(Debug, Default)]
struct Replay {
    playing: Option<Event>,
    paused: Option<Event>,
    standby: bool,
}

impl Replay {
    fn observe(&mut self, event: &Event) {
        match event {
            Event::NowPlaying { .. } => {
                self.playing = Some(event.clone());
                self.paused = None;
                self.standby = false;
            }
            Event::Paused { .. } => self.paused = Some(event.clone()),
            Event::Resumed { .. } => self.paused = None,
            Event::Standby => self.standby = true,
            Event::Wake => self.standby = false,
            Event::ChannelChanged { .. } | Event::ItemEnded { .. } | Event::Error { .. } => {}
        }
    }

    /// What to send after connecting, in order.
    fn events(&self) -> Vec<Event> {
        let mut events: Vec<Event> = self.playing.iter().chain(&self.paused).cloned().collect();
        if self.standby {
            events.push(Event::Standby);
        }
        events
    }
}

/// Connects, relays, and reconnects until stopped.
async fn run(
    config: RemoteConfig,
    mut events: mpsc::UnboundedReceiver<Event>,
    actions: ActionSender,
    mut stop: oneshot::Receiver<()>,
) {
    let mut replay = Replay::default();
    let mut backoff = config.backoff.clone();
    let mut failures = 0u32;
    loop {
        // Keep tracking the client's state while connecting.
        let connect = connect(&config);
        tokio::pin!(connect);
        let connected = loop {
            tokio::select! {
                _ = &mut stop => return,
                Some(event) = events.recv() => replay.observe(&event),
                result = &mut connect => break result,
            }
        };
        match connected {
            Ok(socket) => {
                tracing::info!(url = %config.url, "remote control connected");
                backoff.reset();
                failures = 0;
                match session(socket, &mut events, &actions, &mut stop, &mut replay).await {
                    End::Done => return,
                    End::Lost(reason) => {
                        tracing::warn!(%reason, "remote control disconnected; reconnecting");
                    }
                }
            }
            Err(err) => {
                failures += 1;
                // Say so once; a server that stays down should not fill
                // the log.
                if failures == 1 {
                    tracing::warn!(url = %config.url, %err, "cannot connect for remote control; will keep trying");
                } else {
                    tracing::debug!(%err, failures, "remote control still cannot connect");
                }
            }
        }
        let wait = tokio::time::sleep(backoff.next_delay());
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut stop => return,
                Some(event) = events.recv() => replay.observe(&event),
                () = &mut wait => break,
            }
        }
    }
}

async fn connect(config: &RemoteConfig) -> Result<Socket, String> {
    let mut request = config
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| e.to_string())?;
    for (name, value) in &config.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?;
        let mut value = HeaderValue::from_str(value).map_err(|e| e.to_string())?;
        value.set_sensitive(name == "authorization");
        request.headers_mut().insert(name, value);
    }
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)).await {
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(tungstenite::Error::Http(response))) => Err(format!(
            "the server refused the connection ({})",
            response.status()
        )),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("timed out".to_owned()),
    }
}

/// Relays over one connection until it ends.
async fn session(
    socket: Socket,
    events: &mut mpsc::UnboundedReceiver<Event>,
    actions: &ActionSender,
    stop: &mut oneshot::Receiver<()>,
    replay: &mut Replay,
) -> End {
    let (mut sink, mut stream) = socket.split();
    for event in replay.events() {
        if let Err(err) = send_event(&mut sink, &event).await {
            return End::Lost(err);
        }
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            _ = &mut *stop => {
                let _ = sink.send(Message::Close(None)).await;
                return End::Done;
            }
            message = stream.next() => {
                last_heard = Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<Action>(&text) {
                        Ok(action) => {
                            tracing::info!(action = action.name(), "remote action");
                            if actions.send(action).is_err() {
                                let _ = sink.send(Message::Close(None)).await;
                                return End::Done;
                            }
                        }
                        // A newer server may know actions this client
                        // does not; skip them rather than disconnect.
                        Err(err) => tracing::warn!(%err, "ignoring an action this client does not understand"),
                    },
                    Some(Ok(Message::Close(frame))) => {
                        let reason = frame
                            .map(|f| f.reason.to_string())
                            .filter(|r| !r.is_empty())
                            .unwrap_or_else(|| "no reason given".to_owned());
                        return End::Lost(format!("closed by the server: {reason}"));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return End::Lost(err.to_string()),
                    None => return End::Lost("connection closed".to_owned()),
                }
            }
            Some(event) = events.recv() => {
                replay.observe(&event);
                if let Err(err) = send_event(&mut sink, &event).await {
                    return End::Lost(err);
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > DEAD_AFTER {
                    return End::Lost("the server stopped answering".to_owned());
                }
                if let Err(err) = sink.send(Message::Ping(Default::default())).await {
                    return End::Lost(err.to_string());
                }
            }
        }
    }
}

async fn send_event(
    sink: &mut futures_util::stream::SplitSink<Socket, Message>,
    event: &Event,
) -> Result<(), String> {
    let text = serde_json::to_string(event).map_err(|e| e.to_string())?;
    sink.send(Message::text(text))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap_and_resets() {
        let ms = Duration::from_millis;
        let mut backoff = Backoff::new(ms(100), ms(1000));
        let delays: Vec<_> = (0..6).map(|_| backoff.next_delay()).collect();
        assert_eq!(
            delays,
            [ms(100), ms(200), ms(400), ms(800), ms(1000), ms(1000)]
        );
        backoff.reset();
        assert_eq!(backoff.next_delay(), ms(100));
    }

    #[test]
    fn default_backoff_is_half_a_second_to_thirty() {
        let mut backoff = Backoff::default();
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
        let last = (0..20).map(|_| backoff.next_delay()).last().unwrap();
        assert_eq!(last, Duration::from_secs(30));
    }

    fn playing(item: &str) -> Event {
        Event::NowPlaying {
            channel_id: "c".into(),
            channel_number: 1,
            item_id: item.into(),
            title: "T".into(),
            position_ms: 0,
            resolution_path: vec!["c".into()],
        }
    }

    fn paused(item: &str) -> Event {
        Event::Paused {
            channel_id: "c".into(),
            item_id: item.into(),
            position_ms: 7,
        }
    }

    #[test]
    fn replay_describes_the_current_state() {
        let mut replay = Replay::default();
        assert!(replay.events().is_empty());
        replay.observe(&playing("a"));
        replay.observe(&paused("a"));
        assert_eq!(replay.events(), [playing("a"), paused("a")]);
        replay.observe(&Event::Resumed {
            channel_id: "c".into(),
            item_id: "a".into(),
            position_ms: 8,
        });
        assert_eq!(replay.events(), [playing("a")]);
        replay.observe(&paused("a"));
        replay.observe(&playing("b"));
        assert_eq!(replay.events(), [playing("b")]);
        replay.observe(&Event::Standby);
        replay.observe(&Event::Error {
            message: "x".into(),
        });
        assert_eq!(replay.events(), [playing("b"), Event::Standby]);
        replay.observe(&Event::Wake);
        assert_eq!(replay.events(), [playing("b")]);
    }

    #[test]
    fn starts_only_once_and_stops_promptly_when_nothing_answers() {
        let (bus, _rx) = crate::bus::ActionBus::channel();
        let (mut source, publisher) = RemoteSource::new(RemoteConfig {
            // Port 1 on loopback refuses connections.
            url: "ws://127.0.0.1:1/api/v1/clients/connect".into(),
            headers: Vec::new(),
            backoff: Backoff::new(Duration::from_millis(10), Duration::from_millis(50)),
        });
        source.start(bus.sender(SOURCE_NAME)).unwrap();
        assert!(source.start(bus.sender(SOURCE_NAME)).is_err());
        publisher.publish(Event::Wake);
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        source.stop();
        assert!(started.elapsed() < Duration::from_secs(2));
        // Publishing after the source stopped is harmless.
        publisher.publish(Event::Standby);
    }
}
