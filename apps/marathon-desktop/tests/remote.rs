//! Remote control end to end (M6): a headless client (the real
//! `RemoteSource` and `Tuner`, with a mock player) connected to a real
//! server, driven by `POST /clients/{id}/actions`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use common::{Recorder, TestServer};
use marathon_api::{Action, ClientInfo, Event};
use marathon_client::api::HttpApi;
use marathon_client::bus::{ActionBus, SourcedAction};
use marathon_client::input::InputSources;
use marathon_client::keyboard::{Key, Keyboard};
use marathon_client::remote::{Backoff, EventPublisher, RemoteConfig, RemoteSource};
use marathon_client::tuner::{SystemClock, Tuner, TunerConfig};
use marathon_server::Server;
use marathon_server::config::Config;

/// Quick reconnects, so the restart test does not wait long.
fn fast_backoff() -> Backoff {
    Backoff::new(Duration::from_millis(50), Duration::from_millis(250))
}

/// A headless client: the remote source on an action bus.
struct Headless {
    _inputs: InputSources,
    actions: Receiver<SourcedAction>,
    publisher: EventPublisher,
}

impl Headless {
    fn connect(api: &HttpApi) -> Self {
        let (bus, actions) = ActionBus::channel();
        let (source, publisher) = RemoteSource::new(RemoteConfig {
            backoff: fast_backoff(),
            ..RemoteConfig::for_api(api)
        });
        let mut inputs = InputSources::new();
        inputs.start(Box::new(source), &bus).unwrap();
        Self {
            _inputs: inputs,
            actions,
            publisher,
        }
    }

    fn next_action(&self) -> SourcedAction {
        self.actions
            .recv_timeout(Duration::from_secs(5))
            .expect("a relayed action arrives")
    }
}

fn clients(base: &str) -> Vec<ClientInfo> {
    reqwest::blocking::get(format!("{base}/api/v1/clients"))
        .unwrap()
        .json()
        .unwrap()
}

