//! Talking to a Marathon server's v1 API (PLAN.md §8).
//!
//! [`Api`] is the interface the [`Tuner`](crate::tuner::Tuner) uses; it is a
//! trait so the tuner can be tested against an in-memory fake.
//! [`HttpApi`] implements it over HTTP.
//!
//! Calls are blocking. The desktop client makes them from its tuner thread,
//! never from the render thread.

use std::time::Duration;

use marathon_api::{
    API_PREFIX, AdvanceRequest, CLIENT_ID_HEADER, ChannelSummary, ErrorBody, ErrorCode,
    HistoryEntry, Item, Now, PreviousRequest, ProgressReport,
};
use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;

/// A failed API call.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The server URL is not usable.
    #[error("invalid server URL {url:?}: {reason}")]
    BadUrl { url: String, reason: String },
    /// The request did not get a response (connection refused, timeout,
    /// ...).
    #[error("cannot reach the server: {0}")]
    Transport(String),
    /// The server answered with an error status.
    #[error("server returned {status}: {message}")]
    Server {
        status: u16,
        /// The machine-readable code, if the body was an [`ErrorBody`].
        code: Option<ErrorCode>,
        message: String,
    },
    /// The response body was not what the API promises.
    #[error("unexpected response from the server: {0}")]
    Decode(String),
}

impl ApiError {
    /// The server's error code, if the server sent one.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            ApiError::Server { code, .. } => *code,
            _ => None,
        }
    }
}

/// The API calls a client makes. Every URL in a returned value
/// (`stream_url`, subtitle `url`) is absolute, ready to hand to a player.
pub trait Api {
    /// `GET /channels`: every channel, by number.
    fn channels(&self) -> Result<Vec<ChannelSummary>, ApiError>;
    /// `GET /channels/{id}/now`: tunes in.
    fn now(&self, channel_id: &str) -> Result<Now, ApiError>;
    /// `POST /channels/{id}/progress`.
    fn progress(&self, channel_id: &str, report: &ProgressReport) -> Result<(), ApiError>;
    /// `POST /channels/{id}/advance`.
    fn advance(&self, channel_id: &str, request: &AdvanceRequest) -> Result<Now, ApiError>;
    /// `POST /channels/{id}/previous`.
    fn previous(&self, channel_id: &str, request: &PreviousRequest) -> Result<Now, ApiError>;
    /// `GET /history?limit=`: recent events, newest first.
    fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, ApiError>;
}

/// How long to wait to connect, and for a whole request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// [`Api`] over HTTP.
#[derive(Debug, Clone)]
pub struct HttpApi {
    base: Url,
    http: Client,
    headers: Vec<(String, String)>,
}

impl HttpApi {
    /// A client for the server at `base_url` (e.g. `http://127.0.0.1:7878`).
    /// `token` is sent as a bearer token if set; `client_id` identifies
    /// this client in the server's history.
    pub fn new(base_url: &str, token: Option<&str>, client_id: &str) -> Result<Self, ApiError> {
        let bad_url = |reason: String| ApiError::BadUrl {
            url: base_url.to_owned(),
            reason,
        };
        let mut base = Url::parse(base_url).map_err(|e| bad_url(e.to_string()))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(bad_url("must be http or https".into()));
        }
        // Treat the base as a directory, so joining keeps any path prefix.
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }

        let mut headers = vec![(CLIENT_ID_HEADER.to_owned(), client_id.to_owned())];
        if let Some(token) = token {
            headers.push((AUTHORIZATION.as_str().to_owned(), format!("Bearer {token}")));
        }
        let mut header_map = HeaderMap::new();
        for (name, value) in &headers {
            let name: reqwest::header::HeaderName = name
                .parse()
                .map_err(|_| bad_url(format!("invalid header name {name}")))?;
            let mut value = HeaderValue::from_str(value)
                .map_err(|_| bad_url(format!("invalid value for header {name}")))?;
            value.set_sensitive(name == AUTHORIZATION);
            header_map.insert(name, value);
        }
        let http = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .default_headers(header_map)
            .build()
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        Ok(Self {
            base,
            http,
            headers,
        })
    }

    /// The server's base URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Headers a player must send when fetching streams and subtitles
    /// (the token, if any, and the client ID), as `(name, value)` pairs.
    pub fn media_headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Resolves a server-relative URL (as found in API responses) against
    /// the base URL.
    pub fn resolve(&self, url: &str) -> String {
        // Responses carry absolute paths like `/api/v1/stream/x`; strip the
        // leading slash so a base with a path prefix keeps it.
        match self.base.join(url.trim_start_matches('/')) {
            Ok(u) => u.into(),
            Err(_) => url.to_owned(),
        }
    }

    /// The URL of an API route: `segments` are appended to `/api/v1` and
    /// percent-encoded.
    fn route(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty();
            path.extend(API_PREFIX.split('/').filter(|s| !s.is_empty()));
            path.extend(segments);
        }
        // `path_segments_mut` only fails for cannot-be-a-base URLs, which
        // `new` rules out by requiring http(s).
        url
    }

    fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ApiError> {
        let response = self.send_raw(request)?;
        response.json().map_err(|e| ApiError::Decode(e.to_string()))
    }

    fn send_raw(&self, request: RequestBuilder) -> Result<reqwest::blocking::Response, ApiError> {
        let response = request
            .send()
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let text = response.text().unwrap_or_default();
        Err(match serde_json::from_str::<ErrorBody>(&text) {
            Ok(body) => ApiError::Server {
                status: status.as_u16(),
                code: Some(body.code),
                message: body.message,
            },
            Err(_) => ApiError::Server {
                status: status.as_u16(),
                code: None,
                message: if text.is_empty() {
                    status.to_string()
                } else {
                    text
                },
            },
        })
    }

    fn absolute_item(&self, item: &mut Item) {
        for sub in &mut item.subtitles {
            sub.url = self.resolve(&sub.url);
        }
    }

    fn absolute_now(&self, mut now: Now) -> Now {
        now.stream_url = self.resolve(&now.stream_url);
        self.absolute_item(&mut now.item);
        if let Some(next) = &mut now.next {
            next.stream_url = self.resolve(&next.stream_url);
            self.absolute_item(&mut next.item);
        }
        now
    }
}

