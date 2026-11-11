//! The HTTP API (PLAN.md §8): routes, handlers, authentication, and the
//! OpenAPI document. The remote-control routes (`/clients`, `/events`)
//! are backed by [`crate::relay`].

use std::convert::Infallible;
use std::path::{Component, Path as FsPath, PathBuf};

use axum::body::{Body, Bytes};
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use marathon_api::{
    API_PREFIX, API_VERSION, Action, ActionAccepted, AdvanceReason, AdvanceRequest,
    CLIENT_ID_HEADER, ChannelSummary, ClientInfo, ClientPlaying, DEFAULT_CLIENT, Direction,
    ErrorBody, ErrorCode, Event, EventKind, Health, HistoryEntry, Item, NextItem, Now,
    PreviousRequest, ProgressReport, RelayedEvent, ScanReport, Subtitle, VersionInfo,
};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;

use crate::app::App;
use crate::error::ApiError;
use crate::playback::subtitles_of;
use crate::sources::Source;

/// Default and maximum `limit` for `GET /history`.
const HISTORY_DEFAULT_LIMIT: u32 = 50;
const HISTORY_MAX_LIMIT: u32 = 1000;

/// Builds the full router: every route under [`API_PREFIX`], wrapped in
/// bearer-token authentication (when configured) and request tracing.
pub fn router(app: App) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/channels", get(list_channels))
        .route("/channels/{id}/now", get(now))
        .route("/channels/{id}/progress", post(progress))
        .route("/channels/{id}/advance", post(advance))
        .route("/channels/{id}/previous", post(previous))
        .route("/items/{id}", get(item))
        .route("/stream/{item_id}", get(stream))
        .route("/subtitles/{item_id}/{n}", get(subtitle))
        .route("/history", get(history))
        .route("/library/rescan", post(rescan))
        .route("/clients", get(list_clients))
        .route("/clients/connect", get(connect))
        .route("/clients/{id}/actions", post(client_action))
        .route("/events", get(events))
        .route("/openapi.json", get(openapi));
    Router::new()
        .nest(API_PREFIX, api)
        .fallback(fallback)
        // Applied to every request, including unknown paths, so nothing is
        // revealed to a client without the token.
        .layer(middleware::from_fn_with_state(app.clone(), require_token))
        .layer(TraceLayer::new_for_http())
        .with_state(app)
}

// --- Authentication -------------------------------------------------------

/// Rejects requests without `Authorization: Bearer <token>` when a token is
/// configured.
async fn require_token(State(app): State<App>, req: Request, next: Next) -> Response {
    if let Some(token) = &app.config().server.token {
        let presented = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let ok = presented.is_some_and(|p| constant_time_eq(p.as_bytes(), token.as_bytes()));
        if !ok {
            return ApiError::Unauthorized.into_response();
        }
    }
    next.run(req).await
}

/// Compares two byte strings in time that depends only on their lengths, so
/// response timing does not reveal how much of a guessed token was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The caller's `X-Marathon-Client` header, if present and sensible.
struct ClientId(Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for ClientId {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientId(
            parts
                .headers
                .get(CLIENT_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.len() <= 128)
                .map(str::to_owned),
        ))
    }
}

fn bad_json(err: JsonRejection) -> ApiError {
    ApiError::BadRequest(err.body_text())
}

fn bad_path(err: PathRejection) -> ApiError {
    ApiError::BadRequest(err.body_text())
}

// --- Handlers -------------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/health", tag = "server",
    responses((status = 200, description = "The server is up", body = Health)))]
async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".to_owned(),
    })
}

#[utoipa::path(get, path = "/api/v1/version", tag = "server",
    responses((status = 200, description = "API and server versions", body = VersionInfo)))]
async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        api_version: API_VERSION,
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

#[utoipa::path(get, path = "/api/v1/channels", tag = "channels",
    responses((status = 200, description = "Every channel, by number, with what it is on",
        body = Vec<ChannelSummary>)))]
async fn list_channels(State(app): State<App>) -> Result<Json<Vec<ChannelSummary>>, ApiError> {
    Ok(Json(app.list_channels().await?))
}

