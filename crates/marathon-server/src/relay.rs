//! The client relay (PLAN.md §9 "Input and control", "Remote adapters"):
//! which clients are connected to their control channel, how actions reach
//! them, and how their events reach observers.
//!
//! ```text
//! POST /clients/{id}/actions ──► Relay::send_action ──► that client's WebSocket
//! client WebSocket ──► Relay::publish ──► GET /events (every subscriber)
//! ```
//!
//! The relay is in-memory only: connections and the event stream do not
//! survive a restart (clients reconnect on their own), and nothing here is
//! written to the database. Playback history is still recorded by the
//! regular API calls clients make.
//!
//! The HTTP side (the WebSocket loop and the SSE stream) is in
//! [`crate::http`]; this module is plain state, so it is easy to test.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use marathon_api::{Action, ClientInfo, ClientPlaying, DEFAULT_CLIENT, Event, RelayedEvent};
use tokio::sync::{broadcast, mpsc, watch};

/// How many events a slow `GET /events` subscriber may fall behind before
/// it misses some.
const EVENT_BACKLOG: usize = 256;

/// Identifies one WebSocket connection, so a client that reconnects
/// before its old connection is noticed as closed is not unregistered by
/// the old connection's cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionId(u64);

/// The relay. One per server; shared through [`App`](crate::App).
#[derive(Debug)]
pub struct Relay {
    clients: Mutex<Clients>,
    events: broadcast::Sender<RelayedEvent>,
    shutdown: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct Clients {
    by_id: HashMap<String, Client>,
    next_connection: u64,
    /// Increases on every activity, to order clients whose wall-clock
    /// timestamps tie.
    next_activity: u64,
}

#[derive(Debug)]
struct Client {
    connection: ConnectionId,
    actions: mpsc::UnboundedSender<Action>,
    connected_at_ms: i64,
    last_active_ms: i64,
    activity: u64,
    playing: Option<ClientPlaying>,
    paused: bool,
    standby: bool,
}

/// No client is connected under that ID (or, for `default`, at all).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", if .0 == DEFAULT_CLIENT {
    "no client is connected".to_owned()
} else {
    format!("no connected client {}", .0)
})]
pub struct UnknownClient(pub String);

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

impl Relay {
    pub fn new() -> Self {
        Self {
            clients: Mutex::default(),
            events: broadcast::channel(EVENT_BACKLOG).0,
            shutdown: watch::channel(false).0,
        }
    }

    fn clients(&self) -> std::sync::MutexGuard<'_, Clients> {
        // A panic while holding this lock leaves only bookkeeping behind,
        // which is still consistent enough to keep relaying.
        self.clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a new connection for `client_id`, replacing any earlier
    /// one. Returns its ID and the receiver of actions to send down it.
    pub fn connect(&self, client_id: &str) -> (ConnectionId, mpsc::UnboundedReceiver<Action>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let now = now_ms();
        let mut clients = self.clients();
        let connection = ConnectionId(clients.next_connection);
        clients.next_connection += 1;
        let activity = clients.bump();
        let replaced = clients.by_id.insert(
            client_id.to_owned(),
            Client {
                connection,
                actions: tx,
                connected_at_ms: now,
                last_active_ms: now,
                activity,
                playing: None,
                paused: false,
                standby: false,
            },
        );
        drop(clients);
        if replaced.is_some() {
            tracing::info!(
                client = client_id,
                "client reconnected; replacing its old connection"
            );
        } else {
            tracing::info!(client = client_id, "client connected");
        }
        (connection, rx)
    }

    /// Unregisters `connection`, unless the client has connected again
    /// since.
    pub fn disconnect(&self, client_id: &str, connection: ConnectionId) {
        let mut clients = self.clients();
        if clients
            .by_id
            .get(client_id)
            .is_some_and(|c| c.connection == connection)
        {
            clients.by_id.remove(client_id);
            tracing::info!(client = client_id, "client disconnected");
        }
    }