impl Api for HttpApi {
    fn channels(&self) -> Result<Vec<ChannelSummary>, ApiError> {
        let mut channels: Vec<ChannelSummary> =
            self.send(self.http.get(self.route(&["channels"])))?;
        for item in channels.iter_mut().filter_map(|c| c.current.as_mut()) {
            self.absolute_item(item);
        }
        Ok(channels)
    }

    fn now(&self, channel_id: &str) -> Result<Now, ApiError> {
        let now = self.send(self.http.get(self.route(&["channels", channel_id, "now"])))?;
        Ok(self.absolute_now(now))
    }

    fn progress(&self, channel_id: &str, report: &ProgressReport) -> Result<(), ApiError> {
        let url = self.route(&["channels", channel_id, "progress"]);
        self.send_raw(self.http.post(url).json(report)).map(drop)
    }

    fn advance(&self, channel_id: &str, request: &AdvanceRequest) -> Result<Now, ApiError> {
        let url = self.route(&["channels", channel_id, "advance"]);
        let now = self.send(self.http.post(url).json(request))?;
        Ok(self.absolute_now(now))
    }

    fn previous(&self, channel_id: &str, request: &PreviousRequest) -> Result<Now, ApiError> {
        let url = self.route(&["channels", channel_id, "previous"]);
        let now = self.send(self.http.post(url).json(request))?;
        Ok(self.absolute_now(now))
    }

    fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, ApiError> {
        let mut url = self.route(&["history"]);
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        self.send(self.http.get(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(base: &str) -> HttpApi {
        HttpApi::new(base, Some("secret"), "client-1").unwrap()
    }

    #[test]
    fn routes_are_built_under_the_api_prefix_and_encoded() {
        let a = api("http://127.0.0.1:7878");
        assert_eq!(
            a.route(&["channels", "a b/c", "now"]).as_str(),
            "http://127.0.0.1:7878/api/v1/channels/a%20b%2Fc/now"
        );
        let prefixed = api("https://example.com/marathon");
        assert_eq!(
            prefixed.route(&["history"]).as_str(),
            "https://example.com/marathon/api/v1/history"
        );
    }

    #[test]
    fn server_paths_resolve_against_the_base() {
        assert_eq!(
            api("http://127.0.0.1:7878/").resolve("/api/v1/stream/x"),
            "http://127.0.0.1:7878/api/v1/stream/x"
        );
        assert_eq!(
            api("https://example.com/marathon/").resolve("/api/v1/stream/x"),
            "https://example.com/marathon/api/v1/stream/x"
        );
    }

    #[test]
    fn media_headers_carry_token_and_client_id() {
        let a = api("http://localhost:1");
        assert_eq!(
            a.media_headers(),
            &[
                (CLIENT_ID_HEADER.to_owned(), "client-1".to_owned()),
                ("authorization".to_owned(), "Bearer secret".to_owned()),
            ]
        );
        let anonymous = HttpApi::new("http://localhost:1", None, "c").unwrap();
        assert_eq!(anonymous.media_headers().len(), 1);
    }

    #[test]
    fn rejects_unusable_urls() {
        for bad in ["not a url", "ftp://host/", "mailto:x@y"] {
            assert!(
                matches!(HttpApi::new(bad, None, "c"), Err(ApiError::BadUrl { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn unreachable_server_is_a_transport_error() {
        // Port 1 on loopback refuses connections.
        let err = api("http://127.0.0.1:1").channels().unwrap_err();
        assert!(matches!(err, ApiError::Transport(_)), "{err}");
    }
}