#[utoipa::path(get, path = "/api/v1/channels/{id}/now", tag = "channels",
    description = "Tunes in: returns the current item, saved position, stream URLs, and the \
                   next item for prefetching. Records a `tuned` event.",
    params(("id" = String, Path, description = "Channel ID")),
    responses(
        (status = 200, description = "What the channel is playing", body = Now),
        (status = 404, description = "No such channel", body = ErrorBody),
        (status = 409, description = "The channel has no playable items", body = ErrorBody),
    ))]
async fn now(
    State(app): State<App>,
    Path(id): Path<String>,
    ClientId(client): ClientId,
) -> Result<Json<Now>, ApiError> {
    Ok(Json(app.now(&id, client.as_deref()).await?))
}

#[utoipa::path(post, path = "/api/v1/channels/{id}/progress", tag = "channels",
    description = "Saves the playback position of the channel's current item.",
    params(("id" = String, Path, description = "Channel ID")),
    request_body = ProgressReport,
    responses(
        (status = 204, description = "Saved"),
        (status = 400, description = "Malformed request", body = ErrorBody),
        (status = 404, description = "No such channel", body = ErrorBody),
        (status = 409, description = "`item_id` is not the current item (`stale_item`), or \
                                      the channel is empty", body = ErrorBody),
    ))]
async fn progress(
    State(app): State<App>,
    Path(id): Path<String>,
    ClientId(client): ClientId,
    body: Result<Json<ProgressReport>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(report) = body.map_err(bad_json)?;
    app.progress(&id, &report, client.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/channels/{id}/advance", tag = "channels",
    description = "Moves the channel to its next item. Idempotent: if `from_item_id` is not \
                   the current item, returns the current state without advancing.",
    params(("id" = String, Path, description = "Channel ID")),
    request_body = AdvanceRequest,
    responses(
        (status = 200, description = "What the channel is now playing", body = Now),
        (status = 400, description = "Malformed request", body = ErrorBody),
        (status = 404, description = "No such channel", body = ErrorBody),
        (status = 409, description = "The channel has no playable items", body = ErrorBody),
    ))]
async fn advance(
    State(app): State<App>,
    Path(id): Path<String>,
    ClientId(client): ClientId,
    body: Result<Json<AdvanceRequest>, JsonRejection>,
) -> Result<Json<Now>, ApiError> {
    let Json(req) = body.map_err(bad_json)?;
    Ok(Json(
        app.advance(&id, &req.from_item_id, req.reason, client.as_deref())
            .await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/channels/{id}/previous", tag = "channels",
    description = "Steps the channel back via its rule. Like `advance`, only moves if \
                   `from_item_id` is the current item; stays put if there is nothing to go \
                   back to.",
    params(("id" = String, Path, description = "Channel ID")),
    request_body = PreviousRequest,
    responses(
        (status = 200, description = "What the channel is now playing", body = Now),
        (status = 400, description = "Malformed request", body = ErrorBody),
        (status = 404, description = "No such channel", body = ErrorBody),
        (status = 409, description = "The channel has no playable items", body = ErrorBody),
    ))]
async fn previous(
    State(app): State<App>,
    Path(id): Path<String>,
    ClientId(client): ClientId,
    body: Result<Json<PreviousRequest>, JsonRejection>,
) -> Result<Json<Now>, ApiError> {
    let Json(req) = body.map_err(bad_json)?;
    Ok(Json(
        app.previous(&id, &req.from_item_id, client.as_deref())
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/items/{id}", tag = "library",
    params(("id" = String, Path, description = "Item ID")),
    responses(
        (status = 200, description = "Item metadata", body = Item),
        (status = 404, description = "No such item", body = ErrorBody),
    ))]
async fn item(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Item>, ApiError> {
    Ok(Json(app.item(&id).await?))
}

#[utoipa::path(get, path = "/api/v1/stream/{item_id}", tag = "library",
    description = "The item's media bytes. Supports `Range` requests and `HEAD`. Items from a \
                   Jellyfin source are proxied from Jellyfin, ranges included.",
    params(("item_id" = String, Path, description = "Item ID")),
    responses(
        (status = 200, description = "The whole file", content_type = "application/octet-stream"),
        (status = 206, description = "The requested range", content_type = "application/octet-stream"),
        (status = 404, description = "No such item, or its file is missing", body = ErrorBody),
        (status = 416, description = "The range cannot be satisfied"),
        (status = 502, description = "The item's Jellyfin server failed or is unreachable",
            body = ErrorBody),
    ))]
async fn stream(
    State(app): State<App>,
    Path(item_id): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    match media(&app, &item_id).await? {
        Media::File(file) => {
            // ServeFile handles Range, If-Range, conditional requests, and
            // HEAD, and returns 404 itself if the file has vanished since
            // the last scan.
            let response = match ServeFile::new(file).oneshot(req).await {
                Ok(response) => response,
                Err(never) => match never {},
            };
            Ok(response.map(Body::new))
        }
        Media::Jellyfin { client, id } => client.stream(&id, req.method(), req.headers()).await,
    }
}

#[utoipa::path(get, path = "/api/v1/subtitles/{item_id}/{n}", tag = "library",
    params(
        ("item_id" = String, Path, description = "Item ID"),
        ("n" = u32, Path, description = "Subtitle index, from the item's `subtitles`"),
    ),
    responses(
        (status = 200, description = "The subtitle file", content_type = "text/plain"),
        (status = 404, description = "No such item or subtitle", body = ErrorBody),
    ))]