/// Waits until `GET /clients` satisfies `ok`, or panics after 5s.
fn wait_until(base: &str, what: &str, ok: impl Fn(&[ClientInfo]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // While a server restarts, requests may fail; keep asking.
        let list = reqwest::blocking::get(format!("{base}/api/v1/clients"))
            .ok()
            .and_then(|r| r.json::<Vec<ClientInfo>>().ok());
        if list.as_deref().is_some_and(&ok) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// POSTs an action as JSON; returns the status.
fn post_action(base: &str, client: &str, action: &Action) -> u16 {
    reqwest::blocking::Client::new()
        .post(format!("{base}/api/v1/clients/{client}/actions"))
        .json(action)
        .send()
        .unwrap()
        .status()
        .as_u16()
}

#[test]
fn relayed_actions_drive_the_tuner() {
    let server = TestServer::start(None);
    let base = server.url();
    let api = server.api("tv");
    let client = Headless::connect(&api);
    wait_until(&base, "tv connects", |c| c.iter().any(|c| c.id == "tv"));

    let publisher = client.publisher.clone();
    let mut tuner = Tuner::new(
        api,
        Recorder::default(),
        SystemClock,
        TunerConfig::default(),
    )
    .with_event_sink(move |event| publisher.publish(event));
    tuner.start(Some(1)).unwrap();

    // `default` is the TV, the only client.
    assert_eq!(post_action(&base, "default", &Action::ChannelUp), 202);
    let relayed = client.next_action();
    assert_eq!(&*relayed.source, "remote");
    assert_eq!(relayed.action, Action::ChannelUp);
    tuner.handle_action(relayed.action).unwrap();
    assert_eq!(tuner.now().unwrap().channel_number, 2);

    // The tuner's events made it back: the server knows what is on.
    wait_until(&base, "the TV reports channel 2", |c| {
        c[0].playing
            .as_ref()
            .is_some_and(|p| p.channel_id == "other")
    });

    assert_eq!(
        post_action(&base, "tv", &Action::GoToChannel { number: 1 }),
        202
    );
    tuner.handle_action(client.next_action().action).unwrap();
    assert_eq!(tuner.now().unwrap().channel_id, "show");
    wait_until(&base, "the TV reports channel 1", |c| {
        c[0].playing
            .as_ref()
            .is_some_and(|p| p.channel_id == "show")
    });
}

/// Every keypress's action, as the keyboard would send it.
fn keyboard_actions() -> Vec<Action> {
    let mut keys = vec![
        Key::PageUp,
        Key::PageDown,
        Key::ArrowUp,
        Key::ArrowDown,
        Key::ArrowLeft,
        Key::ArrowRight,
        Key::Space,
        Key::Enter,
        Key::Escape,
    ];
    keys.extend((' '..='~').map(Key::Character));
    let now = Instant::now();
    let mut actions: Vec<Action> = keys
        .into_iter()
        .filter_map(|key| Keyboard::new().press(key, now))
        .collect();
    // Digits, committed with Enter.
    let mut kb = Keyboard::new();
    kb.press(Key::Character('4'), now);
    actions.extend(kb.press(Key::Enter, now));
    assert!(actions.len() >= 15, "{actions:?}");
    actions
}

/// Anything the keyboard (or anything else) can do can be done through the
/// API: every action variant, and every action a key produces, is relayed
/// intact.
#[test]
fn every_action_is_reachable_through_the_relay() {
    let server = TestServer::start(None);
    let base = server.url();
    let client = Headless::connect(&server.api("tv"));
    wait_until(&base, "tv connects", |c| c.iter().any(|c| c.id == "tv"));

    let all: Vec<Action> = Action::examples()
        .into_iter()
        .chain(keyboard_actions())
        .collect();
    for action in &all {
        assert_eq!(post_action(&base, "tv", action), 202, "{action:?}");
    }
    for action in &all {
        assert_eq!(&client.next_action().action, action);
    }
    assert!(client.actions.try_recv().is_err(), "nothing extra arrives");
}

/// A standalone server on its own runtime, so it can be stopped and
/// started again on the same port.
struct Standalone {
    runtime: tokio::runtime::Runtime,
    addr: SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Standalone {
    fn start(config: Config) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(Server::bind(config)).unwrap();
        let addr = server.local_addr();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = runtime.spawn(server.serve(async {
            let _ = stopped.await;
        }));
        Self {
            runtime,
            addr,
            stop,
            task,
        }
    }

    fn stop(self) {
        self.stop.send(()).unwrap();
        self.runtime.block_on(self.task).unwrap().unwrap();
    }
}

#[test]
fn reconnects_with_backoff_after_a_server_restart() {
    let dir = common::library();
    let toml = common::config_toml(dir.path(), None);
    let server = Standalone::start(Config::from_toml_str(&toml).unwrap());
    let addr = server.addr;
    let base = format!("http://{addr}");
    let client = Headless::connect(&HttpApi::new(&base, None, "tv").unwrap());
    wait_until(&base, "tv connects", |c| c.iter().any(|c| c.id == "tv"));
    let playing = Event::NowPlaying {
        channel_id: "show".into(),
        channel_number: 1,
        item_id: "i1".into(),
        title: "Show · S01E01 · One".into(),
        position_ms: 0,
        resolution_path: vec!["show".into()],
    };
    client.publisher.publish(playing);
    wait_until(&base, "the TV reports what it plays", |c| {
        c[0].playing.is_some()
    });

    server.stop();
    // Down for a moment: the client keeps trying, backing off.
    std::thread::sleep(Duration::from_millis(400));
    let toml = toml.replace("127.0.0.1:0", &addr.to_string());
    let server = Standalone::start(Config::from_toml_str(&toml).unwrap());
    assert_eq!(server.addr, addr);

    // Back without anyone doing anything, and it re-reported what it is
    // playing.
    wait_until(&base, "tv reconnects", |c| {
        c.iter()
            .any(|c| c.id == "tv" && c.playing.as_ref().is_some_and(|p| p.item_id == "i1"))
    });
    assert_eq!(post_action(&base, "tv", &Action::ShowInfo), 202);
    assert_eq!(client.next_action().action, Action::ShowInfo);
    assert_eq!(clients(&base).len(), 1);
    server.stop();
}

#[test]
fn the_token_is_sent_on_the_control_channel() {
    let server = TestServer::start(Some("sesame"));
    let base = server.url();
    let api = HttpApi::new(&base, Some("sesame"), "tv").unwrap();
    let client = Headless::connect(&api);
    let http = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let action = loop {
        let response = http
            .post(format!("{base}/api/v1/clients/tv/actions"))
            .bearer_auth("sesame")
            .json(&Action::Back)
            .send()
            .unwrap();
        if response.status() == 202 {
            break client.next_action().action;
        }
        assert_eq!(response.status(), 404, "not connected yet");
        assert!(Instant::now() < deadline, "tv never connected");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(action, Action::Back);
}