    /// Resolves `id` to a connected client: itself, or for
    /// [`DEFAULT_CLIENT`] the most recently active one.
    pub fn resolve(&self, id: &str) -> Result<String, UnknownClient> {
        self.clients()
            .resolve(id)
            .map(str::to_owned)
            .ok_or_else(|| UnknownClient(id.to_owned()))
    }

    /// Hands `action` to client `id` (or `default`). Returns the client it
    /// went to.
    ///
    /// Delivery is best effort: the action is queued on the client's
    /// connection, and lost if that connection drops before it is sent.
    pub fn send_action(&self, id: &str, action: Action) -> Result<String, UnknownClient> {
        let clients = self.clients();
        let unknown = || UnknownClient(id.to_owned());
        let target = clients.resolve(id).ok_or_else(unknown)?.to_owned();
        let client = clients.by_id.get(&target).ok_or_else(unknown)?;
        // The receiver lives as long as the connection's task; if it has
        // just ended, the client is as good as gone.
        client.actions.send(action.clone()).map_err(|_| unknown())?;
        tracing::info!(client = %target, action = action.name(), "relayed action");
        Ok(target)
    }

    /// Records an event from `client_id` and passes it to every
    /// `GET /events` subscriber.
    pub fn publish(&self, client_id: &str, event: Event) {
        let mut clients = self.clients();
        let activity = clients.bump();
        if let Some(client) = clients.by_id.get_mut(client_id) {
            client.last_active_ms = now_ms();
            client.activity = activity;
            client.observe(&event);
        }
        drop(clients);
        tracing::debug!(client = client_id, event = event.name(), "client event");
        // An error only means nobody is subscribed.
        let _ = self.events.send(RelayedEvent {
            client_id: client_id.to_owned(),
            ts_ms: now_ms(),
            event,
        });
    }

    /// A receiver of every event published from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<RelayedEvent> {
        self.events.subscribe()
    }

    /// Every connected client, most recently active first.
    pub fn list(&self) -> Vec<ClientInfo> {
        let clients = self.clients();
        let default = clients.resolve(DEFAULT_CLIENT).map(str::to_owned);
        let mut list: Vec<_> = clients
            .by_id
            .iter()
            .map(|(id, c)| {
                (
                    c.activity,
                    ClientInfo {
                        id: id.clone(),
                        connected_at_ms: c.connected_at_ms,
                        last_active_ms: c.last_active_ms,
                        is_default: default.as_deref() == Some(id),
                        playing: c.playing.clone(),
                        paused: c.paused,
                        standby: c.standby,
                    },
                )
            })
            .collect();
        list.sort_by_key(|(activity, _)| std::cmp::Reverse(*activity));
        list.into_iter().map(|(_, info)| info).collect()
    }

    /// Tells every open control channel and event stream to close, so the
    /// server's graceful shutdown does not wait on them.
    pub fn shut_down(&self) {
        self.shutdown.send_replace(true);
    }

    /// Resolves once [`Relay::shut_down`] has been called.
    pub async fn shutting_down(&self) {
        let mut rx = self.shutdown.subscribe();
        // An error means the sender is gone, which cannot happen while
        // `self` is alive; either way there is nothing left to wait for.
        let _ = rx.wait_for(|stop| *stop).await;
    }
}

impl Clients {
    fn bump(&mut self) -> u64 {
        self.next_activity += 1;
        self.next_activity
    }

    /// `default` always means the most recently active client; the HTTP
    /// layer refuses to register a client under that ID.
    fn resolve(&self, id: &str) -> Option<&str> {
        if id == DEFAULT_CLIENT {
            self.by_id
                .iter()
                .max_by_key(|(_, c)| c.activity)
                .map(|(id, _)| id.as_str())
        } else {
            self.by_id.get_key_value(id).map(|(id, _)| id.as_str())
        }
    }
}