async fn subtitle(
    State(app): State<App>,
    path: Result<Path<(String, u32)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((item_id, n)) = path.map_err(bad_path)?;
    let not_found = || ApiError::NotFound(format!("item {item_id} has no subtitle {n}"));
    let row = app.db().item(&item_id).await?.ok_or_else(not_found)?;
    let subtitles = subtitles_of(&row)?;
    let sub = usize::try_from(n)
        .ok()
        .and_then(|n| subtitles.get(n))
        .ok_or_else(not_found)?;
    let root = app.source_root(&row.source_id).ok_or_else(not_found)?;
    let file = safe_join(&root, &sub.path).ok_or_else(not_found)?;
    let bytes = match tokio::fs::read(&file).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(err) => return Err(ApiError::Internal(err.into())),
    };
    let content_type = match sub.format.as_str() {
        "srt" => "application/x-subrip",
        "vtt" => "text/vtt",
        "ass" | "ssa" => "text/x-ssa",
        _ => "application/octet-stream",
    };
    Ok((
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        bytes,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct HistoryParams {
    limit: Option<u32>,
}

#[utoipa::path(get, path = "/api/v1/history", tag = "history",
    params(("limit" = Option<u32>, Query,
        description = "How many events to return (default 50, at most 1000)")),
    responses(
        (status = 200, description = "Recent events, newest first", body = Vec<HistoryEntry>),
        (status = 400, description = "Malformed query", body = ErrorBody),
    ))]
async fn history(
    State(app): State<App>,
    query: Result<Query<HistoryParams>, QueryRejection>,
) -> Result<Json<Vec<HistoryEntry>>, ApiError> {
    let Query(params) = query.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let limit = params
        .limit
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let rows = app.db().history(limit).await?;
    let events = rows
        .into_iter()
        .filter_map(|row| {
            let Some(kind) = EventKind::parse(&row.kind) else {
                tracing::warn!(id = %row.id, kind = %row.kind, "skipping event of unknown kind");
                return None;
            };
            Some(HistoryEntry {
                id: row.id,
                ts_ms: row.ts,
                client_id: row.client_id,
                channel_id: row.channel_id,
                resolved_channel_id: None,
                resolution_path: Vec::new(),
                item_id: row.item_id,
                kind,
                position_ms: row.position_ms.and_then(|p| u64::try_from(p).ok()),
            })
        })
        .collect();
    Ok(Json(events))
}

#[utoipa::path(post, path = "/api/v1/library/rescan", tag = "library",
    description = "Rescans every source and rebuilds channel membership. Returns when done.",
    responses((status = 200, description = "What the scan found", body = ScanReport)))]
async fn rescan(State(app): State<App>) -> Result<Json<ScanReport>, ApiError> {
    Ok(Json(app.rescan().await?))
}

// --- Remote control and events (PLAN.md §9) --------------------------------

#[utoipa::path(get, path = "/api/v1/clients", tag = "control",
    description = "Clients connected to their control channel, most recently active first, \
                   with what each is playing.",
    responses((status = 200, description = "Connected clients", body = Vec<ClientInfo>)))]
async fn list_clients(State(app): State<App>) -> Json<Vec<ClientInfo>> {
    Json(app.relay().list())
}

#[utoipa::path(get, path = "/api/v1/clients/connect", tag = "control",
    description = "A client's control channel, as a WebSocket. The client must send \
                   `X-Marathon-Client`. The server sends each relayed `Action` as a JSON text \
                   message; the client sends each of its `Event`s the same way, and the server \
                   republishes them on `GET /events`. A client that connects again replaces \
                   its earlier connection.",
    params(("X-Marathon-Client" = String, Header, description = "The client's ID")),
    responses(
        (status = 101, description = "Switching to the WebSocket protocol"),
        (status = 400, description = "Not a WebSocket request, or no usable client ID",
            body = ErrorBody),
    ))]
async fn connect(
    State(app): State<App>,
    ClientId(client): ClientId,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let upgrade = upgrade.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let client = client.ok_or_else(|| {
        ApiError::BadRequest(format!(
            "the control channel needs a {CLIENT_ID_HEADER} header"
        ))
    })?;
    if client == DEFAULT_CLIENT {
        return Err(ApiError::BadRequest(format!(
            "{DEFAULT_CLIENT:?} is reserved and cannot be a client ID"
        )));
    }
    Ok(upgrade.on_upgrade(move |socket| control_channel(app, client, socket)))
}

/// Runs one client's control channel until either side closes it or the
/// server shuts down.
async fn control_channel(app: App, client: String, mut socket: WebSocket) {
    let relay = app.relay();
    let (connection, mut actions) = relay.connect(&client);
    let shutdown = relay.shutting_down();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            action = actions.recv() => {
                // `None`: the client connected again, replacing this
                // connection.
                let Some(action) = action else { break };
                let text = match serde_json::to_string(&action) {
                    Ok(text) => text,
                    Err(err) => {
                        tracing::error!(%err, "cannot serialize an action");
                        continue;
                    }
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<Event>(&text) {
                    Ok(event) => relay.publish(&client, event),
                    Err(err) => tracing::warn!(
                        client = %client, %err, "ignoring a malformed event from a client"
                    ),
                },
                // Pings are answered by the WebSocket library.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
            () = &mut shutdown => {
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: close_code::AWAY,
                        reason: "server shutting down".into(),
                    })))
                    .await;
                break;
            }
        }
    }
    relay.disconnect(&client, connection);
}

#[utoipa::path(post, path = "/api/v1/clients/{id}/actions", tag = "control",
    description = "Relays an action to a connected client, as if pressed on its remote. \
                   `id` may be `default`: the most recently active client. The body is \
                   parsed as JSON whatever its `Content-Type`, so a bare `curl -d` works. \
                   Delivery is best effort: the action is lost if the client disconnects \
                   before it is sent.",
    params(("id" = String, Path, description = "Client ID, or `default`")),
    request_body = Action,
    responses(
        (status = 202, description = "Handed to the client's control channel",
            body = ActionAccepted),
        (status = 400, description = "Not a valid action", body = ErrorBody),
        (status = 404, description = "No such client is connected", body = ErrorBody),
    ))]
async fn client_action(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<ActionAccepted>), ApiError> {
    let action: Action = serde_json::from_slice(&body)
        .map_err(|err| ApiError::BadRequest(format!("not a valid action: {err}")))?;
    let client_id = app
        .relay()
        .send_action(&id, action)
        .map_err(|err| ApiError::NotFound(err.to_string()))?;
    Ok((StatusCode::ACCEPTED, Json(ActionAccepted { client_id })))
}

#[utoipa::path(get, path = "/api/v1/events", tag = "control",
    description = "Server-Sent Events: every event any connected client publishes, from now \
                   on. Each message's `event:` field is the event's name (`now_playing`, \
                   ...) and its `data:` a `RelayedEvent` as JSON. A subscriber that falls far \
                   behind gets a comment saying how many events it missed.",
    responses((status = 200, description = "The event stream", body = RelayedEvent,
        content_type = "text/event-stream")))]