impl Client {
    /// Keeps track of what the client is doing, for `GET /clients`.
    fn observe(&mut self, event: &Event) {
        match event {
            Event::NowPlaying {
                channel_id,
                channel_number,
                item_id,
                title,
                resolution_path,
                ..
            } => {
                self.playing = Some(ClientPlaying {
                    channel_id: channel_id.clone(),
                    channel_number: *channel_number,
                    item_id: item_id.clone(),
                    title: title.clone(),
                    resolution_path: resolution_path.clone(),
                });
                self.paused = false;
                self.standby = false;
            }
            Event::Paused { .. } => self.paused = true,
            Event::Resumed { .. } => self.paused = false,
            Event::Standby => self.standby = true,
            Event::Wake => self.standby = false,
            Event::ChannelChanged { .. } | Event::ItemEnded { .. } | Event::Error { .. } => {}
        }
    }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_playing(item: &str) -> Event {
        Event::NowPlaying {
            channel_id: "c".into(),
            channel_number: 1,
            item_id: item.into(),
            title: "T".into(),
            position_ms: 0,
            resolution_path: vec!["c".into()],
        }
    }

    #[test]
    fn actions_reach_the_named_client() {
        let relay = Relay::new();
        let (_, mut a) = relay.connect("a");
        let (_, mut b) = relay.connect("b");
        assert_eq!(relay.send_action("a", Action::ChannelUp).unwrap(), "a");
        assert_eq!(a.try_recv().unwrap(), Action::ChannelUp);
        assert!(b.try_recv().is_err());
        assert_eq!(
            relay.send_action("nobody", Action::ChannelUp),
            Err(UnknownClient("nobody".into()))
        );
    }

    #[test]
    fn default_is_the_most_recently_active_client() {
        let relay = Relay::new();
        let err = relay.resolve(DEFAULT_CLIENT).unwrap_err();
        assert_eq!(err.to_string(), "no client is connected");
        let (_, _a) = relay.connect("a");
        assert_eq!(relay.resolve(DEFAULT_CLIENT).unwrap(), "a");
        let (_, _b) = relay.connect("b");
        assert_eq!(relay.resolve(DEFAULT_CLIENT).unwrap(), "b");
        // An event makes a client active again.
        relay.publish("a", Event::Wake);
        assert_eq!(relay.resolve(DEFAULT_CLIENT).unwrap(), "a");
        let list = relay.list();
        assert_eq!(list[0].id, "a");
        assert!(list[0].is_default);
        assert!(!list[1].is_default);
    }

    #[test]
    fn a_stale_connection_does_not_unregister_its_replacement() {
        let relay = Relay::new();
        let (old, _) = relay.connect("a");
        let (new, mut rx) = relay.connect("a");
        relay.disconnect("a", old);
        assert_eq!(relay.list().len(), 1);
        relay.send_action("a", Action::Quit).unwrap();
        assert_eq!(rx.try_recv().unwrap(), Action::Quit);
        relay.disconnect("a", new);
        assert!(relay.list().is_empty());
    }

    #[test]
    fn events_are_broadcast_and_tracked() {
        let relay = Relay::new();
        let mut sub = relay.subscribe();
        let (_, _rx) = relay.connect("a");
        relay.publish("a", now_playing("i1"));
        relay.publish(
            "a",
            Event::Paused {
                channel_id: "c".into(),
                item_id: "i1".into(),
                position_ms: 1,
            },
        );
        let first = sub.try_recv().unwrap();
        assert_eq!(first.client_id, "a");
        assert_eq!(first.event, now_playing("i1"));
        assert!(matches!(
            sub.try_recv().unwrap().event,
            Event::Paused { .. }
        ));

        let info = &relay.list()[0];
        assert_eq!(info.playing.as_ref().unwrap().item_id, "i1");
        assert!(info.paused);
        relay.publish("a", Event::Standby);
        assert!(relay.list()[0].standby);
        relay.publish("a", now_playing("i2"));
        let info = &relay.list()[0];
        assert!(!info.paused && !info.standby);
    }

    #[tokio::test]
    async fn shutting_down_resolves_after_shut_down() {
        let relay = std::sync::Arc::new(Relay::new());
        let waiter = tokio::spawn({
            let relay = relay.clone();
            async move { relay.shutting_down().await }
        });
        relay.shut_down();
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .unwrap()
            .unwrap();
        // And for anyone who starts waiting afterwards.
        relay.shutting_down().await;
    }
}