async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<sse::Event, Infallible>>> {
    let rx = app.relay().subscribe();
    let stream = futures_util::stream::unfold((app, rx), |(app, mut rx)| async move {
        let message = tokio::select! {
            () = app.relay().shutting_down() => return None,
            received = rx.recv() => match received {
                Ok(relayed) => match sse::Event::default()
                    .event(relayed.event.name())
                    .json_data(&relayed)
                {
                    Ok(message) => message,
                    Err(err) => {
                        tracing::error!(%err, "cannot serialize an event");
                        sse::Event::default().comment("an event could not be serialized")
                    }
                },
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "an event subscriber fell behind");
                    sse::Event::default().comment(format!("missed {missed} events"))
                }
                Err(RecvError::Closed) => return None,
            },
        };
        Some((Ok(message), (app, rx)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "server",
    responses((status = 200, description = "This OpenAPI document", content_type = "application/json")))]
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

async fn fallback() -> ApiError {
    ApiError::NotFound("no such route".to_owned())
}

// --- Files ----------------------------------------------------------------

/// Where a playable item's bytes come from.
enum Media {
    File(PathBuf),
    Jellyfin {
        client: std::sync::Arc<crate::jellyfin::JellyfinClient>,
        id: String,
    },
}

/// Finds where to stream a playable item from.
async fn media(app: &App, item_id: &str) -> Result<Media, ApiError> {
    let not_found = || ApiError::NotFound(format!("no item {item_id}"));
    let row = app.db().item(item_id).await?.ok_or_else(not_found)?;
    if row.missing {
        return Err(ApiError::NotFound(format!(
            "item {item_id} is missing from its source"
        )));
    }
    match app.source(&row.source_id).ok_or_else(not_found)? {
        Source::Filesystem { root } => safe_join(root, &row.path)
            .map(Media::File)
            .ok_or_else(not_found),
        Source::Jellyfin(client) => Ok(Media::Jellyfin {
            client: client.clone(),
            id: row.external_id.ok_or_else(not_found)?,
        }),
    }
}

/// Joins a stored relative path onto a source root, refusing anything that
/// could escape it. Stored paths come from scans and are always plain
/// relative paths; this is defence in depth.
fn safe_join(root: &FsPath, rel: &str) -> Option<PathBuf> {
    let rel = FsPath::new(rel);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| root.join(rel))
}

// --- OpenAPI --------------------------------------------------------------

/// The OpenAPI document for the v1 API, served at `/api/v1/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Marathon API",
        description = "Channels of personal video that resume where you left off. \
                       See PLAN.md §8. Every route requires `Authorization: Bearer <token>` \
                       when the server has a token configured. Clients identify themselves \
                       with an `X-Marathon-Client` header.",
    ),
    paths(
        health, version, list_channels, now, progress, advance, previous, item, stream,
        subtitle, history, rescan, list_clients, connect, client_action, events, openapi,
    ),
    components(schemas(
        Health, VersionInfo, ChannelSummary, Now, NextItem, Item, Subtitle, ProgressReport,
        AdvanceRequest, AdvanceReason, PreviousRequest, HistoryEntry, EventKind, ScanReport, ErrorBody,
        ErrorCode, Action, Direction, Event, RelayedEvent, ClientInfo, ClientPlaying,
        ActionAccepted,
    )),
    modifiers(&BearerAuth),
    security((), ("bearer" = [])),
)]
pub struct ApiDoc;

/// Declares the optional bearer-token security scheme.
struct BearerAuth;

impl utoipa::Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret!"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn safe_join_refuses_escapes() {
        let root = FsPath::new("/media");
        assert_eq!(
            safe_join(root, "TV/a.mkv"),
            Some(PathBuf::from("/media/TV/a.mkv"))
        );
        assert_eq!(safe_join(root, "../etc/passwd"), None);
        assert_eq!(safe_join(root, "/etc/passwd"), None);
        assert!(safe_join(root, "TV/./a.mkv").is_some());
    }
}
